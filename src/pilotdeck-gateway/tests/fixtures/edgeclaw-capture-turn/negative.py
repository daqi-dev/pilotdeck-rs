#!/usr/bin/env python3
"""Prove each capture-turn check fails on its own real product mutation.

Run from the repository root after the work is committed (mutating_sources
refuses to mutate a product tree that differs from HEAD):

    python3 src/pilotdeck-gateway/tests/fixtures/edgeclaw-capture-turn/negative.py \
        _tmp/20261026-issue-759/negative-capture-turn
"""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot


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
    # (name, file, before, after, test, marker)
    mutants = [
        (
            'fork-carryover-filter-removed',
            prepare,
            '.filter(|message| !super::memory::fork_carryover(message))',
            '.filter(|_message| true)',
            'ts_capture_turn_scenarios_match',
            'captureTurn scenarios',
        ),
        (
            'always-on-skip-removed',
            prepare,
            'if is_always_on_session(&session_id) {',
            'if false && is_always_on_session(&session_id) {',
            'always_on_sessions_are_skipped',
            'must not reach the resolver',
        ),
        (
            'capture-errors-not-swallowed',
            prepare,
            'Err(_) => Ok(()),',
            'Err(error) => Err(ModuleError::new(error).into()),',
            'resolver_failures_are_swallowed',
            'captureTurn must never reject',
        ),
        (
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
            "reading 'startsWith'",
        ),
        (
            'supports-capture-turn-removed',
            prepare,
            '                | Operation::CaptureTurn\n',
            '',
            'manifest_advertises_capture_turn',
            'capture_turn',
        ),
        (
            'manifest-capture-turn-removed',
            manifest,
            'if context.capture_turn {',
            'if false {',
            'manifest_advertises_capture_turn',
            'manifest context methods',
        ),
    ]
    results = []

    # A committed tree passes HEAD. The uncommitted-patch runner can point
    # NEGATIVE_BASELINE at a temporary `git commit-tree` object with the same
    # product bytes, without moving the branch.
    baseline = os.environ.get('NEGATIVE_BASELINE', 'HEAD')
    with mutating_sources(root, [prepare, manifest], baseline=baseline) as snapshots:
        originals = {path: snapshots[path].decode() for path in (prepare, manifest)}
        for name, path, before, _after, _test, _marker in mutants:
            assert originals[path].count(before) == 1, (name, 'mutation anchor must be unique')

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

        assert run('positive', '') == 0, 'positive tests must pass before mutating'
        try:
            for name, path, before, after, test, marker in mutants:
                original = originals[path]
                assert path.read_text() == original, 'another writer changed the product source'
                mutated = original.replace(before, after, 1)
                # Reserve the Cargo slot before exposing the mutation.
                gate = wait_for_slot(root / 'scripts/cargo-slots.sh')
                path.write_text(mutated)
                try:
                    code = run(name, test, gate)
                    raw = (output / f'{name}.log').read_text()
                    killed = (
                        code == 101
                        and 'test result: FAILED' in raw
                        and 'could not compile' not in raw
                        and marker in raw
                    )
                    results.append(
                        dict(name=name, file=str(path.relative_to(root)),
                             test=test, exit=code, marker=marker, killed=killed)
                    )
                    print(f'{name}: killed={killed}, exit={code}', flush=True)
                    assert killed, f'{name}: expected a compiled test assertion failure'
                finally:
                    assert path.read_text() == mutated, 'another writer changed the mutant'
                    path.write_text(original)
        finally:
            restored = {
                path.name: path.read_text() == originals[path]
                for path in (prepare, manifest)
            }
            (output / 'summary.json').write_text(json.dumps(dict(
                results=results,
                restored=restored,
                beforeSha256={path.name: hashlib.sha256(
                    originals[path].encode()).hexdigest() for path in (prepare, manifest)},
                afterSha256={path.name: hashlib.sha256(
                    path.read_bytes()).hexdigest() for path in (prepare, manifest)},
            ), indent=2) + '\n')
            assert all(restored.values()), restored
        assert run('restored', '') == 0, 'restored product must pass'


if __name__ == '__main__':
    main()
