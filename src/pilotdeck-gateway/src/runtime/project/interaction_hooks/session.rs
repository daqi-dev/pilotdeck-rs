//! Session lifecycle authority is distinct from the sidecar turn authority.
//! User abort is an executor signal, not revocation: SessionEnd still runs.
use super::*;

pub struct SessionHooks(TurnHooks);
impl InteractionHooks {
    pub fn bind_session(
        &self,
        mut binding: lifecycle::Binding,
        run_id: JsString,
        settings: HooksSettings,
    ) -> Result<SessionHooks, HostError> {
        let signal = binding.signal.take();
        let scope = Self {
            state: self.sessions.clone(),
            sessions: Arc::default(),
            permission: self.permission.clone(),
        };
        let hooks = scope.bind_turn(
            binding,
            run_id,
            settings,
            None,
            &mut ModuleRouter::default(),
        )?;
        // Revocation still signals and drains an admitted executor; ordinary
        // abort must not close this scope before the lifecycle tail runs.
        let state = lock(&hooks.state);
        hooks.check(&state)?;
        *lock(&hooks.run.session_signal) = signal;
        drop(state);
        Ok(SessionHooks(hooks))
    }
}
impl lifecycle::LifecyclePort for SessionHooks {
    fn dispatch(&self, input: LifecycleDispatchInput) -> HostFuture<LifecycleDispatchResult> {
        self.0.dispatch_owned(input, true)
    }
    fn dispose(&self) -> HostFuture<()> {
        lifecycle::LifecyclePort::dispose(&self.0)
    }
}
impl Drop for SessionHooks {
    fn drop(&mut self) {
        self.0.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::modules::lifecycle::LifecyclePort;
    use tokio::{
        sync::Semaphore,
        time::{Duration, timeout},
    };
    fn binding(signal: Cancellation) -> lifecycle::Binding {
        lifecycle::Binding {
            session_id: "session-743".into(),
            turn_id: "turn-1".into(),
            cwd: "/tmp".into(),
            permission_mode: None,
            base_env: None,
            signal: Some(signal),
        }
    }
    fn input() -> LifecycleDispatchInput {
        LifecycleDispatchInput {
            event: "SessionEnd".into(),
            base_input: JsObject::new(),
            payload: Some(JsObject::new()),
            match_query: Some("SessionEnd".into()),
            cwd: "/tmp".into(),
            env: None,
            signal: None,
        }
    }
    fn settings() -> HooksSettings {
        vec![(
            "SessionEnd".into(),
            vec![HookMatcher {
                hooks: vec![HookCommand::Callback {
                    name: "held".into(),
                    condition: None,
                    status_message: None,
                }],
                ..Default::default()
            }],
        )]
    }
    struct Held {
        entered: Arc<Semaphore>,
        release: Arc<Semaphore>,
    }
    impl CallbackHookHandler for Held {
        fn call(&self, input: CallbackInput) -> CallbackFuture {
            let entered = self.entered.clone();
            let release = self.release.clone();
            Box::pin(async move {
                assert!(input.signal.is_some(), "#743 session scope signal supplied");
                entered.add_permits(1);
                release
                    .acquire()
                    .await
                    .expect("#743 session scope release")
                    .forget();
                Ok(CallbackResult::Void)
            })
        }
    }
    async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
        timeout(Duration::from_secs(5), future)
            .await
            .expect("#743 session scope bounded")
    }
    #[tokio::test]
    async fn session_abort_does_not_revoke_admitted_end() {
        let resource = InteractionHooks::new(InteractionPermission::default());
        let signal = Cancellation::new();
        let session = resource
            .bind_session(binding(signal.clone()), "run".into(), settings())
            .expect("#743 session scope bind");
        let entered = Arc::new(Semaphore::new(0));
        let release = Arc::new(Semaphore::new(0));
        let _registration = session
            .0
            .run
            .runtime
            .hooks()
            .callback_executor()
            .register(
                &"held".into(),
                Arc::new(Held {
                    entered: entered.clone(),
                    release: release.clone(),
                }),
            )
            .expect("#743 session scope registration");
        let mut dispatch = session.dispatch(input());
        tokio::select! {_=bounded(entered.acquire())=>{},result=&mut dispatch=>panic!("#743 session scope entered before finish {result:?}")}
        signal.abort();
        tokio::task::yield_now().await;
        crate::negative_assert!(
            !session.0.run.closed.load(Ordering::SeqCst),
            "#743 session abort retains End authority"
        );
        assert!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(dispatch.as_mut().poll(cx)))
                .await
                .is_pending(),
            "#743 session scope holds ignored abort"
        );
        release.add_permits(1);
        crate::test_negative_receipt::expect_ok(
            bounded(dispatch).await,
            "#743 session abort returns actual hook result",
        );
        bounded(session.dispose())
            .await
            .expect("#743 session scope dispose");
    }
    #[tokio::test]
    async fn aborted_replacement_does_not_wait_for_unfinished_predecessor() {
        let resource = InteractionHooks::new(InteractionPermission::default());
        let old = resource
            .bind_session(binding(Cancellation::new()), "old".into(), settings())
            .expect("#743 session barrier old bind");
        let entered = Arc::new(Semaphore::new(0));
        let release = Arc::new(Semaphore::new(0));
        let _registration = old
            .0
            .run
            .runtime
            .hooks()
            .callback_executor()
            .register(
                &"held".into(),
                Arc::new(Held {
                    entered: entered.clone(),
                    release: release.clone(),
                }),
            )
            .expect("#743 session barrier callback");
        let mut dispatch = old.dispatch(input());
        tokio::select! {_=bounded(entered.acquire())=>{},result=&mut dispatch=>panic!("#743 session barrier held predecessor {result:?}")}
        drop(dispatch); // Cancels receive, leaves admitted callback owned by cleanup.
        let signal = Cancellation::new();
        let next = resource
            .bind_session(binding(signal.clone()), "next".into(), vec![])
            .expect("#743 session barrier next bind");
        let mut pending = next.dispatch(input());
        assert!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(pending.as_mut().poll(cx)))
                .await
                .is_pending(),
            "#743 session barrier predecessor blocks"
        );
        signal.abort();
        let result = timeout(Duration::from_secs(2), pending).await;
        release.add_permits(1); // Always release the predecessor before assertions.
        crate::negative_assert!(
            matches!(result, Ok(Err(_))),
            "#743 session barrier cancellation terminates wait"
        );
        bounded(next.dispose())
            .await
            .expect("#743 session barrier next close");
        bounded(resource.dispose())
            .await
            .expect("#743 session barrier generation close");
    }
    #[tokio::test]
    async fn preaborted_session_end_command_still_executes() {
        let root = std::env::temp_dir().join(format!("pdrs-session-743-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("#743 preabort command directory");
        let config=pilotdeck_protocol::parse_json(r#"{"SessionEnd":[{"hooks":[{"type":"command","command":"printf completed > completed"}]}]}"#).expect("#743 preabort command config");
        let resource = InteractionHooks::new(InteractionPermission::default());
        let signal = Cancellation::new();
        signal.abort();
        let session = resource
            .bind_session(
                binding(signal),
                "run".into(),
                crate::hooks::config::parse_hooks_config(&config).settings,
            )
            .expect("#743 preabort command bind");
        let mut request = input();
        request.cwd = root.to_string_lossy().as_ref().into();
        let result = bounded(session.dispatch(request)).await;
        let result =
            crate::test_negative_receipt::expect_ok(result, "#743 preabort End dispatch admitted");
        crate::negative_assert!(
            result
                .non_blocking_errors
                .iter()
                .any(|error| error.code == "hook_cancelled"),
            "#743 preabort End retains signal"
        );
        crate::negative_assert_eq!(
            std::fs::read_to_string(root.join("completed"))
                .ok()
                .as_deref(),
            Some("completed"),
            "#743 preabort End command completed"
        );
        bounded(session.dispose())
            .await
            .expect("#743 preabort command dispose");
        std::fs::remove_dir_all(root).expect("#743 preabort command cleanup");
    }
}
