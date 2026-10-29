"""D0 recording contract; deliberately independent of the frozen rulers."""
from contextlib import contextmanager
import fcntl
import hashlib
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import time
import uuid

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = Path(__file__).resolve().parent
HEAD = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85'
LOCK = Path('/tmp/pdrs-parity.lock')
TOKEN = 'd0-fixture-not-a-secret'
# #933: the only loopback port mock-gateway scenarios may reach. Matches the
# policy.py allow rule and the environment() gateway URLs; never 18789.
MOCK_PORT = 28999
DIRS = ('home', 'pilot', 'project', 'tmp', 'bin', 'xdg-config', 'xdg-cache', 'xdg-data', 'xdg-state')
FILES = {'pilot/pilotdeck.yaml': b'schemaVersion: 1\n', 'project/keep.txt': b'D0 fixture\x00\xff\n',
         'openssl.cnf': b''}
OPENSSL_CONFIG = {'path': 'openssl.cnf', 'sha256': hashlib.sha256(b'').hexdigest()}
# Registered suites keep each batch inside the existing 30-scenario lease.
SUITE = os.environ.get('PDRS_CLI_PARITY_SUITE', 'default')
if SUITE not in ('default', 'chat-search'):
    raise ValueError(f'unknown CLI parity suite: {SUITE}')
SCENARIOS = json.loads((SCRIPT / ('chat-search.json' if SUITE == 'chat-search' else 'scenarios.json')).read_text())


class Blocked(RuntimeError):
    pass


def per_stream_scenarios(scenarios):
    """Validate the registry before allowing any cross-stream order exception."""
    declared = set()
    for scenario in scenarios:
        if 'streamOrder' not in scenario and 'streamOrderReason' not in scenario:
            continue
        order, reason = scenario.get('streamOrder'), scenario.get('streamOrderReason')
        if not isinstance(order, str) or order != 'per-stream':
            raise Blocked(f"apparatus.stream-order: {scenario['id']}: streamOrder must be 'per-stream'")
        if not isinstance(reason, str) or not reason.strip():
            raise Blocked(f"apparatus.stream-order: {scenario['id']}: streamOrderReason must be a nonempty string")
        declared.add(scenario['id'])
    return declared


def mock_gateway_scenarios(scenarios):
    """Validate the #933 mock-gateway marker before any profile is relaxed.

    The marker must be exactly the boolean True: a truthy string or 1 would
    silently widen which scenarios get the loopback allow rule.
    """
    marked = set()
    for scenario in scenarios:
        if 'mockGateway' not in scenario:
            continue
        if scenario['mockGateway'] is not True:
            raise Blocked(f"apparatus.mock-gateway: {scenario['id']}: mockGateway must be the boolean True")
        marked.add(scenario['id'])
    return marked


# 已启动、尚未被 stop_group 证实全部停止的进程组（pgid）。锁是否可释放只看它是否为空，不看异常类型。
LIVE_GROUPS: set[int] = set()


class CleanupBlocked(Blocked):
    """An owned process may remain: do not hand the shared lock to another run."""


def save(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + '\n')


def digest(path):
    h = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            h.update(chunk)
    return h.hexdigest()


@contextmanager
def open_plain(path):
    """Pin a plain evidence file before reading; never follow a swapped leaf."""
    if path.absolute() != path.resolve():
        raise ValueError(f'evidence path contains symlink or traversal: {path}')
    before = path.lstat()
    if not stat.S_ISREG(before.st_mode) or before.st_nlink != 1:
        raise ValueError(f'not a plain, single-link evidence file: {path}')
    # NONBLOCK also prevents a FIFO swapped in before open from hanging the
    # verifier; the descriptor type is checked before any bytes are read.
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, 'rb') as stream:
        current = os.fstat(stream.fileno())
        if (not stat.S_ISREG(current.st_mode) or current.st_nlink != 1
                or (current.st_dev, current.st_ino) != (before.st_dev, before.st_ino)):
            raise ValueError(f'evidence file changed before open: {path}')
        yield stream


def digest_plain(path):
    h = hashlib.sha256()
    with open_plain(path) as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            h.update(chunk)
    return h.hexdigest()


def read_plain(path):
    with open_plain(path) as stream:
        return stream.read()


def hashes():
    paths = [p for p in SCRIPT.iterdir() if p.is_file() and p.suffix in ('.py', '.mjs', '.c', '.txt', '.md', '.json')]
    paths += list((ROOT / 'specs/cli-parity').glob('*.md'))
    return {p.relative_to(ROOT).as_posix(): digest(p) for p in sorted(paths)}


def baseline_path(explicit=None, environ=None):
    env = os.environ if environ is None else environ
    value = explicit if explicit is not None else (env.get('PDRS_BASELINE') or ROOT / 'resources/PilotDeck')
    path = Path(value).expanduser().resolve()
    if not (path / 'dist/src/cli/pilotdeck.js').is_file():
        raise Blocked(f'baseline missing: {path}; use --baseline or PDRS_BASELINE')
    return path


def output_path(path):
    path = Path(path).absolute()
    # Reject links, including existing parents; never write through a redirect.
    if path != path.resolve() or not path.is_relative_to(ROOT / '_tmp') or path == ROOT / '_tmp':
        raise Blocked('output must be a non-symlink child of this checkout _tmp')
    if path.exists():
        raise Blocked(f'refusing to overwrite evidence: {path}')
    return path


def fixture_relative(name):
    parts = Path(name).parts
    if not parts or Path(name).is_absolute() or '..' in parts or name != Path(name).as_posix():
        raise ValueError(f'invalid fixture path: {name}')
    return Path(name)


def fixture(path, scenario=None, baseline=None):
    scenario = scenario or {}
    initial_tree(scenario)  # Validate every extra path before creating the runtime.
    path.mkdir(mode=0o700)
    for name in DIRS:
        (path / name).mkdir(mode=0o700)
    files = dict(FILES)
    for name, text in scenario.get('fixtureFiles', {}).items():
        fixture_relative(name)
        if name in files:
            raise ValueError(f'fixture cannot replace base file: {name}')
        files[name] = text.encode()
    for name, blob in scenario.get('fixtureBlobs', {}).items():
        fixture_relative(name)
        fixture_relative(blob['path'])
        if name in files or baseline is None or len(blob['revision']) != 40 or any(c not in '0123456789abcdef' for c in blob['revision']):
            raise ValueError(f'invalid fixture blob: {name}')
        # Read the pinned historical blob only; never check out or modify baseline.
        data = subprocess.check_output(['/usr/bin/git', '-C', str(baseline), 'show',
                                        blob['revision'] + ':' + blob['path']],
                                       env={'PATH': '/usr/bin:/bin', 'GIT_CONFIG_NOSYSTEM': '1',
                                            'GIT_CONFIG_GLOBAL': '/dev/null'}, timeout=10)
        if len(data) != blob['size'] or hashlib.sha256(data).hexdigest() != blob['sha256']:
            raise ValueError(f'fixture blob identity changed: {name}')
        files[name] = data
    for name, data in files.items():
        dest = path / fixture_relative(name)
        for parent in reversed(dest.relative_to(path).parents[:-1]):
            (path / parent).mkdir(mode=0o700, exist_ok=True)
        dest.write_bytes(data)
        dest.chmod(0o600)


def snapshot(root):
    """Inventory links without following them; hash regular files through a pinned fd."""
    if root.is_symlink() or not root.is_dir():
        raise ValueError('runtime root missing or symlink')
    result = {'.': {'type': 'directory', 'mode': stat.S_IMODE(root.stat().st_mode)}}
    def visit(folder):
        for path in sorted(folder.iterdir()):
            info = path.lstat()
            row = {'mode': stat.S_IMODE(info.st_mode)}
            if stat.S_ISLNK(info.st_mode):
                target = os.readlink(path)
                if Path(target).is_absolute() or not path.resolve().is_relative_to(root.resolve()):
                    raise ValueError(f'outside symlink: {path.relative_to(root)}')
                row.update(type='symlink', target=target)
            elif stat.S_ISDIR(info.st_mode):
                row['type'] = 'directory'
                visit(path)
            elif stat.S_ISREG(info.st_mode):
                if info.st_nlink != 1:
                    raise ValueError(f'hardlink: {path.relative_to(root)}')
                row.update(type='file', size=info.st_size, sha256=digest_plain(path))
            else:
                raise ValueError(f'special node: {path.relative_to(root)}')
            result[path.relative_to(root).as_posix()] = row
    visit(root)
    return dict(sorted(result.items()))


def staged_skills_identity(folder):
    """The staged copy is self-contained: no links or external dependencies."""
    if folder.absolute() != folder.resolve():
        raise ValueError(f'staged skills path contains symlink or traversal: {folder}')
    manifest = snapshot(folder)
    if any(row['type'] == 'symlink' for row in manifest.values()):
        raise ValueError(f'staged skills contains a symlink: {folder}')
    digest = hashlib.sha256(json.dumps(manifest, sort_keys=True, separators=(',', ':')).encode()).hexdigest()
    return {'path': str(folder), 'sha256': digest}


def initial_tree(scenario=None):
    scenario = scenario or {}
    result = {name: {'mode': 0o700, 'type': 'directory'} for name in DIRS}
    result['.'] = {'mode': 0o700, 'type': 'directory'}
    result.update({name: {'mode': 0o600, 'type': 'file', 'size': len(data),
                         'sha256': hashlib.sha256(data).hexdigest()} for name, data in FILES.items()})
    extra = {name: {'size': len(text.encode()), 'sha256': hashlib.sha256(text.encode()).hexdigest()}
             for name, text in scenario.get('fixtureFiles', {}).items()}
    if extra.keys() & scenario.get('fixtureBlobs', {}).keys():
        raise ValueError('duplicate fixture file/blob')
    extra.update({name: {'size': blob['size'], 'sha256': blob['sha256']}
                  for name, blob in scenario.get('fixtureBlobs', {}).items()})
    for name, identity in extra.items():
        rel = fixture_relative(name)
        if name in result:
            raise ValueError(f'fixture cannot replace base entry: {name}')
        for parent in rel.parents:
            key = parent.as_posix()
            existing = result.get(key)
            if existing and existing['type'] != 'directory':
                raise ValueError(f'fixture file used as directory: {key}')
            result[key] = {'mode': 0o700, 'type': 'directory'}
        result[name] = {'mode': 0o600, 'type': 'file', **identity}
    return dict(sorted(result.items()))


def environment(runtime, scenario):
    paths = {'HOME': 'home', 'USERPROFILE': 'home', 'CFFIXED_USER_HOME': 'home',
             'PILOT_HOME': 'pilot', 'PILOTDECK_CONFIG_PATH': 'pilot/pilotdeck.yaml',
             'XDG_CONFIG_HOME': 'xdg-config', 'XDG_CACHE_HOME': 'xdg-cache',
             'XDG_DATA_HOME': 'xdg-data', 'XDG_STATE_HOME': 'xdg-state',
             'TMPDIR': 'tmp', 'TMP': 'tmp', 'TEMP': 'tmp', 'PWD': 'project', 'PATH': 'bin',
             'OPENSSL_CONF': 'openssl.cnf'}
    env = {key: str(runtime / value) for key, value in paths.items()}
    env.update(LANG='C', LC_ALL='C', TZ='UTC', TERM='dumb', NO_COLOR='1',
               ANALYTICS_ENABLED='0', DOCKER_CONTAINER='1', COMMIT_HASH=HEAD,
               # #933: all four gateway coordinates derive from MOCK_PORT, the
               # same constant the sandbox allow rule is built from.
               PILOTDECK_GATEWAY_PORT=str(MOCK_PORT), PILOTDECK_GATEWAY_HOST='127.0.0.1',
               PILOTDECK_GATEWAY_URL=f'ws://127.0.0.1:{MOCK_PORT}/ws',
               PILOTDECK_SDK_GATEWAY_URL=f'ws://127.0.0.1:{MOCK_PORT}/ws')
    if scenario['token']:
        env['PILOTDECK_SDK_AUTH_TOKEN'] = TOKEN
    # Scenario-specific variables naming a path under the runtime; they may add
    # variables but never replace an isolation variable above.
    for key, relative in scenario.get('envPaths', {}).items():
        parts = Path(relative).parts
        # Tokens are controlled only by the scenario's `token` bit, even when unset.
        if key in env or key.endswith('TOKEN') or not parts or Path(relative).is_absolute() or '..' in parts:
            raise ValueError(f'invalid scenario envPaths entry: {key}')
        env[key] = str(runtime / relative)
    return env


def env_summary(env):
    return {key: ({'redacted': True, 'length': len(value),
                   'sha256': hashlib.sha256(value.encode()).hexdigest()} if 'TOKEN' in key else value)
            for key, value in sorted(env.items())}


# #1064: .guard 只在 mkdir/owner 发布/释放的临界区内被兄弟短暂持有；卡死在临界区里的
# 兄弟进程不能让任何调用方的截止期失效。60s 远大于该临界区、远小于 run_round 的
# 2280s 租约闹钟，也是 negative_process 给同型 guard 的有界上限。
GUARD_TIMEOUT = 60
GUARD_INTERVAL = 0.05


@contextmanager
def lock_guard(lock, timeout=GUARD_TIMEOUT):
    path = lock.with_name(lock.name + '.guard')
    fd = os.open(path, os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, 'r+') as guard:
        info = os.fstat(guard.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or info.st_uid != os.getuid():
            raise Blocked('unsafe parity guard')
        # 阻塞 flock 本身没有截止期：LOCK_NB + 轮询让调用方的任何期限都真实生效
        # （#1064 P1-1），超时抛既有 Blocked，锁目录从不因超时被改动。
        deadline = time.monotonic() + timeout
        while True:
            try:
                fcntl.flock(guard, fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except BlockingIOError:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise Blocked(f'parity guard busy: {path}') from None
                time.sleep(min(GUARD_INTERVAL, remaining))
        yield


def discard_unpublished_lease(lock, mine, identity, owner_identity, written):
    """#1064: 回收本次获取中已 mkdir 但从未发布的租约目录（获取被取消/写入失败）。

    只删归属三证俱全的目录：目录 inode 与本次捕获的 identity 一致、owner.json
    的 inode 与本次 O_EXCL 创建时捕获的一致、且 owner 磁盘字节逐字节等于本次
    已证写入的前缀（written）。identity 为 None（mkdir 未执行/未返回、或身份
    尚未捕获）一律保留——「目录为空」不是本次创建的证明（r1 P1-1）；owner 字节
    解析失败或内容不是本次所写也不是（r1 P1-2：`null\\n`、损坏 JSON、外来前缀
    形态字节均保留）。mine 是调用方当次租约身份，供保留目录的人工排查关联；
    回收判定本身只用字节级证明。清理自身的 OSError 一律吞掉（best-effort），
    不掩盖触发清理的原始异常。
    """
    try:
        current = lock.lstat()
    except OSError:
        return
    if not stat.S_ISDIR(current.st_mode):
        return
    if identity is None or (current.st_dev, current.st_ino) != (identity.st_dev, identity.st_ino):
        return
    try:
        entries = set(os.listdir(lock))
    except OSError:
        return
    if entries - {'owner.json'}:
        return
    if 'owner.json' in entries:
        if owner_identity is None:
            return
        try:
            owner = (lock / 'owner.json').lstat()
        except OSError:
            return
        if (owner.st_dev, owner.st_ino) != (owner_identity.st_dev, owner_identity.st_ino):
            return
        try:
            on_disk = (lock / 'owner.json').read_bytes()
        except OSError:
            return
        if on_disk != written:
            return
        try:
            (lock / 'owner.json').unlink()
        except OSError:
            return
    try:
        lock.rmdir()
    except OSError:
        pass


@contextmanager
def parity_lock(lock=LOCK, timeout=GUARD_TIMEOUT):
    """Cooperate with existing .guard + mkdir contract; never steal a lock."""
    mine = {'pid': os.getpid(), 'timestamp': time.time(), 'token': uuid.uuid4().hex}
    identity = None
    directory = None
    try:
        with lock_guard(lock, timeout):
            # 「是否本次创建」以 identity 是否捕获为准：identity 只在 lock.mkdir()
            # 正常返回之后赋值，mkdir 未执行/未返回/身份未捕获时保持 None，回收
            # 一律不进（r1 P1-1）；赋值与 lstat 之间的极窄信号窗口同样保守保留。
            owner_identity = None
            written = None
            try:
                try:
                    lock.mkdir(mode=0o700)
                except FileExistsError as error:
                    # 只接 mkdir：目录属于他人/前驱时只报既有 Blocked，绝不回收
                    # （r1 P2-1：owner.json 的 O_EXCL 冲突不在此子句，走下面的
                    # 回收分支并关闭目录 fd，FileExistsError 原样上抛）。
                    raise Blocked(f'parity lock held: {lock}') from error
                identity = lock.lstat()
                directory = os.open(lock, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
                current = os.fstat(directory)
                if (identity.st_dev, identity.st_ino) != (current.st_dev, current.st_ino) or current.st_uid != os.getuid():
                    raise Blocked('lock directory changed before ownership write')
                payload = json.dumps(mine).encode()
                fd = os.open('owner.json', os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600, dir_fd=directory)
                try:
                    owner_identity = os.fstat(fd)
                    written = b''
                    view = memoryview(payload)
                    while view:
                        view = view[os.write(fd, view):]
                    # 循环完整走完，payload 已全部落盘且计数可证；循环中被取消时
                    # written 停在上一次已证前缀，磁盘多出的字节按外来保留。
                    written = payload
                finally:
                    os.close(fd)
            except BaseException:
                # 只回收归属三证俱全的目录，否则原样保留并上抛原异常；先回收再关
                # fd：清理途中若再来一次中断，至多丢 fd/掩盖原异常的链，不会因关
                # fd 的顺序跳过回收本身。
                discard_unpublished_lease(lock, mine, identity, owner_identity, written)
                if directory is not None:
                    os.close(directory)
                    directory = None
                raise
        yield mine
    finally:
        # 获取从未完成（Blocked/未执行）或取消回收分支已关掉 fd 并清空时（directory
        # 为 None），没有可释放的租约，也不吞正在传播的异常。本 finally 从进入
        # lock_guard 之前就开始覆盖：guard 临界区退出与 yield 之间的取消同样走
        # 释放路径，不留无回收窗口（r1 concurrency P2-1），目录 fd 也不会泄漏。
        if directory is not None:
            # 不论正常还是任何异常（含 KeyboardInterrupt）：只有本次启动的进程组全部被 stop_group 证实停止，才释放锁
            release = not LIVE_GROUPS
            try:
                # 释放阶段同样有界（#1064 P1-1）：卡死的兄弟不能让清理无限期阻塞；
                # 超时抛 Blocked、lease 原样保留，交给 README 的人工流程。
                with lock_guard(lock, timeout):
                    current = lock.lstat()
                    if (identity.st_dev, identity.st_ino) != (current.st_dev, current.st_ino):
                        raise Blocked('lock directory changed; retaining lock')
                    fd = os.open('owner.json', os.O_RDONLY | os.O_NOFOLLOW, dir_fd=directory)
                    with os.fdopen(fd) as owner:
                        if json.load(owner) != mine:
                            raise Blocked('lock ownership changed; retaining lock')
                    if release:
                        os.unlink('owner.json', dir_fd=directory)
                        lock.rmdir()
            finally:
                os.close(directory)
