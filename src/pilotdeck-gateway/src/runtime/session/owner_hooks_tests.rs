use super::*;

fn golden() -> Value {
    let raw = include_str!("../../../tests/fixtures/session-owner-hooks/oracle.json");
    crate::test_baseline::oracle(raw);
    serde_json::from_str(raw).unwrap()
}
fn js(value: &JsValue) -> Value {
    serde_json::from_str(&pilotdeck_protocol::stringify_json(value).unwrap().unwrap()).unwrap()
}

struct Hooks {
    mode: String,
    now: AtomicI64,
    reads: AtomicUsize,
    messages: AtomicUsize,
    events: Mutex<Vec<Value>>,
    gate: Gate,
    owner: Mutex<Weak<Inner>>,
}
impl Hooks {
    fn event(&self, event: Value) {
        self.events.lock().unwrap().push(event);
    }
    fn events(&self) -> Vec<Value> {
        self.events.lock().unwrap().clone()
    }
    fn owner(&self) -> SessionOwner {
        SessionOwner(self.owner.lock().unwrap().upgrade().unwrap())
    }
}
impl LiveSession for Hooks {
    fn snapshot(&self) -> JsValue {
        self.reads.fetch_add(1, Ordering::SeqCst);
        assert_ne!(self.mode, "throw", "snapshot failed");
        if self.mode == "normal" || self.mode == "dispose-error" || self.mode == "disabled" {
            return parse_json(&format!(
                "{{\"messages\":{}}}",
                json!(vec![0; self.messages.load(Ordering::SeqCst)])
            ))
            .unwrap();
        }
        if self.mode == "inherited-messages" {
            let mut snapshot = pilotdeck_protocol::JsObject::new();
            snapshot.set_prototype(parse_json(r#"{"messages":[1,2]}"#).unwrap());
            return JsValue::Object(snapshot);
        }
        if self.mode == "inherited-length" {
            let mut messages = pilotdeck_protocol::JsObject::new();
            messages.set_prototype(parse_json(r#"{"length":"two"}"#).unwrap());
            let mut snapshot = pilotdeck_protocol::JsObject::new();
            snapshot.insert("messages".into(), JsValue::Object(messages));
            return JsValue::Object(snapshot);
        }
        if self.mode == "undefined" {
            let mut messages = pilotdeck_protocol::JsObject::new();
            messages.insert("length".into(), JsValue::Undefined);
            let mut snapshot = pilotdeck_protocol::JsObject::new();
            snapshot.insert("messages".into(), JsValue::Object(messages));
            return JsValue::Object(snapshot);
        }
        parse_json(match self.mode.as_str() {
            "missing" => "{}",
            "string" => r#"{"messages":"😀"}"#,
            "length" => r#"{"messages":{"length":"two"}}"#,
            "null" => r#"{"messages":{"length":null}}"#,
            _ => r#"{"messages":{}}"#,
        })
        .unwrap()
    }
    fn abort(&self, _: &JsString) -> RuntimeResult {
        Ok(())
    }
    fn dispose(&self) -> DisposeFuture<'_> {
        Box::pin(async {
            self.event(json!("dispose"));
            self.gate.wait().await;
            self.event(json!("disposed"));
            self.messages.fetch_add(1, Ordering::SeqCst);
            if self.mode == "dispose-error" {
                Err(RuntimeError::new("dispose failed"))
            } else {
                Ok(())
            }
        })
    }
}
struct Factory(Arc<Hooks>);
impl SessionFactory for Factory {
    fn create(
        &self,
        _: SessionContext,
        _: Option<LiveSessionHandle>,
    ) -> BoxFuture<Result<LiveSessionHandle, RuntimeError>> {
        let handle = LiveSessionHandle::new(self.0.clone(), None);
        Box::pin(async { Ok(handle) })
    }
    fn can_recreate(&self) -> bool {
        true
    }
}
fn harness(mode: &str) -> (SessionOwner, Arc<Hooks>) {
    let h = Arc::new(Hooks {
        mode: mode.into(),
        now: AtomicI64::new(7),
        reads: AtomicUsize::new(0),
        messages: AtomicUsize::new(0),
        events: Mutex::new(vec![]),
        gate: Gate::new(),
        owner: Mutex::new(Weak::new()),
    });
    let mut options = SessionOwnerOptions::new(Arc::new(Factory(h.clone())));
    options.idle_sweep_interval_ms = 0;
    options.idle_timeout_ms = 100;
    let clock = h.clone();
    options.now_ms = Arc::new(move || clock.now.load(Ordering::SeqCst));
    let general = h.clone();
    options.on_evict = Some(Arc::new(move |_, reason| {
        // Readback proves callbacks run without the owner lock.
        general.owner().cached_session_count();
        general.messages.fetch_add(1, Ordering::SeqCst);
        general.event(json!(format!(
            "evict:{}",
            match reason {
                EvictionReason::Idle => "idle",
                EvictionReason::Closed => "closed",
                EvictionReason::DirtyRecreate => "dirty_recreate",
                EvictionReason::Shutdown => "shutdown",
            }
        )));
    }));
    if mode != "disabled" {
        let idle = h.clone();
        options.on_session_idle_evict = Some(Arc::new(move |key, s| {
            let mut snapshot = json!({"sessionKey": s.session_key.to_utf8().unwrap(), "lastUsedAt": s.last_used_at,
                "context": {"sessionKey": s.context.session_key.to_utf8().unwrap(),
                    "projectKey": s.context.project_key.as_ref().map(|v| v.to_utf8().unwrap()),
                    "channelKey": s.context.channel_key.to_utf8().unwrap(),
                    "allowedTools": s.context.allowed_tools.as_ref().map(|v| v.iter().map(|s| s.to_utf8().unwrap()).collect::<Vec<_>>()),
                    "disallowedTools": s.context.disallowed_tools.as_ref().map(|v| v.iter().map(|s| s.to_utf8().unwrap()).collect::<Vec<_>>()) }});
            if let Some(count) = &s.message_count {
                snapshot["messageCount"] = js(count);
            }
            idle.event(json!({"key": key.to_utf8().unwrap(), "snapshot": snapshot,
                "cached": idle.owner().cached_session_count(), "busy": idle.owner().has_active_user_turn(&"p".into())}));
        }));
    }
    let owner = SessionOwner::new(options);
    *h.owner.lock().unwrap() = Arc::downgrade(&owner.0);
    (owner, h)
}
fn ctx() -> SessionContext {
    let mut ctx = context("user");
    ctx.project_key = Some("p".into());
    ctx.allowed_tools = Some(vec!["read".into()]);
    ctx.disallowed_tools = Some(vec![]);
    ctx
}

#[tokio::test]
async fn active_user_turn_matches_ts() {
    for row in golden()["active"].as_array().unwrap() {
        let (owner, h) = harness("normal");
        let mut ctx = ctx();
        ctx.session_key = row["key"].as_str().unwrap().into();
        ctx.project_key = row["project"].as_str().map(JsString::from);
        if row["cached"] == true {
            owner.get_or_create(ctx.clone()).await.unwrap();
        }
        if row["turn"] == true {
            assert!(owner.begin_turn(&ctx.session_key, &"".into()).is_some());
        }
        assert_eq!(
            owner.has_active_user_turn(&row["query"].as_str().unwrap().into()),
            row["busy"],
            "active case {row}"
        );
        h.gate.open();
        owner.shutdown().await.unwrap();
    }
    let (owner, h) = harness("normal");
    let handle = owner.get_or_create(ctx()).await.unwrap();
    let maintenance = owner.begin_maintenance(&"user".into()).unwrap().unwrap();
    let activity = handle.begin_activity().unwrap();
    let completion = owner.begin_completion(&"user".into(), &"fence".into());
    assert!(
        !owner.has_active_user_turn(&"p".into()),
        "non-admission activity counted"
    );
    drop(activity);
    drop(maintenance);
    owner.complete_turn(&"user".into(), &completion);
    h.now.store(1000, Ordering::SeqCst);
    assert!(!owner.has_active_user_turn(&"p".into()));
    assert_eq!(owner.cached_session_count(), 1, "busy query must not sweep");
    h.gate.open();
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn idle_hook_matches_ts_with_turn_interleaving() {
    for row in golden()["idle"].as_array().unwrap() {
        let (owner, h) = harness(row["mode"].as_str().unwrap());
        owner.get_or_create(ctx()).await.unwrap();
        h.now.store(107, Ordering::SeqCst);
        owner.sweep_idle();
        assert_eq!(
            owner.cached_session_count(),
            row["boundary"],
            "strict timeout"
        );
        let key = JsString::from("user");
        let run = JsString::from("run");
        let reservation = owner.begin_turn(&key, &run).unwrap();
        h.now.store(108, Ordering::SeqCst);
        owner.sweep_idle();
        assert_eq!(
            json!({"cached": owner.cached_session_count(), "busy": owner.has_active_user_turn(&"p".into())}),
            row["during"]
        );
        owner.end_turn(&key, Some(&run), Some(&reservation));
        h.now.store(209, Ordering::SeqCst);
        owner.sweep_idle();
        let next = owner.begin_turn(&key, &"next".into()).unwrap();
        assert!(
            !owner.has_active_user_turn(&"p".into()),
            "admission after eviction has no record"
        );
        owner.end_turn(&key, Some(&"next".into()), Some(&next));
        // Current-thread executor has not polled the drain yet, as TS awaits its resource queue.
        assert_eq!(
            json!({"cached": owner.cached_session_count(), "busy": owner.has_active_user_turn(&"p".into()),
            "events": h.events(), "reads": h.reads.load(Ordering::SeqCst)}),
            row["draining"]
        );
        until(|| h.events().contains(&json!("dispose"))).await;
        assert_eq!(
            h.reads.load(Ordering::SeqCst),
            0,
            "snapshot before disposal finished"
        );
        assert_eq!(
            h.events(),
            vec![json!("dispose")],
            "callback before disposal finished"
        );
        h.gate.open();
        let _ = bounded(owner.shutdown()).await;
        assert_eq!(
            json!(h.events()),
            row["events"],
            "idle callback order/payload: {}",
            row["mode"]
        );
        assert_eq!(
            h.reads.load(Ordering::SeqCst),
            row["reads"],
            "snapshot optional callback"
        );
    }
}

#[tokio::test]
async fn non_idle_hooks_are_silent() {
    for row in golden()["nonIdle"].as_array().unwrap() {
        let (owner, h) = harness("normal");
        owner.get_or_create(ctx()).await.unwrap();
        h.gate.open();
        match row["action"].as_str().unwrap() {
            "close" => owner.close(&"user".into()).await.unwrap(),
            "recreate" => {
                owner.mark_all_dirty("reload");
                owner.get_or_create(ctx()).await.unwrap();
            }
            _ => owner.shutdown().await.unwrap(),
        }
        assert_eq!(
            h.events().iter().filter(|v| v.is_object()).count(),
            row["idleCalls"],
            "non-idle callback"
        );
        assert_eq!(
            h.reads.load(Ordering::SeqCst),
            row["reads"],
            "non-idle snapshot"
        );
        owner.shutdown().await.unwrap();
    }
}
