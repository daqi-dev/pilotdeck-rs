#!/usr/bin/env python3
"""离线核对 A5.1 依赖账本；只读输入，Python + 已安装 Node/TypeScript parser，无服务、网络或冻结文件写入。

PASS 表示账本结构/集合/锚点完整，不表示 BLOCKED 项已解决或原生实现通过。
默认同时运行负例自检；v2 组/范围先展开，既有检查全部保留。
--ledger 支持待评审副本，--baseline 指定只读 B/；--compare-expanded 作一次性逐项对照。
"""
import argparse
from collections import Counter
import copy
import hashlib
from functools import lru_cache
import json
import os
from pathlib import Path
import re
import sys
import subprocess

SCRIPT_DIR = Path(__file__).resolve().parent
if str(SCRIPT_DIR) not in sys.path:
    # ``python3 -I`` (isolated mode) does not put the script directory on
    # sys.path, so the sibling modules below need an explicit path.
    sys.path.insert(0, str(SCRIPT_DIR))

from ledger_format import expand, read_json, unique_object  # noqa: E402
from strict_compare import strict_equal  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
LEDGER = ROOT / 'specs/gateway-coverage/ledger.json'
INVENTORY = ROOT / 'docs/research/005_phase-a-gateway/method-inventory.md'
SCHEMA = ROOT / 'schema/gateway-protocol.schema.json'
SNAPSHOT = ROOT / 'schema/gateway-methods.snapshot.json'
DRIVER = ROOT / 'scripts/gateway-parity/driver.mts'
REF = re.compile(r'B/([A-Za-z0-9_./-]+):([0-9]+)')
PROSE_REF = re.compile(r'B/[A-Za-z0-9_./-]+(?::[^\s，。；、）)\]}`]+)?')
SOURCE_MENTION = re.compile(r'(?:[A-Za-z0-9_.-]+/)*[A-Za-z0-9_.-]+\.(?:tsx?|mts|mjs|js)(?![A-Za-z0-9_])'
                            r'(?::[^\s，。；、）)\]}`]+)?')
UNCONFIGURED = {'CAPABILITY_UNAVAILABLE', 'not_configured', 'SKILL_MODULE_DISABLED', 'unsupported'}
DYNAMIC_CODE = re.compile(r'(?:<[^<>\s]+>|N/A|ERR_[A-Z0-9]+(?:_[A-Z0-9]+)*)')
# Exact source/value exceptions, reviewed independently of ledger authors.
# This field is a telemetry label (Error.name), not an RPC/domain code.
CODE_LITERAL_EXEMPTIONS = {
    ('src/tool/builtin/executeCode.ts', 198, 'string'):
        'inputSchema.properties.code describes Python source input, not a domain error code.',
    ('src/tool/builtin/executeCode.ts', 198,
     'Python 3 source code to execute. Use `from pilotdeck_tools import ...` to call allowed PilotDeck tools.'):
        'inputSchema.properties.code.description is schema prose, not a domain error code.',

    ('src/cli/ProjectMemoryMaintenanceController.ts', 94, 'UnknownError'):
        'Memory maintenance catch reports Error.name / UnknownError to telemetry; it does not emit a domain code.',
    ('src/agent/modules/transport/stdioAgentLoopSidecarConnection.ts', 184, 'null'):
        'ExitStatus.code is a process exit number or null on startup failure, not a domain error code.',
    ('src/extension/plugins/builtin/funasr/funasr-local-mcp.mjs', 127, '127'):
        'FunASR launcher status.code is a process exit number, not a domain error code.',
}



def beyond_depth(entries, graph):
    """Review-only full traversal; never adds required ranges or error records.

    Hops are shortest call-edge distances from the actual RPC handler. Reporting
    starts beyond the checker-owned bound and omits code names by design.
    """
    from collections import deque
    result = []
    for entry in entries:
        route = graph['routes'].get(entry.get('method'))
        if not route:
            continue
        recorded = {b.get('code') for b in entry.get('branches', {}).get('error', []) if isinstance(b, dict)}
        reached, pending = {route['handler']: 0}, deque([route['handler']])
        while pending:
            fid = pending.popleft()
            for call in graph['functions'][fid]['calls']:
                for target in call['targets']:
                    if target not in reached:
                        reached[target] = reached[fid] + 1
                        pending.append(target)
        code_depths = {}
        for fid, depth in reached.items():
            if depth <= graph['depth']:
                continue
            for error in graph['functions'][fid]['errors']:
                code = error['code']
                if code not in recorded:
                    code_depths[code] = min(depth, code_depths.get(code, depth))
        if code_depths:
            result.append({'method': entry['method'], 'first_hop': min(code_depths.values()),
                           'hops': sorted(set(code_depths.values()))})
    return sorted(result, key=lambda row: row['method'])
CATEGORIES = {'(a)': 10, '(b)': 46, '(c)': 4, '待定': 41}
BASELINE_COMMIT = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85'
REQUIRED = {
    'method', 'frozen_category', 'work_package', 'owner', 'handler', 'provider',
    'state', 'notifications', 'branches', 'a6_dependencies', 'configuration',
    'lifecycle', 'scenario_ids', 'proposed_scenarios', 'status', 'blockers',
    'source_audit', 'guard',
}
EVIDENCE_FIELDS = ('handler', 'provider', 'state', 'notifications', 'lifecycle', 'guard')


@lru_cache(maxsize=2)
def source_graph(baseline):
    """The installed TS parser matches actual blocks, expressions and bindings.

    No product module is executed. Missing Node/parser or analysis failures are
    fatal. Cache only within one validation process, never from a ledger file.
    """
    result = subprocess.run(['node', str(ROOT / 'scripts/gateway-coverage/source_graph.cjs'),
                             str(baseline)], capture_output=True, text=True, timeout=120)
    if result.returncode:
        raise ValueError('independent source analysis failed: ' + result.stderr[-2000:])
    return json.loads(result.stdout)


# Exceptions belong to the checker, not the ledger author. All have actual
# handler evidence; these have no separate callable provider in the CLI graph.
PROVIDER_EXCEPTIONS = {
    'manager_browsers': 'CLI does not supply managerBrowsers; host-only optional capability, no production constructor.',
    'resume_session': 'Identity response implemented entirely in IPG; no downstream provider call.',
    'new_session': 'IPG UUID response; node:crypto.randomUUID is an external builtin, no repository function.',
    'supported_agents': 'Static IPG response; no downstream provider call.',
}


def expected_owner(package):
    # Plan 005: A4 10; A7 C/S1-S8 + U9/U10; A8 other U groups.
    if package == 'A4':
        return 'A4'
    if package in {'C', 'U9', 'U10', *(f'S{i}' for i in range(1, 9))}:
        return 'A7'
    if package in {f'U{i}' for i in range(1, 14)}:
        return 'A8'
    return None


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def default_baseline():
    # Works in the main checkout and in _build/wt-* without a machine-specific path.
    env = os.environ.get('PDRS_BASELINE')
    if env:
        return Path(env)
    for parent in (ROOT, *ROOT.parents):
        candidate = parent / 'resources/PilotDeck'
        if candidate.is_dir():
            return candidate
    return ROOT / 'resources/PilotDeck'


def string_list(value, nonempty=False):
    return (isinstance(value, list) and (bool(value) or not nonempty)
            and all(isinstance(x, str) and x.strip() for x in value)
            and len(value) == len(set(value)))


def producing_line_origin(expression, source_line, node, number):
    return (not expression.lstrip().startswith(('//', '/*', '*'))
            and expression == source_line.strip()
            and node['start'] == node['end'] == number
            and node['expression'] in expression)


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


def validate(data, baseline, inputs, graph=None):
    data, errors = expand(data)
    if isinstance(data, dict) and not strict_equal(data.get('format_version'), 1):
        # ``expand`` rejects a compact ledger whose format_version is not the
        # exact int 2 (2.0 / True); stop before the v2-only shapes reach
        # validate() and raise AttributeError instead of a reported failure.
        if not any(error['code'] == 'format_version' for error in errors):
            errors.append({'code': 'format_version', 'path': '$',
                           'detail': 'expected format_version=1'})
        return errors, {}
    references = []
    referenced_files = set()
    file_cache = {}
    source_lines = {}

    def lines(relative):
        if relative not in source_lines:
            source_lines[relative] = (baseline / relative).read_text().splitlines()
        return source_lines[relative]
    graph = graph if graph is not None else source_graph(baseline)
    functions = graph['functions']
    function_index = {}
    default_classes = {p['class'] for p in graph['default_construction_points']}
    for function in functions:
        function_index.setdefault((function['file'], function['start']), []).append(function)
    automatic_count = 0
    provider_count = 0
    audited_ranges = set()
    method_function_count = 0

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
                elif key == 'file' and (where.startswith('$.error_classes.') or where == '$.default_boot'):
                    # Source-derived metadata has a separately checked refs field.
                    continue
                elif key == 'expression' and where.endswith('.origin'):
                    # Exact source text, verified against its origin line below.
                    continue
                else:
                    walk(child, f'{where}.{key}')
        elif isinstance(value, list):
            for i, child in enumerate(value):
                walk(child, f'{where}[{i}]')
        elif isinstance(value, str):
            for mention in SOURCE_MENTION.finditer(value):
                if not REF.fullmatch(mention.group()):
                    fail('reference_format', where, 'source prose requires full B/path:line: ' + mention.group())
            if 'B/' not in value:
                return
            # Validate EVERY occurrence: one valid anchor must not conceal a bare
            # path, malformed line suffix or another invalid anchor in the prose.
            found = list(PROSE_REF.finditer(value))
            if len(found) != value.count('B/'):
                fail('reference_format', where, 'malformed embedded baseline anchor')
            for match in found:
                check_ref(match.group(), where)

    if not isinstance(data, dict):
        return [{'code': 'document', 'path': '$', 'detail': 'expected object'}], {}
    if any(control['result'] != 'PASS' for control in graph['parser_controls']):
        fail('parser_controls', '$', graph['parser_controls'])
    if not isinstance(data.get('baseline'), dict) or data['baseline'].get('commit') != BASELINE_COMMIT:
        fail('baseline', '$.baseline', 'expected the schema-FROZEN baseline commit')
    for name, source in inputs['protocol_sources'].items():
        path = baseline / 'src/gateway/protocol' / name
        if not path.is_file() or digest(path) != source['sha256']:
            fail('baseline_protocol', name, 'baseline differs from frozen protocol source hash')
    scope = data.get('scope', {})
    if (not isinstance(scope, dict) or not strict_equal(scope.get('total'), 101)
            or not strict_equal(scope.get('remaining_after_a4'), 91)
            or not strict_equal(scope.get('category_counts'), CATEGORIES)):
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
    if data.get('provider_exceptions') != PROVIDER_EXCEPTIONS:
        fail('provider_exceptions', '$.provider_exceptions', 'must exactly match reviewed checker-owned reasons')
    catalog = {name: dict(value, refs=[f"B/{value['file']}:{value['line']}"]) for name, value in graph['errorClasses'].items()}
    if not strict_equal(data.get('error_classes'), catalog):
        fail('error_class_catalog', '$.error_classes', 'all production Error classes, inherited code and origins must match source')
    if not strict_equal(data.get('default_boot'), dict(graph['boot'], refs=[f"B/{graph['boot']['file']}:{graph['boot']['line']}"])):
        fail('default_construction', '$.default_boot', 'default root must be CLI createLocalGateway call')
    transport = next(f for f in functions if f['file'] == 'src/gateway/server/GatewayWsConnection.ts' and f['symbol'] == 'handleRequest')
    expected_transport = {'summary': '所有 RPC 的共同 WS 错误响应边界；只用于运输码字面量与动态 origin 核验，不把 switch 的其他方法领域码归给本方法。',
                          'refs': [f"B/{transport['file']}:{transport['start']}", f"B/{transport['file']}:{transport['end']}"]}
    if data.get('transport_boundary') != expected_transport:
        fail('transport_scope', '$.transport_boundary', 'common response boundary must match independently bounded function')
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
        if owner not in ('A4', 'A7', 'A8'):
            fail('owner', where, 'method owner must be A4/A7/A8; A6 is a dependency')
        if owner != expected_owner(inputs['groups'].get(method)):
            fail('owner_mapping', where, 'owner disagrees with plan 005 work-package mapping')
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
            if not isinstance(branch, list) or (kind != 'error' and not branch):
                fail('branches', where + '.' + kind, 'at least one evidenced branch required')
                continue
            for j, item in enumerate(branch):
                evidence(item, f'{where}.branches.{kind}[{j}]')
                if kind != 'success' and (not isinstance(item, dict) or not isinstance(item.get('code'), str) or not item['code'].strip()):
                    fail('branches', where + '.' + kind, 'explicit error/result code or N/A required')
        audit = entry.get('source_audit')
        if not isinstance(audit, dict) or not isinstance(audit.get('functions'), list) or not audit['functions']:
            fail('source_audit', where, 'independently bounded handler/provider/callee ranges required')
        else:
            route = graph['routes'].get(method)
            if route is None:
                fail('provider_scope', where, 'WS to IPG route not discovered')
                continue
            reached = [functions[x['id']] for x in route['functions']]
            reached_keys = {(f['file'], f['start'], f['end']) for f in reached}
            automatic_count += len(reached)
            providers = [functions[i] for i in route['providers']]
            provider_count += bool(providers)
            constructed_classes = {point['class'] for point in route['construction_points']}
            for provider in providers:
                if provider['class'] and provider['class'] not in constructed_classes:
                    fail('default_construction', where, 'provider class has no production construction point: ' + provider['class'])
            expected_providers = sorted({f"B/{f['file']}:{f['start']}" for f in providers})
            if entry.get('provider', {}).get('default_refs') != expected_providers:
                fail('default_construction', where, 'provider.default_refs must equal independently discovered default providers')
            if audit.get('automatic_providers') != expected_providers:
                fail('provider_scope', where, {'expected_automatic_providers': expected_providers})
            if not providers and method not in PROVIDER_EXCEPTIONS:
                fail('provider_scope', where, 'no provider discovered and no reviewed exception')
            if not strict_equal(audit.get('depth'), graph['depth']):
                fail('callee_scope', where, 'reported callee depth differs from checker-owned bound')
            if audit.get('default_constructions') != route['construction_points']:
                fail('default_construction', where, 'default provider construction points disagree with source graph')
            recorded_codes = {b.get('code') for b in branches.get('error', []) if isinstance(b, dict)}
            scanned_codes, literals, roles, seen_ranges = set(), set(), set(), set()
            scanned = {f['id']: f for f in reached}
            for i, scope in enumerate(audit['functions']):
                location = f'{where}.source_audit.functions[{i}]'
                if not isinstance(scope, dict):
                    fail('source_audit', location, 'expected function scope object')
                    continue
                refs = scope.get('refs', [])
                if not string_list(refs, True) or len(refs) not in (1, 2) or any(not REF.fullmatch(r) for r in refs):
                    fail('source_audit', location, 'function start/end refs required')
                    continue
                start, end = REF.fullmatch(refs[0]), REF.fullmatch(refs[-1])
                path, first, last = start[1], int(start[2]), int(end[2])
                matches = [f for f in function_index.get((path, first), []) if f['symbol'] == scope.get('symbol')]
                # Multiple callbacks can start on one line; the containing callable
                # with that symbol is the reviewed scope, never a self-reported end.
                actual = max(matches, key=lambda f: f['end']) if matches else None
                if actual is None:
                    fail('source_audit', location, 'start/symbol is not an independently discovered function')
                    continue
                expected_assembly = ('code_definition' if actual.get('kind') == 'error_class' else
                                     'automatic_default' if (actual['file'], actual['start'], actual['end']) in reached_keys else
                                     'optional_host' if ('FileGatewayUserDialogStore' in actual['file'] or
                                         scope.get('role') == 'provider' and
                                         (actual['id'] not in graph['default_functions'] or
                                          actual['class'] and actual['class'] not in default_classes))
                                     else 'supplemental_review')
                if scope.get('assembly') != expected_assembly:
                    fail('default_construction', location, {'expected_assembly': expected_assembly})
                if scope.get('optional') is True and (actual['id'] in graph['default_functions'] or
                                                      (actual['file'], actual['start'], actual['end']) in reached_keys):
                    fail('optional_default_scope', location, 'optional=true is forbidden on a default-reachable function')
                if expected_assembly == 'optional_host' and scope.get('optional') is not True:
                    fail('default_construction', location, 'provider outside default construction graph requires optional=true')
                actual_last = actual['end']
                key = (path, first, actual_last)
                if key in seen_ranges:
                    fail('source_audit', location, 'duplicate function range')
                seen_ranges.add(key)
                audited_ranges.add(key)
                method_function_count += 1
                role = scope.get('role')
                roles.add(role)
                if role not in ('handler', 'provider', 'callee'):
                    fail('source_audit', location, 'handler/provider/callee role required')
                if end[1] != path or last != actual_last:
                    fail('source_audit', location, {'independent_end': actual_last, 'reported_end': last})
                source = '\n'.join(lines(path)[first - 1:actual_last])
                if scope.get('sha256') != hashlib.sha256(source.encode()).hexdigest():
                    fail('source_audit', location, 'hash differs from independently bounded complete source')
                scanned[actual['id']] = actual
            if 'handler' not in roles:
                fail('source_audit', where, 'handler scope missing')
            required_ranges = {(f['file'], f['start'], f['end']) for f in reached}
            missing_ranges = required_ranges - seen_ranges
            if missing_ranges:
                fail('callee_scope', where, {'missing_automatic_ranges': sorted(missing_ranges)})
            if any((f['file'], f['start'], f['end']) not in seen_ranges for f in providers):
                fail('provider_scope', where, 'automatic provider missing from ranges')
            handler = functions[route['handler']]
            if (handler['file'], handler['start'], handler['end']) not in seen_ranges:
                fail('provider_scope', where, 'real WS handler missing from ranges')
            for function in scanned.values():
                scanned_codes.update(x['code'] for x in function['errors'])
                literals.update(x['value'] for x in function['literals'])
                for invalid in function.get('invalid_code_literals', []):
                    key = (function['file'], invalid['line'], invalid['code'])
                    if key not in CODE_LITERAL_EXEMPTIONS:
                        fail('invalid_produced_code', where, dict(invalid, file=function['file']))
                if function['unknown_errors']:
                    fail('unknown_error_class', where, function['unknown_errors'])
            # The common, independently bounded WS response boundary supplies
            # transport literals/origins, without attributing every WS switch
            # branch to every method's provider domain errors.
            scanned[transport['id']] = transport
            for invalid in transport.get('invalid_code_literals', []):
                if (transport['file'], invalid['line'], invalid['code']) not in CODE_LITERAL_EXEMPTIONS:
                    fail('invalid_produced_code', where, dict(invalid, file=transport['file']))
            literals.update(x['value'] for x in transport['literals'])
            # Only this RPC's independently parsed WS case, never another
            # method's switch branch, supplies optional-method result literals.
            literals.update(graph['ws_branch_literals'].get(method, []))
            domain = scanned_codes - UNCONFIGURED
            if domain and recorded_codes.issubset(UNCONFIGURED | {'N/A', 'gateway_request_failed'}):
                fail('provider_domain_errors', where, {'missing_domain_codes': sorted(domain - recorded_codes)})
            if scanned_codes - recorded_codes:
                fail('error_code_coverage', where, {'missing_scanned_codes': sorted(scanned_codes - recorded_codes)})
            # Every unresolved production has its own origin obligation. A
            # dynamic entry at the WS catch cannot cover an unrelated provider.
            dynamic_origins = {}
            for branch in branches.get('error', []):
                if not isinstance(branch, dict) or branch.get('dynamic') is not True:
                    continue
                origin = branch.get('origin', {})
                expression = origin.get('expression')
                if not isinstance(expression, str):
                    continue
                for ref in origin.get('refs', []):
                    match = REF.fullmatch(ref) if isinstance(ref, str) else None
                    if match:
                        dynamic_origins.setdefault(match[1], []).append((int(match[2]), expression))
            for function in scanned.values():
                for production in function.get('dynamic_code_expressions', []):
                    if not any(production['start'] <= number <= production['end'] and
                               (expression == production['expression'] or
                                producing_line_origin(expression, lines(function['file'])[number - 1],
                                                      production, number))
                               for number, expression in dynamic_origins.get(function['file'], [])):
                        fail('dynamic_code_coverage', where,
                             dict(production, file=function['file']))
            # Reverse inclusion rejects invented codes. Nonliteral codes/results
            # require a precise producing expression inside a scanned function.
            for kind in ('error', 'unconfigured'):
                for j, branch in enumerate(branches.get(kind, [])):
                    location = f'{where}.branches.{kind}[{j}]'
                    code = branch.get('code')
                    if branch.get('dynamic') is True:
                        if (not isinstance(code, str) or not DYNAMIC_CODE.fullmatch(code) or
                                code.startswith('ERR_') and code not in graph['node_errno'] and code not in literals):
                            fail('dynamic_code_shape', location,
                                 'dynamic code must be <placeholder>, N/A, or a source literal / Node constants ERR_* key')
                        origin = branch.get('origin', {})
                        refs = origin.get('refs', [])
                        if not string_list(refs, True) or not isinstance(origin.get('expression'), str) or not origin['expression'].strip():
                            fail('dynamic_code_origin', location, 'dynamic code requires producing refs and expression')
                        else:
                            for ref in refs:
                                match = REF.fullmatch(ref)
                                if not match or not any(f['file'] == match[1] and f['start'] <= int(match[2]) <= f['end'] for f in scanned.values()):
                                    fail('dynamic_code_origin', location, 'origin outside independently scanned functions: ' + str(ref))
                                elif not any(p['start'] <= int(match[2]) <= p['end'] and
                                             (p['expression'] == origin['expression'] or
                                              producing_line_origin(origin['expression'],
                                                                    lines(match[1])[int(match[2]) - 1],
                                                                    p, int(match[2])))
                                             for f in scanned.values() if f['file'] == match[1]
                                             for p in f.get('code_expressions', [])):
                                    fail('dynamic_code_origin', location, 'expression must equal a producing AST node or a whole line containing that node: ' + ref)
                    elif code == 'N/A':
                        # Absence of a domain error is a result annotation, never a
                        # dynamic code production. Verify its original return AST.
                        origin = branch.get('origin', {})
                        refs = origin.get('refs', [])
                        if (origin.get('kind') != 'no_domain_error' or not string_list(refs, True) or
                                not all(REF.fullmatch(ref) and any(
                                    f['file'] == REF.fullmatch(ref)[1] and not f['errors'] and
                                    any(p['start'] == int(REF.fullmatch(ref)[2]) and
                                        (p['expression'] == origin.get('expression') or
                                         lines(f['file'])[p['start'] - 1].strip() == origin.get('expression'))
                                        for p in f.get('result_expressions', []))
                                    for f in scanned.values()) for ref in refs)):
                            fail('non_error_result', location, 'N/A requires an independently scanned return without direct domain errors')
                    elif code not in literals:
                        fail('invented_error_code', location, {'code': code})
            tags = set()
            if any(f['class'] == 'InProcessGateway' and f['symbol'] == 'closeSession' for f in reached):
                tags.update(('session_permissions_removed', 'permission_mode_cleared', 'reconnect_owner_disposed', 'dialog_project_key_deleted'))
            if any(f['class'] == 'SessionRouter' and f['symbol'] == 'close' for f in reached):
                tags.update(('agent_evicted', 'permission_mode_cleared'))
            if any(f['symbol'] == 'onSessionEvict' for f in reached):
                tags.add('permission_mode_cleared')
            if tags - set(state.get('effects', [])):
                fail('close_state_effects', where, {'missing': sorted(tags - set(state.get('effects', [])))})
            if any(f['class'] == 'ProjectRuntimeRegistry' and f['symbol'] == 'resolve' for f in reached):
                if 'lazy_project_runtime_and_watcher' not in entry.get('lifecycle', {}).get('effects', []):
                    fail('lazy_runtime_lifecycle', where, 'registry.resolve can build/cache runtime and activate watcher')
            # An unconstructed class may be supplemental host evidence, never a
            # default provider. Structured optional=true is mandatory for it.
            for scope in audit['functions']:
                if scope.get('symbol') and any('FileGatewayUserDialogStore' in ref for ref in scope.get('refs', [])) and scope.get('optional') is not True:
                    fail('default_construction', where, 'FileGatewayUserDialogStore has no default CLI construction; mark optional=true')
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
            if actual and (not isinstance(recorded, dict)
                           or not strict_equal(recorded.get('line_count'), actual[0])
                           or not strict_equal(recorded.get('sha256'), actual[1])):
                fail('source_drift', relative, 'baseline file differs from the audited ledger source')
    files = (SCHEMA, SNAPSHOT, INVENTORY, DRIVER,
             ROOT / 'docs/research/008_phase-a-session-domain/gap.md',
             ROOT / 'docs/plan/005_phase-a-session-domain.md')
    wanted_hashes = {str(p.relative_to(ROOT)): digest(p) for p in files}
    if not strict_equal(data.get('input_files'), wanted_hashes):
        fail('input_drift', '$.input_files', 'schema/snapshot/inventory/driver changed since audit')
    summary = {
        'parser': graph['parser'], 'callee_depth': graph['depth'],
        'code_factory_declarations': len(graph['factory_inventory']),
        'code_literal_exemptions': [{'file': p, 'line': n, 'value': v, 'reason': reason}
                                    for (p, n, v), reason in CODE_LITERAL_EXEMPTIONS.items()],
        'automatic_function_associations': automatic_count, 'automatic_provider_methods': provider_count,
        'provider_exception_methods': len(PROVIDER_EXCEPTIONS),
        'method_count': len(entries), 'expected_count': len(expected),
        'categories': dict(Counter(x.get('frozen_category') for x in entries if isinstance(x, dict))),
        'by_status': dict(Counter(x.get('status') for x in entries if isinstance(x, dict))),
        'by_owner': dict(Counter(x.get('owner') for x in entries if isinstance(x, dict))),
        'baseline_files': len(referenced_files), 'unique_references': len(set(references)),
        'existing_a2_methods': sum(x.get('scenario_ids') != ['NONE'] for x in entries if isinstance(x, dict)),
        'method_function_ranges': method_function_count,
        'unique_function_ranges': len(audited_ranges),
        'beyond_depth_review': beyond_depth(entries, graph) if not errors else [],
    }
    return errors, summary


def self_test(data, baseline, inputs):
    """Mutations are in memory only and must fail for the intended reason."""
    compact_data = data
    data, expansion_errors = expand(data)
    if expansion_errors:
        raise ValueError(str(expansion_errors))
    graph = source_graph(baseline)
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
    # The proposed-id gate in validate() is separate from the scenario_ids
    # branch; a numeric case suffix must be rejected here too (the manifest
    # validator already has the same paired control).
    numeric_proposed = copy.deepcopy(data)
    numeric_entry = numeric_proposed['methods'][0]
    numeric_entry['proposed_scenarios'].append('a51.%s.case2' % numeric_entry['method'])
    cases.append(('numeric_proposed_id', numeric_proposed, 'scenarios'))
    def mutate_method(name, field, value):
        mutated = copy.deepcopy(data)
        next(e for e in mutated['methods'] if e['method'] == name)[field] = value
        return mutated
    for name, owner in [('native_archive_artifact', 'A7'), ('restore_session_transcript', 'A8'),
                        ('memory_list', 'A7'), ('hook_async_result', 'A8'),
                        ('read_subagent_messages', 'A8'), ('memory_list', 'A6')]:
        cases.append((f'wrong_owner_{name}_{owner}', mutate_method(name, 'owner', owner), 'owner_mapping'))
    omitted = copy.deepcopy(data)
    restore = next(e for e in omitted['methods'] if e['method'] == 'restore_session_transcript')
    restore['branches']['error'] = [copy.deepcopy(restore['branches']['unconfigured'][0])]
    cases.append(('provider_only_unconfigured', omitted, 'provider_domain_errors'))
    single = copy.deepcopy(data)
    artifact = next(e for e in single['methods'] if e['method'] == 'native_archive_artifact')
    artifact['branches']['error'] = [b for b in artifact['branches']['error'] if b['code'] != 'ARCHIVE_ARTIFACT_NOT_FOUND']
    cases.append(('missing_one_provider_code', single, 'error_code_coverage'))
    for suffix, bad in [('bare', f'B/{relative}'), ('zero', f'B/{relative}:0'),
                        ('filename', 'InProcessGateway.ts'),
                        ('suffix', f'B/{relative}:1oops'), ('outside', f'B/{relative}:{line_count + 1}')]:
        mixed = copy.deepcopy(data)
        mixed['methods'][0]['provider']['summary'] += f' 有效引用 {anchor}，另一引用 {bad}。'
        cases.append((f'mixed_prose_{suffix}', mixed,
                      'reference_line' if suffix in ('zero', 'outside') else 'reference_format'))
    no_scope = copy.deepcopy(data)
    del no_scope['methods'][0]['source_audit']
    cases.append(('missing_source_audit', no_scope, 'source_audit'))
    shortened = copy.deepcopy(data)
    scope = next(e for e in shortened['methods'] if e['method'] == 'restore_session_transcript')['source_audit']['functions'][0]
    scope['refs'] = scope['refs'][:1]
    cases.append(('shortened_function_scan', shortened, 'source_audit'))
    missing_provider = copy.deepcopy(data)
    restore = next(e for e in missing_provider['methods'] if e['method'] == 'restore_session_transcript')
    restore['source_audit']['functions'] = [f for f in restore['source_audit']['functions']
                                           if f['symbol'] != 'restoreSessionTranscript' or f['role'] == 'handler']
    cases.append(('omitted_default_provider_scan', missing_provider, 'provider_scope'))
    for source in ('docs/research/008_phase-a-session-domain/gap.md', 'docs/plan/005_phase-a-session-domain.md'):
        drift = copy.deepcopy(data)
        drift['input_files'][source] = '0' * 64
        cases.append(('changed_input_hash_' + Path(source).stem, drift, 'input_drift'))
    narrowed = copy.deepcopy(data)
    for entry in narrowed['methods']:
        entry['source_audit']['functions'] = [f for f in entry['source_audit']['functions'] if f['role'] != 'callee']
        for scope in entry['source_audit']['functions']:
            scope['refs'] = scope['refs'][:1]
            match = REF.fullmatch(scope['refs'][0])
            source = (baseline / match[1]).read_text().splitlines()[int(match[2]) - 1]
            scope['sha256'] = hashlib.sha256(source.encode()).hexdigest()
        entry['branches']['error'] = copy.deepcopy(entry['branches']['unconfigured'])
    cases.append(('shrink_all_ranges_rehash_delete_callees', narrowed, 'source_audit'))
    no_callees = copy.deepcopy(data)
    for entry in no_callees['methods']:
        entry['source_audit']['functions'] = [f for f in entry['source_audit']['functions'] if f['role'] != 'callee']
    cases.append(('delete_all_callees', no_callees, 'callee_scope'))
    invented = copy.deepcopy(data)
    invented['methods'][0]['branches']['error'].append({'code': 'TOTALLY_FAKE_CODE', 'summary': 'fabricated negative control', 'refs': [anchor]})
    cases.append(('invented_error_code', invented, 'invented_error_code'))
    for label, mutate in (
        ('remove', lambda value: value.pop('manager_browsers')),
        ('invent', lambda value: value.update({'restore_session_transcript': 'skip provider'})),
        ('empty_reason', lambda value: value.update({'manager_browsers': ''})),
    ):
        exception = copy.deepcopy(data)
        mutate(exception['provider_exceptions'])
        cases.append(('provider_exception_' + label, exception, 'provider_exceptions'))
    no_effect = copy.deepcopy(data)
    next(e for e in no_effect['methods'] if e['method'] == 'mcp_server_reconnect')['state']['effects'] = []
    cases.append(('omit_close_state_effects', no_effect, 'close_state_effects'))
    no_lazy = copy.deepcopy(data)
    next(e for e in no_lazy['methods'] if e['method'] == 'background_tasks')['lifecycle']['effects'] = []
    cases.append(('omit_lazy_runtime_lifecycle', no_lazy, 'lazy_runtime_lifecycle'))
    false_default = copy.deepcopy(data)
    next(e for e in false_default['methods'] if e['method'] == 'user_dialog_release')['source_audit']['default_constructions'].append(
        {'class': 'FileGatewayUserDialogStore', 'ref': anchor})
    cases.append(('optional_store_claimed_default', false_default, 'default_construction'))
    wrong_assembly = copy.deepcopy(data)
    next(f for e in wrong_assembly['methods'] for f in e['source_audit']['functions'] if f.get('assembly') == 'optional_host')['assembly'] = 'automatic_default'
    cases.append(('optional_provider_marked_default', wrong_assembly, 'default_construction'))
    no_origin = copy.deepcopy(data)
    next(b for e in no_origin['methods'] for b in e['branches']['error'] if b.get('dynamic'))['origin'] = {}
    cases.append(('dynamic_code_missing_origin', no_origin, 'dynamic_code_origin'))
    for label, method, code, ref, expression, required in (
        ('fake_dynamic_paren', 'update_settings', 'TOTALLY_FAKE_CODE', None, '(', 'dynamic_code_origin'),
        ('other_method_dynamic_code', 'memory_list', 'PROJECT_DELETED', 'B/src/gateway/server/GatewayWsConnection.ts:130', 'e', 'dynamic_code_origin'),
        ('dynamic_expression_absent', 'update_settings', '<error.code>', None, 'fabricated()', 'dynamic_code_origin'),
        ('dynamic_partial_expression', 'memory_list', '<error.code>', 'B/src/gateway/server/GatewayWsConnection.ts:130', 'e', 'dynamic_code_origin'),
        ('dynamic_constant_full_line', 'update_settings', 'TOTALLY_FAKE_CODE', None, None, 'dynamic_code_shape'),
    ):
        mutated = copy.deepcopy(data)
        entry = next(e for e in mutated['methods'] if e['method'] == method)
        handler = graph['functions'][graph['routes'][method]['handler']]
        ref = ref or f"B/{handler['file']}:{handler['start']}"
        match = REF.fullmatch(ref)
        expression = expression if expression is not None else (baseline / match[1]).read_text().splitlines()[int(match[2]) - 1].strip()
        entry['branches']['error'].append({'code': code, 'summary': 'dynamic negative control', 'refs': [ref],
                                           'dynamic': True, 'origin': {'refs': [ref], 'expression': expression}})
        cases.append((label, mutated, required))
        if code in ('TOTALLY_FAKE_CODE', 'PROJECT_DELETED'):
            cases.append((label + '_shape', mutated, 'dynamic_code_shape'))
    for role in ('handler', 'provider', 'callee'):
        mutated = copy.deepcopy(data)
        scope = next(s for e in mutated['methods'] for s in e['source_audit']['functions']
                     if s['role'] == role and s['assembly'] == 'automatic_default')
        scope['optional'] = True
        cases.append(('default_' + role + '_marked_optional', mutated, 'optional_default_scope'))
    for code in ('object', 'string', 'function', 'AgentLoop'):
        mutated = copy.deepcopy(data)
        entry = next(e for e in mutated['methods'] if e['method'] == 'update_settings')
        entry['branches']['error'].append({'code': code, 'summary': 'type/name is not an error code', 'refs': [anchor]})
        cases.append(('non_error_literal_' + code, mutated, 'invented_error_code'))
    mutated = copy.deepcopy(data)
    next(s for e in mutated['methods'] for s in e['source_audit']['functions'] if s['assembly'] == 'optional_host').pop('optional')
    cases.append(('optional_injection_missing_marker', mutated, 'default_construction'))
    if strict_equal(compact_data.get('format_version'), 2):
        unknown = copy.deepcopy(compact_data)
        target = next(e for e in unknown['methods'] if e['branches']['error']['groups'])
        target['branches']['error']['groups'][0] = 'does_not_exist'
        cases.append(('unknown_error_group', unknown, 'error_group_reference'))
        fake = copy.deepcopy(compact_data)
        next(iter(fake['error_code_groups'].values()))['errors'][0]['code'] = 'TOTALLY_FAKE_CODE'
        cases.append(('invented_code_in_group', fake, 'invented_error_code'))
        conflict = copy.deepcopy(compact_data)
        group = next(iter(conflict['error_code_groups'].values()))
        other = next(rid for table in conflict['ranges'].values() for rid in table if rid not in group['source_ranges'])
        group['source_ranges'].append(other)
        cases.append(('group_declared_by_two_sources', conflict, 'error_group_source'))
        unknown_range = copy.deepcopy(compact_data)
        unknown_range['methods'][0]['source_audit']['functions'][0]['ranges'][0] = 'missing_range'
        cases.append(('unknown_range', unknown_range, 'range_reference'))
        wrong_hash = copy.deepcopy(compact_data)
        next(iter(next(iter(wrong_hash['ranges'].values())).values()))['sha256'] = '0' * 64
        cases.append(('changed_shared_range_hash', wrong_hash, 'source_audit'))
    binding_methods = [e['method'] for e in data['methods']
                       if any(b['code'] == 'PROJECT_BINDING_STORE_INVALID' for b in e['branches']['error'])]
    for method in binding_methods:
        omitted = copy.deepcopy(data)
        entry = next(e for e in omitted['methods'] if e['method'] == method)
        entry['branches']['error'] = [b for b in entry['branches']['error'] if b['code'] != 'PROJECT_BINDING_STORE_INVALID']
        cases.append(('omit_binding_factory_code_' + method, omitted, 'error_code_coverage'))
    for code in ('RUN_POLICY_INVALID', 'agent_model_error', 'INVALID_MESSAGE', 'path_not_allowed', 'frontmatter_missing'):
        omitted = copy.deepcopy(data)
        entry = next(e for e in omitted['methods'] if any(b['code'] == code for b in e['branches']['error']))
        entry['branches']['error'] = [b for b in entry['branches']['error'] if b['code'] != code]
        cases.append(('omit_discovered_factory_code_' + code, omitted, 'error_code_coverage'))
    for method, codes in (
        ('submit_turn', ('tool_failure_limit_reached', 'model_failure_limit_reached', 'agent_max_budget_reached')),
        ('submit_turn', ('INVALID_SDK_MODEL', 'INVALID_SDK_FALLBACK_MODEL', 'INVALID_SDK_SUBAGENT_MODEL')),
        ('seed_read_state', ('INVALID_SDK_MODEL', 'INVALID_SDK_FALLBACK_MODEL', 'INVALID_SDK_SUBAGENT_MODEL')),
    ):
        for code in codes:
            omitted = copy.deepcopy(data)
            entry = next(e for e in omitted['methods'] if e['method'] == method)
            assert any(b['code'] == code for b in entry['branches']['error'])
            entry['branches']['error'] = [b for b in entry['branches']['error'] if b['code'] != code]
            cases.append(('omit_symbol_code_' + method + '_' + code, omitted, 'error_code_coverage'))
    for label, file, number in (
        ('parameter', 'src/agent/protocol/errors.ts', 38),
        ('partial_conditional', 'src/gateway/client/InProcessGateway.ts', 883),
        ('transport', 'src/gateway/server/GatewayWsConnection.ts', 216),
    ):
        omitted = copy.deepcopy(data)
        entry = next(e for e in omitted['methods'] if e['method'] == 'submit_turn')
        ref = f'B/{file}:{number}'
        removed = [b for b in entry['branches']['error']
                   if b.get('dynamic') and ref in b.get('origin', {}).get('refs', [])]
        assert removed, ref
        entry['branches']['error'] = [b for b in entry['branches']['error'] if b not in removed]
        cases.append(('omit_dynamic_production_' + label, omitted, 'dynamic_code_coverage'))
    for label, code, ref in (
        ('invented_ERR_with_real_code_node', 'ERR_TOTALLY_FAKE_CODE', 'B/src/gateway/server/GatewayWsConnection.ts:233'),
        ('whole_line_without_production', '<error.code>', 'B/src/gateway/server/GatewayWsConnection.ts:130'),
    ):
        mutated = copy.deepcopy(data)
        relative, number = REF.fullmatch(ref).groups()
        expression = (baseline / relative).read_text().splitlines()[int(number) - 1].strip()
        next(e for e in mutated['methods'] if e['method'] == 'memory_list')['branches']['error'].append(
            {'code': code, 'summary': 'round 6 dynamic negative control', 'refs': [ref], 'dynamic': True,
             'origin': {'refs': [ref], 'expression': expression}})
        cases.append((label, mutated, 'dynamic_code_shape' if label.startswith('invented') else 'dynamic_code_origin'))
    for label, code, dynamic, expression, expected in (
        ('unconfigured_invented_code', 'TOTALLY_FAKE_CODE', False, None, 'invented_error_code'),
        ('unconfigured_english_string', 'ordinary English message', False, None, 'invented_error_code'),
        ('unconfigured_dynamic_fake_ERR', 'ERR_TOTALLY_FAKE_CODE', True, 'error.code', 'dynamic_code_shape'),
        ('unconfigured_dynamic_missing_origin', '<error.code>', True, None, 'dynamic_code_origin'),
        ('unconfigured_dynamic_nonproducing_line', '<error.code>', True, 'e', 'dynamic_code_origin'),
        ('unconfigured_NA_missing_origin', 'N/A', False, None, 'non_error_result'),
    ):
        mutated = copy.deepcopy(data)
        ref = 'B/src/gateway/server/GatewayWsConnection.ts:233'
        branch = {'code': code, 'summary': 'round 7 unconfigured control', 'refs': [ref]}
        if dynamic:
            branch['dynamic'] = True
        if expression is not None:
            branch['origin'] = {'refs': [ref], 'expression': expression}
        next(e for e in mutated['methods'] if e['method'] == 'memory_list')['branches']['unconfigured'].append(branch)
        cases.append((label, mutated, expected))
    # Type strictness: plain == accepts bool/int and int/float drift.  Values
    # are derived from the current ledger, so these stay genuine type pairs
    # after a legitimate count change (e.g. a category total moving by one).
    format_version_bool = copy.deepcopy(data)
    format_version_bool['format_version'] = True
    cases.append(('strict_format_version_bool', format_version_bool, 'format_version'))
    scope_total_float = copy.deepcopy(data)
    scope_total_float['scope']['total'] = float(scope_total_float['scope']['total'])
    cases.append(('strict_scope_total_float', scope_total_float, 'scope'))
    scope_remaining_float = copy.deepcopy(data)
    scope_remaining_float['scope']['remaining_after_a4'] = float(
        scope_remaining_float['scope']['remaining_after_a4'])
    cases.append(('strict_scope_remaining_float', scope_remaining_float, 'scope'))
    scope_category_float = copy.deepcopy(data)
    scope_category_float['scope']['category_counts']['(b)'] = float(
        scope_category_float['scope']['category_counts']['(b)'])
    cases.append(('strict_scope_category_float', scope_category_float, 'scope'))
    line_count_float = copy.deepcopy(data)
    line_count_file = next(iter(line_count_float['source_files']))
    line_count_float['source_files'][line_count_file]['line_count'] = float(
        line_count_float['source_files'][line_count_file]['line_count'])
    cases.append(('strict_line_count_float', line_count_float, 'source_drift'))
    # Nested source-derived catalogs: ``line``/``code_argument`` are ints and
    # ``code`` is a bool inside dict/list data, so a retype must not satisfy
    # the pin (the manifest already uses strict_equal for the same ledger).
    error_class_line_float = copy.deepcopy(data)
    error_class_name = next(name for name, value in error_class_line_float['error_classes'].items()
                            if type(value.get('line')) is int)
    error_class_line_float['error_classes'][error_class_name]['line'] = float(
        error_class_line_float['error_classes'][error_class_name]['line'])
    cases.append(('strict_error_class_line_float', error_class_line_float, 'error_class_catalog'))
    error_class_code_bool = copy.deepcopy(data)
    error_class_code_name = next(name for name, value in error_class_code_bool['error_classes'].items()
                                 if type(value.get('code')) is bool)
    error_class_code_bool['error_classes'][error_class_code_name]['code'] = int(
        error_class_code_bool['error_classes'][error_class_code_name]['code'])
    cases.append(('strict_error_class_code_bool', error_class_code_bool, 'error_class_catalog'))
    code_argument_bool = copy.deepcopy(data)
    code_argument_name = next(name for name, value in code_argument_bool['error_classes'].items()
                              if type(value.get('code_argument')) is int and value['code_argument'] == 0)
    code_argument_bool['error_classes'][code_argument_name]['code_argument'] = False
    cases.append(('strict_code_argument_bool', code_argument_bool, 'error_class_catalog'))
    default_boot_line_float = copy.deepcopy(data)
    default_boot_line_float['default_boot']['line'] = float(
        default_boot_line_float['default_boot']['line'])
    cases.append(('strict_default_boot_line_float', default_boot_line_float, 'default_construction'))
    depth_float = copy.deepcopy(data)
    depth_float['methods'][0]['source_audit']['depth'] = float(
        depth_float['methods'][0]['source_audit']['depth'])
    cases.append(('strict_depth_float', depth_float, 'callee_scope'))
    # The raw disk value is what ``expand`` reads; 2.0 used to be accepted as
    # v2 and rewritten to the int 1, so the expanded-field control above could
    # never observe it.  Feed the retype back through expand().
    format_version_float = copy.deepcopy(data)
    format_version_float['format_version'] = 2.0
    cases.append(('strict_format_version_float', format_version_float, 'format_version'))
    results = []
    for name, mutated, expected_code in cases:
        errors, _ = validate(mutated, baseline, inputs)
        detected = any(e['code'] == expected_code for e in errors)
        results.append({'name': name, 'expected': 'FAIL', 'observed': 'FAIL' if errors else 'PASS',
                        'required_error': expected_code, 'result': 'PASS' if detected else 'FAIL'})
    if strict_equal(compact_data.get('format_version'), 2):
        # Same strict JSON loader used by --ledger: one name cannot have two
        # source declarations silently resolved by last-key-wins parsing.
        name, group = next(iter(compact_data['error_code_groups'].items()))
        other = copy.deepcopy(group)
        other['source_ranges'] = ['different_source']
        encoded_name = json.dumps(name)
        raw = '{' + encoded_name + ':' + json.dumps(group) + ',' + encoded_name + ':' + json.dumps(other) + '}'
        detected = False
        try:
            json.loads(raw, object_pairs_hook=unique_object)
        except ValueError as exc:
            detected = str(exc).startswith('duplicate_json_key:')
        results.append({'name': 'duplicate_group_json_declaration', 'expected': 'FAIL',
                        'observed': 'FAIL' if detected else 'PASS', 'required_error': 'duplicate_json_key',
                        'result': 'PASS' if detected else 'FAIL'})
    graph = source_graph(baseline)
    # An existing off-root new expression must not satisfy a provider class's
    # default construction obligation, even if the ledger repeats that point.
    altered = copy.deepcopy(graph)
    altered_data = copy.deepcopy(data)
    route = altered['routes']['memory_list']
    provider = next(altered['functions'][i] for i in route['providers'] if altered['functions'][i]['class'])
    route['construction_points'] = [p for p in route['construction_points'] if p['class'] != provider['class']]
    next(e for e in altered_data['methods'] if e['method'] == 'memory_list')['source_audit']['default_constructions'] = route['construction_points']
    errors, _ = validate(altered_data, baseline, inputs, altered)
    detected = any(e['code'] == 'default_construction' for e in errors)
    results.append({'name': 'provider_constructor_outside_default_graph', 'expected': 'FAIL',
                    'observed': 'FAIL' if errors else 'PASS', 'required_error': 'default_construction',
                    'result': 'PASS' if detected else 'FAIL'})
    # This literal was extracted from a renamed, newly declared code-bearing
    # Error class by the SAME production constructor rule (ast_rules.cjs).
    renamed_code = next(c['observed_code'] for c in graph['parser_controls'] if c['name'] == 'renamed_code_error_class')
    for label, change, expected_code in (
        ('replace_with_previously_uncovered_code_class', lambda f: f['errors'].append({'code': renamed_code, 'line': f['start'], 'class': 'R3PreviouslyUncoveredError'}), 'error_code_coverage'),
        ('replace_with_undefined_error_class', lambda f: f['unknown_errors'].append({'class': 'R3UndefinedError', 'line': f['start']}), 'unknown_error_class'),
    ):
        altered = copy.deepcopy(graph)
        handler = altered['functions'][altered['routes'][data['methods'][0]['method']]['handler']]
        change(handler)
        errors, _ = validate(data, baseline, inputs, altered)
        detected = any(e['code'] == expected_code for e in errors)
        results.append({'name': label, 'expected': 'FAIL', 'observed': 'FAIL' if errors else 'PASS',
                        'required_error': expected_code, 'result': 'PASS' if detected else 'FAIL'})
    for label, file, value, expected in (
        ('invalid_factory_message_is_fatal', None, 'ordinary message', 'invalid_produced_code'),
        ('telemetry_exemption_cannot_hide_other_value', 'src/cli/ProjectMemoryMaintenanceController.ts', 'OtherError', 'invalid_produced_code'),
    ):
        altered = copy.deepcopy(graph)
        target = (next(f for f in altered['functions'] if f['file'] == file and f['start'] <= 94 <= f['end'])
                  if file else altered['functions'][altered['routes']['memory_list']['handler']])
        target['invalid_code_literals'].append({'code': value, 'line': 94 if file else target['start'], 'class': 'factory'})
        errors, _ = validate(data, baseline, inputs, altered)
        detected = any(e['code'] == expected for e in errors)
        results.append({'name': label, 'expected': 'FAIL', 'observed': 'FAIL' if errors else 'PASS',
                        'required_error': expected, 'result': 'PASS' if detected else 'FAIL'})
    for label, code, expression in (
        ('positive_dynamic_exact_production_node', '<error.code>', 'error.code'),
        ('positive_dynamic_whole_line_containing_node', '<error.code>',
         (baseline / 'src/gateway/server/GatewayWsConnection.ts').read_text().splitlines()[232].strip()),
        ('positive_dynamic_source_ERR_literal', 'ERR_SOURCE_FIXTURE', 'error.code'),
    ):
        altered = copy.deepcopy(graph)
        mutated = copy.deepcopy(data)
        ref = 'B/src/gateway/server/GatewayWsConnection.ts:233'
        next(e for e in mutated['methods'] if e['method'] == 'memory_list')['branches']['error'].append(
            {'code': code, 'summary': 'round 6 positive control', 'refs': [ref], 'dynamic': True,
             'origin': {'refs': [ref], 'expression': expression}})
        if code.startswith('ERR_'):
            altered['functions'][altered['routes']['memory_list']['handler']]['literals'].append(
                {'value': code, 'line': altered['functions'][altered['routes']['memory_list']['handler']]['start']})
        errors, _ = validate(mutated, baseline, inputs, altered)
        results.append({'name': label, 'expected': 'PASS', 'observed': 'FAIL' if errors else 'PASS',
                        'result': 'FAIL' if errors else 'PASS'})
    for expression in ('error.code', (baseline / 'src/gateway/server/GatewayWsConnection.ts').read_text().splitlines()[232].strip()):
        mutated = copy.deepcopy(data)
        ref = 'B/src/gateway/server/GatewayWsConnection.ts:233'
        next(e for e in mutated['methods'] if e['method'] == 'memory_list')['branches']['unconfigured'].append(
            {'code': '<error.code>', 'summary': 'round 7 unconfigured positive control', 'refs': [ref],
             'dynamic': True, 'origin': {'refs': [ref], 'expression': expression}})
        errors, _ = validate(mutated, baseline, inputs)
        results.append({'name': 'positive_unconfigured_' + ('exact_node' if expression == 'error.code' else 'whole_line'),
                        'expected': 'PASS', 'observed': 'FAIL' if errors else 'PASS', 'result': 'FAIL' if errors else 'PASS'})
    # Exercise the production whole-line predicate: these otherwise have an
    # exact line match, single-line span and contained producing expression.
    for label, expression in (('slash', '// error.code'), ('block', '/* error.code */'), ('star', '* error.code')):
        accepted = producing_line_origin(expression, '  ' + expression, {'start': 1, 'end': 1, 'expression': 'error.code'}, 1)
        results.append({'name': 'dynamic_whole_line_comment_' + label, 'expected': 'FAIL',
                        'observed': 'PASS' if accepted else 'FAIL', 'result': 'FAIL' if accepted else 'PASS'})
    return results


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--ledger', type=Path, default=LEDGER)
    parser.add_argument('--baseline', type=Path, default=default_baseline())
    parser.add_argument('--compare-expanded', type=Path,
                        help='one-time exact comparison with round-3 expanded ledger')
    args = parser.parse_args()
    try:
        data = read_json(args.ledger)
        inputs = load_inputs()
        baseline = args.baseline.resolve()
        failures, summary = validate(data, baseline, inputs)
        comparison = None
        if args.compare_expanded:
            expanded, expansion_errors = expand(data)
            previous = read_json(args.compare_expanded)
            changes = []
            def compare(old, new, path='$'):
                if type(old) is not type(new):
                    changes.append(path)
                elif isinstance(old, dict):
                    for key in sorted(old.keys() | new.keys()):
                        if key not in old or key not in new:
                            changes.append(path + '.' + key)
                        else:
                            compare(old[key], new[key], path + '.' + key)
                elif isinstance(old, list):
                    if len(old) != len(new):
                        changes.append(path + '.length')
                    for i, (left, right) in enumerate(zip(old, new)):
                        compare(left, right, f'{path}[{i}]')
                elif old != new:
                    changes.append(path)
            compare(previous, expanded)
            comparison = {'result': 'PASS' if not changes and not expansion_errors else 'FAIL',
                          'reference_sha256': digest(args.compare_expanded),
                          'method_count': len(expanded['methods']),
                          'error_records': sum(len(e['branches']['error']) for e in expanded['methods']),
                          'function_associations': sum(len(e['source_audit']['functions']) for e in expanded['methods']),
                          'changed_paths': changes}
            if comparison['result'] != 'PASS':
                failures.append({'code': 'expanded_equivalence', 'path': '$', 'detail': comparison})
        controls = self_test(data, baseline, inputs) if not failures else []
        failed = bool(failures) or any(c['result'] != 'PASS' for c in controls)
        report = {'status': 'FAIL' if failed else 'PASS', 'scope': 'static_coverage_ledger',
                  **summary, 'failures': failures, 'negative_self_tests': controls,
                  'parser_self_tests': source_graph(baseline)['parser_controls'],
                  'expanded_comparison': comparison}
    except (OSError, UnicodeError, ValueError, KeyError, TypeError, subprocess.TimeoutExpired) as exc:
        report = {'status': 'FAIL', 'failures': [{'code': 'input', 'detail': str(exc)}], 'negative_self_tests': []}
        failed = True
    print(json.dumps(report, ensure_ascii=False, indent=2))
    return int(failed)


if __name__ == '__main__':
    sys.exit(main())
