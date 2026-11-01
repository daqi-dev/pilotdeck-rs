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
    def _collect(cls, output, *, real_settle=False, after_final=None):
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
        self.assertTrue(len(verdicts) == 72
                        and [row['status'] for row in verdicts] == ['PASS'] * 72,
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
                        and [row['afterExecs'] for row in witness['opens']] == list(range(1, 73)),
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
        # the rewritten rows, so the driver's own hello record is the anchor.
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
                        and len(cell['cases']) == 72 and cell['executedCases'] == cell['expectedCases'], marker)

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
        self.assertTrue(len(cell['executedCases']) == 71,
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
        self.assertTrue(len(cell['executedCases']) == 71,
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


if __name__ == '__main__':
    unittest.main(verbosity=2)
