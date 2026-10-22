#!/usr/bin/env python3
"""Compile product mutations in an owned /tmp workspace; retain failure evidence."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import run_owned, wait_for_slot


def main():
    rust = Path(__file__).resolve().parents[4]
    output = Path(sys.argv[1]).resolve()
    output.mkdir(parents=True, exist_ok=True)
    assert os.environ.get('CARGO_HOME'), 'set CARGO_HOME'
    assert os.environ.get('PDRS_BASELINE'), 'set PDRS_BASELINE'
    handle = 'pilotdeck-gateway/src/runtime/session/handle.rs'
    fixture = 'pilotdeck-gateway/tests/fixtures/session-abort-contract/oracle.json'
    test = 'runtime::session::handle::abort_contract_tests::abort_at_lease_and_dispose_windows_matches_ts'
    trace = 'lease/dispose abort trace; active=false'
    mutants = [
        ('suppress-abort-while-draining', handle,
         'if progress.state == HandleState::Disposed {',
         'if progress.state != HandleState::Active {', trace),
        ('disposed-before-lease-release', handle,
         'if let Some(mut lease) = guard.lease.take()',
         'handle.0.progress.lock().unwrap().state = HandleState::Disposed;\n'
         '            if let Some(mut lease) = guard.lease.take()', trace),
        ('forward-abort-after-disposed', handle,
         'if progress.state == HandleState::Disposed {', 'if false {', trace),
        # Production order (TS createAgentSession.ts:229,234) releases the
        # project lease before disposing session resources. Reverting to the
        # pre-alignment Rust order opens the session window first, so the
        # lease window never opens; the replay is caught by its own designed
        # in-test 3-second bound, not by any external timeout.
        ('reverted-cleanup-order', handle,
         'let mut disposal_failures = Vec::new();\n'
         '            if let Some(mut lease) = guard.lease.take()\n'
         '                && let Err(error) = lease.release().await\n'
         '            {\n'
         '                disposal_failures.push(error);\n'
         '            }\n'
         '            if let Err(error) = handle.0.session.dispose().await {\n'
         '                disposal_failures.push(error);\n'
         '            }',
         'let mut disposal_failures = Vec::new();\n'
         '            if let Err(error) = handle.0.session.dispose().await {\n'
         '                disposal_failures.push(error);\n'
         '            }\n'
         '            if let Some(mut lease) = guard.lease.take()\n'
         '                && let Err(error) = lease.release().await\n'
         '            {\n'
         '                disposal_failures.push(error);\n'
         '            }', 'controlled dispose/lease interleaving hung'),
    ]
    originals = {name: (rust / name).read_text() for name in {m[1] for m in mutants} | {fixture}}
    oracle = json.loads(originals[fixture])
    source = 'src/agent/scope/AgentHandle.ts'
    mutants.append(('tampered-source-hash', fixture,
                    oracle['sourceSha256'][source], '0' * 64,
                    f'oracle hash for {source} does not match'))
    for name, file, before, _after, _marker in mutants:
        assert originals[file].count(before) == 1, (name, originals[file].count(before))
    results = []
    with tempfile.TemporaryDirectory(prefix='pdrs-abort-contract-negative-', dir='/tmp') as directory:
        scratch = Path(directory) / 'src'
        shutil.copytree(rust, scratch, ignore=shutil.ignore_patterns(
            'target', '.git', 'node_modules', '.env*'))
        shutil.copytree(rust.parent / 'specs', Path(directory) / 'specs')
        env = dict(os.environ, CARGO_NET_OFFLINE='true', CARGO_TARGET_DIR=str(scratch / 'target'))
        # Restore every mutated file each round, then inject only the current
        # mutation; a mutation must never ride along into a later round.
        for name, file, before, after, marker in [('baseline', None, None, None, None)] + mutants:
            for path, original in originals.items():
                content = original
                if path == file:
                    content = content.replace(before, after, 1)
                (scratch / path).write_text(content)
            gate = wait_for_slot(rust.parent / 'scripts/cargo-slots.sh')
            command = ['cargo', 'test', '--offline', '-p', 'pilotdeck-gateway',
                       '--lib', test, '--', '--exact', '--nocapture']
            result = run_owned(command, cwd=scratch, env=env, timeout=600)
            raw = result.stdout + result.stderr
            (output / f'negative-{name}.log').write_text(gate.stdout + gate.stderr + raw + f'\nexit={result.returncode}\n')
            valid = ('1 passed; 0 failed' in raw and result.returncode == 0) if name == 'baseline' else (
                result.returncode == 101 and marker in raw
                and '0 passed; 1 failed' in raw and 'could not compile' not in raw)
            results.append(dict(name=name, file=file, before=before, after=after,
                                command=command, exit=result.returncode, valid=valid))
            (output / 'negative-summary.json').write_text(json.dumps(results, indent=2) + '\n')
            print(f'{name}: {"PASS" if valid else "INVALID"} exit={result.returncode}', flush=True)
            if not valid:
                raise SystemExit(1)
    print('all mutations detected; scratch workspace restored and removed', flush=True)


if __name__ == '__main__':
    main()
