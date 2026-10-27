#!/usr/bin/env python3
"""Bounded exp003 orchestration. Own PID handles only; exactly three ports."""
import argparse
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import time
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / '_tmp/20261014-exp003'
SCRIPT = ROOT / 'scripts/exp003'
TOKEN = 'exp003-isolated-test-token'
# Admission tuple: the baseline products every exp003 stage needs on disk.
# test_regressions.py references this constant instead of duplicating it (#245).
REQUIRED_BASELINE_FILES = ('dist/src/cli/pilotdeck.js', 'dist/src/gateway/client/GatewayWsClient.js',
                           'node_modules/tsx/dist/loader.mjs', 'ui/server/index.js')
PROCESSES = []
RESULTS = {}
LISTENERS = []
CLEANUP_ERRORS = []
STOPPED = set()


def save(name, value):
    (OUT / name).write_text(json.dumps(value, indent=2, ensure_ascii=False) + '\n')


def start(name, argv, env, port=None, cwd=ROOT):
    log = (OUT / f'{name}.log').open('w')
    try:
        proc = subprocess.Popen(argv, cwd=cwd, env=env, stdout=log, stderr=subprocess.STDOUT,
                                start_new_session=True)
    except Exception:
        log.close()
        raise
    entry = {'name': name, 'pid': proc.pid, 'processGroup': proc.pid, 'port': port,
             'argv': [str(x) for x in argv]}
    PROCESSES.append((proc, log, entry))
    save('processes.json', [item[2] for item in PROCESSES])
    return proc


def stop(proc):
    if proc.pid in STOPPED:
        return
    # Every group was created by start(); never discover or kill a group by port.
    def signal_group(sig):
        try:
            os.killpg(proc.pid, sig)
        except ProcessLookupError:
            pass

    try:
        signal_group(signal.SIGTERM)
        try:
            proc.wait(timeout=8)
        except subprocess.TimeoutExpired:
            pass
        # Also reap descendants that survived TERM even if the direct child exited.
        signal_group(signal.SIGKILL)
        if proc.poll() is None:
            proc.wait(timeout=5)
    except (OSError, subprocess.TimeoutExpired) as error:
        CLEANUP_ERRORS.append({'pid': proc.pid, 'error': str(error)})
        print(f'cleanup warning for owned group {proc.pid}: {error}', flush=True)
    else:
        STOPPED.add(proc.pid)


def assert_loopback(port, proc):
    if port not in (3301, 28789, 28790):
        raise RuntimeError(f'port {port} is outside exp003')
    result = subprocess.run(['lsof', '-nP', '-a', '-p', str(proc.pid), '-iTCP',
                             '-sTCP:LISTEN', '-Fn'], capture_output=True, text=True, timeout=5)
    addresses = [line[1:] for line in result.stdout.splitlines() if line.startswith('n')]
    LISTENERS.append({'pid': proc.pid, 'port': port, 'addresses': addresses,
                      'loopbackOnly': result.returncode == 0 and addresses == [f'127.0.0.1:{port}']})
    save('listeners.json', LISTENERS)
    if not LISTENERS[-1]['loopbackOnly']:
        raise RuntimeError(f'owned process {proc.pid} must listen only on 127.0.0.1:{port}: {addresses}')


def wait_http(port, proc):
    deadline = time.monotonic() + 45
    while time.monotonic() < deadline:
        if proc.poll() is not None:
            raise RuntimeError(f'process {proc.pid} exited {proc.returncode} before port {port}')
        try:
            with urllib.request.urlopen(f'http://127.0.0.1:{port}/health', timeout=1) as response:
                if response.status == 200:
                    assert_loopback(port, proc)
                    return
        except (OSError, urllib.error.URLError):
            time.sleep(.1)
    raise RuntimeError(f'port {port} not ready')


def environment(home, label):
    home.mkdir(parents=True, exist_ok=False)
    registration = home / 'projects/exp003-fixture'
    registration.mkdir(parents=True)
    (registration / '.cwd').write_text(str(OUT / 'project') + '\n')
    token_path = home / 'server-token'
    token_path.write_text(TOKEN + '\n')  # MUST exist before either TS process.
    token_path.chmod(0o600)
    (home / 'pilotdeck.yaml').write_text('''schemaVersion: 1
agent:
  model: parity/deterministic
  maxContextTokens: 65536
  maxOutputTokens: 8192
model:
  providers:
    parity:
      protocol: openai
      url: http://127.0.0.1:28790/exp003/v1
      apiKey: exp003-mock-only
      models:
        deterministic:
          capabilities:
            supportsToolUse: true
            maxContextTokens: 65536
            maxOutputTokens: 8192
telemetry:
  enabled: false
''' + f'''webui:
  runtime:
    host: 127.0.0.1
    serverPort: 3301
    databasePath: {json.dumps(str(home / 'auth.db'))}
    workspacesRoot: {json.dumps(str(OUT / 'workspaces'))}
''')
    env = dict(os.environ)
    # Prevent inherited runtime/token/sidecar configuration from changing the experiment.
    for key in list(env):
        if key.startswith(('PILOTDECK_', 'EXP003_')) or key in ('PILOT_HOME', 'DATABASE_PATH', 'NODE_OPTIONS'):
            env.pop(key)
    env.update(PILOT_HOME=str(home), DATABASE_PATH=str(home / 'auth.db'),
               TMPDIR=str(OUT / 'tmp'), WORKSPACES_ROOT=str(OUT / 'workspaces'), PILOTDECK_GATEWAY_PORT='28790',
               PILOTDECK_GATEWAY_HOST='127.0.0.1', PILOTDECK_GATEWAY_TOKEN=TOKEN,
               PILOTDECK_GATEWAY_TOKEN_PATH=str(token_path),
               PILOTDECK_GATEWAY_URL='ws://127.0.0.1:28789/ws', SERVER_PORT='3301', HOST='127.0.0.1',
               DISABLE_LOCAL_AUTH='1', PILOTDECK_DISABLE_LOCAL_AUTH='1',
               EXP003_POLL_WATCH='1',
               PILOTDECK_SKIP_BROWSER_OPEN='1', EXP003_MODEL_LOG=str(OUT / f'{label}-model.jsonl'))
    return env


def driver(mode, env, baseline, project, label=None):
    label = label or mode
    child_env = dict(env, EXP003_FRAMES=str(OUT / f'{label}-frames.jsonl'))
    return start(f'driver-{label}', ['node', SCRIPT / 'driver.mjs', mode,
                 OUT / f'{label}.json', baseline, project], child_env)


def check_driver(proc, timeout=60):
    try:
        code = proc.wait(timeout=timeout)
    except subprocess.TimeoutExpired:
        stop(proc)
        raise RuntimeError('driver timeout') from None
    if code:
        raise RuntimeError(f'driver exited {code}; see its log')


def protocol_probe(env, baseline, project, label, url):
    check_driver(driver('reconnect-protocol', dict(env, EXP003_GATEWAY_URL=url), baseline, project, label))
    evidence = json.loads((OUT / f'{label}.json').read_text())
    if evidence.get('status') == 'PASS-with-note':
        return {'status': 'PASS-with-note', 'note': evidence['limitation']}


def scenario(name, callback):
    try:
        result = callback()
        RESULTS[name] = result if isinstance(result, dict) else {'status': 'PASS'}
    except Exception as error:
        RESULTS[name] = {'status': 'FAIL', 'error': str(error)}
    save('scenarios.json', RESULTS)
    print(f'{name}: {RESULTS[name]}', flush=True)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--baseline', type=Path, help='baseline checkout (default: PDRS_BASELINE or this checkout resources/PilotDeck)')
    args = parser.parse_args()
    baseline = Path(args.baseline or os.environ.get('PDRS_BASELINE') or ROOT / 'resources/PilotDeck').resolve()
    # Gateway and driver use dist; ui/server uses the tsx loader.
    missing = [name for name in REQUIRED_BASELINE_FILES if not (baseline / name).is_file()]
    if missing:
        parser.error(f'baseline at {baseline} lacks {missing}; pass --baseline or set PDRS_BASELINE')
    OUT.mkdir(parents=True, exist_ok=True)
    # Refuse to overwrite a previous run or use occupied ports. No port-based killing.
    if (OUT / 'processes.json').exists():
        raise RuntimeError('existing evidence: archive this run under OUT before rerunning')
    for port in (3301, 28789, 28790):
        with socket.socket() as sock:
            # Match service binds: old TCP TIME_WAIT is not a live listener.
            sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            sock.bind(('127.0.0.1', port))
    (OUT / 'tmp').mkdir(exist_ok=True)
    project = OUT / 'project'
    project.mkdir(exist_ok=True)
    (project / 'note.txt').write_text('exp003 project fixture\n')
    preload = SCRIPT / 'preload.mjs'
    binary = ROOT / 'src/target/debug/pilotdeck-gateway'

    def gateway(env, label):
        proc = start(f'gateway-{label}', ['node', '--import', preload,
                     baseline / 'dist/src/cli/pilotdeck.js', 'server'], dict(env, EXP003_MOCK='1'), 28790, project)
        wait_http(28790, proc)
        return proc

    def proxy(env, label):
        proc = start(f'proxy-{label}', [binary], dict(env, PILOTDECK_PROXY_LISTEN='127.0.0.1:28789',
                     PILOTDECK_PROXY_UPSTREAM='http://127.0.0.1:28790'), 28789)
        wait_http(28789, proc)
        return proc

    try:
        env = environment(OUT / 'home', 'main')
        ts = gateway(env, 'main')
        rs = proxy(env, 'main')
        ui = start('ui', ['node', '--import', preload, '--import',
                   baseline / 'node_modules/tsx/dist/loader.mjs', baseline / 'ui/server/index.js'],
                   dict(env, EXP003_FRAMES=str(OUT / 'bridge-frames.jsonl')), 3301, project)
        wait_http(3301, ui)

        scenario('S2-S3-ui', lambda: check_driver(driver('ui', env, baseline, project)))

        def reconnect():
            nonlocal rs
            ready = OUT / 'reconnect-ready.json'
            proc = driver('reconnect', dict(env, EXP003_RECONNECT_READY=str(ready)), baseline, project)
            deadline = time.monotonic() + 30
            while not ready.exists():
                if proc.poll() is not None or time.monotonic() > deadline:
                    raise RuntimeError('pending interaction not reached')
                time.sleep(.1)
            old_pid = rs.pid
            killed_at = time.monotonic()
            stop(rs)
            # Exceed the real cache's 1s first backoff; do not alter the client delays.
            time.sleep(1.6)
            rs = proxy(env, 'restarted')
            ready_seconds = time.monotonic() - killed_at
            try:
                check_driver(proc, 55)
            finally:
                frames = [json.loads(line) for line in (OUT / 'bridge-frames.jsonl').read_text().splitlines()]
                methods = [item['frame'].get('method') for item in frames if isinstance(item.get('frame'), dict)]
                save('reconnect-process.json', {'killedPid': old_pid, 'replacementPid': rs.pid,
                     'downSecondsUntilReplacementReady': ready_seconds,
                     'downSecondsUntilDriverDone': time.monotonic() - killed_at, 'observedMethods': methods})
            assert 'reconnect_interaction' in methods and 'active_turn_snapshot' in methods

        scenario('S4-reconnect', reconnect)
        for label, url in [('protocol-direct', 'ws://127.0.0.1:28790/ws'), ('protocol-proxy', 'ws://127.0.0.1:28789/ws')]:
            scenario(label, lambda label=label, url=url: protocol_probe(env, baseline, project, label, url))
        scenario('S5-http', lambda: check_driver(driver('http', env, baseline, project)))
        scenario('close-codes', lambda: check_driver(driver('close', env, baseline, project)))
        stop(ui)
        stop(ts)
        stop(rs)

        def equivalent():
            for label, url in [('direct', 'ws://127.0.0.1:28790/ws'), ('proxy', 'ws://127.0.0.1:28789/ws')]:
                round_env = environment(OUT / f'home-{label}', label)
                round_ts = gateway(round_env, label)
                round_rs = proxy(round_env, label) if label == 'proxy' else None
                try:
                    check_driver(driver('equivalence', dict(round_env, EXP003_GATEWAY_URL=url), baseline, project, label))
                finally:
                    if round_rs:
                        stop(round_rs)
                    stop(round_ts)
            proc = start('compare', ['python3', SCRIPT / 'compare.py', OUT / 'direct-frames.jsonl',
                         OUT / 'proxy-frames.jsonl', '--out', OUT / 'comparison.json'], os.environ)
            check_driver(proc)

        scenario('S6-equivalence', equivalent)

        def positive():
            rows = [json.loads(line) for line in (OUT / 'proxy-frames.jsonl').read_text().splitlines()]
            changed = next(row for row in rows if row.get('frame', {}).get('type') == 'hello_ok')
            changed['frame']['serverVersion'] = 'INJECTED_KNOWN_DIFFERENCE'
            changed['raw'] = json.dumps(changed['frame'], separators=(',', ':'))
            injection = OUT / 'positive-frames.jsonl'
            injection.write_text(''.join(json.dumps(row) + '\n' for row in rows))
            proc = start('positive', ['python3', SCRIPT / 'compare.py', OUT / 'direct-frames.jsonl', injection,
                         '--out', OUT / 'positive.json'], os.environ)
            assert proc.wait(timeout=10) == 1, 'injected difference must return FAIL/exit 1'
            assert json.loads((OUT / 'positive.json').read_text())['status'] == 'FAIL'

        scenario('S7-positive', positive)
    finally:
        for proc, log, entry in reversed(PROCESSES):
            stop(proc)
            entry['exitCode'] = proc.returncode
            entry['stopped'] = proc.poll() is not None
            log.close()
        save('processes.json', [item[2] for item in PROCESSES])
        ports = {}
        for port in (3301, 28789, 28790):
            try:
                with socket.socket() as sock:
                    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
                    sock.bind(('127.0.0.1', port))
                ports[str(port)] = 'released (SO_REUSEADDR bind succeeded, socket then closed)'
            except OSError as error:
                ports[str(port)] = str(error)
        owned_groups = {proc.pid for proc, _, _ in PROCESSES}
        remaining_groups = []
        # Signal 0 checks existence without sending a signal. No ps access or
        # global process discovery is needed in the managed sandbox.
        for group in sorted(owned_groups):
            try:
                os.killpg(group, 0)
                remaining_groups.append(group)
            except ProcessLookupError:
                pass
            except OSError as error:
                CLEANUP_ERRORS.append({'processGroup': group, 'error': f'group audit: {error}'})
        audit = {'allRecordedProcessesStopped': all(item[0].poll() is not None for item in PROCESSES),
             'cleanupErrors': CLEANUP_ERRORS, 'ports': ports,
             'ownedProcessGroupsRemaining': remaining_groups,
             'allOwnedProcessGroupsGone': not remaining_groups and not CLEANUP_ERRORS,
             'allListenersLoopback': bool(LISTENERS) and all(item['loopbackOnly'] for item in LISTENERS)}
        save('audit.json', audit)
    clean = (audit['allRecordedProcessesStopped'] and audit['allListenersLoopback']
             and audit['allOwnedProcessGroupsGone']
             and all(value.startswith('released') for value in ports.values()))
    return 0 if clean and RESULTS and all(item['status'] in ('PASS', 'PASS-with-note') for item in RESULTS.values()) else 1


if __name__ == '__main__':
    raise SystemExit(main())
