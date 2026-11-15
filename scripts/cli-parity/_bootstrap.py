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

# #2513 fix-r4 (review boundary#2): the executing code object of this loader
# itself, captured before anything else can replace the file on disk. Entries
# re-exec this module and then verify this fingerprint against HEAD after the
# lease (run_round._verify_loaded_apparatus) — a bootstrap mutated inside a
# mutation window would otherwise keep loading unverified apparatus modules
# under the lease.
_bootstrap_code = sys._getframe().f_code

# #2513 fix-r15 (DS r14 review): the verification set is this loader's own
# record — (name, module object) pairs a mutant cannot unbind, unlike the
# fix-r14 sys.modules derivation, where a module that popped itself out of
# sys.modules kept its stamp and silently left the comparison. Entries re-exec
# this module, so the registry is process-shared like _cli_parity_pycache_prefix
# above; the post-lease verification replays it. A module missing from it while
# still stamped in sys.modules is a red, not a silent exit (run_round).
if not hasattr(sys, '_cli_parity_loaded_apparatus'):
    sys._cli_parity_loaded_apparatus = []


# One inventory for the apparatus tests and their negative-control driver.
ENTRY_FILES = ('compare.py', 'controls.py', 'host_negative_controls.py',
               'run_round.py', 'test_cli_parity.py', 'policy.py')
APPARATUS_FILES = ENTRY_FILES + ('common.py', '_bootstrap.py')
APPARATUS_MODULES = tuple(Path(name).stem for name in APPARATUS_FILES)
MIRROR_FILES = APPARATUS_FILES + ('scenarios.json', 'chat-search.json')


def _record_loaded(name, module):
    # The registry holds module object references, so a mutant that edits or
    # empties sys.modules cannot make a loaded module leave the verified set.
    if not any(pair[0] == name and pair[1] is module
               for pair in sys._cli_parity_loaded_apparatus):
        sys._cli_parity_loaded_apparatus.append((name, module))


def load(name, path):
    path = Path(path).resolve()
    existing = sys.modules.get(name)
    existing_file = getattr(existing, '__file__', None)
    if existing is not None and existing_file and Path(existing_file).resolve() == path:
        if not isinstance(getattr(existing, '_apparatus_source', None), bytes):
            raise RuntimeError(f'unverified apparatus module cache: {path}')
        _record_loaded(name, existing)
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
    _record_loaded(name, module)
    return module
