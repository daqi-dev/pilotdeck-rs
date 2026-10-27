#!/usr/bin/env python3
"""Prove the EdgeClaw service facade checks fail on their own product mutation.

The new `service/mod.rs` must be committed before this runner can obtain the
`mutating_sources` HEAD guard. Until then it refuses with the shared
`SourceBaselineError` instead of mutating an uncommitted product tree.

    CARGO_TARGET_DIR=/abs/target python3 \
        src/pilotdeck-gateway/tests/fixtures/edgeclaw-service-facade/negative.py \
        _tmp/20261026-issue-775/negative-service-facade [mutant ...]

Every selected mutant is applied, compiled and run, then restored. The runner
does not stop at the first failure: it records KILLED / SURVIVED / INVALID for
all cases and exits non-zero if any case is not KILLED. Compile errors and
failures that do not land on the named test's assertion are INVALID. Without
mutant names it runs the full set; names are for debugging one binding.
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
SERVICE = ROOT / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/service/mod.rs'
TEST = 'ts_service_facade_scenarios_match'
TEST_FILE = 'pilotdeck-gateway/tests/edgeclaw_service_facade.rs'


ANCHORS = {
    'default-root-dir': '#775 paths for scenario 0 paths-defaults',
    'workspace-state-key': '#775 sqlite rows for scenario 0 paths-defaults',
    'default-capture-source': '#775 sqlite rows for scenario 3 capture-normalization',
    'case-trace-preview-not-normalized': '#775 facade steps for scenario 5 traces',
    'clear-all-uses-current-workspace': '#775 facade steps for scenario 7 clear',
    'close-not-idempotent-guarded': '#775 facade steps for scenario 9 closed-matrix',
    'capture-holds-indexer-lock': '#775 logger called while the indexer mutex is held',
    'root-empty-not-default': '#775 rootDir="" must fall back to HOME/.edgeclaw/memory',
    'memory-empty-pollutes-data-dir': '#775 memoryDir="" must keep the workspace dataDir',
    'case-id-seed-lossy': '#775 automatic caseId seed keeps the lone-surrogate startedAt',
    'workspace-data-dir-seed': '#775 workspace data-dir slug',
    'settings-truncate-nonfinite': '#775 facade steps for scenario 10 settings-nonfinite',
    'reopen-indexer-defaults': '#775 reopened indexer restores autoIndexIntervalMinutes',
    # fix-r2: the early return is shared by every mostly-empty capture. This
    # mutant first turns `captured: false` into `true` in scenario 3
    # (capture-normalization's assistant-only/tool-only turns), before the
    # closed-empty scenario 11 ever runs; bind the first observable cell.
    'closed-capture-empty-not-early': '#775 facade steps for scenario 3 capture-normalization',
}


def mutation(name, path, before, after, test=TEST):
    return dict(
        name=name,
        path=path,
        old=before,
        new=after,
        test=test,
        anchor=ANCHORS[name],
    )


MUTANTS = [
    mutation(
        'default-root-dir',
        SERVICE,
        '_ => absolute(&home.join(".edgeclaw").join("memory"))?,',
        '_ => absolute(&home.join(".edgeclaw-mutated").join("memory"))?,',
    ),
    mutation(
        'workspace-state-key',
        SERVICE,
        'repository.storage.set_pipeline_state(\n            "workspaceDir",',
        'repository.storage.set_pipeline_state(\n            "workspaceDirMutated",',
    ),
    mutation(
        'default-capture-source',
        SERVICE,
        'let source = options.source.clone().unwrap_or_else(|| "edgeclaw".into());',
        'let source = options.source.clone().unwrap_or_else(|| "edgeclaw-mutated".into());',
    ),
    mutation(
        'case-trace-preview-not-normalized',
        SERVICE,
        'Some(object) => JsValue::Object(normalize_case_trace_context_preview(object)),',
        'Some(object) => JsValue::Object(object.clone()),',
    ),
    mutation(
        'clear-all-uses-current-workspace',
        SERVICE,
        'ClearMemoryScope::AllMemory => self.repository.clear_all_memory_data()?,',
        'ClearMemoryScope::AllMemory => self.repository.clear_current_workspace_memory_data()?,',
    ),
    mutation(
        'close-not-idempotent-guarded',
        SERVICE,
        '    pub fn close(&self) -> Result<()> {\n        self.repository.close()?;\n        Ok(())\n    }',
        '    pub fn close(&self) -> Result<()> {\n        match self.repository.close() {\n            Ok(()) => Ok(()),\n            Err(FileMemoryError::Storage(StorageError::Closed)) => Ok(()),\n            Err(error) => Err(ServiceError::File(error)),\n        }\n    }',
    ),
    mutation(
        'capture-holds-indexer-lock',
        SERVICE,
        '        let indexer = self\n            .indexer\n            .lock()\n            .expect("edgeclaw service indexer mutex poisoned")\n            .clone();\n        indexer.capture_l0_session(CaptureL0SessionInput {',
        '        let indexer = self\n            .indexer\n            .lock()\n            .expect("edgeclaw service indexer mutex poisoned");\n        indexer.capture_l0_session(CaptureL0SessionInput {',
        test='duplicate_capture_logger_can_save_settings',
    ),
    mutation(
        'root-empty-not-default',
        SERVICE,
        'Some(root) if !root.as_os_str().is_empty() => absolute(root)?,',
        'Some(root) => absolute(root)?,',
        test='empty_path_options_use_ts_default_rules',
    ),
    mutation(
        'memory-empty-pollutes-data-dir',
        SERVICE,
        'memory_dir_option.filter(|path| !path.as_os_str().is_empty()),',
        'memory_dir_option,',
        test='empty_path_options_use_ts_default_rules',
    ),
    mutation(
        'case-id-seed-lossy',
        SERVICE,
        'seed.extend_from_slice(started_at.units());',
        'seed.extend(started_at.to_utf8().unwrap_or_default().encode_utf16());',
        test='case_id_seed_keeps_lone_surrogate_started_at',
    ),
    mutation(
        'workspace-data-dir-seed',
        SERVICE,
        'let seed = JsString::from(workspace_dir.to_string_lossy().into_owned());',
        'let seed = JsString::from(root_dir.to_string_lossy().into_owned());',
    ),
    mutation(
        'settings-truncate-nonfinite',
        SERVICE,
        'auto_index_interval_minutes: merged.auto_index_interval_minutes,',
        'auto_index_interval_minutes: if merged.auto_index_interval_minutes.is_nan() { 0.0 } else { merged.auto_index_interval_minutes },',
    ),
    mutation(
        'reopen-indexer-defaults',
        SERVICE,
        'let mut heartbeat_options = HeartbeatOptions::new(initial_indexing_settings);',
        'let mut heartbeat_options = HeartbeatOptions::new(default_indexing_settings.clone());',
        test='reopen_restores_persisted_indexer_settings',
    ),
    mutation(
        'closed-capture-empty-not-early',
        SERVICE,
        'if normalized_messages.is_empty() {',
        'if false && normalized_messages.is_empty() {',
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
    output = Path(sys.argv[1]).resolve()
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
        '-p', 'pilotdeck-gateway', '--test', 'edgeclaw_service_facade',
    ]
    results = []

    def run(label, mutant_test):
        with (output / f'{label}.log').open('w') as log:
            gate = wait_for_slot(ROOT / 'scripts/cargo-slots.sh')
            log.write(gate.stdout + gate.stderr)
            log.flush()
            result = run_owned(
                command + ['--', mutant_test, '--exact', '--nocapture'],
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
        with mutating_sources(ROOT, [SERVICE], baseline='HEAD') as snapshots:
            original = snapshots[SERVICE].decode()
            for mutant in selected:
                occurrences = original.count(mutant['old'])
                if occurrences != 1:
                    record(
                        mutant,
                        'INVALID',
                        f"anchor appears {occurrences} times, expected exactly once",
                    )
                    continue
                if not passed(run(f"{mutant['name']}-positive", mutant['test']), mutant['test']):
                    record(mutant, 'INVALID', 'positive control did not pass the bound test')
                    continue
                mutated = original.replace(mutant['old'], mutant['new'], 1)
                status, reason = 'INVALID', ''
                try:
                    SERVICE.write_text(mutated)
                    # Force Cargo to observe the write even on a filesystem
                    # with coarse mtimes; a cached binary would fake SURVIVED.
                    time.sleep(1.1)
                    status, reason = classify(
                        run(f"{mutant['name']}-negative", mutant['test']), mutant
                    )
                except Exception as error:  # noqa: BLE001 - summarized as INVALID
                    status, reason = 'INVALID', f'{type(error).__name__}: {error}'
                finally:
                    SERVICE.write_text(original)
                    time.sleep(1.1)
                    if SERVICE.read_text() != original:
                        raise RuntimeError(f'byte restoration failed: {SERVICE}')
                if not passed(
                    run(f"{mutant['name']}-restored", mutant['test']), mutant['test']
                ):
                    status, reason = 'INVALID', reason + '; restored test did not pass'
                record(mutant, status, reason)
    except SourceBaselineError as error:
        # The new service file is not in HEAD yet: refuse, never fake a status.
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
