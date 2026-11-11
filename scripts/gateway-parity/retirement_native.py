"""Run the approved retirement driver against the Rust production CLI (#1353).

The surrounding session_domain entry owns the lease, round paths, baselines,
method scorer and freezes. The witness traces the production process tree only;
the fixed TS official client and model mock are separate fixture roles.
"""
# >>> apparatus bootstrap (verbatim in every A2 entry; test_safety checks it)
# -I keeps PYTHONPATH, user site and the script directory out of module
# resolution.  It must run before every other standard-library import: a
# shadowed argparse/pathlib would otherwise execute before this check.  ``sys``
# is builtin; os is imported only after the frozen-module preflight below.
import sys

if not sys.flags.isolated:
    raise SystemExit(f'run with -I: python3 -I {__file__}')

if sys.version_info < (3, 11) or 'os' not in sys.modules:
    raise SystemExit('os must be preloaded from CPython 3.11+ frozen modules')
import os

_boot_dir = os.path.dirname(os.path.realpath(__file__))
_boot_dir = os.path.dirname(_boot_dir) if os.path.basename(_boot_dir) == 'controls' else _boot_dir
# Identity, not spelling: a case-insensitive alias, a symlinked spelling or a
# mixed ``//`` form names this very directory while the string differs
# (#1033). Nonexistent sys.path entries skip; an empty entry means cwd.
if any(
        os.path.isdir(entry or os.curdir)
        and any(os.path.isdir(root)
                and os.path.samefile(entry or os.curdir, root)
                for root in (_boot_dir, os.path.join(_boot_dir, 'controls')))
        for entry in sys.path):
    raise SystemExit('apparatus directory on sys.path; run the entry file directly with -I')

import importlib.util
from pathlib import Path
import tempfile

# -B only stops cache writes, so redirect the bytecode prefix too (fix-r5).
sys.pycache_prefix = tempfile.mkdtemp(prefix='pdrs-gateway-pyc-')
sys.dont_write_bytecode = True
# CPython has already compiled the CLI entry before it can acquire a lease.
# Capture that executing object, not a later read of the restored entry file.
_apparatus_code = sys._getframe().f_code

# Load the one guard by explicit path, never sys.path: a same-named package or
# extension module beside the source would win before the scan (fix-r6 P1).
# Inline on purpose: importing the guard is what this block defends.
_here = Path(__file__).resolve().parent
_apparatus = _here.parent if _here.name == 'controls' else _here
_guard_path = _apparatus / '_guard.py'
_guard = sys.modules.get('_guard')
if not (getattr(_guard, '__file__', None)
        and Path(_guard.__file__).resolve() == _guard_path
        and isinstance(getattr(_guard, '_apparatus_source', None), bytes)):
    _spec = importlib.util.spec_from_file_location('_guard', _guard_path)
    _guard = importlib.util.module_from_spec(_spec)
    sys.modules['_guard'] = _guard
    _guard_source = _guard_path.read_bytes()
    try:
        exec(compile(_guard_source, str(_guard_path), 'exec'), _guard.__dict__)
    except BaseException:
        sys.modules.pop('_guard', None)
        raise
    _guard._apparatus_source = _guard_source
if Path(_guard.__file__).resolve() != _guard_path:
    raise RuntimeError(f'_guard: loaded {_guard.__file__}, expected {_guard_path}')

_load = _guard.load
_has_git_checkout = _guard._has_git_checkout
_verify_cli_imports = _guard.verify_cli_imports
run_entry = _guard.run_entry
common = _load('common', _apparatus / 'common.py')
# <<< apparatus bootstrap
import atexit
import shutil

atexit.register(shutil.rmtree, sys.pycache_prefix, ignore_errors=True)
import collections
import hashlib
import json
import socket
import subprocess
import time
import urllib.request


class NativeRetirement:
    def __init__(self, domain, cli, sidecar, manifest, source_manifest):
        self.domain = domain
        self.cli = cli.resolve(strict=True)
        self.sidecar = sidecar.resolve(strict=True)
        self.manifest = manifest.resolve(strict=True)
        self.source_manifest = source_manifest.resolve(strict=True)
        self.source_sha = sha256(self.source_manifest)
        require = domain.require
        require(self.source_manifest.parent == self.manifest.parent.parent, 'NATIVE_BIN_CONFIG',
                'source manifest and bin manifest must describe the same fixed build directory')
        self.source = domain.read(self.source_manifest)
        require(isinstance(self.source.get('files'), list) and self.source['files'], 'NATIVE_BIN_CONFIG',
                'missing tracked source inventory in build source manifest')
        self.validate_source()
        domain.common.verify_native_runtime_libraries(self.source, self.manifest)
        rows = domain.read(self.manifest)
        require(isinstance(rows, list), 'NATIVE_BIN_CONFIG', 'candidate manifest must be the build binary list')
        by_name = {r['path']: r for r in rows}
        require(len(by_name) == len(rows), 'NATIVE_BIN_CONFIG', 'duplicate binary in candidate manifest')
        self.identities = {}
        self.manifest_sha = domain.common.digest(self.manifest)
        for role, name, path in [('host', 'pilotdeck', self.cli),
                                 ('sidecar', 'pilotdeck-agent-loop-sidecar', self.sidecar)]:
            registered = by_name.get(name, {})
            actual = sha256(path)
            require(registered.get('sha256') == actual and path.parent == self.manifest.parent
                    and path.name == name, 'NATIVE_BIN_CONFIG', f'{name} differs from build manifest')
            self.identities[role] = {'path': str(path), 'sha256': actual}
        require(socket.if_nameindex() == [(1, 'lo')], 'NATIVE_BIN_CONFIG',
                'live retirement fixture requires an isolated loopback-only network namespace')
        self.witness = domain._load('retirement_witness', domain.ROOT / 'scripts/native-witness/witness.py')
        self.ownership = set()
        self.mock_script = domain.SCRIPT / 'mock.mjs'
        self.history_script = domain.SCRIPT / 'session_history_driver.mts'

    def validate_source(self):
        d = self.domain
        d.require(sha256(self.source_manifest) == self.source_sha, 'NATIVE_BIN_CONFIG',
                  'build source manifest changed during the suite')
        verify_build_tree(d, d.ROOT, self.source)

    def validate_immutable(self):
        d = self.domain
        d.require(d.common.digest(self.manifest) == self.manifest_sha, 'NATIVE_BIN_CONFIG',
                  'candidate manifest changed during live retirement')
        self.validate_source()
        d.common.verify_native_runtime_libraries(self.source, self.manifest)
        self.witness.native.verify_binaries({'binaries': self.identities})

    def exercise(self, folder, row, baseline, env, deadline, launch, scenarios, history_script):
        d, witness = self.domain, self.witness
        port, mock_port = row['ports']['gateway'], row['ports']['mock']
        config_path = Path(row['runtime']['pilotHome']) / 'pilotdeck.yaml'
        row['nativeConfig'] = {'path': str(config_path), 'sha256': d.common.digest(config_path)}
        argv = [str(self.cli), 'server', '--host', '127.0.0.1', '--port', str(port)]
        allowed = {'PATH', 'LANG', 'LC_ALL', 'LD_LIBRARY_PATH', 'PILOT_HOME', 'TMPDIR',
                   'DATABASE_PATH', 'WORKSPACES_ROOT', 'PILOTDECK_GATEWAY_HOST',
                   'PILOTDECK_GATEWAY_PORT', 'PILOTDECK_GATEWAY_TOKEN',
                   'PILOTDECK_GATEWAY_TOKEN_PATH', 'PILOTDECK_SKIP_BROWSER_OPEN'}
        host_env = {k: v for k, v in env.items() if k in allowed}
        host_env.update(HOME=env['PILOT_HOME'], PILOTDECK_RS_SIDECAR=str(self.sidecar))
        contract = {'profile': 'A8.6/production-cli', 'project': env['GP_PROJECT'],
                    'externalEndpoints': [f'127.0.0.1:{mock_port}'],
                    'binaries': {'host': {**self.identities['host'], 'argv': argv},
                                 'sidecar': {**self.identities['sidecar'], 'argv': [str(self.sidecar)]}}}
        d.save(folder / 'native-environment.json', {
            'keys': sorted(host_env), 'candidateManifest': str(self.manifest),
            'candidateManifestSha256': self.manifest_sha,
            'networkNamespace': os.readlink('/proc/self/ns/net'),
            'hostArgv': argv, 'sidecar': str(self.sidecar), 'config': row['nativeConfig']})
        driver = None

        def stimulus(tracer, capture, capture_deadline):
            nonlocal driver
            ready_by = min(capture_deadline, time.monotonic() + d.RETIREMENT_READINESS_TIMEOUT_S)
            pid = None
            while time.monotonic() < ready_by:
                if tracer.poll() is not None:
                    raise d.RetirementLaunchError(f'owned production tracer exited {tracer.returncode}')
                try:
                    children = Path(f'/proc/{tracer.pid}/task/{tracer.pid}/children').read_text().split()
                    if len(children) == 1 and Path(f'/proc/{children[0]}/exe').resolve(strict=True) == self.cli:
                        with urllib.request.urlopen(f'http://127.0.0.1:{port}/health', timeout=.5) as response:
                            if response.status == 200:
                                pid = int(children[0]); break
                except OSError:
                    pass
                time.sleep(.05)
            if pid is None:
                raise d.RetirementLaunchError('owned production listener readiness timeout')
            listener = subprocess.run(['lsof', '-nP', '-a', '-p', str(pid), '-iTCP', '-sTCP:LISTEN', '-Fn'],
                                      capture_output=True, text=True, timeout=5)
            addresses = [line[1:] for line in listener.stdout.splitlines() if line.startswith('n')]
            row['listeners'] = {'pid': pid, 'addresses': addresses,
                                'loopbackOnly': listener.returncode == 0 and addresses == [f'127.0.0.1:{port}']}
            driver = launch('driver-retirement', ['node', '--import', baseline / 'node_modules/tsx/dist/loader.mjs',
                            d.RETIREMENT_DRIVER, '--url', f'ws://127.0.0.1:{port}/ws',
                            '--token', env['PILOTDECK_GATEWAY_TOKEN'], '--methods', d.RETIREMENT_METHODS_FIXTURE,
                            '--out', folder / 'driver'], None)
            d.require(driver is not None, 'ROUND_EXECUTION', 'official client not launched')
            runner = d._load('run_round', d.SCRIPT / 'run_round.py')
            ready_path = Path(env['GP_RETIREMENT_MODEL_READY'])
            release_path = Path(env['GP_RETIREMENT_MODEL_RELEASE'])
            try:
                # The fixture can hold its first model reply until the original
                # snapshot has sampled the official client's live WS peer.
                while not ready_path.is_file() and time.monotonic() < capture_deadline:
                    try:
                        runner.wait_owned(driver, 0)
                    except subprocess.TimeoutExpired:
                        time.sleep(.02)
                    else:
                        break
                row['connections'] = d._retirement_connections(pid)
            finally:
                release_path.touch(exist_ok=False)
            row['driverExitCode'] = runner.wait_owned(driver, max(.1, capture_deadline - time.monotonic()))
            if scenarios:
                row['scenarios'] = exercise_session_scenarios(d, folder, baseline, env, capture_deadline, launch, scenarios, history_script)
            witness.stop_host(tracer, pid)

        try:
            remaining = max(.001, deadline - time.monotonic())
            _launch, evidence = witness.capture(contract, folder / 'native-witness', host_env,
                    timeout=remaining, limit=d.RETIREMENT_ROUND_TIMEOUT, exercise=stimulus,
                    ownership=self.ownership)
        except Exception as error:
            # Keep successful official method observations independently even
            # when the process witness refused completeness. Never infer a clean
            # tree from absence of a parsed evidence.json.
            if driver is None or row.get('driverExitCode') is None:
                raise
            row['nativeWitness'] = {'status': 'BLOCKED', 'detail': str(error),
                                    'folder': str(folder / 'native-witness')}
            return
        finally:
            # A tracer that failed its own exact reaping proof must also
            # retain the official batch lease, even if no round was returned.
            for owner in self.ownership:
                prefix, _, pid = owner.partition(':')
                if prefix == 'native-witness-tracer' and pid.isdecimal():
                    d.common.LIVE_GROUPS.add(int(pid))
        rows = evidence['processes']
        host = rows[0]
        roles = collections.Counter(p['role'] for p in rows)
        row['census'] = {'rows': rows, 'declared': [p['pid'] for p in rows],
                         'declaredAlive': row['listeners']['pid'] == host['pid'],
                         'source': 'complete retained exec/exit trace; native witness role check is separate'}
        endpoint = contract['externalEndpoints'][0]
        calls = evidence['networkWitness']['calls']
        problems = []
        if roles['host'] != 1 or roles['sidecar'] < 1:
            problems.append('expected one production CLI host and at least one actual Rust sidecar exec')
        if any(p['ppid'] != host['pid'] for p in rows[1:]):
            problems.append('a sidecar does not descend directly from the production host')
        if any(call['target'] != endpoint for call in calls):
            problems.append('production child tree attempted an undeclared connect endpoint')
        if not any(call['target'] == endpoint for call in calls):
            problems.append('real model mock connection was not seen')
        if not evidence['cleanup']['allStopped'] or host.get('exitCode') != 0 or self.ownership:
            problems.append('production trace or owned cleanup is incomplete')
        row['nativeWitness'] = {'status': 'FAIL' if problems else 'PASS', 'problems': problems,
                                'roles': dict(roles), 'folder': str(folder / 'native-witness'),
                                'receiptSha256': d.common.digest(folder / 'native-witness/receipt.json')}
        d.require(d.common.digest(config_path) == row['nativeConfig']['sha256'], 'NATIVE_BIN_CONFIG',
                  'live production config changed during round')


def verify_build_tree(d, root, source):
    """Bind every tracked object, including paths Git status can suppress."""
    def git(*argv):
        env = {key: value for key, value in os.environ.items() if not key.startswith('GIT_')}
        env['GIT_NO_REPLACE_OBJECTS'] = '1'
        return subprocess.check_output(['git', '-C', str(root), *argv], env=env, timeout=30)

    d.require(git('rev-parse', 'HEAD').decode().strip() == source['head']
              and not git('status', '--porcelain', '--untracked-files=no'),
              'NATIVE_BIN_CONFIG', 'tracked source checkout is dirty or changed HEAD')
    idx = [part for part in git('ls-files', '-v', '-z').split(b'\0') if part]
    d.require(all(part.startswith(b'H ') for part in idx), 'NATIVE_BIN_CONFIG',
              'tracked index hides or overrides a worktree path')
    tree = {}
    for line in git('ls-tree', '-r', '-z', '--full-tree', 'HEAD').split(b'\0'):
        if not line:
            continue
        metadata, name = line.split(b'\t', 1)
        mode, kind, oid = metadata.split(b' ')
        d.require(kind == b'blob' and mode in (b'100644', b'100755', b'120000'),
                  'NATIVE_BIN_CONFIG', 'unknown tracked source mode')
        tree[os.fsdecode(name)] = (mode, oid.decode())
    d.require(len(idx) == len(tree) and {os.fsdecode(part[2:]) for part in idx} == tree.keys(),
              'NATIVE_BIN_CONFIG', 'worktree index does not match committed HEAD')
    files = {row['path']: row for row in source['files']}
    regular = {name for name, (mode, _) in tree.items() if mode != b'120000'}
    d.require(len(files) == len(source['files']) and files.keys() == regular,
              'NATIVE_BIN_CONFIG', 'build manifest omitted or added a committed regular file')
    git_hash = git('rev-parse', '--show-object-format').decode().strip()
    d.require(git_hash in ('sha1', 'sha256'), 'NATIVE_BIN_CONFIG', 'unknown Git object format')
    for name, (mode, oid) in tree.items():
        path = root / name
        if mode == b'120000':
            link = os.fsencode(os.readlink(path))
            object_hash = hashlib.new(git_hash, b'blob ' + str(len(link)).encode() + b'\0' + link,
                                      usedforsecurity=False).hexdigest()
            d.require(object_hash == oid, 'NATIVE_BIN_CONFIG', f'tracked symlink target changed: {name}')
            continue
        row = files[name]
        info = path.stat()
        d.require(path.is_file() and path.resolve() == path and type(row['bytes']) is int
                  and info.st_size == row['bytes'] and bool(info.st_mode & 0o111) == (mode == b'100755'),
                  'NATIVE_BIN_CONFIG', f'tracked file mode/size drift: {name}')
        product_hash = hashlib.sha256()
        object_hash = hashlib.new(git_hash, b'blob ' + str(info.st_size).encode() + b'\0', usedforsecurity=False)
        with path.open('rb') as file:
            while data := file.read(1024 * 1024):
                product_hash.update(data)
                object_hash.update(data)
        d.require(product_hash.hexdigest() == row['sha256'] and object_hash.hexdigest() == oid,
                  'NATIVE_BIN_CONFIG', f'tracked file bytes differ from the build manifest or HEAD: {name}')


def sha256(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


# #1315 uses the real registered A2 scenarios as two inputs. A successful
# scenario is not presented as all A7 scenarios or as a new acceptance rule.
SESSION_SCENARIOS = ('permission', 'reconnect', 'u9-u10-history')
SESSION_GROUPS = {
    'C': 'submit_turn reconnect_interaction elicitation_respond permission_decide',
    'S1': 'new_session list_sessions resume_session close_session delete_session rename_session tag_session close_project_sessions record_agent_status_message',
    'S2': 'steer_turn cancel_steer abort_turn',
    'S3': 'read_session_messages fork_session replace_last_turn finalize_last_turn_replacement read_subagent_messages export_session_transcript restore_session_transcript',
    'S4': 'permission_list grant_session_permission user_dialog_list user_dialog_claim user_dialog_release user_dialog_respond',
    'S5': 'session_model_set session_model_clear set_permission_mode set_session_thinking output_styles_list set_output_style seed_read_state',
    'S6': 'usage_snapshot model_usage_snapshot mcp_server_status rewind_files background_task_stop background_tasks',
    'S7': 'sop_status sop_resume always_on_apply always_on_abort always_on_rerun_plan',
    'S8': 'run_get run_events run_reattach',
}


def configure_loopback_mock(d, folder, home, mock_port, env, *, capture_peer):
    env['HOME'] = str(home)
    env['GP_RETIREMENT_WEIXIN_LOG'] = str(folder / 'weixin-mock.jsonl')
    if capture_peer:
        env['GP_RETIREMENT_MODEL_READY'] = str(folder / 'model-request-live')
        env['GP_RETIREMENT_MODEL_RELEASE'] = str(folder / 'model-request-release')
    credential = {'baseUrl': f'http://127.0.0.1:{mock_port}',
                  'botToken': 'retirement-mock-only', 'accountId': 'retirement-mock'}
    # TS resolves the user's default credentials path; native assembly uses
    # the effective pilot home. Both are owned fixture directories.
    for target in (home, home / '.pilotdeck'):
        target.mkdir(exist_ok=True)
        path = target / 'weixin-credentials.json'
        d.save(path, credential)
        path.chmod(0o600)
    with (home / 'pilotdeck.yaml').open('a', encoding='utf-8') as config:
        config.write('extension: {builtinPluginsEnabled: {browser-use: false, funasr: false, windows-skills: false}}\n')


def exercise_session_scenarios(d, folder, baseline, env, deadline, launch, scenarios, history_script):
    runner = d._load('run_round', d.SCRIPT / 'run_round.py')
    # The 101-entry driver deliberately closes its project at the end. A7
    # interaction and history scenarios must start in a separate live project.
    project = folder / 'scenario-project'
    project.mkdir()
    registration = Path(env['PILOT_HOME']) / 'projects/gateway-parity-session-scenarios'
    registration.mkdir()
    (registration / '.cwd').write_text(str(project) + '\n')
    env = {**env, 'GP_PROJECT': str(project)}
    evidence = []
    for scene in scenarios:
        raw = folder / 'scenarios' / scene
        loader = ['node', '--import', baseline / 'node_modules/tsx/dist/loader.mjs']
        endpoint = f"ws://127.0.0.1:{env['PILOTDECK_GATEWAY_PORT']}/ws"
        token = env['PILOTDECK_GATEWAY_TOKEN']
        if scene == 'u9-u10-history':
            fixture = d.ROOT / 'src/pilotdeck-gateway/tests/fixtures/native-history/subagent-bundle.json'
            argv = [*loader, history_script, baseline, fixture, env['GP_PROJECT'], env['PILOT_HOME'], endpoint, token, raw]
        else:
            argv = [*loader, d.SCRIPT / 'driver.mts', '--url', endpoint, '--token', token,
                    '--scenario', scene, '--out', raw]
        driver = launch('driver-session-' + scene, argv, None, child_env=env)
        code = runner.wait_owned(driver, max(.001, min(45, deadline - time.monotonic())))
        evidence.append({'case': scene, 'driverExitCode': code, 'folder': str(raw),
                         'files': {path.name: sha256(path) for path in sorted(raw.glob('*')) if path.is_file()}})
    return evidence


def wire_index(domain, rounds, methods):
    """Retain per-connection total order and protocol sequence namespaces.

    This is a raw receipt, not a redefinition of A7 scenario/event semantics.
    """
    wanted = set(methods)
    output = {}
    for row in rounds:
        if not row.get('frames'):
            continue
        connections, request_ids = {}, {}
        previous = 0
        for wire in row['frames']:
            sequence, conn = wire['seq'], wire['connection']
            domain.require(type(sequence) is int and sequence == previous + 1 and type(conn) is int and conn > 0,
                           'SESSION_WIRE', 'retirement driver lost a wire record or connection id')
            previous = sequence
            frame = wire.get('frame')
            direction = wire['direction']
            domain.require(direction in ('send', 'receive'), 'SESSION_WIRE', 'invalid wire direction')
            if not isinstance(frame, dict):
                domain.require(direction == 'receive' and 'close' in wire and 'raw' not in wire,
                               'SESSION_WIRE', 'record contains no decoded frame or close event')
                connections.setdefault(conn, []).append({'pointer': sequence, 'close': wire['close']})
                continue
            domain.require(json.loads(wire['raw']) == frame, 'SESSION_WIRE', 'decoded frame differs from raw frame')
            frame_id = frame.get('id')
            method = frame.get('method')
            if direction == 'send' and frame.get('type') == 'request':
                request_ids[(conn, frame_id)] = method
            if direction == 'receive':
                method = request_ids.get((conn, frame_id))
            connections.setdefault(conn, []).append({'pointer': sequence, 'method': method,
                'session50': method in wanted, 'direction': direction, 'type': frame.get('type'),
                'requestId': frame_id, 'requestSeq': frame.get('seq'),
                'final': frame.get('final'), 'notificationName': frame.get('name'),
                'payload': frame.get('event', frame.get('result', frame.get('payload'))),
                'interactionBinding': frame.get('interactionBinding')})
        output[row['label']] = connections
    return output


def round_artifacts(folder):
    """Bind durable files and consumer originals after owned shutdown."""
    files = []
    for top in ('driver', 'scenarios', 'native-witness', 'home/projects'):
        for path in sorted((folder / top).rglob('*')):
            if path.is_file() and not path.is_symlink():
                files.append(path)
    for name in ('model.jsonl', 'weixin-mock.jsonl', 'model-request-live', 'model-request-release',
                 'project/retirement-written.txt', 'round.json', 'native-environment.json'):
        path = folder / name
        if path.is_file():
            files.append(path)
    return {path.relative_to(folder).as_posix(): sha256(path) for path in files}


def existing_outcomes(d, output, rounds):
    checker = d.ROOT / 'scripts/retirement-outcome-parity/check_outcome_parity.py'
    oracle = checker.with_name('ts-oracle.json')
    hashes = {str(p): sha256(p) for p in (checker, oracle)}
    args = ['check', '--oracle', str(oracle)]
    for folder in rounds:
        args += ['--round', str(folder)]
    result = subprocess.run([sys.executable, '-I', '-B', str(checker), *args],
                            capture_output=True, text=True, timeout=120)
    (output / 'outcome.stdout').write_text(result.stdout)
    (output / 'outcome.stderr').write_text(result.stderr)
    d.require(hashes == {str(p): sha256(p) for p in (checker, oracle)}, 'APPARATUS_CHANGED',
              'registered #1477 scorer or oracle changed during replay')
    try:
        observed = json.loads(result.stdout) if result.stdout.strip() else None
    except ValueError:
        observed = None
    classification, reason = classify_outcome(result.returncode, observed)
    if classification == 'PASS' and len(rounds) == d.RETIREMENT_ROUNDS and (
            observed.get('p7Evaluated') is not True or type(observed.get('p7Rounds')) is not int
            or observed['p7Rounds'] != d.RETIREMENT_ROUNDS):
        classification, reason = 'FAIL', 'OUTCOME_K5_NOT_EVALUATED'
    return {'exitCode': result.returncode, 'sha256': hashes, 'report': observed,
            'classification': classification, 'reason': reason}


def collect_rounds(native, output, original_output, baseline, count, scenarios, *, reference=False):
    d = native.domain
    attempts, rows, folders = [], [], []
    for index in range(1, count + 1):
        for attempt, ports in enumerate(d.RETIREMENT_PORT_SETS, start=1):
            if not d._retirement_ports_available(ports):
                attempts.append({'round': index, 'ports': ports, 'result': 'in use'})
                continue
            # Existing fixture always gets round-01 at the same live path.
            # Move only after its mock, driver and gateway have all stopped.
            live = original_output / 'round-01'
            dest = output / f'round-{index:02d}'
            try:
                row = d._retirement_round(1, original_output, baseline, ports, d.RETIREMENT_ROUND_TIMEOUT,
                                          native=None if reference else native, scenarios=scenarios, live_profile=True,
                                          mock_script=native.mock_script, history_script=native.history_script)
            except d.RetirementLaunchError as error:
                if live.is_dir():
                    live.rename(output / f'round-{index:02d}-launch-failed-{attempt}')
                attempts.append({'round': index, 'ports': ports, 'result': str(error)})
                continue
            d.require(not native.ownership, 'CLEANUP', f'trace still owns processes: {native.ownership!r}')
            live.rename(dest)
            row['runtimeFolder'] = str(live)
            row['runtimeBeforeArchive'] = row['runtime']
            row['runtime'] = {key: str(dest / Path(value).relative_to(live))
                              for key, value in row['runtime'].items()}
            row['folder'] = str(dest)
            for key in ('nativeConfig', 'nativeWitness'):
                if key in row:
                    for field in ('path', 'folder'):
                        if field in row[key]:
                            row[key][field] = str(dest / Path(row[key][field]).relative_to(live))
            for scene in row.get('scenarios', []):
                scene['folder'] = str(dest / Path(scene['folder']).relative_to(live))
            row['label'] = dest.name
            row['artifactsSha256'] = round_artifacts(dest)
            rows.append(row)
            folders.append(dest)
            attempts.append({'round': index, 'ports': ports, 'result': 'observed'})
            break
        else:
            raise d.EvidenceError('PORT_CONFLICT', f'round {index} could not run on any registered port set')
    return rows, folders, attempts


def round_receipt(row):
    return {k: v for k, v in row.items() if k not in ('frames', 'outcomes', 'http')}


def run_suite(d, native, suite, output, baseline, *, diagnostic_rounds=None, cold_capture=None, product_capture=None):
    expected, provenance = d._retirement_expected(baseline)
    revision = d._retirement_baseline_revision(baseline)
    count = d.RETIREMENT_ROUNDS if diagnostic_rounds is None else diagnostic_rounds
    d.require(type(count) is int and 1 <= count <= d.RETIREMENT_ROUNDS, 'ROUND_COUNT', 'invalid diagnostic count')
    session = suite == 'session'
    scenarios = SESSION_SCENARIOS if session else ()
    reference = None
    active = output / 'active'
    active.mkdir()
    if session:
        reference_folder = output / 'ts-reference'
        reference_folder.mkdir()
        reference, ts_folders, ts_attempts = collect_rounds(native, reference_folder, active, baseline, count, scenarios,
                                                             reference=True)
    rows, folders, attempts = collect_rounds(native, output, active, baseline, count, scenarios)
    for side, data in [('native', rows), *([('ts', reference)] if session else [])]:
        for name in ('driver/frames.jsonl', 'driver/methods.json'):
            d.require(len({row['artifactsSha256'][name] for row in data}) == count,
                      'ROUND_EVIDENCE', f'{side} rounds are not independent for {name}')
    coverage = [d.score_retirement_round(row, expected) for row in rows]
    witnesses = [row['nativeWitness'] for row in rows]
    listeners = [d._retirement_listener_verdict(row.get('listeners'), row['ports']['gateway']) for row in rows]
    network = [d._retirement_network_verdict(row.get('connections', {})) for row in rows]
    controls = d._retirement_controls(rows[0], expected)
    control_statuses = [('BLOCKED' if row['status'] == 'NOT_APPLICABLE' else row['status']) for row in controls['classes']]
    status = d.common.aggregate_status('BLOCKED', *(v['status'] for v in coverage + witnesses + listeners + network),
                                       *control_statuses)[0]
    outcome = existing_outcomes(d, output, folders)
    status = d.common.aggregate_status(status, outcome['classification'])[0]
    native.validate_immutable()
    result = {'status': status, 'executionStatus': status, 'acceptanceStatus': 'NOT_RUN', 'k': count,
              'baseline': {'path': str(baseline), 'revision': revision},
              'source': {'head': native.source['head'], 'manifest': str(native.source_manifest),
                         'sha256': native.source_sha},
              'expected': {'methods': expected, 'count': len(expected), 'provenance': provenance},
              'executed': {v['round']: v['executed'] for v in coverage},
              'topology': {'host': 'rust-production-cli', 'sidecar': 'rust', 'identities': native.identities,
                           'externalRoles': ['fixed TS official WS consumer', 'loopback model mock']},
              'rounds': [round_receipt(row) for row in rows], 'portAudit': attempts,
              'methodCoverage': {'status': d.common.aggregate_status(*(v['status'] for v in coverage))[0],
                                 'k': count, 'rounds': coverage},
              'outcomeParity': outcome,
              'hostAudit': {'nativeWitness': witnesses, 'listener': listeners, 'sampledPeers': network,
                            'jsHostRequired': d._retirement_js_host_census()},
              'controls': {**controls, 'status': d.common.aggregate_status(*control_statuses)[0]},
              'cleanupAudit': {'status': d.common.aggregate_status(*('PASS' if d._cleanup_stopped(row['audit'])
                                                               else 'FAIL' for row in rows))[0]},
              'entryInventory': d._retirement_entry_inventory_aggregate(
                  [(row['label'], d._retirement_entry_inventory(baseline, row)) for row in rows]),
              'missingFaces': ['production 150-entry and configuration coverage', 'real native TS-fallback injection',
                               'cold restart and rollback wired to same-head independent comparator'],
              'coldRestartRollback': {'status': 'NOT_RUN', 'rule': 'COLD_RESTART_PENDING'}}
    if not session:
        entries = d._load('retirement_entries', d.SCRIPT / 'retirement_entries.py')
        d._verify_cli_imports()
        result['productionEntryCoverage'] = entries.report_entries(
            d, native, baseline, expected, rows, coverage, outcome, product_capture=product_capture)
        result['status'] = result['executionStatus'] = d.common.aggregate_status(
            result['status'], result['productionEntryCoverage']['status'])[0]
    if cold_capture is not None:
        cold = d._load('retirement_cold', d.SCRIPT / 'retirement_cold.py')
        d._verify_cli_imports()
        try:
            result['coldRestartRollback'] = cold.verify_cold(d, native, cold_capture.resolve(strict=True), baseline)
            result['missingFaces'].remove('cold restart and rollback wired to same-head independent comparator')
            result['status'] = result['executionStatus'] = d.common.aggregate_status(
                result['status'], result['coldRestartRollback']['status'])[0]
        except (d.EvidenceError, OSError, ValueError, KeyError, TypeError) as error:
            result['coldRestartRollback'] = {'status': 'BLOCKED', 'rule': getattr(error, 'rule', 'COLD_EVIDENCE'),
                                             'detail': str(error)}
    if session:
        groups = {name: names.split() for name, names in SESSION_GROUPS.items()}
        methods = [method for names in groups.values() for method in names]
        a7 = [r['method'] for r in d.read(d.ROOT / 'specs/gateway-coverage/coverage-manifest.json')['methods']
              if r['delivery_package'].startswith('A7')]
        d.require(len(a7) == len(set(a7)) == 52 and len(methods) == len(set(methods)) == 50 and
                  set(a7) == set(methods) | {'active_turn_snapshot', 'hook_async_result'},
                  'SESSION_COVERAGE', 'A7 inventory is not the registered 50 plus its two extensions')
        for side, data in [('ts', reference), ('native', rows)]:
            observations_index = list(data)
            for row in data:
                for scene in row.get('scenarios', []):
                    file = Path(scene['folder']) / 'frames.jsonl'
                    if file.is_file():
                        observations_index.append({'label': row['label'] + '/' + scene['case'],
                                                   'frames': rows_jsonl(file)})
            d.save(output / f'{side}-wire-index.json', wire_index(d, observations_index, methods))
        observations, raw, scenario_evidence, scenario_problems = session_faces(d, reference, rows)
        ts_coverage = [d.score_retirement_round(row, expected) for row in reference]
        ts_outcome = existing_outcomes(d, reference_folder, ts_folders)
        session_status, _ = d.common.aggregate_status(
            result['status'], ts_outcome['classification'], *(row['status'] for row in ts_coverage),
            'FAIL' if scenario_problems else 'PASS')
        result['status'] = session_status
        result['executionStatus'] = session_status
        controls_session = None
        if len(raw['ts']) == len(raw['native']) == count and not scenario_problems:
            controls_session = session_injections(d, raw['ts'][0], raw['native'][0], rows[0], expected)
            result['status'] = result['executionStatus'] = d.common.aggregate_status(
                result['status'], controls_session['status'])[0]
        else:
            result['status'] = result['executionStatus'] = d.common.aggregate_status(result['status'], 'BLOCKED')[0]
        session_executed = {v['round']: [m for m in methods if m in v['executed']] for v in coverage}
        result['retirementDriverCoverage'] = {'expected': result['expected'], 'executed': result['executed']}
        result['expected'] = {'methods': methods, 'count': len(methods), 'groups': groups,
                              'provenance': 'approved C/S1-S8 and independent gateway coverage manifest'}
        result['executed'] = session_executed
        result['session'] = {'expected': groups, 'executed': session_executed,
                             'extensions': {'U9': 'read_session_messages', 'U10': 'read_subagent_messages'},
                             'registeredA2ScenariosExecuted': [case for case in SESSION_SCENARIOS[:2]
                                if all(len(scenario_evidence[side][case]) == count for side in ('ts', 'native'))],
                             'scenarioEvidence': scenario_evidence, 'scenarioProblems': scenario_problems,
                             'observations': observations, 'controls': controls_session,
                             'tsReference': {'rounds': [round_receipt(row) for row in reference], 'portAudit': ts_attempts,
                                             'methodCoverage': ts_coverage, 'outcomeParity': ts_outcome},
                             'missingFaces': ['remaining A7 mutation controls (stale binding, double answer, unknown/replay, fake local, hidden host)',
                                              'accepted reconnect answer after replay, lease/expiry and Module identity scenarios',
                                              'non-default configuration and HTTP same owner scenarios',
                                              'host and deadline consumers bound to this invocation']}
    if diagnostic_rounds is not None:
        result['diagnosticOnly'] = True
    return result


def rows_jsonl(path):
    return [json.loads(line) for line in path.read_text().splitlines() if line.strip()]


def raw_session_input(folder):
    transcripts = sorted((folder / 'home/projects').rglob('web:session-domain-u9.jsonl'))
    return {'permission': rows_jsonl(folder / 'scenarios/permission/frames.jsonl'),
            'history': rows_jsonl(folder / 'scenarios/u9-u10-history/frames.jsonl'),
            'transcript': rows_jsonl(transcripts[0]) if len(transcripts) == 1 else [],
            'transcripts': [str(p) for p in transcripts]}


def session_faces(d, reference, native):
    """Authenticate each side's actual scenario evidence before any comparison."""
    observations, raw, executed, problems = {}, {}, {}, []
    for side, rounds in (('ts', reference), ('native', native)):
        observations[side], raw[side] = [], []
        executed[side] = {scene: [] for scene in SESSION_SCENARIOS}
        for row in rounds:
            label = row['label']
            receipts = row.get('scenarios', [])
            if [r.get('case') for r in receipts] != list(SESSION_SCENARIOS):
                problems.append({'side': side, 'round': label, 'rule': 'SESSION_SCENARIO_INVENTORY'})
            for receipt in receipts:
                case = receipt.get('case')
                if case not in executed[side]:
                    continue
                folder = Path(row['folder']) / 'scenarios' / case
                recorded = receipt.get('files', {})
                valid = (type(receipt.get('driverExitCode')) is int and receipt['driverExitCode'] == 0
                         and receipt.get('folder') == str(folder)
                         and all((folder / name).is_file() and recorded.get(name) == sha256(folder / name)
                                 for name in ('frames.jsonl', 'result.json')))
                if valid:
                    executed[side][case].append(label)
                else:
                    problems.append({'side': side, 'round': label, 'case': case,
                                     'rule': 'SESSION_SCENARIO_EVIDENCE', 'driverExitCode': receipt.get('driverExitCode')})
            try:
                evidence = raw_session_input(Path(row['folder']))
                observed = session_observations(d, evidence)
            except (d.EvidenceError, OSError, ValueError, KeyError, TypeError) as error:
                problem = {'side': side, 'round': label, 'rule': getattr(error, 'rule', 'SESSION_EVIDENCE'),
                           'error': str(error)}
                problems.append(problem)
                observations[side].append(problem)
            else:
                raw[side].append(evidence)
                observations[side].append({'round': label, 'evidence': observed})
    return observations, raw, executed, problems


def classify_outcome(code, document):
    if (type(code) is int and code == 0 and isinstance(document, dict)
            and document.get('status') == 'PASS' and document.get('command') == 'check'
            and document.get('violations') == [] and type(document.get('violationsCount')) is int
            and document['violationsCount'] == 0):
        return 'PASS', None
    if type(code) is int and code == 1:
        return 'FAIL', 'OUTCOME_RULE_VIOLATION'
    if type(code) is int and code == 2:
        return 'BLOCKED', 'OUTCOME_INPUT_REJECTED'
    return 'FAIL', 'OUTCOME_EXECUTION_OR_UNREADABLE_REPORT'


def session_observations(d, evidence):
    """The approved A7 ordinary-turn and permission ordering relationships.

    Don't equate the non-final business terminal event with its final WS
    wrapper, or compare request-event sequence to the durable writer sequence.
    """
    permission = evidence['permission']
    history = evidence['history']
    wire_index(d, [{'label': 'permission', 'frames': permission},
                   {'label': 'u9', 'frames': history}], ('permission_decide', 'submit_turn', 'read_session_messages'))
    one = lambda rows: rows[0] if len(rows) == 1 else None
    frames = lambda rows: [(r, r['frame']) for r in rows if isinstance(r.get('frame'), dict)]
    permission_frames = frames(permission)
    decision = one([(r, f) for r, f in permission_frames if r['direction'] == 'send'
                    and f.get('type') == 'request' and f.get('method') == 'permission_decide'])
    d.require(decision is not None, 'SESSION_PERMISSION', 'one real permission decision must be visible')
    request_row, request = decision
    pending_id = request['params']['requestId']
    conn = request_row['connection']
    response = one([(r, f) for r, f in permission_frames if r['connection'] == conn
                    and r['direction'] == 'receive' and f.get('type') == 'response' and f.get('id') == request['id']])
    d.require(response is not None and response[1].get('ok') is True
              and response[1].get('result', {}).get('delivered') is True,
              'SESSION_PERMISSION', 'one accepted decision needs its own real response')
    matching = [(r, f['payload']) for r, f in permission_frames
                if r['connection'] == conn and r['direction'] == 'receive' and f.get('type') == 'notification'
                and f.get('name') == 'permission_changed' and f.get('payload', {}).get('requestId') == pending_id]
    d.require([p.get('type') for _, p in matching] == ['requested', 'resolved']
              and matching[0][0]['seq'] < request_row['seq'] < matching[1][0]['seq'] < response[0]['seq']
              and all(p.get('sessionKey') == request['params']['sessionKey'] for _, p in matching),
              'SESSION_NOTIFICATIONS', 'permission requested/resolved were lost, reordered, or assigned to another owner')
    history_frames = frames(history)
    submit = one([(r, f) for r, f in history_frames if r['direction'] == 'send'
                  and f.get('type') == 'request' and f.get('method') == 'submit_turn'])
    d.require(submit is not None, 'SESSION_TERMINAL', 'missing one ordinary live U9 submit')
    first, frame = submit
    events = [(r, f) for r, f in history_frames if r['connection'] == first['connection'] and r['direction'] == 'receive'
              and f.get('type') == 'event' and f.get('id') == frame['id']]
    finals = [(r, f) for r, f in events if f.get('final') is True]
    d.require(len(finals) == 1 and finals[-1] == events[-1] and finals[0][1].get('event', {}).get('type') == 'turn_completed',
              'SESSION_TERMINAL', 'ordinary submit requires one final WS terminal as the last event')
    seq = [f.get('seq') for _, f in events]
    d.require(all(type(x) is int for x in seq) and seq == list(range(len(seq))),
              'SESSION_WIRE', 'request-local stream sequence has a gap or duplicate')
    run_id = frame['params']['runId']
    durable = [r for r in evidence['transcript'] if r.get('type') == 'turn_result' and r.get('turnId') == run_id]
    d.require(len(evidence['transcripts']) == 1 and len(durable) == 1,
              'SESSION_DURABLE_TERMINAL', 'ordinary submit must retain exactly one durable terminal')
    return {'permissionRequestId': pending_id, 'notificationWirePointers': [r['seq'] for r, _ in matching],
            'requestStreamSequence': seq, 'durableTerminalSequence': durable[0]['sequence'],
            'durableTranscript': evidence['transcripts'][0]}


def session_injections(d, ts, native, healthy_round, expected):
    """Run injections on copies of actual positives and attribute the rule."""
    def rule(data):
        try:
            session_observations(d, data)
        except d.EvidenceError as error:
            return error.rule
        return None

    for original in (ts, native):
        if rule(original) is not None:
            return {'status': d.common.aggregate_status('BLOCKED')[0], 'cases': [],
                    'reason': 'existing invalid original cannot carry a negative-control positive'}

    def without_notification(original):
        filtered = [r for r in original['permission'] if not (r.get('frame', {}).get('name') == 'permission_changed'
                   and r['frame'].get('payload', {}).get('type') == 'resolved')]
        # Re-index collector-local sequence to make the independent semantic
        # oracle, rather than the generic missing-row check, kill this fault.
        return {**original, 'permission': [{**r, 'seq': i} for i, r in enumerate(filtered, 1)]}

    terminal = next(r for r in native['transcript'] if r.get('type') == 'turn_result')
    duplicate = {**native, 'transcript': [*native['transcript'], dict(terminal)]}
    empty = d.score_retirement_round({**healthy_round, 'outcomes': []}, expected)
    tests = [
        ('missing-notification', 'SESSION_NOTIFICATIONS', [rule(without_notification(native))]),
        ('duplicate-durable-terminal', 'SESSION_DURABLE_TERMINAL', [rule(duplicate)]),
        ('both-sides-identical-bad', 'SESSION_NOTIFICATIONS', [rule(without_notification(ts)),
                                                               rule(without_notification(native))]),
        ('empty-coverage', 'MISSING_GROUP', [r['rule'] for r in empty['rules'] if r['detail'] == 'outcomes']),
    ]
    results = [{'case': name, 'expectedRule': expected_rule, 'firedRules': actual,
                'killed': bool(actual) and all(fired == expected_rule for fired in actual)}
               for name, expected_rule, actual in tests]
    status, _ = d.common.aggregate_status(*('PASS' if result['killed'] else 'FAIL' for result in results))
    return {'status': status, 'cases': results}
