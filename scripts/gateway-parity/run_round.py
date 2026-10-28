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
import tempfile
import time
import urllib.request

# >>> apparatus bootstrap (verbatim in every A2 entry; test_safety checks it)
# Load apparatus modules by explicit path, never sys.path: a same-named package
# or extension module beside the source would win before the scan (fix-r6 P1).
# Inline on purpose: importing common.py is what this block defends.  -B only
# stops cache writes, so redirect the bytecode prefix too (fix-r5).
import importlib.util, sys, tempfile
from pathlib import Path

# -I keeps PYTHONPATH, user site and the script directory out of module
# resolution.  Without it, exit before loading anything and print the command.
if not sys.flags.isolated:
    raise SystemExit(f'run with -I: python3 -I {Path(__file__).resolve()}')

sys.pycache_prefix = tempfile.mkdtemp(prefix='pdrs-gateway-pyc-')
sys.dont_write_bytecode = True
# CPython has already compiled the CLI entry before it can acquire a lease.
# Capture that executing object, not a later read of the restored entry file.
_apparatus_code = sys._getframe().f_code


def _load(name, path):
    path = Path(path).resolve()
    existing = sys.modules.get(name)
    existing_file = getattr(existing, '__file__', None)
    if existing is not None and existing_file and Path(existing_file).resolve() == path:
        if not isinstance(getattr(existing, '_apparatus_source', None), bytes):
            raise RuntimeError(f'#166 unverified apparatus module cache: {path}')
        return existing
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    try:
        source = path.read_bytes()
        exec(compile(source, str(path), 'exec'), module.__dict__)
    except BaseException:
        sys.modules.pop(name, None)
        raise
    module._apparatus_source = source
    if Path(module.__file__).resolve() != path:
        raise RuntimeError(f'{name}: loaded {module.__file__}, expected {path}')
    return module



def _verify_cli_imports():
    # This trust root uses only stdlib + Git, never a helper from disk common:
    # that module's verifier can itself be the active negative-control mutant.
    import subprocess
    from types import ModuleType
    root = _apparatus.parent.parent
    head = subprocess.check_output(['git', '-C', str(root), 'rev-parse', '--verify', 'HEAD'],
                                   timeout=30).decode().strip()
    source = subprocess.check_output(['git', '-C', str(root), 'show',
                                      f'{head}:scripts/gateway-parity/common.py'], timeout=30)
    trusted = ModuleType('pristine_cli_verifier')
    trusted.__file__ = str(_apparatus / 'common.py')
    exec(compile(source, trusted.__file__, 'exec'), trusted.__dict__)
    trusted.verify_loaded_apparatus(head)


_here = Path(__file__).resolve().parent
_apparatus = _here.parent if _here.name == 'controls' else _here
common = _load('common', _apparatus / 'common.py')
# <<< apparatus bootstrap

DEFAULT_OUT = common.DEFAULT_OUT
READ_METHODS = common.READ_METHODS
ROOT = common.ROOT
SCRIPT = common.SCRIPT
SCENARIOS = common.SCENARIOS
apparatus_hashes = common.apparatus_hashes
baseline_dependency_hashes = common.baseline_dependency_hashes
cargo_config_problems = common.cargo_config_problems
digest = common.digest
install_shutdown = common.install_shutdown
parity_lock = common.verified_parity_lock
save = common.save
compare_module = _load('compare', common.SCRIPT / 'compare.py')

TOKEN = 'gateway-parity-isolated-token'


def start_owned(processes, log, entry, argv, **kwargs):
    # Defer delivery until both in-memory ownership records exist. The child
    # must undo the inherited mask before exec, so TERM still works normally.
    with common.deferred_signals():
        try:
            proc = subprocess.Popen(argv, start_new_session=True,
                                    preexec_fn=common.unblock_child_signals, **kwargs)
        except BaseException:
            log.close()
            raise
        common.LIVE_GROUPS.add(proc.pid)
        entry.update(pid=proc.pid, processGroup=proc.pid)
        processes.append((proc, log, entry))
    return proc


def wait_owned(proc, timeout):
    """Observe completion without reaping: reserve the PID through cleanup.

    Popen.wait/poll would release the leader PID before the last killpg. WNOWAIT
    keeps it ours even after exit; lack of this capability fails closed.
    """
    if not all(hasattr(os, name) for name in ('waitid', 'WNOWAIT', 'WEXITED', 'WNOHANG')):
        raise OSError('WNOWAIT is required to retain process-group ownership')
    deadline = time.monotonic() + timeout
    while True:
        result = os.waitid(os.P_PID, proc.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT)
        if result is not None:
            return result.si_status if result.si_code == os.CLD_EXITED else -result.si_status
        if time.monotonic() >= deadline:
            raise subprocess.TimeoutExpired(str(proc.pid), timeout)
        time.sleep(.02)


def stop_owned(proc):
    """Attempt every cleanup step; failure keeps ownership and the shared lock."""
    if proc.pid not in common.LIVE_GROUPS:
        common.LIVE_GROUPS.add(proc.pid)
        return {'allStopped': False, 'remaining': True, 'errors': ['process missing ownership registration']}
    errors, zombie_denials = [], []
    with common.deferred_signals():
        for sig in (signal.SIGTERM, signal.SIGKILL):
            leader_exited = False
            try:
                wait_owned(proc, 0)  # exited but unreaped is still held
                leader_exited = True
            except subprocess.TimeoutExpired:
                pass  # running direct child is also held
            except OSError as error:
                errors.append(str(error))
                break  # identity lost: never signal a reusable bare PGID
            try:
                os.killpg(proc.pid, sig)
            except ProcessLookupError:
                pass
            except PermissionError as error:
                # Darwin denies signals to an unreaped zombie-only group.
                # EPERM itself proves nothing: defer only this known-held,
                # already-exited case until reap + final ESRCH can prove absence.
                (zombie_denials if leader_exited else errors).append(str(error))
            except OSError as error:
                errors.append(str(error))
            try:
                wait_owned(proc, 3)
            except subprocess.TimeoutExpired:
                pass
            except OSError as error:
                errors.append(str(error))
        try:
            wait_owned(proc, 0)
            proc.wait(timeout=3)  # no more signals after reaping the held leader
        except (OSError, subprocess.TimeoutExpired) as error:
            errors.append(str(error))
        deadline = time.monotonic() + 2
        while True:
            try:
                os.killpg(proc.pid, 0)
            except ProcessLookupError:
                remaining = False
                break
            except OSError as error:
                errors.append(str(error))
                remaining = True
                break
            if time.monotonic() >= deadline:
                remaining = True
                break
            time.sleep(.02)
        if remaining or proc.returncode is None:
            errors.extend(zombie_denials)
        stopped = not remaining and not errors and proc.returncode is not None
        if stopped:
            common.LIVE_GROUPS.discard(proc.pid)
    return {'allStopped': stopped, 'remaining': remaining, 'errors': errors,
            'unreapedLeaderSignalDenials': zombie_denials}


def cleanup_owned(processes):
    errors, remaining = [], []
    # One deferral around the whole batch: a pending signal must not interrupt
    # between two owned groups or between cleanup and its audit publication.
    with common.deferred_signals():
        for proc, log, entry in reversed(processes):
            try:
                audit = stop_owned(proc)
                errors.extend(audit['errors'])
                if not audit['allStopped']:
                    remaining.append(proc.pid)
                entry['exitCode'] = proc.returncode
                entry['stopped'] = audit['allStopped']
                entry['cleanupAudit'] = audit
            except OSError as error:
                errors.append(str(error))
                remaining.append(proc.pid)
            finally:
                try:
                    log.close()
                except OSError as error:
                    errors.append(str(error))
        if errors:
            # Even an unexpected cleanup/log failure is fail-closed at release.
            common.LIVE_GROUPS.update(proc.pid for proc, _, _ in processes)
        remaining = sorted(set(remaining) | common.LIVE_GROUPS)
        return {'ownedGroupsRemaining': remaining, 'cleanupErrors': errors,
                'allStopped': not remaining and not errors}


def run_round(side, label, output=DEFAULT_OUT, baseline=None, ports=(28791, 28792, 28793, 28794), timeout=180):
    # 显式参数 > PDRS_BASELINE > 本 checkout 的 resources/PilotDeck；不再回落到主仓绝对路径（#187）
    baseline = Path(baseline or os.environ.get('PDRS_BASELINE') or ROOT / 'resources/PilotDeck').resolve()
    if not (baseline / 'dist/src/cli/pilotdeck.js').is_file():
        raise ValueError(f'baseline not found at {baseline}; pass --baseline or set PDRS_BASELINE')
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
    if side == 'rust':
        # A Rust binary can only be trusted when cargo saw no config outside the
        # reviewed repository. Checked at run time as well as by build_proxy.py.
        problems = cargo_config_problems()
        if problems:
            raise ValueError('BLOCKED: cargo build inputs are not frozen: ' + '; '.join(problems))
    dependencies = baseline_dependency_hashes(baseline)
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
    start_time = time.monotonic()
    run = {'side': side, 'label': label, 'status': 'RUNNING', 'apparatusSha256': apparatus_hashes(), 'scenarios': {},
           'runtime': {'pilotHome': str(home), 'cwd': str(project), 'tmpdir': str(cur / 'tmp')},
           'ports': list(ports), 'baseline': str(baseline), 'pollWatcher': True,
           'baselineDepsSha256': dependencies,
           'baselineSourceSha256': digest(baseline / 'src/gateway/server/GatewayServer.ts'),
           'baselineDistSha256': digest(baseline / 'dist/src/cli/pilotdeck.js')}

    def start(name, argv, port=None, cwd=project, child_env=env):
        log = (cur / f'{name}.log').open('w')
        entry = {'name': name, 'port': port, 'argv': [str(x) for x in argv]}
        proc = start_owned(processes, log, entry, entry['argv'], cwd=cwd,
                           env=child_env, stdout=log, stderr=subprocess.STDOUT)
        save(cur / 'processes.json', [row[2] for row in processes])
        return proc

    def ready(port, proc):
        deadline = min(start_time + timeout, time.monotonic() + 45)
        while time.monotonic() < deadline:
            try:
                code = wait_owned(proc, 0)
            except subprocess.TimeoutExpired:
                pass
            else:
                raise RuntimeError(f'{port}: owned process exited {code}')
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
        observer = start('observer', [sys.executable, '-I', SCRIPT / 'observer.py', '--port', observer_port,
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
                code = wait_owned(driver, max(.1, min(45, start_time + timeout - time.monotonic())))
            except subprocess.TimeoutExpired:
                run['scenarios'][scenario] = {'exitCode': -1}
                save(cur / 'run.json', run)
                # The failed scenario already invalidates the round. Stop it
                # once, together with every other owned group in finally.
                raise TimeoutError(f'{scenario}: driver deadline exceeded')
            run['scenarios'][scenario] = {'exitCode': code}
            save(cur / 'run.json', run)
            if time.monotonic() >= start_time + timeout:
                raise TimeoutError('round wall-clock timeout')
        if apparatus_hashes() != run['apparatusSha256']:
            raise ValueError('apparatus changed during round; report cannot certify mixed versions')
        if baseline_dependency_hashes(baseline) != dependencies:
            raise ValueError('baseline dependencies changed during round; report cannot certify mixed versions')
        run['status'] = 'DONE'
    except Exception as error:
        run['status'] = 'ERROR'
        run['error'] = str(error)
    finally:
        with common.deferred_signals():
            run['audit'] = cleanup_owned(processes)
            run['audit']['listeners'] = listeners
            if not run['audit']['allStopped']:
                run['status'] = 'ERROR'
                run['error'] = 'owned process-group cleanup unverified; shared lock retained'
            run['elapsedSeconds'] = time.monotonic() - start_time
            save(cur / 'processes.json', [row[2] for row in processes])
            save(cur / 'run.json', run)
            cur.rename(archive)
    if not run['audit']['allStopped']:
        raise RuntimeError(run['error'])
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
    return 1 if compare_module.validate(path, args.local_method)[1] else 0


if __name__ == '__main__':
    try:
        _verify_cli_imports()
    except (Exception, KeyboardInterrupt) as error:
        print(json.dumps({'status': 'BLOCKED', 'error': str(error)}, ensure_ascii=False))
        raise SystemExit(2)
    try:
        code = main()
    except Exception as error:
        print(json.dumps({'status': 'BLOCKED', 'error': str(error)}, ensure_ascii=False))
        code = 1
    raise SystemExit(code)
