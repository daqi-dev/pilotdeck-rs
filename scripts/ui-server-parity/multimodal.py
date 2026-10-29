"""DRAFT: TS-only registration, complete-class membership, no field wildcard."""
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
from datetime import datetime, timezone
import hashlib
import binascii
import sqlite3
import subprocess
from itertools import combinations
from pathlib import Path

CONNECTIONS = ('ws.chat.2#0', 'ws.chat.3#0')
# Independent scheduling cycles; connections within each broadcast stay joint.
# Ordinals are zero-based. No observed values or member lists are hardcoded.
UNITS = {'U1': (0, ('changeType',)), 'U2': (1, ('changedFile',))}
UNIT_LOCATION = 'chat.projects_updated.independent_units'
MIN_POOL = 20
SOURCE_KEYS = {'node', 'baseline', 'node_modules', 'recorder_proxy'}


def registered_fields(side, units=None):
    """Exact downstream paths for selected units, joint across both connections.

    Return literal values and the two diagnostic aliases. Fragmentation, missing
    fields and shifted sequence indices are not erased by this selector.
    """
    selected = UNITS if units is None else {unit: UNITS[unit] for unit in units}
    values, paths = {}, set()
    for key in CONNECTIONS:
        frames = [(i, f) for i, f in enumerate(side.ws.get(key, []))
                  if f.get('dir') == 'u2c' and f.get('type') == 'projects_updated']
        typed = side.types.get(key, {}).get('projects_updated', [])
        if len(frames) < 2 or len(typed) < 2:
            raise ValueError(f'{key}: missing second projects_updated')
        values[key] = {}
        for ordinal, fields in selected.values():
            index, frame = frames[ordinal]
            body = frame.get('body', {})
            if (frame.get('opcode') != 1 or frame.get('fin') is not True
                    or any(type(body.get(field)) is not str for field in fields)
                    or typed[ordinal].get('dir') != 'u2c' or typed[ordinal].get('body') != body):
                raise ValueError(f'{key}: invalid registered field/envelope at projects_updated[{ordinal}]')
            values[key][f'projects_updated[{ordinal}]'] = {field: body[field] for field in fields}
            for field in fields:
                paths.add(('ws.sequence', f'ws."{key}"[{index}].body.{field}'))
                paths.add(('ws.structure', f'ws.by_type."{key}".projects_updated[{ordinal}].body.{field}'))
    if values[CONNECTIONS[0]] != values[CONNECTIONS[1]]:
        raise ValueError('registered unit has unequal values across broadcast connections')
    return values, paths


def class_matches(classes, comparisons):
    evidence = []
    for group in classes:
        hits = [i for i in group['member_runs'] if comparisons[i - 1]['verdict'] == 'PASS']
        misses = [i for i in group['member_runs'] if comparisons[i - 1]['verdict'] != 'PASS']
        evidence.append({'class': group['class'], 'matched_members': hits,
                         'unmatched_members': misses, 'complete_match': not misses})
    return evidence


def confirmed_classes(count, pairs):
    groups = []
    for i in range(count):
        matches = [g for g in groups if pairs[(g[0], i)]['verdict'] == 'PASS']
        if len(matches) > 1:
            raise ValueError('TS pair relation is not an equivalence relation')
        if matches:
            matches[0].append(i)
        else:
            groups.append([i])
    membership = {i: n for n, group in enumerate(groups) for i in group}
    if any((p['verdict'] == 'PASS') != (membership[i] == membership[j]) for (i, j), p in pairs.items()):
        raise ValueError('TS pair relation is not transitive; no connected-component merging')
    if len(groups) < 2 or any(len(g) < 2 for g in groups):
        raise ValueError('requires >=2 TS classes, each with >=2 independent members')
    return [{'class': n, 'member_runs': [i + 1 for i in group], 'count': len(group)}
            for n, group in enumerate(groups)]


def capture_identity(directory, expected_mode, apparatus, *, historical=False):
    from compare import read_json
    directory = Path(directory).resolve()
    meta = read_json(directory / 'run-meta.json')
    audit = read_json(directory / 'audit.json')
    recorder = [p for p in audit['processes'] if p.get('name') == 'recorder']
    if (meta.get('mode') != expected_mode or meta.get('synthetic') is not False
            or meta.get('apparatus_sha256') != apparatus
            or audit.get('owned_groups_remaining') != [] or audit.get('cleanup_errors') != []
            or len(recorder) != 1 or recorder[0].get('exitCode') != 0
            or type(recorder[0].get('pid')) is not int or recorder[0]['pid'] <= 0):
        raise ValueError(f'{directory}: requires real, clean, same-apparatus {expected_mode} capture')
    source = meta.get('source_sha256')
    keys = [SOURCE_KEYS, SOURCE_KEYS - {'node_modules'}] if historical else [SOURCE_KEYS]
    if (not isinstance(source, dict) or set(source) not in keys
            or any(not isinstance(v, str) or len(v) != 64 or any(c not in '0123456789abcdef' for c in v) for v in source.values())
            or meta.get('source_sha256_end') != source):
        raise ValueError(f'{directory}: incomplete or changed capture-time binary/source hashes')
    start, end = meta.get('wall_start_ms'), meta.get('wall_end_ms')
    if type(start) is not int or type(end) is not int or not 0 < start <= end:
        raise ValueError('invalid capture wall bounds')
    day = lambda ms: datetime.fromtimestamp(ms / 1000, timezone.utc).date().isoformat()
    if day(start) != day(end):
        raise ValueError('capture crosses UTC midnight')
    if not isinstance(meta.get('run_id'), str) or not meta['run_id'] or not isinstance(meta.get('fixture_path'), str) or not meta['fixture_path'].startswith('/'):
        raise ValueError('invalid capture ID/path')
    return {'directory': str(directory), 'run_id': meta['run_id'], 'recorder_pid': recorder[0]['pid'],
            'recording_sha256': hashlib.sha256((directory / 'recording.jsonl').read_bytes()).hexdigest(),
            'capture_utc_day': day(start), 'source_sha256': source, 'fixture_path': meta['fixture_path'],
            'wall_start_ms': start, 'wall_end_ms': end}


class ReferencePool:
    """Construct completely from TS, then admit disjoint scoring captures."""
    def __init__(self, directories, enumeration, matrix=None, scenarios=None, *, replay_pool_of=None):
        from common import apparatus_hashes
        from compare import read_json, HERE
        self.directories = [Path(d).resolve() for d in directories]
        if len(self.directories) < MIN_POOL or len(set(self.directories)) != len(self.directories):
            raise ValueError('TS reference pool requires >=20 distinct capture directories')
        self.replay_provenance = pool_replay_provenance(replay_pool_of) if replay_pool_of else None
        self.recorded_apparatus = (self.replay_provenance['recorded_apparatus_sha256']
                                   if self.replay_provenance else None)
        self.apparatus = self.recorded_apparatus or apparatus_hashes()
        self.enumeration = read_json(enumeration) if not isinstance(enumeration, dict) else enumeration
        self.matrix = read_json(matrix or HERE / 'coverage.json') if not isinstance(matrix, dict) else matrix
        self.scenarios = read_json(scenarios or HERE / 'scenarios.json') if not isinstance(scenarios, dict) else scenarios
        self.identities, self.cache = [], {}
        for directory in self.directories:
            self.admit(directory, 'direct')
            report = self.compare(directory, directory)
            if report['verdict'] != 'PASS':
                raise ValueError(f'{directory}: TS pool self-check failed: {report["differences"][:2]}')
            registered_fields(self.cache[directory][0])
        pairs = {}
        unit_pairs = {unit: {} for unit in UNITS}
        for i, j in combinations(range(len(self.directories)), 2):
            report = self.compare(self.directories[i], self.directories[j])
            masked = self.compare(self.directories[i], self.directories[j], registered=True)
            if masked['verdict'] != 'PASS':
                raise ValueError(f'TS pool pair {i+1}-{j+1}: difference outside registered paths: {masked["differences"][:2]}')
            pairs[(i, j)] = report
            for unit in UNITS:
                unit_pairs[unit][(i, j)] = self.compare(
                    self.directories[i], self.directories[j],
                    registered=tuple(other for other in UNITS if other != unit))
        units = {}
        for unit, relation in unit_pairs.items():
            classes = confirmed_classes(len(self.directories), relation)
            for group in classes:
                side = self.cache[self.directories[group['member_runs'][0] - 1]][0]
                group['values'] = registered_fields(side, (unit,))[0]
            ordinal, fields = UNITS[unit]
            units[unit] = {'location': f'chat.projects_updated[{ordinal}].body.{fields[0]}',
                           'classes': classes,
                           'pairs': {f'{i+1}-{j+1}': p for (i, j), p in relation.items()}}
        self.registration = {'spec_status': 'DRAFT', 'units': units,
                             'unit_combination': 'independent',
                             'calibration_eligible': self.replay_provenance is None,
                             'replay_provenance': self.replay_provenance,
                             'captures': list(self.identities), 'apparatus_sha256': self.apparatus,
                             'pairs': {f'{i+1}-{j+1}': p for (i, j), p in pairs.items()}}
        self.memberships = {}

    def admit(self, directory, mode):
        from compare import read_json
        from common import validate_clock_check
        directory = Path(directory).resolve()
        clock_check = validate_clock_check(read_json(directory / 'run-meta.json'),
                                           historical=self.recorded_apparatus is not None)
        identity = capture_identity(directory, mode, self.apparatus,
                                    historical=self.recorded_apparatus is not None)
        identity['clock_regression_check'] = clock_check
        for previous in self.identities:
            if any(identity[key] == previous[key] for key in ('directory', 'run_id', 'recording_sha256', 'recorder_pid')):
                raise ValueError('reference/scoring captures overlap or duplicate raw recording')
            if any(identity[key] != previous[key] for key in ('fixture_path', 'source_sha256', 'capture_utc_day')):
                raise ValueError('reference/scoring source, fixture path or UTC day differs')
            if max(identity['wall_start_ms'], previous['wall_start_ms']) <= min(identity['wall_end_ms'], previous['wall_end_ms']):
                raise ValueError('capture intervals overlap')
        local = read_json(directory.parent / 'enumeration/router-enumeration.json')
        if (local.get('appCaptured') is not True or local.get('routes') != self.enumeration.get('routes')
                or local.get('uniqueRouteCount') != self.enumeration.get('uniqueRouteCount')):
            raise ValueError('runtime denominator changed')
        self.identities.append(identity)
        return identity

    def prepared(self, directory):
        from compare import Side
        directory = Path(directory).resolve()
        if directory not in self.cache:
            errors = []
            side = Side(directory, str(directory), self.enumeration, self.matrix, self.scenarios, errors,
                        self.recorded_apparatus)
            self.cache[directory] = (side, errors)
        return self.cache[directory]

    def compare(self, left, right, registered=False):
        from compare import compare_sides
        try:
            prepared = (self.prepared(left), self.prepared(right))
        except (OSError, ValueError, TypeError, KeyError, AttributeError, binascii.Error,
                subprocess.TimeoutExpired, sqlite3.Error):
            # Preserve the comparator's structured integrity FAIL for malformed
            # candidates; failed preparation must not crash the ruler.
            prepared = None
        return compare_sides(left, right, self.enumeration, self.matrix, self.scenarios,
                             recorded_apparatus=self.recorded_apparatus,
                             _prepared=prepared, _registered=registered)

    def evaluate(self, directory):
        directory = Path(directory).resolve()
        if directory not in self.memberships:
            comparisons = [self.compare(reference, directory) for reference in self.directories]
            units = {}
            for unit, registration in self.registration['units'].items():
                unit_comparisons = [self.compare(reference, directory,
                    registered=tuple(other for other in UNITS if other != unit))
                    for reference in self.directories]
                matches = class_matches(registration['classes'], unit_comparisons)
                complete = [g['class'] for g in matches if g['complete_match']]
                units[unit] = {'verdict': 'PASS' if complete else 'FAIL',
                               'matched_classes': complete, 'class_matches': matches,
                               'comparisons': unit_comparisons}
            self.memberships[directory] = {'verdict': 'PASS' if all(
                u['verdict'] == 'PASS' for u in units.values()) else 'FAIL', 'units': units,
                                           'comparisons': comparisons}
        return self.memberships[directory]

    def score_pair(self, left, right):
        report = self.compare(left, right, registered=True)
        if report['verdict'] == 'REFUSED':
            return report
        checks = {name: self.evaluate(path) for name, path in (('left', left), ('right', right))}
        report['multimodal'] = checks
        if any(c['verdict'] != 'PASS' for c in checks.values()):
            report['verdict'] = 'FAIL'
            report['differences'].append({'rule': 'multimodal.membership', 'location': UNIT_LOCATION,
                                          'message': 'capture must match all members of a confirmed TS class in each independent unit'})
            report['rules'] = sorted(set(report['rules']) | {'multimodal.membership'})
        return report


def pool_replay_provenance(path):
    """Bind offline re-evaluation to the original report, including REFUSED pools.

    Every real archive must still match this complete recorded apparatus map;
    source/day/identity/health gates remain unchanged. Never a new calibration.
    """
    from common import apparatus_hashes
    from compare import read_json
    prior = read_json(path)
    recorded = prior.get('apparatus_sha256')
    current = apparatus_hashes()
    if (prior.get('k') != 5 or prior.get('comparison_mode') != 'calibration'
            or prior.get('verdict') not in ('REFUSED', 'FAIL', 'PENDING_REVIEW')
            or not isinstance(recorded, dict) or set(recorded) != set(current)
            or any(not isinstance(v, str) or len(v) != 64
                   or any(c not in '0123456789abcdef' for c in v) for v in recorded.values())):
        raise ValueError('pool replay requires the original calibration ruler report and complete apparatus hashes')
    return {'path': str(path), 'sha256': hashlib.sha256(Path(path).read_bytes()).hexdigest(),
            'recorded_apparatus_sha256': recorded, 'current_apparatus_sha256': current,
            'changed_apparatus': sorted(k for k in current if current[k] != recorded[k]),
            'note': 'Offline historical pool replay only; not calibration of the current apparatus.'}


def compare_with_pool(left, right, enumeration, reference_pool, matrix=None, scenarios=None, *, replay_pool_of=None):
    try:
        pool = ReferencePool(reference_pool, enumeration, matrix, scenarios, replay_pool_of=replay_pool_of)
        pool.admit(left, 'direct')
        from compare import read_json
        pool.admit(right, read_json(Path(right) / 'run-meta.json')['mode'])
        report = pool.score_pair(left, right)
        report['TS_calibration'] = pool.registration
        if pool.replay_provenance:
            report['replay_provenance'] = pool.replay_provenance
        return report
    except (OSError, ValueError, KeyError, TypeError, AttributeError, OverflowError) as error:
        return {'verdict': 'REFUSED', 'spec_status': 'DRAFT', 'calibration_eligible': False,
                'errors': [str(error)], 'differences': [], 'rules': []}
