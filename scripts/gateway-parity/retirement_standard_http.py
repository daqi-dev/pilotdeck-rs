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
"""Full ordinary HTTP comparison; business smoke booleans cannot certify parity.

Reuse the frozen A2 field-scoped normalizer and full durable projection. Input
roots and provider endpoints must already match, never rewrite observed paths.
"""
import hashlib
import json
from http.client import HTTPResponse
from io import BytesIO

compare = _load('compare', _apparatus / 'compare.py')
OMITTED_HEADERS = {'date', 'connection', 'keep-alive', 'transfer-encoding'}


class Wire:
    def __init__(self, value):
        self.value = value

    def makefile(self, *_):
        return BytesIO(self.value)


def http_projection(row, normalizer):
    raw_request = bytes.fromhex(row['requestHex'])
    head, request_body = raw_request.split(b'\r\n\r\n', 1)
    method, target, version = head.split(b'\r\n', 1)[0].decode('ascii').split(' ')
    if method != row['method'] or target != row['path'] or version != 'HTTP/1.1':
        raise ValueError('request metadata differs from original bytes')
    response = HTTPResponse(Wire(bytes.fromhex(row['responseHex'])))
    response.begin()
    body = response.read().decode('utf8', errors='strict')
    if response.status != row['status'] or body != row['body']:
        raise ValueError('response metadata differs from original bytes')
    # Retain repeated headers (including content-length) and their values. Only
    # the four already-approved transport/time headers are omitted.
    headers = {}
    for key, value in response.getheaders():
        key = key.lower()
        if key not in OMITTED_HEADERS:
            headers.setdefault(key, []).append(value)
    try:
        body = json.loads(body)
    except ValueError:
        pass
    return normalizer.value({'method': method, 'path': target,
                             'requestBodyHex': request_body.hex(),
                             'status': response.status, 'headers': headers, 'body': body})


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, ensure_ascii=False,
                                     separators=(',', ':')).encode()).hexdigest()


def difference(left, right, path='$', limit=30):
    """Bounded explanatory pointers; equality itself always checks full values."""
    out = []
    def walk(a, b, at):
        if len(out) >= limit or (type(a) is type(b) and digest(a) == digest(b)):
            return
        if type(a) is not type(b):
            out.append({'path': at, 'kind': 'type'})
        elif isinstance(a, dict):
            for key in sorted(a.keys() | b.keys()):
                if key not in a or key not in b:
                    out.append({'path': at + '.' + key, 'kind': 'missing-key',
                                'side': 'ts' if key not in a else 'native'})
                else:
                    walk(a[key], b[key], at + '.' + key)
                if len(out) >= limit:
                    break
        elif isinstance(a, list):
            if len(a) != len(b):
                out.append({'path': at, 'kind': 'length', 'ts': len(a), 'native': len(b)})
            for index, (av, bv) in enumerate(zip(a, b)):
                walk(av, bv, f'{at}[{index}]')
                if len(out) >= limit:
                    break
        else:
            out.append({'path': at, 'kind': 'value'})
    walk(left, right, path)
    return out


def comparison(d, observed, mode, identity):
    pair = [observed.get((mode, engine)) for engine in ('ts', 'native')]
    if any(item is None for item in pair):
        return {'status': 'BLOCKED', 'missing': ['both-production-originals']}
    if mode != 'http':
        # Timer/model/durable smoke is useful diagnostic evidence, but has no
        # full fixed-TS contract adapter yet. Do not turn two True flags green.
        return {'status': 'BLOCKED', 'missing': ['full-background-wire-model-disk-comparison']}
    values = []
    runtimes = []
    for folder, raw, _ in pair:
        provenance = d.read(folder / 'provenance.json')
        runtime = provenance.get('runtime')
        if not isinstance(runtime, dict) or set(runtime) != {'home', 'project', 'tmp', 'workspaces', 'providerUrl', 'gatewayPort'}:
            return {'status': 'BLOCKED', 'missing': ['registered-identical-runtime-layout']}
        runtimes.append(runtime)
        if Path(runtime['home']).parent != folder:
            preservation = d.read(folder.parent / (folder.name + '.preservation.json'))
            stat = folder.stat()
            if (preservation.get('originalRuntime') != str(Path(runtime['home']).parent)
                    or preservation.get('preservedAt') != str(folder)
                    or (preservation.get('device'), preservation.get('inode')) != (stat.st_dev, stat.st_ino)
                    or preservation.get('exitCode') != 0
                    or preservation.get('originals') != {
                        name: hashlib.sha256((folder / name).read_bytes()).hexdigest()
                        for name in ('result.json', 'provenance.json')}):
                raise ValueError('original-directory preservation binding differs')
        rows = [row for row in raw['http'] if row['id'] == 'gateway.http:' + identity]
        if len(rows) != 1:
            raise ValueError('missing or duplicate HTTP entry original')
        normalizer = compare.entity_normalizer(folder, {})
        for row in raw['http']:
            try:
                normalizer.index(json.loads(row['body']))
            except ValueError:
                pass
        normalizer.index(raw['providerRequests'])
        values.append({'http': http_projection(rows[0], normalizer),
                       'modelRequests': normalizer.value(raw['providerRequests']),
                       'disk': compare.disk(folder, normalizer)})
    if runtimes[0] != runtimes[1]:
        return {'status': 'FAIL', 'rule': 'ENTRY_IDENTICAL_RUNTIME',
                'differences': difference(runtimes[0], runtimes[1])}
    equal = digest(values[0]) == digest(values[1])
    return {'status': 'PASS' if equal else 'FAIL',
            'rule': 'ENTRY_FULL_HTTP_MODEL_DISK',
            'projectionSha256': {'ts': digest(values[0]), 'native': digest(values[1])},
            'differences': difference(values[0], values[1])}
