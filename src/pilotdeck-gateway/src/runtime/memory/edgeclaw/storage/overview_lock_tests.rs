//! Deterministic lock protocol regressions. Only the parent enforces time.
use super::*;
use crate::runtime::memory::edgeclaw::file_memory::{
    FileMemoryClock, lock_test_hook, operation_lock,
};
use std::{
    cell::Cell,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

const CHILD: &str = "runtime::memory::edgeclaw::storage::overview::lock_tests::scan_lock_child";
const ORACLE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/edgeclaw-export/lock-oracle.json"
));

#[test]
fn scan_lock_topologies_finish() {
    run_topologies(false);
}
#[test]
fn standalone_overview_lock_topologies_finish() {
    run_topologies(true);
}
fn run_topologies(standalone: bool) {
    for topology in ["general-single", "two-general", "three-general"] {
        for cache in ["cold", "warm"] {
            if std::env::var("PDRS_R9_PARENT_CASE")
                .is_ok_and(|case| case != format!("{topology}/{cache}"))
            {
                continue;
            }
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", CHILD, "--nocapture", "--test-threads=1"])
                .env("PDRS_R9_LOCK_CASE", topology)
                .env("PDRS_R9_LOCK_CACHE", cache)
                .env(
                    "PDRS_R9_LOCK_STANDALONE",
                    if standalone { "1" } else { "0" },
                )
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(20);
            let status = loop {
                if let Some(status) = child.try_wait().unwrap() {
                    break status;
                }
                if Instant::now() >= deadline {
                    child.kill().unwrap();
                    child.wait().unwrap();
                    let scratch = std::env::temp_dir().join(format!("pdrs-r9-lock-{}", child.id()));
                    let _ = std::fs::remove_dir_all(scratch);
                    panic!(
                        "{topology}/{cache}: child exceeded deadlock deadline (killed and reaped)"
                    );
                }
                // Polling only implements the parent kill deadline. It never
                // determines whether a thread reached a lock acquisition.
                std::thread::sleep(Duration::from_millis(10));
            };
            assert!(status.success(), "{topology}/{cache}: {status}");
        }
    }
}

#[test]
fn scan_lock_child() {
    let Ok(topology) = std::env::var("PDRS_R9_LOCK_CASE") else {
        return;
    };
    let warm = std::env::var("PDRS_R9_LOCK_CACHE").unwrap() == "warm";
    let standalone = std::env::var("PDRS_R9_LOCK_STANDALONE").is_ok_and(|v| v == "1");
    let root = std::env::temp_dir().join(format!("pdrs-r9-lock-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let count = if topology == "three-general" { 3 } else { 2 };
    let clock: FileMemoryClock = Arc::new(|| "2026-10-22T00:00:00.000Z".into());
    let roots = (0..count)
        .map(|i| root.join(format!("workspaces/{i}/memory")))
        .collect::<Vec<_>>();
    let repos = (0..count)
        .map(|i| {
            let general = root.join("home/.pilotdeck");
            let workspace = if topology == "general-single" && i == 1 {
                root.join("project-1")
            } else {
                general.clone()
            };
            let global = if topology == "three-general" && i == 1 {
                roots[2].clone()
            } else {
                root.join("zglobal")
            };
            let repo = EdgeClawFileStores::with_workspace_clock(
                root.join(format!("workspaces/{i}/control.sqlite")),
                &roots[i],
                global,
                workspace,
                general,
                clock.clone(),
            )
            .unwrap();
            repo.storage
                .set_pipeline_state(
                    "workspaceDir",
                    &s(root.join(format!("project-{i}")).to_string_lossy()),
                )
                .unwrap();
            Arc::new(repo)
        })
        .collect::<Vec<_>>();
    let call = |repo: &EdgeClawFileStores, i| {
        if topology == "three-general" && i == 1 {
            repo.list_memory_entries(&JsObject::new()).unwrap()
        } else {
            repo.get_ui_snapshot(None).unwrap()
        }
    };
    if warm {
        for (i, repo) in repos.iter().enumerate() {
            call(repo, i);
        }
    }
    let key = |path: &PathBuf| {
        let lock = operation_lock(path).unwrap();
        Arc::as_ptr(&lock) as usize
    };
    let keys = roots.iter().map(key).collect::<Vec<_>>();
    let g_key = key(&root.join("zglobal"));
    let (held_tx, held_rx) = mpsc::channel();
    let (edge_tx, edge_rx) = mpsc::channel();
    let mut resumes = vec![];
    let mut workers = vec![];
    for (i, repo) in repos.iter().enumerate() {
        let (go_tx, go_rx) = mpsc::channel();
        resumes.push(go_tx);
        let held_tx = held_tx.clone();
        let edge_tx = edge_tx.clone();
        let repo = repo.clone();
        let own = keys[i];
        let target = if i == 0 {
            keys[1]
        } else if topology == "three-general" && i == 1 {
            keys[2]
        } else if topology == "general-single" {
            g_key
        } else {
            keys[0]
        };
        let topology = topology.clone();
        workers.push(std::thread::spawn(move || {
            let parked = Cell::new(false);
            let saw_edge = Cell::new(false);
            lock_test_hook::set(move |event| {
                if event.acquired && event.key == own && !parked.replace(true) {
                    held_tx.send(i).unwrap();
                    // This is inside OperationGuard::enter, after the real W
                    // mutex was acquired. There is no schedule/time inference.
                    go_rx.recv().unwrap();
                }
                if !event.acquired && event.key == target && !saw_edge.replace(true) {
                    edge_tx.send((i, event.held)).unwrap();
                }
            });
            if topology == "three-general" && i == 1 {
                repo.list_memory_entries(&JsObject::new()).unwrap()
            } else if standalone {
                repo.get_overview().unwrap()
            } else {
                repo.get_ui_snapshot(None).unwrap()
            }
        }));
    }
    let mut held = (0..count)
        .map(|_| held_rx.recv().unwrap())
        .collect::<Vec<_>>();
    held.sort();
    eprintln!(
        "{topology}/{}: actual own-W acquired by {held:?}",
        if warm { "warm" } else { "cold" }
    );
    assert_eq!(held, (0..count).collect::<Vec<_>>());
    // A is known to hold its first own W. Resume until the ACTUAL sibling
    // acquisition; only then let B proceed. For the three-store cycle B is
    // additionally confirmed at W_B → G(=W_C) before C can proceed.
    resumes[0].send(()).unwrap();
    let a_edge = edge_rx.recv().unwrap();
    assert_eq!(a_edge.0, 0);
    eprintln!(
        "actual A→B acquisition reached; held roots = {:?}",
        a_edge.1
    );
    resumes[1].send(()).unwrap();
    let b_edge = edge_rx.recv().unwrap();
    assert_eq!(b_edge.0, 1);
    eprintln!(
        "actual B→{} acquisition reached; held roots = {:?}",
        if count == 3 {
            "C"
        } else if topology == "general-single" {
            "G"
        } else {
            "A"
        },
        b_edge.1
    );
    if count == 3 {
        resumes[2].send(()).unwrap();
    }
    let results = workers
        .into_iter()
        .map(|w| w.join().unwrap())
        .collect::<Vec<_>>();
    // Checked AFTER completion: restoring old code must deadlock at the same
    // edges and be killed by the parent, rather than fail an early assertion.
    assert!(
        a_edge.1.is_empty(),
        "A held a store while entering a sibling: {:?}",
        a_edge.1
    );
    let oracle: serde_json::Value = serde_json::from_str(ORACLE).unwrap();
    let expected = oracle["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["topology"] == topology)
        .unwrap();
    let actual: serde_json::Value = serde_json::from_str(
        &pilotdeck_protocol::stringify_json(&JsValue::Array(results))
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    // TS getUiSnapshot calls getOverview directly (sqlite.ts:1710). Extract
    // those actual TS return values for the standalone entry, not constants.
    let expected_observations = expected["observations"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .map(|(i, value)| {
            if standalone && !(topology == "three-general" && i == 1) {
                value["overview"].clone()
            } else {
                value.clone()
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(actual, serde_json::Value::Array(expected_observations));
    for (i, repo) in repos.iter().enumerate() {
        assert_eq!(
            repo.get_workspace_mode(),
            expected["modes"][i].as_str().unwrap()
        );
        repo.close().unwrap();
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn provider_and_composites_acquire_the_complete_root_set_in_order() {
    use std::cell::RefCell;
    let root = std::env::temp_dir().join(format!("pdrs-r9-order-{}", std::process::id()));
    let global = root.join("a-global");
    let workspace = root.join("z-workspace");
    let g = operation_lock(&global).unwrap();
    let w = operation_lock(&workspace).unwrap();
    let keys = [Arc::as_ptr(&g) as usize, Arc::as_ptr(&w) as usize];
    let acquired = std::rc::Rc::new(RefCell::new(Vec::new()));
    let events = acquired.clone();
    lock_test_hook::set(move |event| {
        if event.acquired {
            events.borrow_mut().push(event.key);
        }
    });
    let repo = EdgeClawFileStores::with_workspace_clock(
        root.join("control.sqlite"),
        workspace,
        global,
        root.join("project"),
        root.join("home/.pilotdeck"),
        Arc::new(|| "2026-10-22T00:00:00.000Z".into()),
    )
    .unwrap();
    // Global construction, then workspace construction with its dependency.
    assert_eq!(*acquired.borrow(), vec![keys[0], keys[0], keys[1]]);
    acquired.borrow_mut().clear();
    repo.workspace_memory.repair_manifests().unwrap();
    assert_eq!(*acquired.borrow(), keys, "provider must reenter preheld G");
    acquired.borrow_mut().clear();
    repo.get_ui_snapshot(None).unwrap();
    assert_eq!(
        *acquired.borrow(),
        keys,
        "overview, snapshot and list must reenter"
    );
    acquired.borrow_mut().clear();
    repo.export_memory_bundle().unwrap();
    assert_eq!(
        *acquired.borrow(),
        keys,
        "export uses the same dependency set"
    );
    acquired.borrow_mut().clear();
    repo.capture_current_memory_snapshot().unwrap();
    assert_eq!(
        *acquired.borrow(),
        keys,
        "snapshot/entries must use the same root order"
    );
    lock_test_hook::set(|_| {});
    repo.close().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
