"""The one entry guard for the frozen gateway-parity apparatus (#991).

Historically every A2 CLI copied the same bootstrap / HEAD provenance /
exit-code classifier, so each new defence had to be repeated verbatim in ~10
files and review kept finding the one that was missed.  This module is the
single implementation of that entry contract:

* ``load`` loads an apparatus module from an explicit path (never ``sys.path``)
  and records the exact bytes that were executed for later provenance checks;
* ``_has_git_checkout`` distinguishes a static export (no HEAD to read) from
  a broken checkout (must fail closed); its local copy must stay AST-identical
  to common's copy (checked by test_safety);
* ``verify_cli_imports`` reads the verifier from HEAD's ``common.py`` and uses
  it to reject any already-imported apparatus module whose executed bytes
  differ from the same HEAD;
* ``run_entry`` is the only ``__main__`` body: HEAD preflight, run ``main``,
  and classify failures (untrusted apparatus / cancellation -> BLOCKED/2,
  parity failure -> FAIL/1).

Entry files still carry a short verbatim preamble because the guard itself
must be loaded before ``common.py`` and the ``-I`` check must precede every
other standard-library import.  That preamble is deliberately tiny; all
defence logic lives here and is covered by test_safety's AST check.
"""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
from types import ModuleType

HERE = Path(__file__).resolve().parent
# Entries under controls/ pass the apparatus directory explicitly; the guard
# itself always lives beside common.py.
APPARATUS = HERE.parent if HERE.name == 'controls' else HERE


def load(name, path):
    """Load one apparatus module by explicit path and record executed bytes."""
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


def _has_git_checkout(root):
    """Missing checkout metadata permits static use; broken metadata does not."""
    import os
    if 'GIT_DIR' in os.environ:
        return True
    for directory in (root, *root.parents):
        try:
            (directory / '.git').lstat()
        except FileNotFoundError:
            continue
        return True
    return False


def verify_cli_imports(apparatus=None):
    """Trust only HEAD's ``common.py``; reject already-loaded disk mutants.

    The verifier is read from the pinned HEAD, never from the disk module that
    could itself be the active mutant.  Static exports without checkout
    metadata keep their existing behaviour; broken metadata still raises.
    """
    apparatus = Path(apparatus or APPARATUS)
    root = apparatus.parent.parent
    if not _has_git_checkout(root):
        return None  # Static exports have no HEAD; keep their existing checks.
    head = subprocess.check_output(['git', '-C', str(root), 'rev-parse', '--verify', 'HEAD'],
                                   timeout=30).decode().strip()
    source = subprocess.check_output(['git', '-C', str(root), 'show',
                                      f'{head}:scripts/gateway-parity/common.py'], timeout=30)
    trusted = ModuleType('pristine_cli_verifier')
    trusted.__file__ = str(apparatus / 'common.py')
    exec(compile(source, trusted.__file__, 'exec'), trusted.__dict__)
    trusted.verify_loaded_apparatus(head)
    return head


def _failure(error):
    """Map one escaping entry error to the single status/exit contract."""
    rule = getattr(error, 'rule', None) or 'EVIDENCE'
    # Untrusted apparatus, cancellation and unavailable resources are not a
    # parity verdict: BLOCKED/2.  Everything else is a failed comparison or an
    # ordinary evidence failure: FAIL/1.
    blocked = (rule == 'APPARATUS_IMPORTS'
               or isinstance(error, (PermissionError, TimeoutError, KeyboardInterrupt)))
    return ('BLOCKED', 2) if blocked else ('FAIL', 1), rule


def run_entry(main):
    """Run one CLI entry: HEAD preflight, main, one shared exit classifier."""
    try:
        verify_cli_imports()
    except (Exception, KeyboardInterrupt) as error:
        print(json.dumps({'status': 'BLOCKED', 'error': str(error)}, ensure_ascii=False))
        raise SystemExit(2)
    try:
        code = main()
    except (Exception, KeyboardInterrupt) as error:
        (status, code), rule = _failure(error)
        print(json.dumps({'status': status,
                          'failure': {'rule': rule, 'detail': str(error)}},
                         ensure_ascii=False))
        raise SystemExit(code)
    raise SystemExit(code)
