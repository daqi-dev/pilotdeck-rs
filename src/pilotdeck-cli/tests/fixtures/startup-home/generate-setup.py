#!/usr/bin/env python3
"""Fixed TS setup with injected fake OS home; all IO stays inside new output."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import selectors
import subprocess
import time

p = argparse.ArgumentParser()
p.add_argument('--baseline', type=Path, required=True)
p.add_argument('--node', type=Path, required=True)
p.add_argument('--output', type=Path, required=True)
a = p.parse_args()
base = a.baseline.resolve(strict=True)
node = a.node.resolve(strict=True)
out = a.output.resolve()
out.mkdir(mode=0o700, parents=False, exist_ok=False)
sha = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85'
source = 'src/cli/commands/gatewaySetup.ts'
original = subprocess.check_output(['git', '-C', str(base), 'show', sha + ':' + source])
assert subprocess.check_output(['git', '-C', str(base), 'rev-parse', 'HEAD'], text=True).strip() == sha
assert original == (base / source).read_bytes()
version = json.loads(subprocess.check_output([str(node), '-p', 'JSON.stringify({node:process.version,icu:process.versions.icu})']))
assert version == {'node': 'v22.22.0', 'icu': '78.2'}
assert json.loads((base / 'node_modules/weixin-ilink/package.json').read_text())['version'] == '0.1.0'
qr = {'json': {'qrcode': 'fixture-code', 'qrcode_img_content': 'https://fixture.invalid/qr'}}
confirmed = {'json': {'status': 'confirmed', 'ilink_bot_id': 'fixture-account', 'bot_token': 'fixture-token', 'baseurl': 'https://fixture.invalid/api'}}
cases = [
    {'id': 'absent-pilot-cancel', 'pilot': None, 'home': None},
    {'id': 'empty-pilot-cancel', 'pilot': '', 'home': None},
    {'id': 'literal-tilde-cancel', 'pilot': '~', 'home': None},
    {'id': 'relative-pilot-cancel', 'pilot': 'pilot', 'home': None},
    {'id': 'absolute-pilot-cancel', 'pilot': '<CASE>/pilot', 'home': None},
    {'id': 'weixin-cancel', 'pilot': 'pilot', 'home': None, 'argv': ['weixin'], 'answers': [['继续扫码登录？ [Y/n]: ', 'n']]},
    {'id': 'weixin-save-unset', 'pilot': 'pilot', 'home': None, 'save': True},
    {'id': 'weixin-save-empty', 'pilot': 'pilot', 'home': '', 'save': True},
    {'id': 'weixin-save-relative', 'pilot': 'pilot', 'home': 'relative-home', 'save': True},
    {'id': 'weixin-save-missing-parent', 'pilot': 'pilot', 'home': None, 'save': True, 'missingParent': True},
]
rows = []
for case in cases:
    root = out / case['id']
    root.mkdir(mode=0o700)
    fixture = dict(case, account=str(root / 'account'))
    fixture.setdefault('argv', [])
    fixture.setdefault('answers', [['请选择 [1/2/3/4/q]: ', 'q']])
    if case.get('save'):
        fixture.update(argv=['weixin'], answers=[['继续扫码登录？ [Y/n]: ', '']], responses=[qr, confirmed])
        if not case.get('missingParent'):
            (root / 'pilot').mkdir()
    tmp = root / 'tmp'
    tmp.mkdir()
    data = root / 'case.json'
    data.write_text(json.dumps(fixture, ensure_ascii=False))
    env = dict(os.environ, TMPDIR=str(tmp), TSX_DISABLE_CACHE='1',
               SETUP_EVENTS=str(root / 'fetch-events.json'), SETUP_HOME_EVENTS=str(root / 'home-events.json'))
    for key in ['NODE_OPTIONS', 'HOME', 'PILOT_HOME', 'PILOTDECK_CONFIG_PATH']:
        env.pop(key, None)
    if case['home'] is not None:
        env['HOME'] = case['home']
    if case['pilot'] is not None:
        env['PILOT_HOME'] = case['pilot'].replace('<CASE>', str(root))
    command = [str(node), '--import', str(base / 'node_modules/tsx/dist/loader.mjs'),
               str(Path(__file__).with_name('setup-driver.mjs').resolve()), str(data), str(base / source)]
    child = subprocess.Popen(command, cwd=root, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    sel = selectors.DefaultSelector()
    sel.register(child.stdout, selectors.EVENT_READ)
    stdout = b''
    answer = 0
    deadline = time.monotonic() + 12
    while child.poll() is None and time.monotonic() < deadline:
        for key, _ in sel.select(.03):
            stdout += os.read(key.fd, 65536)
            if answer < len(fixture['answers']) and fixture['answers'][answer][0].encode() in stdout:
                child.stdin.write((fixture['answers'][answer][1] + '\n').encode())
                child.stdin.flush()
                answer += 1
    timeout = child.poll() is None
    if timeout:
        child.kill()
    rest, stderr = child.communicate(timeout=3)
    sel.close()
    stdout += rest
    # Record only known product output roots. Driver input/logs/cache aren't
    # product files; the whole child process is owned and already reaped.
    files = {}
    for folder in ['account', 'pilot', '.pilotdeck', 'relative-home', '~']:
        directory = root / folder
        if directory.exists():
            for file in directory.rglob('*'):
                if file.is_file():
                    files[str(file.relative_to(root))] = file.read_text()
    row = dict(case, stdout=stdout.decode(), stderr=stderr.decode(), code=child.returncode,
               timeout=timeout, answered=answer, files=files,
               accountPilotCreated=(root / 'account/.pilotdeck').exists(),
               emptyPilotCreated=(root / '.pilotdeck').exists(),
               relativePilotCreated=(root / 'relative-home/.pilotdeck').exists(),
               homedirCalls=json.loads((root / 'home-events.json').read_text()),
               fetch=json.loads((root / 'fetch-events.json').read_text()))
    (root / 'raw.json').write_text(json.dumps(row, ensure_ascii=False, indent=2) + '\n')
    assert not timeout and child.returncode == 0 and answer == len(fixture['answers']), case['id']
    # New committed fixture uses a clear case-root placeholder. Raw originals
    # retain exact bytes and absolute paths in the out-of-repo evidence dir.
    rows.append(json.loads(json.dumps(row, ensure_ascii=False).replace(str(root), '<CASE>')))
result = dict(baseline=sha, sourceSha256=hashlib.sha256(original).hexdigest(), **version, cases=rows)
(out / 'setup-home-oracle.json').write_text(json.dumps(result, ensure_ascii=False, indent=2) + '\n')
print('recorded', len(rows), 'setup HOME cases')
