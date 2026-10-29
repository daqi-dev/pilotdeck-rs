#!/usr/bin/env -S python3 -I
"""S2 route × direct HTTP case gate; no S3 behavioral comparison."""
# Isolated bootstrap: verify the source loader before executing local code.
import os as _boot_os
import sys as _boot_sys
if not (_boot_sys.flags.isolated and _boot_sys.flags.ignore_environment
        and _boot_sys.flags.no_user_site and _boot_sys.pycache_prefix is None):
    print('{"verdict":"FAIL","errors":["Python isolation required: use python3 -I; no pycache prefix"]}')
    raise SystemExit(1)
_boot_sys.dont_write_bytecode = True
_boot_dir = _boot_os.path.dirname(_boot_os.path.abspath(__file__))
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
import argparse
import json
import re
from pathlib import Path
from enum import Enum
from apparatus import approval_valid

ROOT = Path(__file__).resolve().parents[2]
RUST_ROUTES = ROOT / "specs/ui-server-parity/rust-routes.json"


class Handler(str, Enum):
    TS = "ts"
    RUST = "rust"


def rust_routes(manifest=None):
    """Static Rust router/config export; never infer ownership from traffic."""
    manifest = json.loads(RUST_ROUTES.read_text()) if manifest is None else manifest
    if type(manifest.get("schema")) is not int or manifest["schema"] != 1 or not isinstance(manifest.get("routes"), list):
        raise ValueError("invalid Rust route manifest")
    routes = set()
    for row in manifest["routes"]:
        if (Handler(row["handler"]) != Handler.RUST or row["method"] not in ("GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS", "ALL")
                or not isinstance(row["path"], str) or not row["path"].startswith("/")
                or any(c.isspace() for c in row["path"]) or "?" in row["path"]):
            raise ValueError("invalid Rust route declaration")
        key = route_key(row["method"], row["path"])
        if key in routes:
            raise ValueError("duplicate Rust route: " + key)
        routes.add(key)
    return routes


def route_key(method, path):
    method = method.upper()
    return f"{'ALL' if method == '_ALL' else method} {path.rstrip('/') or '/'}"


def matches(route, method, path):
    verb, pattern = route.split(' ', 1)
    if verb != 'ALL' and verb != method.upper():
        return False
    pattern = re.escape(pattern)
    pattern = re.sub(r':\w+', '[^/]+', pattern).replace(r'\*', '.*')
    return re.fullmatch(pattern + '/?', path.split('?', 1)[0]) is not None


def check(enumeration, matrix, scenarios, evidence=None, *, takeover=None):
    errors = []
    owned = rust_routes(takeover)
    if enumeration.get('appCaptured') is not True or not enumeration.get('routes'):
        errors.append('invalid/empty runtime enumeration')
    routes = {route_key(r['method'], r['path']) for r in enumeration.get('routes', [])}
    raw_routes = {(r['method'], r['path']) for r in enumeration.get('routes', [])}
    if len(routes) != len(raw_routes):
        errors.append('distinct runtime routes collide after route normalization')
    if enumeration.get('uniqueRouteCount') != len(raw_routes):
        errors.append('runtime uniqueRouteCount disagrees with routes')
    definitions = {}
    for case in scenarios['http_cases']:
        if case['id'] in definitions:
            errors.append(f"duplicate case {case['id']}")
        definitions[case['id']] = case
    rows = {}
    for row in matrix['routes']:
        route = row['route']
        if route in rows:
            errors.append(f'duplicate matrix row {route}')
        rows[route] = row
        cases = row.get('cases', [])
        waiver = row.get('exemption')
        if not cases and not waiver:
            errors.append(f'{route}: missing direct HTTP case or exemption')
        if waiver and (waiver.get('route') != route or not all(isinstance(waiver.get(k), str) and waiver[k].strip() for k in (('reason',) if route in owned else ('reason', 'approver')))):
            errors.append(f'{route}: malformed exemption')
        if route in owned and waiver and not approval_valid(
                waiver.get('Approved-By'), waiver.get('Approval-Basis'), ROOT,
                allow_ai_delegate=False):
            errors.append(f'{route}: Rust exemption requires allowlisted human Approved-By / valid Approval-Basis')
        if 'handler' in row:
            try:
                declared = Handler(row['handler'])
                if (declared == Handler.RUST) != (route in owned):
                    errors.append(f'{route}: handler disagrees with static Rust routes')
            except (ValueError, TypeError):
                errors.append(f'{route}: invalid handler enum')
        for case_id in cases:
            case = definitions.get(case_id)
            if not case or case.get('transport') != 'http' or case.get('route') != route or not isinstance(case.get('expected_statuses'), list) or not case['expected_statuses'] or any(type(status) is not int or not 100 <= status < 600 for status in case['expected_statuses']):
                errors.append(f'{route}: invalid direct HTTP case {case_id}')
                continue
            # Concrete request matching is also checked against evidence below.
            if evidence is not None:
                for side in ('direct', 'proxy'):
                    found = [e for e in evidence if e.get('kind') == 'case_http' and e.get('case') == case_id and e.get('side') == side
                             and e.get('route') == route and e.get('ok') is True and e.get('status') in case['expected_statuses'] and matches(route, e.get('method', ''), e.get('path', ''))]
                    if not found:
                        errors.append(f'{route}: missing successful {side} HTTP evidence for {case_id}')
    for route in sorted(owned - routes):
        errors.append(f'{route}: Rust route absent from runtime enumeration')
    for event in evidence or []:
        if event.get('kind') != 'case_http' or event.get('handler') is None:
            continue
        try:
            handler = Handler(event['handler'])
        except (ValueError, TypeError):
            errors.append('invalid observed handler enum')
            continue
        route = event.get('route')
        if event.get('side') == 'proxy' and (handler == Handler.RUST) != (route in owned):
            errors.append(f'{route}: observed handler disagrees with static Rust routes')
    for route in sorted(routes - rows.keys()):
        errors.append(f'{route}: absent from matrix')
    for route in sorted(rows.keys() - routes):
        errors.append(f'{route}: absent from runtime enumeration (stale matrix)')
    return {'verdict': 'FAIL' if errors else 'PASS', 'runtime_routes': len(routes),
            'covered_routes': sum(bool(r.get('cases')) for r in rows.values()),
            'exempt_routes': sum(bool(r.get('exemption')) for r in rows.values()),
            'mode': 'evidence' if evidence is not None else 'declaration', 'errors': errors}


def main():
    here = Path(__file__).resolve().parent
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--enumeration', type=Path, required=True)
    p.add_argument('--matrix', type=Path, default=here / 'coverage.json')
    p.add_argument('--scenarios', type=Path, default=here / 'scenarios.json')
    p.add_argument('--evidence', type=Path, help='JSONL of case_http records; requires direct + proxy success')
    p.add_argument('--out', type=Path)
    a = p.parse_args()
    try:
        evidence = [json.loads(line) for line in a.evidence.read_text().splitlines() if line] if a.evidence else None
        result = check(json.loads(a.enumeration.read_text()), json.loads(a.matrix.read_text()), json.loads(a.scenarios.read_text()), evidence)
    except (OSError, ValueError, KeyError, TypeError, AttributeError) as error:
        result = {'verdict': 'FAIL', 'errors': [f'{type(error).__name__}: {error}']}
    raw = json.dumps(result, ensure_ascii=False, indent=2) + '\n'
    if a.out:
        a.out.write_text(raw)
    print(raw, end='')
    return 1 if result['verdict'] == 'FAIL' else 0


if __name__ == '__main__':
    raise SystemExit(main())
