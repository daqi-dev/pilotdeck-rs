#!/usr/bin/env python3
"""Bounded read-only-baseline probes: isolated FIFO and 1MiB+ credential JSON."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import selectors
import signal
import subprocess
import time

p = argparse.ArgumentParser()
p.add_argument('--baseline', required=True, type=Path)
p.add_argument('--node', required=True, type=Path)
p.add_argument('--output', required=True, type=Path)
a = p.parse_args()
base, node, out = a.baseline.resolve(), a.node.resolve(), a.output.resolve()
out.mkdir(parents=True, exist_ok=False)
sha = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85'
source = 'src/cli/commands/gatewaySetup.ts'
original = subprocess.check_output(['git', '-C', str(base), 'show', sha + ':' + source])
assert subprocess.check_output(['git', '-C', str(base), 'rev-parse', 'HEAD'], text=True).strip() == sha
assert original == (base / source).read_bytes()
versions = json.loads(subprocess.check_output([str(node), '-p', 'JSON.stringify({node:process.version,icu:process.versions.icu})'], text=True))
assert versions == {'node': 'v22.22.0', 'icu': '78.2'}
assert json.loads((base / 'node_modules/weixin-ilink/package.json').read_text())['version'] == '0.1.0'
results = []
for fifo in [True, False]:
    home = out / ('fifo-no-writer' if fifo else 'oversize-json')
    home.mkdir()
    pilot, tmp = home / 'pilot', home / 'tmp'
    pilot.mkdir()
    tmp.mkdir()
    creds, config = pilot / 'weixin-credentials.json', pilot / 'pilotdeck.yaml'
    config.write_text('keep: original\n')
    if fifo:
        os.mkfifo(creds, 0o600)
    else:
        creds.write_text(json.dumps({'baseUrl': 'https://fixture.invalid/old', 'botToken': 'old-token', 'accountId': 'old-account', 'padding': 'x' * (1024 * 1024)}))
    before = creds.stat()
    before_hash = None if fifo else hashlib.sha256(creds.read_bytes()).hexdigest()
    case = home / 'case.json'
    case.write_text(json.dumps({'argv': ['weixin']}))
    env = {**os.environ, 'HOME': 'home', 'PILOT_HOME': 'pilot', 'PILOTDECK_CONFIG_PATH': 'pilot/pilotdeck.yaml', 'TMPDIR': str(tmp), 'SETUP_EVENTS': str(home / 'events.json')}
    env.pop('NODE_OPTIONS', None)
    child = subprocess.Popen([str(node), '--import', str(base / 'node_modules/tsx/dist/loader.mjs'), str(Path(__file__).with_name('weixin-driver.mjs').resolve()), str(case), str(base / source)], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env, cwd=home)
    sel = selectors.DefaultSelector()
    sel.register(child.stdout, selectors.EVENT_READ)
    stdout, answered = b'', False
    deadline = time.monotonic() + 2
    while child.poll() is None and time.monotonic() < deadline:
        for key, _ in sel.select(.02):
            stdout += os.read(key.fd, 65536)
            if not fifo and not answered and '重新登录？ [y/N]: '.encode() in stdout:
                child.stdin.write(b'n\n')
                child.stdin.flush()
                answered = True
    timed_out = child.poll() is None
    if timed_out:
        child.kill()
    rest, stderr = child.communicate()
    stdout += rest
    after = creds.stat()
    result = {'id': home.name, 'stdout': stdout.decode(), 'stderr': stderr.decode(), 'code': child.returncode, 'timeoutAtTwoSeconds': timed_out, 'answered': answered, 'credentialSize': before.st_size, 'credentialMetadataUnchanged': (before.st_ino, before.st_mode, before.st_size) == (after.st_ino, after.st_mode, after.st_size), 'credentialHashUnchanged': None if fifo else hashlib.sha256(creds.read_bytes()).hexdigest() == before_hash, 'config': config.read_text()}
    (home / 'result.json').write_text(json.dumps(result, ensure_ascii=False, indent=2) + '\n')
    assert result['credentialMetadataUnchanged']
    assert timed_out and child.returncode == -signal.SIGKILL if fifo else not timed_out and child.returncode == 0 and answered and result['credentialHashUnchanged']
    results.append(result)
(out / 'summary.json').write_text(json.dumps({'baseline': sha, 'sourceSha256': hashlib.sha256(original).hexdigest(), **versions, 'sdkVersion': '0.1.0', 'cases': results}, ensure_ascii=False, indent=2) + '\n')
print('confirmed FIFO blocking and oversize-JSON acceptance in fixed TS baseline')
