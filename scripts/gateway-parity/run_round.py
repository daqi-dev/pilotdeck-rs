#!/usr/bin/env python3
"""Run TS direct or TS through A1, always at exactly the same runtime paths.

Each finished cur directory is RENAMED to label after all owned groups stop.
Call run_round under parity_lock for multi-round calibration.
"""
import argparse
import json
import os
from pathlib import Path
import re
import signal
import socket
import subprocess
import sys
import time
import urllib.request

from common import DEFAULT_OUT, READ_METHODS, ROOT, SCRIPT, SCENARIOS, apparatus_hashes, digest, install_shutdown, parity_lock, save

TOKEN = 'gateway-parity-isolated-token'


def run_round(side, label, output=DEFAULT_OUT, baseline=None, ports=(28791, 28792, 28793, 28794), timeout=180):
    baseline = Path(baseline or os.environ.get('PDRS_BASELINE', '/Users/da/ws/pilotdeck-rs/resources/PilotDeck')).resolve()
    output = Path(output).resolve()
    if not output.is_relative_to(ROOT / '_tmp'):
        raise ValueError('runtime/evidence output must be inside this workspace _tmp')
    if not re.fullmatch(r'[a-zA-Z0-9][a-zA-Z0-9_-]*', label) or label == 'cur':
        raise ValueError('invalid archive label')
    if side not in ('ts', 'rust') or len(set(ports)) != 4 or not all(28000 <= p <= 28999 for p in ports):
        raise ValueError('side and four distinct 28xxx ports required')
    cur = output / 'cur'
    archive = output / label
    if cur.exists() or archive.exists():
        raise FileExistsError('refusing to overwrite runtime or evidence; choose another output/label')
    if not 0 < timeout <= 900:
        raise ValueError('round timeout must be <=900s')
    for port in ports:
        with socket.socket() as sock:
            sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            sock.bind(('127.0.0.1', port))
    cur.mkdir(parents=True)
    home, project = cur / 'home', cur / 'project'
    for path in (home, project, cur / 'tmp', cur / 'workspaces', cur / 'scenarios'):
        path.mkdir()
    registration = home / 'projects/gateway-parity-fixture'
    registration.mkdir(parents=True)
    (registration / '.cwd').write_text(str(project) + '\n')
    (project / 'note.txt').write_text('gateway parity fixture\n')
    save(home / 'permissions.json', {'version': 1, 'allowedTools': [], 'askTools': ['bash'],
                                    'disallowedTools': [], 'skipPermissions': False})
    token_path = home / 'server-token'
    token_path.write_text(TOKEN + '\n')
    token_path.chmod(0o600)
    gateway_port, proxy_port, observer_port, mock_port = ports
    (home / 'pilotdeck.yaml').write_text(f'''schemaVersion: 1
agent:
  model: parity/deterministic
  maxContextTokens: 65536
  maxOutputTokens: 8192
model:
  providers:
    parity:
      protocol: openai
      url: http://127.0.0.1:{mock_port}/v1
      apiKey: gateway-parity-mock-only
      models:
        deterministic:
          capabilities:
            supportsToolUse: true
            maxContextTokens: 65536
            maxOutputTokens: 8192
telemetry:
  enabled: false
cron:
  enabled: true
  timezone: UTC
webui:
  runtime:
    host: 127.0.0.1
    serverPort: {proxy_port}
    databasePath: {json.dumps(str(home / 'auth.db'))}
    workspacesRoot: {json.dumps(str(cur / 'workspaces'))}
''')
    env = {key: val for key, val in os.environ.items() if not key.startswith(('PILOTDECK_', 'EXP003_', 'GP_'))
           and key not in ('PILOT_HOME', 'DATABASE_PATH', 'NODE_OPTIONS', 'WORKSPACES_ROOT')}
    env.update(PDRS_BASELINE=str(baseline), PILOT_HOME=str(home), GP_PROJECT=str(project),
               DATABASE_PATH=str(home / 'auth.db'), TMPDIR=str(cur / 'tmp'), WORKSPACES_ROOT=str(cur / 'workspaces'),
               PILOTDECK_GATEWAY_PORT=str(gateway_port), PILOTDECK_GATEWAY_HOST='127.0.0.1',
               PILOTDECK_GATEWAY_TOKEN=TOKEN, PILOTDECK_GATEWAY_TOKEN_PATH=str(token_path),
               GP_POLL_WATCH='1', PILOTDECK_SKIP_BROWSER_OPEN='1', GP_MOCK_PORT=str(mock_port),
               GP_MOCK_URL=f'http://127.0.0.1:{mock_port}', GP_MODEL_LOG=str(cur / 'model.jsonl'))
    processes = []
    listeners = []
    errors = []
    start_time = time.monotonic()
    run = {'side': side, 'label': label, 'status': 'RUNNING', 'apparatusSha256': apparatus_hashes(), 'scenarios': {},
           'runtime': {'pilotHome': str(home), 'cwd': str(project), 'tmpdir': str(cur / 'tmp')},
           'ports': list(ports), 'baseline': str(baseline), 'pollWatcher': True,
           'baselineSourceSha256': digest(baseline / 'src/gateway/server/GatewayServer.ts'),
           'baselineDistSha256': digest(baseline / 'dist/src/cli/pilotdeck.js')}

    def start(name, argv, port=None, cwd=project, child_env=env):
        log = (cur / f'{name}.log').open('w')
        try:
            proc = subprocess.Popen([str(x) for x in argv], cwd=cwd, env=child_env, stdout=log,
                                    stderr=subprocess.STDOUT, start_new_session=True)
        except BaseException:
            log.close()
            raise
        entry = {'name': name, 'pid': proc.pid, 'processGroup': proc.pid, 'port': port, 'argv': [str(x) for x in argv]}
        processes.append((proc, log, entry))
        save(cur / 'processes.json', [row[2] for row in processes])
        return proc

    def stop(proc):
        for sig in (signal.SIGTERM, signal.SIGKILL):
            try:
                os.killpg(proc.pid, sig)
            except ProcessLookupError:
                pass
            except OSError as error:
                errors.append(str(error))
            try:
                proc.wait(timeout=3)
            except subprocess.TimeoutExpired:
                pass

    def ready(port, proc):
        deadline = min(start_time + timeout, time.monotonic() + 45)
        while time.monotonic() < deadline:
            if proc.poll() is not None:
                raise RuntimeError(f'{port}: owned process exited {proc.returncode}')
            try:
                with urllib.request.urlopen(f'http://127.0.0.1:{port}/health', timeout=.5) as response:
                    if response.status == 200:
                        observed = subprocess.run(['lsof', '-nP', '-a', '-p', str(proc.pid), '-iTCP', '-sTCP:LISTEN', '-Fn'],
                                                  capture_output=True, text=True, timeout=5)
                        addresses = [line[1:] for line in observed.stdout.splitlines() if line.startswith('n')]
                        listeners.append({'pid': proc.pid, 'port': port, 'addresses': addresses})
                        if observed.returncode or addresses != [f'127.0.0.1:{port}']:
                            raise RuntimeError(f'non-loopback/extra listener: {addresses}')
                        return
            except (OSError, urllib.error.URLError):
                time.sleep(.1)
        raise TimeoutError(f'port {port} readiness timeout')

    try:
        mock = start('mock', ['node', SCRIPT / 'mock.mjs'], mock_port)
        ready(mock_port, mock)
        ts = start('gateway', ['node', '--import', SCRIPT / 'watcher.mjs', baseline / 'dist/src/cli/pilotdeck.js', 'server'], gateway_port)
        ready(gateway_port, ts)
        observer = start('observer', [sys.executable, SCRIPT / 'observer.py', '--port', observer_port,
                                    '--upstream', gateway_port, '--out', cur / 'upstream.jsonl'], observer_port)
        ready(observer_port, observer)
        # Direct means official driver -> TS. Observer only serves Rust's upstream.
        entry_port = gateway_port
        if side == 'rust':
            binary = ROOT / 'src/target/debug/pilotdeck-gateway'
            run['proxySha256'] = digest(binary)
            rs_env = dict(env, PILOTDECK_PROXY_LISTEN=f'127.0.0.1:{proxy_port}',
                          PILOTDECK_PROXY_UPSTREAM=f'http://127.0.0.1:{observer_port}')
            proxy = start('proxy', [binary], proxy_port, child_env=rs_env)
            ready(proxy_port, proxy)
            entry_port = proxy_port
        for scenario in SCENARIOS:
            # A successful archive read, not just a missing-artifact error.
            if scenario == 'readonly':
                transcripts = list(home.rglob('*gateway-parity-text*.jsonl'))
                if len(transcripts) != 1:
                    raise RuntimeError(f'expected one text transcript, found {transcripts}')
                artifacts = project / '.pilotdeck/tool-results' / transcripts[0].stem
                artifacts.mkdir(parents=True, exist_ok=True)
                (artifacts / 'fixture.txt').write_text('gateway parity fixture artifact\n')
            driver = start(f'driver-{scenario}', ['node', '--import', baseline / 'node_modules/tsx/dist/loader.mjs',
                          SCRIPT / 'driver.mts', '--url', f'ws://127.0.0.1:{entry_port}/ws', '--token', TOKEN,
                          '--scenario', scenario, '--out', cur / 'scenarios' / scenario])
            try:
                code = driver.wait(timeout=max(.1, min(45, start_time + timeout - time.monotonic())))
            except subprocess.TimeoutExpired:
                stop(driver)
                code = -1
            run['scenarios'][scenario] = {'exitCode': code}
            save(cur / 'run.json', run)
            if time.monotonic() >= start_time + timeout:
                raise TimeoutError('round wall-clock timeout')
        run['status'] = 'DONE'
    except Exception as error:
        run['status'] = 'ERROR'
        run['error'] = str(error)
    finally:
        for proc, log, entry in reversed(processes):
            stop(proc)
            entry['exitCode'] = proc.returncode
            entry['stopped'] = proc.poll() is not None
            log.close()
        remaining = []
        for proc, _, _ in processes:
            try:
                os.killpg(proc.pid, 0)
                remaining.append(proc.pid)
            except ProcessLookupError:
                pass
            except OSError as error:
                errors.append(str(error))
        run['audit'] = {'ownedGroupsRemaining': remaining, 'cleanupErrors': errors,
                        'listeners': listeners, 'allStopped': not remaining and not errors}
        run['elapsedSeconds'] = time.monotonic() - start_time
        save(cur / 'processes.json', [row[2] for row in processes])
        # Raw full disk tree survives in home/project. Comparator reads it again,
        # so controls that alter actual bytes cannot evade a cached projection.
        save(cur / 'run.json', run)
        cur.rename(archive)
    print(json.dumps({'side': side, 'label': label, 'status': run['status'], 'scenarios': run['scenarios'], 'error': run.get('error')}, ensure_ascii=False), flush=True)
    return archive


def main():
    install_shutdown()
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--side', choices=['ts', 'rust'], required=True)
    ap.add_argument('--label', required=True)
    ap.add_argument('--output', type=Path, default=DEFAULT_OUT)
    ap.add_argument('--baseline', type=Path)
    ap.add_argument('--ports', type=int, nargs=4, default=[28791, 28792, 28793, 28794])
    ap.add_argument('--timeout', type=float, default=180)
    ap.add_argument('--lock-timeout', type=float, default=600)
    ap.add_argument('--local-method', choices=READ_METHODS, action='append', default=[],
                    help='A4: validate these successful readonly responses as local, with no upstream request')
    args = ap.parse_args()
    if args.local_method and args.side != 'rust':
        ap.error('--local-method requires --side rust')
    takeovers = []
    with parity_lock(args.lock_timeout, takeovers):
        path = run_round(args.side, args.label, args.output, args.baseline, args.ports, args.timeout)
        save(path / 'lock.json', {'takeovers': takeovers})
    from compare import validate
    return 1 if validate(path, args.local_method)[1] else 0


if __name__ == '__main__':
    try:
        code = main()
    except Exception as error:
        print(json.dumps({'status': 'BLOCKED', 'error': str(error)}, ensure_ascii=False))
        code = 1
    raise SystemExit(code)
