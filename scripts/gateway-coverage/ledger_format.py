"""Lossless v2 ledger tables, source-derived groups and deterministic JSON layout.

Array order is evidence: error.order indexes groups (in reference order) followed
by specific records; range associations are contiguous runs with equal roles.
No source reachability or business classification is inferred here.
"""
from collections import defaultdict
import base64
import copy
import hashlib
import json
from pathlib import Path
import re
import sys

SCRIPT_DIR = Path(__file__).resolve().parent
if str(SCRIPT_DIR) not in sys.path:
    # ``python3 -I`` does not put this directory on sys.path when the module
    # is executed directly (``--expand`` / ``--compact``), so the sibling
    # strict-comparison helper needs an explicit path.
    sys.path.insert(0, str(SCRIPT_DIR))

from strict_compare import strict_equal  # noqa: E402

REF = re.compile(r'B/([A-Za-z0-9_./-]+):([0-9]+)')


def identity(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(',', ':'))


def stable_id(value):
    return base64.urlsafe_b64encode(hashlib.sha256(identity(value).encode()).digest()[:4]).decode().rstrip('=')


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError('duplicate_json_key: ' + key)
        result[key] = value
    return result


def read_json(path):
    return json.loads(path.read_text(), object_pairs_hook=unique_object)


def range_index(data):
    result = {}
    tables = data.get('ranges', {})
    if not isinstance(tables, dict):
        raise ValueError('range_definition: expected file table')
    for file, table in tables.items():
        if not isinstance(table, dict):
            raise ValueError('range_definition: expected per-file object')
        for rid, value in table.items():
            if rid in result:
                raise ValueError('range_definition: duplicate range id ' + rid)
            if not isinstance(value, dict):
                raise ValueError('range_definition: expected range object')
            result[rid] = dict(value, file=file)
    return result


def sources(branch, ranges):
    """Innermost recorded, independently bounded callable for each origin ref."""
    found = set()
    for ref in branch.get('refs', []):
        match = REF.fullmatch(ref)
        if not match:
            raise ValueError('error_group_source: malformed producing ref')
        file, line = match[1], int(match[2])
        candidates = [(v['end'] - v['start'], rid) for rid, v in ranges.items()
                      if v['file'] == file and v['start'] <= line <= v['end']]
        if not candidates:
            raise ValueError('error_group_source: origin has no recorded function: ' + ref)
        found.add(min(candidates)[1])
    return sorted(found)


def group_name(source_ids, ranges, members):
    # Function/module names, never code prefixes or hand-maintained domain maps.
    labels = []
    for rid in source_ids:
        row = ranges[rid]
        label = row['symbol']
        if label in ('<anonymous>', 'constructor', 'parse', 'resolve', 'read'):
            label = row['file'].rsplit('/', 1)[-1].split('.')[0] + '_' + label
        label = re.sub(r'([a-z0-9])([A-Z])', r'\1_\2', label)
        label = re.sub(r'[^a-zA-Z0-9]+', '_', label).strip('_').lower()
        labels.append(label)
    entry = max(source_ids, key=lambda rid: (ranges[rid]['end'] - ranges[rid]['start'], rid))
    label = labels[source_ids.index(entry)]
    return label + ('_chain' if len(source_ids) > 1 else '') + '_' + stable_id([source_ids, members])


def compact(data):
    """Extract whole identical records by producing ranges and consumer set."""
    if not strict_equal(data.get('format_version'), 1):
        raise ValueError('compact expects expanded format_version=1')
    result = copy.deepcopy(data)
    result['format_version'] = 2
    ranges, table, points, texts = {}, defaultdict(dict), {}, {}
    for entry in result['methods']:
        runs = []
        for old in entry['source_audit']['functions']:
            first, last = REF.fullmatch(old['refs'][0]), REF.fullmatch(old['refs'][-1])
            file, start, end = first[1], int(first[2]), int(last[2])
            row = dict(file=file, start=start, end=end, symbol=old['symbol'], sha256=old['sha256'])
            rid = stable_id([file, start, end, old['symbol']])
            if rid in ranges and ranges[rid] != row:
                raise ValueError('conflicting range: ' + rid)
            ranges[rid] = row
            table[file][rid] = {k: v for k, v in row.items() if k != 'file'}
            role = {k: v for k, v in old.items() if k not in ('refs', 'symbol', 'sha256')}
            if not runs or {k: v for k, v in runs[-1].items() if k != 'ranges'} != role:
                runs.append(dict(role, ranges=[]))
            runs[-1]['ranges'].append(rid)
        entry['source_audit']['functions'] = runs
        ids = []
        for point in entry['source_audit']['default_constructions']:
            pid = stable_id(point)
            if pid in points and points[pid] != point:
                raise ValueError('conflicting construction id')
            points[pid] = point
            ids.append(pid)
        entry['source_audit']['default_constructions'] = ids
        for container, field in ((entry, 'audit_scope'), (entry['source_audit'], 'dynamic_boundary')):
            if field in container:
                tid = stable_id(container[field])
                if tid in texts and texts[tid] != container[field]:
                    raise ValueError('conflicting audit text id')
                texts[tid] = container[field]
                container[field] = {'text': tid}
    starts = {(row['file'], row['start']): rid for rid, row in sorted(ranges.items())}
    for entry in result['methods']:
        for container, field in ((entry['provider'], 'default_refs'), (entry['source_audit'], 'automatic_providers')):
            container[field] = [starts[(REF.fullmatch(ref)[1], int(REF.fullmatch(ref)[2]))] for ref in container[field]]
    result['ranges'] = dict(table)
    result['construction_points'] = points
    result['audit_texts'] = texts
    records, consumers = {}, defaultdict(list)
    for index, entry in enumerate(data['methods']):
        for branch in entry['branches']['error']:
            key = identity(branch)
            records[key] = branch
            consumers[key].append(index)
    buckets = defaultdict(list)
    for key in sorted(records):
        branch = records[key]
        if len(set(consumers[key])) > 1 and branch.get('observed') == 'lexical':
            provenance = tuple(sources(branch, ranges))
            buckets[provenance, tuple(sorted(set(consumers[key])))].append(key)
    groups, assignment = {}, {}
    for (provenance, users), keys in sorted(buckets.items()):
        members = [copy.deepcopy(records[k]) for k in sorted(keys, key=lambda k: records[k]['code'])]
        name = group_name(list(provenance), ranges, [b['code'] for b in members])
        definition = {'source_ranges': list(provenance), 'errors': members}
        if name in groups and groups[name] != definition:
            raise ValueError('conflicting group name: ' + name)
        groups[name] = definition
        for key in keys:
            assignment[key] = name
    result['error_code_groups'] = groups
    for entry in result['methods']:
        original = entry['branches']['error']
        names = sorted({assignment[identity(b)] for b in original if identity(b) in assignment})
        specific = [b for b in original if identity(b) not in assignment]
        flat = [b for name in names for b in groups[name]['errors']] + specific
        indexes = {identity(b): i for i, b in enumerate(flat)}
        if len(indexes) != len(flat):
            raise ValueError('duplicate error record')
        entry['branches']['error'] = {'groups': names, 'specific': specific,
                                      'order': [indexes[identity(b)] for b in original]}
    # Repeated error explanations differ only in the already stored code.
    # Infer prefix/suffix verbatim; never rewrite the explanation.
    candidates = defaultdict(list)
    def collect(value):
        if isinstance(value, dict):
            summary, code = value.get('summary'), value.get('code')
            if isinstance(summary, str) and isinstance(code, str) and summary.count(code) == 1:
                parts = summary.split(code)
                if len(summary.encode()) > 100:
                    candidates[tuple(parts)].append(value)
            for child in value.values():
                collect(child)
        elif isinstance(value, list):
            for child in value:
                collect(child)
    collect(result)
    templates = {}
    for parts, records in candidates.items():
        if len(records) > 1:
            tid = stable_id(parts)
            if tid in templates and templates[tid] != list(parts):
                raise ValueError('conflicting summary template')
            templates[tid] = list(parts)
            for record in records:
                record['summary'] = {'template': tid}
    result['error_summaries'] = templates
    return result


def expand(data):
    """Return original v1 shape and structural failures before normal checks."""
    if not isinstance(data, dict):
        return copy.deepcopy(data), []
    version = data.get('format_version')
    if not strict_equal(version, 2):
        if strict_equal(version, 1):
            return copy.deepcopy(data), []
        # A float/bool retype (2.0 / True) is not a v2 ledger: silently
        # treating it as v1 and rewriting ``format_version`` to 1 would hide
        # the retype from every downstream pin.
        return copy.deepcopy(data), [{
            'code': 'format_version', 'path': '$.format_version',
            'detail': 'format_version must be exactly int 1 (expanded) or 2 (compact)',
        }]
    result, failures = copy.deepcopy(data), []
    for table in ('ranges', 'error_code_groups', 'construction_points', 'audit_texts', 'error_summaries'):
        if not isinstance(data.get(table), dict):
            raise ValueError(table + ': expected definition table')
    if not isinstance(result.get('methods'), list) or any(not isinstance(e, dict) for e in result['methods']):
        raise ValueError('methods: expected object list')

    def fail(code, path, detail):
        failures.append({'code': code, 'path': path, 'detail': detail})

    used_templates = set()
    templates = data.get('error_summaries', {})
    def restore(value, path='$'):
        if isinstance(value, dict):
            summary = value.get('summary')
            if isinstance(summary, dict):
                tid = summary.get('template')
                parts = templates.get(tid) if isinstance(tid, str) else None
                if (set(summary) != {'template'} or not isinstance(parts, list) or len(parts) != 2
                        or any(not isinstance(p, str) for p in parts) or not isinstance(value.get('code'), str)):
                    fail('error_summary_reference', path, 'invalid summary template')
                else:
                    used_templates.add(tid)
                    value['summary'] = parts[0] + value['code'] + parts[1]
            for key, child in value.items():
                restore(child, path + '.' + key)
        elif isinstance(value, list):
            for i, child in enumerate(value):
                restore(child, f'{path}[{i}]')
    restore(result)
    if used_templates != set(templates) or any(tid != stable_id(parts) for tid, parts in templates.items()):
        fail('error_summary_reference', '$.error_summaries', 'unused template or unstable id')
    ranges = range_index(data)
    for rid, row in ranges.items():
        if (set(row) != {'file', 'start', 'end', 'sha256', 'symbol'}
                or not isinstance(row['start'], int) or not isinstance(row['end'], int)
                or row['start'] < 1 or row['end'] < row['start']
                or rid != stable_id([row['file'], row['start'], row['end'], row['symbol']])):
            fail('range_definition', '$.ranges.' + rid, 'invalid range or unstable id')
    groups = result.get('error_code_groups', {})
    if not isinstance(groups, dict):
        raise ValueError('error_group_definition: expected group table')
    used_groups, used_ranges, used_points, used_texts = set(), set(), set(), set()
    for name, group in groups.items():
        path = '$.error_code_groups.' + name
        if not isinstance(group, dict) or set(group) != {'source_ranges', 'errors'} or not isinstance(group.get('errors'), list) or not group['errors']:
            fail('error_group_definition', path, 'nonempty errors and one source range signature required')
            continue
        provenance = group['source_ranges']
        if not isinstance(provenance, list) or not provenance or any(not isinstance(r, str) or r not in ranges for r in provenance):
            fail('error_group_source', path, 'unknown or invalid source range')
            continue
        used_ranges.update(provenance)
        codes = [b.get('code') for b in group['errors'] if isinstance(b, dict)]
        if len(codes) != len(group['errors']) or any(not isinstance(c, str) for c in codes) or len(set(codes)) != len(codes):
            fail('error_group_definition', path, 'unique string codes required')
            continue
        expected_sources = sorted({rid for b in group['errors'] for rid in sources(b, ranges)})
        if provenance != expected_sources or any(sources(b, ranges) != provenance for b in group['errors']):
            fail('error_group_source', path, 'group declared by conflicting producing ranges')
        expected_name = group_name(provenance, ranges, codes)
        if name != expected_name:
            fail('error_group_source', path, 'name disagrees with source function signature and members')
    for i, entry in enumerate(result.get('methods', [])):
        path = f'$.methods[{i}]'
        audit = entry.get('source_audit', {})
        for container, field in ((entry, 'audit_scope'), (audit, 'dynamic_boundary')):
            if field not in container:
                continue
            value = container[field]
            tid = value.get('text') if isinstance(value, dict) and set(value) == {'text'} else None
            if not isinstance(tid, str) or tid not in data.get('audit_texts', {}):
                fail('audit_text_reference', path, 'unknown audit text')
            else:
                used_texts.add(tid)
                container[field] = data['audit_texts'][tid]
        for container, field in ((entry.get('provider', {}), 'default_refs'), (audit, 'automatic_providers')):
            if field not in container:
                continue
            refs = []
            for rid in container[field]:
                if not isinstance(rid, str) or rid not in ranges:
                    fail('range_reference', path, 'unknown provider range: ' + str(rid))
                else:
                    used_ranges.add(rid)
                    refs.append(f"B/{ranges[rid]['file']}:{ranges[rid]['start']}")
            container[field] = refs
        expanded = []
        for run in audit.get('functions', []):
            if not isinstance(run, dict) or not isinstance(run.get('ranges'), list) or set(run) - {'ranges', 'role', 'assembly', 'optional'}:
                fail('range_reference', path, 'expected range id list with role/assembly')
                continue
            for rid in run['ranges']:
                if not isinstance(rid, str) or rid not in ranges:
                    fail('range_reference', path, 'unknown range: ' + str(rid))
                    continue
                used_ranges.add(rid)
                row = ranges[rid]
                refs = [f"B/{row['file']}:{row['start']}"]
                if row['end'] != row['start']:
                    refs.append(f"B/{row['file']}:{row['end']}")
                expanded.append(dict(symbol=row['symbol'], refs=refs, sha256=row['sha256'],
                                     **{k: v for k, v in run.items() if k != 'ranges'}))
        if 'functions' in audit:
            audit['functions'] = expanded
        points = []
        for pid in audit.get('default_constructions', []):
            if not isinstance(pid, str) or pid not in data.get('construction_points', {}):
                fail('construction_reference', path, 'unknown construction: ' + str(pid))
            else:
                used_points.add(pid)
                points.append(copy.deepcopy(data['construction_points'][pid]))
        if 'default_constructions' in audit:
            audit['default_constructions'] = points
        branches = entry.get('branches', {})
        error = branches.get('error', {})
        if not isinstance(error, dict) or set(error) != {'groups', 'specific', 'order'} or not all(isinstance(error[k], list) for k in error):
            fail('error_group_reference', path, 'groups/specific/order lists required')
            branches['error'] = []
            continue
        flat = []
        if len(set(error['groups'])) != len(error['groups']):
            fail('error_group_reference', path, 'duplicate group reference')
        for name in error['groups']:
            if not isinstance(name, str) or name not in groups:
                fail('error_group_reference', path, 'unknown group: ' + str(name))
            else:
                used_groups.add(name)
                definition = groups[name]
                if not isinstance(definition, dict) or not isinstance(definition.get('errors'), list):
                    fail('error_group_definition', path, 'invalid referenced group')
                    continue
                flat.extend(copy.deepcopy(definition['errors']))
        flat.extend(error['specific'])
        order = error['order']
        if any(type(n) is not int for n in order) or sorted(order) != list(range(len(flat))):
            fail('error_group_order', path, 'order must reference every expanded record exactly once')
            branches['error'] = flat
        else:
            branches['error'] = [flat[n] for n in order]
    if used_groups != set(groups):
        fail('error_group_definition', '$.error_code_groups', 'unused group definitions')
    if used_ranges != set(ranges):
        fail('range_definition', '$.ranges', 'unused range definitions')
    if used_points != set(data.get('construction_points', {})):
        fail('construction_reference', '$.construction_points', 'unused construction definitions')
    for pid, point in data.get('construction_points', {}).items():
        if pid != stable_id(point):
            fail('construction_reference', '$.construction_points.' + pid, 'unstable construction id')
    if used_texts != set(data.get('audit_texts', {})):
        fail('audit_text_reference', '$.audit_texts', 'unused audit text definitions')
    for tid, text in data.get('audit_texts', {}).items():
        if not isinstance(text, str) or tid != stable_id(text):
            fail('audit_text_reference', '$.audit_texts.' + tid, 'invalid text or unstable id')
    for field in ('ranges', 'error_code_groups', 'construction_points', 'audit_texts', 'error_summaries'):
        result.pop(field, None)
    result['format_version'] = 1
    return result, failures


def dumps(data):
    """Sorted keys, newline-delimited structures; scalar arrays/table rows one line.

    No indentation: growing explicit dynamic-origin evidence must retain the
    1.5 MB artifact budget without dropping records or shortening prose.
    """
    def render(value, depth, path):
        pad, child_pad = '', ''
        if isinstance(value, dict):
            if not value:
                return '{}'
            # Atomic range and construction rows remain separately diffable.
            if ('summary' in value and 'refs' in value
                    or 'ranges' in value and 'role' in value
                    or len(path) == 3 and path[0] == 'ranges'
                    or len(path) == 2 and path[0] in ('construction_points', 'source_files')):
                return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(',', ':'))
            return '{\n' + ',\n'.join(child_pad + json.dumps(k, ensure_ascii=False) + ': ' + render(value[k], depth + 1, path + (k,)) for k in sorted(value)) + '\n' + pad + '}'
        if isinstance(value, list):
            if not value or all(not isinstance(v, (dict, list)) for v in value):
                return json.dumps(value, ensure_ascii=False, separators=(',', ': '))
            return '[\n' + ',\n'.join(child_pad + render(v, depth + 1, path + (str(i),)) for i, v in enumerate(value)) + '\n' + pad + ']'
        return json.dumps(value, ensure_ascii=False)
    return render(data, 0, ()) + '\n'


def main():
    import argparse
    from pathlib import Path
    parser = argparse.ArgumentParser(description=__doc__)
    operation = parser.add_mutually_exclusive_group(required=True)
    operation.add_argument('--compact', type=Path, help='read an expanded v1 ledger')
    operation.add_argument('--expand', type=Path, help='read a table-based v2 ledger')
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    try:
        if args.compact:
            original = read_json(args.compact)
            result = compact(original)
            restored, failures = expand(result)
            if failures or restored != original:
                raise ValueError('compaction is not lossless: ' + str(failures))
        else:
            result, failures = expand(read_json(args.expand))
            if failures:
                raise ValueError('invalid references: ' + str(failures))
        args.output.write_text(dumps(result))
    except (OSError, ValueError, TypeError, KeyError) as exc:
        parser.exit(1, str(exc) + '\n')


if __name__ == '__main__':
    main()
