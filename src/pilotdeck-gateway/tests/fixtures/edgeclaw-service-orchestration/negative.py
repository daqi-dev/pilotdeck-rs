#!/usr/bin/env python3
"""Prove the service orchestration oracle checks fail on their own mutations.

The new `service/orchestration.rs`, the `with_i18n` detail order in
`dream/run.rs`, the replay test and its fixture must be committed before this
runner can obtain the `mutating_sources` HEAD guard. Until then it refuses
with the shared `SourceBaselineError` instead of mutating an uncommitted
product tree — 提交后由协调者在宿主跑.

    CARGO_TARGET_DIR=/abs/target python3 \
        src/pilotdeck-gateway/tests/fixtures/edgeclaw-service-orchestration/negative.py \
        _tmp/20261026-issue-776/negative-service-orchestration [mutant ...]

Every selected mutant is applied, compiled and run, then restored. The runner
does not stop at the first failure: it records KILLED / SURVIVED / INVALID for
all cases and exits non-zero if any case is not KILLED. Compile errors and
failures that do not land on the named scenario's `#776 … differs from the
real TS` assertion are INVALID. Without mutant names it runs the full set;
names are for debugging one binding.

fix-r1: `dream-tail-scope` is bound to the two-thread tail-race regression
instead of the oracle replay — its anchor is that test's own dedicated panic
message. `storage/snapshots.rs` joined the guarded source set for the
rollback-result mutant.

fix-r3: the tail-race regression moved to `tests/common/dream_tail_race.rs`
and is compiled into the library test binary (`--lib`), because the
`dream_tail_gate` seam is now `#[cfg(test)]`. `flush-preempt-mark-consumed`
binds the concurrent flush cell's `capturedSessions == 2` assertion, and
`wal-read-becomes-write` binds the real `control.sqlite` reader assertion.
`heartbeat/indexer.rs` joined the guarded source set.

#1036: the tail-race "second dream did not install" signal is now the
deterministic `try_lock` proof, so `dream-tail-scope`'s anchor is the new
"root operation lock was free" panic. The three new boundary cells bind
`interval-boundary-inclusive` / `interval-scale-ms` / `backlog-threshold-lower`.
`MemoryRepositoryStage` gained a `Drop` fallback (P2-5), which masks the old
skip-cleanup shapes: `stage-dispose-on-error` now removes only the fallback
`mark()` (double-counting `disposed`), `stage-dispose-skip-cleanup` no-ops the
shared `cleanup()` body, and the new `stage-drop-cleanup` /
`stage-cancel-counter` bind the
`dream_cancellation_disposes_stage_and_balances_counters` test.

fix-r1 P2-2: `MemoryRepositoryStage::dispose` resets the `disposed` flag when
`cleanup` fails, keeping TS's repeatable-cleanup semantics; the new
`dispose-retry-resets-flag` mutant binds the dedicated
`failed_dispose_retry_still_removes_staged_roots` cell in edgeclaw_snapshots.
`Drop` now keys on a separate explicit-attempt latch, so it still cleans a
cancelled dream's never-disposed stage (`stage-drop-cleanup` keeps that
binding) while a FAILED explicit dispose keeps its residue like TS —
`stage-drop-explicit-residue` binds the `restore-dispose-failure` oracle cell.
"""
import json
import os
from pathlib import Path
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import (
    SourceBaselineError,
    mutating_sources,
    run_owned,
    wait_for_slot,
)

ROOT = Path(__file__).resolve().parents[5]
ORCHESTRATION = ROOT / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/service/orchestration.rs'
DREAM_RUN = ROOT / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/dream/run.rs'
SNAPSHOTS = ROOT / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/storage/snapshots.rs'
STORAGE = ROOT / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/storage.rs'
INDEXER = (ROOT / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/'
           'heartbeat/indexer.rs')
TEST = 'rust_matches_ts_oracle'
TEST_FILE = 'pilotdeck-gateway/tests/edgeclaw_service_orchestration.rs'
# fix-r3 P1-1: `dream_tail_gate` is `#[cfg(test)]`, so an integration test can
# no longer reach it; the two-thread regression is compiled into the library
# test binary (`--lib`) and its full module path is the libtest filter.
TAIL_RACE_TEST = 'dream_tail_race::dream_tail_cannot_interleave_across_threads'
# rustc records the `#[path]` relative to `src/lib.rs`; classify matches the
# repository-relative suffix so an exact `src/../` spelling is not required.
TAIL_RACE_FILE = 'tests/common/dream_tail_race.rs'
# fix-r3 P1-2: a deterministic parked-flush cell whose second flush asserts
# the business outcome (`capturedSessions == 2`), not the session filter.
FLUSH_CONCURRENT_TEST = 'flush_second_completes_when_first_never_does'
# fix-r1 P1-6: the cross-process same-DB WAL cell's Rust acceptance test.
WAL_TWO_PROCESS_TEST = 'cross_process_wal_writer_blocks_service_writes_like_ts'
WAL_TWO_PROCESS_FILE = 'pilotdeck-gateway/tests/edgeclaw_wal_two_process.rs'
# fix-r2: the parked-flush cell cannot observe a widened session_keys;
# this deterministic cell exercises the same flush -> run_heartbeat filter.
FLUSH_FILTER_TEST = 'flush_session_filter_is_honored'
# #1036 P2-5: the dream-cancellation cell — a dropped `dream` future must
# still dispose its stage (Drop fallback) and keep the counters balanced.
DREAM_CANCEL_TEST = 'dream_cancellation_disposes_stage_and_balances_counters'


ANCHORS = {
    # `flush({reason: 'manual'})` writes the auto-index anchor before the
    # heartbeat; without it the post-run reconcile falls back to the earliest
    # pending timestamp (00:00:02) instead of the frozen now (00:00:00).
    'flush-manual-anchor':
        '#776 scenario flush-inflight-capture: sqlite differs from the real TS',
    # `Math.random` draw parity: `random_suffix` must consume exactly one draw
    # per trace/mutation id; a second draw shifts every fragment and with it
    # the first `dream_trace_*` hash.
    'trace-suffix-double-draw':
        '#776 scenario dream-stage-act: sqlite differs from the real TS',
    # The post-dream pipeline state write, not just the snapshot metadata.
    'dream-status-pipeline':
        '#776 scenario dream-stage-act: sqlite differs from the real TS',
    # `kvDetail`/`listDetail`/`jsonDetail` spread `labelI18n` between `label`
    # and `kind`; appending it at the end changes the `state_json` bytes.
    'detail-i18n-append':
        '#776 scenario dream-stage-act: sqlite differs from the real TS',
    # A failing staged run must dispose its stage exactly once; skipping the
    # dispose leaves `disposed` at 0 and the stage temp dir behind.
    'stage-dispose-on-error':
        '#776 scenario dream-stage-error: observed differs from the real TS',
    # 20 pending dialogue turns trigger indexing; 21 must not.
    'backlog-threshold':
        '#776 scenario maintenance-backlog: observed differs from the real TS',
    # `intervalMinutes <= 0` disables the interval check; interval 0 must not
    # index the pending session.
    'interval-zero-disabled':
        '#776 scenario maintenance-interval-zero: observed differs from the real TS',
    # The scheduled dream reports `trigger: "scheduled"` in `dreamResult`.
    'scheduled-trigger-string':
        '#776 scenario maintenance-dream-due: observed differs from the real TS',
    # fix-r1 P1-2: the disposed counter alone must not satisfy the residue
    # check — a stage that is counted but never cleaned up leaves its
    # `.dream-*` sibling temps behind (`stageTempLeft` in the oracle).
    'stage-dispose-skip-cleanup':
        '#776 scenario dream-stage-error: observed differs from the real TS',
    # fix-r1 P1-4 (D-3 换根丢写): the post-dream root swap must actually run;
    # skipping it leaves the pre-dream live roots in place.
    'dream-replace-skip':
        '#776 scenario dream-stage-act: observed differs from the real TS',
    # fix-r1 P1-4 (rollback × dream): the rollback result must report the
    # snapshot's runtime state; inverting the truthiness drops every key.
    'rollback-result-runtime-state':
        '#776 scenario dream-stage-rollback-ok: observed differs from the real TS',
    # fix-r1 P1-4 (flush × flush): fix-r2 re-bound this mutant. The parked
    # flush resolves `list_pending_session_keys` before its gated request, and
    # the concurrent second flush indexes s2 before the first resumes, so
    # widening `session_keys` to None produced the same `flush-inflight-flush`
    # cell (host r3 + local repro: SURVIVED). The deterministic
    # `flush_session_filter_is_honored` cell exercises the same flush →
    # run_heartbeat filter without that interleaving and fails here.
    'flush-concurrent-consumption':
        '#776 session-filter: flush captured sessions outside its sessionKeys',
    # fix-r1 P1-1: the dream success tail must hold one shared root operation
    # scope across install and replace (two-thread tail-race regression).
    # #1036 P2-3: the kill is the deterministic `try_lock` proof — with
    # per-call scopes both root locks are free while the first dream parks,
    # so the test thread grabs one and fails at this dedicated panic.
    'dream-tail-scope':
        '#1036 tail race: the workspace root operation lock was free while '
        'the first dream was parked between install and replace',
    # fix-r1 P1-6: node:sqlite keeps `busy_timeout` at 0, so a cross-process
    # write-lock failure must be immediate; a 5s wait fails the elapsed bound.
    'wal-busy-timeout':
        '#776 wal-two-process: the locked write must fail immediately',
    # fix-r3 P1-2: consuming s1 before its LLM await makes the concurrent
    # flush see only s2; the second flush's `capturedSessions` drops to 1.
    'flush-preempt-mark-consumed':
        '#776 flush-never-released: the second flush did not process both sessions',
    # fix-r3 P1-3: if the pending-session read starts a write transaction it
    # hits the other process's WAL write lock instead of returning s0.
    'wal-read-becomes-write':
        '#776 wal-two-process: the pending s0 session must still be readable '
        'under the write lock',
    # #1036 item 1: `hasElapsedMinutes` is an inclusive boundary — a capture
    # exactly one default interval old must index. `>` makes the exact cell
    # skip indexing.
    'interval-boundary-inclusive':
        '#776 scenario maintenance-interval-exact: observed differs from the real TS',
    # #1036 item 1: the interval is scaled in whole minutes; shrinking the
    # scale makes the just-before cell (1ms short) index anyway.
    'interval-scale-ms':
        '#776 scenario maintenance-interval-just-before: observed differs from the real TS',
    # #1036 item 2: 19 pending dialogue turns are below the `>= 20` backlog
    # threshold; lowering it to 19 indexes the below-threshold cell.
    'backlog-threshold-lower':
        '#776 scenario maintenance-backlog-below: observed differs from the real TS',
    # #1036 P2-5: the stage's own `Drop` must clean the staged roots when a
    # cancelled dream never disposes explicitly; inverting the swap check
    # leaves all three `.dream-*` temp siblings behind.
    'stage-drop-cleanup':
        '#1036 dream-cancel: the cancelled dream left staged dream temp dirs behind',
    # fix-r1 P2-2: after a FAILED explicit dispose, `Drop` must not clean —
    # TS keeps the residue observable (the `restore-dispose-failure` oracle
    # cell), and only the caller's retry may remove it.
    'stage-drop-explicit-residue':
        'scenario "restore-dispose-failure"',
    # #1036 P2-5: the dispose-fallback guard's `Drop` must balance
    # `dream_stages_disposed` with `created` for a cancelled dream.
    'stage-cancel-counter':
        '#1036 dream-cancel: a cancelled dream must still dispose its stage',
    # fix-r1 P2-2: a failed cleanup must reset the `disposed` flag, so the
    # retried dispose really removes the staged roots instead of reporting
    # the never-cleaned first attempt as success.
    'dispose-retry-resets-flag':
        '#1036 dispose-retry: the retried dispose left the staged temp',
}


def mutation(name, path, before, after, *, test=TEST, test_file=TEST_FILE,
             suite='test'):
    return dict(
        name=name,
        path=path,
        old=before,
        new=after,
        test=test,
        anchor=ANCHORS[name],
        test_file=test_file,
        suite=suite,
    )


MUTANTS = [
    mutation(
        'flush-manual-anchor',
        ORCHESTRATION,
        '.is_some_and(|reason| reason.equals_str("manual"))',
        '.is_some_and(|reason| reason.equals_str("manual-mutated"))',
    ),
    mutation(
        'trace-suffix-double-draw',
        ORCHESTRATION,
        '    fn random_suffix(&self) -> JsString {\n'
        '        let _ = self.random();\n'
        '        (self.service.random_token)()',
        '    fn random_suffix(&self) -> JsString {\n'
        '        let _ = self.random();\n'
        '        let _ = self.random();\n'
        '        (self.service.random_token)()',
    ),
    mutation(
        'dream-status-pipeline',
        ORCHESTRATION,
        '        self.repository.storage.set_pipeline_state(\n'
        '            LAST_DREAM_STATUS_STATE_KEY,\n'
        '            &JsValue::String("success".into()),\n'
        '        )?;',
        '        self.repository.storage.set_pipeline_state(\n'
        '            LAST_DREAM_STATUS_STATE_KEY,\n'
        '            &JsValue::String("success-mutated".into()),\n'
        '        )?;',
    ),
    mutation(
        'detail-i18n-append',
        DREAM_RUN,
        '    let mut ordered = JsObject::new();\n'
        '    for (name, value) in detail.entries() {\n'
        '        ordered.insert(name.clone(), value.clone());\n'
        '        if name.equals_str("label") {\n'
        '            ordered.insert("labelI18n".into(), i18n.clone());\n'
        '        }\n'
        '    }\n'
        '    ordered',
        '    let mut ordered = JsObject::new();\n'
        '    for (name, value) in detail.entries() {\n'
        '        ordered.insert(name.clone(), value.clone());\n'
        '    }\n'
        '    ordered.insert("labelI18n".into(), i18n);\n'
        '    ordered',
    ),
    mutation(
        # #1036 P2-5: the stage's own `Drop` now cleans up after a skipped
        # explicit dispose, so removing the whole block would survive. The
        # binding mutation drops only the fallback `mark()` — the guard then
        # counts the same dispose a second time and `disposed` becomes 2
        # where the real TS oracle cell says 1.
        'stage-dispose-on-error',
        ORCHESTRATION,
        '            Err(error) => {\n'
        '                _dispose_fallback.mark();\n'
        '                self.dream_stages_disposed.fetch_add(1, Ordering::Relaxed);\n'
        '                return match stage.dispose() {\n'
        '                    Ok(()) => Err(error),\n'
        '                    Err(dispose_error) => Err(dispose_error.into()),\n'
        '                };\n'
        '            }',
        '            Err(error) => {\n'
        '                self.dream_stages_disposed.fetch_add(1, Ordering::Relaxed);\n'
        '                return match stage.dispose() {\n'
        '                    Ok(()) => Err(error),\n'
        '                    Err(dispose_error) => Err(dispose_error.into()),\n'
        '                };\n'
        '            }',
    ),
    mutation(
        'backlog-threshold',
        ORCHESTRATION,
        'const AUTO_INDEX_PENDING_DIALOGUE_TURN_THRESHOLD: i64 = 20;',
        'const AUTO_INDEX_PENDING_DIALOGUE_TURN_THRESHOLD: i64 = 21;',
    ),
    mutation(
        'interval-zero-disabled',
        ORCHESTRATION,
        '    if interval_minutes <= 0.0 {\n'
        '        return Ok(false);\n'
        '    }',
        '    if interval_minutes < 0.0 {\n'
        '        return Ok(false);\n'
        '    }',
    ),
    mutation(
        'scheduled-trigger-string',
        ORCHESTRATION,
        '            Self::Scheduled => "scheduled",',
        '            Self::Scheduled => "scheduled-mutated",',
    ),
    mutation(
        # #1036 P2-5: with the `Drop` fallback, `drop(stage)` cleans up just
        # like the explicit dispose, so that shape would survive. The binding
        # mutation no-ops the shared `cleanup()` body itself — every dispose
        # path (explicit or Drop) then leaves the staged `.dream-*` siblings
        # behind and the dream-stage-error cell's `stageTempLeft` fails.
        'stage-dispose-skip-cleanup',
        SNAPSHOTS,
        '    fn cleanup(&self) -> Result<()> {\n'
        '        let _ = self.repository.close();\n'
        '        for p in [\n'
        '            &self.staged_workspace_root,\n'
        '            &self.staged_global_root,\n'
        '            &self.staged_db_path,\n'
        '        ] {\n'
        '            remove(&self.io, p)?;\n'
        '        }\n'
        '        Ok(())\n'
        '    }',
        '    fn cleanup(&self) -> Result<()> {\n'
        '        let _ = self.repository.close();\n'
        '        Ok(())\n'
        '    }',
    ),
    mutation(
        'dream-replace-skip',
        ORCHESTRATION,
        '                self.repository\n'
        '                    .replace_live_roots_with_stage(&stage, &stage.snapshot)?;\n'
        '                Ok(())',
        '                Ok(())',
    ),
    mutation(
        'rollback-result-runtime-state',
        SNAPSHOTS,
        '            for k in KEYS {\n'
        '                let v = required(field(before, "runtimeState"), k)?;\n'
        '                if truthy(v) {\n'
        '                    out.insert(k.into(), v.clone());\n'
        '                }\n'
        '            }',
        '            for k in KEYS {\n'
        '                let v = required(field(before, "runtimeState"), k)?;\n'
        '                if !truthy(v) {\n'
        '                    out.insert(k.into(), v.clone());\n'
        '                }\n'
        '            }',
    ),
    mutation(
        'flush-concurrent-consumption',
        ORCHESTRATION,
        '            .run_heartbeat(HeartbeatRunOptions {\n'
        '                batch_size: options.batch_size,\n'
        '                session_keys: options.session_keys.clone(),\n'
        '                reason: options.reason.clone(),\n'
        '            })',
        '            .run_heartbeat(HeartbeatRunOptions {\n'
        '                batch_size: options.batch_size,\n'
        '                session_keys: None,\n'
        '                reason: options.reason.clone(),\n'
        '            })',
        test=FLUSH_FILTER_TEST,
    ),
    mutation(
        'dream-tail-scope',
        ORCHESTRATION,
        '        let _tail_scope = self.repository.snapshot_operation_scope()?;\n',
        '        let _tail_scope = ();\n',
        test=TAIL_RACE_TEST,
        test_file=TAIL_RACE_FILE,
        suite='lib',
    ),
    mutation(
        'flush-preempt-mark-consumed',
        INDEXER,
        '''            let sessions = self
                .repository
                .storage
                .list_unindexed_l0_by_session(&session_key)?;
            self.fire(HeartbeatStage::AfterListUnindexed, &session_key);
            if sessions.is_empty() {
                continue;
            }
''',
        '''            let sessions = self
                .repository
                .storage
                .list_unindexed_l0_by_session(&session_key)?;
            self.fire(HeartbeatStage::AfterListUnindexed, &session_key);
            if sessions.is_empty() {
                continue;
            }
            // Mutation: consume the batch before the LLM await.
            let preempted: Vec<String> = sessions
                .iter()
                .map(|session| session.l0_index_id.clone())
                .collect();
            self.repository.storage.mark_l0_indexed(&preempted)?;
''',
        test=FLUSH_CONCURRENT_TEST,
    ),
    mutation(
        'wal-read-becomes-write',
        STORAGE,
        '''        if preferred.is_empty() {
            let conn = self.conn()?;
''',
        '''        if preferred.is_empty() {
            let conn = self.conn()?;
            // Mutation: the read path starts a write transaction.
            conn.execute_batch("BEGIN IMMEDIATE")?;
            conn.execute_batch("ROLLBACK")?;
''',
        test=WAL_TWO_PROCESS_TEST,
        test_file=WAL_TWO_PROCESS_FILE,
    ),
    mutation(
        'wal-busy-timeout',
        STORAGE,
        '        conn.busy_timeout(Duration::ZERO)?;',
        '        conn.busy_timeout(Duration::from_secs(5))?;',
        test=WAL_TWO_PROCESS_TEST,
        test_file=WAL_TWO_PROCESS_FILE,
    ),
    # #1036 item 1: the interval comparison is inclusive. The exact cell has
    # `nowMs - anchorMs == intervalMinutes * 60_000`, so `>` skips its index.
    mutation(
        'interval-boundary-inclusive',
        ORCHESTRATION,
        '    Ok(now_ms - anchor_ms >= interval_minutes * 60_000.0)',
        '    Ok(now_ms - anchor_ms > interval_minutes * 60_000.0)',
    ),
    # #1036 item 1: a minute is 60_000 ms. Shrinking the scale by 1ms makes
    # the just-before cell (exactly 1ms short of the interval) index anyway.
    mutation(
        'interval-scale-ms',
        ORCHESTRATION,
        '    Ok(now_ms - anchor_ms >= interval_minutes * 60_000.0)',
        '    Ok(now_ms - anchor_ms >= interval_minutes * 59_999.0)',
    ),
    # #1036 item 2: the backlog threshold is `>= 20`; 19 pending turns must
    # not index. Lowering the threshold to 19 indexes the below-threshold
    # cell (its fresh anchor keeps the interval path false there).
    mutation(
        'backlog-threshold-lower',
        ORCHESTRATION,
        'const AUTO_INDEX_PENDING_DIALOGUE_TURN_THRESHOLD: i64 = 20;',
        'const AUTO_INDEX_PENDING_DIALOGUE_TURN_THRESHOLD: i64 = 19;',
    ),
    # #1036 P2-5: the stage's `Drop` must actually clean up when a cancelled
    # dream never disposes explicitly; consuming the flag without cleaning
    # leaves the three `.dream-*` temp siblings behind.
    mutation(
        'stage-drop-cleanup',
        SNAPSHOTS,
        '        if !*self.explicit.get_mut() {\n'
        '            let _ = self.cleanup();\n'
        '        }',
        '        let _ = self.explicit.get_mut();',
        test=DREAM_CANCEL_TEST,
    ),
    # fix-r1 P2-2: `Drop` keys on the explicit-attempt latch, not the success
    # latch. Keying on `disposed` makes a failed dispose's residue disappear
    # at drop time, and the `restore-dispose-failure` oracle cell (which
    # keeps the `.global/.control.sqlite.restore-*` leftovers, like TS) fails.
    mutation(
        'stage-drop-explicit-residue',
        SNAPSHOTS,
        '        if !*self.explicit.get_mut() {\n'
        '            let _ = self.cleanup();\n'
        '        }',
        '        if !*self.disposed.get_mut() {\n'
        '            let _ = self.cleanup();\n'
        '        }',
        test='rust_seed_and_snapshot_lifecycle_match_ts',
        test_file='pilotdeck-gateway/tests/edgeclaw_snapshots.rs',
    ),
    # #1036 P2-5: the dispose-fallback guard must keep `dream_stages_disposed`
    # balanced with `created` for a cancelled dream; removing its increment
    # leaves the counter at (1, 0).
    mutation(
        'stage-cancel-counter',
        ORCHESTRATION,
        '        if !self.disposed.get() {\n'
        '            self.service\n'
        '                .dream_stages_disposed\n'
        '                .fetch_add(1, Ordering::Relaxed);\n'
        '        }',
        '        if !self.disposed.get() {\n'
        '            // mutated: a cancelled dream no longer balances the counter\n'
        '        }',
        test=DREAM_CANCEL_TEST,
    ),
    # fix-r1 P2-2: without the reset, the flag latched by the faulted first
    # `dispose` makes the retry return `Ok(())` without cleaning, and the
    # staged `.memory/.global/.control.sqlite.retry-*` siblings survive.
    mutation(
        'dispose-retry-resets-flag',
        SNAPSHOTS,
        '        let result = self.cleanup();\n'
        '        if result.is_err() {\n'
        '            self.disposed.store(false, Ordering::Release);\n'
        '        }\n'
        '        result',
        '        self.cleanup()',
        test='failed_dispose_retry_still_removes_staged_roots',
        test_file='pilotdeck-gateway/tests/edgeclaw_snapshots.rs',
    ),
]


def passed(result, test):
    log = result.stdout + result.stderr
    return (
        result.returncode == 0
        and f'test {test} ... ok' in log
        and 'test result: ok. 1 passed;' in log
    )


def classify(result, mutant):
    log = result.stdout + result.stderr
    if passed(result, mutant['test']):
        return 'SURVIVED', 'bound test still passed'
    if 'could not compile' in log:
        return 'INVALID', 'mutation did not compile'
    if (
        result.returncode != 0
        and 'test result: FAILED' in log
        and f"test {mutant['test']} ... FAILED" in log
        and any(
            'panicked at ' in line and mutant['test_file'] in line
            for line in log.splitlines()
        )
        and mutant['anchor'] in log
    ):
        return 'KILLED', 'bound test failed at its named assertion'
    return 'INVALID', 'missing bound runtime failure (wrong panic or missing test)'


def main():
    output = Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else None
    if output is None:
        print('usage: negative.py OUTPUT_DIR [mutant ...]', file=sys.stderr)
        return 2
    requested = sys.argv[2:]
    known = {mutant['name'] for mutant in MUTANTS}
    unknown = [name for name in requested if name not in known]
    if unknown:
        print(f'unknown mutant(s): {", ".join(unknown)}', file=sys.stderr)
        return 2
    selected = [
        mutant for mutant in MUTANTS if not requested or mutant['name'] in requested
    ]
    output.mkdir(parents=True, exist_ok=True)
    target = os.environ.get('CARGO_TARGET_DIR')
    if not target:
        print('CARGO_TARGET_DIR must be set by the caller', file=sys.stderr)
        return 2
    env = dict(os.environ)
    env['CARGO_TARGET_DIR'] = target
    results = []

    def run(label, mutant):
        command = [
            'cargo', 'test', '--offline', '--locked',
            '--manifest-path', str(ROOT / 'src/Cargo.toml'),
            '-p', 'pilotdeck-gateway',
        ]
        if mutant['suite'] == 'lib':
            command.append('--lib')
        else:
            command += ['--test', Path(mutant['test_file']).stem]
        # No `--nocapture`: it splices the test's stdout into the
        # `test <name> ... ok/FAILED` status line (#938 r14) that
        # passed()/classify() match exactly (#1053).
        args = ['--', mutant['test'], '--exact']
        with (output / f'{label}.log').open('w') as log:
            gate = wait_for_slot(ROOT / 'scripts/cargo-slots.sh')
            log.write(gate.stdout + gate.stderr)
            log.flush()
            result = run_owned(
                command + args,
                cwd=ROOT, env=env, timeout=900,
            )
            log.write(result.stdout + result.stderr)
        return result

    def record(mutant, status, reason):
        results.append(dict(
            name=mutant['name'],
            test=mutant['test'],
            test_file=mutant['test_file'],
            anchor=mutant['anchor'],
            status=status,
            reason=reason,
        ))
        counts = {
            state: sum(row['status'] == state for row in results)
            for state in ('KILLED', 'SURVIVED', 'INVALID')
        }
        (output / 'summary.json').write_text(
            json.dumps({'controls': results, 'counts': counts}, indent=2) + '\n'
        )
        print(f"{mutant['name']}: {status}: {reason}", flush=True)

    try:
        with mutating_sources(
            ROOT,
            [ORCHESTRATION, DREAM_RUN, SNAPSHOTS, STORAGE, INDEXER],
            baseline='HEAD',
        ) as snapshots:
            for mutant in selected:
                path = mutant['path']
                original = snapshots[path].decode()
                occurrences = original.count(mutant['old'])
                if occurrences != 1:
                    record(
                        mutant,
                        'INVALID',
                        f"anchor appears {occurrences} times, expected exactly once",
                    )
                    continue
                status, reason = 'INVALID', ''
                try:
                    if not passed(
                        run(f"{mutant['name']}-positive", mutant), mutant['test']
                    ):
                        status = 'INVALID'
                        reason = 'positive control did not pass the bound test'
                    else:
                        mutated = original.replace(mutant['old'], mutant['new'], 1)
                        path.write_text(mutated)
                        # Force Cargo to observe the write even on a filesystem
                        # with coarse mtimes; a cached binary would fake SURVIVED.
                        time.sleep(1.1)
                        status, reason = classify(
                            run(f"{mutant['name']}-negative", mutant), mutant
                        )
                except Exception as error:  # noqa: BLE001 - summarized as INVALID
                    status, reason = 'INVALID', f'{type(error).__name__}: {error}'
                finally:
                    path.write_text(original)
                    time.sleep(1.1)
                    if path.read_text() != original:
                        raise RuntimeError(f'byte restoration failed: {path}')
                try:
                    if not passed(
                        run(f"{mutant['name']}-restored", mutant), mutant['test']
                    ):
                        status, reason = (
                            'INVALID', reason + '; restored test did not pass'
                        )
                except Exception as error:  # noqa: BLE001 - keep summarizing
                    status, reason = (
                        'INVALID', reason + f'; restored: {type(error).__name__}: {error}'
                    )
                record(mutant, status, reason)
    except SourceBaselineError as error:
        # The new orchestration file is not in HEAD yet: refuse, never fake a
        # status. 提交后由协调者在宿主跑.
        for mutant in selected[len(results):]:
            record(mutant, 'INVALID', f'source guard: {error}')
        counts = {
            state: sum(row['status'] == state for row in results)
            for state in ('KILLED', 'SURVIVED', 'INVALID')
        }
        print(f'summary: KILLED={counts["KILLED"]}, SURVIVED={counts["SURVIVED"]}, '
              f'INVALID={counts["INVALID"]}', flush=True)
        return 3
    except Exception as error:  # noqa: BLE001 - never stop before summarizing
        # Timeout, restore failure or any unexpected runner error: account for
        # every remaining mutant instead of aborting on the first one.
        for mutant in selected[len(results):]:
            record(mutant, 'INVALID', f'runner error: {type(error).__name__}: {error}')
        counts = {
            state: sum(row['status'] == state for row in results)
            for state in ('KILLED', 'SURVIVED', 'INVALID')
        }
        print(f'summary: KILLED={counts["KILLED"]}, SURVIVED={counts["SURVIVED"]}, '
              f'INVALID={counts["INVALID"]}', flush=True)
        return 3
    counts = {
        state: sum(row['status'] == state for row in results)
        for state in ('KILLED', 'SURVIVED', 'INVALID')
    }
    print(
        f'summary: KILLED={counts["KILLED"]}, SURVIVED={counts["SURVIVED"]}, '
        f'INVALID={counts["INVALID"]}',
        flush=True,
    )
    return int(not results or any(row['status'] != 'KILLED' for row in results))


if __name__ == '__main__':
    sys.exit(main())
