#!/usr/bin/env python3
"""Sequential product mutations; always restore original bytes, never touch TS.
Run only in the task's exclusively owned worktree, with no concurrent cargo/edit.
"""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[5]
EVIDENCE = Path(sys.argv[1]).resolve()
EVIDENCE.mkdir(parents=True, exist_ok=True)
PRODUCT = ROOT / 'src/pilotdeck-gateway/src/runtime/replacement.rs'
OWNER = ROOT / 'src/pilotdeck-gateway/src/runtime/session/owner.rs'
ORIGINAL = {path: path.read_text() for path in (PRODUCT, OWNER)}
PREFIX = 'runtime::replacement::tests::'
# Every mutation is independently compiled and run against one targeted test.
MUTATIONS = [
    ('writer_pending_guard', PRODUCT,
     'if state.reservations.contains(key) || state.pending.contains_key(key) {',
     'if state.reservations.contains(key) {', PREFIX + 'writer_reservations'),
    ('replace_write_reservation', PRODUCT,
     'state.reservations.insert(key.clone());\n            active',
     '// mutation: omit replace reservation\n            active', PREFIX + 'concurrent_replace_and_writer'),
    ('unicode_whitespace', PRODUCT, ' | 0xfeff)', ')', PREFIX + 'invalid_replacement_ids'),
    ('active_run_conflict', PRODUCT, '&& *active != string(&input, "expectedTurnId")',
     '&& false && *active != string(&input, "expectedTurnId")', PREFIX + 'active_conflict_and_empty'),
    ('abort_old_turn', PRODUCT,
     'let abort = active.map(|run| port(|| self.0.session.abort_expected_turn(&key, &run)));',
     'let abort = active.filter(|_| false).map(|run| port(|| self.0.session.abort_expected_turn(&key, &run)));',
     PREFIX + 'replace_failure_abort'),
    ('drain_before_rewrite', PRODUCT, 'port(|| this.0.session.close_session(&key)).await?;',
     '// mutation: rewrite without draining writer', PREFIX + 'claim_release_rollback'),
    ('failure_releases_write', PRODUCT, 'state.reservations.remove(&key);\n            result',
     '// mutation: retain failed write reservation\n            result', PREFIX + 'replace_failure_close'),
    ('claim_run_identity', PRODUCT,
     'if pending.replacement_turn_id != *run || pending.phase != Phase::Prepared {',
     'if pending.phase != Phase::Prepared {', PREFIX + 'replace_abort_close_commit'),
    ('claim_release_phase', PRODUCT,
     'pending.phase = Phase::Prepared;\n            self.schedule_locked(&mut state, key);',
     '// mutation: keep submitting\n            self.schedule_locked(&mut state, key);',
     PREFIX + 'claim_release_rollback'),
    ('accepted_commit_action', PRODUCT, 'finalize_input(key, pending, "commit")',
     'finalize_input(key, pending, "rollback")', PREFIX + 'optional_project'),
    ('accepted_failure_clears_pending', PRODUCT,
     'this.0.state.lock().unwrap().pending.remove(&key);\n            Ok(JsValue::Null)\n        });\n        Box::pin(async move {\n            let _ = commit.await;\n        })\n    }\n    pub fn finalize_last_turn_replacement',
     '// mutation: retain stale accepted transaction\n            Ok(JsValue::Null)\n        });\n        Box::pin(async move {\n            let _ = commit.await;\n        })\n    }\n    pub fn finalize_last_turn_replacement',
     PREFIX + 'replace_abort_close_commit'),
    ('transaction_identity', PRODUCT,
     '.filter(|p| p.transaction_id == string(&input, "transactionId"))',
     '.filter(|_| true)', PREFIX + 'transaction_identity'),
    ('finalize_phase', PRODUCT, 'if pending.phase != Phase::Prepared {',
     'if false && pending.phase != Phase::Prepared {', PREFIX + 'replace_abort_close_commit'),
    ('rollback_active_guard', PRODUCT, 'if active {\n                this.reset_prepared',
     'if false && active {\n                this.reset_prepared', PREFIX + 'finalize_active_retry'),
    ('finalize_failure_retries', PRODUCT,
     '} else {\n                this.reset_prepared(&key, &identity);\n            }\n            result',
     '} else {\n                // mutation: leave finalizing\n            }\n            result', PREFIX + 'rollback_failure_finalize'),
    ('timeout_action', PRODUCT, 'finalize_input(&key, pending, "rollback")',
     'finalize_input(&key, pending, "commit")', PREFIX + 'timeout_rollback'),
    ('timeout_active_guard', PRODUCT, 'if active {\n                self.schedule_locked',
     'if false && active {\n                self.schedule_locked', PREFIX + 'timeout_active_retry'),
    ('timeout_failure_retries', PRODUCT,
     'this.reset_prepared(&key, &identity);\n                }\n            }\n        });',
     '// mutation: do not rearm failed expiry\n                }\n            }\n        });', PREFIX + 'timeout_failure_retry'),
    ('dispose_clears_pending', PRODUCT, 'state.pending.clear();',
     '// mutation: preserve pending on disposal', PREFIX + 'dispose_cancels_timeout'),
    ('fork_verbatim', PRODUCT, 'Some(fork) => running(port(|| fork(input))),',
     'Some(fork) => { let _ = input; running(port(|| fork(JsValue::Null))) },', PREFIX + 'fork_forward_and_failure'),
    ('required_finalize_port', PRODUCT,
     'finalize_last_turn_replacement is required when replace_last_turn is configured.',
     'broken missing finalize error', PREFIX + 'missing_ports'),
    ('maintenance_shutdown_vs_busy', OWNER,
     'return Err(RuntimeError::new("Session router is shut down."));\n        }\n        Ok(self\n',
     'return Ok(None);\n        }\n        Ok(self\n',
     'runtime::session::owner::tests::maintenance_shutdown_is_distinct_from_busy'),
    ('stale_finalize_identity', PRODUCT, '&& Arc::ptr_eq(&pending.identity, identity)',
     '&& true', PREFIX + 'stale_finalize_failure_preserves_new_claim'),
    ('stale_timer_identity', PRODUCT, 'Arc::ptr_eq(token, &timer_token)',
     '{ let _ = (token, &timer_token); true }', PREFIX + 'stale_timer_cannot_consume_rearmed_transaction'),
    ('dropped_promise_continues', PRODUCT,
     'let task = tokio::spawn(future);\n    Box::pin(async move {\n        task.await\n            .unwrap_or_else(|error| Err(ReplacementError::new(error.to_string())))\n    })',
     'Box::pin(future)', PREFIX + 'dropped_caller_does_not_strand_reservation'),
    ('real_owner_writer_drain', PRODUCT, 'let close = self.close(key);',
     'let _ = key; let close = std::future::ready(Ok::<(), super::project::RuntimeError>(()));',
     'runtime::session::owner::tests::replacement_adapter_drains_real_owner_before_storage'),
]

# Guard every anchor before mutating anything; an unexpected source version fails closed.
for name, path, before, after, test in MUTATIONS:
    count = ORIGINAL[path].count(before)
    # Each mutation must target exactly one implementation site.
    expected = 1
    assert count == expected, (name, count, expected)

summary = []
for name, path, before, after, test in MUTATIONS:
    while True:
        slots = subprocess.run(['/Users/da/ws/pilotdeck-rs/scripts/cargo-slots.sh'], capture_output=True, text=True)
        if slots.returncode == 0:
            break
        if slots.returncode != 1:
            raise RuntimeError(f'cargo slots failed: {slots.stdout}\n{slots.stderr}')
        time.sleep(5)
    mutated = ORIGINAL[path].replace(before, after, 1)
    assert path.read_text() == ORIGINAL[path], f'concurrent edit: {path}'
    log = EVIDENCE / f'{name}.log'
    print(f'RUN {name}: {test}', flush=True)
    try:
        path.write_text(mutated)
        with log.open('w') as output:
            output.write(slots.stdout + slots.stderr)
            output.flush()
            proc = subprocess.run(['cargo', 'test', '--offline', '-p', 'pilotdeck-gateway', '--lib', test, '--', '--exact'],
                                  cwd=ROOT / 'src', stdout=output, stderr=subprocess.STDOUT, timeout=240)
        text = log.read_text(errors='replace')
        assert proc.returncode == 101 and f'test {test} ... FAILED' in text and '1 failed;' in text, \
            f'{name}: expected a compiled failing test; see {log}'
        assert 'could not compile' not in text and 'running 1 test' in text, name
        row = {'mutation': name, 'test': test, 'exit': proc.returncode,
               'failure': next((line.strip() for line in text.splitlines() if 'panicked at' in line), ''),
               'source': str(path.relative_to(ROOT)), 'before': before, 'after': after,
               'originalSha256': hashlib.sha256(ORIGINAL[path].encode()).hexdigest()}
        summary.append(row)
        print(f'RED {name}: 0 passed; 1 failed', flush=True)
    finally:
        assert path.read_text() == mutated, f'concurrent source change: {path}; preserve it for owner review'
        path.write_text(ORIGINAL[path])
        assert path.read_text() == ORIGINAL[path]
        (EVIDENCE / 'summary.json').write_text(json.dumps(summary, ensure_ascii=False, indent=2) + '\n')
print(f'CONFIRMED {len(summary)} independent compiled failures; source bytes restored', flush=True)
