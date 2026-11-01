#!/usr/bin/env python3
# anchor-selfcheck: unsupported data controls, no source mutation table
"""#1140 defensive authentication tests using labelled simulated originals only."""
import sys
if not sys.flags.isolated:
    raise SystemExit('run with python3 -I')
sys.dont_write_bytecode = True
import copy
import importlib.util
import hashlib
import io
import json
from contextlib import nullcontext, redirect_stdout
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[5]
p = ROOT / 'scripts/gateway-parity/controls/session_domain.py'
spec = importlib.util.spec_from_file_location('chain_controls', p)
c = importlib.util.module_from_spec(spec)
exec(compile(p.read_bytes(), str(p), 'exec'), c.__dict__)
s = c.session_domain_module
n = s.native_evidence


class ChainTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='pdrs-1140-chain-')
        self.addCleanup(self.temp.cleanup)
        self.folder = c.native_capture_fixture(Path(self.temp.name))
        self.healthy = s._load_native_artifacts(self.folder)
        self.assertTrue(c.control_deadline_report(self.folder)['cells'][-1]['status'] == 'PASS',
                        '#1140 r8 chain healthy labelled control')

    def read(self, name):
        return json.loads((self.folder / name).read_text())

    def write(self, name, value):
        c.save(self.folder / name, value)
        self.repin()

    def repin(self):
        receipt = self.read('capture/receipt.json')
        receipt.update({key: c.digest(self.folder / name) for name, key in c.NATIVE_RECEIPT_KEYS.items()})
        c.save(self.folder / 'capture/receipt.json', receipt)

    def reject(self, marker, rule=None):
        failure = None
        try:
            c.control_deadline_report(self.folder)
        except s.EvidenceError as error:
            failure = error
        except Exception as error:
            self.fail('#1140 r8 unrelated chain exception: ' + repr(error))
        self.assertTrue(failure is not None and (rule is None or failure.rule == rule), marker)

    def test_jsonl_unicode_separators(self):
        marker = '#1140 r9 JSONL splits only LF and retains Unicode text'
        source = self.read('session-domain/sources.json')[0]
        identity = self.healthy['rows'][0]['identity']
        home, _ = n.session_profile(self.healthy['contract'], self.healthy['expected'])
        entries = [{'type': 'agent_status_message', 'text': 'a' + char + 'b'}
                   for char in ('\u0085', '\u2028', '\u2029', '\x0b', '\x0c')]
        source['transcriptText'] = '\n'.join(json.dumps(entry, ensure_ascii=False) for entry in entries) + '\n'
        try:
            result = n.disk_observation(source, identity, home)
        except Exception as error:
            self.fail(marker + ': ' + repr(error))
        self.assertTrue(result['durableStatus']['entries'] == entries, marker)

    def test_jsonl_malformed_is_incomplete(self):
        marker = '#1140 r9 malformed JSONL is controlled Incomplete'
        source = self.read('session-domain/sources.json')[0]
        identity = self.healthy['rows'][0]['identity']
        home, _ = n.session_profile(self.healthy['contract'], self.healthy['expected'])
        source['transcriptText'] = '{broken}\n'
        error = None
        try:
            n.disk_observation(source, identity, home)
        except Exception as caught:
            error = caught
        self.assertTrue(isinstance(error, n.Incomplete) and 'transcript JSONL malformed' in str(error), marker)

    def test_registry_malformed_is_incomplete(self):
        marker = '#1140 r10 malformed registry is controlled Incomplete'
        source = self.read('session-domain/sources.json')[0]
        identity = self.healthy['rows'][0]['identity']
        home, _ = n.session_profile(self.healthy['contract'], self.healthy['expected'])
        source['registryText'] = '{broken}'
        error = None
        try:
            n.disk_observation(source, identity, home)
        except Exception as caught:
            error = caught
        self.assertTrue(isinstance(error, n.Incomplete) and 'registry JSON malformed' in str(error), marker)

    def test_observer_binding_shape_require(self):
        marker = '#1140 r10 observer binding shape directly rejected'
        for binding in (None, [], 'binding', 1):
            with self.subTest(binding=binding):
                error = None
                try:
                    s._observer_binding([{'frame': {'type': 'hello_ok', 'interactionBinding': binding}}])
                except Exception as caught:
                    error = caught
                self.assertTrue(isinstance(error, s.EvidenceError) and error.rule == 'BINDING'
                                and 'observer hello binding malformed' in str(error), marker)

    def test_jsonl_shared_line_boundaries(self):
        marker = '#1140 r10 JSONL retains internal blanks and only drops terminal LF'
        for text, expected in (('', []), ('{}', ['{}']), ('{}\n', ['{}']),
                               ('{}\n\n', ['{}', '']), ('\n', ['']),
                               ('{}\r\n', ['{}\r']), ('{}\x0b{}\n', ['{}\x0b{}'])):
            with self.subTest(text=repr(text)):
                self.assertTrue(n.jsonl_lines(text) == expected, marker)

    def test_jsonl_compare_disk_and_controls(self):
        marker = '#1140 r10 compare and controls preserve JSONL Unicode text'
        compare = s._load('compare', s.SCRIPT / 'compare.py')
        run_all_controls = s._load('run_all', s.SCRIPT / 'controls/run_all.py')
        entries = [{'type': 'agent_status_message', 'text': 'a' + char + 'b'}
                   for char in ('\u2028', '\u0085', '\x0b')]
        raw = ''.join(json.dumps(entry, ensure_ascii=False) + '\n' for entry in entries)
        folder = Path(self.temp.name) / 'jsonl-round'
        artifact = folder / 'home' / 'chats' / 'unicode.jsonl'
        artifact.parent.mkdir(parents=True)
        artifact.write_text(raw)
        frames = folder / 'scenarios' / 'auth' / 'frames.jsonl'
        frames.parent.mkdir(parents=True)
        frames.write_text(raw)
        try:
            normalizer = compare.entity_normalizer(folder, {})
            projected = compare.disk(folder, normalizer)
            content = next(row['content'] for row in projected if row['path'].endswith('unicode.jsonl'))
            self.assertTrue(content == compare.transcript_projection(entries, compare.Normalizer()), marker)
            self.assertTrue(run_all_controls.rows(folder) == entries, marker)
            permission = artifact.with_name('gateway-parity-permission.jsonl')
            permission_rows = [{'type': 'model_stream_event',
                                'event': {'requestFingerprint': 'original', 'text': entry['text']}}
                               for entry in entries]
            permission.write_text(''.join(json.dumps(row, ensure_ascii=False) + '\n' for row in permission_rows))
            run_all_controls.corrupt_fingerprint(folder)
            changed = [json.loads(line) for line in n.jsonl_lines(permission.read_text())]
            permission_rows[0]['event']['requestFingerprint'] = '0' * 64
            self.assertTrue(changed == permission_rows, marker)
            # The shared delimiter must not weaken the existing blank-record rejection.
            artifact.write_text(raw + '\n')
            with self.assertRaises(ValueError, msg=marker):
                compare.disk(folder, compare.Normalizer())
        except Exception as error:
            self.fail(marker + ': ' + repr(error))

    def test_jsonl_validate_frames_and_upstream(self):
        marker = '#1140 r10 recorder and upstream JSONL preserve Unicode text'
        compare = s._load('compare', s.SCRIPT / 'compare.py')
        folder = Path(self.temp.name) / 'jsonl-recordings'
        text = 'a\u2028b\u0085c\x0bd'
        row = {'seq': 1, 'connection': 1, 'direction': 'receive', 'transportEnd': True,
               'frame': {'type': 'hello_ok', 'text': text}}
        c.save(folder / 'run.json', {'status': 'DONE', 'side': 'rust', 'audit': {'allStopped': True},
                                    'scenarios': {'auth': {'exitCode': 0}}})
        c.save(folder / 'scenarios/auth/result.json', {'scenario': 'auth', 'status': 'PASS'})
        c.save(folder / 'scenarios/auth/complete.json', {'scenario': 'auth', 'complete': True,
                                                       'records': 1, 'connections': 1})
        (folder / 'scenarios/auth/frames.jsonl').write_text(json.dumps(row, ensure_ascii=False) + '\n')
        (folder / 'project').mkdir()
        (folder / 'project/note.txt').write_text('gateway parity fixture\n')
        (folder / 'project/written.txt').write_text('HTTP written fixture\n')
        c.save(folder / 'scenarios/http/http.json',
               [{'status': 200, 'body': '{}', 'chunks': [{'base64': 'e30=', 'elapsedMs': 0}]}] * 8)
        c.save(folder / 'scenarios/quirks/result.json', {'results': []})
        methods = compare.READ_METHODS + ['submit_turn', 'permission_decide', 'cron_create']
        upstream = [{'seq': index, 'frame': {'method': method, 'text': text}}
                    for index, method in enumerate(methods, 1)]
        (folder / 'upstream.jsonl').write_text(''.join(json.dumps(item, ensure_ascii=False) + '\n' for item in upstream))
        try:
            with patch.object(compare, 'SCENARIOS', ['auth']):
                frames, errors = compare.validate(folder)
            # Other independent oracles intentionally fail in this small recording fixture.
            self.assertTrue(frames.get('auth') == [row]
                            and not any(error['rule'] in ('G0.round', 'G2.recording', 'G2.observer')
                                        for error in errors), marker + ': ' + repr(errors))
        except Exception as error:
            self.fail(marker + ': ' + repr(error))

    def test_receipt_schema_is_shared(self):
        marker = '#1140 r10 receipt producers and consumers use one schema'
        # Sentinel addition exercises derivation, not today's literal key count.
        extra = {'capture/extra.json': 'extraCaptureSha256',
                 'session-domain/extra.json': 'extraSessionSha256'}
        with patch.dict(n.NATIVE_RECEIPT_KEYS, extra):
            self.assertTrue(c.NATIVE_RECEIPT_KEYS is n.NATIVE_RECEIPT_KEYS, marker)
            self.assertTrue(n.receipt_artifacts('capture').get('extra.json') == 'extraCaptureSha256'
                            and n.receipt_artifacts('session-domain').get('extra.json') == 'extraSessionSha256', marker)
            for scope, key in (('capture', 'extraCaptureSha256'), ('session-domain', 'extraSessionSha256')):
                path = self.folder / scope / 'extra.json'
                path.write_text('{}')
                receipt = self.read('capture/receipt.json')
                receipt[key] = c.digest(path)
                c.save(self.folder / 'capture/receipt.json', receipt)
            s._load_native_artifacts(self.folder)
            for scope in ('capture', 'session-domain'):
                path = self.folder / scope / 'extra.json'
                path.write_text('{"changed":true}')
                error = None
                try:
                    s._load_native_artifacts(self.folder)
                except Exception as caught:
                    error = caught
                self.assertTrue(isinstance(error, s.EvidenceError) and error.rule == 'NATIVE_CAPTURE'
                                and 'artifact' in str(error) and 'extra.json' in str(error), marker)
                path.write_text('{}')

    def test_hashes_cover_a2(self):
        marker = '#1140 r9 E1 hashes include canonical A2 dependencies'
        try:
            result = s.hashes()
            canonical = {f'scripts/gateway-parity/{name}': s.digest(s.SCRIPT / name)
                         for name in s.common.A2_SCRIPTS}
        except Exception as error:
            self.fail(marker + ': ' + repr(error))
        self.assertTrue(all(result.get(path) == value for path, value in canonical.items()), marker)

    def test_formal_rejects_synthetic(self):
        with self.assertRaises(s.EvidenceError, msg='#1140 r8 formal rejects synthetic'):
            s.load_native_capture(self.folder)

    def test_missing_binary_is_blocked(self):
        binary = self.healthy['contract']['binaries']['host']['path']
        sha = n.sha
        def absent(path):
            if str(path) == binary:
                raise FileNotFoundError(binary)
            return sha(path)
        error = None
        with patch.object(n, 'sha', side_effect=absent):
            try:
                s._load_native_artifacts(self.folder)
            except Exception as caught:
                error = caught
        self.assertTrue(isinstance(error, s.ApparatusVerificationError),
                        '#1140 r8 absent binary is controlled BLOCKED prerequisite')

    def test_failure_receipt(self):
        c.save(self.folder / 'capture/failure.json', {'status': 'INCOMPLETE'})
        self.reject('#1140 r8 failure plus receipt rejected', 'NATIVE_CAPTURE')

    def test_empty_trace(self):
        (self.folder / 'capture/trace.log').write_text('')
        self.repin()
        self.reject('#1140 r8 empty raw trace rejected', 'NATIVE_CAPTURE')

    def test_omitted_short_descendant(self):
        # Raw retains a complete short-lived process; hashes are re-pinned.
        value = self.read('capture/evidence.json')
        value['processes'] = [p for p in value['processes'] if p['pid'] != 102]
        value['execWitness']['events'] = [e for e in value['execWitness']['events'] if e['pid'] != 102]
        self.write('capture/evidence.json', value)
        self.reject('#1140 r8 formal replay catches omitted descendant', 'PROCESS_WITNESS')

    def test_omitted_network(self):
        value = self.read('capture/evidence.json')
        value['networkWitness']['calls'].pop()
        self.write('capture/evidence.json', value)
        self.reject('#1140 r8 formal replay catches omitted network', 'NETWORK_WITNESS')

    def test_failure_flags_not_promoted(self):
        value = self.read('capture/evidence.json')
        value['execWitness']['complete'] = False
        self.write('capture/evidence.json', value)
        self.reject('#1140 r8 incomplete flags never promoted', 'PROCESS_WITNESS')

    def test_provider_case_missing(self):
        marker = '#1140 r10 missing required provider request is a per-case FAIL'
        required = n.required_provider_cases(c.read(s.SCRIPT / 'fixtures/session-expectations.json'))
        case = sorted(required)[0]
        value = self.read('session-domain/provider.json')
        value['requests'] = [request for request in value['requests'] if request['case'] != case]
        self.write('session-domain/provider.json', value)
        cell = c.control_deadline_report(self.folder)['cells'][-1]
        failed = [row for row in cell['cases'] if row['status'] != 'PASS']
        self.assertTrue(failed == [{'case': case, 'status': 'FAIL', 'rule': 'EXPECTATION_MISMATCH',
                                    'surfaces': ['provider.requests']}]
                        and cell['executedCases'] == cell['expectedCases']
                        and len(cell['cases']) == 72, marker)

    def test_provider_optional_case_missing(self):
        marker = '#1140 r10 non-model expectations do not require provider requests'
        required = n.required_provider_cases(c.read(s.SCRIPT / 'fixtures/session-expectations.json'))
        value = self.read('session-domain/provider.json')
        value['requests'] = [request for request in value['requests'] if request['case'] in required]
        self.write('session-domain/provider.json', value)
        try:
            cell = c.control_deadline_report(self.folder)['cells'][-1]
        except Exception as error:
            self.fail(marker + ': ' + repr(error))
        self.assertTrue(cell['status'] == 'PASS' and len(cell['cases']) == 72, marker)

    def test_provider_requirement_derived_from_expectations(self):
        marker = '#1140 r10 provider requirement follows expectation frame changes'
        document = c.read(s.SCRIPT / 'fixtures/session-expectations.json')
        expected = {cell + '/' + order + '/' + phase
                    for cell in ('no-registry-fast', 'registry-fast')
                    for order in ('unknown-first', 'known-final-first') for phase in ('model', 'tool')}
        self.assertTrue(n.required_provider_cases(document) == expected, marker)
        self.assertTrue(n.required_provider_cases({'cases': []}) == set(), marker)
        # Synthetic expectation inputs exercise the pure function, not Rust results
        # or timeout labels; an arbitrary renamed case must track its frames.
        for output in ('turn_completed:model_error', 'turn_completed:completed', 'assistant_text_delta:',
                       'assistant_thinking_delta:', 'assistant_block:', 'model_stream_event:',
                       'tool_calls_detected:', 'tool_call_started:', 'tool_call_completed:'):
            with self.subTest(output=output):
                sample = {'cases': [{'case': 'renamed-case', 'rust': {'frames': [output]}}]}
                self.assertTrue(n.required_provider_cases(sample) == {'renamed-case'}, marker)
                sample['cases'][0]['rust']['frames'] = ['agent_status:turn_timeout', 'error:agent_invalid_state']
                self.assertTrue(n.required_provider_cases(sample) == set(), marker)

    def test_no_provider_traffic_is_scored(self):
        marker = '#1140 r10 zero model traffic reaches per-case scoring'
        value = self.read('session-domain/provider.json')
        value['requests'] = []
        self.write('session-domain/provider.json', value)
        raw = (self.folder / 'capture/trace.log').read_text()
        self.replace_trace(''.join(line for line in raw.splitlines(True) if ' connect(' not in line))
        try:
            capture = s._load_native_artifacts(self.folder)
            collector = s._load('no_traffic_collector', ROOT / 'scripts/native-witness/collect.py')
            collector.validate_session(capture['contract'], capture['evidence'], capture['rows'], [], capture['expected'])
            cell = c.control_deadline_report(self.folder)['cells'][-1]
        except Exception as error:
            self.fail(marker + ': ' + repr(error))
        required = n.required_provider_cases(c.read(s.SCRIPT / 'fixtures/session-expectations.json'))
        self.assertTrue({row['case'] for row in cell['cases'] if row['status'] == 'FAIL'} == required
                        and len(cell['cases']) == 72 and cell['executedCases'] == cell['expectedCases']
                        and all(row['surfaces'] == ['provider.requests']
                                for row in cell['cases'] if row['case'] in required), marker)

    def test_provider_marker_wrong_field(self):
        value = self.read('session-domain/provider.json')
        row = value['requests'][0]
        row['raw'] = json.dumps({'model': 'mock', 'stream': True, 'messages': [], 'unrelated': f"[native-witness:{row['case']}] controlled wait"})
        self.write('session-domain/provider.json', value)
        self.reject('#1140 r8 provider marker must be user message', 'NATIVE_CHAIN')

    def test_provider_title_not_model(self):
        marker = '#1140 r8 title cannot cover model case'
        required = n.required_provider_cases(c.read(s.SCRIPT / 'fixtures/session-expectations.json'))
        case = sorted(required)[0]
        value = self.read('session-domain/provider.json')
        request = next(row for row in value['requests'] if row['case'] == case)
        body = json.loads(request['raw'])
        body['stream'] = False
        request['raw'] = json.dumps(body)
        self.write('session-domain/provider.json', value)
        cell = c.control_deadline_report(self.folder)['cells'][-1]
        self.assertTrue([row['case'] for row in cell['cases'] if row['status'] == 'FAIL'] == [case]
                        and next(row for row in cell['cases'] if row['case'] == case)['surfaces']
                        == ['provider.requests'], marker)

    def test_hello_only(self):
        value = self.read('session-domain/observer.json')
        value['rows'] = value['rows'][:1]
        self.write('session-domain/observer.json', value)
        self.reject('#1140 r8 hello alone cannot cover cases', 'NATIVE_CHAIN')

    def test_cross_case_frames(self):
        value = self.read('session-domain/cases.json')
        value['cases'][0]['observation']['frames'] = [{'type': 'text', 'text': 'other case'}]
        self.write('session-domain/cases.json', value)
        self.reject('#1140 r8 swapped frames disagree with observer', 'NATIVE_CHAIN')

    def test_cross_project_registry(self):
        sources = self.read('session-domain/sources.json')
        target = next(source for source in sources if json.loads(source['registryText'])['records'])
        document = json.loads(target['registryText'])
        document['records'][0]['projectKey'] += '-other'
        target['registryText'] = json.dumps(document)
        index = sources.index(target)
        identity = self.healthy['rows'][index]['identity']
        home, _ = n.session_profile(self.healthy['contract'], self.healthy['expected'])
        self.assertTrue(n.disk_observation(target, identity, home)['registry']['record'] == {},
                        '#1140 r8 registry uses full project session run key')
        self.write('session-domain/sources.json', sources)
        self.reject('#1140 r8 registry uses full project session run key', 'NATIVE_CHAIN')

    def test_duplicate_registry(self):
        sources = self.read('session-domain/sources.json')
        target = next(source for source in sources if json.loads(source['registryText'])['records'])
        document = json.loads(target['registryText'])
        document['records'].append(copy.deepcopy(document['records'][0]))
        target['registryText'] = json.dumps(document)
        self.write('session-domain/sources.json', sources)
        self.reject('#1140 r8 duplicate full run key rejected', 'NATIVE_CHAIN')

    def test_inapplicable_records(self):
        value = self.read('session-domain/cases.json')
        value['cases'][0]['observation']['registry'] = {'applicable': False, 'events': [], 'record': {'state': 'wrong'}}
        self.write('session-domain/cases.json', value)
        self.assertTrue(s._native_observation_structure(value['cases'][0]['observation']) is not None,
                        '#1140 r8 inapplicable shape rejects record')
        self.reject('#1140 r8 inapplicable record rejected', 'OBSERVATION_STRUCTURE')

    def test_old_case_identity(self):
        value = self.read('session-domain/cases.json')
        value['cases'][0]['identity']['runId'] = 'previous-capture-run'
        self.write('session-domain/cases.json', value)
        self.reject('#1140 r8 stale case identity rejected', 'NATIVE_CHAIN')

    def test_cross_project_transcript(self):
        sources = self.read('session-domain/sources.json')
        sources[0]['projectMarkerText'] += '-other'
        self.write('session-domain/sources.json', sources)
        self.reject('#1140 r8 transcript must belong to requested project', 'NATIVE_CHAIN')

    def test_decoy_marker(self):
        raw = (self.folder / 'capture/trace.log').read_text()
        path = self.healthy['evidence']['transcriptPaths'][0]
        hx = lambda text: '"' + ''.join(f'\\x{b:02x}' for b in text.encode()) + '"'
        raw = raw.replace(hx(path), hx('/decoy/chats/' + Path(path).name))
        self.replace_trace(raw)
        self.reject('#1140 r8 decoy marker cannot attribute sidecar', 'NATIVE_PROCESS')

    def replace_trace(self, raw):
        (self.folder / 'capture/trace.log').write_text(raw)
        launch = self.read('capture/launch.json')
        c.save(self.folder / 'capture/evidence.json', n.parse_trace(raw, launch))
        self.repin()

    def test_failed_marker(self):
        raw = (self.folder / 'capture/trace.log').read_text().replace('0644) = 5', '0644) = -1 ENOENT (No such file)', 1)
        self.replace_trace(raw)
        self.reject('#1140 r8 failed open cannot attribute sidecar', 'NATIVE_PROCESS')

    def test_balanced_decoy(self):
        raw = (self.folder / 'capture/trace.log').read_text()
        lines = raw.splitlines(True)
        first, second = [i for i, line in enumerate(lines) if 'openat(' in line][:2]
        # First real transcript opens only after both sidecars; a decoy at the
        # old boundary cannot make the empty/two-child allocation look sound.
        marker = lines.pop(first)
        lines.insert(second + 3, marker)
        lines.insert(first, marker.replace('\\x2f\\x68\\x6f\\x6d\\x65', '\\x2f\\x64\\x65\\x63\\x6f\\x79'))
        # The prefix is machine-dependent; explicitly replace the first path.
        path = self.healthy['evidence']['transcriptPaths'][0]
        hx = lambda text: '"' + ''.join(f'\\x{b:02x}' for b in text.encode()) + '"'
        lines[first] = marker.replace(hx(path), hx('/decoy/chats/' + Path(path).name))
        self.replace_trace(''.join(lines))
        self.reject('#1140 r8 balanced allocation plus decoy rejected', 'NATIVE_PROCESS')

    def test_profile_helper(self):
        for name in ('capture/prelaunch.json', 'capture/launch.json'):
            contract = self.read(name)
            contract['binaries']['helper'] = dict(contract['binaries']['sidecar'], argv=[sys.executable, 'helper'])
            contract['processRoles'].append('helper')
            self.write(name, contract)
        launch = self.read('capture/launch.json')
        helper = launch['binaries']['helper']
        hx = lambda text: '"' + ''.join(f'\\x{b:02x}' for b in text.encode()) + '"'
        row = '101 clone(flags=SIGCHLD) = 999\n999 execve(' + hx(helper['path']) + ', [' + ', '.join(hx(arg) for arg in helper['argv']) + '], 0x0) = 0\n999 +++ exited with 0 +++\n'
        raw = (self.folder / 'capture/trace.log').read_text().replace('101 +++ exited with 0 +++', row + '101 +++ exited with 0 +++')
        self.replace_trace(raw)
        with self.assertRaises(n.Incomplete, msg='#1140 r8 contract cannot expand frozen roles'):
            n.session_profile(launch, self.healthy['expected'])
        self.reject('#1140 r8 contract cannot expand frozen roles', 'NATIVE_CHAIN')

    def test_profile_endpoint(self):
        for name in ('capture/prelaunch.json', 'capture/launch.json'):
            contract = self.read(name)
            contract['externalEndpoints'].append('127.0.0.1:29999')
            self.write(name, contract)
        raw = (self.folder / 'capture/trace.log').read_text()
        call = next(line for line in raw.splitlines(True) if 'connect(' in line)
        self.replace_trace(raw.replace(call, call + call.replace('htons(29000)', 'htons(29999)')))
        with self.assertRaises(n.Incomplete, msg='#1140 r8 contract cannot expand provider endpoints'):
            n.session_profile(self.read('capture/launch.json'), self.healthy['expected'])
        self.reject('#1140 r8 contract cannot expand provider endpoints', 'NATIVE_CHAIN')

    def test_profile_connections(self):
        for name in ('capture/prelaunch.json', 'capture/launch.json'):
            contract = self.read(name)
            contract['connections'] = 2
            self.write(name, contract)
        with self.assertRaises(n.Incomplete, msg='#1140 r8 frozen profile fixes connection count'):
            n.session_profile(self.read('capture/launch.json'), self.healthy['expected'])
        self.reject('#1140 r8 frozen profile fixes connection count', 'NATIVE_CHAIN')

    def test_observer_wrong_request(self):
        value = self.read('session-domain/observer.json')
        index = next(i for i, row in enumerate(value['rows']) if row['direction'] == 'receive' and row['frame'].get('id'))
        frame = dict(value['rows'][index]['frame'], id='case-02')
        value['rows'][index] = c.control_wire(frame)
        self.write('session-domain/observer.json', value)
        self.reject('#1140 r8 observer event requires active request', 'NATIVE_CHAIN')

    def test_synchronized_old_identity(self):
        for name, key in (('session-domain/cases.json', 'cases'), ('session-domain/driver.json', 'identities')):
            value = self.read(name)
            identity = value[key][0]['identity'] if key == 'cases' else value[key][0]
            identity['runId'] = 'old-run'
            self.write(name, value)
        self.reject('#1140 r8 driver case cannot drift from submitted run', 'NATIVE_CHAIN')

    def test_provider_multiple_case_owners(self):
        value = self.read('session-domain/provider.json')
        body = json.loads(value['requests'][0]['raw'])
        body['messages'] = [{'role': 'user', 'content': f'[native-witness:{case}] controlled wait'}
                            for case in self.healthy['expected']]
        for row in value['requests']:
            row['raw'] = json.dumps(body)
        self.write('session-domain/provider.json', value)
        self.reject('#1140 r8 provider body cannot belong to multiple cases', 'NATIVE_CHAIN')

    def test_null_run_identity(self):
        rows = self.read('session-domain/cases.json')
        index = next(i for i, row in enumerate(rows['cases']) if not row['observation']['registry']['applicable'])
        request = rows['cases'][index]['identity']['requestId']
        rows['cases'][index]['identity']['runId'] = None
        self.write('session-domain/cases.json', rows)
        driver = self.read('session-domain/driver.json')
        driver['identities'][index]['runId'] = None
        self.write('session-domain/driver.json', driver)
        observer = self.read('session-domain/observer.json')
        for i, row in enumerate(observer['rows']):
            if row['direction'] == 'send':
                frame = json.loads(row['frame'])
                if frame.get('id') == request:
                    frame['params']['runId'] = None
                    observer['rows'][i] = c.control_wire(frame, 'send')
        self.write('session-domain/observer.json', observer)
        self.reject('#1140 r8 null run is not an execution identity', 'NATIVE_CHAIN')

    def test_final_envelope(self):
        observer = self.read('session-domain/observer.json')
        for i, row in enumerate(observer['rows']):
            if row['direction'] == 'receive' and row['frame'].get('final') is True:
                observer['rows'][i] = c.control_wire(dict(row['frame'], type='unrelated_notification'))
        self.write('session-domain/observer.json', observer)
        self.reject('#1140 r8 final must have event envelope', 'NATIVE_CHAIN')

    def test_event_sequence(self):
        observer = self.read('session-domain/observer.json')
        i = next(i for i, row in enumerate(observer['rows']) if row['direction'] == 'receive' and row['frame'].get('id'))
        observer['rows'][i] = c.control_wire(dict(observer['rows'][i]['frame'], seq=99))
        self.write('session-domain/observer.json', observer)
        self.reject('#1140 r8 event sequence must be contiguous', 'NATIVE_CHAIN')

    def test_formal_ws_profile(self):
        value = self.read('session-domain/observer.json')
        row = value['rows'][0]
        wire = bytearray.fromhex(row['wireHex'])
        wire[0] &= 127
        row['wireHex'], row['wire']['fin'] = wire.hex(), False
        self.write('session-domain/observer.json', value)
        self.reject('#1140 r8 formal loader rejects consistent fragmented hello', 'OBSERVER_WIRE')

    def frozen_input_change(self, filename):
        # A small data-only hash fixture; real digest validation runs here.
        # Other independent checker stages have their own tests and are stubbed
        # rather than making a second checkout or changing the production manifest.
        checker = s._load('chain_frozen_checker', ROOT / 'scripts/gateway-parity/check_frozen.py')
        fixture = Path(self.temp.name).resolve() / 'hash-fixture'
        folder = fixture / 'specs/gateway-parity'
        folder.mkdir(parents=True)
        entries = []
        for name in ('acceptance.md', 'normalization.md', 'accepted-diffs.md', filename):
            path = folder / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text('registered input')
            entries.append(hashlib.sha256(path.read_bytes()).hexdigest() + ' ' + name)
        (folder / 'FROZEN.md').write_text('\n'.join(entries) + '\n')
        def check():
            output = io.StringIO()
            with patch.object(checker, 'ROOT', fixture), patch.object(checker, 'SCRIPT', fixture / 'scripts'), \
                    patch.object(checker, 'A2_SCRIPTS', ()), \
                    patch.object(checker, 'unknown_apparatus_files', return_value=[]), \
                    patch.object(checker, 'check_diffs', return_value=[]), \
                    patch.object(checker, 'check_manifest', return_value=[]), \
                    patch.object(checker, 'check_registration', return_value=[]), redirect_stdout(output):
                code = checker.main([])
            return {'exitCode': code, 'output': output.getvalue()}
        self.assertTrue(check()['exitCode'] == 0, '#1140 r8 registered data hash positive')
        (folder / filename).write_text('unregistered change')
        changed = check()
        self.assertTrue(changed['exitCode'] != 0 and filename in changed['output'],
                        '#1140 r8 data hash change detected')
        output = Path(self.temp.name) / 'input-blocked'
        with patch.object(s, 'install_shutdown'), patch.object(s, 'parity_lock', return_value=nullcontext()), \
                patch.object(s, 'hashes', return_value={}), patch.object(s, 'frozen_check', side_effect=check), \
                patch.object(s, 'deadline_report', return_value={'status': 'PASS', 'cells': [{'status': 'PASS'}]}) as score, \
                patch.object(sys, 'argv', ['session', '--suite', 'deadline', '--output', str(output)]), redirect_stdout(io.StringIO()):
            code = s.main()
        report = json.loads((output / 'report.json').read_text())
        self.assertTrue(code == 2 and report['status'] == 'BLOCKED' and 'cells' not in report and not score.called,
                        '#1140 r8 unregistered ' + filename + ' blocks deadline')

    def test_unregistered_manifest_blocks_deadline(self):
        self.frozen_input_change('fixtures/session-calibration.json')

    def test_unregistered_expectation_blocks_deadline(self):
        self.frozen_input_change('fixtures/session-expectations.json')

    def test_deadline_frozen_failure(self):
        output = Path(self.temp.name) / 'blocked'
        with patch.object(s, 'install_shutdown'), patch.object(s, 'parity_lock', return_value=nullcontext()), \
                patch.object(s, 'hashes', return_value={}), patch.object(s, 'frozen_check', return_value={'exitCode': 1}), \
                patch.object(s, 'deadline_report', return_value={'status': 'PASS', 'cells': [{'status': 'PASS'}]}) as score, \
                patch.object(sys, 'argv', ['session', '--suite', 'deadline', '--output', str(output)]), redirect_stdout(io.StringIO()):
            code = s.main()
        report = json.loads((output / 'report.json').read_text())
        self.assertTrue(code == 2 and report['status'] == 'BLOCKED' and 'cells' not in report and not score.called,
                        '#1140 r8 frozen failure blocks before scoring')

    def test_final_frozen_failure(self):
        output = Path(self.temp.name) / 'changed'
        with patch.object(s, 'install_shutdown'), patch.object(s, 'parity_lock', return_value=nullcontext()), \
                patch.object(s, 'hashes', return_value={}), patch.object(s, 'frozen_check', side_effect=[{'exitCode': 0}, {'exitCode': 1}]), \
                patch.object(s, 'deadline_report', return_value={'status': 'PASS', 'cells': [{'status': 'PASS'}]}), \
                patch.object(sys, 'argv', ['session', '--suite', 'deadline', '--output', str(output)]), redirect_stdout(io.StringIO()):
            code = s.main()
        report = json.loads((output / 'report.json').read_text())
        self.assertTrue(code != 0 and report['status'] != 'PASS' and 'cells' not in report,
                        '#1140 r8 final frozen failure revokes cells')


if __name__ == '__main__':
    unittest.main()
