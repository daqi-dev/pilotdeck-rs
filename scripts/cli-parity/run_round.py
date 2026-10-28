#!/usr/bin/env python3
"""Offline CLI calibration: two TS rounds, optionally one --rust-bin round."""
import sys

# -I keeps PYTHONPATH, user site and the script directory out of module
# resolution.  Without it, exit before importing anything: a same-named stdlib
# file beside this entry (json.py, pathlib.py, tempfile.py) would otherwise
# execute before the guard.  ``sys`` is builtin/frozen and already loaded.
if not sys.flags.isolated:
    raise SystemExit(f'run with -I: python3 -I {__file__}')

import argparse
from contextlib import contextmanager
import hashlib
import importlib.util
import json
import math
import os
from pathlib import Path
import selectors
import shutil
import signal
import socket
import stat
import subprocess
import tempfile
import time

# Load apparatus modules by explicit path, never sys.path: a same-named package
# or extension module beside the source would win before the scan (#701 item 3,
# same defect gateway-parity fix-r6 P1).  Inline on purpose: importing common.py
# is what this block defends.
sys.pycache_prefix = tempfile.mkdtemp(prefix='pdrs-cli-parity-pyc-')
sys.dont_write_bytecode = True


def _load(name, path):
    path = Path(path).resolve()
    existing = sys.modules.get(name)
    existing_file = getattr(existing, '__file__', None)
    if existing is not None and existing_file and Path(existing_file).resolve() == path:
        if not isinstance(getattr(existing, '_apparatus_source', None), bytes):
            raise RuntimeError(f'unverified apparatus module cache: {path}')
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


_here = Path(__file__).resolve().parent
_common = _load('common', _here / 'common.py')
LIVE_GROUPS = _common.LIVE_GROUPS
Blocked = _common.Blocked
CleanupBlocked = _common.CleanupBlocked
HEAD = _common.HEAD
MOCK_PORT = _common.MOCK_PORT
OPENSSL_CONFIG = _common.OPENSSL_CONFIG
ROOT = _common.ROOT
SCRIPT = _common.SCRIPT
SCENARIOS = _common.SCENARIOS
TOKEN = _common.TOKEN
baseline_path = _common.baseline_path
digest = _common.digest
environment = _common.environment
env_summary = _common.env_summary
fixture = _common.fixture
hashes = _common.hashes
mock_gateway_scenarios = _common.mock_gateway_scenarios
output_path = _common.output_path
parity_lock = _common.parity_lock
per_stream_scenarios = _common.per_stream_scenarios
save = _common.save
snapshot = _common.snapshot
staged_skills_identity = _common.staged_skills_identity
compare = _load('compare', _here / 'compare.py').compare
sandbox_profile = _load('policy', _here / 'policy.py').sandbox_profile

MAX_BYTES = 4 * 1024 * 1024


def git_head(path):
    # Explicit executable and fresh environment: no GIT_* or user/global config.
    result = subprocess.run(['/usr/bin/git', '-c', 'safe.directory=*', '-C', str(path), 'rev-parse', 'HEAD'],
                            env={'PATH': '/usr/bin:/bin', 'GIT_CONFIG_NOSYSTEM': '1', 'GIT_CONFIG_GLOBAL': '/dev/null'},
                            capture_output=True, text=True, timeout=10)
    if result.returncode:
        raise Blocked('cannot identify source HEAD: ' + result.stderr.strip())
    return result.stdout.strip()


def executable(path):
    path = Path(path).resolve()
    if not path.is_file() or not os.access(path, os.X_OK):
        raise Blocked(f'not executable: {path}')
    return {'path': str(path), 'sha256': digest(path)}


def directory_identity(folder, baseline):
    """Full installed content, including hidden pnpm entries and link targets.

    pnpm hardlinks are content-hashed, not rejected. External symlinks fail
    closed: a source outside this baseline has no registered read contract.
    """
    if not folder.is_dir():
        raise Blocked(f'baseline dependency missing: {folder}')
    manifest = {}
    def visit(path, active):
        resolved = path.resolve()
        if not resolved.is_relative_to(baseline):
            raise Blocked(f'baseline external link: {path}')
        if resolved in active:
            raise Blocked(f'baseline link cycle: {path}')
        info = path.lstat()
        row = {'mode': stat.S_IMODE(info.st_mode)}
        if path.is_symlink():
            # Target content is covered by its canonical subtree. Following
            # dependency links recursively would recurse around pnpm cycles.
            relative = resolved.relative_to(baseline)
            if not resolved.exists():
                raise Blocked(f'baseline dangling link target: {path}')
            # Existing pnpm links to ui/apps stay inventoried, but their target
            # data is not readable under policy.py (canonical vnode matching).
            row.update(type='symlink', target=os.readlink(path), resolved=relative.as_posix())
        elif path.is_dir():
            row['type'] = 'directory'
            for child in sorted(path.iterdir()):
                visit(child, active | {resolved})
        elif path.is_file():
            row.update(type='file', size=path.stat().st_size, sha256=digest(path))
        else:
            raise Blocked(f'baseline special or dangling file: {path}')
        manifest[path.relative_to(folder).as_posix()] = row
    visit(folder, set())
    return hashlib.sha256(json.dumps(manifest, sort_keys=True, separators=(',', ':')).encode()).hexdigest()


def baseline_identity(baseline):
    head = git_head(baseline)
    if head != HEAD:
        raise Blocked(f'baseline HEAD {head} does not match registered {HEAD}')
    # Match every baseline subtree the TS policy permits, including dependencies
    # and bundled skills. resources/ is not readable and is deliberately absent.
    trees = {name: directory_identity(baseline / name, baseline)
             for name in ('src', 'dist', 'packages', 'node_modules', 'skills')}
    manifest = {name: digest(baseline / name)
                for name in ('package.json', 'package-lock.json', 'pnpm-lock.yaml')
                if (baseline / name).is_file()}
    return {'path': str(baseline), 'sourceHead': head, 'entrySha256': digest(baseline / 'dist/src/cli/pilotdeck.js'),
            'files': manifest, 'trees': trees, 'trackedClean': 'NOT_ASSERTED (HEAD plus actual file hashes)'}


def rust_identity(binary, source):
    return {'side': 'rust', 'executable': executable(binary), 'sourceHead': git_head(source),
            'sourcePath': str(Path(source).resolve())}


def stage_rust(output, expected, baseline):
    """Rust release layout built per round, never committed: the --rust-bin copy at
    <output>/rust-stage/bin/pilotdeck next to a copy of the baseline skills/ tree.
    The original binary stays the identity; the copy must hash identically."""
    stage = output / 'rust-stage'
    (stage / 'bin').mkdir(parents=True, mode=0o700)
    staged = stage / 'bin' / 'pilotdeck'
    shutil.copyfile(expected['executable']['path'], staged)
    staged.chmod(0o755)
    # copytree follows a root-level link, so the scan below would not see it.
    if (baseline / 'skills').is_symlink():
        raise Blocked('baseline skills/ is a symlink; Rust stage copy not registered')
    shutil.copytree(baseline / 'skills', stage / 'skills', symlinks=True)
    # Links would point back into the baseline, which the Rust policy cannot read.
    if any(path.is_symlink() for path in (stage / 'skills').rglob('*')):
        raise Blocked('baseline skills/ contains symlinks; Rust stage copy not registered')
    identity = executable(staged)
    if identity['path'] != str(staged) or identity['sha256'] != expected['executable']['sha256']:
        raise Blocked('staged Rust binary differs from --rust-bin')
    return identity


def check_budget(timeout, with_rust):
    rounds = 3 if with_rust else 2
    # Reserve five minutes for hashing/probes/controls; a separate hard deadline
    # covers slower-than-estimated work while the shared lock is held.
    if len(SCENARIOS) > 30 or len(SCENARIOS) * timeout * rounds + 300 >= 2400:
        raise Blocked('batch exceeds 30 scenarios or shared-lock 2400s lease budget')


def lease_expired(signum, frame):
    raise KeyboardInterrupt('batch lease deadline')


def process_table():
    result = subprocess.run(['/bin/ps', '-axo', 'pid=,ppid=,pgid=,stat=,comm='],
                            capture_output=True, text=True, timeout=5,
                            env={'PATH': '/usr/bin:/bin', 'LC_ALL': 'C'})
    if result.returncode:
        raise Blocked('process tree unavailable: ' + result.stderr.strip())
    rows = []
    for line in result.stdout.splitlines():
        pid, ppid, pgid, state, command = line.strip().split(None, 4)
        rows.append({'pid': int(pid), 'ppid': int(ppid), 'pgid': int(pgid), 'state': state, 'command': command})
    if not any(row['pid'] == os.getpid() for row in rows):
        raise Blocked('process tree missing driver PID')
    return rows


DEFERRED = {signal.SIGINT, signal.SIGTERM, signal.SIGALRM}


@contextmanager
def deferred_signals():
    """挂起（非忽略）SIGINT/SIGTERM/SIGALRM：期间到达的信号排队，解除后立即投递——
    租约闹钟不会因此丢失，中断也不会落在「进程组已建未登记」或「清理一半」的窗口里。"""
    old = signal.pthread_sigmask(signal.SIG_BLOCK, DEFERRED)
    try:
        yield
    finally:
        signal.pthread_sigmask(signal.SIG_SETMASK, old)


def _unblock_in_child():
    signal.pthread_sigmask(signal.SIG_UNBLOCK, DEFERRED)


def spawn(*args, **kwargs):
    """启动进程组并登记，两步之间不可被信号打断（gate-final concurrency P1：登记窗口）。

    #933 fix-r3 P1-1：解除屏蔽的瞬间排队的信号即投递，可能先于本函数返回——
    任何异常路径都先回收自己登记的进程组再上抛，调用者永远不会接手一个
    「已登记却拿不到句柄」的孤儿进程组（回收失败则保持登记，锁保留）。"""
    proc = None
    try:
        with deferred_signals():
            # 子进程会继承信号掩码；exec 前解除挂起，被测进程按正常信号语义运行（TERM 清理也才有效）
            proc = subprocess.Popen(*args, preexec_fn=_unblock_in_child, **kwargs)
            LIVE_GROUPS.add(proc.pid)  # start_new_session：pid 即 pgid
    except BaseException:
        if proc is not None:
            stop_group(proc)  # 回收失败时保持登记：锁保留，fail-closed
        raise
    return proc


def stop_group(proc):
    """清理期间挂起信号（清理有界：TERM→1s→KILL→1s→2s）；只有证实全部停止才从登记移除。"""
    with deferred_signals():
        audit = _stop_group(proc)
        if audit['allStopped']:
            LIVE_GROUPS.discard(proc.pid)
        return audit


def _stop_group(proc):
    errors = []
    for sig in (signal.SIGTERM, signal.SIGKILL):
        try:
            os.killpg(proc.pid, sig)
        except ProcessLookupError:
            break
        except OSError as error:
            errors.append(str(error))
        try:
            proc.wait(timeout=1)
        except subprocess.TimeoutExpired:
            pass
    try:
        proc.wait(timeout=2)
    except subprocess.TimeoutExpired:
        errors.append('leader did not exit')
    remaining = []
    try:
        os.killpg(proc.pid, 0)
        remaining.append(proc.pid)
    except ProcessLookupError:
        pass
    except OSError as error:
        errors.append(str(error))
    return {'allStopped': not errors and not remaining, 'remaining': remaining, 'errors': errors}


def capture(argv, env, cwd, stdin, timeout):
    """Bounded pipe recording, one POSIX session, kill whole group even after EOF."""
    record = {'status': 'ERROR', 'timedOut': False, 'processTree': []}
    events, data = [], {'stdout': bytearray(), 'stderr': bytearray()}
    started = time.monotonic()
    proc = None
    try:
        # #933 fix-r3 P1-1：句柄必须先入库，排队信号才能投递；否则该窗口里的
        # 异常会让已登记的进程组成为孤儿。投递点落在 except 里 → 先回收再上抛。
        with deferred_signals():
            proc = spawn(argv, cwd=cwd, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                         stderr=subprocess.PIPE, start_new_session=True, umask=0o077)
    except BaseException:
        if proc is not None:
            stop_group(proc)
        raise
    record['pid'] = proc.pid
    selector = selectors.DefaultSelector()
    try:
        # This batch fixes stdin to empty; future input must use nonblocking writes.
        if stdin:
            raise ValueError('D0-pr1 only registers empty stdin')
        proc.stdin.close()
        for name, stream in (('stdout', proc.stdout), ('stderr', proc.stderr)):
            os.set_blocking(stream.fileno(), False)
            selector.register(stream, selectors.EVENT_READ, name)
        sampled = 0.0
        while selector.get_map() or proc.poll() is None:
            now = time.monotonic()
            if now - started >= timeout:
                record['timedOut'] = True
                raise TimeoutError('scenario timeout')
            if now >= sampled:
                rows = [row for row in process_table() if row['pgid'] == proc.pid or row['ppid'] == proc.pid]
                record['processTree'].extend(row for row in rows if row not in record['processTree'])
                if any(row['pid'] != proc.pid for row in rows):
                    raise RuntimeError('unexpected child: offline profile forbids fork')
                sampled = now + 0.05
            for key, _ in selector.select(.02):
                chunk = os.read(key.fileobj.fileno(), 65536)
                if not chunk:
                    selector.unregister(key.fileobj)
                    continue
                data[key.data].extend(chunk)
                events.append({'stream': key.data, 'hex': chunk.hex()})
                if sum(map(len, data.values())) > MAX_BYTES:
                    raise RuntimeError('output limit exceeded; recording rejected')
        record['exitCode'] = proc.wait(timeout=1)
        record['status'] = 'DONE'
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError, KeyboardInterrupt) as error:
        record['error'] = str(error)
    finally:
        selector.close()
        record['audit'] = stop_group(proc)
        record['exitCode'] = proc.returncode
        record['elapsedSeconds'] = time.monotonic() - started
        for stream in (proc.stdout, proc.stderr):
            stream.close()
    if not record['audit']['allStopped'] or not record['processTree']:
        record['status'] = 'ERROR'
    return record, {key: bytes(value) for key, value in data.items()}, events


def preflight(output, runtime, profile, node):
    evidence = {'status': 'BLOCKED', 'errors': [], 'profileSha256': digest(profile),
                'profilePath': str(profile), 'profileText': profile.read_text(), 'probeProfileSha256': digest(profile)}
    sentinel = output / 'outside-sentinel.txt'
    sentinel.write_text('D0 must stay outside child read/write scope\n')
    original = digest(sentinel)
    try:
        evidence['driverProcess'] = next(row for row in process_table() if row['pid'] == os.getpid())
    except (Blocked, OSError) as error:
        evidence['errors'].append(str(error))
    proc = None
    try:
        # #933 fix-r3 P1-1：同 capture()——句柄入库前不解除屏蔽。
        # #933 fix-r3 P2：探针的 connectDenied 检查必须测由 MOCK_PORT 派生的
        # 端口，探针从 argv[3] 读取，自身不持有任何端口字面量。
        with deferred_signals():
            proc = spawn(['/usr/bin/sandbox-exec', '-f', str(profile), str(node),
                          str(SCRIPT / 'isolation_probe.mjs'), str(sentinel), str(MOCK_PORT)],
                         cwd=runtime / 'project', env=environment(runtime, SCENARIOS[0]),
                         stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                         start_new_session=True, umask=0o077)
    except BaseException:
        if proc is not None:
            audit = stop_group(proc)
            if not audit['allStopped']:
                raise CleanupBlocked('isolation probe cleanup failed; retaining shared lock')
        raise
    stdout, stderr = b'', b''
    try:
        stdout, stderr = proc.communicate(b'', timeout=10)
    except (subprocess.TimeoutExpired, KeyboardInterrupt) as error:
        evidence['errors'].append(f'isolation probe interrupted: {error}')
    finally:
        evidence['audit'] = stop_group(proc)
        for stream in (proc.stdin, proc.stdout, proc.stderr):
            stream.close()
    (output / 'isolation.stdout.bin').write_bytes(stdout)
    (output / 'isolation.stderr.bin').write_bytes(stderr)
    evidence['exitCode'] = proc.returncode
    if not evidence['audit']['allStopped']:
        evidence['errors'].append('isolation probe cleanup failed')
    if proc.returncode:
        evidence['errors'].append('sandbox probe failed: ' + stderr.decode(errors='replace')[:1000])
    else:
        evidence['probe'] = json.loads(stdout)
        if evidence['probe']['status'] != 'PASS':
            evidence['errors'].append('isolation probe assertions failed')
    if digest(sentinel) != original:
        evidence['errors'].append('outside sentinel changed')
    if not evidence['errors']:
        evidence['status'] = 'PASS'
    save(output / 'isolation.json', evidence)
    if not evidence['audit']['allStopped']:
        raise CleanupBlocked('isolation probe cleanup failed; retaining shared lock')
    if evidence['status'] != 'PASS':
        raise Blocked('; '.join(evidence['errors']))
    return evidence


def mock_listening_line(port):
    """The readiness line the mock gateway prints itself once its bind is
    committed. Python and Node each write a copy of this text (#933); the ping
    unit test binds the two together so neither side can drift alone."""
    return f'mock-gateway listening on 127.0.0.1:{port}'


def mock_gateway_preflight(output, runtime, node, profile):
    """Start the #933 mock gateway outside the sandbox, then prove the relaxed
    profile still denies every network surface except 127.0.0.1:28999.

    The port is owned by the mock's own bind, never by a check-then-use
    preflight: readiness is the mock's own ``listening`` stdout line (Node only
    prints it from the listen callback, i.e. after the kernel committed the
    bind), so a competitor that grabbed 28999 makes the owned mock exit and the
    whole round refuse. Every step after spawn() is cleanup-protected: any
    exception or signal in that window stops the registered mock before
    propagating, so a dead or half-started mock can never back a later round.

    Returns (proc, evidence); the caller stops the registered process inside
    the parity lock."""
    logs = output / 'mock-gateway'
    logs.mkdir()
    # Node writes stdout to a file synchronously on POSIX, so this line is
    # observable in the log as soon as the listen callback fires.
    listening = mock_listening_line(MOCK_PORT)
    proc = None
    with (logs / 'stdout.log').open('wb') as stdout_log, (logs / 'stderr.log').open('wb') as stderr_log:
        try:
            # #933 fix-r3 P1-1：句柄入库前不解除屏蔽——投递点落在 try 内，
            # 下面的 except 才能看到 proc 并 stop_group（评审 P1 复现的窗口）。
            with deferred_signals():
                proc = spawn([node, str(SCRIPT / 'mock_gateway.mjs'), str(MOCK_PORT)],
                             env={'PATH': '/usr/bin:/bin', 'LC_ALL': 'C', 'PILOTDECK_MOCK_TOKEN': TOKEN},
                             stdin=subprocess.DEVNULL, stdout=stdout_log, stderr=stderr_log,
                             start_new_session=True, umask=0o077)
            # Everything past spawn() is inside the guard: a profile read
            # failure or a delivered signal must still stop the mock.
            evidence = {'status': 'BLOCKED', 'errors': [], 'profilePath': str(profile),
                        'profileText': profile.read_text(), 'profileSha256': digest(profile),
                        'probeProfileSha256': digest(profile), 'mockProcess': {
                            'pid': proc.pid, 'argv': [node, str(SCRIPT / 'mock_gateway.mjs'), str(MOCK_PORT)],
                            'token': {'redacted': True, 'length': len(TOKEN),
                                      'sha256': hashlib.sha256(TOKEN.encode()).hexdigest()}}}
            deadline = time.monotonic() + 10
            while listening not in (logs / 'stdout.log').read_text(errors='replace'):
                if proc.poll() is not None:
                    raise Blocked('mock gateway exited before listening: code '
                                 + str(proc.returncode) + '; stderr: '
                                 + (logs / 'stderr.log').read_text(errors='replace')[:200])
                if time.monotonic() >= deadline:
                    raise Blocked('mock gateway did not confirm listening within 10s')
                time.sleep(0.05)
            # The probe script itself must be readable under the relaxed profile;
            # only runtime paths are, so run the copy inside the runtime fixture.
            probe_script = runtime / 'gateway-isolation-probe.mjs'
            shutil.copyfile(SCRIPT / 'gateway_isolation_probe.mjs', probe_script)
            # Keep the path short: macOS sun_path is 104 bytes.
            unix_path = runtime / 'tmp' / 'gw-probe.sock'
            listener = socket.socket(socket.AF_UNIX)
            listener.bind(str(unix_path))
            listener.listen(4)
            try:
                record, streams, _ = capture(['/usr/bin/sandbox-exec', '-f', str(profile), node,
                                              str(probe_script), str(MOCK_PORT), '18789', '29000', str(unix_path)],
                                             environment(runtime, SCENARIOS[0]), runtime / 'project', b'', 10)
            finally:
                listener.close()
                unix_path.unlink(missing_ok=True)
            # The probe reached the port; the live listener must still be the
            # owned mock, not a replacement that grabbed a died mock's port.
            if proc.poll() is not None:
                raise Blocked(f'mock gateway exited after readiness: code {proc.returncode}')
            evidence['exitCode'] = record['exitCode']
            evidence['audit'] = record['audit']
            evidence['probeCleanup'] = {'unixSocketRemoved': not unix_path.exists()}
            if not record['audit']['allStopped']:
                evidence['errors'].append('gateway isolation probe cleanup failed')
            if record['status'] != 'DONE' or record['exitCode'] != 0:
                evidence['errors'].append('gateway isolation probe failed: '
                                          + streams['stderr'].decode(errors='replace')[:1000])
            else:
                try:
                    evidence['gatewayProbe'] = json.loads(streams['stdout'])
                except ValueError:
                    evidence['errors'].append('gateway isolation probe emitted no JSON report')
            if not evidence['errors']:
                evidence['status'] = 'PASS'
            save(output / 'mock-gateway.json', evidence)
            if not record['audit']['allStopped']:
                raise CleanupBlocked('gateway isolation probe cleanup failed; retaining shared lock')
            if evidence['status'] != 'PASS':
                raise Blocked('; '.join(evidence['errors']))
            return proc, evidence
        except BaseException:
            # The mock is never left running for a later round on any failure path.
            if proc is not None:
                audit = stop_group(proc)
                if not audit['allStopped']:
                    raise CleanupBlocked('mock gateway cleanup failed; retaining shared lock')
            raise


def _denied_node_exec(record, streams):
    """Classify a Node exec attempt under a profile that must deny it."""
    stderr = streams['stderr']
    denied = record['status'] == 'DONE' and record['timedOut'] is False and record['exitCode'] not in (None, 0) and not streams['stdout'] and b'execvp' in stderr and any(
        s in stderr for s in (b'Operation not permitted', b'Permission denied'))
    proof = {'denied': denied, 'exitCode': record['exitCode'], 'stdoutHex': streams['stdout'].hex(),
             'stderrHex': stderr.hex(), 'audit': record['audit'], 'status': record['status'], 'timedOut': record['timedOut']}
    return denied, proof


def rust_isolation(output, runtime, profile, node, ts_isolation):
    """The common probe uses the TS policy; Rust is its restricted policy.

    Its actual policy is reconstructed by compare. Additionally test exec of
    Node under that exact Rust policy, without adding a probe exec exception.
    """
    record, streams, _ = capture(['/usr/bin/sandbox-exec', '-f', str(profile), node, '--version'],
                                 environment(runtime, SCENARIOS[0]), runtime / 'project', b'', 10)
    denied, proof = _denied_node_exec(record, streams)
    evidence = {**ts_isolation, 'profilePath': str(profile), 'profileText': profile.read_text(),
                'profileSha256': digest(profile), 'nodeExec': proof, 'status': 'PASS' if denied else 'BLOCKED'}
    save(output / 'rust-isolation.json', evidence)
    if not record['audit']['allStopped']:
        raise CleanupBlocked('Rust Node-denial probe cleanup failed')
    if not denied:
        raise Blocked('Rust sandbox did not prove Node exec denied')
    return evidence


def rust_mock_isolation(output, runtime, profile, node, ts_mock_isolation):
    """#933: the rust-mock profile opens only 28999 outbound; Node exec must
    stay denied under it, exactly like the base Rust policy."""
    record, streams, _ = capture(['/usr/bin/sandbox-exec', '-f', str(profile), node, '--version'],
                                 environment(runtime, SCENARIOS[0]), runtime / 'project', b'', 10)
    denied, proof = _denied_node_exec(record, streams)
    evidence = {**ts_mock_isolation, 'profilePath': str(profile), 'profileText': profile.read_text(),
                'profileSha256': digest(profile), 'nodeExec': proof, 'status': 'PASS' if denied else 'BLOCKED'}
    save(output / 'rust-mock-isolation.json', evidence)
    if not record['audit']['allStopped']:
        raise CleanupBlocked('Rust mock Node-denial probe cleanup failed')
    if not denied:
        raise Blocked('Rust mock sandbox did not prove Node exec denied')
    return evidence


def run_round(output, runtime, profile, isolation, side, provenance, timeout, mock_profile=None, mock_isolation=None):
    if baseline_identity(Path(provenance['baseline']['path'])) != provenance['baseline']:
        raise Blocked('baseline changed before round')
    mock_ids = mock_gateway_scenarios(SCENARIOS)
    if mock_ids and (mock_profile is None or mock_isolation is None):
        # A marked scenario run under the base profile would fail to connect
        # without any isolation evidence for the relaxed profile: refuse early.
        raise Blocked(f'{side}: mock gateway scenarios {sorted(mock_ids)} require a mock profile and isolation evidence')
    folder = output / side
    folder.mkdir()
    (folder / 'cases').mkdir()
    actual_side = 'ts' if side.startswith('ts-') else 'rust'
    run = {'schema': 1, 'kind': 'cli-d0-recording', 'origin': 'live-os-sandbox', 'side': actual_side, 'status': 'RUNNING',
           'runtime': str(runtime), 'scenarios': [s['id'] for s in SCENARIOS],
           'apparatus': hashes(), 'isolation': isolation, 'provenance': provenance}
    if mock_isolation is not None:
        run['mockIsolation'] = mock_isolation
    save(folder / 'round.json', run)
    for scenario in SCENARIOS:
        case = folder / 'cases' / scenario['id']
        case.mkdir()
        fixture(runtime, scenario, Path(provenance['baseline']['path']))
        save(case / 'initial.json', snapshot(runtime))
        command = [provenance.get('stagedExecutable', provenance['executable'])['path']]
        if actual_side == 'ts':
            command.append(str(Path(provenance['baseline']['path']) / 'dist/src/cli/pilotdeck.js'))
        command.extend(scenario['argv'])
        env = environment(runtime, scenario)
        # #933: marked scenarios run under the relaxed mock profile; every other
        # scenario keeps the byte-identical base profile of the previous batch.
        scenario_profile = mock_profile if scenario['id'] in mock_ids else profile
        binding = mock_isolation if (mock_isolation is not None and scenario['id'] in mock_ids) else isolation
        if digest(scenario_profile) != binding['profileSha256']:
            raise Blocked('sandbox profile changed before scenario')
        sandbox_argv = ['/usr/bin/sandbox-exec', '-f', str(scenario_profile), *command]
        record, streams, events = capture(sandbox_argv,
                                           env, runtime / 'project', bytes.fromhex(scenario['stdinHex']), timeout)
        record.update(argv=scenario['argv'], executedArgv=command, stdinHex=scenario['stdinHex'],
                      cwd=str(runtime / 'project'), env=env_summary(env), sandboxArgv=sandbox_argv,
                      profileSha256=digest(scenario_profile))
        for name, value in streams.items():
            (case / f'{name}.bin').write_bytes(value)
        save(case / 'events.json', events)
        save(case / 'record.json', record)
        save(case / 'final.json', snapshot(runtime))
        runtime.rename(case / 'tree')
        print(f'{side} {scenario["id"]}: {record["status"]} exit={record["exitCode"]}', flush=True)
        if not record['audit']['allStopped']:
            raise CleanupBlocked(f'{side}/{scenario["id"]}: cleanup failed; retaining shared lock')
        if record['profileSha256'] != binding['profileSha256']:
            raise Blocked('sandbox profile changed during scenario')
        if record['status'] != 'DONE':
            raise Blocked(f'{side}/{scenario["id"]}: incomplete recording')
    if baseline_identity(Path(provenance['baseline']['path'])) != provenance['baseline']:
        raise Blocked('baseline changed during round')
    run['status'] = 'DONE'
    save(folder / 'round.json', run)
    return folder


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--baseline', type=Path, help='explicit > PDRS_BASELINE > this checkout resources/PilotDeck')
    ap.add_argument('--rust-bin', type=Path, help='optional Rust CLI; no candidate is run by default')
    ap.add_argument('--rust-source', type=Path, help='required with --rust-bin: checkout used to build the binary')
    ap.add_argument('--node', default=shutil.which('node'), help='installed Node executable; no download')
    ap.add_argument('--output', type=Path, default=ROOT / '_tmp/20261017-d0/calibration')
    ap.add_argument('--timeout', type=float, default=15, help='per scenario seconds, (0, 60]')
    args = ap.parse_args()
    if not math.isfinite(args.timeout) or not 0 < args.timeout <= 60:
        ap.error('--timeout must be finite and in (0, 60]')
    if bool(args.rust_bin) != bool(args.rust_source):
        ap.error('--rust-bin and --rust-source must be supplied together')
    check_budget(args.timeout, bool(args.rust_bin))
    output = output_path(args.output)
    output.mkdir(parents=True, mode=0o700)
    summary = {'status': 'BLOCKED', 'scope': 'D0-pr1 calibration; no command migration claim'}
    staged = staged_skills = None
    try:
        per_stream_scenarios(SCENARIOS)
        mock_ids = mock_gateway_scenarios(SCENARIOS)
        if sys.platform != 'darwin' or not Path('/usr/bin/sandbox-exec').is_file():
            raise Blocked('D0-pr1 requires macOS sandbox-exec; no unconfined fallback')
        baseline = baseline_path(args.baseline)
        if not args.node:
            raise Blocked('Node executable not found')
        node = executable(args.node)
        identity = baseline_identity(baseline)
        provenance = {'side': 'ts', 'executable': node, 'sourceHead': HEAD, 'sourcePath': str(baseline),
                      'baseline': identity, 'node': node, 'opensslConfig': OPENSSL_CONFIG}
        runtime = output / 'runtime'
        expected_rust = rust_identity(args.rust_bin, args.rust_source) if args.rust_bin else None
        if expected_rust and (expected_rust['executable']['path'] == node['path']
                              or expected_rust['executable']['sha256'] == node['sha256']
                              or expected_rust['sourcePath'] == str(baseline)):
            raise Blocked('Rust candidate must not be the TS runtime or source')
        profile = output / 'ts.sb'
        profile.write_text(sandbox_profile(runtime, baseline, node['path'], 'ts'))
        ts_mock_profile = None
        if mock_ids:
            ts_mock_profile = output / 'ts-mock.sb'
            ts_mock_profile.write_text(sandbox_profile(runtime, baseline, node['path'], 'ts', mock_gateway=True))
        with parity_lock() as owner:
            # Stop before other runners may consider the lease expired. The
            # handler raises through capture's finally so owned groups are reaped.
            previous_alarm = signal.signal(signal.SIGALRM, lease_expired)
            signal.alarm(2280)
            save(output / 'lock.json', owner)
            fixture(runtime)
            isolation = preflight(output, runtime, profile, node['path'])
            runtime.rename(output / 'preflight-tree')
            mock_proc = mock_isolation = None
            try:
                if mock_ids:
                    fixture(runtime)
                    # #933 fix-r3 P1-1：交接窗口同样覆盖元组解包——句柄入库
                    # （mock_proc 赋值）后才解除屏蔽，finally 才停得掉 mock。
                    with deferred_signals():
                        mock_proc, mock_isolation = mock_gateway_preflight(output, runtime, node['path'], ts_mock_profile)
                    runtime.rename(output / 'mock-preflight-tree')
                first = run_round(output, runtime, profile, isolation, 'ts-1', provenance, args.timeout,
                                  mock_profile=ts_mock_profile, mock_isolation=mock_isolation)
                second = run_round(output, runtime, profile, isolation, 'ts-2', provenance, args.timeout,
                                   mock_profile=ts_mock_profile, mock_isolation=mock_isolation)
                summary['selfComparison'] = compare(first, second)
                save(output / 'self-comparison.json', summary['selfComparison'])
                if summary['selfComparison']['status'] != 'PASS':
                    summary['status'] = 'FAIL'
                else:
                    run_controls = _load('controls', _here / 'controls.py').run_controls
                    summary['controls'] = run_controls(first, output / 'controls')
                    summary['status'] = summary['controls']['status']
                    if args.rust_bin:
                        staged = stage_rust(output, expected_rust, baseline)
                        # The Rust round reads this copy, not baseline/skills; re-checked below.
                        staged_skills = staged_skills_identity(output / 'rust-stage/skills')
                        rust_profile = output / 'rust.sb'
                        rust_profile.write_text(sandbox_profile(runtime, baseline, staged['path'], 'rust'))
                        fixture(runtime)
                        rust_probe = rust_isolation(output, runtime, rust_profile, node['path'], isolation)
                        runtime.rename(output / 'rust-preflight-tree')
                        rust_mock_profile = rust_mock = None
                        if mock_ids:
                            rust_mock_profile = output / 'rust-mock.sb'
                            rust_mock_profile.write_text(sandbox_profile(runtime, baseline, staged['path'], 'rust', mock_gateway=True))
                            fixture(runtime)
                            rust_mock = rust_mock_isolation(output, runtime, rust_mock_profile, node['path'], mock_isolation)
                            runtime.rename(output / 'rust-mock-preflight-tree')
                        rust_provenance = {**expected_rust, 'stagedExecutable': staged, 'stagedSkills': staged_skills, 'baseline': identity,
                                           'node': node, 'opensslConfig': OPENSSL_CONFIG}
                        candidate = run_round(output, runtime, rust_profile, rust_probe, 'rust', rust_provenance, args.timeout,
                                              mock_profile=rust_mock_profile, mock_isolation=rust_mock)
                        summary['rustComparison'] = compare(first, candidate, expected_rust)
                        if summary['rustComparison']['status'] != 'PASS':
                            summary['status'] = 'FAIL'
                # Ensure baseline executable and observed source/build were unchanged during recording.
                if executable(args.node) != node or baseline_identity(baseline) != identity:
                    raise Blocked('baseline/runtime changed during recording')
                if args.rust_bin and rust_identity(args.rust_bin, args.rust_source) != expected_rust:
                    raise Blocked('Rust executable/source HEAD changed during recording')
                if staged and executable(staged['path']) != staged:
                    raise Blocked('staged Rust binary changed during recording')
                if staged and staged_skills_identity(output / 'rust-stage/skills') != staged_skills:
                    raise Blocked('staged Rust skills changed during recording')
            finally:
                if mock_proc is not None:
                    # All rounds share one mock; stop it while still holding the
                    # lock so a surviving process keeps the lease retained.
                    audit = stop_group(mock_proc)
                    if not audit['allStopped']:
                        raise CleanupBlocked('mock gateway cleanup failed; retaining shared lock')
    except (RuntimeError, OSError, ValueError, subprocess.SubprocessError, KeyboardInterrupt) as error:
        summary['status'] = 'BLOCKED'
        summary['error'] = str(error)
    finally:
        if 'previous_alarm' in locals():
            signal.alarm(0)
            signal.signal(signal.SIGALRM, previous_alarm)
    save(output / 'summary.json', summary)
    print(json.dumps(summary, ensure_ascii=False, indent=2))
    return {'PASS': 0, 'FAIL': 1, 'BLOCKED': 2}[summary['status']]


if __name__ == '__main__':
    def interrupted(signum, frame):
        raise KeyboardInterrupt(f'signal {signum}')
    signal.signal(signal.SIGTERM, interrupted)
    try:
        code = main()
    except (Blocked, OSError) as error:
        print(json.dumps({'status': 'BLOCKED', 'error': str(error)}))
        code = 2
    raise SystemExit(code)
