#!/usr/bin/env python3
"""#1123 deterministic Notify regressions; run only after committing the fix.

--check validates anchors without mutating sources. The shared matrix runner
requires HEAD-equal sources, compiles every mutant, runs all selected controls,
and checks restoration. CARGO_TARGET_DIR must be supplied by the caller.
"""
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
# matrix_main uses these APIs for guarded mutations and every cargo process.
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import matrix_main

ROOT = Path(__file__).resolve().parents[5]
TESTS = ROOT / 'src/pilotdeck-gateway/src/native_ws/tests.rs'
PREFIX = 'native_ws::tests::'
MUTATIONS = [
    ('late-release-registration', TESTS,
     '                let released = self.release.notified();\n'
     '                tokio::pin!(released);\n'
     '                released.as_mut().enable();\n'
     '                self.entered.notify_one();\n'
     '                if let Some(gate) = &self.after_entered {\n'
     '                    gate.acquire()\n'
     '                        .await\n'
     '                        .expect("#1123 after-entered gate stays open")\n'
     '                        .forget();\n'
     '                }\n'
     '                released.await;',
     '                self.entered.notify_one();\n'
     '                if let Some(gate) = &self.after_entered {\n'
     '                    gate.acquire()\n'
     '                        .await\n'
     '                        .expect("#1123 after-entered gate stays open")\n'
     '                        .forget();\n'
     '                }\n'
     '                let released = self.release.notified();\n'
     '                tokio::pin!(released);\n'
     '                released.as_mut().enable();\n'
     '                released.await;',
     [(PREFIX + 'requests_release_before_await_reaches_all_waiters',
       '#1123 release before await reaches every entered request')]),
    ('single-release-instead-of-broadcast', TESTS,
     'requests.release.notify_waiters(); // #1123 broadcast before either release await',
     'requests.release.notify_one(); // #1123 broadcast before either release await',
     [(PREFIX + 'requests_release_before_await_reaches_all_waiters',
       '#1123 release before await reaches every entered request')]),
    ('skip-after-entered-gate', TESTS,
     'if let Some(gate) = &self.after_entered {',
     'if let Some(gate) = &None::<Arc<Semaphore>> {',
     [(PREFIX + 'requests_release_before_await_reaches_all_waiters',
       '#1123 both requests registered at after-entered gate')]),
    ('skip-release-wait', TESTS,
     '                released.await;',
     '                let _ = released;',
     [(PREFIX + 'requests_release_after_await_reaches_all_waiters',
       '#1123 both requests await release')]),
    ('hold-next-request-too', TESTS,
     'self.hold.swap(false, Ordering::SeqCst)',
     'self.hold.load(Ordering::SeqCst)',
     [(PREFIX + 'requests_unreleased_or_cancelled_predecessor_does_not_block',
       '#1123 unheld successor bypasses unreleased predecessor')]),
]


def main():
    return matrix_main(ROOT, __file__, '', MUTATIONS,
                       passed_count=1, child_count=0, paths=[TESTS], exact=True)


if __name__ == '__main__':
    raise SystemExit(main())
