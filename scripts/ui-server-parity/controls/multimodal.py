"""Synthetic end-to-end 1B controls; identity stub is local to this test only."""
# Isolated bootstrap: verify the source loader before executing local code.
import os as _boot_os
import sys as _boot_sys
if not (_boot_sys.flags.isolated and _boot_sys.flags.ignore_environment
        and _boot_sys.flags.no_user_site and _boot_sys.pycache_prefix is None):
    print('{"verdict":"FAIL","errors":["Python isolation required: use python3 -I; no pycache prefix"]}')
    raise SystemExit(1)
_boot_sys.dont_write_bytecode = True
_boot_dir = _boot_os.path.dirname(_boot_os.path.dirname(_boot_os.path.abspath(__file__)))
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
import copy
import base64
import json
from pathlib import Path
import shutil
import tempfile
from unittest.mock import patch

from multimodal import ReferencePool, compare_with_pool, UNIT_LOCATION
from compare import compare_sides, read_jsonl


def run_multimodal_controls():
    from controls.run_all import fixture, write_json, write_log, b64, hdr
    results = []
    def record(name, verdict, expected):
        results.append({'name': 'multimodal-' + name, 'level': 'synthetic-reference-pool',
                        'expected_verdict': expected, 'verdict': verdict, 'passed': verdict == expected,
                        'expected_rule': None})
    with tempfile.TemporaryDirectory(prefix='b2-multimodal-') as temp:
        root = Path(temp).resolve()
        enumeration, matrix, manifest = fixture(root)
        path = root / 'left/recording.jsonl'
        rows = read_jsonl(path)
        for row in rows:
            if row.get('case') == 'ws.probe.1':
                row['case'] = 'ws.chat.2'
        frame = next(r for r in rows if r['kind'] == 'ws_frame' and r['dir'] == 'u2c')
        close = next(r for r in rows if r['kind'] == 'ws_frame' and r['opcode'] == 8)
        transcript = 'exp008-fixture/chats/web:33333333-3333-4333-8333-000000000002.jsonl'
        name = transcript + '.projections.json'
        for value in ('first.jsonl', name):
            body = json.dumps({'type': 'projects_updated', 'changedFile': value, 'changeType': 'change' if value == 'first.jsonl' else 'add', 'projects': []})
            new = {**frame, 'payload_b64': b64(body), 'bytes': len(body), 'arrival_ns': close['arrival_ns']}
            rows.insert(rows.index(close), new)
        # Duplicate one complete WS conversation with another exact case key.
        second = [copy.deepcopy(r) for r in rows if r.get('id') == 4 or r.get('connection') == 1]
        for row in second:
            if 'id' in row:
                row['id'] = 5
            if 'connection' in row:
                row['connection'] = 2
            if row.get('case') == 'ws.chat.2':
                row['case'] = 'ws.chat.3'
        rows[-1:-1] = second
        write_log(path, rows)
        def mutate(directory, fn):
            path = directory / 'recording.jsonl'
            rows = read_jsonl(path)
            for row in rows:
                if row['kind'] == 'ws_frame' and row['opcode'] == 1:
                    body = json.loads(base64.b64decode(row['payload_b64']))
                    fn(body, row)
                    raw = json.dumps(body).encode()
                    row.update(payload_b64=b64(raw), bytes=len(raw))
            write_log(path, rows)
        dirs = []
        for i in range(23):
            parent = root / f'run{i}'
            directory = parent / 'direct'
            shutil.copytree(root / 'left', directory)
            (parent / 'enumeration').mkdir()
            write_json(parent / 'enumeration/router-enumeration.json', enumeration)
            if i in (15, 16):
                mutate(directory, lambda b, r: b.update(changedFile=transcript) if b.get('changedFile') == name else None)
            if 17 <= i < 20 or i == 21:
                mutate(directory, lambda b, r: b.update(changeType='add') if b.get('changedFile') == 'first.jsonl' else None)
            if i in (21, 22):
                meta = json.loads((directory / 'run-meta.json').read_text())
                meta['mode'] = 'proxy'
                write_json(directory / 'run-meta.json', meta)
                proxy_rows = read_jsonl(directory / 'recording.jsonl')
                proxy_rows[0]['upstream'] = 'http://127.0.0.1:13103'
                for row in proxy_rows:
                    if row['kind'] == 'http_response':
                        row['handler'] = 'ts'
                        row['headers'].append(hdr('x-pilotdeck-handler', 'ts'))
                write_log(directory / 'recording.jsonl', proxy_rows)
                cases = read_jsonl(directory / 'cases.jsonl')
                for case in cases:
                    case.update(side='proxy', handler='ts')
                (directory / 'cases.jsonl').write_text(''.join(json.dumps(c) + '\n' for c in cases))
            dirs.append(directory)
        def synthetic_identity(directory, mode, apparatus):
            # Never write fake real run metadata. Production identity gate is
            # separately tested below and unpatched in all CLI entry points.
            i = dirs.index(Path(directory))
            return {'directory': str(directory), 'run_id': str(i), 'recording_sha256': str(i),
                    'recorder_pid': i + 1, 'fixture_path': '/fixture/common/direct',
                    'source_sha256': {'test': 'synthetic'}, 'capture_utc_day': '1970-01-01',
                    'wall_start_ms': i * 3000 + 1, 'wall_end_ms': i * 3000 + 2000}
        refused = compare_with_pool(dirs[20], dirs[21], enumeration, dirs[:19], matrix, manifest)
        record('pool-under-20', refused['verdict'], 'REFUSED')
        # Real entry must reject these synthetic fixtures even with 20 members.
        record('synthetic-pool-rejected', compare_with_pool(dirs[20], dirs[21], enumeration, dirs[:20], matrix, manifest)['verdict'], 'REFUSED')
        with patch('multimodal.capture_identity', side_effect=synthetic_identity):
            pool = ReferencePool(dirs[:20], enumeration, matrix, manifest)
            pool.admit(dirs[20], 'direct')
            pool.admit(dirs[21], 'proxy')
            report = pool.score_pair(dirs[20], dirs[21])
            record('independent-complete-classes', report['verdict'], 'PASS')
            assert [c['count'] for c in pool.registration['units']['U1']['classes']] == [17, 3]
            assert [c['count'] for c in pool.registration['units']['U2']['classes']] == [18, 2]
            assert len(pool.registration['pairs']) == 190
            assert all(len(u['pairs']) == 190 for u in pool.registration['units'].values())
            record('second-file-class-complete', pool.evaluate(dirs[15])['verdict'], 'PASS')
            record('strict-pair-still-fails', compare_sides(dirs[20], dirs[21], enumeration, matrix, manifest)['verdict'], 'FAIL')
            def fresh_score():
                pool.cache.pop(dirs[22], None)
                pool.memberships.pop(dirs[22], None)
                return pool.score_pair(dirs[20], dirs[22])
            original = (dirs[22] / 'recording.jsonl').read_bytes()
            for label, fn in [
                ('outside-classes', lambda b, r: b.update(changedFile='third-file.json') if b.get('changedFile') == name else None),
                ('mixed-connections', lambda b, r: b.update(changedFile=transcript) if b.get('changedFile') == name and r['connection'] == 2 else None),
                ('U1-outside-classes', lambda b, r: b.update(changeType='unlink') if b.get('changedFile') == 'first.jsonl' else None),
                ('U1-mixed-connections', lambda b, r: b.update(changeType='add') if b.get('changedFile') == 'first.jsonl' and r['connection'] == 2 else None),
                ('first-changedFile-not-registered', lambda b, r: b.update(changedFile='first-changed.jsonl') if b.get('changedFile') == 'first.jsonl' else None),
                ('unknown-field', lambda b, r: b.update(extra='bad') if b.get('changedFile') == name else None),
                ('missing-field', lambda b, r: b.pop('changedFile') if b.get('changedFile') == name else None),
                ('null-field', lambda b, r: b.update(changedFile=None) if b.get('changedFile') == name else None),
                ('missing-changeType', lambda b, r: b.pop('changeType') if b.get('changedFile') == 'first.jsonl' else None),
                ('null-changeType', lambda b, r: b.update(changeType=None) if b.get('changedFile') == 'first.jsonl' else None),
                ('type-changeType', lambda b, r: b.update(changeType=1) if b.get('changedFile') == 'first.jsonl' else None),
                ('second-changeType-not-registered', lambda b, r: b.update(changeType='change') if b.get('changedFile') == name else None),
            ]:
                (dirs[22] / 'recording.jsonl').write_bytes(original)
                mutate(dirs[22], fn)
                report = fresh_score()
                record(label, report['verdict'], 'FAIL')
                hits = [d for d in report['differences'] if d['rule'] == 'multimodal.membership'
                        and d['location'] == UNIT_LOCATION]
                results[-1].update(expected_rule='multimodal.membership', matched=hits,
                                   passed=results[-1]['passed'] and bool(hits))
            # TS2-like combination absent from the joint pool, but independently
            # confirmed: U1 add (3 members) and U2 transcript (2 members).
            (dirs[22] / 'recording.jsonl').write_bytes(original)
            mutate(dirs[22], lambda b, r: b.update(changedFile=transcript) if b.get('changedFile') == name
                   else b.update(changeType='add') if b.get('changedFile') == 'first.jsonl' else None)
            report = fresh_score()
            record('independent-TS2-combination', report['verdict'], 'PASS')
            membership = report['multimodal']['right']
            assert all(c['verdict'] == 'FAIL' for c in membership['comparisons'])
            for unit, members in [('U1', [18, 19, 20]), ('U2', [16, 17])]:
                matched = membership['units'][unit]
                assert matched['matched_classes'] == [1]
                assert matched['class_matches'][1]['matched_members'] == members
                assert matched['class_matches'][1]['unmatched_members'] == []
            results[-1]['units'] = {u: m['class_matches'] for u, m in membership['units'].items()}
            (dirs[22] / 'recording.jsonl').write_text('')
            record('candidate-health-gate', fresh_score()['verdict'], 'FAIL')
            member_original = (dirs[0] / 'recording.jsonl').read_bytes()
            mutate(dirs[0], lambda b, r: b.update(unregistered='noise') if b.get('changedFile') == name else None)
            refused = compare_with_pool(dirs[20], dirs[21], enumeration, dirs[:20], matrix, manifest)
            record('pool-unregistered-noise', refused['verdict'], 'REFUSED')
            (dirs[0] / 'recording.jsonl').write_bytes(member_original)
            (dirs[22] / 'recording.jsonl').write_bytes(original)
            # Candidate is within 250ms of one member, >250ms from the other
            # sixteen in U1 / seventeen in U2; pool members are within the bound.
            # This exercises FULL member comparisons, not a mocked pass matrix.
            pool.cache.clear()
            pool.memberships.clear()
            rows0 = read_jsonl(dirs[0] / 'recording.jsonl')
            for r in rows0:
                if r['kind'] in ('http_chunk', 'http_end') and r.get('id') == 3 and r.get('direction') == 'response':
                    r['arrival_ns'] = str(int(r['arrival_ns']) + 100_000_000)
            write_log(dirs[0] / 'recording.jsonl', rows0)
            pool = ReferencePool(dirs[:20], enumeration, matrix, manifest)
            rows22 = read_jsonl(dirs[22] / 'recording.jsonl')
            for r in rows22:
                if r['kind'] in ('http_chunk', 'http_end') and r.get('id') == 3 and r.get('direction') == 'response':
                    r['arrival_ns'] = str(int(r['arrival_ns']) + 300_000_000)
            write_log(dirs[22] / 'recording.jsonl', rows22)
            membership = pool.evaluate(dirs[22])
            for unit in ('U1', 'U2'):
                assert membership['units'][unit]['class_matches'][0]['matched_members'] == [1], membership
            record('partial-class-members', membership['verdict'], 'FAIL')
            results[-1]['units'] = {u: m['class_matches'] for u, m in membership['units'].items()}
    return results
