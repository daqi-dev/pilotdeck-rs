#!/usr/bin/env python3
"""Prove each capture-turn check fails on its own real product mutation.

Run from the repository root after the work is committed (mutating_sources
refuses to mutate a product tree that differs from HEAD):

    python3 src/pilotdeck-gateway/tests/fixtures/edgeclaw-capture-turn/negative.py \
        _tmp/20261026-issue-759/negative-capture-turn

Each mutant names the exact test that must fail and the failure shape that
proves the test really executed: an `assertion` diagnostic, or a panic at this
integration test file for `assert!` / `expect_err` paths. A compile error or a
different test failing is INVALID evidence. Every mutant is classified before
the summary; a non-KILLED mutant never aborts the remaining loop, and any
non-KILLED result exits 1. A restored-positive timeout or process-start
failure is recorded as `restoredPass=false` with its reason instead of
dropping the already-collected summary.
"""
import hashlib
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

TEST_FILE = 'pilotdeck-gateway/tests/edgeclaw_capture_turn.rs'


def restore_positive(run):
    """Run the restored positive control without discarding the summary.

    A timeout or a process-start failure is evidence about this run, not a
    reason to lose the per-mutant classification already collected (#774
    P2-1). The caller records ``restoredPass=false`` plus this reason and
    still exits nonzero.
    """
    try:
        code = run('restored', '')
        return code == 0, None if code == 0 else f'exit={code}'
    except Exception as error:
        return False, f'{type(error).__name__}: {error}'


def summary_payload(*, results, failures, restored, restored_ok,
                    restored_error, before_sha256, after_sha256):
    counts = {
        state: sum(row['status'] == state for row in results)
        for state in ('KILLED', 'SURVIVED', 'INVALID')
    }
    payload = dict(
        results=results,
        failures=failures,
        counts=counts,
        restored=restored,
        restoredPass=restored_ok,
        beforeSha256=before_sha256,
        afterSha256=after_sha256,
    )
    if restored_error is not None:
        payload['restoredError'] = restored_error
    return payload


def write_summary(output, payload):
    (output / 'summary.json').write_text(json.dumps(payload, indent=2) + '\n')


def main():
    root = Path(__file__).resolve().parents[5]
    output = Path(sys.argv[1]).resolve()
    output.mkdir(parents=True, exist_ok=True)
    prepare = root / 'src/pilotdeck-gateway/src/runtime/context/prepare.rs'
    manifest = root / 'src/pilotdeck-gateway/src/host/modules/manifest.rs'
    env = dict(os.environ)
    env.setdefault('CARGO_TARGET_DIR', str(root / '_build/cargo-target'))
    command = [
        'cargo', 'test', '--offline', '--locked',
        '--manifest-path', str(root / 'src/Cargo.toml'),
        '-p', 'pilotdeck-gateway', '--test', 'edgeclaw_capture_turn',
    ]
    # (name, file, before, after, test, failure, anchor)
    # `failure` is how the named test must die:
    #   'assertion' — a Rust assertion diagnostic carrying `anchor`
    #   'panic'     — `assert!` / `expect_err` panic at this test file with `anchor`
    mutate = lambda name, path, before, after, test, failure, anchor: dict(
        name=name, path=path, before=before, after=after,
        test=test, failure=failure, anchor=anchor,
    )
    mutants = [
        mutate(
            'fork-carryover-filter-removed',
            prepare,
            '.filter(|message| !super::memory::fork_carryover(message))',
            '.filter(|_message| true)',
            'ts_capture_turn_scenarios_match',
            'assertion',
            'captureTurn scenarios',
        ),
        mutate(
            'always-on-skip-removed',
            prepare,
            'if is_always_on_session(&session_id) {',
            'if false && is_always_on_session(&session_id) {',
            'always_on_sessions_are_skipped',
            'panic',
            'must not reach the resolver',
        ),
        mutate(
            'capture-errors-not-swallowed',
            prepare,
            'Err(_) => Ok(()),',
            'Err(error) => Err(ModuleError::new(error).into()),',
            'resolver_failures_are_swallowed',
            'panic',
            'captureTurn must never reject',
        ),
        mutate(
            'malformed-session-no-error',
            prepare,
            '''            Some(JsValue::Null) => {
                return Err(ModuleError::new(
                    "Cannot read properties of null (reading 'startsWith')",
                )
                .into());
            }
''',
            '            Some(JsValue::Null) => String::new(),',
            'malformed_session_id_matches_ts_startswith_failure',
            'panic',
            # Rejection-only control: the product error text has its own
            # mutant below, so this anchor must not borrow the test's
            # `expected` wording (#786 / #774 P2-2).
            '#786 non-string sessionId must reject like isAlwaysOnSession',
        ),
        mutate(
            'malformed-session-wrong-error-text',
            prepare,
            '''            Some(JsValue::Null) => {
                return Err(ModuleError::new(
                    "Cannot read properties of null (reading 'startsWith')",
                )
                .into());
            }
''',
            '''            Some(JsValue::Null) => {
                return Err(ModuleError::new(
                    "#774 wrong product error text",
                )
                .into());
            }
''',
            'malformed_session_id_matches_ts_startswith_failure',
            'panic',
            # Exclusive message of the product-error-text assertion at
            # edgeclaw_capture_turn.rs:413-416: still Err, but wrong text.
            '#786 product error must contain the TS text',
        ),
        mutate(
            'supports-capture-turn-removed',
            prepare,
            '                | Operation::CaptureTurn\n',
            '',
            'manifest_advertises_capture_turn',
            'assertion',
            'capture_turn',
        ),
        mutate(
            'manifest-capture-turn-removed',
            manifest,
            'if context.capture_turn {',
            'if false {',
            'manifest_advertises_capture_turn',
            'assertion',
            'manifest context methods',
        ),
        mutate(
            'messages-not-array-reaches-resolver',
            prepare,
            '''        let Some(JsValue::Array(messages)) = fields.get("messages") else {
            return Ok(());
        };
''',
            '''        let empty = Vec::new();
        let messages = match fields.get("messages") {
            Some(JsValue::Array(messages)) => messages,
            _ => &empty,
        };
''',
            'ts_capture_turn_scenarios_match',
            'assertion',
            'captureTurn scenarios',
        ),
        mutate(
            'errored-non-boolean-folded-to-false',
            prepare,
            '            errored: fields.get("errored").cloned().unwrap_or(JsValue::Undefined),\n',
            '''            errored: fields
                .get("errored")
                .and_then(JsValue::as_bool)
                .map(JsValue::Bool)
                .unwrap_or(JsValue::Bool(false)),
''',
            'ts_capture_turn_scenarios_match',
            'assertion',
            'captureTurn scenarios',
        ),
    ]
    results = []

    # A committed tree passes HEAD. The uncommitted-patch runner can point
    # NEGATIVE_BASELINE at a temporary `git commit-tree` object with the same
    # product bytes, without moving the branch.
    baseline = os.environ.get('NEGATIVE_BASELINE', 'HEAD')
    files = [prepare, manifest]
    with mutating_sources(root, files, baseline=baseline) as snapshots:
        originals = {path: snapshots[path].decode() for path in files}
        for mutant in mutants:
            path = mutant['path']
            assert originals[path].count(mutant['before']) == 1, (
                mutant['name'], 'mutation anchor must be unique')

        def run(name, test, gate=None):
            with (output / f'{name}.log').open('w') as log:
                gate = gate or wait_for_slot(root / 'scripts/cargo-slots.sh')
                log.write(gate.stdout + gate.stderr)
                log.flush()
                result = run_owned(
                    command + ([test, '--', '--exact', '--nocapture'] if test else []),
                    cwd=root,
                    env=env,
                    timeout=600,
                )
                log.write(result.stdout + result.stderr)
                return result.returncode

        def failure_shape(mutant, raw):
            # A real test failure, not a compile error, and the named test ran.
            if (
                f"test {mutant['test']} ... FAILED" not in raw
                or 'test result: FAILED' not in raw
                or 'could not compile' in raw
            ):
                return False
            # Every accepted failure must carry a panic location in this test
            # file; the anchor then pins which branch/message did it.
            if f'panicked at {TEST_FILE}:' not in raw or mutant['anchor'] not in raw:
                return False
            if mutant['failure'] == 'assertion':
                return 'assertion' in raw
            return True

        assert run('positive', '') == 0, 'positive tests must pass before mutating'
        failures = []
        try:
            for mutant in mutants:
                name, path = mutant['name'], mutant['path']
                original = originals[path]
                assert path.read_text() == original, 'another writer changed the product source'
                mutated = original.replace(mutant['before'], mutant['after'], 1)
                # Reserve the Cargo slot before exposing the mutation.
                gate = wait_for_slot(root / 'scripts/cargo-slots.sh')
                path.write_text(mutated)
                try:
                    try:
                        code = run(name, mutant['test'], gate)
                        raw = (output / f'{name}.log').read_text()
                        killed = code == 101 and failure_shape(mutant, raw)
                        reason = None if killed else (
                            'bound test still passed' if code == 0
                            else f'exit={code}: bound failure missing'
                        )
                    except Exception as error:
                        # A timeout or harness fault is INVALID, not a reason to
                        # stop: the remaining mutants must still be classified.
                        code, killed = None, False
                        reason = f'{type(error).__name__}: {error}'
                    status = (
                        'KILLED' if killed
                        else 'SURVIVED' if code == 0
                        else 'INVALID'
                    )
                    results.append(dict(
                        name=name,
                        file=str(path.relative_to(root)),
                        test=mutant['test'],
                        failure=mutant['failure'],
                        anchor=mutant['anchor'],
                        exit=code,
                        status=status,
                        reason=reason,
                    ))
                    print(f'{name}: {status}, exit={code}', flush=True)
                    if status != 'KILLED':
                        failures.append(name)
                finally:
                    assert path.read_text() == mutated, 'another writer changed the mutant'
                    path.write_text(original)
        finally:
            restored = {
                path.name: path.read_text() == originals[path]
                for path in files
            }
            assert all(restored.values()), restored
        restored_ok, restored_error = restore_positive(run)
        payload = summary_payload(
            results=results,
            failures=failures,
            restored=restored,
            restored_ok=restored_ok,
            restored_error=restored_error,
            before_sha256={path.name: hashlib.sha256(
                originals[path].encode()).hexdigest() for path in files},
            after_sha256={path.name: hashlib.sha256(
                path.read_bytes()).hexdigest() for path in files},
        )
        write_summary(output, payload)
        counts = payload['counts']
        print('summary: ' + ', '.join(
            f'{state}={count}' for state, count in counts.items()), flush=True)
        if failures or not restored_ok:
            detail = f'; restored_error={restored_error}' if restored_error else ''
            print(
                f'NOT KILLED: {", ".join(failures) or "<none>"}; '
                f'restored_passed={restored_ok}{detail}',
                file=sys.stderr,
            )
            sys.exit(1)


if __name__ == '__main__':
    main()
