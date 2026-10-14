"""Round 3 end-to-end schema-1 mutations, through wire validation and dispatcher."""
import base64
import copy
from datetime import datetime, timezone
import json
from pathlib import Path
import shutil
import tempfile

from compare import compare_sides, read_jsonl


def run_stream_clock_controls(fixture, write_log):
    results = []
    with tempfile.TemporaryDirectory(prefix='b2-stream-clock-') as directory:
        root = Path(directory)
        enumeration, matrix, manifest = fixture(root)

        def frame(template, direction, body):
            raw = json.dumps(body, ensure_ascii=False).encode()
            return {**template, 'dir': direction, 'masked': direction == 'c2u',
                    'leg': 'downstream-read' if direction == 'c2u' else 'downstream-write',
                    'bytes': len(raw), 'payload_b64': base64.b64encode(raw).decode()}

        def body(row):
            return json.loads(base64.b64decode(row['payload_b64']))

        def replace(row, value):
            row.update(frame(row, row['dir'], value))

        for label in ('left', 'right'):
            path = root / label / 'recording.jsonl'
            rows = read_jsonl(path)
            # Add a separate /shell connection, leaving chat/OOB evidence intact.
            handshake = [copy.deepcopy(r) for r in rows if r.get('id') == 4]
            for r in handshake:
                r['id'] = 5
                if 'connection' in r:
                    r['connection'] = 2
                if r['kind'] == 'http_request':
                    r.update(path='/shell', case='ws.pty.2')
            template = {**next(r for r in rows if r['kind'] == 'ws_frame'), 'connection': 2}
            def f(direction, value):
                return frame(template, direction, value)
            chunks = ['print 世界\r\n', '\x1b[31mTAIL\x1b[0m'] if label == 'left' else ['pr', 'int 世', '界\r\n\x1b[31m', 'TAIL\x1b[0m']
            outputs = [f('u2c', {'type': 'output', 'data': value}) for value in chunks]
            resizes = [f('c2u', {'type': 'resize', 'cols': 80, 'rows': 24}),
                       f('c2u', {'type': 'resize', 'cols': 100, 'rows': 30})]
            mixed = outputs + resizes if label == 'left' else resizes[:1] + outputs[:1] + resizes[1:] + outputs[1:]
            shell = [f('c2u', {'type': 'init', 'cols': 80, 'rows': 24})] + mixed
            shell += [f('u2c', {'type': 'auth_url', 'url': url}) for url in ('https://a.test', 'https://b.test')]
            # Control frames may interrupt a fragmented message. Their relative
            # position remains significant for all non-output frames.
            raw_input = json.dumps({'type': 'input', 'data': 'ls\n'}).encode()
            cut = len(raw_input) // 2
            shell += [
                {**template, 'fin': False, 'bytes': cut, 'payload_b64': base64.b64encode(raw_input[:cut]).decode()},
                {**template, 'opcode': 9, 'bytes': 1, 'payload_b64': base64.b64encode(b'p').decode()},
                {**template, 'opcode': 0, 'bytes': len(raw_input) - cut, 'payload_b64': base64.b64encode(raw_input[cut:]).decode()},
            ]
            shell += [{**r, 'connection': 2} for r in rows if r['kind'] == 'ws_frame' and r['opcode'] == 8]
            shell += [{**next(r for r in rows if r['kind'] == 'ws_end'), 'connection': 2}]
            rows[-1:-1] = handshake + shell
            # Two entities with equal aliases and strictly ordered lifetimes.
            offset = 0 if label == 'left' else 86400000
            def iso(ms):
                return datetime.fromtimestamp((1700000000000 + offset + ms) / 1000, timezone.utc).isoformat(timespec='milliseconds')
            sessions = []
            for i in (1, 2):
                sessions.append({'id': 'fixed-' + str(i), 'createdAt': iso(i * 1000),
                                 'created_at': iso(i * 1000), 'lastActivity': iso(i * 1000 + 500),
                                 'updated_at': iso(i * 1000 + 500)})
            watcher = {'type': 'projects_updated', 'projects': [{'name': 'p', 'sessions': sessions,
                        'lastActivity': 1700000002500 + offset}],
                       'projectListRevision': 1700000003000 + offset, 'timestamp': iso(4000),
                       'changedFile': 'literal.jsonl'}
            chat_template = next(r for r in rows if r['kind'] == 'ws_frame' and r['connection'] == 1)
            close = next(i for i, r in enumerate(rows) if r['kind'] == 'ws_frame' and r['connection'] == 1 and r['opcode'] == 8)
            rows.insert(close, frame(chat_template, 'u2c', watcher))
            write_log(path, rows)

        def mutate(rows, name):
            complete = [r for r in rows if r['kind'] == 'ws_frame' and r['opcode'] == 1 and r['fin']]
            outputs = [r for r in complete if body(r).get('type') == 'output']
            resizes = [r for r in complete if body(r).get('type') == 'resize']
            watcher = next(r for r in complete if body(r).get('type') == 'projects_updated')
            value = body(watcher)
            session = value['projects'][0]['sessions'][0]
            if name == 'pty-byte':
                replace(outputs[0], {**body(outputs[0]), 'data': 'pR'})
            elif name == 'pty-drop':
                rows.remove(outputs[1])
            elif name == 'pty-extra':
                rows.insert(rows.index(outputs[1]), copy.deepcopy(outputs[1]))
            elif name == 'pty-order':
                a, b = rows.index(outputs[0]), rows.index(outputs[1])
                rows[a], rows[b] = rows[b], rows[a]
            elif name in ('pty-envelope', 'both-pty-envelope'):
                replace(outputs[0], {**body(outputs[0]), 'unexpected': True})
            elif name == 'pty-mask':
                resizes[0]['masked'] = False
            elif name == 'fragment-control-order':
                ping = next(r for r in rows if r['kind'] == 'ws_frame' and r['opcode'] == 9)
                index = rows.index(ping)
                rows[index - 1], rows[index] = rows[index], rows[index - 1]
            elif name == 'pty-direction':
                outputs[0].update(frame(outputs[0], 'c2u', body(outputs[0])))
            elif name in ('resize-value', 'resize-drop', 'resize-extra', 'resize-order'):
                if name == 'resize-value':
                    replace(resizes[0], {**body(resizes[0]), 'cols': 81})
                elif name == 'resize-drop':
                    rows.remove(resizes[0])
                elif name == 'resize-extra':
                    rows.insert(rows.index(resizes[0]), copy.deepcopy(resizes[0]))
                else:
                    a, b = rows.index(resizes[0]), rows.index(resizes[1])
                    rows[a], rows[b] = rows[b], rows[a]
            elif name == 'pty-fragment-equivalence':
                row = outputs[0]
                raw = base64.b64decode(row['payload_b64'])
                cut = len(raw) // 2
                tail = {**row, 'opcode': 0, 'payload_b64': base64.b64encode(raw[cut:]).decode(), 'bytes': len(raw) - cut}
                row.update(fin=False, payload_b64=base64.b64encode(raw[:cut]).decode(), bytes=cut)
                rows.insert(rows.index(row) + 1, tail)
            elif name == 'non-shell-rechunk':
                next(r for r in rows if r['kind'] == 'http_request' and r['id'] == 5)['path'] = '/ws'
            elif name == 'auth-url-order':
                urls = [r for r in complete if body(r).get('type') == 'auth_url']
                a, b = rows.index(urls[0]), rows.index(urls[1])
                rows[a], rows[b] = rows[b], rows[a]
            elif name in ('clock-swap', 'both-clock-swap'):
                session['createdAt'], session['lastActivity'] = session['lastActivity'], session['createdAt']
            elif name == 'clock-cross-entity':
                other = value['projects'][0]['sessions'][1]
                for key in ('createdAt', 'created_at', 'lastActivity', 'updated_at'):
                    session[key], other[key] = other[key], session[key]
            elif name == 'clock-alias-equality':
                session['created_at'] = session['lastActivity']
            elif name == 'clock-missing':
                del session['createdAt']
            elif name == 'clock-null':
                session['createdAt'] = None
            elif name == 'clock-invalid':
                session['createdAt'] = 'not a clock'
            elif name == 'clock-unknown':
                session['timestamp'] = 'literal new field'
            elif name == 'changed-file':
                value['changedFile'] = 'literal.jsonl.projections.json'
            elif name == 'clock-type':
                session['createdAt'] = 1700086401000
            else:
                raise AssertionError(name)
            replace(watcher, value)

        cases = [('pty-clock-equivalence', None, None), ('pty-fragment-equivalence', None, None)]
        cases += [(name, 'ws.sequence', '.u2c.output_b64') for name in ('pty-byte', 'pty-drop', 'pty-extra', 'pty-order', 'pty-direction')]
        cases += [('pty-envelope', 'ws.structure', '.u2c.output')]
        cases += [('both-pty-envelope', 'ws.structure', '.u2c.output'),
                  ('pty-mask', 'evidence.integrity', 'right')]
        cases += [(name, 'ws.sequence', '.c2u.frames') for name in ('resize-value', 'resize-drop', 'resize-extra', 'resize-order')]
        cases += [('auth-url-order', 'ws.sequence', '.u2c.frames'), ('non-shell-rechunk', 'ws.sequence', 'ws.')]
        cases += [('fragment-control-order', 'ws.sequence', '.c2u.frames')]
        cases += [(name, 'ws.sequence', '.projects[0].sessions[0]') for name in (
            'clock-swap', 'clock-cross-entity', 'clock-alias-equality', 'clock-missing', 'clock-null', 'clock-type', 'clock-unknown')]
        cases += [('clock-invalid', 'normalization.field', '.createdAt'),
                  ('both-clock-swap', 'normalization.field', '.lastActivity'),
                  ('changed-file', 'ws.sequence', '.changedFile')]
        for name, rule, location in cases:
            test = root / name
            for label in ('left', 'right'):
                shutil.copytree(root / label, test / label)
            if name != 'pty-clock-equivalence':
                labels = ('left', 'right') if name.startswith('both-') or name == 'non-shell-rechunk' else ('right',)
                for label in labels:
                    path = test / label / 'recording.jsonl'
                    rows = read_jsonl(path)
                    mutate(rows, name)
                    write_log(path, rows)
            report = compare_sides(test / 'left', test / 'right', enumeration, matrix, manifest)
            expected = 'FAIL' if rule else 'PASS'
            hits = [d for d in report['differences'] if d['rule'] == rule and location in d['location']] if rule else []
            results.append({'name': name, 'level': 'end-to-end', 'expect': expected,
                            'actual': report['verdict'], 'expected_rule': rule, 'expected_location': location,
                            'both_sides': name.startswith('both-'), 'matched': hits[:3],
                            'passed': report['verdict'] == expected and (bool(hits) if rule else True),
                            'rules': report['rules']})
    return results
