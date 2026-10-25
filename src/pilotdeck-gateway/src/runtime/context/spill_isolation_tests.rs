use super::tests::{ReleaseSpillGate, ResetSpillHook};
use super::*;
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

fn message(id: &str, body: &str) -> JsValue {
    pilotdeck_protocol::parse_json(
        &json!({"role":"user","content":[{
            "type":"tool_result","toolCallId":id,"content":[{"type":"text","text":body}]
        }]})
        .to_string(),
    )
    .unwrap()
}
fn field(value: &JsValue, key: &str) -> String {
    value
        .get(key)
        .unwrap()
        .as_string()
        .unwrap()
        .to_utf8()
        .unwrap()
}
fn reference(value: &JsValue) -> &JsValue {
    let JsValue::Array(items) = value.get("content").unwrap() else {
        panic!("content")
    };
    &items[0]
}
async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), future)
        .await
        .expect("operation watchdog")
}
async fn until(predicate: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("hook watchdog");
}

#[allow(clippy::await_holding_lock)]
async fn matrix(relation: &str, event: &str, two: bool) {
    let _order = SPILL_HOOK_ORDER.lock().unwrap();
    let _bound = SpillWaitTimeoutOverride::new(Duration::from_millis(200));
    let scratch = crate::test_scratch::Scratch::new("spill-matrix");
    let dir = scratch.path().join(".pilotdeck/tool-results/a");
    let make = |dir| {
        Arc::new(ToolResultBudget::new(ToolResultBudgetOptions {
            tool_results_dir: dir,
            max_result_size_chars: Some(1),
            max_result_size_tokens: Some(100),
            preview_bytes: Some(100),
        }))
    };
    let a = make(dir.clone());
    let b = if two {
        make(if relation == "different-root" {
            dir.with_file_name("b")
        } else {
            dir.clone()
        })
    } else {
        a.clone()
    };
    let name = format!("{relation}-{event}-{}", if two { "two" } else { "one" });
    let oracle: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/spill-isolation/oracle.json"
    ))
    .unwrap();
    let row = oracle["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == name)
        .unwrap();
    if relation == "different-root" && !two {
        assert!(row["na"].is_string());
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(a.tool_results_dir(), b.tool_results_dir());
        return;
    }
    let parked = Arc::new(AtomicBool::new(false));
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let (hook_parked, hook_gate) = (parked.clone(), gate.clone());
    let target = a.spill_directory.to_utf8().unwrap();
    if event != "sequential" {
        MID_SPILL_WRITE
            .lock()
            .unwrap()
            .replace(Arc::new(move |path| {
                if path.to_utf8().unwrap().starts_with(&target)
                    && !hook_parked.swap(true, Ordering::SeqCst)
                {
                    let (lock, cv) = &*hook_gate;
                    let mut open = lock.lock().unwrap();
                    while !*open {
                        open = cv.wait(open).unwrap();
                    }
                }
                None
            }));
    }
    let _reset = ResetSpillHook;
    let release = ReleaseSpillGate(gate.clone());
    let leader_budget = a.clone();
    let mut leader = Some(tokio::spawn(async move {
        leader_budget
            .apply_to_message(&message("c/1", "XXXXXXXX"), Some(&"t".into()))
            .await
    }));
    if event == "sequential" {
        leader.take().unwrap().await.unwrap().unwrap();
    } else {
        until(|| parked.load(Ordering::SeqCst)).await;
    }
    let ticket = a
        .state
        .lock()
        .unwrap()
        .in_flight_spills
        .values()
        .next()
        .cloned();
    if event == "cancelled" {
        let task = leader.take().unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
    }
    let next_budget = b.clone();
    let id = if relation == "different-id" {
        "other"
    } else {
        "c/1"
    };
    let follower = tokio::spawn(async move {
        next_budget
            .apply_to_message(&message(id, "YYYYYYYY"), Some(&"t".into()))
            .await
    });
    let same = relation == "same-id" && !two;
    if same && event == "concurrent" {
        until(|| SPILL_WAITERS.load(Ordering::SeqCst) > 0).await;
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
    }
    let first = tokio::time::timeout(Duration::from_secs(5), follower)
        .await
        .unwrap()
        .unwrap();
    let result = if same && matches!(event, "never" | "cancelled") {
        assert!(
            first
                .unwrap_err()
                .message
                .contains("expired attempt is isolated")
        );
        assert!(b.state.lock().unwrap().in_flight_spills.is_empty());
        bounded(b.apply_to_message(&message(id, "YYYYYYYY"), Some(&"t".into())))
            .await
            .unwrap()
    } else {
        first.unwrap()
    };
    let r = reference(&result);
    let ts_preview = if same && event == "sequential" {
        "XXXXXXXX"
    } else {
        "YYYYYYYY"
    };
    let ts_bytes = if relation == "same-id" {
        if event == "sequential" {
            "XXXXXXXX"
        } else {
            "XXXX"
        }
    } else {
        "YYYYYYYY"
    };
    assert_eq!(row["preview"], ts_preview);
    assert_eq!(row["bytes"], ts_bytes);
    assert_eq!(row["aliasBytes"], ts_bytes);
    assert!(
        row["path"]
            .as_str()
            .unwrap()
            .starts_with("<root>/.pilotdeck/tool-results/")
    );
    assert!(
        row["readFilePath"]
            .as_str()
            .unwrap()
            .starts_with(".pilotdeck/tool-results/refs/")
    );
    let expected = if same && matches!(event, "sequential" | "concurrent") {
        "XXXXXXXX"
    } else {
        "YYYYYYYY"
    };
    assert_eq!(field(r, "preview"), expected);
    assert_eq!(fs::read(field(r, "path")).unwrap(), expected.as_bytes());
    assert_eq!(
        fs::read(scratch.path().join(field(r, "readFilePath"))).unwrap(),
        expected.as_bytes()
    );
    assert!(PathBuf::from(field(r, "path")).starts_with(b.tool_results_dir().to_utf8().unwrap()));
    println!(
        "{name}: path={} readFilePath={} preview={} bytes={expected}; TS={row}",
        field(r, "path"),
        field(r, "readFilePath"),
        field(r, "preview")
    );
    drop(release);
    if let Some(leader) = leader {
        let result = bounded(leader).await.unwrap();
        if same && event == "never" {
            assert!(result.is_err());
        } else {
            result.unwrap();
        }
    }
    if let Some(ticket) = ticket {
        until(|| Arc::strong_count(&ticket) == 1).await;
    }
    until(|| a.state.lock().unwrap().in_flight_spills.is_empty()).await;
    assert_eq!(fs::read(field(r, "path")).unwrap(), expected.as_bytes());
    assert_eq!(
        bounded(b.apply_to_message(&message(id, "later"), Some(&"t".into())))
            .await
            .unwrap(),
        result
    );
}
macro_rules! cases { ($($name:ident: $rel:literal, $event:literal, $two:literal;)*) => {$(
    #[tokio::test] async fn $name() { matrix($rel,$event,$two).await; }
)*}; }
cases! {
    same_id_sequential_one: "same-id", "sequential", false;
    same_id_sequential_two: "same-id", "sequential", true;
    same_id_concurrent_one: "same-id", "concurrent", false;
    same_id_concurrent_two: "same-id", "concurrent", true;
    same_id_never_one: "same-id", "never", false;
    same_id_never_two: "same-id", "never", true;
    same_id_cancelled_one: "same-id", "cancelled", false;
    same_id_cancelled_two: "same-id", "cancelled", true;
    different_id_sequential_one: "different-id", "sequential", false;
    different_id_sequential_two: "different-id", "sequential", true;
    different_id_concurrent_one: "different-id", "concurrent", false;
    different_id_concurrent_two: "different-id", "concurrent", true;
    different_id_never_one: "different-id", "never", false;
    different_id_never_two: "different-id", "never", true;
    different_id_cancelled_one: "different-id", "cancelled", false;
    different_id_cancelled_two: "different-id", "cancelled", true;
    different_root_sequential_one: "different-root", "sequential", false;
    different_root_sequential_two: "different-root", "sequential", true;
    different_root_concurrent_one: "different-root", "concurrent", false;
    different_root_concurrent_two: "different-root", "concurrent", true;
    different_root_never_one: "different-root", "never", false;
    different_root_never_two: "different-root", "never", true;
    different_root_cancelled_one: "different-root", "cancelled", false;
    different_root_cancelled_two: "different-root", "cancelled", true;
}

// Both claim-time expiry and old-owner Drop run while the new owner is parked.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn media_stale_owner_cannot_revoke_successor() {
    use std::sync::atomic::AtomicUsize;
    let _order = SPILL_HOOK_ORDER.lock().unwrap();
    let _bound = SpillWaitTimeoutOverride::new(Duration::from_millis(200));
    let scratch = crate::test_scratch::Scratch::new("spill-aba");
    let mut options = ToolResultBudgetOptions::new(scratch.path().join("spill"));
    options.max_result_size_chars = Some(1);
    let budget = Arc::new(ToolResultBudget::new(options));
    let msg = pilotdeck_protocol::parse_json(
        r#"{"role":"user","content":[{"type":"image","mimeType":"image/png","data":"abcdefgh"}]}"#,
    )
    .unwrap();
    let gates = [
        Arc::new((Mutex::new(false), Condvar::new())),
        Arc::new((Mutex::new(false), Condvar::new())),
    ];
    let releases = [
        ReleaseSpillGate(gates[0].clone()),
        ReleaseSpillGate(gates[1].clone()),
    ];
    let entered = Arc::new(AtomicUsize::new(0));
    let (calls, hooks) = (entered.clone(), gates.clone());
    let prefix = budget.spill_directory().to_utf8().unwrap();
    MID_SPILL_WRITE
        .lock()
        .unwrap()
        .replace(Arc::new(move |path| {
            if path.to_utf8().unwrap().starts_with(&prefix) {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                if n < 2 {
                    let (lock, cv) = &*hooks[n];
                    let mut open = lock.lock().unwrap();
                    while !*open {
                        open = cv.wait(open).unwrap();
                    }
                }
            }
            None
        }));
    let _reset = ResetSpillHook;
    let launch = || {
        let (b, m) = (budget.clone(), msg.clone());
        tokio::spawn(async move {
            b.apply_to_supplemental_message(&m, Some(&"c".into()), None)
                .await
        })
    };
    let old = launch();
    until(|| entered.load(Ordering::SeqCst) == 1).await;
    // Deterministically expire the real registered lease before a follower claims.
    let key = budget
        .state
        .lock()
        .unwrap()
        .in_flight_spills
        .keys()
        .next()
        .unwrap()
        .clone();
    let old_ticket = budget.state.lock().unwrap().in_flight_spills[&key].clone();
    until(|| old_ticket.started.elapsed() >= Duration::from_millis(200)).await;
    let mut next = launch();
    tokio::select! {
        result=&mut next => panic!("stale claim failed before successor gate: {result:?}"),
        _=until(|| entered.load(Ordering::SeqCst) == 2) => {}
    }
    let successor = budget.state.lock().unwrap().in_flight_spills[&key].clone();
    *gates[0].0.lock().unwrap() = true;
    gates[0].1.notify_all();
    assert!(bounded(old).await.unwrap().is_err());
    let successor_survived = budget
        .state
        .lock()
        .unwrap()
        .in_flight_spills
        .get(&key)
        .is_some_and(|ticket| Arc::ptr_eq(ticket, &successor));
    assert!(
        successor_survived,
        "old owner removed the live successor ticket"
    );
    let third = launch();
    until(|| SPILL_WAITERS.load(Ordering::SeqCst) > 0).await;
    drop(releases);
    let value = bounded(next).await.unwrap().unwrap();
    let path = field(reference(&value), "path");
    assert_ne!(path, key.to_utf8().unwrap());
    assert_eq!(bounded(third).await.unwrap().unwrap(), value);
    assert_eq!(fs::read(path).unwrap(), b"abcdefgh");
}

#[tokio::test]
async fn media_two_budgets_use_distinct_context_paths() {
    let scratch = crate::test_scratch::Scratch::new("spill-media-contexts");
    let make = || {
        ToolResultBudget::new(ToolResultBudgetOptions {
            tool_results_dir: scratch.path().join("spill"),
            max_result_size_chars: Some(1),
            max_result_size_tokens: None,
            preview_bytes: None,
        })
    };
    let msg = pilotdeck_protocol::parse_json(
        r#"{"role":"user","content":[{"type":"image","mimeType":"image/png","data":"abcdefgh"}]}"#,
    )
    .unwrap();
    let (a, b) = (make(), make());
    let call: JsValue = "c".into();
    let (ar, br) = tokio::join!(
        a.apply_to_supplemental_message(&msg, Some(&call), None),
        b.apply_to_supplemental_message(&msg, Some(&call), None)
    );
    let (ar, br) = (ar.unwrap(), br.unwrap());
    assert_ne!(field(reference(&ar), "path"), field(reference(&br), "path"));
    for value in [ar, br] {
        assert_eq!(
            fs::read(field(reference(&value), "path")).unwrap(),
            b"abcdefgh"
        );
    }
}
