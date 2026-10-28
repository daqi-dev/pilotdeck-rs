"""Shared source-only loader for the CLI parity apparatus (#701)."""
import sys

import atexit
import importlib.util
from pathlib import Path
import shutil
import tempfile

# Always compile apparatus source, never reuse adjacent bytecode. Redirect
# stdlib cache reads too: dont_write_bytecode only disables writes. Entries
# re-exec this module, so keep the owned directory on sys for process-wide reuse.
if not hasattr(sys, '_cli_parity_pycache_prefix'):
    sys._cli_parity_pycache_prefix = tempfile.mkdtemp(prefix='pdrs-cli-parity-pyc-')
    atexit.register(shutil.rmtree, sys._cli_parity_pycache_prefix, ignore_errors=True)
sys.pycache_prefix = sys._cli_parity_pycache_prefix
sys.dont_write_bytecode = True


# One inventory for the apparatus tests and their negative-control driver.
ENTRY_FILES = ('compare.py', 'controls.py', 'host_negative_controls.py',
               'run_round.py', 'test_cli_parity.py', 'policy.py')
APPARATUS_FILES = ENTRY_FILES + ('common.py', '_bootstrap.py')
APPARATUS_MODULES = tuple(Path(name).stem for name in APPARATUS_FILES)
MIRROR_FILES = APPARATUS_FILES + ('scenarios.json', 'chat-search.json')


def load(name, path):
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
