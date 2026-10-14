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
import subprocess

SPEC_FILES = ('acceptance.md', 'normalization.md', 'accepted-diffs.md', 'multimodal.md')
# All git-tracked exp008 files are apparatus, including its other entrypoints.
EXP008_FILES = ('compare.py', 'controls.py', 'driver.mjs', 'enumerate_hook.mjs',
                'preload.mjs', 'proxy/Cargo.lock', 'proxy/Cargo.toml', 'proxy/src/main.rs',
                'proxy/tests/node-ws-semantics.cjs', 'reconcile_routes.py', 'run.py', 'vite.config.mjs')
APPARATUS_PATHS = ('scripts/ui-server-parity', 'scripts/exp008', 'specs/ui-server-parity')


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
            if os.path.islink(path):
                errors.append('apparatus symlink: ' + path)
            if name == '__pycache__' or name.endswith('.pyc'):
                errors.append('forbidden Python cache: ' + path)
        for name in names:
            path = os.path.join(parent, name)
            if os.path.isfile(path):
                files.append(path)
            else:
                errors.append('apparatus nonregular file: ' + path)
    registered = tracked_files(directory)
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
    status = subprocess.check_output(['git', '-C', str(root), 'status', '--porcelain',
                                     '--untracked-files=all', '--', *APPARATUS_PATHS], text=True)
    if status:
        raise ApparatusUnsafe('dirty apparatus git status: ' + status.strip())
    head = subprocess.check_output(['git', '-C', str(root), 'rev-parse', 'HEAD'], text=True).strip()
    if expected_head is not None and head != expected_head:
        raise ApparatusUnsafe('batch HEAD changed; start a NEW batch')
    return head


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
