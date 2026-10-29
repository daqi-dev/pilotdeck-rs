#!/usr/bin/env python3
"""Prove the EdgeClaw service facade checks fail on their own product mutation.

The new `service/mod.rs`, `storage.rs`, test driver and fixture encoder must be
committed before this runner can obtain the `mutating_sources` HEAD guard.
Until then it refuses with the shared `SourceBaselineError` instead of mutating
an uncommitted product tree.

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
import subprocess
import sys
import time
import types

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_suite import classify as shared_classify, passed

from negative_process import (
    SourceBaselineError,
    mutating_sources,
    run_owned,
    wait_for_slot,
)

ROOT = Path(__file__).resolve().parents[5]
SERVICE = ROOT / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/service/mod.rs'
STORAGE = ROOT / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/storage.rs'
FILE_MEMORY = ROOT / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/file_memory.rs'
CONFIG = ROOT / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/llm/config.rs'
OBSERVE = ROOT / 'src/pilotdeck-gateway/tests/fixtures/edgeclaw-service-facade/observe.mts'
TEST_SOURCE = ROOT / 'src/pilotdeck-gateway/tests/edgeclaw_service_facade.rs'
TEST = 'ts_service_facade_scenarios_match'
VERIFY = 'real_ts_reads_rust_service_observations'
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
    # fix-r3: `clampInt` must project `-0` to `+0` like TS `Math.max(min, …)`;
    # the first returned value (numeric -0) is the binding observable.
    'settings-negative-zero': '#775 saveSettings numeric -0 autoIndexIntervalMinutes returned -0',
    # fix-r4: `mergeIndexingSettings` has the same `Math.max(0, …)` zero rule.
    'interval-negative-zero': '#775 defaultIndexingSettings numeric -0 autoIndexIntervalMinutes must be +0 like TS Math.max',
    # fix-r4: the traces fixture now stores a non-alphabetical `retrieval` key
    # order; the exact SQLite `state_json` string is the binding observable.
    'case-trace-retrieval-key-order': '#775 sqlite rows for scenario 5 traces',
    # fix-r4: `1e20` is a finite JS number and must survive `getSettings`;
    # only non-finite defaults become `null` on the wire.
    'settings-large-finite-preserved': '#775 facade steps for scenario 12 settings-large-finite',
    # fix-r5 P1: the new `maxLines: -0` argument is the first finite `-0` the
    # service oracle encodes. Removing the TS branch makes the fresh real-TS
    # run differ from the committed `$type:number:"-0"` envelope.
    'observe-negative-zero-encode': '#775 negative-zero encoding mismatch at',
    # fix-r5 P1 (Rust side): the same oracle argument must keep its `-0`
    # envelope when the Rust driver re-encodes it.
    'encode-negative-zero': '#775 facade steps for scenario 4 facade-sequence',
    # fix-r5 P2-1: `null` must stay distinct from the `undefined` default 80.
    # fix-r7 P1: bind it to its own witness test so the non-number mutant below
    # can preserve this cell without the old row substituting for new coverage.
    'get-null-maxlines':
        '#775 service.get null maxLines must follow TS ToNumber to zero lines',
    # fix-r6 P1: the TS default only covers `undefined`; every other value is
    # coerced by `trimContentLines`' relational comparisons. fix-r7 P1: the
    # mutant keeps `Some(JsValue::Null) => 0.0` and only restores the old
    # non-number fallback, bound to the dedicated seven-row test.
    'get-nonnumber-maxlines':
        '#775 service.get non-number maxLines false must follow TS ToNumber',
    # fix-r6 P1 sibling: the TS-shaped FileMemoryStore adapter has the same rule.
    'file-memory-nonnumber-maxlines':
        '#775 file-memory getMemoryRecordsByIds maxLines false must follow TS ToNumber',
    # fix-r5 P2-2: the facade must reuse the indexer's UTF-16 message, not the
    # old lossy `Display` copy.
    'heartbeat-message-not-reused': '#775 ServiceError::js_message must reuse HeartbeatError::message (lone surrogate preserved)',
    'service-get-maxlines-early': '#975 service maxLines must stay unconverted without a record',
    'file-get-maxlines-early': '#975 file maxLines must stay unconverted without a record',
    'file-get-maxlines-skip': '#975 valid record must reach maxLines conversion in every route',
    'file-get-ids-unchecked': '#975 file ids map error must precede maxLines conversion',

}


def mutation(name, path, before, after, test=TEST, include_ignored=False,
             witness=None):
    return dict(
        name=name,
        path=path,
        old=before,
        new=after,
        test=test,
        anchor=ANCHORS[name],
        include_ignored=include_ignored,
        witness=witness,
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
        '"workspaceDir"',
        '"workspaceDirMutated"',
    ),
    mutation(
        'default-capture-source',
        SERVICE,
        'options.source.clone().unwrap_or_else(|| "edgeclaw".into())',
        'options.source.clone().unwrap_or_else(|| "edgeclaw-mutated".into())',
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
        '        self.repository.close()?;\n        Ok(())',
        '        match self.repository.close() {\n            Ok(()) => Ok(()),\n            Err(FileMemoryError::Storage(StorageError::Closed)) => Ok(()),\n            Err(error) => Err(ServiceError::File(error)),\n        }',
    ),
    mutation(
        'capture-holds-indexer-lock',
        SERVICE,
        '            let indexer = self\n                .indexer\n                .lock()\n                .expect("edgeclaw service indexer mutex poisoned")\n                .clone();\n            indexer.capture_l0_session(CaptureL0SessionInput {',
        '            let indexer = self\n                .indexer\n                .lock()\n                .expect("edgeclaw service indexer mutex poisoned");\n            indexer.capture_l0_session(CaptureL0SessionInput {',
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
        'memory_dir_option.filter(|path| !path.as_os_str().is_empty())',
        'memory_dir_option',
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
        'JsString::from(workspace_dir.to_string_lossy().into_owned())',
        'JsString::from(root_dir.to_string_lossy().into_owned())',
    ),
    mutation(
        'settings-truncate-nonfinite',
        SERVICE,
        'auto_index_interval_minutes: if (crate::mutants::on("settings-truncate-nonfinite")\n            && merged.auto_index_interval_minutes.is_nan())\n            || (crate::mutants::on("settings-large-finite-preserved")\n                && merged.auto_index_interval_minutes.is_finite()\n                && merged.auto_index_interval_minutes > 10_080.0)\n        {\n            0.0\n        } else {\n            merged.auto_index_interval_minutes\n        },',
        'auto_index_interval_minutes: if merged.auto_index_interval_minutes.is_nan() { 0.0 } else { merged.auto_index_interval_minutes },',
    ),
    mutation(
        'reopen-indexer-defaults',
        SERVICE,
        'HeartbeatOptions::new(initial_indexing_settings)',
        'HeartbeatOptions::new(default_indexing_settings.clone())',
        test='reopen_restores_persisted_indexer_settings',
    ),
    mutation(
        'closed-capture-empty-not-early',
        SERVICE,
        'if !crate::mutants::on("closed-capture-empty-not-early") && normalized_messages.is_empty() {',
        'if false && normalized_messages.is_empty() {',
    ),
    mutation(
        'settings-negative-zero',
        STORAGE,
        '0.0 /* Math.max(+0, ±0) is +0 */',
        'number.floor() // mutated: keep -0',
        test='save_settings_projects_negative_zero_like_ts',
    ),
    mutation(
        'interval-negative-zero',
        CONFIG,
        'if clamped == 0.0 { 0.0 } else { clamped }',
        'if clamped == 0.0 { -0.0 } else { clamped }',
        test='default_indexing_settings_project_negative_zero_like_ts',
    ),
    mutation(
        'settings-large-finite-preserved',
        SERVICE,
        'auto_index_interval_minutes: if (crate::mutants::on("settings-truncate-nonfinite")\n            && merged.auto_index_interval_minutes.is_nan())\n            || (crate::mutants::on("settings-large-finite-preserved")\n                && merged.auto_index_interval_minutes.is_finite()\n                && merged.auto_index_interval_minutes > 10_080.0)\n        {\n            0.0\n        } else {\n            merged.auto_index_interval_minutes\n        },',
        'auto_index_interval_minutes: if merged.auto_index_interval_minutes.is_finite()\n            && merged.auto_index_interval_minutes > 10_080.0\n        {\n            0.0\n        } else {\n            merged.auto_index_interval_minutes\n        },',
    ),
    mutation(
        'case-trace-retrieval-key-order',
        SERVICE,
        '        next.insert("retrieval".into(), field("retrieval"));',
        '        let retrieval = field("retrieval");\n'
        '        let sorted = retrieval.as_object().map(|object| {\n'
        '            let mut sorted = JsObject::new();\n'
        '            let mut entries = object.entries().to_vec();\n'
        '            entries.sort_by(|left, right| {\n'
        '                left.0\n'
        '                    .to_utf8()\n'
        '                    .unwrap_or_default()\n'
        '                    .cmp(&right.0.to_utf8().unwrap_or_default())\n'
        '            });\n'
        '            for (key, value) in entries {\n'
        '                sorted.insert(key, value);\n'
        '            }\n'
        '            JsValue::Object(sorted)\n'
        '        });\n'
        '        next.insert("retrieval".into(), sorted.unwrap_or(retrieval));',
    ),
    # fix-r5 P1: TS `encode` used to keep the `Object.is(value, -0)` test
    # inside the non-finite guard, so finite `-0` fell through to
    # `JSON.stringify(-0)` -> 0. The committed oracle has the `$type` envelope;
    # the fresh real-TS verifier must fail when the branch is removed.
    mutation(
        'observe-negative-zero-encode',
        OBSERVE,
        "if (typeof value === 'number' && (Object.is(value, -0) || !Number.isFinite(value))) {",
        "if (typeof value === 'number' && !Number.isFinite(value)) {",
        test=VERIFY,
        include_ignored=True,
    ),
    # fix-r5 P1 Rust side: `encode_js` must mirror that finite `-0` envelope.
    mutation(
        'encode-negative-zero',
        TEST_SOURCE,
        '            } else if !pilotdeck_gateway::mutants::on("encode-negative-zero")\n                && *number == 0.0\n                && number.is_sign_negative()\n            {',
        '            } else if false\n                && *number == 0.0\n                && number.is_sign_negative()\n            {',
    ),
    # #975: preserve the older coercion regressions after passing raw values.
    mutation(
        'get-null-maxlines', SERVICE,
        'crate::mutants::on("get-null-maxlines")', 'true',
        test='service_get_null_max_lines_matches_ts_empty_content',
    ),
    mutation(
        'get-nonnumber-maxlines', SERVICE,
        '            Some(value) => value.clone(),',
        '            Some(JsValue::Null) => JsValue::Number(0.0),\n'
        '            Some(value) => JsValue::Number(value.as_number().unwrap_or(80.0)),',
        test='service_get_coerces_non_number_max_lines',
        witness='service_get_null_max_lines_matches_ts_empty_content',
    ),
    mutation(
        'file-memory-nonnumber-maxlines', FILE_MEMORY,
        'crate::mutants::on("file-memory-nonnumber-maxlines")', 'true',
        test='file_memory_store_get_records_coerces_non_number_max_lines',
    ),
    mutation(
        'service-get-maxlines-early', SERVICE,
        'crate::mutants::on("service-get-maxlines-early")', 'true',
        test='service_get_maxlines_deferred_975',
    ),
    mutation(
        'file-get-maxlines-early', FILE_MEMORY,
        'crate::mutants::on("file-get-maxlines-early")', 'true',
        test='file_get_maxlines_deferred_975',
    ),
    mutation(
        'file-get-maxlines-skip', FILE_MEMORY,
        'crate::mutants::on("file-get-maxlines-skip")', 'true',
        test='get_maxlines_converts_existing_records_975',
    ),
    mutation(
        'file-get-ids-unchecked', FILE_MEMORY,
        'crate::mutants::on("file-get-ids-unchecked")', 'true',
        test='file_get_ids_before_maxlines_975',
    ),
    # fix-r5 P2-2: restore the old hand-copied Heartbeat arm. `IndexError`
    # `Display` is lossy for a lone surrogate, so the dedicated test fails.
    mutation(
        'heartbeat-message-not-reused',
        SERVICE,
        '            Self::Heartbeat(error) => error.message(),',
        '            Self::Heartbeat(error) => match error {\n'
        '                HeartbeatError::Storage(StorageError::Closed) => {\n'
        '                    JsString::from("database is not open")\n'
        '                }\n'
        '                HeartbeatError::File(error) => error.js_message(),\n'
        '                other => JsString::from(other.to_string()),\n'
        '            },',
        test='service_error_heartbeat_message_reuses_heartbeat_message',
    ),
]


def classify(result, mutant, *, source_file=TEST_FILE):
    # #988: both engines share the verdict, retaining this fixture's stricter
    # panic-file binding. The library inclusion has an explicit ../ path.
    bound = types.SimpleNamespace(test=mutant['test'], assertion=mutant['anchor'])
    status, reason = shared_classify(result, bound)
    log = result.stdout + result.stderr
    if status == 'KILLED' and (
        'could not compile' in log or f'panicked at {source_file}:' not in log
    ):
        return 'INVALID', 'missing bound runtime failure (wrong panic or build error)'
    return status, reason


def classifier_self_test():
    """fix-r6 P1: unrelated Node exits must not look like the -0 kill.

    Synthetic libtest/Node logs only: no cargo, no source mutation. The wrapper
    emits `#775 verify.mts failed` for every non-zero Node exit, so the bound
    anchor must be the fixture's dedicated `-0` line. A temp-file ENOENT and a
    source-closure failure share the wrapper text and must stay INVALID.
    """
    def failed(stderr_body):
        return types.SimpleNamespace(
            stdout='',
            stderr=(
                f'test {VERIFY} ... FAILED\n'
                'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; '
                '0 filtered out;\n'
                f'panicked at {TEST_FILE}:1580:5:\n'
                '#775 verify.mts failed\n'
                f'stdout:\n\nstderr:\n{stderr_body}\n'
            ),
            returncode=101,
        )

    mutant = {'test': VERIFY, 'anchor': ANCHORS['observe-negative-zero-encode']}
    checks = [
        (
            'temporary-file-error',
            failed(
                "Error: ENOENT: no such file or directory, open "
                "'/tmp/pdrs-edgeclaw-verify/rust-observations.json'"
            ),
            'INVALID',
        ),
        (
            'source-verification-error',
            failed(
                'AssertionError [ERR_ASSERTION]: baseline source closure '
                'differs from the oracle'
            ),
            'INVALID',
        ),
        (
            'negative-zero-mismatch',
            failed(
                'Error: #775 negative-zero encoding mismatch at '
                '$[4].ops[13].args.maxLines: expected '
                '{"$type":"number","value":"-0"} actual 0'
            ),
            'KILLED',
        ),
    ]
    wrong = []
    for name, result, expected in checks:
        status, reason = classify(result, mutant)
        print(f'classifier {name}: {status}: {reason}', flush=True)
        if status != expected:
            wrong.append(f'{name}: expected {expected}, got {status}')
    if wrong:
        print(f'classifier self-test failed: {"; ".join(wrong)}', file=sys.stderr)
        return False
    return True


def run_mutant(mutant, original, run):
    """Run inside mutating_sources; a failed phase cannot skip the next binding."""
    path = mutant['path']
    source = original.decode()
    status, reason = 'INVALID', ''
    try:
        occurrences = source.count(mutant['old'])
        if occurrences != 1:
            raise ValueError(f'anchor appears {occurrences} times, expected exactly once')
        if not passed(run(f"{mutant['name']}-positive", mutant['test'], mutant['include_ignored']), mutant['test']):
            raise ValueError('positive control did not pass the bound test')
        if mutant['witness'] and not passed(
            run(f"{mutant['name']}-witness-positive", mutant['witness'], mutant['include_ignored']),
            mutant['witness'],
        ):
            raise ValueError('witness test did not pass before mutation')
        path.write_text(source.replace(mutant['old'], mutant['new'], 1))
        # A coarse-mtime filesystem must not reuse the original binary.
        time.sleep(1.1)
        if run(f"{mutant['name']}-compile", mutant['test'], compile_only=True).returncode:
            raise RuntimeError('mutation did not compile; see compile log')
        witness_failed = mutant['witness'] and not passed(
            run(f"{mutant['name']}-witness-negative", mutant['witness'], mutant['include_ignored']),
            mutant['witness'],
        )
        status, reason = classify(
            run(f"{mutant['name']}-negative", mutant['test'], mutant['include_ignored']), mutant
        )
        if witness_failed:
            status, reason = 'INVALID', reason + '; witness test failed under mutation'
    except Exception as error:
        status, reason = 'INVALID', f'{type(error).__name__}: {error}'
    finally:
        # The enclosing guard owns the snapshot and independently verifies exit.
        path.write_bytes(original)
        time.sleep(1.1)
        if path.read_bytes() != original:
            raise RuntimeError(f'byte restoration failed: {path}')
    try:
        if not passed(run(f"{mutant['name']}-restored", mutant['test'], mutant['include_ignored']), mutant['test']):
            status, reason = 'INVALID', reason + '; restored test did not pass'
    except Exception as error:
        status, reason = 'INVALID', reason + f'; restored: {type(error).__name__}: {error}'
    return status, reason


def main():
    if sys.argv[1:] == ['--self-test']:
        return 0 if classifier_self_test() else 2
    if len(sys.argv) < 2:
        print('NEW_OUTPUT_DIR is required', file=sys.stderr)
        return 2
    output = Path(sys.argv[1]).resolve()
    requested = sys.argv[2:]
    known = {mutant['name'] for mutant in MUTANTS}
    unknown = [name for name in requested if name not in known]
    if unknown or len(requested) != len(set(requested)):
        print(f'unknown or duplicate mutant(s): {requested}', file=sys.stderr)
        return 2
    selected = [
        mutant for mutant in MUTANTS if not requested or mutant['name'] in requested
    ]
    output.mkdir(parents=True, exist_ok=False)
    if not classifier_self_test():
        return 2
    target = os.environ.get('CARGO_TARGET_DIR')
    if not target:
        print('CARGO_TARGET_DIR must be set by the caller', file=sys.stderr)
        return 2
    env = dict(os.environ)
    env.pop('PDRS_MUTANT', None)
    env.pop('PDRS_FACADE_ENCODER_MUTANT', None)
    env['CARGO_TARGET_DIR'] = target
    command = [
        'cargo', 'test', '--offline', '--locked',
        '--manifest-path', str(ROOT / 'src/Cargo.toml'),
        '-p', 'pilotdeck-gateway', '--test', 'edgeclaw_service_facade',
    ]
    results = []

    def run(label, mutant_test, include_ignored=False, compile_only=False):
        args = ['--', mutant_test, '--exact', '--nocapture']
        if include_ignored:
            # libtest option, so it stays after cargo's `--` separator.
            args.insert(1, '--include-ignored')
        if compile_only:
            args = ['--no-run']
        with (output / f'{label}.log').open('w') as log:
            try:
                gate = wait_for_slot(ROOT / 'scripts/cargo-slots.sh')
                log.write(gate.stdout + gate.stderr)
                log.flush()
                result = run_owned(command + args, cwd=ROOT, env=env, timeout=900)
                log.write(result.stdout + result.stderr)
            except Exception as error:
                if isinstance(error, subprocess.TimeoutExpired):
                    for part in (error.stdout, error.stderr):
                        log.write(part.decode(errors='replace') if isinstance(part, bytes) else part or '')
                log.write(f'\n{type(error).__name__}: {error}\n')
                raise
        return result

    def publish():
        counts = {
            state: sum(row['status'] == state for row in results)
            for state in ('KILLED', 'SURVIVED', 'INVALID')
        }
        pending = output / 'summary.json.tmp'
        pending.write_text(
            json.dumps({'controls': results, 'counts': counts}, indent=2) + '\n'
        )
        pending.replace(output / 'summary.json')

    def record(mutant, status, reason):
        results.append(dict(
            name=mutant['name'], test=mutant['test'], anchor=mutant['anchor'],
            status=status, reason=reason,
        ))
        publish()
        print(f"{mutant['name']}: {status}: {reason}", flush=True)

    try:
        with mutating_sources(
            ROOT,
            [SERVICE, STORAGE, FILE_MEMORY, CONFIG, OBSERVE, TEST_SOURCE],
            baseline='HEAD',
        ) as snapshots:
            for mutant in selected:
                status, reason = run_mutant(mutant, snapshots[mutant['path']], run)
                record(mutant, status, reason)
    except Exception as error:
        # Lock/baseline/restoration errors prevent safe successor mutations.
        for row in results:
            row.update(status='INVALID', reason=row['reason'] + f'; source guard: {error}')
        for mutant in selected[len(results):]:
            record(mutant, 'INVALID', f'source guard: {error}')
        publish()
        counts = {
            state: sum(row['status'] == state for row in results)
            for state in ('KILLED', 'SURVIVED', 'INVALID')
        }
        print(f'summary: KILLED={counts["KILLED"]}, SURVIVED={counts["SURVIVED"]}, '
              f'INVALID={counts["INVALID"]}', flush=True)
        return 3 if isinstance(error, SourceBaselineError) else 2
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
