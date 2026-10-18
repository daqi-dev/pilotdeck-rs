"""Git inventory admission and hash-checked absolute Python source loading.

Bootstraps verify this file against FROZEN (or HEAD if absent) under python -I.
Local source never enters sys.path and bytecode is never read or written.
"""
import os
import sys
import hashlib
import importlib.abc
import importlib.util
from pathlib import Path
import re
import shutil
import stat
import subprocess

SPEC_FILES = ('acceptance.md', 'normalization.md', 'accepted-diffs.md', 'multimodal.md',
              'rust-routes.json', 'human-approvers.txt')
# All git-tracked exp008 files are apparatus, including its other entrypoints.
EXP008_FILES = ('compare.py', 'controls.py', 'driver.mjs', 'enumerate_hook.mjs',
                'preload.mjs', 'proxy/Cargo.lock', 'proxy/Cargo.toml', 'proxy/src/main.rs',
                'proxy/tests/node-ws-semantics.cjs', 'reconcile_routes.py', 'run.py', 'vite.config.mjs')
APPARATUS_PATHS = ('scripts/ui-server-parity', 'scripts/exp008', 'specs/ui-server-parity')
TOOLCHAIN_FILES = ('rust-toolchain', 'rust-toolchain.toml')
run_tool = subprocess.run


class ApparatusUnsafe(ValueError):
    pass


def tracked_files(directory):
    directory = Path(directory).absolute()
    root = directory.parents[1]
    try:
        names = subprocess.check_output(['git', '-C', str(root), 'ls-files', '-z', '--',
                                         str(directory.relative_to(root))], stderr=subprocess.PIPE)
    except (OSError, subprocess.CalledProcessError) as error:
        raise ApparatusUnsafe('apparatus git inventory unavailable: ' + str(directory)) from error
    return {str(root / os.fsdecode(name)) for name in names.split(b'\0') if name}


def scan(directory):
    """Directory files must exactly equal git's tracked inventory; never follow links."""
    directory = os.path.abspath(directory)
    errors, files = [], []
    if os.path.islink(directory):
        raise ApparatusUnsafe('apparatus symlink: ' + directory)
    if not os.path.isdir(directory):
        raise ApparatusUnsafe('apparatus directory missing: ' + directory)
    for parent, directories, names in os.walk(directory, followlinks=False,
                                              onerror=lambda error: errors.append(str(error))):
        for name in directories + names:
            path = os.path.join(parent, name)
            if name == '.DS_Store' and not stat.S_ISREG(os.lstat(path).st_mode):
                errors.append('nonregular .DS_Store: ' + path)
            if os.path.islink(path):
                errors.append('apparatus symlink: ' + path)
            if name == '__pycache__' or name.endswith('.pyc'):
                errors.append('forbidden Python cache: ' + path)
        for name in names:
            path = os.path.join(parent, name)
            if name == '.DS_Store' and stat.S_ISREG(os.lstat(path).st_mode):
                continue
            if stat.S_ISREG(os.lstat(path).st_mode):
                files.append(path)
            else:
                errors.append('apparatus nonregular file: ' + path)
    registered = tracked_files(directory)
    registered = {p for p in registered if Path(p).name != '.DS_Store'}
    errors.extend('untracked apparatus file: ' + path for path in sorted(set(files) - registered))
    errors.extend('missing tracked apparatus file: ' + path for path in sorted(registered - set(files)))
    if errors:
        raise ApparatusUnsafe('; '.join(sorted(errors)))
    return sorted(files)


def git_admission(root, expected_head=None):
    """Calibration must start/resume on the same clean apparatus checkout."""
    root = Path(root)
    scan(root / 'scripts/ui-server-parity')
    scan(root / 'scripts/exp008')
    toolchain_files(root)
    status = subprocess.check_output(['git', '-C', str(root), 'status', '--porcelain',
                                     '--untracked-files=all', '-z', '--', *APPARATUS_PATHS,
                                     *TOOLCHAIN_FILES])
    # Exact basename + regular file only; never exempt links/directories.
    records = status.split(b'\0')
    status = b'\0'.join(record for record in records if record and not
                       (Path(os.fsdecode(record[3:])).name == '.DS_Store'
                        and os.path.lexists(root / os.fsdecode(record[3:]))
                        and stat.S_ISREG((root / os.fsdecode(record[3:])).lstat().st_mode)))
    if status:
        raise ApparatusUnsafe('dirty apparatus git status: ' + os.fsdecode(status))
    head = subprocess.check_output(['git', '-C', str(root), 'rev-parse', 'HEAD'], text=True).strip()
    if expected_head is not None and head != expected_head:
        raise ApparatusUnsafe('batch HEAD changed; start a NEW batch')
    return head


def toolchain_files(root):
    """Optional root toolchain selectors must be tracked regular files."""
    root = Path(root)
    tracked = subprocess.check_output(['/usr/bin/git', '-C', str(root), 'ls-files', '-z', '--',
                                       *TOOLCHAIN_FILES]).split(b'\0')
    files = []
    for name in TOOLCHAIN_FILES:
        path = root / name
        present = os.path.lexists(path)
        if present != (os.fsencode(name) in tracked):
            raise ApparatusUnsafe('untracked/missing toolchain input: ' + str(path))
        if present:
            if not stat.S_ISREG(path.lstat().st_mode):
                raise ApparatusUnsafe('nonregular toolchain input: ' + str(path))
            files.append(path)
    return files


def toolchain_selector(root):
    """Only a tracked root selector with a numbered release is admissible."""
    import tomllib
    files = toolchain_files(root)
    if len(files) != 1:
        raise ApparatusUnsafe('exactly one tracked root Rust toolchain selector required')
    path = files[0]
    contents = path.read_text().strip()
    if path.name.endswith('.toml') or contents.startswith('['):
        table = tomllib.loads(contents).get('toolchain', {})
        if 'path' in table:
            raise ApparatusUnsafe('root Rust toolchain path= forbidden: ' + str(path))
        channel = table.get('channel')
    else:
        channel = contents
    if not isinstance(channel, str) or not re.fullmatch(r'[0-9]+\.[0-9]+(?:\.[0-9]+)?', channel):
        raise ApparatusUnsafe('root Rust toolchain must declare a numbered release')
    return channel


def rust_toolchain_admission(cwd, home, root=None):
    root = Path(root or Path(__file__).resolve().parents[2]).resolve()
    allowed = set(toolchain_files(root))
    for start in (Path.cwd(), Path(cwd), Path(home)):
        # Check lexical and real ancestors; aliases must not hide a selector.
        for origin in (start.absolute(), start.resolve()):
            for directory in (origin, *origin.parents):
                for name in TOOLCHAIN_FILES:
                    path = directory / name
                    if os.path.lexists(path) and path not in allowed:
                        raise ApparatusUnsafe('Rust ancestor toolchain forbidden: ' + str(path))
    return toolchain_selector(root)


def cargo_config_admission(cwd, home, root=None):
    """Cargo searches cwd ancestors plus CARGO_HOME; reject before any process."""
    channel = rust_toolchain_admission(cwd, home, root)
    for directory in set([Path(cwd).absolute(), *Path(cwd).absolute().parents,
                          Path(home).absolute(), *Path(home).absolute().parents]):
        if directory.is_symlink():
            raise ApparatusUnsafe('Cargo ancestor symlink: ' + str(directory))
        for name in ('.cargo/config', '.cargo/config.toml'):
            if os.path.lexists(directory / name):
                raise ApparatusUnsafe('Cargo ancestor config forbidden: ' + str(directory / name))
    for name in ('config', 'config.toml', 'credentials', 'credentials.toml'):
        if os.path.lexists(Path(home) / name):
            raise ApparatusUnsafe('Cargo home config/credentials forbidden: ' + str(Path(home) / name))
    return channel


def cargo_environment(cwd, home, target, root=None):
    channel = cargo_config_admission(cwd, home, root)
    # Trust the installed host rustup, never a repository or inherited PATH tool.
    import pwd
    user_home = Path(pwd.getpwuid(os.getuid()).pw_dir)
    tools = user_home / '.cargo/bin'
    if not (tools / 'rustup').is_file():
        raise ApparatusUnsafe('installed rustup missing: ' + str(tools))
    # An allowlist also removes linker/compiler, loader and shell injection vars.
    return {'HOME': str(user_home), 'PATH': os.pathsep.join(map(str, [tools, '/usr/bin', '/bin', '/usr/sbin', '/sbin'])),
            'CARGO_HOME': str(home), 'CARGO_TARGET_DIR': str(target),
            'RUSTUP_HOME': str(user_home / '.rustup'), 'RUSTUP_AUTO_INSTALL': '0',
            'RUSTUP_TOOLCHAIN': channel}


def prepare_cargo(cwd, home, target, seed, root=None):
    cargo_config_admission(cwd, home, root)
    target.mkdir(exist_ok=False)
    home.mkdir(exist_ok=False)
    for path in (Path(seed), Path(seed) / 'registry'):
        if path.is_symlink():
            raise ApparatusUnsafe('Cargo cache seed directory symlink: ' + str(path))
    # Do not copy unpacked code, git checkouts, configuration or credentials.
    for name in ('index', 'cache'):
        source = Path(seed) / 'registry' / name
        if not os.path.lexists(source):
            continue
        if not source.is_dir() or source.is_symlink():
            raise ApparatusUnsafe('nonregular Cargo cache seed: ' + str(source))
        for parent, dirs, names in os.walk(source, followlinks=False):
            for path in [Path(parent), *(Path(parent) / n for n in dirs + names)]:
                mode = path.lstat().st_mode
                if not (stat.S_ISREG(mode) or stat.S_ISDIR(mode)):
                    raise ApparatusUnsafe('nonregular Cargo cache seed: ' + str(path))
        shutil.copytree(source, home / 'registry' / name)
    return cargo_environment(cwd, home, target, root)


def cargo_process(command, cwd, home, target, *, root=None, **kwargs):
    env = cargo_environment(cwd, home, target, root)
    if '--manifest-path' in command:
        cargo_archive_admission(home, Path(command[command.index('--manifest-path') + 1]))
    return run_tool(command, cwd=cwd, env=env, check=True, **kwargs)


def cargo_archive_admission(home, manifest):
    """Cargo trusts cached .crate files in some offline paths; check them ourselves."""
    import tomllib
    lock = tomllib.loads(manifest.with_name('Cargo.lock').read_text())
    for package in lock.get('package', []):
        source = package.get('source')
        if source is None:
            continue  # local, git-admitted manifest/source
        if source != 'registry+https://github.com/rust-lang/crates.io-index':
            raise ApparatusUnsafe('unsupported Cargo dependency source: ' + source)
        name = package['name'] + '-' + package['version'] + '.crate'
        expected = package.get('checksum')
        candidates = list((Path(home) / 'registry/cache').glob('*/' + name))
        if not expected or not candidates:
            raise ApparatusUnsafe('locked Cargo archive absent/checksum missing: ' + name)
        for archive in candidates:
            if not stat.S_ISREG(archive.lstat().st_mode):
                raise ApparatusUnsafe('nonregular locked Cargo archive: ' + str(archive))
            with archive.open('rb') as data:
                actual = hashlib.file_digest(data, 'sha256').hexdigest()
            if actual != expected:
                raise ApparatusUnsafe('Cargo.lock archive checksum mismatch: ' + str(archive))


def cargo_identity(cwd, home, target, root=None):
    versions = {}
    for name, args in (('rustc', ['-vV']), ('cargo', ['-V'])):
        versions[name] = cargo_process([name, *args], cwd, home, target,
                                      root=root, capture_output=True, text=True).stdout.strip()
    channel = rust_toolchain_admission(cwd, home, root)
    release = re.search(r'^release: (.+)$', versions['rustc'], re.M)
    # rustup accepts major.minor selectors; patches resolve within that series.
    expected = re.escape(channel) + (r'\.[0-9]+' if channel.count('.') == 1 else '')
    if not release or re.fullmatch(expected, release[1]) is None:
        raise ApparatusUnsafe('rustc release differs from root declaration: ' + channel)
    return versions


def manifest_pins(root):
    root = Path(root).resolve()
    folder = root / 'specs/ui-server-parity'
    manifest = folder / 'FROZEN.md'
    pins = {}
    if manifest.is_file():
        for digest, name in re.findall(r'^([0-9a-f]{64})  ([^\s]+)\s*$', manifest.read_text(), re.M):
            path = (folder / name).resolve()
            if path in pins or not path.is_relative_to(root):
                raise ApparatusUnsafe('duplicate/escaping apparatus hash path: ' + name)
            pins[path] = digest
    return pins


def verified_source(path, root, pins):
    path, root = Path(path).absolute(), Path(root).resolve()
    if path.is_symlink() or not path.is_relative_to(root):
        raise ApparatusUnsafe('unsafe source path: ' + str(path))
    data = path.read_bytes()
    if (root / 'specs/ui-server-parity/FROZEN.md').is_file():
        expected = pins.get(path)
        if not expected or hashlib.sha256(data).hexdigest() != expected:
            raise ApparatusUnsafe('unregistered/changed source hash: ' + str(path))
    else:
        # Preserve missing-manifest DRAFT, but admit only committed source.
        expected = subprocess.check_output(['git', '-C', str(root), 'show',
                                            'HEAD:' + str(path.relative_to(root))])
        if data != expected:
            raise ApparatusUnsafe('source differs from HEAD without manifest: ' + str(path))
    return data


class SourceLoader(importlib.abc.Loader):
    def __init__(self, path, root, pins):
        self.path, self.root, self.pins = path, root, pins

    def create_module(self, spec):
        return None

    def exec_module(self, module):
        data = verified_source(self.path, self.root, self.pins)
        exec(compile(data, str(self.path), 'exec'), module.__dict__)


def load_source(name, path):
    root = Path(__file__).resolve().parents[2]
    path = Path(path).absolute()
    loader = SourceLoader(path, root, manifest_pins(root))
    spec = importlib.util.spec_from_file_location(name, path, loader=loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


class ApparatusFinder(importlib.abc.MetaPathFinder):
    def __init__(self, directory, pins):
        self.directory = Path(directory)
        self.root = self.directory.parents[1]
        self.pins = pins
        self.files = set(scan(directory))

    def find_spec(self, fullname, path=None, target=None):
        if fullname == 'controls':
            # An explicit namespace package; never search sys.path for it.
            spec = importlib.util.spec_from_loader(fullname, loader=None, is_package=True)
            spec.submodule_search_locations = []
            return spec
        relative = fullname.replace('.', '/') + '.py'
        source = self.directory / relative
        if str(source) not in self.files:
            return None
        return importlib.util.spec_from_file_location(
            fullname, source, loader=SourceLoader(source, self.root, self.pins))


def preflight(directory):
    sys.dont_write_bytecode = True
    try:
        if not (sys.flags.isolated and sys.flags.ignore_environment and sys.flags.no_user_site
                and sys.pycache_prefix is None):
            raise ApparatusUnsafe('Python isolation required: python3 -I; pycache prefix forbidden')
        directory = Path(directory).resolve()
        root = directory.parents[1]
        files = scan(directory) + scan(root / 'scripts/exp008')
        files += [root / 'specs/ui-server-parity' / name for name in SPEC_FILES]
        files += toolchain_files(root)
        pins = manifest_pins(root)
        for file in files:
            verified_source(file, root, pins)
        # Never insert a repository directory into sys.path. Idempotent across modules.
        if not any(getattr(finder, '_b2_directory', None) == str(directory) for finder in sys.meta_path):
            finder = ApparatusFinder(directory, pins)
            finder._b2_directory = str(directory)
            sys.meta_path.insert(0, finder)
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(str(error), file=sys.stderr)
        print('{"verdict":"FAIL","errors":["unsafe apparatus; see stderr"]}')
        raise SystemExit(1)


def node_environment(values=None):
    """Node version probes and B2 children inherit only these host variables."""
    env = {key: os.environ[key] for key in ('PATH', 'LANG', 'LC_ALL', 'LC_CTYPE', 'TZ')
           if key in os.environ}
    if values:
        env.update(values)
    # Even caller-supplied fixture variables cannot inject a loader/search path.
    env.pop('NODE_PATH', None)
    env.pop('NODE_OPTIONS', None)
    return env


def _visible(value):
    import unicodedata
    return all(char.isprintable() and unicodedata.category(char)[0] not in ('C', 'M')
               and ord(char) not in (0x115F, 0x1160, 0x3164, 0xFFA0, 0x2800)
               and not (0xFE00 <= ord(char) <= 0xFE0F or 0xE0100 <= ord(char) <= 0xE01EF)
               and not (char.isspace() and char != ' ') for char in value)


def approver_identity(value):
    """NFKC + casefold, then drop all whitespace and invisible characters."""
    import unicodedata
    value = unicodedata.normalize('NFKC', value).casefold()
    return ''.join(char for char in value if not char.isspace() and _visible(char))


def human_approvers(text):
    """Allowlist identities: blank and '#' comment lines are skipped.

    A line is a comment when its normalized identity starts with '#', so leading
    whitespace, zero-width characters or a full-width '＃' cannot turn a comment
    into an identity. Any other '#' (an inline comment) makes the whole list
    empty: fail closed rather than guess which part is the identity.
    """
    identities = set()
    for line in text.splitlines():
        identity = approver_identity(line)
        if not identity or identity.startswith('#'):
            continue
        if '#' in identity:
            return set()
        identities.add(identity)
    return identities


def approval_valid(approver, basis, root, *, allow_ai_delegate=False):
    """Route allowlist + basis; FROZEN explicitly selects its unchanged R5 policy."""
    approvals = [approver] if isinstance(approver, str) else []
    bases = [basis] if isinstance(basis, str) else []
    root = Path(root).resolve()

    def concrete(value):
        return bool(value.strip()) and not re.search(r'<|>|TODO|pending|null|待批准|待人批准|待定标', value, re.I)

    if allow_ai_delegate:
        # Preserve FROZEN's R5 named-human and exact AI-delegate rules.
        approved = (len(approvals) == 1 and concrete(approvals[0]) and _visible(approvals[0])
                    and not re.search(r'Claude|GPT|GLM|Gemini|DeepSeek|Qwen|Doubao|ChatGPT|模型', approvals[0], re.I))
        if approved and re.search(r'AI|代批|待人复核', approvals[0], re.I):
            approved = approvals[0].strip() == 'AI 代批，待人复核（协调者）'
    else:
        try:
            names = (root / 'specs/ui-server-parity/human-approvers.txt').read_text(encoding='utf-8')
            humans = human_approvers(names)
        except (OSError, UnicodeError):
            humans = set()
        approved = len(approvals) == 1 and approver_identity(approvals[0]) in humans
    # Both approval modes require one concrete basis: calibration report + two reviews.
    basis_paths = re.findall(r'(?:[^\s`+;,，；]+/)+[^\s`+;,，；]+\.(?:json|md|log|txt)', bases[0]) if len(bases) == 1 else []
    allowed = [root]
    try:
        common = subprocess.check_output(['git', '-C', str(root), 'rev-parse',
                                         '--path-format=absolute', '--git-common-dir'],
                                        text=True, stderr=subprocess.PIPE).strip()
        allowed.append(Path(common).resolve().parent / '_tmp')
    except (OSError, subprocess.CalledProcessError):
        pass
    resolved_basis = [(root / name).resolve() for name in basis_paths]
    valid_basis = (len(bases) == 1 and concrete(bases[0]) and _visible(bases[0])
                   and len(set(resolved_basis)) >= 3
                   and all(path.is_file() and any(path.is_relative_to(base) for base in allowed)
                           for path in resolved_basis))
    return approved and valid_basis
