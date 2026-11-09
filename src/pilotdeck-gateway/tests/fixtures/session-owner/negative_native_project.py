#!/usr/bin/env python3
"""Compile real product mutations in an owned /tmp workspace; never edit the checkout."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / '_shared'))
from negative_control import source_copy
from negative_panic import named_test_killed
from negative_selection import baseline_passed
from negative_process import run_owned, wait_for_slot


def main():
    rust = Path(__file__).resolve().parents[4]
    output = Path(sys.argv[1]).resolve()
    output.mkdir(parents=True, exist_ok=True)
    assert os.environ.get('PDRS_BASELINE'), 'PDRS_BASELINE is required for the shared replay guard'
    assert os.environ.get('CARGO_HOME'), 'CARGO_HOME is required'
    env = dict(os.environ, CARGO_NET_OFFLINE='true')
    owner = 'pilotdeck-gateway/src/runtime/session/owner.rs'
    read = 'pilotdeck-gateway/src/runtime/turn/read.rs'
    fixture = 'pilotdeck-gateway/tests/fixtures/session-owner/oracle.json'
    module = 'runtime::session::owner::tests::'
    mutants = [
        ('foreign-retained-snapshot-exposed', read, '    coordinator\n        .snapshot_for_project(input, project)\n        .unwrap_or_else(|| ActiveTurnSnapshot {\n            active: false,\n            session_key: input.session_key.clone(),\n            run_id: None,\n            events: Vec::new(),\n            truncated: false,\n            terminal: false,\n        })', '    let _ = project; coordinator.snapshot(input)', 'native_owner_close_keeps_retained_replay_scoped_to_original_project', 'assertion failed:'),
        ('unbound-owner-creation-accepted', owner, 'if state.project_binding {\n            let Some(project) = context.project_key.as_ref() else {\n                return Box::pin(async { Err(project_mismatch()) });\n            };', 'if state.project_binding && context.project_key.is_some() {\n            let Some(project) = context.project_key.as_ref() else {\n                return Box::pin(async { Err(project_mismatch()) });\n            };', 'native_project_binding_rejects_unbound_creation_before_and_during_scope', 'native unbound creation must not create a wildcard owner'),
    ]
    originals = {name: (rust / name).read_text() for name in {m[1] for m in mutants} | {fixture}}
    oracle = json.loads(originals[fixture])
    source_hash = oracle['sourceSha256']['src/gateway/SessionRouter.ts']
    mutants.append(('tampered-source-hash', fixture, source_hash, '0' * 64,
                    'dirty_identity_and_publication_matches_ts',
                    'oracle hash for src/gateway/SessionRouter.ts does not match'))
    for name, file, before, after, test, marker in mutants:
        assert originals[file].count(before) == 1, (name, originals[file].count(before))
    results = []
    failures = []
    with tempfile.TemporaryDirectory(prefix='pdrs-session-owner-negative-', dir='/tmp') as directory:
        root = Path(directory)
        scratch = root / 'src'
        env['CARGO_TARGET_DIR'] = str(scratch / 'target')
        # Same workspace, dependencies and manifests; no new dependency or mock
        # copy of project/refresh behavior is introduced by this harness.
        # One copy manifest for every control: `source_copy` carries `src/`
        # plus the compile-time inputs derived from it, so a new input class
        # cannot reach some controls and miss this one (#1570 r14 sibling).
        source_copy(root)
        for name, file, before, after, test, marker in [('baseline', None, None, None, '', 'test result: ok')] + mutants:
            for path, original in originals.items():
                content = original
                if path == file:
                    assert content.count(before) == 1, (name, content.count(before))
                    content = content.replace(before, after, 1)
                (scratch / path).write_text(content)
            gate = wait_for_slot(rust.parent / 'scripts/cargo-slots.sh')
            command = ['cargo', 'test', '--offline', '--manifest-path', str(scratch / 'Cargo.toml'),
                       '-p', 'pilotdeck-gateway', '--lib', module + test]
            command += ['--', '--exact'] if test else []
            result = run_owned(command, env=env, timeout=600 if name == 'baseline' else 240)
            raw = result.stdout + result.stderr
            (output / f'negative-{name}.log').write_text(gate.stdout + gate.stderr + raw + f'\nexit={result.returncode}\n')
            valid = (result.returncode == 0 and 'test result: ok.' in raw) if name == 'baseline' else (
                result.returncode == 101 and named_test_killed(raw, module + test, marker)
                and 'could not compile' not in raw)
            results.append(dict(name=name, file=file, before=before, after=after, test=module + test,
                                exit=result.returncode, marker=marker, valid=valid))
            (output / 'negative-summary.json').write_text(json.dumps(results, ensure_ascii=False, indent=2) + '\n')
            print(f'{name}: {"PASS" if valid else "INVALID"} exit={result.returncode}', flush=True)
            if not valid:
                failures.append(name)
    if failures:
        print(f'not killed: {", ".join(failures)}', file=sys.stderr)
        return 1
    print('all mutations compiled and failed their targeted assertion; /tmp workspace removed')
    return 0


if __name__ == '__main__':
    sys.exit(main())
