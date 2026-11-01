#!/usr/bin/env python3
# anchor-selfcheck: unsupported data controls, no source mutation table
"""#1140 fix-r11: deterministic durable waiting and adversarial input contracts."""
import sys
if not sys.flags.isolated:
    raise SystemExit('run with python3 -I')
sys.dont_write_bytecode = True
import ast
import contextlib
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import threading
import time
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[5]

def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    exec(compile(path.read_bytes(), str(path), 'exec'), module.__dict__)
    return module

c = load('r11_collector', ROOT / 'scripts/native-witness/collect.py')
s = load('r11_session', ROOT / 'scripts/gateway-parity/session_domain.py')
n = s.native_evidence
w = c.w
mock_gateway = load('r11_gateway', Path(__file__).parent.parent / 'native-session-e2e/mock_gateway.py')


class R11Tests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='pdrs-1140-r11-')
        self.addCleanup(self.temp.cleanup)
        self.home = Path(self.temp.name).resolve()
        self.project, self.session, self.run = '/project', 'session', 'run'
        self.chat = self.home / 'projects/mock/chats/session.jsonl'
        self.chat.parent.mkdir(parents=True)
        (self.chat.parent.parent / '.cwd').write_text(self.project)
        self.clock = 0.0
        self.sleeps = []
        self.write('completed')

    def write(self, state, text='complete'):
        self.chat.write_text(json.dumps({'type': 'agent_status_message', 'event': text}) + '\n')
        document = {'records': [{'projectKey': self.project, 'sessionKey': self.session,
                                'runId': self.run, 'state': state}], 'events': {}}
        temporary = self.home / 'runs.json.partial'
        temporary.write_text(json.dumps(document))
        temporary.replace(self.home / 'runs.json')

    def sleep(self, duration):
        self.clock += duration
        self.sleeps.append(duration)

    def wait(self, on_sleep=None, deadline=10, received=(), expectation=None):
        with patch.object(c.time, 'monotonic', side_effect=lambda: self.clock), \
                patch.object(c.time, 'sleep', side_effect=on_sleep or self.sleep):
            return c.wait_observation(self.home, self.project, self.session, self.run, received, 'request', deadline,
                                      expectation=expectation)

    def test_settle_three_samples(self):
        marker = '#1140 r11 terminal files require three stable samples'
        try:
            result = self.wait()
        except Exception as error:
            self.fail(marker + ': ' + repr(error))
        self.assertTrue(result[0] is not None and self.sleeps == [0.05, 0.05], marker)

    def test_settle_resets_on_write(self):
        marker = '#1140 r11 changed files reset the stable sample count'
        def tick(duration):
            self.sleep(duration)
            if len(self.sleeps) == 2:
                self.write('completed', 'later append')
        try:
            result = self.wait(tick)
        except Exception as error:
            self.fail(marker + ': ' + repr(error))
        self.assertTrue(len(self.sleeps) == 4 and result[0]['durableStatus']['entries'][0]['event'] == 'later append', marker)

    def test_settle_timeout_not_run(self):
        marker = '#1140 r11 unsettled run is NOT_RUN with durable reason'
        self.write('running')
        try:
            observation, identity, source = self.wait(deadline=0.11)
            case = c.session_manifest()[0]
            verdict = s.score_native_cases([{'case': case, 'observation': observation,
                                             'reason': source.get('collectionError')}], c.session_manifest(), [])[0]
        except Exception as error:
            self.fail(marker + ': ' + repr(error))
        self.assertTrue(observation is None and identity['runId'] == self.run
                        and verdict['status'] == 'NOT_RUN' and verdict['detail'] == '落盘未完成'
                        and self.clock == 0.11, marker)

    def test_settle_missing_and_cancelled(self):
        marker = '#1140 r11 missing files block and cancellation propagates'
        self.chat.unlink()
        caught = None
        try:
            self.wait(deadline=0.06)
        except (Exception, KeyboardInterrupt) as error:
            caught = error
        self.assertTrue(isinstance(caught, c.Blocked) and '落盘未完成' in str(caught), marker)
        cancelled = None
        self.write('completed')
        original_read = Path.read_text
        def interrupted_read(path, *args, **kwargs):
            if path == self.chat:
                raise KeyboardInterrupt('controlled transcript read cancellation')
            return original_read(path, *args, **kwargs)
        try:
            with patch.object(Path, 'read_text', interrupted_read):
                self.wait(deadline=0.11)
        except (Exception, KeyboardInterrupt) as error:
            cancelled = error
        self.assertTrue(isinstance(cancelled, KeyboardInterrupt), marker)

    def test_settle_registry_inapplicable(self):
        marker = '#1140 r12 inapplicable registry settles terminal transcript promptly'
        try:
            expectations = c.session_expectations()
            inapplicable = [row for row in expectations.values() if row['registry']['applicable'] is False]
            self.assertTrue(len(inapplicable) == 18 and any(not row['durableStatus'] for row in inapplicable), marker)
            for expected in inapplicable:
                self.clock, self.sleeps = 0.0, []
                self.chat.write_text(''.join(json.dumps({'type': 'agent_status_message', 'event': label}) + '\n'
                                             for label in expected['durableStatus']))
                (self.home / 'runs.json').write_text('{"records": [], "events": {}}')
                final = [{'id': 'request', 'final': True}]
                observed, _, _ = self.wait(received=final, expectation=expected)
                self.assertTrue(observed is not None and observed['registry']['applicable'] is False
                                and self.sleeps == [0.05, 0.05], marker)
        except Exception as error:
            self.fail(marker + ': ' + repr(error))

    def test_settle_registry_expected_not_observed(self):
        marker = '#1140 r12 missing required registry is not an inapplicable case'
        try:
            self.write('completed', 'turn_timeout')
            (self.home / 'runs.json').write_text('{"records": [], "events": {}}')
            expected = {'registry': {'applicable': True}, 'durableStatus': ['turn_timeout']}
            observed, _, source = self.wait(deadline=0.11, received=[{'id': 'request', 'final': True}],
                                             expectation=expected)
            self.assertTrue(observed is None and source['collectionError'] == '落盘未完成'
                            and self.clock == 0.11, marker)
        except Exception as error:
            self.fail(marker + ': ' + repr(error))

    def test_settle_inapplicable_transcript_pending(self):
        marker = '#1140 r12 inapplicable registry still waits for transcript finality'
        expected = {'registry': {'applicable': False}, 'durableStatus': ['turn_timeout']}
        try:
            for frames in ([], [{'id': 'request', 'final': False}]):
                self.clock, self.sleeps = 0.0, []
                self.write('completed', 'turn_timeout')
                observed, _, _ = self.wait(deadline=0.11, received=frames, expectation=expected)
                self.assertTrue(observed is None, marker)
            self.clock, self.sleeps = 0.0, []
            self.chat.write_text('')
            (self.home / 'runs.json').write_text('{"records": [], "events": {}}')
            def tick(duration):
                self.sleep(duration)
                if len(self.sleeps) == 3:
                    # A different terminal status is a product scoring difference,
                    # not an excuse to report this readable case as NOT_RUN.
                    self.chat.write_text(json.dumps({'type': 'agent_status_message', 'event': 'turn_failed'}) + '\n')
            observed, _, _ = self.wait(tick, received=[{'id': 'request', 'final': True}], expectation=expected)
            self.assertTrue(observed is not None and len(self.sleeps) == 5
                            and observed['durableStatus']['entries'][0]['event'] == 'turn_failed', marker)
        except Exception as error:
            self.fail(marker + ': ' + repr(error))

    def test_settle_missing_registry_file(self):
        marker = '#1140 r12 absent registry file affects only its case'
        try:
            for applicable in (False, True):
                self.clock, self.sleeps = 0.0, []
                self.write('completed', 'turn_timeout')
                (self.home / 'runs.json').unlink()
                expected = {'registry': {'applicable': applicable}, 'durableStatus': ['turn_timeout']}
                observed, _, source = self.wait(deadline=0.11, received=[{'id': 'request', 'final': True}],
                                                 expectation=expected)
                self.assertTrue((observed is None) == applicable and source['registryText'] is None
                                and self.clock == (0.11 if applicable else 0.1), marker)
            # A later case can still collect after the required case timed out.
            self.clock, self.sleeps = 0.0, []
            self.write('completed')
            self.assertTrue(self.wait()[0] is not None, marker)
        except Exception as error:
            self.fail(marker + ': ' + repr(error))

    def test_settle_inapplicable_completed_transcript(self):
        marker = '#1140 r12 completed transcript differences remain scoreable with Unicode JSONL'
        try:
            self.chat.write_text(json.dumps({'type': 'turn_result', 'text': 'complete\u2028result'}, ensure_ascii=False) + '\n')
            (self.home / 'runs.json').unlink()
            expected = {'registry': {'applicable': False}, 'durableStatus': ['turn_timeout']}
            observed, _, _ = self.wait(received=[{'id': 'request', 'final': True}], expectation=expected)
            self.assertTrue(observed is not None and not observed['durableStatus']['entries']
                            and self.sleeps == [0.05, 0.05], marker)
        except Exception as error:
            self.fail(marker + ': ' + repr(error))

    def test_settle_missing_signature_transitions(self):
        marker = '#1140 r12 file absence is a distinct signature and other stat errors propagate'
        try:
            registry = self.home / 'runs.json'
            present = c.file_signatures((self.chat, registry))
            registry.unlink()
            absent = c.file_signatures((self.chat, registry))
            registry.write_text('{"records": [], "events": {}}')
            restored = c.file_signatures((self.chat, registry))
            self.assertTrue(absent[1] is None and present[1] != absent[1] and restored[1] != absent[1], marker)
            with patch.object(Path, 'stat', side_effect=PermissionError('controlled stat permission')):
                with self.assertRaises(PermissionError, msg=marker):
                    c.file_signatures((registry,))
        except Exception as error:
            self.fail(marker + ': ' + repr(error))

    def test_collector_inapplicable_without_registry_file(self):
        marker = '#1140 r12 first registry-free cases collect without blocking the capture'
        try:
            e2e = load('r12_registry_e2e', Path(__file__).parent.parent / 'native-session-e2e/test_e2e.py')
            fixture = e2e.E2ETests
            fixture.host, fixture.sidecar = self.home / 'host', self.home / 'sidecar'
            fixture.host.write_bytes(b'\x7fELF controlled host')
            fixture.sidecar.write_bytes(b'\x7fELF controlled sidecar')
            expected = c.session_expectations()
            selected = c.session_manifest()[:2]
            expected = {case: expected[case] for case in selected}
            self.assertTrue(all(row['registry']['applicable'] is False for row in expected.values()), marker)
            replace = Path.replace
            def without_registry(path, target):
                if Path(target).name == 'runs.json':
                    path.unlink()  # Model the registry's first lazy creation, not a corrupt file.
                    return Path(target)
                return replace(path, target)
            output = self.home / 'collector-no-registry'
            with patch.object(e2e.c, 'session_manifest', return_value=selected), \
                    patch.object(e2e, 'load_expectations', return_value=expected), \
                    patch.object(Path, 'replace', without_registry), \
                    patch.object(e2e.c, 'SESSION_SETTLE_LIMIT_SECONDS', 0.5):
                fixture._collect(output, real_settle=True)
            rows = json.loads((output / 'session-domain/cases.json').read_text())['cases']
            receipt = json.loads((output / 'capture/receipt.json').read_text())
            self.assertTrue(len(rows) == 2 and all(row['observation'] is not None for row in rows)
                            and not (output / 'home/runs.json').exists()
                            and receipt['status'] == 'WITNESS_CAPTURED', marker)
        except Exception as error:
            self.fail(marker + ': ' + repr(error))

    def test_settle_mock_final_before_disk(self):
        marker = '#1140 r11 post-final mock waits for complete durable files'
        case = c.session_manifest()[0]
        release, committed = threading.Event(), threading.Event()
        def after_final():
            if not release.wait(3):
                raise RuntimeError(marker + ': release deadline')
        expectation = {'frames': [], 'durableStatus': ['done'],
                       'registry': {'applicable': True, 'state': 'completed', 'events': [], 'result': None}}
        # Port 0 is a test-only socket; production retains its fixed port contract.
        gateway = mock_gateway.Gateway(0, self.home, 'http://unused', {case: expectation}, after_final=after_final)
        client = None
        try:
            with patch.object(mock_gateway.urllib.request, 'urlopen'):
                client = c.WireClient(gateway.listener.getsockname()[1], time.monotonic() + 5)
                client.send({'type': 'hello'})
                self.assertTrue(client.receive()['type'] == 'hello_ok', marker)
                client.send({'type': 'request', 'id': 'request', 'params': {
                    'projectKey': self.project, 'sessionKey': self.session, 'runId': self.run,
                    'message': f'[native-witness:{case}] controlled wait'}})
                final = client.receive()
                self.assertTrue(final.get('final') is True, marker)
                old = c.read_observation(self.home, self.project, self.session, self.run, [final], 'request')
                self.assertTrue(old['registry']['record']['state'] == 'running'
                                and old['durableStatus']['entries'] == [], marker)
                original_sleep = time.sleep
                def advance(duration):
                    release.set()
                    if not gateway.completed.wait(2):
                        raise RuntimeError(marker + ': mock commit deadline')
                    committed.set()
                    original_sleep(duration)
                with patch.object(c.time, 'sleep', side_effect=advance):
                    observed, _, _ = c.wait_observation(self.home, self.project, self.session, self.run,
                                                       [final], 'request', time.monotonic() + 3)
                self.assertTrue(committed.is_set() and observed['registry']['record']['state'] == 'completed'
                                and observed['durableStatus']['entries'][0]['event'] == 'done', marker)
        except Exception as error:
            self.fail(marker + ': ' + repr(error))
        finally:
            release.set()
            if client is not None:
                client.socket.close()
            self.assertTrue(gateway.close(3), marker + ': mock cleanup ' + repr(gateway.errors))

    def test_observer_binding_parity(self):
        marker = '#1140 r11 observer binding implementations agree on values and rejection'
        def hello(value):
            return {'frame': {'type': 'hello_ok', 'interactionBinding': value}}
        samples = [[], [hello(None)], [hello([])], [hello({'a': 1}), hello({'a': 2})],
                 [hello({'a': 1})], [hello({'a': 1, 'b': 2}), hello({'b': 2, 'a': 1})],
                 [{'frame': []}, hello({'generation': 1})], [hello(True)]]
        expected = [False, False, False, False, True, True, True, False]
        for rows, accepted in zip(samples, expected):
            answers = []
            for function, argument, error_type in ((w.observer_binding, {'observer': {'rows': rows}}, w.Incomplete),
                                                   (s._observer_binding, rows, s.EvidenceError)):
                try:
                    answers.append((True, function(argument)))
                except error_type:
                    answers.append((False, None))
                except Exception as error:
                    self.fail(marker + ': ' + repr(error))
            self.assertTrue(answers[0] == answers[1] and answers[0][0] == accepted, marker)

    def test_non_object_frames(self):
        marker = '#1140 r11 non-object JSON wire frames reject on OBSERVER_WIRE'
        for row in (None, []):
            error = None
            try:
                s._audit_observer_rows([row])
            except s.EvidenceError as caught:
                error = caught
            except Exception as caught:
                self.fail(marker + ': ' + repr(caught))
            self.assertTrue(error is not None and error.rule == 'OBSERVER_WIRE', marker)
        for value in ([], None, 1, 'text', True):
            payload = json.dumps(value).encode()
            row = {'direction': 'receive', 'frame': value, 'connection': 1,
                   'wireHex': (bytes([0x81, len(payload)]) + payload).hex(),
                   'wire': {'opcode': 1, 'fin': True, 'rsv': 0, 'masked': False,
                            'lengthCode': len(payload), 'payloadLength': len(payload)}}
            error = None
            try:
                s._audit_observer_rows([row])
            except s.EvidenceError as caught:
                error = caught
            except Exception as caught:
                self.fail(marker + ': ' + repr(caught))
            self.assertTrue(error is not None and error.rule == 'OBSERVER_WIRE', marker)

    def test_provider_objects(self):
        marker = '#1140 r11 provider bodies and messages reject with controlled reason'
        for body, reason in (([], 'body'), (None, 'body'),
                             ({'stream': True, 'model': 'mock', 'messages': [None]}, 'message')):
            error = None
            try:
                n.provider_cases([{'raw': json.dumps(body)}], [])
            except n.Incomplete as caught:
                error = caught
            except Exception as caught:
                self.fail(marker + ': ' + repr(caught))
            self.assertTrue(error is not None and reason in str(error), marker)

    def test_deferred_receipt(self):
        marker = '#1140 r11 deferred capture cannot publish success before session artifacts'
        wt = load('r11_witness_tests', Path(__file__).with_name('test_witness.py'))
        fixture = wt.WitnessTests('test_smoke_before_receipt')
        fixture.setUp()
        self.addCleanup(fixture.doCleanups)
        original = wt.w.capture
        def deferred(*args, **kwargs):
            return original(*args, defer_receipt=True, **kwargs)
        folder = fixture.folder / 'deferred'
        try:
            with patch.object(wt.w, 'capture', side_effect=deferred):
                fixture.capture_fixture(folder, lambda *_: None)
            receipt = json.loads((folder / 'receipt.json').read_text())
        except Exception as error:
            self.fail(marker + ': ' + repr(error))
        self.assertTrue(receipt['status'] == 'INCOMPLETE', marker)
        rejected = None
        try:
            wt.w.load_capture(folder)
        except wt.w.Incomplete as error:
            rejected = error
        self.assertTrue(rejected is not None, marker)

    def test_publication_failure(self):
        marker = '#1140 r11 session publication failure leaves incomplete receipt and failure record'
        from unittest.mock import Mock
        for fault in (OSError('injected publish'), KeyboardInterrupt('injected cancel')):
            output = self.home / type(fault).__name__
            server, worker = Mock(), Mock()
            worker.is_alive.return_value = False
            def capture(_contract, capture_output, _environment, **kwargs):
                capture_output.mkdir(parents=True)
                receipt = {'status': 'INCOMPLETE' if kwargs.get('defer_receipt') else 'WITNESS_CAPTURED'}
                w.save(capture_output / 'receipt.json', receipt)
                return 'launch', 'evidence'
            caught = None
            try:
                with patch.object(c, 'BoundedHTTPServer', return_value=server), \
                        patch.object(c.threading, 'Thread', return_value=worker), \
                        patch.object(c.w, 'identity', return_value={}), patch.object(c.w, 'capture', side_effect=capture), \
                        patch.object(c, 'publish_session_domain', side_effect=fault):
                    c._native_run(__file__, __file__, output, 28999, session_domain=True)
            except (OSError, KeyboardInterrupt) as error:
                caught = error
            try:
                receipt = json.loads((output / 'capture/receipt.json').read_text())
                failure = json.loads((output / 'capture/failure.json').read_text())
            except Exception as error:
                self.fail(marker + ': ' + repr(error))
            self.assertTrue(caught is fault and receipt['status'] == 'INCOMPLETE'
                            and failure['status'] == 'INCOMPLETE', marker)

    def test_jsonl_sibling_readers(self):
        marker = '#1140 r11 all sibling JSONL readers retain Unicode separators'
        # Test actual reader iterator ASTs, with only the input text replaced.
        # B2 CLI bootstrap deliberately refuses uncommitted frozen candidates;
        # this pure-function probe does not relax its production admission.
        import types
        def jsonl_helper(filename):
            # Execute the real helper without module bootstrap (g03 loads the
            # TS baseline at import time, which the offline CI job lacks).
            path = ROOT / 'scripts' / filename
            tree = ast.parse(path.read_bytes().decode(), filename=str(path))
            definitions = [node for node in tree.body
                           if isinstance(node, ast.FunctionDef) and node.name == 'jsonl_lines']
            self.assertTrue(len(definitions) == 1, marker + ': helper coverage ' + filename)
            namespace = {}
            exec(compile(ast.Module(body=definitions, type_ignores=[]), str(path), 'exec'), namespace)
            return namespace['jsonl_lines']

        sample = json.dumps({'text': 'a\u2028b\u2029c\u0085d'}, ensure_ascii=False) + '\n'
        files = {'parity/g03_compare.py': 1, 'parity/score.py': 1,
                 'parity/controls/run_all.py': 1, 'parity/controls/multimodal.py': 3,
                 'exp008/compare.py': 1, 'exp008/run.py': 1, 'ui-server-parity/compare.py': 1,
                 'ui-server-parity/snapshot.py': 1, 'ui-server-parity/run.py': 1,
                 'ui-server-parity/check_coverage.py': 1}
        try:
            parity = jsonl_helper('parity/g03_compare.py')
            b1 = jsonl_helper('exp008/compare.py')
            self.assertTrue(parity(sample) == b1(sample) == n.jsonl_lines(sample), marker)
            for filename, count in files.items():
                tree = ast.parse((ROOT / 'scripts' / filename).read_bytes().decode())
                candidates = []
                for node in ast.walk(tree):
                    if isinstance(node, ast.comprehension) and isinstance(node.target, ast.Name) and node.target.id == 'line':
                        candidates.append(node.iter)
                    elif isinstance(node, ast.For) and any(isinstance(child, ast.Call)
                            and isinstance(child.func, ast.Attribute) and child.func.attr == 'loads'
                            for child in ast.walk(node)):
                        iterator = node.iter
                        if isinstance(iterator, ast.Call) and isinstance(iterator.func, ast.Name) and iterator.func.id == 'enumerate':
                            iterator = iterator.args[0]
                        candidates.append(iterator)
                readers = [node for node in candidates if isinstance(node, ast.Call)
                           and not (isinstance(node.func, ast.Attribute) and isinstance(node.func.value, ast.Attribute)
                                    and node.func.value.attr in ('stdout', 'stderr'))
                           and ((isinstance(node.func, ast.Attribute) and node.func.attr in ('jsonl_lines', 'splitlines'))
                                or (isinstance(node.func, ast.Name) and node.func.id == 'jsonl_lines'))]
                self.assertTrue(len(readers) == count, marker + ': reader coverage ' + filename)
                helper = parity if filename.startswith('parity/') else b1
                for reader in readers:
                    if isinstance(reader.func, ast.Attribute) and reader.func.attr == 'splitlines':
                        reader.func.value = ast.Constant(sample)
                    else:
                        reader.args = [ast.Constant(sample)]
                    expression = ast.fix_missing_locations(ast.Expression(reader))
                    lines = eval(compile(expression, filename, 'eval'), {'jsonl_lines': helper,
                                 'G': types.SimpleNamespace(jsonl_lines=helper),
                                 'b1_comparator': types.SimpleNamespace(jsonl_lines=helper)})
                    self.assertTrue([json.loads(line) for line in lines if line] == [json.loads(sample)], marker + ': ' + filename)
        except Exception as error:
            self.fail(marker + ': ' + repr(error))

    def test_deadline_non_object_report(self):
        marker = '#1140 r11 deadline persists non-object rejection as FAIL with reason'
        fixture_tools = load('r11_controls', ROOT / 'scripts/gateway-parity/controls/session_domain.py')
        folder = fixture_tools.native_capture_fixture(self.home / 'invalid-wire')
        document = json.loads((folder / 'session-domain/observer.json').read_text())
        row = next(row for row in document['rows'] if row['direction'] == 'receive' and row['frame'].get('type') != 'hello_ok')
        row.update(frame=[], wireHex='81025b5d', wire={'opcode': 1, 'fin': True, 'rsv': 0,
                    'masked': False, 'lengthCode': 2, 'payloadLength': 2})
        fixture_tools.save(folder / 'session-domain/observer.json', document)
        receipt = json.loads((folder / 'capture/receipt.json').read_text())
        receipt['observerSha256'] = fixture_tools.digest(folder / 'session-domain/observer.json')
        fixture_tools.save(folder / 'capture/receipt.json', receipt)
        output = self.home / 'deadline-report'
        try:
            with patch.object(s, 'install_shutdown'), patch.object(s, 'parity_lock', return_value=contextlib.nullcontext()), \
                    patch.object(s, 'hashes', return_value={}), patch.object(s, 'frozen_check', return_value={'exitCode': 0}), \
                    patch.object(s, 'deadline_report', side_effect=lambda *_, **__: s._load_native_artifacts(folder)), \
                    patch.object(sys, 'argv', ['session', '--suite', 'deadline', '--output', str(output)]), \
                    contextlib.redirect_stdout(io.StringIO()):
                code = s.main()
            report = json.loads((output / 'report.json').read_text())
        except Exception as error:
            self.fail(marker + ': ' + repr(error))
        self.assertTrue(code == 1 and report['status'] == 'FAIL' and report['executionStatus'] == 'NOT_RUN'
                        and report['failure']['rule'] == 'OBSERVER_WIRE'
                        and 'object' in report['failure']['detail'], marker + ': ' + repr(report))

    def test_decoded_send_object(self):
        marker = '#1140 r11 decoded send JSON must reject with controlled reason'
        fixture_tools = load('r11_send_controls', ROOT / 'scripts/gateway-parity/controls/session_domain.py')
        folder = fixture_tools.native_capture_fixture(self.home / 'send-shape')
        healthy = s._load_native_artifacts(folder)
        observer = json.loads((folder / 'session-domain/observer.json').read_text())
        row = next(row for row in observer['rows'] if row['direction'] == 'send')
        row['frame'] = '[]'
        args = [healthy['contract'], healthy['rows'], observer,
                json.loads((folder / 'session-domain/provider.json').read_text()),
                json.loads((folder / 'session-domain/driver.json').read_text()),
                json.loads((folder / 'session-domain/sources.json').read_text()), healthy['expected']]
        caught = None
        try:
            n.session_chain(*args)
        except n.Incomplete as error:
            caught = error
        except Exception as error:
            self.fail(marker + ': ' + repr(error))
        self.assertTrue(caught is not None and 'object' in str(caught), marker)

    def test_receipt_promotion(self):
        marker = '#1140 r11 successful session publication alone promotes receipt'
        for broken in (False, True):
            output = self.home / str(broken)
            (output / 'capture').mkdir(parents=True)
            w.save(output / 'capture/receipt.json', {'status': 'INCOMPLETE'})
            original_save = w.save
            def publish(path, value):
                if broken and path.name == 'provider.json':
                    raise OSError('injected partial publish')
                original_save(path, value)
            try:
                with patch.object(w, 'save', side_effect=publish):
                    c.publish_session_domain(output, {}, [], [], 'endpoint', {'binding': {}})
            except OSError:
                self.assertTrue(broken, marker)
            receipt = json.loads((output / 'capture/receipt.json').read_text())
            self.assertTrue(receipt['status'] == ('INCOMPLETE' if broken else 'WITNESS_CAPTURED'), marker)

    def test_collector_receive_shape(self):
        marker = '#1140 r11 collector rejects non-object received JSON before dispatch'
        client = object.__new__(c.WireClient)
        client.rows = []
        for value in ([], None, 1, 'text', True):
            payload = json.dumps(value).encode()
            chunks = iter((bytes([0x81, len(payload)]), payload))
            client.read = lambda _: next(chunks)
            error = None
            try:
                client.receive()
            except w.Incomplete as caught:
                error = caught
            except Exception as caught:
                self.fail(marker + ': ' + repr(caught))
            self.assertTrue(error is not None and 'object' in str(error), marker)

    def test_settle_terminal_states(self):
        marker = '#1140 r11 settle accepts exactly terminal run states'
        for state in ('accepted', 'running', 'completed', 'failed', 'aborted', 'interrupted', 'unknown'):
            self.clock, self.sleeps = 0.0, []
            self.write(state)
            try:
                result = self.wait(deadline=0.11)
            except Exception as error:
                self.fail(marker + ': ' + repr(error))
            self.assertTrue((result[0] is not None) == (state in ('completed', 'aborted', 'interrupted')), marker)

    def test_collector_wait_wired(self):
        marker = '#1140 r11 real collector waits after final before recording the case'
        e2e = load('r11_e2e', Path(__file__).parent.parent / 'native-session-e2e/test_e2e.py')
        fixture = e2e.E2ETests
        fixture.host, fixture.sidecar = self.home / 'host', self.home / 'sidecar'
        fixture.host.write_bytes(b'\x7fELF controlled host')
        fixture.sidecar.write_bytes(b'\x7fELF controlled sidecar')
        case = next(case for case, expected in c.session_expectations().items() if expected['registry']['applicable'])
        expectations = {case: {'frames': [], 'durableStatus': ['done'],
                              'registry': {'applicable': True, 'state': 'completed', 'events': [], 'result': None}}}
        release = threading.Event()
        def after_final():
            if not release.wait(3):
                raise RuntimeError(marker + ': deterministic release deadline')
        read = e2e.c.read_observation
        def observed(*args, **kwargs):
            result = read(*args, **kwargs)
            release.set()  # Only after the incomplete snapshot was actually read.
            return result
        output = self.home / 'collector-wait'
        try:
            with patch.object(e2e.c, 'session_manifest', return_value=[case]), \
                    patch.object(e2e, 'load_expectations', return_value=expectations), \
                    patch.object(e2e.c, 'read_observation', side_effect=observed):
                fixture._collect(output, real_settle=True, after_final=after_final)
            row = json.loads((output / 'session-domain/cases.json').read_text())['cases'][0]
            self.assertTrue(row['observation']['registry']['record']['state'] == 'completed'
                            and row['observation']['durableStatus']['entries'][0]['event'] == 'done', marker)
        except Exception as error:
            self.fail(marker + ': ' + repr(error))
        finally:
            release.set()

    def test_settle_timeout_full_chain(self):
        marker = '#1140 r11 unsettled case passes authentication and stays NOT_RUN'
        fixture_tools = load('r11_timeout_controls', ROOT / 'scripts/gateway-parity/controls/session_domain.py')
        folder = fixture_tools.native_capture_fixture(self.home / 'timeout-chain')
        rows = json.loads((folder / 'session-domain/cases.json').read_text())
        rows['cases'][0].update(observation=None, reason='落盘未完成')
        fixture_tools.save(folder / 'session-domain/cases.json', rows)
        receipt = json.loads((folder / 'capture/receipt.json').read_text())
        receipt['casesSha256'] = fixture_tools.digest(folder / 'session-domain/cases.json')
        fixture_tools.save(folder / 'capture/receipt.json', receipt)
        try:
            report = fixture_tools.control_deadline_report(folder)
            verdict = report['cells'][-1]['cases'][0]
        except Exception as error:
            self.fail(marker + ': ' + repr(error))
        self.assertTrue(verdict['status'] == 'NOT_RUN' and verdict['detail'] == '落盘未完成', marker)

    def test_settle_failed_requires_final_error(self):
        marker = '#1140 r11 failed attempts settle only with persisted and final errors'
        for final_type, durable_type, final_flag, accepted in (
                ('error', 'error', True, True), ('error', 'error', False, False),
                ('turn_completed', 'error', True, False), ('error', 'turn_completed', True, False)):
            self.clock, self.sleeps = 0.0, []
            self.write('failed')
            path = self.home / 'runs.json'
            registry = json.loads(path.read_text())
            registry['records'][0]['result'] = {'type': durable_type, 'code': 'turn_timeout'}
            temporary = path.with_suffix('.partial')
            temporary.write_text(json.dumps(registry))
            temporary.replace(path)
            frames = [{'id': 'request', 'final': final_flag, 'event': {'type': final_type, 'code': 'result_unknown'}}]
            try:
                result = self.wait(deadline=0.11, received=frames)
            except Exception as error:
                self.fail(marker + ': ' + repr(error))
            self.assertTrue((result[0] is not None) == accepted, marker)

    def test_deadline_shape_exception_report(self):
        marker = '#1140 r11 residual JSON shape exceptions persist FAIL reports'
        for exception in (AttributeError, IndexError):
            output = self.home / exception.__name__
            try:
                with patch.object(s, 'install_shutdown'), patch.object(s, 'parity_lock', return_value=contextlib.nullcontext()), \
                        patch.object(s, 'hashes', return_value={}), patch.object(s, 'frozen_check', return_value={'exitCode': 0}), \
                        patch.object(s, 'deadline_report', side_effect=exception('malformed JSON shape')), \
                        patch.object(sys, 'argv', ['session', '--suite', 'deadline', '--output', str(output)]), \
                        contextlib.redirect_stdout(io.StringIO()):
                    code = s.main()
                report = json.loads((output / 'report.json').read_text())
            except Exception as error:
                self.fail(marker + ': ' + repr(error))
            self.assertTrue(code == 1 and report['status'] == 'FAIL' and report['failure']['rule'] == 'EVIDENCE'
                            and report['failure']['detail'] == 'malformed JSON shape', marker)

    def test_settle_file_signatures(self):
        marker = '#1140 r11 both files require stable size and mtime'
        import os
        for path in (self.chat, self.home / 'runs.json'):
            for dimension in ('size', 'mtime'):
                self.clock, self.sleeps = 0.0, []
                self.write('completed')
                def tick(duration):
                    self.sleep(duration)
                    if len(self.sleeps) == 2:
                        old = path.stat()
                        if dimension == 'size':
                            with path.open('a') as stream:
                                stream.write(' ')
                            os.utime(path, ns=(old.st_atime_ns, old.st_mtime_ns))
                        else:
                            os.utime(path, ns=(old.st_atime_ns, old.st_mtime_ns + 1000000))
                try:
                    result = self.wait(tick)
                except Exception as error:
                    self.fail(marker + ': ' + repr(error))
                self.assertTrue(result[0] is not None and len(self.sleeps) == 4, marker)


if __name__ == '__main__':
    unittest.main()
