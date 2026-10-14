#!/usr/bin/env python3
"""离线核对 A5.1 依赖账本；只读输入，标准库，无服务、网络或冻结文件写入。

PASS 表示账本结构/集合/锚点完整，不表示 BLOCKED 项已解决或原生实现通过。
默认同时运行负例自检；--ledger 支持验证待评审副本，--baseline 指定只读 B/。
"""
import argparse
from collections import Counter
import copy
import hashlib
import json
from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parents[2]
LEDGER = ROOT / 'specs/gateway-coverage/ledger.json'
INVENTORY = ROOT / 'docs/research/005_phase-a-gateway/method-inventory.md'
SCHEMA = ROOT / 'schema/gateway-protocol.schema.json'
SNAPSHOT = ROOT / 'schema/gateway-methods.snapshot.json'
DRIVER = ROOT / 'scripts/gateway-parity/driver.mts'
REF = re.compile(r'B/([A-Za-z0-9_./-]+):([0-9]+)')
CATEGORIES = {'(a)': 10, '(b)': 46, '(c)': 4, '待定': 41}
BASELINE_COMMIT = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85'
REQUIRED = {
    'method', 'frozen_category', 'work_package', 'owner', 'handler', 'provider',
    'state', 'notifications', 'branches', 'a6_dependencies', 'configuration',
    'lifecycle', 'scenario_ids', 'proposed_scenarios', 'status', 'blockers',
}
EVIDENCE_FIELDS = ('handler', 'provider', 'state', 'notifications', 'lifecycle')


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def default_baseline():
    # Works in the main checkout and in _build/wt-* without a machine-specific path.
    for parent in (ROOT, *ROOT.parents):
        candidate = parent / 'resources/PilotDeck'
        if candidate.is_dir():
            return candidate
    return ROOT / 'resources/PilotDeck'


def string_list(value, nonempty=False):
    return (isinstance(value, list) and (bool(value) or not nonempty)
            and all(isinstance(x, str) and x.strip() for x in value)
            and len(value) == len(set(value)))


def load_inputs():
    schema = json.loads(SCHEMA.read_text())
    snapshot = json.loads(SNAPSHOT.read_text())
    rows = re.findall(r'^\| (\d+) \| `([^`]+)` \| ([^|]+) \|', INVENTORY.read_text(), re.M)
    categories = {}
    for _, method, cell in rows:
        categories[method] = next((c for c in CATEGORIES if c in cell), '待定')
    groups = {}
    gap = (ROOT / 'docs/research/008_phase-a-session-domain/gap.md').read_text()
    for line in gap.splitlines():
        match = re.match(r'\| (C|S[1-8]|U\d+) ', line)
        if match:
            for method in re.findall(r'`([a-z_]+)`', line.split('|')[2]):
                if method in groups:
                    raise ValueError(f'duplicate work-package assignment: {method}')
                groups[method] = match.group(1)
    groups.update({m: 'A4' for m, c in categories.items() if c == '(a)'})
    driver = DRIVER.read_text()
    matches = list(re.finditer(r"args\.scenario === '([^']+)'", driver))
    scenarios = {}
    for index, match in enumerate(matches):
        end = matches[index + 1].start() if index + 1 < len(matches) else len(driver)
        body = driver[match.end():end]
        used = {m for m in categories if re.search(r"['\"]" + re.escape(m) + r"['\"]", body)}
        if 'remote.submitTurn(' in body:
            used.add('submit_turn')
        scenarios[match.group(1)] = used
    return {
        'schema': schema['definitions']['WsGatewayMethod']['enum'],
        'snapshot': snapshot['methods'],
        'inventory': [m for _, m, _ in rows],
        'inventory_numbers': [int(n) for n, _, _ in rows],
        'categories': categories, 'groups': groups, 'scenarios': scenarios,
        'protocol_sources': snapshot['source'],
    }


def validate(data, baseline, inputs):
    errors = []
    references = []
    referenced_files = set()
    file_cache = {}

    def fail(code, where, detail):
        errors.append({'code': code, 'path': where, 'detail': detail})

    def check_ref(value, where):
        if not isinstance(value, str) or not REF.fullmatch(value):
            fail('reference_format', where, 'expected B/relative/path:positive-line')
            return
        match = REF.fullmatch(value)
        relative, number = match.groups()
        referenced_files.add(relative)
        references.append(value)
        path = (baseline / relative).resolve()
        if not path.is_relative_to(baseline) or '..' in Path(relative).parts:
            fail('reference_path', where, 'reference escapes baseline')
            return
        if relative not in file_cache:
            try:
                raw = path.read_bytes()
                file_cache[relative] = (len(raw.decode('utf-8').splitlines()), hashlib.sha256(raw).hexdigest())
            except (OSError, UnicodeError) as exc:
                file_cache[relative] = None
                fail('reference_file', where, str(exc))
        cached = file_cache[relative]
        if cached is not None and not 1 <= int(number) <= cached[0]:
            fail('reference_line', where, f'{value}: line outside 1..{cached[0]}')

    def evidence(value, where):
        if not isinstance(value, dict) or not isinstance(value.get('summary'), str) or not value['summary'].strip():
            fail('evidence', where, 'nonempty summary required')
            return
        if not string_list(value.get('refs'), nonempty=True):
            fail('evidence', where, 'nonempty unique refs required')

    def walk(value, where='$'):
        if isinstance(value, dict):
            for key, child in value.items():
                if key == 'refs':
                    if not isinstance(child, list):
                        fail('reference_format', where + '.refs', 'refs must be a list')
                    else:
                        for i, item in enumerate(child):
                            check_ref(item, f'{where}.refs[{i}]')
                else:
                    walk(child, f'{where}.{key}')
        elif isinstance(value, list):
            for i, child in enumerate(value):
                walk(child, f'{where}[{i}]')
        elif isinstance(value, str) and re.search(r'B/\S+', value):
            # Also validate embedded prose anchors; malformed B/ anchors fail closed.
            found = REF.findall(value)
            if not found:
                fail('reference_format', where, 'malformed embedded baseline anchor')
            for path, number in found:
                check_ref(f'B/{path}:{number}', where)

    if not isinstance(data, dict):
        return [{'code': 'document', 'path': '$', 'detail': 'expected object'}], {}
    if data.get('format_version') != 1:
        fail('format_version', '$', 'expected format_version=1')
    if not isinstance(data.get('baseline'), dict) or data['baseline'].get('commit') != BASELINE_COMMIT:
        fail('baseline', '$.baseline', 'expected the schema-FROZEN baseline commit')
    for name, source in inputs['protocol_sources'].items():
        path = baseline / 'src/gateway/protocol' / name
        if not path.is_file() or digest(path) != source['sha256']:
            fail('baseline_protocol', name, 'baseline differs from frozen protocol source hash')
    scope = data.get('scope', {})
    if (not isinstance(scope, dict) or scope.get('total') != 101
            or scope.get('remaining_after_a4') != 91 or scope.get('category_counts') != CATEGORIES):
        fail('scope', '$.scope', 'expected 101 total, 91 non-A4, frozen 10/46/4/41 categories')
    for source in ('schema', 'snapshot', 'inventory'):
        values = inputs[source]
        if not string_list(values, nonempty=True) or len(values) != 101:
            fail('input_methods', source, 'expected 101 unique method strings')
    if inputs['inventory_numbers'] != list(range(1, 102)):
        fail('input_methods', 'inventory', 'row numbers must be 1..101')
    expected = set(inputs['schema'])
    if expected != set(inputs['snapshot']) or expected != set(inputs['inventory']):
        fail('input_methods', '$', 'schema, snapshot and inventory disagree')
    if dict(Counter(inputs['categories'].values())) != CATEGORIES:
        fail('input_categories', '$', 'frozen inventory category totals disagree')
    entries = data.get('methods')
    if not isinstance(entries, list):
        fail('methods', '$.methods', 'expected list')
        entries = []
    names = [entry.get('method') for entry in entries if isinstance(entry, dict)]
    strings = [name for name in names if isinstance(name, str)]
    duplicates = [name for name, count in Counter(strings).items() if count > 1]
    if duplicates:
        fail('duplicate_method', '$.methods', duplicates)
    if len(names) != len(entries) or len(strings) != len(entries) or set(strings) != expected:
        fail('method_set', '$.methods', {'missing': sorted(expected - set(strings)), 'extra': sorted(set(strings) - expected)})
    configuration = data.get('configuration', {})
    if not isinstance(configuration, dict) or not configuration:
        fail('configuration', '$.configuration', 'nonempty configuration catalog required')
        configuration = {}
    for key, value in configuration.items():
        evidence(value, f'$.configuration.{key}')
    for index, entry in enumerate(entries):
        where = f'$.methods[{index}]'
        if not isinstance(entry, dict):
            fail('entry', where, 'expected object')
            continue
        missing = REQUIRED - entry.keys()
        for key in sorted(missing):
            fail('required_field', where + '.' + key, 'missing required field')
        method = entry.get('method')
        if not isinstance(method, str):
            continue
        if entry.get('frozen_category') != inputs['categories'].get(method):
            fail('frozen_category', where, method)
        if entry.get('work_package') != inputs['groups'].get(method):
            fail('work_package', where, method)
        owner = entry.get('owner')
        if owner not in ('A4', 'A6', 'A7', 'A8'):
            fail('owner', where, 'owner must be A4/A6/A7/A8')
        for field in EVIDENCE_FIELDS:
            evidence(entry.get(field), where + '.' + field)
        state = entry.get('state', {})
        kinds = state.get('kinds') if isinstance(state, dict) else None
        if not string_list(kinds, True) or not set(kinds).issubset({'storage', 'memory', 'external', 'none'}):
            fail('state', where, 'state.kinds must identify storage/memory/external/none')
        notification = entry.get('notifications', {})
        if not isinstance(notification, dict) or not string_list(notification.get('names')) or not notification.get('scope'):
            fail('notifications', where, 'names and scope required, empty names means no identified notification')
        branches = entry.get('branches', {})
        for kind in ('success', 'error', 'unconfigured'):
            branch = branches.get(kind) if isinstance(branches, dict) else None
            if not isinstance(branch, list) or not branch:
                fail('branches', where + '.' + kind, 'at least one evidenced branch required')
                continue
            for j, item in enumerate(branch):
                evidence(item, f'{where}.branches.{kind}[{j}]')
                if kind != 'success' and (not isinstance(item, dict) or not isinstance(item.get('code'), str) or not item['code'].strip()):
                    fail('branches', where + '.' + kind, 'explicit error/result code or N/A required')
        deps = entry.get('a6_dependencies')
        if not string_list(deps) or not set(deps).issubset({f'A6.{i}' for i in range(1, 10)}):
            fail('dependencies', where, 'unknown/duplicate A6 dependency')
        config = entry.get('configuration')
        if not string_list(config) or not set(config).issubset(configuration):
            fail('configuration', where, 'unknown/duplicate configuration reference')
        ids = entry.get('scenario_ids')
        proposed = entry.get('proposed_scenarios')
        if not string_list(ids, True):
            fail('scenarios', where, 'nonempty unique scenario_ids required; use NONE if absent')
        else:
            if 'NONE' in ids and ids != ['NONE']:
                fail('scenarios', where, 'NONE cannot coexist with existing IDs')
            for scenario in ids:
                if scenario != 'NONE' and method not in inputs['scenarios'].get(scenario, set()):
                    fail('scenarios', where, f'{scenario} does not invoke {method} in frozen A2 driver')
        if not string_list(proposed) or any(not re.fullmatch(r'a51\.' + re.escape(method) + r'\.[a-z_]+', x) for x in proposed):
            fail('scenarios', where, 'proposed scenario must be a51.<method>.<case>')
        if ids == ['NONE'] and not proposed:
            fail('scenarios', where, 'NONE requires proposed scenarios')
        status, blockers = entry.get('status'), entry.get('blockers')
        if status not in ('READY', 'BLOCKED'):
            fail('status', where, 'unknown status')
        if not isinstance(blockers, list):
            fail('status', where, 'blockers must be a list')
        elif status == 'READY' and blockers:
            fail('status', where, 'READY cannot contain blockers')
        elif status == 'BLOCKED' and not blockers:
            fail('status', where, 'BLOCKED requires missing evidence and retrieval plan')
        elif status == 'BLOCKED':
            for blocker in blockers:
                if not isinstance(blocker, dict) or any(not isinstance(blocker.get(k), str) or not blocker[k].strip() for k in ('missing', 'obtain')):
                    fail('status', where, 'each blocker needs missing and obtain')
        if status == 'READY' and not (isinstance(ids, list) and any(x != 'NONE' for x in ids) or proposed):
            fail('scenarios', where, 'READY needs existing or proposed scenario')
    bindings = data.get('external_bindings')
    if not isinstance(bindings, dict) or not bindings:
        fail('external_bindings', '$', 'nonempty provider/cleanup catalog required')
    else:
        for key, value in bindings.items():
            if not isinstance(value, dict):
                fail('external_bindings', key, 'expected object')
                continue
            for field in ('provider', 'lifecycle'):
                evidence(value.get(field), f'$.external_bindings.{key}.{field}')
            if value.get('status') == 'BLOCKED' and not all(isinstance(value.get(k), str) and value[k].strip() for k in ('missing', 'obtain')):
                fail('external_bindings', key, 'BLOCKED requires missing/obtain')
    walk(data)
    source_files = data.get('source_files', {})
    if not isinstance(source_files, dict) or set(source_files) != referenced_files:
        fail('source_files', '$', 'source_files must exactly cover referenced baseline files')
    if isinstance(source_files, dict):
        for relative in referenced_files:
            recorded = source_files.get(relative)
            actual = file_cache.get(relative)
            if actual and (not isinstance(recorded, dict) or recorded.get('line_count') != actual[0] or recorded.get('sha256') != actual[1]):
                fail('source_drift', relative, 'baseline file differs from the audited ledger source')
    files = (SCHEMA, SNAPSHOT, INVENTORY, DRIVER)
    wanted_hashes = {str(p.relative_to(ROOT)): digest(p) for p in files}
    if data.get('input_files') != wanted_hashes:
        fail('input_drift', '$.input_files', 'schema/snapshot/inventory/driver changed since audit')
    summary = {
        'method_count': len(entries), 'expected_count': len(expected),
        'categories': dict(Counter(x.get('frozen_category') for x in entries if isinstance(x, dict))),
        'by_status': dict(Counter(x.get('status') for x in entries if isinstance(x, dict))),
        'by_owner': dict(Counter(x.get('owner') for x in entries if isinstance(x, dict))),
        'baseline_files': len(referenced_files), 'unique_references': len(set(references)),
        'existing_a2_methods': sum(x.get('scenario_ids') != ['NONE'] for x in entries if isinstance(x, dict)),
    }
    return errors, summary


def self_test(data, baseline, inputs):
    """Mutations are in memory only and must fail for the intended reason."""
    cases = []
    deleted = copy.deepcopy(data)
    deleted['methods'].pop()
    cases.append(('delete_method', deleted, 'method_set'))
    duplicate = copy.deepcopy(data)
    duplicate['methods'].append(copy.deepcopy(duplicate['methods'][0]))
    cases.append(('duplicate_method', duplicate, 'duplicate_method'))
    out_of_bounds = copy.deepcopy(data)
    anchor = out_of_bounds['methods'][0]['handler']['refs'][0]
    relative = REF.fullmatch(anchor).group(1)
    line_count = len((baseline / relative).read_text().splitlines())
    out_of_bounds['methods'][0]['handler']['refs'][0] = f'B/{relative}:{line_count + 1}'
    cases.append(('out_of_bounds_line', out_of_bounds, 'reference_line'))
    missing_owner = copy.deepcopy(data)
    del missing_owner['methods'][0]['owner']
    cases.append(('missing_owner', missing_owner, 'required_field'))
    empty_scenarios = copy.deepcopy(data)
    empty_scenarios['methods'][0]['scenario_ids'] = ['NONE']
    empty_scenarios['methods'][0]['proposed_scenarios'] = []
    cases.append(('missing_scenarios', empty_scenarios, 'scenarios'))
    changed_category = copy.deepcopy(data)
    changed_category['methods'][0]['frozen_category'] = '(a)'
    cases.append(('changed_category', changed_category, 'frozen_category'))
    unknown_scenario = copy.deepcopy(data)
    unknown_scenario['methods'][0]['scenario_ids'] = ['does_not_exist']
    cases.append(('unknown_scenario', unknown_scenario, 'scenarios'))
    results = []
    for name, mutated, expected_code in cases:
        errors, _ = validate(mutated, baseline, inputs)
        detected = any(e['code'] == expected_code for e in errors)
        results.append({'name': name, 'expected': 'FAIL', 'observed': 'FAIL' if errors else 'PASS',
                        'required_error': expected_code, 'result': 'PASS' if detected else 'FAIL'})
    return results


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--ledger', type=Path, default=LEDGER)
    parser.add_argument('--baseline', type=Path, default=default_baseline())
    args = parser.parse_args()
    try:
        data = json.loads(args.ledger.read_text())
        inputs = load_inputs()
        baseline = args.baseline.resolve()
        failures, summary = validate(data, baseline, inputs)
        controls = self_test(data, baseline, inputs) if not failures else []
        failed = bool(failures) or any(c['result'] != 'PASS' for c in controls)
        report = {'status': 'FAIL' if failed else 'PASS', 'scope': 'static_coverage_ledger',
                  **summary, 'failures': failures, 'negative_self_tests': controls}
    except (OSError, UnicodeError, ValueError, KeyError, TypeError) as exc:
        report = {'status': 'FAIL', 'failures': [{'code': 'input', 'detail': str(exc)}], 'negative_self_tests': []}
        failed = True
    print(json.dumps(report, ensure_ascii=False, indent=2))
    return int(failed)


if __name__ == '__main__':
    sys.exit(main())
