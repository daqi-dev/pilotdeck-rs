#!/usr/bin/env python3
"""#743 HEAD-only nonce controls. --check is read-only; commit before mutations.

Each selected mutant is compiled, run, restored and accounted for by the shared
matrix executor. Use the caller's CARGO_TARGET_DIR; never create a shadow repo.
The full suite is a coordinator job after applying/committing the delivery.
"""
from pathlib import Path
import sys

root = Path(__file__).resolve().parents[5]
fixtures = root / 'src/pilotdeck-gateway/tests/fixtures'
sys.path.insert(0, str(fixtures))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import matrix_main

shell = root / 'src/pilotdeck-gateway/src/runtime/turn/live_session.rs'
live = root / 'src/pilotdeck-gateway/src/runtime/turn/live.rs'
hooks = root / 'src/pilotdeck-gateway/src/runtime/project/interaction_hooks.rs'
handle = root / 'src/pilotdeck-gateway/src/runtime/session/handle.rs'
steer = root / 'src/pilotdeck-gateway/src/runtime/project/interaction_steer.rs'
writer = root / 'src/pilotdeck-gateway/src/storage/transcript_adapter.rs'
runner = root / 'src/pilotdeck-gateway/src/runtime/turn/runner.rs'
production = 'runtime::turn::host_factory::tests::session_hooks_wrap_real_turn_and_retain_lease_through_end'
session_tests = 'runtime::turn::live_session::tests::'
test = 'runtime::turn::live_session::tests::session_matrix_matches_ts'
scope = 'runtime::project::interaction_hooks::session::tests::'
# Literal six-field rows are also consumed by anchor_selfcheck.py.
cases = [
    ('skip-end', shell, '.hook("SessionEnd", &[("reason", self.end_reason)])', '.hook("MissingEnd", &[("reason", self.end_reason)])', test, '#743 session matrix start-success'),
    ('skip-start', shell, 'self.hook("SessionStart", &[("source", "startup")])', 'self.hook("MissingStart", &[("source", "startup")])', test, '#743 session matrix start-success'),
    ('skip-setup', shell, 'self.hook("Setup", &[])', 'self.hook("MissingSetup", &[])', test, '#743 session matrix start-success'),
    ('start-source', shell, '"source", "startup"', '"source", "resume"', test, '#743 session matrix start-success'),
    ('session-ended-event', shell, 'self.event("session_ended")', 'self.event("missing_session_ended")', test, '#743 session matrix start-success'),
    ('drop-current-turn-clear', live, '        self.current_turn_id = None;\n        if self.status ==', '        // mutation: retain current turn\n        if self.status ==', test, '#743 session matrix start-success'),
    ('aborted-result-status', live, 'Some(kind) if kind.equals_str("aborted") => super::live_session::SessionStatus::Aborted,', 'Some(kind) if kind.equals_str("aborted") => super::live_session::SessionStatus::Idle,', test, '#743 session matrix abort-start-cooperative'),
    ('error-result-status', live, 'Some(kind) if kind.equals_str("error") => super::live_session::SessionStatus::Failed,', 'Some(kind) if kind.equals_str("error") => super::live_session::SessionStatus::Idle,', test, '#743 session matrix turn-model-failure'),
    ('runner-error-preserves-running', shell, 'self.state.lock().unwrap().fail();', 'let _ = &self.state;', test, '#743 session matrix turn-runner-throw'),
    ('session-signal-lost', hooks, 'input.signal = Some(signal);', 'input.signal = Some(Cancellation::new());', scope+'preaborted_session_end_command_still_executes', '#743 preabort End retains signal'),
    ('barrier-cancel-lost', hooks, '_ = signal.cancelled() => return Err(stale()),', '_ = std::future::pending::<()>() => return Err(stale()),', scope+'aborted_replacement_does_not_wait_for_unfinished_predecessor', '#743 session barrier cancellation terminates wait'),
    ('completion-delays-steer-close', runner, '        self.close_session_steer().await?;\n        self.finish_artifacts(&result).await;\n        let written = result.clone();\n        if let Err(error) = self.io(move |r, s, t| r.complete_turn(s, t, written)).await {\n            return self\n                .fail(object([\n                    ("code", "agent_invalid_state".into()),\n                    ("message", JsValue::String(error.to_string().into())),\n                ]))\n                .await;\n        }\n', '        self.finish_artifacts(&result).await;\n        let written = result.clone();\n        if let Err(error) = self.io(move |r, s, t| r.complete_turn(s, t, written)).await {\n            return self\n                .fail(object([\n                    ("code", "agent_invalid_state".into()),\n                    ("message", JsValue::String(error.to_string().into())),\n                ]))\n                .await;\n        }\n        self.close_session_steer().await?;\n', production, '#743 turn_result waits durable steer close'),
    ('finish-before-terminal', steer, 'session_finishes: session_owned,', 'session_finishes: false,', production, '#743 finish occurs after terminal yield'),
    ('skip-finish-before-end', shell, 'lifecycle.finish_steer();', 'let _ = lifecycle;', production, '#743 finish before End'),
    ('owner-slot-kept-after-none', handle, 'self._activity.take();', 'let _ = &self._activity;', session_tests+'owner_holds_end_and_releases_at_none', '#743 owner release on None'),
    ('writer-close-does-not-fence', writer, 'self.closed.store(true, Ordering::SeqCst);', 'self.closed.store(false, Ordering::SeqCst);', session_tests+'dispose_running_signals_then_closes_writer', '#743 dispose closes writer'),
    ('dispose-loses-abort-signal', live, '            state.signal.abort();', '            // mutation: no session abort signal', session_tests+'dispose_running_signals_then_closes_writer', '#743 dispose aborts running signal'),
    ('recompute-end-reason', shell, 'self.phase = Phase::Ended;', 'self.end_reason = if self.state.lock().unwrap().status == SessionStatus::Aborted { "other" } else { "prompt_input_exit" }; self.phase = Phase::Ended;', test, '#743 session matrix abort-end-cooperative'),
    ('drop-needs-current-runtime', shell, 'spawn_after(&self.executor, done, || {}, cleanup);', 'spawn_after(&tokio::runtime::Handle::current(), done, || {}, cleanup);', session_tests+'lifecycle_drop_off_runtime_uses_captured_executor', '#743 off-runtime lifecycle drop cannot panic'),
    ('shutdown-bare-spawn', shell, 'crate::runtime::cleanup_task::spawn_after(&self.executor, done, || {}, cleanup);', 'self.executor.spawn(async move { done.send_replace(Some(cleanup.await)); });', session_tests+'lifecycle_shutdown_drop_settles_without_unwind', '#743 shutdown completion guard settles unpolled cleanup'),
    ('ordinary-hook-error-rejects-session', shell, 'future.await.map(|_| ())', 'future.await.and_then(|result| if result.non_blocking_errors.is_empty() && result.blocking_errors.is_empty() { Ok(()) } else { Err(ModuleError::new("mutated hook error").into()) })', session_tests+'ordinary_hook_results_keep_session_tail', '#743 executor returned errors ignored'),
    ('drop-steer-unapplied', runner, '"steer_unapplied"', '"missing_steer_unapplied"', production, '#743 discarded steer emits unapplied once'),
]
if __name__ == '__main__':
    raise SystemExit(matrix_main(root, __file__, '', cases, passed_count=1,
        child_count=0, paths=[shell, live, hooks, handle, steer, writer, runner,
        root / 'src/pilotdeck-gateway/src/runtime/turn/host_factory_tests.rs',
        root / 'src/pilotdeck-gateway/src/runtime/turn/interaction.rs',
        root / 'src/pilotdeck-gateway/tests/fixtures/live-session/peer.py',
        root / 'src/pilotdeck-gateway/src/runtime/turn/live_session_tests.rs',
        root / 'src/pilotdeck-gateway/src/runtime/project/interaction_hooks/session.rs'], exact=True))
