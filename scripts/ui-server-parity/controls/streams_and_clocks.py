"""Round 3 end-to-end schema-1 mutations, through wire validation and dispatcher."""
import base64
import copy
from datetime import datetime, timezone
import json
import hashlib
from pathlib import Path
import shutil
import tempfile

from compare import compare_sides, read_jsonl
from snapshot import capture


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
            offset = 0 if label == 'left' else 1000
            def iso(ms):
                return datetime.fromtimestamp((1700000000000 + offset + ms) / 1000, timezone.utc).isoformat(timespec='milliseconds').replace('+00:00', 'Z')
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


def replace_response(rows, rid, body):
    from controls.run_all import response_body, hdr
    raw = json.dumps(body, separators=(',', ':')).encode()
    response_body(rows, rid, lambda _: raw)
    response = next(r for r in rows if r['kind'] == 'http_response' and r['id'] == rid)
    response['headers'] = [h for h in response['headers'] if h['name'].lower() not in ('etag', 'content-length')]
    tag = base64.b64encode(hashlib.sha1(raw).digest()).decode().rstrip('=')
    response['headers'] += [hdr('etag', f'W/"{len(raw):x}-{tag}"'), hdr('content-length', str(len(raw)))]


def mutate_clock(body, context, name):
    sessions = body[0]['sessions'] if context == 'readonly.projects' else body['sessions']
    session = sessions[0]
    if name == 'format':
        session['createdAt'] = session['createdAt'].replace('T', ' ').removesuffix('Z')
    elif name == 'unit':
        body[0]['lastActivity'] //= 1000
    elif name == 'swap':
        session['createdAt'], session['lastActivity'] = session['lastActivity'], session['createdAt']
    elif name == 'outside-wall':
        session['createdAt'] = '2023-11-13T22:13:21.000Z'
    elif name == 'cross-entity':
        sessions[0], sessions[1] = sessions[1], sessions[0]
    elif name == 'alias':
        session['created_at'] = session['lastActivity']
    else:
        raise AssertionError(name)


def run_http_clock_controls():
    from controls.run_all import fixture, write_log, b64
    results = []
    with tempfile.TemporaryDirectory(prefix='b2-http-clock-') as tmp:
        root = Path(tmp)
        enumeration, matrix, manifest = fixture(root)
        sessions = []
        for i in (1, 3):
            sessions.append({'createdAt': f'2023-11-14T22:13:2{i}.000Z',
                             'created_at': f'2023-11-14T22:13:2{i}.000Z',
                             'lastActivity': f'2023-11-14T22:13:2{i+1}.000Z',
                             'updated_at': f'2023-11-14T22:13:2{i+1}.000Z'})
        for label in ('left', 'right'):
            path = root / label / 'recording.jsonl'
            rows = read_jsonl(path)
            template = [copy.deepcopy(r) for r in rows if r.get('id') == 2]
            for rid, context, body in ((6, 'readonly.projects', [{'lastActivity': 1700000004000, 'sessions': sessions}]),
                                       (7, 'readonly.sessions', {'sessions': sessions})):
                extra = copy.deepcopy(template)
                for r in extra:
                    r['id'] = rid
                    if r['kind'] == 'http_request':
                        r.update(method='GET', path='/api/' + context.split('.')[1], case=context)
                        r['headers'] = [h for h in r['headers'] if h['name'] != 'x-pdrs-parity-case']
                        r['headers'].append({'name': 'x-pdrs-parity-case', 'value_b64': b64(context)})
                rows[-1:-1] = extra
                replace_response(rows, rid, body)
            write_log(path, rows)
        for context, rid in (('readonly.projects', 6), ('readonly.sessions', 7)):
            for name in ('equivalence', 'format', 'swap', 'outside-wall', 'cross-entity', 'alias', 'unit'):
                if name == 'unit' and rid == 7:
                    continue
                for both in ((False, True) if name in ('format', 'swap', 'unit', 'outside-wall') else (False,)):
                    test = root / f'{context}-{name}-{both}'
                    for label in ('left', 'right'):
                        shutil.copytree(root / label, test / label)
                        if name == 'equivalence' or label == 'left' and not both:
                            continue
                        path = test / label / 'recording.jsonl'
                        rows = read_jsonl(path)
                        raw = b''.join(base64.b64decode(r['body_b64']) for r in rows if r['kind'] == 'http_chunk' and r['id'] == rid and r['direction'] == 'response')
                        body = json.loads(raw)
                        mutate_clock(body, context, name)
                        replace_response(rows, rid, body)
                        write_log(path, rows)
                    report = compare_sides(test / 'left', test / 'right', enumeration, matrix, manifest)
                    expected = 'PASS' if name == 'equivalence' else 'FAIL'
                    rule = 'http.body' if name in ('cross-entity', 'alias') else 'normalization.field'
                    hits = [d for d in report['differences'] if d['rule'] == rule and context in d['location']]
                    results.append({'name': f'http-clock-{context}-{name}' + ('-both' if both else ''),
                                    'level': 'end-to-end', 'expect': expected, 'actual': report['verdict'],
                                    'both_sides': both, 'expected_rule': rule if expected == 'FAIL' else None,
                                    'passed': report['verdict'] == expected and (bool(hits) if expected == 'FAIL' else True),
                                    'matched': hits[:3], 'differences': report['differences'][:5]})
    return results


SESSION = 'web:33333333-3333-4333-8333-000000000002'
CHAT = f'home/projects/exp008-fixture/chats/{SESSION}.jsonl'
STAMPS = [f'2023-11-14T22:13:2{i}.000Z' for i in (1, 2, 3)]


def run_history_clock_controls():
    from controls.run_all import fixture, write_json, write_log, b64
    results = []
    with tempfile.TemporaryDirectory(prefix='b2-history-clock-') as tmp:
        root = Path(tmp)
        enumeration, matrix, manifest = fixture(root)
        events = [dict(sequence=i + 1, entryId=f'event-{i}', parentEntryId=f'event-{i-1}' if i else None,
                       turnId='turn', sessionId=SESSION, type=kind, createdAt=stamp)
                  for i, (stamp, kind) in enumerate(zip(STAMPS, ('accepted_input', 'agent_status_message', 'session_metadata')))]
        events[-1]['metadata'] = {'updatedAt': STAMPS[0]}
        anchor = {k: v for k, v in events[-1].items() if k not in ('metadata', 'sessionId')}
        projection = dict(sessionId=SESSION, asOfSequence=3, anchor=anchor, projections={
            'agent-transcript.conversation': {'asOfSequence': 3, 'state': {'entries': [{'entry': events[0]}]}},
            'web-history.agent-statuses': {'asOfSequence': 3, 'state': [events[1]]},
            'agent-transcript.metadata': {'asOfSequence': 3, 'state': events[-1]['metadata']}})
        messages = [dict(kind='text', id=f'm-{i}', sessionId=SESSION, timestamp=stamp) for i, stamp in enumerate(STAMPS)]
        for label in ('left', 'right'):
            side = root / label
            chat = side / CHAT
            chat.parent.mkdir(parents=True)
            chat.write_text(''.join(json.dumps(e) + '\n' for e in events))
            write_json(Path(str(chat) + '.projections.json'), projection)
            write_json(side / 'snapshot.json', capture(side))
            path = side / 'recording.jsonl'
            rows = read_jsonl(path)
            extra = [copy.deepcopy(r) for r in rows if r.get('id') == 2]
            for r in extra:
                r['id'] = 6
                if r['kind'] == 'http_request':
                    r.update(method='GET', path='/api/messages', case='readonly.messages')
                    r['headers'] = [h for h in r['headers'] if h['name'] != 'x-pdrs-parity-case']
                    r['headers'].append({'name': 'x-pdrs-parity-case', 'value_b64': b64('readonly.messages')})
            rows[-1:-1] = extra
            replace_response(rows, 6, {'messages': messages})
            write_log(path, rows)

        contracts = {
            'messages-reverse': 'history timestamps must be nondecreasing',
            'messages-read-time': 'history timestamp lacks local @chat',
            'chat-reverse': '@chat createdAt must be nondecreasing',
            'anchor': 'projection timestamp differs from local event identity',
            'conversation': 'projection timestamp differs from local event identity',
            'statuses': 'projection timestamp differs from local event identity',
            'metadata': 'projection metadata updatedAt differs from latest log metadata',
        }
        for name in ('equivalence', 'ties', 'incomplete', 'fake-incomplete', *contracts, 'clock-regression', 'clock-missing'):
            for both in ((False, True) if name in contracts or name in ('clock-regression', 'incomplete') else (False,)):
                test = root / f'{name}-{both}'
                for label in ('left', 'right'):
                    side = test / label
                    shutil.copytree(root / label, side)
                    if label == 'left' and not both:
                        continue
                    path = side / 'recording.jsonl'
                    rows = read_jsonl(path)
                    body = {'messages': copy.deepcopy(messages)}
                    if name == 'messages-reverse':
                        for message, stamp in zip(body['messages'], reversed(STAMPS)):
                            message['timestamp'] = stamp
                    elif name == 'messages-read-time':
                        for message in body['messages']:
                            message['timestamp'] = '2023-11-14T22:13:29.000Z'
                    elif name == 'ties':
                        body['messages'][1]['timestamp'] = STAMPS[0]
                    elif name in ('incomplete', 'fake-incomplete'):
                        injected = dict(kind='status', provider='pilotdeck', sessionId=SESSION,
                                        id=SESSION + '-incomplete-turn-status-turn',
                                        text='本轮记录尚未写入最终结果，已恢复当时产生的工具调用和输出。',
                                        timestamp='2023-11-14T22:13:29.000Z')
                        if name == 'fake-incomplete':
                            injected['entryId'] = 'event-1'
                        body['messages'].append(injected)
                    if name.startswith('messages-') or name in ('ties', 'incomplete', 'fake-incomplete'):
                        replace_response(rows, 6, body)
                        write_log(path, rows)
                    chat = side / CHAT
                    if name == 'chat-reverse':
                        changed = copy.deepcopy(events)
                        for e, stamp in zip(changed, reversed(STAMPS)):
                            e['createdAt'] = stamp
                        chat.write_text(''.join(json.dumps(e) + '\n' for e in changed))
                    if name in ('anchor', 'conversation', 'statuses', 'metadata'):
                        changed = copy.deepcopy(projection)
                        target = {'anchor': changed['anchor'],
                                  'conversation': changed['projections']['agent-transcript.conversation']['state']['entries'][0]['entry'],
                                  'statuses': changed['projections']['web-history.agent-statuses']['state'][0],
                                  'metadata': changed['projections']['agent-transcript.metadata']['state']}[name]
                        # Another *valid log time* must not satisfy an identity witness.
                        target['updatedAt' if name == 'metadata' else 'createdAt'] = STAMPS[1 if name != 'statuses' else 0]
                        write_json(Path(str(chat) + '.projections.json'), changed)
                    if name.startswith('clock-'):
                        meta = json.loads((side / 'run-meta.json').read_text())
                        if name == 'clock-missing':
                            del meta['clock_regression_check']
                        else:
                            # Keep claimed PASS, wall still increasing: offset regression
                            # must be recomputed from samples, not trusted from status.
                            meta['clock_regression_check']['samples'][1]['wall_ns'] -= 100_000_000
                        write_json(side / 'run-meta.json', meta)
                    write_json(side / 'snapshot.json', capture(side))
                report = compare_sides(test / 'left', test / 'right', enumeration, matrix, manifest)
                expect = 'REFUSED' if name.startswith('clock-') else 'PASS' if name in ('equivalence', 'ties') or name == 'incomplete' and both else 'FAIL'
                hits = [d for d in report['differences'] if d['rule'] == 'normalization.field'
                        and contracts.get(name, 'history timestamp lacks local @chat') in d['message']]
                if name == 'incomplete':
                    valid = (report['reading_clock_paths']['right'] == ['http[readonly.messages#0].body.messages[3].timestamp']
                             and not any(d['rule'] == 'normalization.field' for d in report['differences']))
                else:
                    valid = bool(hits) if expect == 'FAIL' else True
                results.append(dict(name='history-clock-' + name + ('-both' if both else ''), both_sides=both,
                                    expect=expect, actual=report['verdict'], expected_rule='normalization.field' if hits else None,
                                    passed=report['verdict'] == expect and valid,
                                    matched=hits[:4], differences=report['differences'],
                                    clock_regression_check=report['clock_regression_check']))
    return results
