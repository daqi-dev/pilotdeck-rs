# anchor-selfcheck: unsupported end-to-end capture run, no mutation table
#!/usr/bin/env python3
"""#1140 fix-r1 end-to-end: the collector's real output scores per case.

Runs the real session-domain collection (``collect._native_run``) against a
real-socket mock gateway (``mock_gateway.py``), with only the OS layer mocked
(strace, pidfd, platform) and the settle/provider delays shortened, then loads
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
import tempfile
import time
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[5]
spec = importlib.util.spec_from_file_location('witness_collector', ROOT / 'scripts/native-witness/collect.py')
c = importlib.util.module_from_spec(spec)
spec.loader.exec_module(c)
sys.path.insert(0, str(Path(__file__).resolve().parent))
from mock_gateway import Gateway, load_expectations

# Ports outside the fixed 28999..29008 collection series: this offline run must
# never contend with a real collection's gateway or provider endpoint.
PORT = 28997


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
        cls.folder = Path(cls.temp.name)
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

    @classmethod
    def tearDownClass(cls):
        cls.temp.cleanup()

    # ---- the real collection, OS layer mocked ------------------------------
    @classmethod
    def _trace(cls, gateway_argv):
        host = f'100 execve({hex_string(gateway_argv[0])}, ' \
               f'[{", ".join(map(hex_string, gateway_argv))}], 0x1 /* 0 vars */) = 0\n'
        sidecar = f'102 execve({hex_string(str(cls.sidecar))}, ' \
                  f'[{hex_string(str(cls.sidecar))}], 0x1 /* 0 vars */) = 0\n'
        return (host
                + '100 clone(child_stack=NULL, flags=CLONE_VM|CLONE_VFORK|SIGCHLD) = 102\n'
                + sidecar
                + '100 connect(3, {sa_family=AF_INET, sin_port=htons(' + str(PORT + 1) + '), '
                  'sin_addr=inet_addr(' + hex_string('127.0.0.1') + ')}, 16) = -1 EINPROGRESS '
                  '(Operation now in progress)\n'
                + '102 exit_group(0) = ?\n102 +++ exited with 0 +++\n'
                + '100 exit_group(0) = ?\n100 +++ exited with 0 +++\n')

    @classmethod
    def _collect(cls, output):
        tracer = mock.Mock(pid=99)
        tracer.poll.return_value = None
        tracer.wait.return_value = 0

        def spawn(argv, **kwargs):
            # The mocked strace writes the synthetic trace the recorder would
            # have produced, derived from the very argv the collector
            # registered, plus the readiness line the driver polls for.
            trace = Path(argv[argv.index('-o') + 1])
            trace.write_text(cls._trace(argv[argv.index('--') + 1:]))
            stderr = kwargs.get('stderr')
            if stderr is not None:
                stderr.write(f'native listening 127.0.0.1:{PORT}\n'.encode())
                stderr.flush()
            return tracer

        gateway = Gateway(PORT, output / 'home', f'http://127.0.0.1:{PORT + 1}',
                         load_expectations(ROOT))
        try:
            with contextlib.ExitStack() as stack:
                for patch in (mock.patch.object(sys, 'platform', 'linux'),
                              mock.patch.object(c.w.shutil, 'which', return_value='/strace'),
                              mock.patch.object(c.w.subprocess, 'run',
                                                return_value=mock.Mock(stdout='--kill-on-exit')),
                              mock.patch.object(c.w.subprocess, 'Popen', side_effect=spawn),
                              mock.patch.object(c.w, 'stop_host'),
                              mock.patch.object(c, 'SESSION_SETTLE_SECONDS', 0.01),
                              mock.patch.object(c, 'SESSION_PROVIDER_DELAY', 0.0)):
                    stack.enter_context(patch)
                c._native_run(cls.host, cls.sidecar, output, PORT, session_domain=True)
        finally:
            cls.assertTrue(gateway.close(30),
                           '#1140 e2e mock gateway served the whole manifest without errors')

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

    def test_e2e_data_control_surface_mismatch(self):
        folder = self._mutated('surface-mismatch')
        case = json.loads((self.output / 'session-domain/cases.json').read_text())['cases'][3]['case']

        def corrupt(document):
            document['cases'][3]['observation']['frames'].append(
                {'type': 'agent_status', 'event': 'forged-event'})
        self._rewrite(folder, 'session-domain/cases.json', 'casesSha256', corrupt)
        report = self._score(folder, '#1140 e2e one mutated surface fails exactly that case')
        failures = [row for row in self._native_cell(report)['cases'] if row['status'] == 'FAIL']
        self.assertTrue(len(failures) == 1 and failures[0]['case'] == case
                        and failures[0]['rule'] == 'EXPECTATION_MISMATCH'
                        and failures[0]['surfaces'] == ['frames'],
                        '#1140 e2e one mutated surface fails exactly that case')

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
        folder = self._mutated('empty-ledger')

        def corrupt(document):
            document['requests'] = []
        self._rewrite(folder, 'session-domain/provider.json', 'providerSha256', corrupt)
        frozen = load_frozen()
        try:
            frozen.deadline_report(native_capture=str(folder))
        except frozen.EvidenceError as error:
            self.assertTrue(error.rule == 'NETWORK_WITNESS',
                            '#1140 e2e an empty provider ledger is rejected: ' + str(error))
            return
        except Exception as error:
            self.assertTrue(False, '#1140 e2e an empty provider ledger is rejected (unexpected '
                            + type(error).__name__ + ': ' + str(error) + ')')
            return
        self.assertTrue(False, '#1140 e2e an empty provider ledger is rejected')

    def test_e2e_data_control_missing_record_state(self):
        folder = self._mutated('missing-record-state')
        rows = json.loads((self.output / 'session-domain/cases.json').read_text())['cases']
        index, case = next((position, row['case']) for position, row in enumerate(rows)
                           if row['observation']['registry'].get('applicable'))

        def corrupt(document):
            document['cases'][index]['observation']['registry']['record'].pop('state')
        self._rewrite(folder, 'session-domain/cases.json', 'casesSha256', corrupt)
        report = self._score(folder, '#1140 e2e a missing run-record state is a per-case FAIL')
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
        report = self._score(folder, '#1140 e2e an observed applicability mismatch fails that case')
        failures = [row for row in self._native_cell(report)['cases'] if row['status'] == 'FAIL']
        self.assertTrue(len(failures) == 1 and failures[0]['case'] == case
                        and 'registry.applicable' in failures[0]['surfaces'],
                        '#1140 e2e an observed applicability mismatch fails that case')


if __name__ == '__main__':
    unittest.main(verbosity=2)
