"""Deterministic Seatbelt policy, shared by recorder and evidence validator."""
import sys

# -I keeps PYTHONPATH, user site and the script directory out of module
# resolution.  Without it, exit before importing anything: a same-named stdlib
# file beside this module (json.py, pathlib.py, tempfile.py) would otherwise
# execute before the guard.  ``sys`` is builtin/frozen and already loaded.
if not sys.flags.isolated:
    raise SystemExit(f'run with -I: python3 -I {__file__}')

import importlib.util
import json
from pathlib import Path
import tempfile

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


_common = _load('common', Path(__file__).resolve().parent / 'common.py')
MOCK_PORT = _common.MOCK_PORT
SCRIPT = _common.SCRIPT

# #933 mock-gateway scenarios append exactly this rule after (deny network*):
# the apparatus-owned loopback port is the only network surface they get. The
# host literal must be "localhost" ("127.0.0.1" makes sandbox-exec reject the
# profile: host must be * or localhost; verified by the D0-pr2 probe). The port
# is derived from MOCK_PORT so the rule cannot drift away from environment().
MOCK_GATEWAY_ALLOW = f'(allow network-outbound (remote ip "localhost:{MOCK_PORT}"))'


def sandbox_profile(runtime, baseline, binary, side, mock_gateway=False):
    if side not in ('ts', 'rust'):
        raise ValueError('unknown sandbox side')
    if not isinstance(mock_gateway, bool):
        raise ValueError('mock_gateway must be a boolean')
    runtime, baseline, binary = Path(runtime), Path(baseline), Path(binary)
    # Rust has no Homebrew or baseline access. System libraries are required by
    # native macOS executables; an unsupported external dylib fails closed.
    read_dirs = [runtime, Path('/System'), Path('/usr/lib'), Path('/Library/Apple')]
    # dyld 启动任何 Mach-O 都要读根目录对象（literal，非子树）；TS/Rust 两侧都需要（真实宿主实测：缺它即 SIGABRT）
    read_files = [binary, Path('/'), Path('/dev/null'), Path('/dev/urandom'), Path('/dev/random')]
    if side == 'rust':
        # Staged release layout <stage>/bin/pilotdeck + <stage>/skills (run_round.stage_rust);
        # the binary resolves bundled skills from <exe_dir>/../skills, never the baseline.
        if binary.parent.name != 'bin':
            raise ValueError('Rust binary must be the staged <stage>/bin copy')
        read_dirs.append(binary.parent.parent / 'skills')
    if side == 'ts':
        read_dirs += [baseline / name for name in ('dist', 'node_modules', 'packages', 'skills')]
        read_dirs += [Path('/opt/homebrew/Cellar'), Path('/opt/homebrew/opt')]
        read_files += [baseline / 'package.json', SCRIPT / 'isolation_probe.mjs']
    # Metadata is limited to readable objects and their ancestor directory
    # objects, not ancestor subtrees. No global stat/exists or directory service.
    ancestors = {parent for path in read_dirs + read_files for parent in path.parents}
    def item(kind, path):
        # Inputs are already canonicalized by the caller. Do not inspect the
        # current filesystem while validating archived policy text.
        return f'({kind} {json.dumps(str(path))})'
    readable = ' '.join(item('subpath', p) for p in read_dirs)
    readable += ' ' + ' '.join(item('literal', p) for p in read_files)
    rules = ['(version 1)', '(deny default)', '(allow sysctl-read)',
             '(allow file-read-metadata ' + readable + ' ' +
             ' '.join(item('literal', p) for p in sorted(ancestors)) + ')',
             '(allow file-read-data file-map-executable ' + readable + ')',
             '(allow file-write* ' + item('subpath', runtime) + ')',
             '(allow file-read* file-write* (literal "/dev/null") (literal "/dev/stdin") (literal "/dev/stdout") (literal "/dev/stderr"))',
             '(allow process-exec ' + item('literal', binary) + ')',
             '(deny network*)', '(deny process-fork)']
    if mock_gateway:
        # Probe-verified: deny precedes route lookup, so this opens exactly the
        # 28999 outbound path; 18789/other ports/Unix sockets/listen stay EPERM.
        rules.insert(rules.index('(deny network*)') + 1, MOCK_GATEWAY_ALLOW)
    return '\n'.join(rules) + '\n'
