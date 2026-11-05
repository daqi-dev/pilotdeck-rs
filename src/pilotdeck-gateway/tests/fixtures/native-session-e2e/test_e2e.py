# anchor-selfcheck: unsupported end-to-end capture run, no mutation table
#!/usr/bin/env python3
"""#1140 fix-r1 end-to-end: the collector's real output scores per case.

Runs the real session-domain collection (``collect._native_run``) against a
real-socket mock gateway (``mock_gateway.py``), with only the OS layer mocked
(strace, pidfd, platform) and provider delays shortened, then loads
the frozen scoring by explicit path and requires the deadline report to score
every one of the 72 manifest cases PASS/FAIL per case -- never EVIDENCE
fail-closed (#1140 fix-r1 P1).  The data controls below corrupt exactly one
captured surface at a time (receipt hashes re-pinned, so the scoring logic is
under test, not the tamper detector) and require the dedicated rule or the
per-case FAIL.  Source mutations live in negative_controls.py.
"""
import sys
if not sys.flags.isolated:
    raise SystemExit('run with python3 -I')
sys.dont_write_bytecode = True
import contextlib
import importlib.util
import io
import json
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile
import time
import threading
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[5]
spec = importlib.util.spec_from_file_location('witness_collector', ROOT / 'scripts/native-witness/collect.py')
c = importlib.util.module_from_spec(spec)
spec.loader.exec_module(c)
sys.path.insert(0, str(Path(__file__).resolve().parent))
from mock_gateway import Gateway, load_expectations

def _reserve_port_pair(candidates=range(28999, 29009)):
    """Hold both fixed-profile ports until handoff; mirror each server's bind policy."""
    for candidate in candidates:
        gateway_guard = socket.socket()
        try:
            gateway_guard.bind(('127.0.0.1', candidate))
        except OSError:
            gateway_guard.close()
            continue
        port = gateway_guard.getsockname()[1]
        provider_guard = socket.socket()
        try:
            provider_guard.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            provider_guard.bind(('127.0.0.1', port + 1))
        except (OSError, OverflowError):
            provider_guard.close()
            gateway_guard.close()
            continue
        return port, gateway_guard, provider_guard
    raise AssertionError('#1140 e2e: no free gateway/provider port pair')


def load_frozen():
    """The frozen scoring by explicit path (run_control's shape, no CLI lease).

    Byte-loading the working tree is the point: the source mutations in
    negative_controls.py run this file against the mutated bytes.
    """
    path = ROOT / 'scripts/gateway-parity/session_domain.py'
    spec = importlib.util.spec_from_file_location('e2e_frozen_session_domain', path)
    module = importlib.util.module_from_spec(spec)
    exec(compile(path.read_bytes(), str(path), 'exec'), module.__dict__)
    return module


def manifest_case_count():
    """#1402 the scored denominator is the calibration manifest length."""
    return len(c.session_manifest())


def hex_string(text):
    return '"' + ''.join(f'\\x{byte:02x}' for byte in text.encode()) + '"'


class E2ETests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory(prefix='pdrs-1140-e2e-')
        cls.addClassCleanup(cls.temp.cleanup)
        cls.folder = Path(cls.temp.name).resolve()
        # Fake ELF executables: the recorder's identity check pins the magic
        # and the hash, never the platform.  The names avoid the audit's
        # ts-gateway/host-helper markers.
        cls.host = cls.folder / 'mock-host'
        cls.sidecar = cls.folder / 'mock-sidecar'
        cls.host.write_bytes(b'\x7fELF' + b'#1140 e2e host stand-in')
        cls.sidecar.write_bytes(b'\x7fELF' + b'#1140 e2e sidecar stand-in')
        # The recorder's identity pins the resolved path (macOS /var symlink),
        # and the synthetic trace below must show the exact same bytes.
        cls.host, cls.sidecar = cls.host.resolve(), cls.sidecar.resolve()
        cls.output = cls.folder / 'session-capture'
        cls._collect(cls.output)

    # ---- the real collection, OS layer mocked ------------------------------
    @classmethod
    def _trace(cls, gateway_argv, output):
        # One sidecar per manifest session (#1140 cap-fix): the real assembly
        # spawns a fresh sidecar for every session, and the collector's
        # contract now declares -- and validate_session pre-checks -- exactly
        # that multiset, so the synthetic trace must carry the same shape.
        # #1140 fix-r4 P1-1: the host opens each session's transcript (the
        # path the mock gateway actually writes) before that session's
        # sidecar exec -- the interleaving the per-session window attribution
        # over the strace openat witness requires.
        host = f'100 execve({hex_string(gateway_argv[0])}, ' \
               f'[{", ".join(map(hex_string, gateway_argv))}], 0x1 /* 0 vars */) = 0\n'
        pids = list(range(102, 102 + len(c.session_manifest())))
        sessions = ''.join(
            f'100 openat(AT_FDCWD, {hex_string(str(output / "home" / "projects" / "mock" / "chats" / (c.session_key(case) + ".jsonl")))}, '
            f'O_WRONLY|O_CREAT|O_APPEND, 0644) = 5\n'
            + f'100 clone(child_stack=NULL, flags=CLONE_VM|CLONE_VFORK|SIGCHLD) = {pid}\n'
            + f'{pid} execve({hex_string(str(cls.sidecar))}, '
              f'[{hex_string(str(cls.sidecar))}], 0x1 /* 0 vars */) = 0\n'
            for pid, case in zip(pids, c.session_manifest()))
        return (host
                + sessions
                + '100 connect(3, {sa_family=AF_INET, sin_port=htons(' + str(cls.port + 1) + '), '
                  'sin_addr=inet_addr(' + hex_string('127.0.0.1') + ')}, 16) = -1 EINPROGRESS '
                  '(Operation now in progress)\n'
                + ''.join(f'{pid} exit_group(0) = ?\n{pid} +++ exited with 0 +++\n' for pid in pids)
                + '100 exit_group(0) = ?\n100 +++ exited with 0 +++\n')

    @classmethod
    def _collect(cls, output, *, real_settle=False, after_final=None, synthetic_stimuli=True):
        # The all-expectations projection fixture includes intentionally absent
        # registries; it reads them directly. test_r11.test_collector_wait_wired
        # uses real_settle=True and a final-before-disk socket gateway to cover
        # the production waiting path without this projection-only substitute.
        tracer = mock.Mock(pid=99)
        tracer.poll.return_value = None
        tracer.wait.return_value = 0

        def spawn(argv, **kwargs):
            # The mocked strace writes the synthetic trace the recorder would
            # have produced, derived from the very argv the collector
            # registered, plus the readiness line the driver polls for.
            trace = Path(argv[argv.index('-o') + 1])
            trace.write_text(cls._trace(argv[argv.index('--') + 1:], output))
            stderr = kwargs.get('stderr')
            if stderr is not None:
                stderr.write(f'native listening 127.0.0.1:{cls.port}\n'.encode())
                stderr.flush()
            return tracer

        cls.port, gateway_guard, provider_guard = _reserve_port_pair()
        try:
            # The gateway slot is handed over to Gateway's own bind here; the
            # provider slot only when _native_run is about to bind it.
            gateway_guard.close()
            gateway = Gateway(cls.port, output / 'home', f'http://127.0.0.1:{cls.port + 1}',
                              load_expectations(ROOT), after_final=after_final)
            try:
                with contextlib.ExitStack() as stack:
                    if synthetic_stimuli:
                        # #1368 this gateway replays registered outputs; it is
                        # not a real stimulus adapter. Keep these existing
                        # scorer surface controls explicitly synthetic. The
                        # real collection gate is tested below without this tap.
                        stack.enter_context(mock.patch.object(c.stimuli, 'execution', return_value={
                            'status': 'WITNESSED', 'scope': 'SYNTHETIC_CONTROL_ONLY'}))
                    for patch in (mock.patch.object(sys, 'platform', 'linux'),
                                  mock.patch.object(c.w.shutil, 'which', return_value='/strace'),
                                  mock.patch.object(c.w.subprocess, 'run',
                                                    return_value=mock.Mock(stdout='--kill-on-exit')),
                                  mock.patch.object(c.w.subprocess, 'Popen', side_effect=spawn),
                                  mock.patch.object(c.w, 'stop_host'),
                                  mock.patch.object(c, 'wait_observation', side_effect=c.wait_observation if real_settle else
                                                    lambda home, project, key, run, frames, request, deadline, **kwargs:
                                                    c.read_observation(home, project, key, run, frames, request, retain=True)),
                                  mock.patch.object(c, 'SESSION_PROVIDER_DELAY', 0.0)):
                        stack.enter_context(patch)
                    provider_guard.close()
                    c._native_run(cls.host, cls.sidecar, output, cls.port, session_domain=True)
            finally:
                if not gateway.close(30):
                    error = '#1140 e2e mock gateway served the whole manifest without errors: ' + repr(gateway.errors)
                    if sys.exception() is None:
                        raise AssertionError(error)
                    sys.exception().add_note(error)
        finally:
            gateway_guard.close()
            provider_guard.close()

    # ---- scoring helpers ---------------------------------------------------
    def test_unwitnessed_stimuli_are_not_executed(self):
        marker = '#1368 captured requests without target witnesses all score NOT_RUN'
        raw_marker = '#1368 NOT_RUN retains all raw originals and missing target reasons'
        folder = self.folder / 'unwitnessed-capture'
        try:
            self._collect(folder, synthetic_stimuli=False)
        except Exception as error:
            self.fail(marker + ': ' + repr(error))
        # Check retained originals before scoring: a missing source must hit
        # this dedicated assertion rather than a later authentication failure.
        try:
            rows = json.loads((folder / 'session-domain/cases.json').read_text())['cases']
            sources = json.loads((folder / 'session-domain/sources.json').read_text())
            self.assertTrue(len(rows) == len(sources) == manifest_case_count()
                            and all(isinstance(source['transcriptText'], str)
                                    and Path(source['transcriptPath']).is_file() for source in sources)
                            and all(row['stimulus']['status'] == 'NOT_RUN' and row['stimulus']['missing']
                                    and '未执行' in row['reason'] for row in rows), raw_marker)
        except Exception as error:
            self.fail(raw_marker + ': ' + repr(error))
        report = self._score(folder, marker)
        candidates = [cell for cell in report['cells'] if cell.get('cases')]
        self.assertTrue(len(candidates) == 1, '#1368 NOT_RUN retains exactly one native candidate')
        cell = candidates[0]
        self.assertTrue(len(cell['cases']) == manifest_case_count()
                        and all(row['status'] == 'NOT_RUN' for row in cell['cases']), marker)
        self.assertTrue(cell['executedCases'] == [], '#1368 unwitnessed stimuli never count as executed')

    def _score(self, folder, marker):
        """Score one capture; any broad failure becomes this test's own
        assertion failure, so a source mutant kills its bound test instead
        of erroring it."""
        frozen = load_frozen()
        try:
            return frozen.deadline_report(native_capture=str(folder))
        except Exception as error:
            self.assertTrue(False, marker + ' (scoring failed closed: '
                            + type(error).__name__ + ': ' + str(error) + ')')

    def _score_projection(self, folder, marker):
        """Pure comparison controls, separate from authentication of originals."""
        frozen = load_frozen()
        try:
            native = frozen.native_deadline_evidence(self.output)
            rows = json.loads((folder / 'session-domain/cases.json').read_text())['cases']
            native['verdicts'] = frozen.score_native_cases(rows, native['expected'], native['providerCases'])
            return frozen._deadline_report(native)
        except Exception as error:
            self.fail(marker + ': ' + repr(error))

    def _native_cell(self, report):
        executed = [cell for cell in report['cells'] if cell['executedCases']]
        self.assertTrue(len(executed) == 1, '#1140 e2e exactly the native cell carries evidence')
        return executed[0]

    def _mutated(self, label):
        return shutil.copytree(self.output, self.folder / ('control-' + label))

    @staticmethod
    def _repin(folder, relative, key):
        receipt_path = folder / 'capture/receipt.json'
        receipt = json.loads(receipt_path.read_text())
        receipt[key] = c.w.sha(folder / relative)
        receipt_path.write_text(json.dumps(receipt))

    def _rewrite(self, folder, relative, key, corrupt):
        document = json.loads((folder / relative).read_text())
        corrupt(document)
        (folder / relative).write_text(json.dumps(document))
        self._repin(folder, relative, key)

    # ---- the end-to-end claims ---------------------------------------------
    def test_e2e_scores_real_capture(self):
        report = self._score(self.output, '#1140 e2e real capture scores 72/72 per-case PASS')
        cell = self._native_cell(report)
        verdicts = cell['cases']
        self.assertTrue(len(verdicts) == manifest_case_count()
                        and [row['status'] for row in verdicts] == ['PASS'] * manifest_case_count(),
                        '#1140 e2e real capture scores 72/72 per-case PASS')
        self.assertTrue(all(row['rule'] is None for row in verdicts),
                        '#1140 e2e a healthy case carries no failing rule')
        self.assertTrue(cell['witness']['auditRule'] is None,
                        '#1140 e2e the real recorder rows pass the frozen audit')
        self.assertTrue(report['status'] == 'NOT_RUN',
                        '#1140 e2e native PASS leaves the batch NOT_RUN (no evidence for the other cells)')
        self.assertTrue(cell['executedCases'] == cell['expectedCases'],
                        '#1140 e2e all 72 cases count as executed')

    def test_e2e_capture_schema(self):
        # The P1 contract on the collector's real output: the launch contract
        # declares the planned connections and every recorder row carries the
        # wire metadata the frozen audit reads, with sent rows recorded as the
        # payload text (#1140 fix-r1).
        launch = json.loads((self.output / 'capture/launch.json').read_text())
        self.assertTrue(launch.get('connections') == 1,
                        '#1140 e2e the collector contract declares connections')
        # #1140 cap-fix: the collector declares one sidecar per manifest
        # session (host x1 plus sidecar x len(manifest)), matching the real
        # gateway assembly; the shrunk ['host', 'sidecar'] contract failed
        # closed on real 72-session captures.
        self.assertTrue(launch.get('processRoles')
                        == ['host'] + ['sidecar'] * len(c.session_manifest()),
                        '#1140 e2e the collector declares one sidecar per manifest session')
        observer = json.loads((self.output / 'session-domain/observer.json').read_text())
        rows = observer['rows']
        self.assertTrue(bool(rows) and all(isinstance(row.get('wire'), dict) for row in rows),
                        '#1140 e2e recorder rows carry wire metadata')
        sends = [row for row in rows if row['direction'] == 'send']
        self.assertTrue(bool(sends) and all(isinstance(row['frame'], str) for row in sends),
                        '#1140 e2e sent rows record the payload text')
        hellos = [row['frame'] for row in rows
                  if isinstance(row['frame'], dict) and row['frame'].get('type') == 'hello_ok']
        self.assertTrue(len(hellos) == 1, '#1140 e2e the observer recorded exactly one hello')
        # #1140 fix-r3: the driver's own hello record is published as its own
        # receipt-pinned artifact and must agree with the observer's hello
        # binding -- that anchor is what rejects a rewritten binding value.
        driver = json.loads((self.output / 'session-domain/driver.json').read_text())
        self.assertTrue(driver.get('binding') == hellos[0]['interactionBinding'],
                        '#1140 e2e the driver hello record matches the observer hello binding')
        # #1140 fix-r4 P1-1: the host's transcript opens are witnessed in one
        # shared order with the execs (afterExecs 1..72, one per session, in
        # manifest order) -- the field the per-session window attribution
        # attributes sidecars by.
        evidence = json.loads((self.output / 'capture/evidence.json').read_text())
        witness = evidence.get('transcriptWitness')
        self.assertTrue(isinstance(witness, dict) and witness.get('complete') is True
                        and witness.get('source') == 'strace-openat'
                        and isinstance(witness.get('opens'), list)
                        and [row['afterExecs'] for row in witness['opens']] == list(range(1, manifest_case_count() + 1)),
                        '#1140 e2e the transcript witness interleaves one open per session with the exec order')

    def test_e2e_parallel_suites(self):
        # #1140 fix-r2 P2-a: two simultaneous suites must both pass.  Each
        # child's setUpClass performs the full real collection on its own
        # self-assigned port pair; the child runs a single named test so this
        # never recurses.  The pre-fix fixed ports made the second child fail
        # with 'Address already in use'.
        script = str(Path(__file__).resolve())
        children = [subprocess.Popen([sys.executable, '-I', script,
                                      'E2ETests.test_e2e_capture_schema'],
                                     stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                    for _ in range(2)]
        try:
            outputs = [child.communicate(timeout=600) for child in children]
        except subprocess.TimeoutExpired:
            for child in children:
                child.kill()
            for child in children:
                child.communicate()
            self.assertTrue(False, '#1140 e2e parallel collections timed out')
        for child, (out, err) in zip(children, outputs):
            self.assertTrue(child.returncode == 0,
                            '#1140 e2e both parallel collections must pass\n'
                            + out.decode(errors='replace') + err.decode(errors='replace'))

    def test_e2e_data_control_surface_mismatch(self):
        folder = self._mutated('surface-mismatch')
        case = json.loads((self.output / 'session-domain/cases.json').read_text())['cases'][3]['case']

        def corrupt(document):
            document['cases'][3]['observation']['frames'].append(
                {'type': 'agent_status', 'event': 'forged-event'})
        self._rewrite(folder, 'session-domain/cases.json', 'casesSha256', corrupt)
        report = self._score_projection(folder, '#1140 e2e one mutated surface fails exactly that case')
        failures = [row for row in self._native_cell(report)['cases'] if row['status'] == 'FAIL']
        self.assertTrue(len(failures) == 1 and failures[0]['case'] == case
                        and failures[0]['rule'] == 'EXPECTATION_MISMATCH'
                        and failures[0]['surfaces'] == ['frames'],
                        '#1140 e2e one mutated surface fails exactly that case')

    def test_e2e_data_control_forged_binding_value(self):
        # #1140 fix-r3: the hello binding rewritten to another well-formed
        # value (wire bytes recomputed, receipt re-pinned) must be rejected on
        # BINDING -- every observer-side comparison is self-consistent with
        # the rewritten rows, so the unchanged driver copy catches this edit.
        # #1206: a writer changing both copies and receipt is outside this control.
        folder = self._mutated('forged-binding-value')

        def corrupt(document):
            row = next(entry for entry in document['rows']
                       if isinstance(entry.get('frame'), dict)
                       and entry['frame'].get('type') == 'hello_ok')
            row['frame']['interactionBinding'] = {'connectionId': 'forged-connection',
                                                  'generation': 2}
            payload = json.dumps(row['frame']).encode()
            prefix = bytes([0x81, 126]) + len(payload).to_bytes(2, 'big') if len(payload) >= 126 else bytes([0x81, len(payload)])
            row['wireHex'] = (prefix + payload).hex()
            row['wire']['lengthCode'] = 126 if len(payload) >= 126 else len(payload)
            row['wire']['payloadLength'] = len(payload)
        self._rewrite(folder, 'session-domain/observer.json', 'observerSha256', corrupt)
        frozen = load_frozen()
        try:
            frozen.deadline_report(native_capture=str(folder))
        except frozen.EvidenceError as error:
            self.assertTrue(error.rule == 'BINDING',
                            '#1140 e2e a forged hello binding value is rejected on BINDING: ' + str(error))
            return
        except Exception as error:
            self.assertTrue(False, '#1140 e2e a forged hello binding value is rejected on BINDING '
                            '(unexpected ' + type(error).__name__ + ': ' + str(error) + ')')
            return
        self.assertTrue(False, '#1140 e2e a forged hello binding value is rejected on BINDING')

    def test_e2e_data_control_send_frame_object(self):
        folder = self._mutated('send-frame-object')

        def corrupt(document):
            for row in document['rows']:
                if row['direction'] == 'send' and isinstance(row['frame'], str):
                    row['frame'] = json.loads(row['frame'])
                    return
        self._rewrite(folder, 'session-domain/observer.json', 'observerSha256', corrupt)
        try:
            load_frozen().deadline_report(native_capture=str(folder))
        except Exception as error:
            self.assertTrue(getattr(error, 'rule', None) == 'OBSERVER_WIRE'
                            or 'wire bytes/decoded frame disagree' in str(error),
                            '#1140 e2e send frame as object is rejected on the wire contract: '
                            + type(error).__name__ + ': ' + str(error))
            return
        self.assertTrue(False, '#1140 e2e send frame as object is rejected on the wire contract')

    def test_e2e_data_control_row_without_wire(self):
        folder = self._mutated('row-without-wire')

        def corrupt(document):
            document['rows'][1].pop('wire')
        self._rewrite(folder, 'session-domain/observer.json', 'observerSha256', corrupt)
        frozen = load_frozen()
        try:
            frozen.deadline_report(native_capture=str(folder))
        except frozen.EvidenceError as error:
            self.assertTrue(error.rule == 'OBSERVER_WIRE',
                            '#1140 e2e a recorder row without wire is rejected: ' + str(error))
            return
        except Exception as error:
            self.assertTrue(False, '#1140 e2e a recorder row without wire is rejected (unexpected '
                            + type(error).__name__ + ': ' + str(error) + ')')
            return
        self.assertTrue(False, '#1140 e2e a recorder row without wire is rejected')

    def test_e2e_data_control_empty_ledger(self):
        marker = '#1140 r10 empty provider ledger fails required cases without shrinking denominator'
        folder = self._mutated('empty-ledger')

        def corrupt(document):
            document['requests'] = []
        self._rewrite(folder, 'session-domain/provider.json', 'providerSha256', corrupt)
        frozen = load_frozen()
        report = self._score(folder, marker)
        cell = report['cells'][-1]
        required = frozen.native_evidence.required_provider_cases(
            frozen.read(frozen.SCRIPT / 'fixtures/session-expectations.json'))
        self.assertTrue({row['case'] for row in cell['cases'] if row['status'] == 'FAIL'} == required
                        and all(row['surfaces'] == ['provider.requests']
                                for row in cell['cases'] if row['case'] in required)
                        and len(cell['cases']) == manifest_case_count() and cell['executedCases'] == cell['expectedCases'], marker)

    def test_e2e_data_control_missing_record_state(self):
        folder = self._mutated('missing-record-state')
        rows = json.loads((self.output / 'session-domain/cases.json').read_text())['cases']
        index, case = next((position, row['case']) for position, row in enumerate(rows)
                           if row['observation']['registry'].get('applicable'))

        def corrupt(document):
            document['cases'][index]['observation']['registry']['record'].pop('state')
        self._rewrite(folder, 'session-domain/cases.json', 'casesSha256', corrupt)
        report = self._score_projection(folder, '#1140 e2e a missing run-record state is a per-case FAIL')
        failures = [row for row in self._native_cell(report)['cases'] if row['status'] == 'FAIL']
        self.assertTrue(len(failures) == 1 and failures[0]['case'] == case
                        and 'registry.state' in failures[0]['surfaces'],
                        '#1140 e2e a missing run-record state is a per-case FAIL')

    def test_e2e_data_control_observation_null(self):
        folder = self._mutated('observation-null')
        case = json.loads((self.output / 'session-domain/cases.json').read_text())['cases'][5]['case']

        def corrupt(document):
            document['cases'][5]['observation'] = None
        self._rewrite(folder, 'session-domain/cases.json', 'casesSha256', corrupt)
        report = self._score(folder, '#1140 e2e a missing observation stays NOT_RUN and unexecuted')
        cell = self._native_cell(report)
        verdict = next(row for row in cell['cases'] if row['case'] == case)
        self.assertTrue(verdict['status'] == 'NOT_RUN',
                        '#1140 e2e a missing observation stays NOT_RUN')
        self.assertTrue(case not in cell['executedCases'],
                        '#1140 e2e a NOT_RUN case is not executed')
        self.assertTrue(len(cell['executedCases']) == manifest_case_count() - 1,
                        '#1140 e2e the other 71 cases stay executed')

    def test_e2e_data_control_observation_missing(self):
        # #1140 fix-r2 P3-2: a case row whose observation key is absent scores
        # that case NOT_RUN instead of raising a batch-level KeyError.
        folder = self._mutated('observation-missing')
        case = json.loads((self.output / 'session-domain/cases.json').read_text())['cases'][5]['case']

        def corrupt(document):
            document['cases'][5].pop('observation')
        self._rewrite(folder, 'session-domain/cases.json', 'casesSha256', corrupt)
        report = self._score(folder, '#1140 e2e a row without the observation key scores '
                                       'NOT_RUN, not a batch KeyError')
        cell = self._native_cell(report)
        verdict = next(row for row in cell['cases'] if row['case'] == case)
        self.assertTrue(verdict['status'] == 'NOT_RUN',
                        '#1140 e2e a row without the observation key scores NOT_RUN (no batch KeyError)')
        self.assertTrue(case not in cell['executedCases'],
                        '#1140 e2e a NOT_RUN case is not executed')
        self.assertTrue(len(cell['executedCases']) == manifest_case_count() - 1,
                        '#1140 e2e the other 71 cases stay executed')

    def test_e2e_data_control_frames_without_type(self):
        folder = self._mutated('frames-without-type')

        def corrupt(document):
            row = next(entry for entry in document['cases']
                       if entry['observation']['frames'])
            row['observation']['frames'][0].pop('type')
        self._rewrite(folder, 'session-domain/cases.json', 'casesSha256', corrupt)
        frozen = load_frozen()
        try:
            frozen.deadline_report(native_capture=str(folder))
        except frozen.EvidenceError as error:
            self.assertTrue(error.rule == 'OBSERVATION_STRUCTURE',
                            '#1140 e2e frames without a type are a structure rejection: ' + str(error))
            return
        except Exception as error:
            self.assertTrue(False, '#1140 e2e frames without a type are a structure rejection '
                            '(unexpected ' + type(error).__name__ + ': ' + str(error) + ')')
            return
        self.assertTrue(False, '#1140 e2e frames without a type are a structure rejection')

    def test_e2e_data_control_no_registry_applicable(self):
        folder = self._mutated('no-registry-applicable')
        rows = json.loads((self.output / 'session-domain/cases.json').read_text())['cases']
        index, case = next((position, row['case']) for position, row in enumerate(rows)
                           if not row['observation']['registry'].get('applicable'))

        def corrupt(document):
            document['cases'][index]['observation']['registry'] = {
                'applicable': True, 'events': [], 'record': {}}
        self._rewrite(folder, 'session-domain/cases.json', 'casesSha256', corrupt)
        report = self._score_projection(folder, '#1140 e2e an observed applicability mismatch fails that case')
        failures = [row for row in self._native_cell(report)['cases'] if row['status'] == 'FAIL']
        self.assertTrue(len(failures) == 1 and failures[0]['case'] == case
                        and 'registry.applicable' in failures[0]['surfaces'],
                        '#1140 e2e an observed applicability mismatch fails that case')

    def test_e2e_data_control_sidecar_misallocated(self):
        # #1140 fix-r4 P1-1: the role multiset is unchanged (one session with
        # two sidecars, another with none), so only the per-session window
        # attribution over the host's transcript opens can reject the capture.
        folder = self._mutated('sidecar-misallocated')

        trace = folder / 'capture/trace.log'
        lines = trace.read_text().splitlines(True)
        position = [i for i, line in enumerate(lines) if ' openat(' in line][36]
        marker = lines.pop(position)
        lines.insert(position + 2, marker)
        raw = ''.join(lines)
        trace.write_text(raw)
        launch = json.loads((folder / 'capture/launch.json').read_text())
        evidence = c.w.parse_trace(raw, launch)
        c.w.save(folder / 'capture/evidence.json', evidence)
        receipt = json.loads((folder / 'capture/receipt.json').read_text())
        receipt.update(traceSha256=c.w.sha(trace), evidenceSha256=c.w.sha(folder / 'capture/evidence.json'))
        c.w.save(folder / 'capture/receipt.json', receipt)
        frozen = load_frozen()
        try:
            frozen.deadline_report(native_capture=str(folder))
        except frozen.EvidenceError as error:
            self.assertTrue(error.rule == 'NATIVE_PROCESS',
                            '#1140 e2e a balanced sidecar misallocation is rejected on NATIVE_PROCESS: '
                            + str(error))
            return
        except Exception as error:
            self.assertTrue(False, '#1140 e2e a balanced sidecar misallocation is rejected on '
                            'NATIVE_PROCESS (unexpected ' + type(error).__name__ + ': ' + str(error) + ')')
            return
        self.assertTrue(False, '#1140 e2e a balanced sidecar misallocation is rejected on NATIVE_PROCESS')


class LifecycleTests(unittest.TestCase):
    def _collector_profile(self, field, expected, marker):
        harness = type('ProfileHarness', (E2ETests,), {})
        try:
            try:
                harness.setUpClass()
            except c.w.Incomplete as error:
                if str(error) != marker:
                    raise
                self.fail(marker + ': collector emitted an invalid session contract')
            launch = json.loads((harness.output / 'capture/launch.json').read_text())
            self.assertTrue(launch.get(field) == expected, marker)
        finally:
            harness.doClassCleanups()

    def test_collector_profile_connections(self):
        self._collector_profile('connections', 1, '#1140 session profile connections')

    def test_collector_profile_roles(self):
        self._collector_profile('processRoles', ['host'] + ['sidecar'] * len(c.session_manifest()),
                                '#1140 session profile roles')

    def test_consecutive_collections_reuse_port_pair(self):
        marker = '#1140 r9 consecutive collections release the same port pair'
        harness = type('CollectionHarness', (E2ETests,), {})
        reserve = _reserve_port_pair
        port, gateway_guard, provider_guard = reserve()
        gateway_guard.close()
        provider_guard.close()
        try:
            with mock.patch.dict(globals(), _reserve_port_pair=lambda: reserve([port])):
                harness.setUpClass()
                harness._collect(harness.folder / 'second-capture')
                pair = reserve([port])
                pair[1].close()
                pair[2].close()
            self.assertTrue(not c.common.LIVE_GROUPS, marker)
        except Exception as error:
            self.fail(marker + ': ' + repr(error))
        finally:
            harness.doClassCleanups()

    def test_failed_reservation_closes_both_sockets(self):
        marker = '#1140 r9 failed reservation closes both guards'
        first, second = mock.Mock(), mock.Mock()
        first.getsockname.return_value = ('127.0.0.1', 28999)
        second.bind.side_effect = OSError('occupied fixture port')
        with mock.patch.object(socket, 'socket', side_effect=[first, second]):
            with self.assertRaises(AssertionError, msg=marker):
                _reserve_port_pair([28999])
        self.assertTrue(first.close.called and second.close.called, marker)

    def test_gateway_cancel_unconnected_and_partial_frame(self):
        marker = '#1140 r9 gateway cancellation stops listener and active socket'
        with tempfile.TemporaryDirectory(prefix='pdrs-1140-cancel-') as folder:
            for partial in (False, True):
                with self.subTest(partial=partial):
                    gateway = Gateway(0, Path(folder), 'unused', {'unfinished': {}})
                    peer = None
                    try:
                        if partial:
                            peer = socket.create_connection(gateway.listener.getsockname(), timeout=1)
                            peer.sendall(b'GET /ws HTTP/1.1\r\n')
                            self.assertTrue(gateway.connected.wait(5), marker)
                        self.assertTrue(not gateway.close(5) and gateway.errors and not gateway.thread.is_alive()
                                        and gateway.listener.fileno() == -1
                                        and (gateway.connection is None or gateway.connection.fileno() == -1), marker)
                    finally:
                        if peer is not None:
                            peer.close()
                        gateway.close(2)

    def test_gateway_manifest_completion_matrix(self):
        marker = '#1140 r10 gateway success requires the whole manifest'
        read_frame = Gateway._read_frame
        for count in (1, 2):
            for eof in (False, True):
                with self.subTest(completed_turns=count, peer_eof=eof):
                    ready = [threading.Event() for _ in range(4)]
                    reads = []
                    def read(gateway, connection):
                        ready[len(reads)].set()
                        reads.append(None)
                        return read_frame(gateway, connection)
                    def turn(gateway, connection, request):
                        gateway._send_frame(connection, {'final': True})
                        return request['case']
                    with tempfile.TemporaryDirectory(prefix='pdrs-1140-completion-') as folder, \
                            mock.patch.object(Gateway, '_read_frame', read), \
                            mock.patch.object(Gateway, '_turn', turn):
                        gateway = Gateway(0, Path(folder), 'unused', {'first': {}, 'second': {}})
                        peer = None
                        try:
                            peer = c.WireClient(gateway.listener.getsockname()[1], time.monotonic() + 5)
                            peer.send({'type': 'hello'})
                            self.assertTrue(peer.receive().get('type') == 'hello_ok', marker)
                            for case in ('first', 'second')[:count]:
                                peer.send({'case': case})
                                self.assertTrue(peer.receive() == {'final': True}, marker)
                            # The next read hook runs after the previous turn's completion accounting.
                            self.assertTrue(ready[count + 1].wait(5), marker)
                            self.assertTrue(gateway.connection.gettimeout() == 180,
                                            '#1140 r10 gateway idle read retains 180 second allowance')
                            if eof:
                                peer.socket.close()
                                self.assertTrue(gateway.finished.wait(5), marker)
                            result = gateway.close(5)
                            self.assertTrue(result is (count == 2) and not gateway.thread.is_alive(), marker)
                            self.assertTrue(gateway.completed.is_set() is (count == 2), marker)
                            self.assertTrue((not gateway.errors) is (count == 2),
                                            '#1140 r10 incomplete manifest records an error')
                        except Exception as error:
                            self.fail(marker + ': ' + repr(error))
                        finally:
                            if peer is not None:
                                peer.socket.close()
                            gateway.close(2)

    def test_failed_gateway_close_is_asserted(self):
        marker = '#1140 r9 failed gateway cleanup cannot be swallowed by classmethod'
        harness = type('FailedCloseHarness', (E2ETests,), {})
        close = Gateway.close
        def failed_close(gateway, timeout=30):
            close(gateway, timeout)
            return False
        try:
            with mock.patch.object(Gateway, 'close', failed_close):
                with self.assertRaisesRegex(AssertionError, '#1140 e2e mock gateway served', msg=marker):
                    harness.setUpClass()
        finally:
            harness.doClassCleanups()

    def test_collection_exception_still_cleans_up(self):
        marker = '#1140 r9 collection failure leaves no owned socket or thread'
        harness = type('FailureHarness', (E2ETests,), {})
        reserve = _reserve_port_pair
        port, first, second = reserve()
        first.close()
        second.close()
        try:
            with mock.patch.dict(globals(), _reserve_port_pair=lambda: reserve([port])), \
                    mock.patch.object(c, 'validate_session', side_effect=c.w.Incomplete('injected validation failure')):
                with self.assertRaises(c.w.Incomplete, msg=marker):
                    harness.setUpClass()
            pair = reserve([port])
            pair[1].close()
            pair[2].close()
            self.assertTrue(not c.common.LIVE_GROUPS, marker)
        except Exception as error:
            self.fail(marker + ': ' + repr(error))
        finally:
            harness.doClassCleanups()


class NegativeRunnerTests(unittest.TestCase):
    """#1402 the e2e runner takes the global parity lease per case, not per run.

    ``_drive`` runs the real ``main`` of ``negative_controls.py`` with only the
    guard and the subprocesses mocked; the lease protocol, the stage order and
    the restore proof under test are the runner's own code. The synthetic rows
    mutate a scratch file, and the lease gap control uses a private lock
    directory, so the real /tmp/pdrs-parity.lock is never touched.
    """

    MARKER = '#1402 synthetic marker'
    SYNTHETIC_TEST = 'test_negative_synthetic_bound'
    PRISTINE = 'x = 0\ny = 1\n'
    OK_LOG = 'Ran 1 test in 0.001s\n\nOK\n'
    KILLED_LOG = ('FAIL: test_negative_synthetic_bound '
                  '(__main__.NegativeRunnerTests.test_negative_synthetic_bound)\n'
                  'AssertionError: #1402 synthetic marker\n'
                  'Ran 1 test in 0.001s\n\nFAILED (failures=1)\n')

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='pdrs-1402-runner-')
        self.addCleanup(self.tmp.cleanup)
        self.folder = Path(self.tmp.name)
        self.source = self.folder / 'mutant_source.py'
        self.source.write_text(self.PRISTINE)
        self.out = self.folder / 'results'
        self.events = []
        self.timeouts = []
        self.disks = []
        c.common.LIVE_GROUPS.clear()
        self.addCleanup(c.common.LIVE_GROUPS.clear)

    def load_runner(self):
        path = Path(__file__).with_name('negative_controls.py')
        try:
            spec = importlib.util.spec_from_file_location('e2e_negative_controls', path)
            module = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(module)
        except Exception as error:
            self.fail('#1402 the e2e negative runner must load by explicit path: '
                      + str(path) + ': ' + repr(error))
        return module

    def _drive(self, runner, *, run_effect=None, slot_error=None, gap=0.0,
               lease=None, window=0.0):
        """Run the real e2e main(); record stage events, timeouts, disk bytes.

        Stage labels come from the runner's own calls (passed/classify/with),
        never from call counting, so injected failures cannot mislabel a stage.
        """
        events, timeouts, disks = self.events, self.timeouts, self.disks
        originals = {self.source: self.source.read_bytes()}
        ok = subprocess.CompletedProcess([], 0, '', self.OK_LOG)
        killed = subprocess.CompletedProcess([], 1, '', self.KILLED_LOG)
        real_passed = runner.passed

        @contextlib.contextmanager
        def mutating(root, paths, **_kwargs):
            if lease is None:
                events.append('acquire')
                try:
                    yield dict(originals)
                finally:
                    events.append('release')
            else:
                # The real lease protocol on a private directory: the drive is
                # evidence that a second host can take the between-case gap.
                with c.common.parity_lock(timeout=2, lock=lease):
                    events.append(('acquire', time.monotonic()))
                    if window:
                        time.sleep(window)
                    try:
                        yield dict(originals)
                    finally:
                        events.append(('release', time.monotonic()))

        def run_owned(command, **kwargs):
            # cargo is the only command that names its stage; a Python bound
            # call runs against the mutant only for the negative control.
            stage = 'cargo' if command and command[0] == 'cargo' else (
                'negative' if self.source.read_text() != self.PRISTINE else 'bound')
            timeouts.append((stage, kwargs.get('timeout')))
            if stage in ('cargo', 'negative'):
                events.append(stage)
                disks.append((stage, self.source.read_text()))
            error = run_effect(stage) if run_effect is not None else None
            if error is not None:
                raise error
            return killed if stage == 'negative' else ok

        def passed(result):
            if result.returncode != 0:
                # classify probes the negative result internally through the
                # module-global passed; that is not a stage of its own.
                return real_passed(result)
            # positive precedes the lease; restored is the first Python stage
            # after a release -- a plain string without a lease, a timestamped
            # tuple when the real lease protocol records the event.
            last = events[-1] if events else None
            stage = 'restored' if (last == 'release' or
                                   (isinstance(last, tuple) and last[0] == 'release')) else 'positive'
            events.append(stage)
            disks.append((stage, self.source.read_text()))
            if stage == 'restored' and gap:
                time.sleep(gap)
            return real_passed(result)

        def slot(_script, **kwargs):
            if slot_error is not None:
                raise slot_error
            events.append('slot')
            timeouts.append(('slot', kwargs.get('timeout')))
            return ok

        rows = [('first', self.source, 'x = 0', 'x = 1', self.SYNTHETIC_TEST),
                ('second', self.source, 'y = 1', 'y = 2', self.SYNTHETIC_TEST)]
        with mock.patch.object(runner, 'cases', rows), \
                mock.patch.object(runner, 'messages', {row[0]: self.MARKER for row in rows}), \
                mock.patch.object(runner, 'preflight'), \
                mock.patch.object(runner, 'execution_environment', return_value={}), \
                mock.patch.object(runner, 'run_owned', run_owned), \
                mock.patch.object(runner, 'wait_for_slot', slot), \
                mock.patch.object(runner, 'mutating_sources', mutating), \
                mock.patch.object(runner, 'passed', passed), \
                mock.patch.object(sys, 'argv',
                                  [str(Path(__file__).with_name('negative_controls.py')),
                                   str(self.out)]):
            code = runner.main()
        return code, json.loads((self.out / 'summary.json').read_text())

    def test_negative_lease_taken_and_released_per_case(self):
        marker = '#1402 each case takes and releases the parity lease exactly once'
        runner = self.load_runner()
        code, summary = self._drive(runner)
        expected = ['positive', 'slot', 'acquire', 'cargo', 'negative', 'release', 'restored']
        self.assertTrue(code == 0 and self.events == expected * 2, marker)
        self.assertTrue(all(row['status'] == 'KILLED' and row['compiled'] for row in summary['controls']), marker)
        self.assertTrue(self.events.count('acquire') == 2 and self.events.count('release') == 2, marker)
        for stage, text in self.disks:
            self.assertTrue((text != self.PRISTINE) == (stage in ('cargo', 'negative')),
                            marker + ': the mutant is on disk only inside the lease (' + stage + ')')

    def test_negative_lease_gap_admits_second_host(self):
        marker = '#1402 a second process acquires the parity lease in the between-case gap'
        runner = self.load_runner()
        lease = self.folder / 'parity.lock'
        marker_path = self.folder / 'second-host-acquired'
        poller_script = self.folder / 'poller.py'
        poller_script.write_text(
            'import importlib.util\n'
            'import sys\n'
            'import time\n'
            'from pathlib import Path\n'
            'lock, common, acquired = (Path(argument) for argument in (sys.argv[1], sys.argv[2], sys.argv[4]))\n'
            'hold = float(sys.argv[3])\n'
            'spec = importlib.util.spec_from_file_location("poller_common", common)\n'
            'module = importlib.util.module_from_spec(spec)\n'
            'spec.loader.exec_module(module)\n'
            'Path(sys.argv[5]).write_text("ready")\n'
            'deadline = time.monotonic() + 30\n'
            'while not lock.exists() and time.monotonic() < deadline:\n'
            '    time.sleep(0.02)\n'
            'with module.parity_lock(timeout=30, lock=lock):\n'
            '    acquired.write_text(str(time.monotonic()))\n'
            '    time.sleep(hold)\n')
        ready = self.folder / 'second-host-ready'
        poller = subprocess.Popen([sys.executable, '-I', '-B', str(poller_script), str(lease),
                                   str(ROOT / 'scripts/gateway-parity/common.py'), '0.2',
                                   str(marker_path), str(ready)])
        try:
            deadline = time.monotonic() + 30
            while not ready.is_file() and poller.poll() is None and time.monotonic() < deadline:
                time.sleep(0.02)
            self.assertTrue(ready.is_file(), marker + ': the second host must reach the lease loop')
            # window holds the first lease open long enough for the second
            # host to observe it; gap is the between-case window it must win.
            code, summary = self._drive(runner, lease=lease, gap=0.6, window=1.0)
            self.assertTrue(poller.wait(60) == 0, marker + ': the poller itself must exit cleanly')
        finally:
            if poller.poll() is None:
                poller.kill()
                poller.wait(10)
        acquires = [entry[1] for entry in self.events
                    if isinstance(entry, tuple) and entry[0] == 'acquire']
        releases = [entry[1] for entry in self.events
                    if isinstance(entry, tuple) and entry[0] == 'release']
        self.assertTrue(code == 0 and len(acquires) == len(releases) == 2 and marker_path.is_file()
                        and summary['controls'][0]['status'] == 'KILLED', marker)
        self.assertTrue(releases[0] < float(marker_path.read_text()) < acquires[1],
                        marker + ': the second host holds the lease only inside the gap')

    def test_negative_lease_bound_composition(self):
        marker = '#1402 the per-case lease bound is composed and below the stale-lease age'
        runner = self.load_runner()
        self.assertTrue(runner.PER_CASE_LEASE_SECONDS == runner.BUILD_SECONDS + runner.BOUND_TEST_SECONDS
                        and min(runner.BUILD_SECONDS, runner.BOUND_TEST_SECONDS, runner.SLOT_WAIT_SECONDS) > 0
                        and runner.PER_CASE_LEASE_SECONDS < c.common.MAX_AGE, marker)
        code, _summary = self._drive(runner)
        expected = {'slot': runner.SLOT_WAIT_SECONDS, 'cargo': runner.BUILD_SECONDS,
                    'bound': runner.BOUND_TEST_SECONDS, 'negative': runner.BOUND_TEST_SECONDS}
        self.assertTrue(code == 0 and all(timeout == expected[stage]
                                          for stage, timeout in self.timeouts), marker)

    def test_negative_lease_released_on_case_failure(self):
        marker = '#1402 a failed case releases the lease and the next case continues'
        runner = self.load_runner()

        def effect(stage):
            return subprocess.TimeoutExpired(['python'], 300) if stage == 'negative' else None

        code, summary = self._drive(runner, run_effect=effect)
        expected = ['positive', 'slot', 'acquire', 'cargo', 'negative', 'release', 'restored']
        self.assertTrue(code == 1 and self.events == expected * 2, marker)
        self.assertTrue(all(row['status'] == 'INVALID' for row in summary['controls'])
                        and all('timed out' in row['reason'] for row in summary['controls']), marker)
        self.assertTrue(self.events.count('release') == self.events.count('acquire') == 2, marker)

    def test_negative_lease_released_and_loud_on_cancellation(self):
        marker = '#1402 a cancelled case must release the parity lease and stay loud'
        runner = self.load_runner()

        def effect(stage):
            return KeyboardInterrupt('#1402 injected cancellation') if stage == 'negative' else None

        with self.assertRaises(KeyboardInterrupt, msg=marker):
            self._drive(runner, run_effect=effect)
        expected = ['positive', 'slot', 'acquire', 'cargo', 'negative', 'release']
        self.assertTrue(self.events == expected, marker + ': the lease must be released on the way out')
        summary = json.loads((self.out / 'summary.json').read_text())
        self.assertTrue(all(row['status'] == 'INVALID' for row in summary['controls'])
                        and len(summary['controls']) == 2, marker)
        self.assertTrue(self.source.read_text() == self.PRISTINE,
                        marker + ': the cancelled case must restore the mutated bytes')

    def test_negative_lease_not_taken_before_slot(self):
        marker = '#1402 the lease is never taken while waiting for a cargo slot'
        runner = self.load_runner()
        code, summary = self._drive(runner,
                                     slot_error=RuntimeError('#1402 no cargo slot'))
        self.assertTrue(code == 1 and 'acquire' not in self.events
                        and 'cargo' not in self.events and 'negative' not in self.events, marker)
        self.assertTrue(all(row['status'] == 'INVALID' for row in summary['controls']), marker)

    def test_negative_chain_denominators_derived(self):
        marker = '#1402 chain denominators derive from the calibration manifest'
        path = Path(__file__).resolve().parent.parent / 'native-witness/test_chain.py'
        try:
            spec = importlib.util.spec_from_file_location('chain_denominators', path)
            module = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(module)
        except Exception as error:
            self.fail(marker + ': ' + str(path) + ': ' + repr(error))
        suite = unittest.defaultTestLoader.loadTestsFromNames([
            'ChainTests.test_provider_case_missing',
            'ChainTests.test_provider_optional_case_missing',
            'ChainTests.test_no_provider_traffic_is_scored'], module)
        log = io.StringIO()
        result = unittest.TextTestRunner(stream=log, verbosity=2).run(suite)
        self.assertTrue(result.wasSuccessful(), marker + ': ' + log.getvalue())


if __name__ == '__main__':
    unittest.main(verbosity=2)
