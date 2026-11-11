"""Per-entry behavioral evidence from the registered 150 inventory.

One passing request does not certify a configurable feature. This face states
which rules were exercised and refuses cross-build or cfg(test) substitutions.
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
import json
import unittest
module = _load('retirement_standard_http', _apparatus / 'retirement_standard_http.py')

def row(body, headers=()):
    raw = json.dumps(body, separators=(',', ':')).encode()
    header = [('Content-Type', 'application/json'), ('Content-Length', str(len(raw))), *headers]
    wire = b'HTTP/1.1 200 OK\r\n' + ''.join(f'{key}: {value}\r\n' for key, value in header).encode() + b'\r\n' + raw
    return {'method': 'GET', 'path': '/health', 'status': 200, 'body': raw.decode(),
            'requestHex': b'GET /health HTTP/1.1\r\nHost: 127.0.0.1:28478\r\n\r\n'.hex(),
            'responseHex': wire.hex()}


def project(value):
    return module.http_projection(value, module.compare.Normalizer())


class FullProjection(unittest.TestCase):
    def test_extra_body_field_is_not_hidden_by_success_marker(self):
        self.assertNotEqual(project(row({'ok': True})), project(row({'ok': True, 'unexpected': 1})))

    def test_content_length_is_retained(self):
        projected = project(row({'ok': True}))
        self.assertEqual(projected['headers']['content-length'], ['11'])

    def test_extra_header_is_retained(self):
        self.assertNotEqual(project(row({'ok': True})), project(row({'ok': True}, [('X-Contract', 'changed')])))

    def test_only_registered_response_headers_are_omitted(self):
        self.assertEqual(project(row({'ok': True}, [('Date', 'one'), ('Connection', 'close')])),
                         project(row({'ok': True}, [('Date', 'two'), ('Connection', 'keep-alive')])))

    def test_body_metadata_must_match_wire(self):
        value = row({'ok': True}); value['body'] = '{"ok":false}'
        with self.assertRaises(ValueError): project(value)

    def test_request_metadata_must_match_wire(self):
        value = row({'ok': True}); value['path'] = '/different'
        with self.assertRaises(ValueError): project(value)

    def test_business_timing_and_paths_remain_exact(self):
        self.assertNotEqual(project(row({'updatedAt': 1, 'path': '/one'})),
                            project(row({'updatedAt': 2, 'path': '/two'})))

    def test_diagnostic_background_cannot_certify_parity(self):
        present = {('cron', side): object() for side in ('ts', 'native')}
        self.assertEqual(module.comparison(None, present, 'cron', 'cron')['status'], 'BLOCKED')

    def test_missing_side_cannot_certify_parity(self):
        self.assertEqual(module.comparison(None, {}, 'http', '/health')['status'], 'BLOCKED')


if __name__ == '__main__':
    unittest.main()
