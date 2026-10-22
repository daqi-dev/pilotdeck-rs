#!/usr/bin/env python3
"""Compile product mutations in an owned /tmp workspace; retain failure evidence."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time


def main():
    rust = Path(__file__).resolve().parents[4]
    output = Path(sys.argv[1]).resolve()
    output.mkdir(parents=True, exist_ok=True)
    assert os.environ.get('CARGO_HOME'), 'set CARGO_HOME'
    assert os.environ.get('PDRS_BASELINE'), 'set PDRS_BASELINE'
    handle = 'pilotdeck-gateway/src/runtime/session/handle.rs'
    original = (rust / handle).read_text()
    test = 'runtime::session::handle::abort_contract_tests::abort_at_dispose_and_lease_windows_matches_ts'
    mutants = [
        ('suppress-abort-while-draining',
         'if progress.state == HandleState::Disposed {',
         'if progress.state != HandleState::Active {'),
        ('disposed-before-lease-release',
         'if let Some(mut lease) = guard.lease.take()',
         'handle.0.progress.lock().unwrap().state = HandleState::Disposed;\n'
         '            if let Some(mut lease) = guard.lease.take()'),
        ('forward-abort-after-disposed',
         'if progress.state == HandleState::Disposed {', 'if false {'),
    ]
    for name, before, _ in mutants:
        assert original.count(before) == 1, (name, original.count(before))
    results = []
    with tempfile.TemporaryDirectory(prefix='pdrs-abort-contract-negative-', dir='/tmp') as directory:
        scratch = Path(directory) / 'src'
        shutil.copytree(rust, scratch, ignore=shutil.ignore_patterns(
            'target', '.git', 'node_modules', '.env*'))
        shutil.copytree(rust.parent / 'specs', Path(directory) / 'specs')
        env = dict(os.environ, CARGO_NET_OFFLINE='true', CARGO_TARGET_DIR=str(scratch / 'target'))
        for name, before, after in [('baseline', None, None)] + mutants:
            (scratch / handle).write_text(original if before is None else original.replace(before, after, 1))
            gate_logs = []
            while True:
                gate = subprocess.run(['/Users/da/ws/pilotdeck-rs/scripts/cargo-slots.sh'],
                                      capture_output=True, text=True, check=False)
                gate_logs.append(gate.stdout + gate.stderr)
                if gate.returncode == 0:
                    break
                assert gate.returncode == 1, gate_logs[-1]
                print('cargo slots full; waiting 30s', flush=True)
                time.sleep(30)
            command = ['cargo', 'test', '--offline', '-p', 'pilotdeck-gateway',
                       '--lib', test, '--', '--exact', '--nocapture']
            result = subprocess.run(command, cwd=scratch, env=env, capture_output=True,
                                    text=True, timeout=600)
            raw = result.stdout + result.stderr
            (output / f'negative-{name}.log').write_text(''.join(gate_logs) + raw + f'\nexit={result.returncode}\n')
            valid = ('1 passed; 0 failed' in raw and result.returncode == 0) if name == 'baseline' else (
                result.returncode == 101 and 'dispose/lease abort trace; active=false' in raw
                and '0 passed; 1 failed' in raw and 'could not compile' not in raw)
            results.append(dict(name=name, file=handle, before=before, after=after,
                                command=command, exit=result.returncode, valid=valid))
            (output / 'negative-summary.json').write_text(json.dumps(results, indent=2) + '\n')
            print(f'{name}: {"PASS" if valid else "INVALID"} exit={result.returncode}', flush=True)
            if not valid:
                raise SystemExit(1)
        (scratch / handle).write_text(original)
    print('all mutations detected; scratch workspace restored and removed', flush=True)


if __name__ == '__main__':
    main()
