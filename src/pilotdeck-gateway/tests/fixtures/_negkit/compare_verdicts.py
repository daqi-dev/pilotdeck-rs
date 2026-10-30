#!/usr/bin/env python3
# anchor-selfcheck: unsupported read-only summary comparator, no source mutations
"""Compare complete negative-control verdict tables; see README.md for the schema.

Exit 0: equivalent (not necessarily successful runs), 1: different/empty/duplicate
tables, 2: unreadable or unrecognized input. No input or source file is written.
"""
import argparse
import json
from pathlib import Path
import sys


STATUSES = frozenset(('KILLED', 'SURVIVED', 'INVALID', 'PASS'))


class InputError(ValueError):
    """The summary cannot safely be interpreted as a verdict table."""


def unique_members(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise InputError(f'duplicate JSON member: {key!r}')
        result[key] = value
    return result


def reject_constant(value):
    raise InputError(f'non-JSON numeric constant: {value}')


def required_string(value, field):
    if not isinstance(value, str) or not value.strip():
        raise InputError(f'{field}: expected a nonempty string')
    return value


def binding(test, assertion, field):
    required_string(test, field + '.test')
    if isinstance(assertion, list):
        if not assertion:
            raise InputError(f'{field}.assertion: empty marker list')
        for marker in assertion:
            required_string(marker, field + '.assertion[]')
    else:
        required_string(assertion, field + '.assertion')
    # Preserve marker list order and string/list shape; do not infer equivalence.
    return {'test': test, 'assertion': assertion}


def single_binding(row, field):
    if 'assertion' in row:
        return binding(row.get('test'), row['assertion'], field)
    # Some independent negative_process consumers omit the assertion marker.
    # Absence is explicit; present null/empty/wrongly typed markers still fail.
    test = required_string(row.get('test'), field + '.test')
    return {'test': test, 'assertion': ''}


def normalize(row, index):
    field = f'controls[{index}]'
    if not isinstance(row, dict):
        raise InputError(f'{field}: expected an object')
    # Only observed metadata is ignored. In particular, never drop another
    # runner's binding aliases (`anchor`, `message`), even beside `assertion`.
    unknown = row.keys() - {'mutation', 'name', 'status', 'test', 'assertion', 'targets',
                            'reason', 'compiled', 'restoredPass', 'hits'}
    if unknown:
        raise InputError(f'{field}: unsupported row fields: {sorted(unknown)!r}')
    status = row.get('status')
    if not isinstance(status, str) or status not in STATUSES:
        raise InputError(f'{field}.status: unknown status {status!r}')
    if 'mutation' in row:
        if 'name' in row or 'targets' in row:
            raise InputError(f'{field}: mixed mutation and name/targets schemas')
        key = required_string(row['mutation'], field + '.mutation')
        bindings = [single_binding(row, field)]
    elif 'name' in row and 'targets' in row:
        if 'test' in row or 'assertion' in row:
            raise InputError(f'{field}: mixed test/assertion and targets schemas')
        key = required_string(row['name'], field + '.name')
        targets = row['targets']
        if not isinstance(targets, list) or not targets:
            raise InputError(f'{field}.targets: expected a nonempty list')
        bindings = []
        for offset, target in enumerate(targets):
            if not isinstance(target, list) or len(target) != 2:
                raise InputError(f'{field}.targets[{offset}]: expected [test, assertion]')
            bindings.append(binding(*target, f'{field}.targets[{offset}]'))
    elif 'name' in row and 'test' in row:
        key = required_string(row['name'], field + '.name')
        bindings = [single_binding(row, field)]
    else:
        raise InputError(f'{field}: expected mutation/test, name/test or name/targets')
    return {'case': key, 'status': status, 'bindings': bindings}


def load_table(path):
    if not path.is_file():
        raise InputError('expected a readable regular file')
    with path.open(encoding='utf-8') as stream:
        document = json.load(stream, object_pairs_hook=unique_members,
                             parse_constant=reject_constant)
    if not isinstance(document, dict) or not isinstance(document.get('controls'), list):
        raise InputError('expected an object with controls[]')
    return [normalize(row, index) for index, row in enumerate(document['controls'])]


def compare(old, new):
    indexed = {}
    duplicates = []
    for side, table in (('old', old), ('new', new)):
        grouped = {}
        for number, row in enumerate(table, 1):
            grouped.setdefault(row['case'], []).append((number, row))
        indexed[side] = grouped
        for key, entries in sorted(grouped.items()):
            if len(entries) > 1:
                duplicates.append({'side': side, 'case': key,
                                   'rows': [number for number, _ in entries]})
    differences = {
        'missing_cases': [], 'status_changes': [], 'test_changes': [],
        'assertion_changes': [], 'duplicate_keys': duplicates,
        'empty_tables': [side for side, table in (('old', old), ('new', new)) if not table],
    }
    table_diff = []
    same = 0
    keys = sorted(indexed['old'].keys() | indexed['new'].keys())
    for key in keys:
        before = [row for _, row in indexed['old'].get(key, [])]
        after = [row for _, row in indexed['new'].get(key, [])]
        equal = len(before) == len(after) == 1 and before == after
        same += int(equal)
        table_diff.append({'case': key, 'old': before, 'new': after, 'equal': equal})
        if not before or not after:
            differences['missing_cases'].append({
                'case': key, 'missing_from': 'old' if not before else 'new', 'severity': 'P0'})
        # Duplicates already invalidate equivalence. Never choose a winning row.
        if len(before) != 1 or len(after) != 1:
            continue
        left, right = before[0], after[0]
        for category, first, second in (
            ('status_changes', left['status'], right['status']),
            ('test_changes', [b['test'] for b in left['bindings']],
             [b['test'] for b in right['bindings']]),
            ('assertion_changes', [b['assertion'] for b in left['bindings']],
             [b['assertion'] for b in right['bindings']]),
        ):
            if first != second:
                differences[category].append({'case': key, 'old': first, 'new': second})
    return {'equivalent': not any(differences.values()),
            'totals': {'old': len(old), 'new': len(new), 'union': len(keys)},
            'same': same, 'differences': differences, 'table': table_diff, 'errors': []}


def cell(value):
    # JSON quoting keeps newlines/tabs in identifiers from forging table rows.
    return json.dumps(value, ensure_ascii=True, sort_keys=True)


def render(report):
    if report['errors']:
        lines = ['INPUT ERROR (exit 2)']
        for error in report['errors']:
            lines.append(f"{error['side']}: {cell(error['path'])}: {cell(error['reason'])}")
        return '\n'.join(lines)
    totals = report['totals']
    lines = [('EQUIVALENT' if report['equivalent'] else 'NOT EQUIVALENT')
             + f" old={totals['old']} new={totals['new']} union={totals['union']} same={report['same']}",
             '', 'CASE TABLE (case / status / bindings)', 'case\told\tnew\tequal']
    for row in report['table']:
        lines.append('\t'.join((cell(row['case']), cell(row['old']), cell(row['new']),
                                'yes' if row['equal'] else 'no')))
    for key, title in (
        ('missing_cases', 'P0 MISSING CASES (case loss)'),
        ('status_changes', 'STATUS CHANGES'),
        ('test_changes', 'TEST BINDING CHANGES'),
        ('assertion_changes', 'ASSERTION CHANGES'),
        ('duplicate_keys', 'DUPLICATE CASE KEYS'),
        ('empty_tables', 'EMPTY TABLES'),
    ):
        lines += ['', title]
        lines += [cell(item) for item in report['differences'][key]] or ['(none)']
    return '\n'.join(lines)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('old', type=Path, help='old runner summary.json')
    parser.add_argument('new', type=Path, help='new runner summary.json')
    parser.add_argument('--json', action='store_true', help='emit one JSON report to stdout')
    args = parser.parse_args(argv)
    tables, errors = {}, []
    for side, path in (('old', args.old), ('new', args.new)):
        try:
            tables[side] = load_table(path)
        except (OSError, ValueError, RecursionError) as error:
            errors.append({'side': side, 'path': str(path), 'reason': str(error)})
    if errors:
        report = {'equivalent': False, 'totals': None, 'same': 0,
                  'differences': None, 'table': [], 'errors': errors}
        code = 2
    else:
        report = compare(tables['old'], tables['new'])
        code = 0 if report['equivalent'] else 1
    print(json.dumps(report, indent=2, ensure_ascii=True) if args.json else render(report))
    return code


if __name__ == '__main__':
    sys.exit(main())
