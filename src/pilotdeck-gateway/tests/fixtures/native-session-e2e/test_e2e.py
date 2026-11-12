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
import errno
import fcntl
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import socket
import stat
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

def _port_lease_dir():
    """#1510 the lease directory: fixed absolute path, one per uid.

    Deliberately independent of ``$TMPDIR``: two parallel copies started with
    different TMPDIR values would otherwise lease different files for the same
    port and silently stop excluding each other exactly inside the closed-guard
    handoff window.  The directory is 0700 and must be owned by this uid, so a
    foreign file cannot make a candidate look leased.
    """
    owner = os.getuid() if hasattr(os, 'getuid') else 0
    directory = Path('/tmp') / f'pdrs-e2e-port-leases-{owner}'
    directory.mkdir(mode=0o700, exist_ok=True)
    info = os.lstat(directory)
    # lstat + S_ISDIR, not os.path.isdir: the latter follows a symlink, so a
    # link owned by this uid would pass while the leases landed elsewhere.
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != owner:
        raise RuntimeError(f'#1510 the port-lease directory {directory} is not a directory '
                           f'owned by uid {owner}')
    # mkdir(mode=0o700) only applies when the directory is created; a leftover
    # directory with looser bits would let another uid plant files in it, which
    # then read as "port busy" below.  Self-heal instead of trusting the mode.
    if stat.S_IMODE(info.st_mode) & 0o077:
        os.chmod(directory, 0o700)
        info = os.lstat(directory)
        if stat.S_IMODE(info.st_mode) & 0o077:
            raise RuntimeError(f'#1510 the port-lease directory {directory} is not private '
                               f'(mode {oct(stat.S_IMODE(info.st_mode))})')
    return directory


def _port_lease_path(port):
    """#1510 the per-port lease file; never removed (unlink would trade the
    inode for a fresh open race between holders)."""
    return _port_lease_dir() / f'pdrs-e2e-port-{port}.lock'


def _acquire_port_lease(port):
    """#1510 take this port's cross-process lease, or None when a sibling holds it.

    flock dies with its holder, so a crashed suite never leaves a stale lease.
    Only "somebody else holds it / it is not ours" means the candidate is busy;
    any other errno (unsupported filesystem, EMFILE, ...) is an environment
    failure and must surface instead of masquerading as port exhaustion.
    """
    # #1510 r4: an unopenable lease path is NOT "this port is busy".  With a
    # 0700 directory owned by this uid, EACCES/EPERM/ELOOP mean the lease tree
    # itself is broken (a symlink or a foreign file); folding that into "busy"
    # made a refused scan look like proof of exclusion, so let it surface.
    lease = os.open(_port_lease_path(port),
                    os.O_CREAT | os.O_RDWR | getattr(os, 'O_NOFOLLOW', 0)
                    | getattr(os, 'O_CLOEXEC', 0), 0o600)
    try:
        fcntl.flock(lease, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError as error:
        os.close(lease)
        if error.errno in (errno.EACCES, errno.EAGAIN, errno.EWOULDBLOCK):
            return None
        raise
    try:
        # #1510 r2: never unlinking only constrains *this* code.  An external
        # /tmp cleaner ages entries by their times (Linux systemd-tmpfiles
        # defaults to 10 days; macOS 26 no longer ships periodic(8)) and could
        # unlink a lease file a sibling still holds, after which the next
        # opener leases a fresh inode for the same port and the closed-guard
        # handoff window is open again.  Refreshing the times while holding
        # the lock keeps a live lease out of a cleaner's reach.
        os.utime(lease, None)
    except OSError:
        pass
    return lease


def _free_private_pair():
    """#1510 a port pair nobody leases right now, outside the scan range.

    The starting point is derived from the pid so parallel copies usually
    disagree, but pid values mod 10000 are not unique: probing both leases
    skips a pair a sibling already holds instead of turning this probe into a
    false FAIL about leases that were in fact released.
    """
    base = 40000 + (os.getpid() % 10000) * 2
    for offset in range(0, 2000, 2):
        candidate = 40000 + ((base - 40000) + offset) % 20000
        gateway = _acquire_port_lease(candidate)
        if gateway is None:
            continue
        peer = _acquire_port_lease(candidate + 1)
        if peer is None:
            os.close(gateway)
            continue
        os.close(peer)
        os.close(gateway)
        return candidate
    raise AssertionError('#1510 no unleased port pair outside the scan range')


# #1510 concurrent sibling scanners transiently hold candidate leases and
# guards while walking the same range; one pass can false-exhaust although a
# pair frees up milliseconds later.  Bounded retries absorb that transient
# scan interference without masking exhaustion: they do NOT wait out a sibling
# that holds a pair for a whole collection (leases live as long as the
# collection), so a genuinely busy range still fails with the reason map after
# this budget.  Total budget: 7 waits x 0.05 s = 0.35 s across 8 passes.
PORT_SCAN_PASSES = 8
PORT_SCAN_PASS_SECONDS = 0.05


def _scan_port_pair_once(candidates):
    """One reservation pass: ((port, guards, leases) or None, blocked reasons)."""
    blocked = []
    for candidate in candidates:
        gateway_lease = _acquire_port_lease(candidate)
        if gateway_lease is None:
            blocked.append(f'{candidate}:lease')
            continue
        try:
            provider_lease = _acquire_port_lease(candidate + 1)
        except BaseException:
            # #1510 r2: only a held peer lease returns None.  Every other errno
            # (EMFILE, an unsupported filesystem) and a rejected lease
            # directory raise instead, and the gateway lease must not outlive
            # that: it would shrink this process's range for good.
            os.close(gateway_lease)
            raise
        if provider_lease is None:
            os.close(gateway_lease)
            blocked.append(f'{candidate}:peer-lease')
            continue
        try:
            gateway_guard = socket.socket()
        except OSError:
            # #1510 an fd exhaustion here must not leave both leases held for
            # the rest of this process (which would shrink the range forever).
            os.close(gateway_lease)
            os.close(provider_lease)
            raise
        try:
            gateway_guard.bind(('127.0.0.1', candidate))
        except (OSError, OverflowError) as error:
            # Same shape as the provider bind below: an out-of-range candidate
            # raises OverflowError, not OSError, and must not leak the guard or
            # the two leases.
            gateway_guard.close()
            os.close(gateway_lease)
            os.close(provider_lease)
            blocked.append(f'{candidate}:gateway-bind:{getattr(error, "errno", error)}')
            continue
        port = gateway_guard.getsockname()[1]
        try:
            provider_guard = socket.socket()
        except OSError:
            gateway_guard.close()
            os.close(gateway_lease)
            os.close(provider_lease)
            raise
        try:
            provider_guard.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            provider_guard.bind(('127.0.0.1', port + 1))
        except (OSError, OverflowError) as error:
            provider_guard.close()
            gateway_guard.close()
            os.close(gateway_lease)
            os.close(provider_lease)
            blocked.append(f'{candidate}:provider-bind:{getattr(error, "errno", error)}')
            continue
        return (port, gateway_guard, provider_guard, (gateway_lease, provider_lease)), blocked
    return None, blocked


def _reserve_port_pair(candidates=range(28999, 29009), passes=PORT_SCAN_PASSES):
    """Hold both fixed-profile ports until handoff; mirror each server's bind policy.

    The gateway guard deliberately sets NO SO_REUSEADDR even though
    mock_gateway's listener does: on BSD/macOS REUSEADDR can let a second
    process bind a port that is already actively listened on, which would void
    the guard exactly when two copies race (#1510).  A guard that is stricter
    than the service may skip a TIME_WAIT port; that is the safe direction.
    The provider guard mirrors its service's REUSEADDR because macOS otherwise
    lets a REUSEADDR provider share a bound gateway port.

    The guard sockets only defend against foreign processes: once a guard is
    released for the real server's bind, an uncooperating peer can still take
    the port.  This suite's own concurrent processes (the parent class and the
    parallel-suite children) raced exactly inside those closed-guard handoff
    windows (#1510), so both ports of the pair also take a flock lease, held
    for the whole collection.  Both, not just the gateway candidate: the
    provider port is the next sibling's gateway scan target and a REUSEADDR
    provider guard can share a bound gateway port on macOS.  A failed pass is
    retried (see PORT_SCAN_PASSES) because sibling scanners transiently block
    candidates; a genuinely exhausted range still fails with the reason map.
    """
    reservation, blocked = None, []
    for attempt in range(passes):
        reservation, blocked = _scan_port_pair_once(candidates)
        if reservation is not None:
            return reservation
        if attempt + 1 < passes:
            time.sleep(PORT_SCAN_PASS_SECONDS)
    raise AssertionError('#1510 e2e: no free gateway/provider port pair ('
                         + ', '.join(blocked) + ')')


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

        cls.port, gateway_guard, provider_guard, port_leases = _reserve_port_pair()
        try:
            # The gateway slot is handed over to Gateway's own bind here; the
            # provider slot only when _native_run is about to bind it.  The
            # #1510 port leases stay held across both handoff windows.
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
                                  mock.patch.object(c, 'SESSION_PROVIDER_DELAY', 0.0),
                                  # #2396: this mock gateway replays registered
                                  # output frames but never publishes the s2-s4
                                  # product witness receipts, so every
                                  # receipt-gated case would otherwise burn the
                                  # stimulus driver's full bounded waits. The
                                  # real collection keeps the generous bounds;
                                  # here only the budget is shrunk.
                                  mock.patch.object(c.stimulus_driver, 'CASE_BUDGET_SECONDS', 1.0),
                                  mock.patch.object(c.stimulus_driver, 'MILESTONE_SECONDS', 0.2)):
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
            for lease in port_leases:
                os.close(lease)

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

    def test_memory_rehearsal_holds_tree_lock(self):
        marker = '#1398 e2e memory rehearsal holds the source tree lock'
        path = Path(__file__).with_name('negative_controls.py')
        try:
            spec = importlib.util.spec_from_file_location('e2e_memory_lock', path)
            runner = importlib.util.module_from_spec(spec)
            exec(compile(path.read_bytes(), str(path), 'exec'), runner.__dict__)
            seen, order = [], []

            class recording_lock:
                def __enter__(self):
                    order.append('lock')

                def __exit__(self, *_exc):
                    return None

            def recording_tree_lock(*arguments, **keywords):
                seen.append((arguments, keywords))
                return recording_lock()

            def fake_run_owned(command, **_keywords):
                order.append('child')
                return subprocess.CompletedProcess(command, 0, '', 'Ran 1 test in 0.001s\n\nOK\n')

            previous_target = os.environ.get('CARGO_TARGET_DIR')
            if previous_target is None:
                os.environ['CARGO_TARGET_DIR'] = str(Path(__file__).resolve().parents[5] / '_build/cargo-target')
            try:
                with tempfile.TemporaryDirectory(prefix='pdrs-1398-e2e-') as folder:
                    with mock.patch.object(runner, 'tree_lock', recording_tree_lock), \
                            mock.patch.object(runner, 'run_owned', fake_run_owned), \
                            mock.patch('sys.stdout', new_callable=io.StringIO), \
                            mock.patch.object(sys, 'argv', [str(path), str(Path(folder) / 'out'), '--memory',
                                                           '--case', 'memory-rehearsal-without-lock']):
                        runner.main()
            finally:
                if previous_target is None:
                    del os.environ['CARGO_TARGET_DIR']
            self.assertTrue(seen == [((runner.root,), {'timeout': 900})]
                            and order[:1] == ['lock'] and 'child' in order, marker)
        except (Exception, SystemExit) as error:
            self.fail(marker + ': ' + repr(error))

    def test_memory_rehearsal_preflights_anchors_inside_the_lock(self):
        # #2795 fix-r13（评审修 4）：--memory 的全表锚点预检（fix-r12 折进
        # rehearsal 锁窗的那一块）此前零钉子——删掉 one() 里的预检块，锁窗/
        # 取锁次数契约不红，预检被挪出锁窗（回到 #2795 fix-r4 的缺陷形状）
        # 三重机检都抓不到。本测试给预检加可观测事件，断言「恰一次、在锁
        # 内、先于任何 child」。
        marker = '#2795 memory rehearsal preflights the whole anchor table inside the lock, before any child'
        path = Path(__file__).with_name('negative_controls.py')
        try:
            spec = importlib.util.spec_from_file_location('e2e_memory_precheck', path)
            runner = importlib.util.module_from_spec(spec)
            exec(compile(path.read_bytes(), str(path), 'exec'), runner.__dict__)
            seen, order = [], []

            class recording_lock:
                def __enter__(self):
                    order.append('lock')

                def __exit__(self, *_exc):
                    return None

            def recording_tree_lock(*arguments, **keywords):
                seen.append((arguments, keywords))
                return recording_lock()

            def fake_errors():
                order.append('precheck')
                return []

            def fake_report(errors):
                order.append('report')

            def fake_run_owned(command, **_keywords):
                order.append('child')
                return subprocess.CompletedProcess(command, 0, '', 'Ran 1 test in 0.001s\n\nOK\n')

            previous_target = os.environ.get('CARGO_TARGET_DIR')
            if previous_target is None:
                os.environ['CARGO_TARGET_DIR'] = str(Path(__file__).resolve().parents[5] / '_build/cargo-target')
            try:
                with tempfile.TemporaryDirectory(prefix='pdrs-2795-e2e-precheck-') as folder:
                    with mock.patch.object(runner, 'tree_lock', recording_tree_lock), \
                            mock.patch.object(runner, 'run_owned', fake_run_owned), \
                            mock.patch.object(runner, '_anchor_errors', fake_errors), \
                            mock.patch.object(runner, '_anchor_report', fake_report), \
                            mock.patch('sys.stdout', new_callable=io.StringIO), \
                            mock.patch.object(sys, 'argv', [str(path), str(Path(folder) / 'out'), '--memory']):
                        runner.main()
            finally:
                if previous_target is None:
                    del os.environ['CARGO_TARGET_DIR']
            self.assertTrue(seen == [((runner.root,), {'timeout': 900})]
                            and order[:3] == ['lock', 'precheck', 'report']
                            and order.count('precheck') == 1
                            and order.index('precheck') < order.index('child'), marker)
        except (Exception, SystemExit) as error:
            self.fail(marker + ': ' + repr(error))

    def test_memory_rehearsal_refuses_stale_anchor_table(self):
        # #2795 fix-r13（评审修 3②/③ + 修 4「删块即红」负控）：--memory 启动期
        # 撞上过期锚点表是拒跑（rc 2、锚点错误行上 stderr、不发布 summary、
        # 撤走 .claim 认领目录、任何 child 未起），不是 rc 1 + 一份看起来跑完
        # 了的全 INVALID summary。删掉 one() 的预检块或撤掉 main 的拒跑分支
        # 后本测试转红。
        marker = '#2795 memory rehearsal refuses a stale anchor table before any child runs'
        path = Path(__file__).with_name('negative_controls.py')
        try:
            spec = importlib.util.spec_from_file_location('e2e_memory_stale', path)
            runner = importlib.util.module_from_spec(spec)
            exec(compile(path.read_bytes(), str(path), 'exec'), runner.__dict__)
            # The stale bytes are built by concatenation so the literal never
            # appears in this file (a self-anchoring row would count 1 and the
            # precheck would pass); the target is the native evidence script,
            # never this test file itself.
            stale = [('stale-anchor-probe', runner.native,
                      'r13 stale anchor probe bytes ' + 'that appear nowhere', 'x',
                      'test_memory_rehearsal_holds_tree_lock')]
            launched = []

            class recording_lock:
                def __enter__(self):
                    pass

                def __exit__(self, *_exc):
                    return None

            def fake_run_owned(command, **_keywords):
                launched.append(list(command))
                return subprocess.CompletedProcess(command, 0, '', 'Ran 1 test in 0.001s\n\nOK\n')

            previous_target = os.environ.get('CARGO_TARGET_DIR')
            if previous_target is None:
                os.environ['CARGO_TARGET_DIR'] = str(Path(__file__).resolve().parents[5] / '_build/cargo-target')
            try:
                with tempfile.TemporaryDirectory(prefix='pdrs-2795-e2e-stale-') as folder:
                    out = Path(folder) / 'out'
                    with mock.patch.object(runner, 'tree_lock',
                                           lambda *_a, **_k: recording_lock()), \
                            mock.patch.object(runner, 'run_owned', fake_run_owned), \
                            mock.patch.object(runner, 'cases', stale), \
                            mock.patch.object(runner, 'messages',
                                               {'stale-anchor-probe': 'unused'}), \
                            mock.patch('sys.stdout', new_callable=io.StringIO), \
                            mock.patch('sys.stderr', new_callable=io.StringIO) as stderr, \
                            mock.patch.object(sys, 'argv', [str(path), str(out), '--memory',
                                                           '--case', 'stale-anchor-probe']):
                        try:
                            runner.main()
                            raised = None
                        except SystemExit as exit_code:
                            raised = exit_code
                    self.assertTrue(raised is not None and raised.code == 2
                                    and not launched
                                    and 'STALE ANCHOR stale-anchor-probe' in stderr.getvalue()
                                    and not (out / 'summary.json').exists()
                                    and not out.exists(), marker)
            finally:
                if previous_target is None:
                    del os.environ['CARGO_TARGET_DIR']
        except (Exception, SystemExit) as error:
            self.fail(marker + ': ' + repr(error))

    def test_memory_rehearsal_refuses_a_busy_tree_lock(self):
        # #2795 fix-r13（评审修 3①）：兄弟变异运行器持锁窗口（cargo no-run
        # ≤900 + bound 300 + restored 300 ≈ 1500 s）长于 900 s 取锁上限时，
        # 忙锁是拒跑（rc 2 + 家族文案 + 不发布 summary + 撤走认领目录），
        # 不是 rc 1 + 全 INVALID summary（#2431 家族形状）。
        marker = '#2795 memory rehearsal refuses a busy tree lock with rc 2 and no evidence'
        path = Path(__file__).with_name('negative_controls.py')
        try:
            spec = importlib.util.spec_from_file_location('e2e_memory_busy', path)
            runner = importlib.util.module_from_spec(spec)
            exec(compile(path.read_bytes(), str(path), 'exec'), runner.__dict__)
            launched = []

            class refusing_lock:
                def __enter__(self):
                    raise TimeoutError('probe: source lock busy')

                def __exit__(self, *_exc):
                    return None

            def fake_run_owned(command, **_keywords):
                launched.append(list(command))
                return subprocess.CompletedProcess(command, 0, '', 'Ran 1 test in 0.001s\n\nOK\n')

            previous_target = os.environ.get('CARGO_TARGET_DIR')
            if previous_target is None:
                os.environ['CARGO_TARGET_DIR'] = str(Path(__file__).resolve().parents[5] / '_build/cargo-target')
            try:
                with tempfile.TemporaryDirectory(prefix='pdrs-2795-e2e-busy-') as folder:
                    out = Path(folder) / 'out'
                    with mock.patch.object(runner, 'tree_lock',
                                           lambda *_a, **_k: refusing_lock()), \
                            mock.patch.object(runner, 'run_owned', fake_run_owned), \
                            mock.patch('sys.stdout', new_callable=io.StringIO), \
                            mock.patch('sys.stderr', new_callable=io.StringIO) as stderr, \
                            mock.patch.object(sys, 'argv', [str(path), str(out), '--memory',
                                                           '--case', 'memory-rehearsal-without-lock']):
                        try:
                            runner.main()
                            raised = None
                        except SystemExit as exit_code:
                            raised = exit_code
                    self.assertTrue(raised is not None and raised.code == 2
                                    and not launched
                                    and 'tree lock or anchor read failed: TimeoutError' in stderr.getvalue()
                                    and not (out / 'summary.json').exists()
                                    and not out.exists(), marker)
            finally:
                if previous_target is None:
                    del os.environ['CARGO_TARGET_DIR']
        except (Exception, SystemExit) as error:
            self.fail(marker + ': ' + repr(error))


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
        port, gateway_guard, provider_guard, port_leases = reserve()
        gateway_guard.close()
        provider_guard.close()
        for lease in port_leases:
            os.close(lease)
        try:
            with mock.patch.dict(globals(), _reserve_port_pair=lambda: reserve([port])):
                harness.setUpClass()
                harness._collect(harness.folder / 'second-capture')
                pair = reserve([port])
                pair[1].close()
                pair[2].close()
                for lease in pair[3]:
                    os.close(lease)
            self.assertTrue(not c.common.LIVE_GROUPS, marker)
        except Exception as error:
            self.fail(marker + ': ' + repr(error))
        finally:
            harness.doClassCleanups()

    def test_failed_reservation_closes_both_sockets(self):
        marker = '#1140 r9 failed reservation closes both guards'
        # #1510 r2: a pair outside the scan range, free *right now*.  A bare
        # pid-derived pair collides whenever two parallel copies draw congruent
        # pids, and a held pair would surface as "the leases were not released"
        # -- a false FAIL for a test that pins the opposite.
        pair = _free_private_pair()
        made = []

        def fake_socket():
            guard = mock.Mock()
            if not len(made) % 2:
                # gateway guard of each pass
                guard.getsockname.return_value = ('127.0.0.1', pair)
            else:
                # every pass fails the provider bind, so the retries exhaust too
                guard.bind.side_effect = OSError('occupied fixture port')
            made.append(guard)
            return guard

        with mock.patch.object(socket, 'socket', side_effect=fake_socket):
            with self.assertRaises(AssertionError, msg=marker):
                _reserve_port_pair([pair], passes=3)
        self.assertTrue(made and all(guard.close.called for guard in made), marker)
        self._assert_pair_releasable(pair)

        # #1510 r2: the other two failure branches leak just as easily and had
        # no control at all -- a gateway bind that fails (`blocked`, not raise)
        # and socket creation itself (EMFILE re-raises out of the scan).
        def failing_gateway_socket():
            guard = mock.Mock()
            guard.bind.side_effect = OSError(errno.EADDRINUSE, 'gateway bind failed')
            return guard

        with mock.patch.object(socket, 'socket', side_effect=failing_gateway_socket):
            with self.assertRaises(AssertionError, msg=marker):
                _reserve_port_pair([pair], passes=2)
        self._assert_pair_releasable(pair)

        with mock.patch.object(socket, 'socket',
                               side_effect=OSError(errno.EMFILE, 'too many open files')):
            with self.assertRaises(OSError, msg=marker):
                _reserve_port_pair([pair], passes=1)
        self._assert_pair_releasable(pair)

        # And a peer lease that raises instead of reporting "busy" (EMFILE, an
        # unsupported filesystem) must release the gateway lease first.
        real_acquire, calls = _acquire_port_lease, []

        def raising_peer(port):
            calls.append(port)
            if len(calls) % 2 == 0:
                raise OSError(errno.EMFILE, 'too many open files')
            return real_acquire(port)

        with mock.patch.dict(globals(), {'_acquire_port_lease': raising_peer}):
            with self.assertRaises(OSError, msg=marker):
                _reserve_port_pair([pair], passes=1)
        self._assert_pair_releasable(pair)

    def _assert_pair_releasable(self, pair):
        # #1510 a failed reservation must not leave either port leased: both
        # must be immediately re-leasable from this process (flock is per open
        # file description on macOS and Linux, probed by this acquisition).
        for port in (pair, pair + 1):
            lease = _acquire_port_lease(port)
            try:
                self.assertIsNotNone(
                    lease, '#1510 a failed reservation releases both port leases (' + str(port) + ')')
            finally:
                if lease is not None:
                    os.close(lease)

    def test_port_reservation_is_exclusive_across_processes(self):
        marker = '#1510 the closed-guard handoff window stays exclusive via the port lease'
        port, gateway_guard, provider_guard, port_leases = _reserve_port_pair()
        # #1510 r3: the lease location is part of the contract, not an
        # implementation detail.  Deriving it from $TMPDIR (or from
        # tempfile.gettempdir()) makes two copies with different TMPDIR values
        # lease different files for the same port and reopens the handoff
        # window this issue is about -- pin it so that change cannot pass.
        self.assertEqual(str(_port_lease_dir()),
                         '/tmp/pdrs-e2e-port-leases-' + str(os.getuid()),
                         '#1510 the port-lease directory must not follow $TMPDIR')
        # Replicate the handoff window: both guards are released for the real
        # servers' binds, so only the port leases still defend the pair.  A
        # sibling process reserving now must not pick this pair.  The child is
        # handed exactly this pair as its only candidates, so the verdict never
        # depends on which unrelated ports happen to be free in this run: a
        # full default range let a lucky lower free pair hide a dropped lease.
        gateway_guard.close()
        provider_guard.close()
        try:
            child = subprocess.run(
                # -B: the child must not drop .pyc of a mutated fixture into the
                # guarded source tree.  timeout stays well below the negative
                # control's outer bound (BOUND_TEST_SECONDS = 300), otherwise the
                # outer kill fires first and this TimeoutExpired branch is dead:
                # the control would read INVALID instead of KILLED.
                [sys.executable, '-I', '-B', '-c',
                 'import importlib.util, os, sys\n'
                 'spec = importlib.util.spec_from_file_location("e2e_port_lease_child", sys.argv[1])\n'
                 'module = importlib.util.module_from_spec(spec)\n'
                 'spec.loader.exec_module(module)\n'
                 'base = int(sys.argv[2])\n'
                 'port, gateway_guard, provider_guard, port_leases = '
                 'module._reserve_port_pair(range(base, base + 2))\n'
                 'gateway_guard.close()\n'
                 'provider_guard.close()\n'
                 'for lease in port_leases:\n'
                 '    os.close(lease)\n'
                 'print(port)\n',
                 str(Path(__file__).resolve()), str(port)],
                capture_output=True, text=True, timeout=120)
        except subprocess.TimeoutExpired:
            # No marker here on purpose: the negative controls classify a run as
            # KILLED when the assertion line carries this marker, so an
            # infrastructure hang must NOT reuse it (#1510 r3).  classify() then
            # reports INVALID instead of a false "the mutant was caught".
            self.fail('#1510 the sibling reservation did not finish in time '
                      '(infrastructure, not a mutant verdict)')
        finally:
            for lease in port_leases:
                os.close(lease)
        if child.returncode == 0:
            # The sibling must avoid the whole leased pair, not just the
            # gateway slot: the provider port (+1) is the next sibling's scan
            # target, so an overlapping pair still reopens the handoff window.
            sibling = int(child.stdout.strip())
            self.assertFalse(sibling in (port, port + 1) or sibling + 1 in (port, port + 1),
                             marker + ': the sibling picked the leased pair '
                             + str(port) + '/' + str(port + 1) + ' inside the handoff window'
                             + ' (it picked ' + str(sibling) + '/' + str(sibling + 1) + ')')
        else:
            # Refused: the refusal must come from our leases.  A bind error
            # would mean some unrelated process held the ports, which proves
            # nothing about the handoff window.
            log = child.stdout + child.stderr
            # #1510 r4: a crash (ImportError, EMFILE, a rejected lease
            # directory) also exits non-zero, and the assertions below would
            # then fail WITH the marker -- measured: the real classify() turns
            # such a log into KILLED, i.e. "the mutant was caught" for a mutant
            # nothing ever exercised.  So the exclusion evidence is checked
            # first, without the marker: the scan must have exhausted because
            # BOTH ports reported the lease reason.  An exhaustion report that
            # names a bind error instead means an unrelated process held the
            # ports, which proves nothing about the handoff window (r5: the
            # bare exhaustion check let that case fall through to the marked
            # assertions below and still grade KILLED).
            excluded = ('#1510 e2e: no free gateway/provider port pair (' in log
                        and str(port) + ':lease' in log
                        and str(port + 1) + ':lease' in log)
            if not excluded:
                self.fail('#1510 the sibling reservation failed for an infrastructure '
                          'reason, not because the pair was leased (no mutant verdict): '
                          + log.replace('\n', ' ')[-400:])
            self.assertIn(str(port) + ':lease', log,
                          marker + ': the sibling was not excluded by the gateway lease: ' + log)
            self.assertIn(str(port + 1) + ':lease', log,
                          marker + ': the sibling was not excluded by the provider lease: ' + log)

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
        port, first, second, port_leases = reserve()
        first.close()
        second.close()
        for lease in port_leases:
            os.close(lease)
        try:
            with mock.patch.dict(globals(), _reserve_port_pair=lambda: reserve([port])), \
                    mock.patch.object(c, 'validate_session', side_effect=c.w.Incomplete('injected validation failure')):
                with self.assertRaises(c.w.Incomplete, msg=marker):
                    harness.setUpClass()
            pair = reserve([port])
            pair[1].close()
            pair[2].close()
            for lease in pair[3]:
                os.close(lease)
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
            # positive precedes the lease (or follows the previous case's
            # release); restored now runs inside the lease, right after the
            # negative control -- a plain string without a lease, a
            # timestamped tuple when the real lease protocol records the event.
            last = events[-1] if events else None
            stage = 'restored' if (last in ('cargo', 'negative') or
                                   (isinstance(last, tuple) and last[0] == 'acquire')) else 'positive'
            events.append(stage)
            disks.append((stage, self.source.read_text()))
            return real_passed(result)

        def slot(_script, **kwargs):
            if slot_error is not None:
                raise slot_error
            events.append('slot')
            timeouts.append(('slot', kwargs.get('timeout')))
            # The between-case gap a second host must win: slot runs outside
            # the lease, between the previous release and the next acquire.
            if gap:
                time.sleep(gap)
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
        expected = ['positive', 'slot', 'acquire', 'cargo', 'negative', 'restored', 'release']
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
        self.assertTrue(runner.PER_CASE_LEASE_SECONDS == runner.BUILD_SECONDS + 2 * runner.BOUND_TEST_SECONDS
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
        # The negative stage timed out: the exception propagates out of the
        # guarded window before the restored control, which stays inside the
        # lease exactly like the other guarded runners.
        expected = ['positive', 'slot', 'acquire', 'cargo', 'negative', 'release']
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
