"""Full-path ID controls: a legal rename passes; entity collapse still fails.

Each row exercises the real recursive policy dispatcher and structural diff,
using cross-channel witnesses, not a mock normalization implementation.
"""
# Isolated bootstrap: verify the source loader before executing local code.
import sys as _boot_sys
if not (_boot_sys.flags.isolated and _boot_sys.flags.ignore_environment
        and _boot_sys.flags.no_user_site and _boot_sys.pycache_prefix is None):
    print('{"verdict":"FAIL","errors":["Python isolation required: use python3 -I; no pycache prefix"]}')
    raise SystemExit(1)
if _boot_sys.version_info < (3, 11) or 'os' not in _boot_sys.modules:
    print('{"verdict":"FAIL","errors":["os must be preloaded from CPython 3.11+ frozen modules"]}')
    raise SystemExit(1)
import os as _boot_os
_boot_sys.dont_write_bytecode = True
_boot_dir = _boot_os.path.dirname(_boot_os.path.dirname(_boot_os.path.abspath(__file__)))
# Identity, not spelling: a case-insensitive alias, a symlinked spelling or a
# mixed ``//`` form names this very directory while the string differs
# (fix-r3 P1, same defect as #701 fix-r2). Nonexistent sys.path entries skip.
if any(
        _boot_os.path.isdir(entry or _boot_os.curdir)
        and any(_boot_os.path.isdir(root)
                and _boot_os.path.samefile(entry or _boot_os.curdir, root)
                for root in (_boot_dir, _boot_os.path.join(_boot_dir, 'controls')))
        for entry in _boot_sys.path):
    print('{"verdict":"FAIL","errors":["apparatus directory on sys.path; run the entry file directly with -I"]}')
    raise SystemExit(1)
_boot_source = _boot_os.path.join(_boot_dir, 'apparatus.py')
if _boot_os.path.islink(_boot_dir) or _boot_os.path.islink(_boot_source):
    print('{"verdict":"FAIL","errors":["apparatus bootstrap symlink"]}')
    raise SystemExit(1)
import hashlib as _boot_hashlib
import subprocess as _boot_subprocess
_boot_root = _boot_os.path.dirname(_boot_os.path.dirname(_boot_dir))
_boot_manifest = _boot_os.path.join(_boot_root, 'specs/ui-server-parity/FROZEN.md')
with open(_boot_source, 'rb') as _boot_file:
    _boot_bytes = _boot_file.read()
try:
    if _boot_os.path.isfile(_boot_manifest):
        with open(_boot_manifest) as _boot_file:
            _boot_pins = [line.split()[0] for line in _boot_file
                          if line.rstrip().endswith('  ../../scripts/ui-server-parity/apparatus.py')]
        if len(_boot_pins) != 1 or _boot_hashlib.sha256(_boot_bytes).hexdigest() != _boot_pins[0]:
            raise ValueError('apparatus bootstrap hash mismatch')
    else:
        _boot_trusted = _boot_subprocess.check_output(
            ['git', '-C', _boot_root, 'show', 'HEAD:scripts/ui-server-parity/apparatus.py'])
        if _boot_bytes != _boot_trusted:
            raise ValueError('apparatus bootstrap differs from HEAD without manifest')
except (OSError, ValueError, _boot_subprocess.CalledProcessError) as _boot_error:
    print(str(_boot_error), file=_boot_sys.stderr)
    print('{"verdict":"FAIL","errors":["apparatus bootstrap hash admission failed"]}')
    raise SystemExit(1)
_boot_namespace = {'__file__': _boot_source, '__name__': '_apparatus_bootstrap'}
exec(compile(_boot_bytes, _boot_source, 'exec'), _boot_namespace)
_boot_namespace['preflight'](_boot_dir)
import json
import re

from compare import Normalizer, diff
from policy import HTTP_IDS, WS_IDS, DISK_IDS, SSE_IDS, field_key

A = '11111111-1111-4111-8111-111111111111'
B = '22222222-2222-4222-8222-222222222222'
C = 'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa'
D = 'bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb'


def path_parts(path):
    return [0 if p == '*' else json.loads(p) if p.startswith('"') else p
            for p in re.findall(r'"(?:[^"\\]|\\.)*"|[^.]+', path)]


def document(path, value):
    for part in reversed(path_parts(path)):
        value = [value] if part == 0 else {part: value}
    return value


def locations():
    for channel, contexts in (('http', HTTP_IDS), ('ws', WS_IDS), ('disk', DISK_IDS),
                              ('sse', {'upload.events:upload_completed': SSE_IDS})):
        for context, paths in contexts.items():
            for path, category in paths.items():
                if category not in ('upload-url', 'token'):
                    yield channel, context, path, category


def run_identity_controls():
    rows = []
    for channel, context, path, category in locations():
        expected_location = f'{channel}[{context}].body' + ''.join(
            f'[{part}]' if type(part) is int else '.' + field_key(part) for part in path_parts(path))
        def evaluate(mutation=None):
            errors = []
            left, right = Normalizer('left', errors), Normalizer('right', errors)
            # Two independent identities are established on other full paths.
            # Cross semantic categories/channels must share the same UUID map.
            for norm, one, two in ((left, A, B), (right, C, D)):
                norm.value([{'turnId': one}], 'disk', '@chat')
                norm.value({'runId': two}, 'ws', 'status')
            suffix = ':text:0' if category in ('block', 'timeline') else ''
            lv, rv = B + suffix, (C if mutation == 'collapse' else D) + suffix
            if mutation == 'suffix':
                rv = D + ':text:1'
            if mutation == 'kind':
                rv = D + ':thinking:0'
            location = f'{channel}[{context}].body'
            documents = [document(path, lv), document(path, rv)]
            if channel == 'http' and context == 'readonly.messages' and path.startswith('messages.'):
                for norm, doc in zip((left, right), documents):
                    stamp = '2023-11-14T22:13:20.000Z'
                    norm.wall_bounds = (1700000000000, 1700000010000)
                    message = doc['messages'][0]
                    message['timestamp'] = stamp
                    message.setdefault('sessionId', 'identity-fixture-session')
                    message.setdefault('sequence', 1)
                    norm.chat_clocks[message['sessionId'], message['sequence']] = (stamp, message.get('entryId'))
            a = left.value(documents[0], channel, context, location=location)
            b = right.value(documents[1], channel, context, location=location)
            rule = {'http': 'http.body', 'ws': 'ws.structure', 'disk': 'disk.snapshot', 'sse': 'sse.sequence'}[channel]
            diff(a, b, errors, rule, location)
            return errors, rule

        good, _ = evaluate()
        for mutation in (['collapse', 'suffix', 'kind'] if category in ('block', 'timeline') else ['collapse']):
            bad, rule = evaluate(mutation)
            hit = [d for d in bad if d['rule'] == rule and d['location'] == expected_location]
            rows.append({'name': f'id-{mutation}:{channel}:{context}:{path}',
                         'level': 'full-path-policy', 'expect': 'FAIL', 'expected_rule': rule,
                         'expected_location': expected_location, 'policy_path': path,
                         'equivalent_rename': not good, 'actual': 'FAIL' if bad else 'PASS',
                         'passed': not good and bool(hit), 'matched': hit[:1]})
    return rows
