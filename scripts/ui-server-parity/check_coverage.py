#!/usr/bin/env python3
"""S2 route × direct HTTP case gate; no S3 behavioral comparison."""
# Admit source-only apparatus before json or any local module can be imported.
import os as _boot_os
import sys as _boot_sys
_boot_sys.dont_write_bytecode = True
_boot_dir = _boot_os.path.dirname(_boot_os.path.abspath(__file__))
_boot_source = _boot_os.path.join(_boot_dir, 'apparatus.py')
if _boot_os.path.islink(_boot_dir) or _boot_os.path.islink(_boot_source):
    print('{"verdict":"FAIL","errors":["apparatus bootstrap symlink"]}')
    raise SystemExit(1)
_boot_namespace = {'__file__': _boot_source, '__name__': '_apparatus_bootstrap'}
with open(_boot_source, 'rb') as _boot_file:
    exec(compile(_boot_file.read(), _boot_source, 'exec'), _boot_namespace)
_boot_namespace['preflight'](_boot_dir)
import argparse
import json
import re
from pathlib import Path


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


def check(enumeration, matrix, scenarios, evidence=None):
    errors = []
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
        if waiver and (waiver.get('route') != route or not all(isinstance(waiver.get(k), str) and waiver[k].strip() for k in ('reason', 'approver'))):
            errors.append(f'{route}: malformed exemption')
        if evidence is not None and waiver and isinstance(waiver.get('approver'), str) and 'AI 代批' in waiver['approver']:
            # Check all observed handlers, including failures and cases outside
            # the declared coverage list; a waiver cannot hide Rust takeover.
            if any(e.get('kind') == 'case_http' and e.get('handler') == 'rust'
                   and e.get('route') == route for e in evidence):
                errors.append(f'{route}: Rust-handled route exemption requires human approval (AI 代批 forbidden)')
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
