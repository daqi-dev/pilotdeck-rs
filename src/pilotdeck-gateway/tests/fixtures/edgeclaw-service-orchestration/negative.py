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
TEST = 'rust_matches_ts_oracle'
TEST_FILE = 'pilotdeck-gateway/tests/edgeclaw_service_orchestration.rs'


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
}


def mutation(name, path, before, after):
    return dict(
        name=name,
        path=path,
        old=before,
        new=after,
        test=TEST,
        anchor=ANCHORS[name],
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
        'stage-dispose-on-error',
        ORCHESTRATION,
        '            Err(error) => {\n'
        '                self.dream_stages_disposed.fetch_add(1, Ordering::Relaxed);\n'
        '                return match stage.dispose() {\n'
        '                    Ok(()) => Err(error),\n'
        '                    Err(dispose_error) => Err(dispose_error.into()),\n'
        '                };\n'
        '            }',
        '            Err(error) => {\n'
        '                return Err(error);\n'
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
        and f'panicked at {TEST_FILE}:' in log
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
    command = [
        'cargo', 'test', '--offline', '--locked',
        '--manifest-path', str(ROOT / 'src/Cargo.toml'),
        '-p', 'pilotdeck-gateway', '--test', 'edgeclaw_service_orchestration',
    ]
    results = []

    def run(label):
        args = ['--', TEST, '--exact', '--nocapture']
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
            [ORCHESTRATION, DREAM_RUN],
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
                if not passed(run(f"{mutant['name']}-positive")):
                    record(mutant, 'INVALID', 'positive control did not pass the bound test')
                    continue
                mutated = original.replace(mutant['old'], mutant['new'], 1)
                status, reason = 'INVALID', ''
                try:
                    path.write_text(mutated)
                    # Force Cargo to observe the write even on a filesystem
                    # with coarse mtimes; a cached binary would fake SURVIVED.
                    time.sleep(1.1)
                    status, reason = classify(run(f"{mutant['name']}-negative"), mutant)
                except Exception as error:  # noqa: BLE001 - summarized as INVALID
                    status, reason = 'INVALID', f'{type(error).__name__}: {error}'
                finally:
                    path.write_text(original)
                    time.sleep(1.1)
                    if path.read_text() != original:
                        raise RuntimeError(f'byte restoration failed: {path}')
                if not passed(run(f"{mutant['name']}-restored")):
                    status, reason = 'INVALID', reason + '; restored test did not pass'
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
