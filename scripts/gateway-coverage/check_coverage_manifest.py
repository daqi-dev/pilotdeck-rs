#!/usr/bin/env python3
"""A5.1 coverage manifest validator (issue #616).

Checks specs/gateway-coverage/coverage-manifest.json:

1. method set is one-to-one with the frozen 101-method list
   (schema definitions.WsGatewayMethod.enum, gateway-methods.snapshot.json and
   docs/research/005_phase-a-gateway/method-inventory.md), with no duplicates;
2. every method has an owner derived from plan 005 / gap.md work packages and
   at least one scenario id (existing driver group and/or proposed a51.* id);
3. every handler -> provider -> state/memory -> notifications -> success/error/
   unconfigured stage carries a live B/path:line anchor, and every BLOCKED row
   has a nonempty missing/obtain reason;
4. the row is an exact projection of the merged 101-method dependency ledger
   (specs/gateway-coverage/ledger.json, PR #79) for owner/status/branches/
   configuration/lifecycle/blockers, so the manifest cannot drift from the
   evidence ledger;
5. every referenced baseline file matches the sha256 pinned in the ledger's
   source_files table.

Runs under ``python3 -I`` (stdlib only; ledger_format is imported from this
directory explicitly).  A missing read-only baseline is fail-closed (exit 2),
never treated as PASS.
"""
import argparse
import copy
import hashlib
import json
import os
import re
import sys
from collections import Counter
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
if str(SCRIPT_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPT_DIR))
from ledger_format import expand, unique_object  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
MANIFEST = ROOT / 'specs/gateway-coverage/coverage-manifest.json'
LEDGER = ROOT / 'specs/gateway-coverage/ledger.json'
SCHEMA = ROOT / 'schema/gateway-protocol.schema.json'
SNAPSHOT = ROOT / 'schema/gateway-methods.snapshot.json'
INVENTORY = ROOT / 'docs/research/005_phase-a-gateway/method-inventory.md'
GAP = ROOT / 'docs/research/008_phase-a-session-domain/gap.md'
DRIVER = ROOT / 'scripts/gateway-parity/driver.mts'

REF = re.compile(r'B/([A-Za-z0-9_./-]+):([0-9]+)')
CATEGORIES = {'(a)': 10, '(b)': 46, '(c)': 4, '待定': 41}
CATEGORY_ORDER = ('(a)', '(b)', '(c)', '待定')
PINNED_COMMIT = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85'
A6_PACKAGES = {f'A6.{i}' for i in range(1, 10)}
STATE_KINDS = {'storage', 'memory', 'external', 'none'}
STATUSES = {'READY', 'BLOCKED'}
STAGE_KEYS = {
    'handler': ({'refs', 'summary'}, {'refs', 'summary'}),
    'provider': ({'refs', 'summary'}, {'refs', 'default_refs', 'summary'}),
    'state': ({'refs', 'summary', 'kinds'}, {'refs', 'summary', 'kinds', 'optional_kinds', 'effects'}),
    'notifications': ({'refs', 'summary', 'scope', 'names'},
                      {'refs', 'summary', 'scope', 'names', 'event_types', 'unresolved'}),
    'lifecycle': ({'refs', 'summary'}, {'refs', 'summary', 'effects'}),
    'guard': ({'refs', 'summary'}, {'refs', 'summary'}),
}
BRANCH_SUCCESS_KEYS = {'refs', 'summary'}
BRANCH_UNCONFIGURED_KEYS = {'refs', 'summary', 'code', 'dynamic', 'origin'}
ERROR_KEYS = {'count', 'codes', 'dynamic_codes', 'sample', 'refs', 'evidence'}
BINDING_KEYS = {'configuration', 'lifecycle', 'missing', 'obtain', 'provider', 'status'}


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def read_json(path):
    return json.loads(Path(path).read_text(), object_pairs_hook=unique_object)


def string_list(value, nonempty=False, unique=True):
    return (isinstance(value, list) and (bool(value) or not nonempty)
            and all(isinstance(item, str) and item.strip() for item in value)
            and (not unique or len(value) == len(set(value))))


def expected_owner(package):
    if package == 'A4':
        return 'A4'
    if package in {'C', 'U9', 'U10', *(f'S{i}' for i in range(1, 9))}:
        return 'A7'
    if package in {f'U{i}' for i in range(1, 14)}:
        return 'A8'
    return None


def default_baseline():
    env = os.environ.get('PDRS_BASELINE')
    if env:
        return Path(env)
    for parent in (ROOT, *ROOT.parents):
        candidate = parent / 'resources/PilotDeck'
        if candidate.is_dir():
            return candidate
    return ROOT / 'resources/PilotDeck'


class RefChecker:
    """Resolve B/ anchors and pin whole files to the ledger source table."""

    def __init__(self, baseline, source_files):
        self.baseline = baseline
        self.pins = source_files if isinstance(source_files, dict) else {}
        self.digests = {}
        self.checked = 0
        self.files = set()

    def check(self, ref, where, fail):
        if not isinstance(ref, str) or not REF.fullmatch(ref):
            fail('reference_format', where, 'expected B/relative/path:positive-line')
            return
        relative, number = REF.fullmatch(ref).groups()
        self.checked += 1
        self.files.add(relative)
        if relative.startswith('/') or '..' in Path(relative).parts:
            fail('reference_path', where, 'reference escapes baseline: ' + ref)
            return
        pin = self.pins.get(relative)
        if not isinstance(pin, dict) or not isinstance(pin.get('sha256'), str):
            fail('baseline_pin', where, 'file is not pinned by ledger.source_files: ' + relative)
            return
        path = self.baseline / relative
        try:
            if relative not in self.digests:
                raw = path.read_bytes()
                self.digests[relative] = (hashlib.sha256(raw).hexdigest(),
                                          len(raw.decode('utf-8').splitlines()))
            digest, line_count = self.digests[relative]
        except (OSError, UnicodeError) as exc:
            fail('reference_file', where, str(exc))
            return
        if digest != pin['sha256'] or line_count != pin.get('line_count'):
            fail('baseline_pin', where, 'baseline file differs from frozen snapshot: ' + relative)
            return
        if not 1 <= int(number) <= line_count:
            fail('reference_line', where, f'{ref}: line outside 1..{line_count}')


def load_inputs():
    schema = read_json(SCHEMA)
    snapshot = read_json(SNAPSHOT)
    rows = re.findall(r'^\| (\d+) \| `([^`]+)` \| ([^|]+) \|', INVENTORY.read_text(), re.M)
    categories, numbers, inventory_methods = {}, [], []
    for number, method, cell in rows:
        numbers.append(int(number))
        inventory_methods.append(method)
        categories[method] = next((category for category in CATEGORY_ORDER if category in cell), '待定')
    groups = {}
    for line in GAP.read_text().splitlines():
        match = re.match(r'\| (C|S[1-8]|U\d+) ', line)
        if not match:
            continue
        for method in re.findall(r'`([a-z_]+)`', line.split('|')[2]):
            if method in groups:
                raise ValueError(f'duplicate work-package assignment: {method}')
            groups[method] = match.group(1)
    groups.update({method: 'A4' for method, category in categories.items() if category == '(a)'})
    scenarios = set(re.findall(r"args\.scenario === '([^']+)'", DRIVER.read_text()))
    return {
        'schema_methods': schema['definitions']['WsGatewayMethod']['enum'],
        'snapshot_methods': snapshot['methods'],
        'inventory_methods': inventory_methods,
        'inventory_numbers': numbers,
        'categories': categories,
        'groups': groups,
        'driver_scenarios': scenarios,
        'schema_sha': sha256(SCHEMA),
        'snapshot_sha': sha256(SNAPSHOT),
        'inventory_sha': sha256(INVENTORY),
    }


def validate(data, ledger_raw, ledger, inputs, checker):
    """Return (failures, summary) for one manifest object."""
    errors = []

    def fail(code, where, detail):
        errors.append({'code': code, 'path': where, 'detail': detail})

    if not isinstance(data, dict):
        return [{'code': 'document', 'path': '$', 'detail': 'expected object'}], {}

    if data.get('format_version') != 1:
        fail('format_version', '$.format_version', 'expected format_version=1')
    if not isinstance(data.get('comment'), str) or not data['comment'].strip():
        fail('comment', '$.comment', 'nonempty comment required')

    # ---- frozen method list ------------------------------------------------
    schema, snapshot = inputs['schema_methods'], inputs['snapshot_methods']
    inventory, numbers = inputs['inventory_methods'], inputs['inventory_numbers']
    for name, values in (('schema', schema), ('snapshot', snapshot), ('inventory', inventory)):
        if not string_list(values, nonempty=True) or len(values) != 101:
            fail('input_methods', name, 'expected 101 unique methods')
    if numbers != list(range(1, 102)):
        fail('input_methods', 'inventory', 'row numbers must be 1..101')
    expected = set(schema)
    if expected != set(snapshot) or expected != set(inventory):
        fail('input_methods', '$', 'schema, snapshot and inventory disagree')
    if dict(Counter(inputs['categories'].values())) != CATEGORIES:
        fail('input_categories', '$', 'frozen inventory category totals disagree')
    if set(inputs['groups']) != expected:
        fail('input_groups', '$', {'missing': sorted(expected - set(inputs['groups'])),
                                   'extra': sorted(set(inputs['groups']) - expected)})

    ledger_entries = {}
    if not isinstance(ledger.get('methods'), list):
        fail('ledger', '$.source', 'expanded ledger has no methods list')
        ledger_methods = []
    else:
        ledger_methods = ledger['methods']
        names = [entry.get('method') for entry in ledger_methods if isinstance(entry, dict)]
        duplicates = [name for name, count in Counter(names).items() if count > 1]
        if duplicates:
            fail('ledger_method_set', '$.source', {'duplicate': duplicates})
        if set(names) != expected:
            fail('ledger_method_set', '$.source',
                 {'missing': sorted(expected - set(names)), 'extra': sorted(set(names) - expected)})
        for entry in ledger_methods:
            if isinstance(entry, dict):
                ledger_entries[entry['method']] = entry

    # ---- baseline / frozen inputs -----------------------------------------
    baseline = data.get('baseline', {})
    if not isinstance(baseline, dict):
        fail('baseline', '$.baseline', 'expected object')
        baseline = {}
    if baseline.get('commit') != ledger_raw.get('baseline', {}).get('commit'):
        fail('baseline', '$.baseline', 'commit differs from ledger baseline')
    if baseline.get('commit') != PINNED_COMMIT:
        fail('baseline', '$.baseline', 'expected the schema-FROZEN baseline commit')
    if not isinstance(baseline.get('root'), str) or not baseline['root'].strip():
        fail('baseline', '$.baseline', 'nonempty root_hint required')

    frozen = data.get('frozen_inputs', {})
    if not isinstance(frozen, dict):
        fail('input_hash', '$.frozen_inputs', 'expected object')
        frozen = {}
    if frozen.get('method_count') != 101:
        fail('input_methods', '$.frozen_inputs', 'method_count must be 101')
    expected_hashes = {
        'schema/gateway-protocol.schema.json': inputs['schema_sha'],
        'schema/gateway-methods.snapshot.json': inputs['snapshot_sha'],
        'docs/research/005_phase-a-gateway/method-inventory.md': inputs['inventory_sha'],
    }
    if frozen.get('sha256') != expected_hashes:
        fail('input_hash', '$.frozen_inputs.sha256', 'frozen input hashes differ from checkout')
    if frozen.get('schema_enum') != 'schema/gateway-protocol.schema.json#/definitions/WsGatewayMethod/enum':
        fail('input_methods', '$.frozen_inputs.schema_enum', 'unexpected schema enum path')
    if frozen.get('snapshot') != 'schema/gateway-methods.snapshot.json':
        fail('input_methods', '$.frozen_inputs.snapshot', 'unexpected snapshot path')
    if frozen.get('inventory') != 'docs/research/005_phase-a-gateway/method-inventory.md':
        fail('input_methods', '$.frozen_inputs.inventory', 'unexpected inventory path')

    source = data.get('source', {})
    if not isinstance(source, dict):
        fail('ledger', '$.source', 'expected object')
        source = {}
    if source.get('ledger') != 'specs/gateway-coverage/ledger.json':
        fail('ledger', '$.source.ledger', 'expected specs/gateway-coverage/ledger.json')
    if source.get('ledger_sha256') != sha256(LEDGER):
        fail('ledger_hash', '$.source.ledger_sha256', 'ledger hash differs from checkout')
    if source.get('ledger_format_version') != ledger_raw.get('format_version'):
        fail('ledger', '$.source.ledger_format_version', 'format_version differs from ledger')
    if source.get('ledger_validator') != 'scripts/gateway-coverage/check_ledger.py':
        fail('ledger', '$.source.ledger_validator', 'unexpected ledger validator path')
    if not isinstance(source.get('note'), str) or not source['note'].strip():
        fail('ledger', '$.source.note', 'nonempty provenance note required')
    if not isinstance(data.get('projection'), dict) or not data['projection']:
        fail('projection', '$.projection', 'nonempty projection rules required')
    owner_policy = data.get('owner_policy')
    if (not isinstance(owner_policy, dict) or set(owner_policy) != {'A4', 'A7', 'A8', 'A6'}
            or not all(isinstance(value, str) and value.strip() for value in owner_policy.values())):
        fail('owner', '$.owner_policy', 'owner policy must describe A4/A7/A8/A6')

    semantics = ledger_raw.get('semantics', {})
    expected_semantics = {key: semantics.get(key) for key in ('READY', 'BLOCKED')}
    if data.get('status_semantics') != expected_semantics:
        fail('status_semantics', '$.status_semantics', 'READY/BLOCKED semantics must match ledger')
    scenario_policy = data.get('scenario_policy')
    if (not isinstance(scenario_policy, dict)
            or set(scenario_policy) != {'driver', 'proposed', 'requirement'}
            or not all(isinstance(value, str) and value.strip()
                       for value in scenario_policy.values())):
        fail('scenario', '$.scenario_policy', 'driver/proposed/requirement text required')

    # ---- configuration / external bindings --------------------------------
    configuration = data.get('configuration', {})
    if not isinstance(configuration, dict) or not configuration:
        fail('configuration', '$.configuration', 'nonempty configuration catalog required')
        configuration = {}
    if configuration != ledger_raw.get('configuration'):
        fail('configuration', '$.configuration', 'configuration catalog differs from ledger')
    for key, value in configuration.items():
        if (not isinstance(value, dict) or not isinstance(value.get('summary'), str)
                or not value['summary'].strip() or not string_list(value.get('refs'), True)):
            fail('configuration', f'$.configuration.{key}', 'summary and nonempty refs required')

    bindings = data.get('external_bindings', {})
    ledger_bindings = ledger_raw.get('external_bindings', {})
    if not isinstance(bindings, dict) or not bindings:
        fail('external_bindings', '$.external_bindings', 'nonempty binding table required')
        bindings = {}
    if bindings != ledger_bindings:
        fail('external_bindings', '$.external_bindings', 'binding table differs from ledger')
    if set(bindings) != set(ledger_bindings):
        fail('external_bindings', '$.external_bindings', 'binding keys differ from ledger')
    for name, value in bindings.items():
        where = f'$.external_bindings.{name}'
        if not isinstance(value, dict) or set(value) != BINDING_KEYS:
            fail('external_bindings', where, 'expected configuration/lifecycle/missing/obtain/provider/status')
            continue
        if value.get('status') not in STATUSES:
            fail('external_bindings', where + '.status', 'READY or BLOCKED required')
        if not string_list(value.get('configuration'), True):
            fail('external_bindings', where + '.configuration', 'configuration keys required')
        elif any(key not in configuration for key in value['configuration']):
            fail('configuration', where + '.configuration', 'unknown configuration key')
        for part in ('provider', 'lifecycle'):
            item = value.get(part)
            if (not isinstance(item, dict) or not isinstance(item.get('summary'), str)
                    or not item['summary'].strip() or not string_list(item.get('refs'), True)):
                fail('external_bindings', f'{where}.{part}', 'summary and nonempty refs required')
        for part in ('missing', 'obtain'):
            if not isinstance(value.get(part), str) or not value[part].strip():
                fail('blocked_reason', f'{where}.{part}', 'nonempty reason required')
        if value.get('status') == 'BLOCKED' and not (value.get('missing') and value.get('obtain')):
            fail('blocked_reason', where, 'BLOCKED binding needs missing and obtain')

    # ---- methods ----------------------------------------------------------
    entries = data.get('methods')
    if not isinstance(entries, list):
        fail('methods', '$.methods', 'expected list')
        entries = []
    names = [entry.get('method') for entry in entries if isinstance(entry, dict)]
    duplicates = [name for name, count in Counter(names).items() if count > 1]
    if duplicates:
        fail('duplicate_method', '$.methods', duplicates)
    if len(names) != len(entries) or any(not isinstance(name, str) for name in names):
        fail('entry', '$.methods', 'every entry must be an object with a string method')
    elif set(names) != expected:
        fail('method_set', '$.methods', {'missing': sorted(expected - set(names)),
                                         'extra': sorted(set(names) - set(expected))})

    proposed_seen = set()
    for index, entry in enumerate(entries):
        where = f'$.methods[{index}]'
        if not isinstance(entry, dict):
            fail('entry', where, 'expected object')
            continue
        method = entry.get('method')
        if not isinstance(method, str):
            continue
        for field in ('method', 'frozen_category', 'work_package', 'delivery_package', 'owner',
                      'status', 'scenario_ids', 'handler', 'provider', 'state', 'notifications',
                      'branches', 'configuration', 'lifecycle', 'guard', 'a6_dependencies', 'blockers'):
            if field not in entry:
                fail('required_field', where + '.' + field, 'missing required field')
        if entry.get('frozen_category') != inputs['categories'].get(method):
            fail('frozen_category', where, method)
        if entry.get('work_package') != inputs['groups'].get(method):
            fail('work_package', where, method)
        if entry.get('owner') != expected_owner(inputs['groups'].get(method)):
            fail('owner', where, method)
        if entry.get('status') not in STATUSES:
            fail('status', where, 'READY or BLOCKED required')
        if not isinstance(entry.get('delivery_package'), str) or not entry['delivery_package'].strip():
            fail('required_field', where + '.delivery_package', 'nonempty string required')

        # scenario ids: existing driver group(s) and/or proposed a51 ids.
        scenario = entry.get('scenario_ids')
        if not isinstance(scenario, dict) or set(scenario) != {'driver', 'proposed'}:
            fail('scenario', where + '.scenario_ids', 'driver and proposed lists required')
        else:
            driver = scenario['driver']
            proposed = scenario['proposed']
            if not string_list(driver, True):
                fail('scenario', where + '.scenario_ids.driver', 'nonempty unique driver ids required')
            else:
                if 'NONE' in driver and len(driver) != 1:
                    fail('scenario', where + '.scenario_ids.driver', 'NONE must be the only driver id')
                unknown = set(driver) - inputs['driver_scenarios'] - {'NONE'}
                if unknown:
                    fail('scenario', where + '.scenario_ids.driver', {'unknown': sorted(unknown)})
            if not string_list(proposed, True):
                fail('scenario', where + '.scenario_ids.proposed', 'nonempty unique proposed ids required')
            else:
                pattern = re.compile(r'a51\.' + re.escape(method) + r'\.[a-z0-9_]+$')
                bad = [item for item in proposed if not pattern.fullmatch(item)]
                if bad:
                    fail('scenario', where + '.scenario_ids.proposed', {'bad_ids': bad[:5]})
                repeated = proposed_seen & set(proposed)
                if repeated:
                    fail('scenario_duplicate', where, sorted(repeated)[:5])
                proposed_seen.update(proposed)

        # evidence stages: exact projection of the merged ledger row.
        ledger_entry = ledger_entries.get(method)
        for stage_name, (required, allowed) in STAGE_KEYS.items():
            value = entry.get(stage_name)
            stage_where = f'{where}.{stage_name}'
            if not isinstance(value, dict):
                fail('evidence', stage_where, 'expected object')
                continue
            extra = set(value) - allowed
            missing = required - set(value)
            if extra:
                fail('evidence', stage_where, {'unexpected_keys': sorted(extra)})
            if missing:
                fail('evidence', stage_where, {'missing_keys': sorted(missing)})
            if not isinstance(value.get('summary'), str) or not value['summary'].strip():
                fail('evidence', stage_where + '.summary', 'nonempty summary required')
            if not string_list(value.get('refs'), True):
                fail('evidence', stage_where + '.refs', 'nonempty unique refs required')
            if ledger_entry is not None:
                ledger_stage = ledger_entry.get(stage_name, {})
                if not isinstance(ledger_stage, dict):
                    fail('ledger_projection', stage_where, 'ledger stage is not an object')
                else:
                    unprojected = set(ledger_stage) - allowed
                    if unprojected:
                        fail('ledger_projection', stage_where,
                             {'unprojected_keys': sorted(unprojected)})
                    expected_stage = {key: ledger_stage.get(key)
                                      for key in ledger_stage if key in allowed}
                    if value != expected_stage:
                        fail('ledger_projection', stage_where, 'stage differs from expanded ledger')
            if stage_name == 'state':
                kinds = value.get('kinds')
                if not string_list(kinds, True) or not set(kinds) <= STATE_KINDS:
                    fail('state', stage_where + '.kinds', 'storage/memory/external/none required')
                elif 'none' in kinds and len(kinds) != 1:
                    fail('state', stage_where + '.kinds', 'none cannot be combined with other kinds')
                optional = value.get('optional_kinds', [])
                if not string_list(optional) or not set(optional) <= STATE_KINDS - {'none'}:
                    fail('state', stage_where + '.optional_kinds', 'invalid optional kinds')
                if not isinstance(value.get('effects', []), list) or not all(
                        isinstance(item, str) and item.strip() for item in value.get('effects', [])):
                    fail('state', stage_where + '.effects', 'effect names must be strings')
            elif stage_name == 'notifications':
                if not string_list(value.get('names')):
                    fail('notifications', stage_where + '.names', 'names must be a unique string list')
                if 'event_types' in value and not string_list(value['event_types']):
                    fail('notifications', stage_where + '.event_types', 'event types must be unique strings')
                if not isinstance(value.get('scope'), str) or not value['scope'].strip():
                    fail('notifications', stage_where + '.scope', 'nonempty scope required')
                if 'unresolved' in value and (not isinstance(value['unresolved'], str)
                                              or not value['unresolved'].strip()):
                    fail('notifications', stage_where + '.unresolved', 'nonempty text required')

        # branches
        branches = entry.get('branches')
        if not isinstance(branches, dict) or set(branches) != {'success', 'error', 'unconfigured'}:
            fail('branches', where + '.branches', 'success/error/unconfigured required')
            branches = {}
        success = branches.get('success')
        if not isinstance(success, list) or not success:
            fail('branches', where + '.branches.success', 'at least one success record required')
        else:
            for j, record in enumerate(success):
                record_where = f'{where}.branches.success[{j}]'
                if (not isinstance(record, dict) or set(record) != BRANCH_SUCCESS_KEYS
                        or not isinstance(record.get('summary'), str) or not record['summary'].strip()
                        or not string_list(record.get('refs'), True)):
                    fail('branches', record_where, 'summary and nonempty refs required')
            if ledger_entry is not None and success != ledger_entry['branches']['success']:
                fail('ledger_projection', where + '.branches.success', 'differs from expanded ledger')

        unconfigured = branches.get('unconfigured')
        if not isinstance(unconfigured, list) or not unconfigured:
            fail('branches', where + '.branches.unconfigured', 'at least one unconfigured record required')
        else:
            for j, record in enumerate(unconfigured):
                record_where = f'{where}.branches.unconfigured[{j}]'
                if (not isinstance(record, dict) or not set(record) <= BRANCH_UNCONFIGURED_KEYS
                        or not isinstance(record.get('code'), str) or not record['code'].strip()
                        or not isinstance(record.get('summary'), str) or not record['summary'].strip()
                        or not string_list(record.get('refs'), True)):
                    fail('branches', record_where, 'code, summary and nonempty refs required')
            if ledger_entry is not None and unconfigured != ledger_entry['branches']['unconfigured']:
                fail('ledger_projection', where + '.branches.unconfigured', 'differs from expanded ledger')

        error = branches.get('error')
        if not isinstance(error, dict) or set(error) != ERROR_KEYS:
            fail('error_branch', where + '.branches.error',
                 'count/codes/dynamic_codes/sample/refs/evidence required')
        else:
            count = error.get('count')
            codes = error.get('codes')
            dynamic = error.get('dynamic_codes')
            sample = error.get('sample')
            if type(count) is not int or count < 1:
                fail('error_branch', where + '.branches.error.count', 'positive integer required')
            if not string_list(codes, True) or codes != sorted(codes):
                fail('error_codes', where + '.branches.error.codes', 'sorted unique nonempty codes required')
            if not string_list(dynamic) or dynamic != sorted(dynamic) or not set(dynamic) <= set(codes or []):
                fail('error_dynamic', where + '.branches.error.dynamic_codes', 'sorted subset of codes required')
            if not isinstance(sample, list) or len(sample) != min(3, len(codes or [])):
                fail('error_sample', where + '.branches.error.sample', 'first three codes one record each required')
            elif [record.get('code') for record in sample if isinstance(record, dict)] != list(codes[:3]):
                fail('error_sample', where + '.branches.error.sample', 'sample must follow sorted codes')
            handler_value = entry.get('handler')
            handler_refs = handler_value.get('refs') if isinstance(handler_value, dict) else None
            if error.get('refs') != handler_refs:
                fail('error_branch', where + '.branches.error.refs', 'handler dispatch refs required')
            expected_evidence = 'specs/gateway-coverage/ledger.json#methods[%s].branches.error' % method
            if error.get('evidence') != expected_evidence:
                fail('error_branch', where + '.branches.error.evidence', 'ledger pointer required')
            if ledger_entry is not None:
                records = ledger_entry['branches']['error']
                if count != len(records):
                    fail('error_branch', where + '.branches.error.count', 'count differs from ledger')
                expected_codes = sorted({record['code'] for record in records})
                if codes != expected_codes:
                    fail('error_codes', where + '.branches.error.codes', 'codes differ from ledger')
                expected_dynamic = sorted({record['code'] for record in records if record.get('dynamic')})
                if dynamic != expected_dynamic:
                    fail('error_dynamic', where + '.branches.error.dynamic_codes', 'dynamic codes differ from ledger')
                if isinstance(sample, list) and any(record not in records for record in sample):
                    fail('error_sample', where + '.branches.error.sample', 'sample not found in ledger')

        configuration_keys = entry.get('configuration')
        if not string_list(configuration_keys):
            fail('configuration', where + '.configuration', 'configuration key list required')
        elif any(key not in configuration for key in configuration_keys):
            fail('configuration', where + '.configuration', 'unknown configuration key')

        a6 = entry.get('a6_dependencies')
        if not string_list(a6):
            fail('a6_dependencies', where + '.a6_dependencies', 'unique A6 package list required')
        elif not set(a6) <= A6_PACKAGES:
            fail('a6_dependencies', where + '.a6_dependencies', 'unknown A6 package')

        blockers = entry.get('blockers')
        if not isinstance(blockers, list):
            fail('blocked_reason', where + '.blockers', 'list required')
        else:
            for j, blocker in enumerate(blockers):
                blocker_where = f'{where}.blockers[{j}]'
                if (not isinstance(blocker, dict) or set(blocker) != {'missing', 'obtain'}
                        or not isinstance(blocker.get('missing'), str) or not blocker['missing'].strip()
                        or not isinstance(blocker.get('obtain'), str) or not blocker['obtain'].strip()):
                    fail('blocked_reason', blocker_where, 'nonempty missing and obtain required')
            if entry.get('status') == 'BLOCKED' and not blockers:
                fail('blocked_reason', where, 'BLOCKED row needs a reason')
            if entry.get('status') == 'READY' and blockers:
                fail('blocked_reason', where, 'READY row must not carry blockers')

        if 'scenario_note' in entry and (not isinstance(entry['scenario_note'], str)
                                         or not entry['scenario_note'].strip()):
            fail('scenario', where + '.scenario_note', 'nonempty text required')

        # exact projection of ledger scalar fields
        if ledger_entry is not None:
            for field in ('frozen_category', 'work_package', 'owner', 'delivery_package', 'status',
                          'configuration', 'a6_dependencies', 'blockers'):
                if entry.get(field) != ledger_entry.get(field):
                    fail('ledger_projection', f'{where}.{field}', 'differs from expanded ledger')
            if scenario != {'driver': ledger_entry['scenario_ids'],
                            'proposed': ledger_entry['proposed_scenarios']}:
                fail('ledger_projection', where + '.scenario_ids', 'scenario ids differ from ledger')
            if entry.get('scenario_note') != ledger_entry.get('scenario_note'):
                if entry.get('scenario_note') or ledger_entry.get('scenario_note'):
                    fail('ledger_projection', where + '.scenario_note', 'differs from expanded ledger')

    # ---- domains: memory / snapshot / browser -----------------------------
    domains = data.get('domains')
    if not isinstance(domains, dict) or set(domains) != {'memory', 'snapshot', 'browser'}:
        fail('domains', '$.domains', 'memory/snapshot/browser required')
        domains = {}
    expected_domain_methods = {
        'memory': sorted(method for method, package in inputs['groups'].items() if package == 'U2'),
        'snapshot': sorted(method for method, package in inputs['groups'].items() if package == 'U3'),
        'browser': ['manager_browsers'],
    }
    for name, expected_methods in expected_domain_methods.items():
        value = domains.get(name)
        where = f'$.domains.{name}'
        if not isinstance(value, dict):
            fail('domains', where, 'expected object')
            continue
        if not string_list(value.get('methods'), True) or sorted(value['methods']) != expected_methods:
            fail('domains', where + '.methods', {'expected': expected_methods})
        if not isinstance(value.get('note'), str) or not value['note'].strip():
            fail('domains', where + '.note', 'nonempty note required')
        if not string_list(value.get('provider_refs'), True):
            fail('domains', where + '.provider_refs', 'nonempty refs required')
        if not string_list(value.get('lifecycle_refs'), True):
            fail('domains', where + '.lifecycle_refs', 'nonempty refs required')
        if name in ('memory', 'browser'):
            binding_name = 'memory_factory' if name == 'memory' else 'browser_manager'
            binding = ledger_bindings.get(binding_name, {})
            if value.get('binding') != binding_name:
                fail('domains', where + '.binding', 'expected ' + binding_name)
            for field in ('configuration', 'provider_refs', 'lifecycle_refs', 'status', 'missing', 'obtain'):
                ledger_value = binding.get('provider', {}).get('refs') if field == 'provider_refs' else (
                    binding.get('lifecycle', {}).get('refs') if field == 'lifecycle_refs' else binding.get(field))
                if value.get(field) != ledger_value:
                    fail('domains', f'{where}.{field}', 'differs from external binding')
        else:
            if value.get('binding') is not None:
                fail('domains', where + '.binding', 'snapshot has no host binding (env-only)')
            if value.get('configuration') != ['snapshot']:
                fail('domains', where + '.configuration', 'snapshot configuration key required')
            snapshot_entries = [entry for entry in entries
                                if isinstance(entry, dict) and entry.get('work_package') == 'U3']
            allowed_refs = set(configuration.get('snapshot', {}).get('refs', []))
            for entry in snapshot_entries:
                provider = entry.get('provider')
                if isinstance(provider, dict) and isinstance(provider.get('refs'), list):
                    allowed_refs.update(provider['refs'])
            if not set(value.get('provider_refs', [])) >= set(configuration.get('snapshot', {}).get('refs', [])):
                fail('domains', where + '.provider_refs', 'must include configuration.snapshot refs')
            if not set(value.get('provider_refs', [])) <= allowed_refs:
                fail('domains', where + '.provider_refs', 'refs outside configuration/ledger snapshot providers')
            expected_lifecycle = sorted({
                ref for entry in snapshot_entries
                for ref in (entry.get('lifecycle', {}).get('refs', [])
                            if isinstance(entry.get('lifecycle'), dict) else [])
            })
            if value.get('lifecycle_refs') != expected_lifecycle:
                fail('domains', where + '.lifecycle_refs', 'differs from snapshot method lifecycle refs')
            construction = value.get('construction_refs')
            if not string_list(construction, True) or len(construction) != 2:
                fail('domains', where + '.construction_refs', 'two construction anchors required')
            if value.get('status') != 'READY':
                fail('domains', where + '.status', 'static snapshot domain is READY')
            if not string_list(value.get('unverified'), True):
                fail('domains', where + '.unverified', 'nonempty unverified list required')

    # ---- summary ----------------------------------------------------------
    ready = sum(1 for entry in entries if isinstance(entry, dict) and entry.get('status') == 'READY')
    blocked = sum(1 for entry in entries if isinstance(entry, dict) and entry.get('status') == 'BLOCKED')
    scope = data.get('scope')
    if not isinstance(scope, dict):
        fail('scope', '$.scope', 'expected object')
    else:
        if scope.get('total') != len(entries) or scope.get('total') != 101:
            fail('scope', '$.scope.total', 'must be 101')
        if scope.get('ready') != ready or scope.get('blocked') != blocked:
            fail('scope', '$.scope', {'ready': ready, 'blocked': blocked})
        if scope.get('category_counts') != CATEGORIES:
            fail('scope', '$.scope.category_counts', 'frozen category totals required')
        if not isinstance(scope.get('note'), str) or not scope['note'].strip():
            fail('scope', '$.scope.note', 'nonempty note required')

    computed_a6 = {}
    for entry in entries:
        if not isinstance(entry, dict):
            continue
        for package in entry.get('a6_dependencies') or []:
            row = computed_a6.setdefault(package, {'methods': 0, 'blocked': 0})
            row['methods'] += 1
            row['blocked'] += int(entry.get('status') == 'BLOCKED')
    computed_a6 = {key: computed_a6[key] for key in sorted(computed_a6)}
    if data.get('a6_dependency_summary') != computed_a6:
        fail('a6_summary', '$.a6_dependency_summary', {'expected': computed_a6})

    # ---- every ref in the document resolves and is baseline-pinned --------
    def walk_refs(value, where='$'):
        if isinstance(value, dict):
            for key, child in value.items():
                if key == 'refs' or key.endswith('_refs'):
                    if not isinstance(child, list):
                        fail('reference_format', where + '.' + key, 'refs must be a list')
                    else:
                        for i, ref in enumerate(child):
                            checker.check(ref, f'{where}.{key}[{i}]', fail)
                else:
                    walk_refs(child, where + '.' + key)
        elif isinstance(value, list):
            for i, child in enumerate(value):
                walk_refs(child, f'{where}[{i}]')

    walk_refs(data)

    summary = {
        'method_count': len(entries),
        'ready': ready,
        'blocked': blocked,
        'references_checked': checker.checked,
        'files_checked': len(checker.files),
        'proposed_scenarios': len(proposed_seen),
    }
    return errors, summary


def self_test(data, ledger_raw, ledger, inputs, checker):
    """In-memory negative controls; every mutation must trip its code."""
    cases = []

    def run(name, mutate, expected):
        mutated = copy.deepcopy(data)
        mutate(mutated)
        errors, _ = validate(mutated, ledger_raw, ledger, inputs, checker)
        detected = any(error['code'] == expected for error in errors)
        cases.append({'name': name, 'expected_error': expected,
                      'result': 'PASS' if detected else 'FAIL',
                      'status': 'FAIL' if errors else 'PASS'})

    def blocked_entry(mutated):
        return next(entry for entry in mutated['methods'] if entry['status'] == 'BLOCKED')

    run('remove_one_method', lambda mutated: mutated['methods'].pop(), 'method_set')
    run('duplicate_one_method',
        lambda mutated: mutated['methods'].__setitem__(-1, copy.deepcopy(mutated['methods'][0])),
        'duplicate_method')
    run('remove_owner', lambda mutated: mutated['methods'][0].pop('owner'), 'required_field')
    run('wrong_owner', lambda mutated: mutated['methods'][0].__setitem__('owner', 'A6'), 'owner')
    run('blocked_without_reason',
        lambda mutated: blocked_entry(mutated).__setitem__('blockers', []), 'blocked_reason')
    run('blocker_empty_text',
        lambda mutated: blocked_entry(mutated)['blockers'][0].__setitem__('missing', ''),
        'blocked_reason')
    run('bad_reference_format',
        lambda mutated: mutated['methods'][0]['handler']['refs'].append('B/not-a-reference'),
        'reference_format')
    run('reference_out_of_range',
        lambda mutated: mutated['methods'][0]['handler']['refs'].__setitem__(
            0, mutated['methods'][0]['handler']['refs'][0].rsplit(':', 1)[0] + ':999999'),
        'reference_line')
    run('stage_without_refs',
        lambda mutated: mutated['methods'][0]['provider'].__setitem__('refs', []), 'evidence')
    run('empty_scenario_ids',
        lambda mutated: mutated['methods'][0]['scenario_ids'].__setitem__('proposed', []), 'scenario')
    run('foreign_scenario_id',
        lambda mutated: mutated['methods'][0]['scenario_ids']['proposed'].append('a51.other.success'),
        'scenario')
    run('error_count_mismatch',
        lambda mutated: mutated['methods'][0]['branches']['error'].__setitem__(
            'count', mutated['methods'][0]['branches']['error']['count'] + 1), 'error_branch')
    run('invented_error_code',
        lambda mutated: mutated['methods'][0]['branches']['error']['codes'].append('TOTALLY_FAKE_CODE'),
        'error_codes')
    run('extra_method',
        lambda mutated: mutated['methods'].append(
            dict(copy.deepcopy(mutated['methods'][0]), method='totally_fake_method')), 'method_set')
    run('changed_input_hash',
        lambda mutated: mutated['frozen_inputs']['sha256'].__setitem__(
            'schema/gateway-protocol.schema.json', '0' * 64), 'input_hash')
    run('memory_domain_drift',
        lambda mutated: mutated['domains']['memory'].__setitem__('methods', ['memory_list']), 'domains')
    run('stage_not_from_ledger',
        lambda mutated: mutated['methods'][0]['provider']['refs'].__setitem__(
            0, 'B/src/cli/createLocalGateway.ts:1'), 'ledger_projection')
    run('dropped_error_sample',
        lambda mutated: mutated['methods'][0]['branches']['error'].__setitem__('sample', []),
        'error_sample')
    return cases


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--manifest', type=Path, default=MANIFEST)
    parser.add_argument('--ledger', type=Path, default=LEDGER)
    parser.add_argument('--baseline', type=Path, default=None,
                        help='read-only TS baseline (default: PDRS_BASELINE or ancestor resources/PilotDeck)')
    parser.add_argument('--self-test', action='store_true', help='run in-memory negative controls too')
    args = parser.parse_args()

    baseline = (args.baseline or default_baseline()).resolve()
    try:
        if not baseline.is_dir():
            print(json.dumps({'status': 'FAIL', 'scope': 'a51_coverage_manifest',
                              'failures': [{'code': 'baseline_missing', 'path': '$',
                                            'detail': str(baseline)}],
                              'negative_self_tests': []}, ensure_ascii=False, indent=2))
            return 2
        manifest = read_json(args.manifest)
        ledger_raw = read_json(args.ledger)
        ledger, expansion_errors = expand(ledger_raw)
        if expansion_errors:
            raise ValueError('ledger expansion failed: %r' % expansion_errors)
        inputs = load_inputs()
        checker = RefChecker(baseline, ledger_raw.get('source_files'))
        errors, summary = validate(manifest, ledger_raw, ledger, inputs, checker)
        controls = self_test(manifest, ledger_raw, ledger, inputs, checker) if args.self_test else []
        failed = bool(errors) or any(control['result'] != 'PASS' for control in controls)
        report = {
            'status': 'FAIL' if failed else 'PASS',
            'scope': 'a51_coverage_manifest',
            **summary,
            'failures': errors,
            'negative_self_tests': controls,
        }
    except (OSError, UnicodeError, ValueError, KeyError, TypeError) as exc:
        report = {'status': 'FAIL', 'scope': 'a51_coverage_manifest',
                  'failures': [{'code': 'input', 'path': '$', 'detail': str(exc)}],
                  'negative_self_tests': []}
        failed = True
    print(json.dumps(report, ensure_ascii=False, indent=2))
    return 1 if failed else 0


if __name__ == '__main__':
    sys.exit(main())
