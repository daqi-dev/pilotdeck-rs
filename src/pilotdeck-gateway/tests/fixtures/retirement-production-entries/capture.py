# anchor-selfcheck: unsupported runtime capture and snapshot hashes, no product source mutations
"""Sequential real Server captures using identical paths and ports on both sides.

The outer collector owns the host-wide lease and network-none container. Each
probe owns/reaps its actual product groups before the original directory moves.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import sys

sys.dont_write_bytecode = True
parser = argparse.ArgumentParser()
for name in ('baseline', 'node', 'cli', 'sidecar', 'source-manifest', 'candidate-manifest', 'out'):
    parser.add_argument('--' + name, type=Path, required=True)
parser.add_argument('--mode', choices=('http', 'cron', 'alwayson'), action='append')
a = parser.parse_args()
assert socket.if_nameindex() == [(1, 'lo')]
output = a.out.resolve()
output.mkdir(parents=True, exist_ok=False)
probe = Path(__file__).with_name('probe.py').resolve()
rows = []
for mode in a.mode or ('http', 'cron', 'alwayson'):
    for engine in ('ts', 'native'):
        current = output / 'cur'
        destination = output / (mode + '-' + engine)
        assert not current.exists() and not destination.exists()
        argv = [sys.executable, '-I', '-B', str(probe), '--engine', engine,
                '--mode', mode, '--out', str(current), '--provider-port', '28531',
                '--gateway-port', '28478']
        for name in ('baseline', 'node', 'cli', 'sidecar', 'source-manifest', 'candidate-manifest'):
            argv.extend(['--' + name, str(getattr(a, name.replace('-', '_')).resolve())])
        name = mode + '-' + engine
        with (output / (name + '.command.json')).open('x') as stream:
            json.dump(argv, stream, indent=2)
        with (output / (name + '.stdout')).open('xb') as stdout, (output / (name + '.stderr')).open('xb') as stderr:
            child = subprocess.Popen(argv, stdout=stdout, stderr=stderr, start_new_session=True)
            # Timeout leaves the original root untouched and outer ownership
            # cleanup responsible; never begin the other writer after failure.
            code = child.wait(timeout=175)
        try:
            os.killpg(child.pid, 0)
        except ProcessLookupError:
            pass
        else:
            raise RuntimeError('collector process group remains after reap')
        raw = json.loads((current / 'result.json').read_text())
        assert raw['ownedCleanup']['allStopped'] is True
        for port in (28478, 28531):
            with socket.socket() as sock:
                sock.settimeout(.5)
                assert sock.connect_ex(('127.0.0.1', port)) != 0, 'owned listener remains'
        before = current.stat()
        originals = {part: hashlib.sha256((current / part).read_bytes()).hexdigest()
                     for part in ('result.json', 'provenance.json')}
        current.rename(destination)
        after = destination.stat()
        assert (before.st_dev, before.st_ino) == (after.st_dev, after.st_ino)
        assert all(hashlib.sha256((destination / part).read_bytes()).hexdigest() == value
                   for part, value in originals.items())
        row = {'mode': mode, 'engine': engine, 'exitCode': code,
               'originalRuntime': str(current), 'preservedAt': str(destination),
               'device': before.st_dev, 'inode': before.st_ino, 'originals': originals}
        rows.append(row)
        with (output / (name + '.preservation.json')).open('x') as stream:
            json.dump(row, stream, indent=2)
        if code:
            raise SystemExit(code)
with (output / 'batch.json').open('x') as stream:
    json.dump({'cases': rows, 'scope': 'ORIGINALS_ONLY_NO_PARITY_VERDICT'}, stream, indent=2)
