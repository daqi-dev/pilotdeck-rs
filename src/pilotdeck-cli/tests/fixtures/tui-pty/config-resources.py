#!/usr/bin/env python3
"""Bounded configuration failures through the real TUI entry, on mbp2 only."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import pty
import select
import subprocess
import termios
import time

parser = argparse.ArgumentParser()
parser.add_argument('--binary', required=True, type=Path)
parser.add_argument('--output', required=True, type=Path)
args = parser.parse_args()
binary = args.binary.resolve()
output = args.output.resolve()
output.mkdir()
results = []
valid = '''schemaVersion: 1
agent: {model: fixture/mock}
model:
  providers:
    fixture:
      protocol: openai
      url: http://127.0.0.1:1/v1
      apiKey: mock-only
      models:
        mock: {}
gateway:
  port: 1
'''
for case in ('yaml-fifo', 'yaml-oversize', 'binding-fifo'):
    root = output / case
    home, project = root / 'home', root / 'project'
    home.mkdir(parents=True)
    project.mkdir()
    config = home / 'pilotdeck.yaml'
    expected = b'Configuration input must be a regular file.'
    if case == 'yaml-fifo':
        os.mkfifo(config, 0o600)
    elif case == 'yaml-oversize':
        with config.open('xb') as file:
            file.truncate(8388609)
        expected = b'Configuration input exceeds the 8388608-byte limit.'
    else:
        config.write_text(valid)
        (home / 'staffdeck').mkdir()
        os.mkfifo(home / 'staffdeck/project-bindings.json', 0o600)
    env = {'PATH': '/usr/bin:/bin', 'HOME': str(home), 'PILOT_HOME': str(home),
           'TERM': 'xterm-256color', 'LANG': 'en_US.UTF-8'}
    if case == 'binding-fifo':
        env.update(STAFFDECK_PROJECT_BINDING_ENABLED='true',
                   STAFFDECK_COPY_TENANT_ID='fixture-tenant',
                   STAFFDECK_FORMAL_API_ORIGIN='https://staffdeck.invalid',
                   STAFFDECK_SOP_RUNTIME_ORIGIN='https://staffdeck.invalid/runtime',
                   STAFFDECK_SOP_MANAGEMENT_API_KEY='mock-management-only')
    master, slave = pty.openpty()
    before = termios.tcgetattr(slave)
    raw = bytearray()
    child = subprocess.Popen([str(binary), 'tui'], cwd=project, env=env,
                             stdin=slave, stdout=slave, stderr=slave,
                             start_new_session=True)
    started = time.monotonic()
    failure = None
    try:
        while child.poll() is None and time.monotonic() - started < 5:
            readable, _, _ = select.select([master], [], [], .05)
            if readable:
                raw.extend(os.read(master, 65536))
        assert child.poll() == 1, f'exit={child.poll()}'
        while select.select([master], [], [], .05)[0]:
            raw.extend(os.read(master, 65536))
        assert bytes(raw).replace(b'\r\n', b'\n') == expected + b'\n', repr(raw)
        assert termios.tcgetattr(slave) == before, 'terminal state changed'
        if case == 'yaml-oversize':
            assert config.stat().st_size == 8388609
        elif case == 'yaml-fifo':
            assert config.is_fifo()
        else:
            assert config.read_text() == valid
            assert (home / 'staffdeck/project-bindings.json').is_fifo()
    except BaseException as error:
        failure = repr(error)
    finally:
        if child.poll() is None:
            child.kill()
        child.wait(timeout=3)
        os.close(master)
        os.close(slave)
        (root / 'terminal.bin').write_bytes(raw)
        results.append({'case': case, 'failure': failure, 'exit': child.returncode,
                        'elapsed': time.monotonic() - started})
result = {'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(), 'cases': results}
(output / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
print(json.dumps(result))
raise SystemExit(0 if all(row['failure'] is None for row in results) else 1)
