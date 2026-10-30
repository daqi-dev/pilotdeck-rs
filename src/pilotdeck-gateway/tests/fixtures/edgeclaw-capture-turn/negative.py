#!/usr/bin/env python3
"""Prove each capture-turn check fails on its own real product mutation.

Run from the repository root after the work is committed (mutating_sources
refuses to mutate a product tree that differs from HEAD):

    python3 src/pilotdeck-gateway/tests/fixtures/edgeclaw-capture-turn/negative.py \
        _tmp/20261026-issue-759/negative-capture-turn [mutant ...]

Optional mutant names select a sample run; the positive and restored controls
always execute, and every selected mutant is still classified before the
summary.

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
from types import SimpleNamespace

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_panic import named_test_killed
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import classify as shared_classify

TEST_FILE = 'pilotdeck-gateway/tests/edgeclaw_capture_turn.rs'


def classify_control(mutant, code, raw):
    # The shared classifier (#818 negative_panic criteria) already requires the
    # named test's own FAILED line and panic payload; these checks only add
    # what is specific to this fixture.
    result = SimpleNamespace(returncode=code, stdout=raw, stderr='')
    bound = SimpleNamespace(test=mutant['test'], assertion=mutant['anchor'])
    status, reason = shared_classify(result, bound)
    if status != 'KILLED':
        return status, reason
    if (code != 101 or 'could not compile' in raw
            or f'panicked at {TEST_FILE}:' not in raw
            or (mutant['failure'] == 'assertion' and 'assertion' not in raw)):
        return 'INVALID', 'missing fixture-specific runtime failure'
    # #818 fix-r10 (P1): the shared classify does not carry the pre-rebase
    # strict judge, so keep requiring it too. `named_test_killed` rejects a
    # watchdog/timeout payload (negative_panic HANG_PREFIXES, #813 fix-r8) --
    # a hang is not an assertion kill even when the anchor text appears in
    # it -- and requires the anchor to be a scoped marker in the named
    # test's own panic block.
    if not named_test_killed(raw, mutant['test'], mutant['anchor']):
        return 'INVALID', 'hang/timeout or unscoped payload is not a kill'
    return status, reason


def selected_names(mutants, names):
    """Validate an optional sample selection; empty means run every mutant.

    Sample runs (#747/#786) still bracket the selection with the positive and
    restored controls. Unknown or duplicate names refuse to start rather than
    silently skipping rows.
    """
    if not names:
        return set()
    known = {mutant['name'] for mutant in mutants}
    unknown = sorted(set(names) - known)
    if unknown:
        raise SystemExit(f'unknown mutants: {", ".join(unknown)}')
    if len(set(names)) != len(names):
        raise SystemExit('duplicate mutant names')
    return set(names)


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
    pending = output / 'summary.json.tmp'
    pending.write_text(json.dumps(payload, indent=2) + '\n')
    pending.replace(output / 'summary.json')


def main():
    root = Path(__file__).resolve().parents[5]
    output = Path(sys.argv[1]).resolve()
    output.mkdir(parents=True, exist_ok=True)
    prepare = root / 'src/pilotdeck-gateway/src/runtime/context/prepare.rs'
    manifest = root / 'src/pilotdeck-gateway/src/host/modules/manifest.rs'
    env = dict(os.environ)
    if not env.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be provided by the caller')
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
            # Bind both halves: the named test's own rejection wording and the
            # TS product text it is supposed to match (#786). A mutant that
            # only kills the rejection cannot satisfy the anchor with the
            # test's wording alone.
            "#786 non-string sessionId must reject like isAlwaysOnSession (TS: reading 'startsWith')",
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
            # Exclusive message of the product-error-text assertion in
            # malformed_session_id_matches_ts_startswith_failure: still Err,
            # but wrong text.
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
        mutate(
            # Only the missing-key arm changes: an absent `errored` becomes a
            # phantom `false` key, which the key-set comparison must catch
            # even though JSON serialization would hide the difference (#786).
            'errored-missing-injected-false',
            prepare,
            '            errored: fields.get("errored").cloned().unwrap_or(JsValue::Undefined),\n',
            '            errored: fields.get("errored").cloned().unwrap_or(JsValue::Bool(false)),\n',
            'recorded_call_key_sets_match_ts',
            'panic',
            'recorded call key set',
        ),
    ]
    # Optional sample mode (#747/#786): naming mutants runs only those rows;
    # the positive and restored controls still bracket them.
    selected = selected_names(mutants, sys.argv[2:])
    results = []

    # The source guard must use HEAD; uncommitted trees are not mutation inputs.
    files = [prepare, manifest]
    with mutating_sources(root, files) as snapshots:
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
                # No `--nocapture`: it interleaves a test's stdout with the
                # `test <name> ... FAILED` status line the shared judge reads
                # (#938 r14; #818 fix-r7 removed it from every shared-criteria
                # control).
                result = run_owned(
                    command + ([test, '--', '--exact'] if test else []),
                    cwd=root,
                    env=env,
                    timeout=600,
                )
                log.write(result.stdout + result.stderr)
                return result.returncode

        assert run('positive', '') == 0, 'positive tests must pass before mutating'
        failures = []
        try:
            for mutant in mutants:
                if selected and mutant['name'] not in selected:
                    continue
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
                        status, reason = classify_control(mutant, code, raw)
                    except Exception as error:
                        # Account for every mutant even when its runner fails.
                        code, status = None, 'INVALID'
                        reason = f'{type(error).__name__}: {error}'
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
