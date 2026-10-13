#!/usr/bin/env python3
"""exp008 / B1 coarse reconciliation: runtime Express stack enumeration vs the
static inventory in docs/research/006_phase-b-ui-server/route-inventory.md.

Normalisation only removes what is known to be a labelling/formatting artifact:
  * trailing slash on a mounted router's own `/` route;
  * full-width parenthetical annotations (`（工厂）`, `（stream=true）`);
  * `router.all` reported by Express as `_ALL`;
  * static rows labelled `SSE` / `POST→SSE` matched against the real HTTP method.
Everything else is reported as only-static / only-runtime with a reason.
"""
import argparse
import json
import re
from pathlib import Path


def norm_path(value):
    value = value.strip().strip('`').strip()
    value = re.sub(r'（[^）]*）', '', value)
    value = re.sub(r'\([^)]*\)', '', value)
    value = value.split('?')[0].strip()
    if value == '*':
        return '/*'
    if value.endswith('/') and value != '/':
        value = value.rstrip('/')
    return value or '/'


# Static rows whose method label is a stream form; the real registration is GET
# (module SSE) or POST (agent).
def norm_static_method(method):
    method = method.strip().strip('`').upper()
    if method in ('ALL', '_ALL'):
        return 'ALL'
    if method.startswith('POST→SSE'):
        return 'POST'
    if method.startswith('SSE'):
        return 'GET'
    return method


def norm_runtime_method(method):
    method = method.strip().upper()
    return 'ALL' if method in ('_ALL', 'ALL') else method


def parse_inventory(path):
    routes = []
    for line in Path(path).read_text().splitlines():
        if not line.startswith('|'):
            continue
        cells = [cell.strip() for cell in line.strip('|').split('|')]
        if len(cells) < 2:
            continue
        raw_method = cells[0].strip().strip('`')
        if raw_method.upper() not in ('GET', 'POST', 'PUT', 'DELETE', 'PATCH', 'ALL', 'SSE', 'POST→SSE'):
            continue
        full_path = cells[1].strip().strip('`')
        if not (full_path.startswith('/') or full_path.startswith('*')):
            continue
        routes.append({'method': norm_static_method(raw_method), 'path': norm_path(full_path),
                       'label': raw_method})
    return routes


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('enumeration')
    parser.add_argument('inventory')
    parser.add_argument('--out', required=True)
    args = parser.parse_args()

    enum = json.loads(Path(args.enumeration).read_text())
    runtime = [{'method': norm_runtime_method(route['method']), 'path': norm_path(route['path']),
                'rawMethod': route['method'], 'rawPath': route['path']} for route in enum['routes']]
    static = parse_inventory(args.inventory)

    runtime_set = {(route['method'], route['path']) for route in runtime}
    runtime_paths = {route['path'] for route in runtime}
    static_set = {(route['method'], route['path']) for route in static}
    static_paths = {route['path'] for route in static}

    only_static = []
    for route in static:
        if (route['method'], route['path']) in runtime_set:
            continue
        reason = 'path present under another method label' if route['path'] in runtime_paths else 'not present at runtime (unmounted or factory-internal)'
        only_static.append({**route, 'reason': reason})

    only_runtime = []
    for route in runtime:
        if (route['method'], route['path']) in static_set:
            continue
        reason = 'path present under another method label' if route['path'] in static_paths else 'registered but absent from the static inventory'
        only_runtime.append({**route, 'reason': reason})

    payload = {
        'staticRegistrationRows': len(static),
        'runtimeRouteCount': len(runtime),
        'runtimeUniqueCount': enum['uniqueRouteCount'],
        'onlyStatic': only_static,
        'onlyRuntime': only_runtime,
        'onlyStaticCount': len(only_static),
        'onlyRuntimeCount': len(only_runtime),
        'note': 'Static inventory counts 264 registrations including the unmounted '
                'createUpdateRouter POST /api/update/restart; runtime stack sees 263.',
    }
    Path(args.out).write_text(json.dumps(payload, indent=2, ensure_ascii=False) + '\n')
    print(json.dumps({key: payload[key] for key in
          ('staticRegistrationRows', 'runtimeRouteCount', 'onlyStaticCount', 'onlyRuntimeCount')}, ensure_ascii=False))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
