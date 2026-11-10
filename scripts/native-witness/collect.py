#!/usr/bin/env python3
"""Bounded real native gateway probe, loopback mock only; no A7 score publication."""
import sys
if not sys.flags.isolated:
    raise SystemExit('run with python3 -I')
sys.dont_write_bytecode = True
import argparse
import base64
from contextlib import ExitStack, contextmanager
import errno
import hashlib
import http.server
import importlib.util
import json
import os
from pathlib import Path
import re
import select
import signal
import socket
import subprocess
import threading
import time

spec = importlib.util.spec_from_file_location('native_witness', Path(__file__).with_name('witness.py'))
w = importlib.util.module_from_spec(spec)
spec.loader.exec_module(w)
spec = importlib.util.spec_from_file_location('native_stimulus_plan', Path(__file__).with_name('stimulus_plan.py'))
stimuli = importlib.util.module_from_spec(spec)
spec.loader.exec_module(stimuli)
spec = importlib.util.spec_from_file_location('native_stimulus_driver', Path(__file__).with_name('stimulus_driver.py'))
stimulus_driver = importlib.util.module_from_spec(spec)
spec.loader.exec_module(stimulus_driver)
# Explicit source load preserves the frozen auditor's isolated sys.path and
# records executed bytes for its existing provenance guard.
common_path = Path(__file__).resolve().parents[1] / 'gateway-parity/common.py'
spec = importlib.util.spec_from_file_location('native_witness_common', common_path)
common = importlib.util.module_from_spec(spec)
common._apparatus_source = common_path.read_bytes()
exec(compile(common._apparatus_source, str(common_path), 'exec'), common.__dict__)
sys.modules[spec.name] = common
parity_lock = common.parity_lock


class Blocked(w.Incomplete):
    """A shared lease or fixed port is unavailable; no fallback endpoint."""


@contextmanager
def lease_signals():
    """Install before acquiring the lease; restore only after its cleanup."""
    def cancel(signum, _frame):
        if isinstance(sys.exception(), KeyboardInterrupt):
            return
        raise KeyboardInterrupt(f'#1140 collection cancelled by signal {signum}')
    previous = {sig: signal.getsignal(sig) for sig in (signal.SIGINT, signal.SIGTERM)}
    try:
        for sig in previous:
            signal.signal(sig, cancel)
        yield
    finally:
        for sig, handler in previous.items():
            signal.signal(sig, handler)


@contextmanager
def fixed_port_bind():
    """Only a witness listener bind can establish fixed-port contention."""
    try:
        yield
    except OSError as error:
        if error.errno == errno.EADDRINUSE:
            raise Blocked('#1140 BLOCKED: fixed witness port occupied') from error
        raise


def probe_bind(port, *, reuse_address):
    # Tokio's gateway bind does not reuse addresses; HTTPServer does.
    # Probe with the respective policy, including during startup rechecks.
    with socket.socket() as probe:
        if reuse_address:
            probe.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        with fixed_port_bind():
            probe.bind(('127.0.0.1', port))


class BoundedHTTPServer(http.server.HTTPServer):
    shutdown_timeout = 6

    def server_bind(self):
        with fixed_port_bind():
            super().server_bind()

    def get_request(self):
        connection, address = super().get_request()
        # Covers request line and headers, before BaseHTTPRequestHandler runs.
        connection.settimeout(5)
        return connection, address

    def shutdown(self):
        # HTTPServer.shutdown itself waits without a deadline. A slow request
        # must leave cleanupPending instead of trapping cancellation forever.
        stopper = threading.Thread(target=super().shutdown, daemon=True)
        stopper.start()
        stopper.join(timeout=self.shutdown_timeout)
        w.need(not stopper.is_alive(), 'provider shutdown deadline')


class WireClient:
    # A frame cut mid-parse is resumed here (receive docstring); the class
    # default keeps the stubs that bypass __init__ working.
    _frame = None

    def __init__(self, port, deadline):
        self.deadline = deadline
        self.socket = socket.create_connection(('127.0.0.1', port), timeout=self.remaining())
        self.buffer = b''
        self.rows = []
        self._frame = None
        key = base64.b64encode(os.urandom(16)).decode()
        request = (f'GET /ws HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUpgrade: websocket\r\n'
                   f'Connection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: {key}\r\n\r\n')
        self.socket.sendall(request.encode())
        header = b''
        while not header.endswith(b'\r\n\r\n'):
            header += self.read(1)
            w.need(len(header) < 16384, 'oversized HTTP upgrade')
        accept = base64.b64encode(hashlib.sha1((key + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').encode()).digest())
        w.need(header.startswith(b'HTTP/1.1 101 ') and accept.lower() in header.lower(), 'websocket upgrade rejected')
        self.upgrade = header.decode()

    def remaining(self):
        left = self.deadline - time.monotonic()
        w.need(left > 0, 'native driver deadline')
        return left

    def read(self, count, until=None):
        """Buffer exactly `count` bytes; under `until` (a monotonic cap)
        a timeout returns None and the partial bytes stay buffered."""
        while len(self.buffer) < count:
            left = (until - time.monotonic()) if until is not None else self.remaining()
            if until is not None and left <= 0:
                # The cap expiring is a poll timeout, not the capture
                # deadline: return None and let the next poll continue.
                # Raising here aborted a whole capture because one frame
                # landed on the select/timeout boundary (#2396 fix-r2, DS
                # boundary P1 — observed live as a load flake).
                return None
            w.need(left > 0, 'native driver deadline')
            self.socket.settimeout(left)
            try:
                data = self.socket.recv(max(4096, count - len(self.buffer)))
            except socket.timeout:
                if until is not None:
                    return None
                raise
            w.need(data, 'websocket closed before final')
            self.buffer += data
        value, self.buffer = self.buffer[:count], self.buffer[count:]
        return value

    def send(self, value):
        payload = json.dumps(value, separators=(',', ':')).encode()
        size = len(payload)
        mask = os.urandom(4)
        if size < 126:
            header = bytes([0x81, 0x80 | size])
        elif size <= 65535:
            header = b'\x81\xfe' + size.to_bytes(2, 'big')
        else:
            header = b'\x81\xff' + size.to_bytes(8, 'big')
        frame = header + mask + bytes(byte ^ mask[i % 4] for i, byte in enumerate(payload))
        self.socket.settimeout(self.remaining())
        self.socket.sendall(frame)
        # The recorder contract the frozen audit reads (#1140 fix-r1): every row
        # carries wire metadata for compare.wire_frame, and a sent row's frame
        # is the payload text -- only received frames are recorded decoded.
        self.rows.append({'direction': 'send', 'wireHex': frame.hex(),
                          'wire': {'opcode': 1, 'fin': True, 'rsv': 0, 'masked': True,
                                   'lengthCode': 126 if 126 <= size <= 65535 else 127 if size > 65535 else size,
                                   'payloadLength': size},
                          'frame': payload.decode(), 'connection': 1})

    def receive(self, until=None):
        """One wire frame, or None when `until` (a monotonic cap) expires
        mid-frame. The single-argument read keeps the read(count) seam the
        authentication/r11 stubs monkeypatch (#1140/#1809).

        A frame cut mid-parse (head or extended length consumed, payload
        still short) is remembered in ``self._frame`` and completed by the
        next call — otherwise the already-consumed prefix was dropped and
        the next poll re-parsed payload bytes as a new frame head,
        permanently desynchronizing the stream (#2396 fix-r2, DS
        boundary/concurrency P1). Only a complete, parsed frame is ever
        recorded as a row.
        """
        def capped(count):
            return self.read(count) if until is None else self.read(count, until)

        frame = self._frame
        if frame is None:
            head = capped(2)
            if head is None:
                return None
            w.native.ws_profile('receive', {'opcode': head[0] & 15, 'fin': bool(head[0] & 128),
                                           'rsv': (head[0] >> 4) & 7, 'masked': bool(head[1] & 128)})
            code = head[1] & 127
            frame = {'head': head, 'code': code, 'extra': None, 'length': code}
            self._frame = frame
        if frame['extra'] is None and frame['length'] >= 126:
            extra = capped(2 if frame['length'] == 126 else 8)
            if extra is None:
                return None  # resume here on the next poll
            frame['extra'] = extra
            frame['length'] = int.from_bytes(extra, 'big')
        w.need(frame['length'] <= 4 * 1024 * 1024, 'oversized WS frame')
        payload = capped(frame['length'])
        if payload is None:
            return None
        self._frame = None
        head, code, length, extra = (frame['head'], frame['code'],
                                     frame['length'], frame['extra'] or b'')
        value = json.loads(payload)
        self.rows.append({'direction': 'receive', 'wireHex': (head + extra + payload).hex(),
                          'wire': {'opcode': 1, 'fin': True, 'rsv': 0, 'masked': False,
                                   'lengthCode': code, 'payloadLength': length},
                          'frame': value, 'connection': 1})
        w.need(isinstance(value, dict), 'received JSON frame must be an object')
        return value

    def receive_poll(self, timeout):
        """One frame if it arrives within `timeout` seconds, else None.

        The stimulus driver multiplexes the socket with the product's witness
        directories and must never block on the socket: the native final frame
        is itself gated behind held durable writes (#1370), so waiting on a
        frame while a barrier release is pending would deadlock the
        choreography. Frames already sitting in ``self.buffer`` — several
        wire frames routinely coalesce into one TCP segment, so the socket
        itself never becomes readable again until they are parsed — are
        drained first; only then is the socket selected on. Reading only after
        select fires keeps the recorder contract: only a complete, parsed
        frame is ever recorded as a row. A frame that only partially arrives
        within `timeout` returns None with its bytes left buffered — the
        poll never blocks past its own cap on the capture deadline
        (#2396 fix-r1, codex concurrency P1).
        """
        cap = time.monotonic() + timeout
        if self.buffer:
            return self.receive(until=cap)
        readable, _, _ = select.select([self.socket], [], [], timeout)
        if not readable:
            return None
        return self.receive(until=cap)


def native_run(gateway, sidecar, output, port=28999, session_domain=False, registry='on',
               ts_sidecar_baseline=None, session_cases=()):
    w.need(sys.platform == 'linux', 'Linux only; macOS eslogger + root deferred')
    w.need(28999 <= port <= 29008, 'gateway port must be in 28999 series')
    w.need(registry in ('on', 'off'), '#2396 registry capture group must be on or off')
    with lease_signals(), ExitStack() as lease:
        try:
            lease.enter_context(parity_lock(timeout=1))
        except TimeoutError as error:
            raise Blocked('#1140 BLOCKED: parity lease unavailable') from error
        # Check both fixed endpoints before launching any owned process.
        # The shared lease spans the bind handoff and provider cleanup.
        for candidate, reuse_address in ((port, False), (port + 1, True)):
            probe_bind(candidate, reuse_address=reuse_address)
        result = _native_run(gateway, sidecar, output, port, session_domain=session_domain,
                             registry=registry, ts_sidecar_baseline=ts_sidecar_baseline,
                             session_cases=session_cases)
    print(json.dumps({'status': 'WITNESS_CAPTURED', 'scoring': 'NOT_RUN',
                      'sessionDomain': bool(session_domain), 'registry': registry,
                      'output': str(Path(output).resolve())}))
    return result


def validate_smoke(contract, evidence, requests, home):
    w.need(any(p['role'] == 'sidecar' for p in evidence['processes']), 'real sidecar exec missing')
    w.need(bool(evidence['networkWitness']['calls']) and bool(requests), 'real provider egress missing')
    w.need(all(c['target'] in contract['externalEndpoints'] for c in evidence['networkWitness']['calls']), 'unexpected native egress')
    w.need(bool(list(home.rglob('*.jsonl'))), 'durable native output missing')


# #1140 session-domain collection budget: 72 cases through one gateway, each
# with a bounded durable wait and (for timeout-first variants) a delayed provider
# response, must fit one capture window far below the tracer's own limits.
SESSION_LIMIT_SECONDS = 900
SESSION_TIMEOUT_MS = 400
SESSION_ORDERING_TIMEOUT_MS = 30000
SESSION_PROVIDER_DELAY = 1.5
# #1140 settle arbitration: all 72 measured writes preceded final (p95 -10 ms).
# Ten seconds leaves ample margin while bounding stalled/missing durable writes.
SESSION_SETTLE_LIMIT_SECONDS = 10
# #2396 registry-off capture group: a cell whose frozen expectation demands an
# applicable registry can never settle that surface when the process-frozen
# switch keeps FileRunRegistry closed. The wait_observation semantics stay
# untouched; the caller just bounds those waits so the off capture still fits
# its window (the row is already honestly NOT_RUN on the construction
# receipt — this only shortens the wait before that is recorded).
SESSION_OFF_REGISTRY_SETTLE_SECONDS = 2
SESSION_SETTLE_INTERVAL_SECONDS = 0.05
# RunRegistry.append permits failed -> completed; failed needs the final
# error frame as well as a persisted error result (see wait_observation).
SESSION_TERMINAL_STATES = frozenset(('completed', 'aborted', 'interrupted'))
# #1363 (#7): durable terminal labels for inapplicable-registry cases. This set
# is asserted equal to the frozen fixture's registered durableStatus label
# family in session_expectations(); a fixture that grows a label fails closed
# instead of silently stranding those cases at NOT_RUN.
SESSION_TERMINAL_DURABLE_EVENTS = frozenset(('turn_timeout', 'turn_failed', 'gateway_submit_failed'))
SESSION_ORDERING_VARIANTS = stimuli.ORDERS  # legacy transport timing only


def session_manifest():
    """The frozen 72-case E1 manifest the native capture must declare in full."""
    path = Path(__file__).resolve().parents[2] / 'scripts/gateway-parity/fixtures/session-calibration.json'
    # #1363: same fail-closed contract as session_expectations(); a truncated or
    # non-object frozen input must not escape as JSONDecodeError/KeyError/TypeError.
    try:
        with open(path, encoding='utf-8') as stream:
            document = json.load(stream)
    except ValueError as error:
        raise w.Incomplete('#1140 session manifest is not readable JSON: ' + repr(error)) from error
    cases = document.get('cases') if isinstance(document, dict) else None
    w.need(isinstance(cases, list) and bool(cases), 'session manifest is empty or malformed')
    w.need(all(isinstance(case, str) for case in cases), 'session manifest case must be a string')
    w.need(len(set(cases)) == len(cases), 'session manifest has duplicates')
    w.need([item['case'] for item in stimuli.plans()] == cases,
           '#1368 stimulus plans must cover the frozen manifest exactly in order')
    return cases


def session_expectations():
    """The registered per-case rust expectations, validated before use.

    #1363 (#7, #10): the fixture is read as UTF-8, must be a non-empty table of
    unique string case keys, and every ts/rust side must carry the three fields
    the replay consumer dereferences (``frames`` list of non-empty strings,
    ``registry.applicable`` boolean, ``durableStatus`` list of strings). Its
    durable label family must equal SESSION_TERMINAL_DURABLE_EVENTS. A fixture
    that drifts fails closed here instead of silently collapsing duplicate
    keys, crashing on an unknown manifest case, or stranding
    inapplicable-registry cases at NOT_RUN.
    """
    path = Path(__file__).resolve().parents[1] / 'gateway-parity/fixtures/session-expectations.json'
    try:
        document = json.loads(path.read_text(encoding='utf-8'))
    except ValueError as error:
        # A truncated or zero-byte fixture must fail closed like any other
        # structural drift: raise the same Incomplete the w.need checks raise
        # (and that the fixture tests assert on), not a bare JSONDecodeError.
        raise w.Incomplete('#1140 session expectations is not readable JSON: ' + repr(error)) from error
    rows = document.get('cases') if isinstance(document, dict) else None
    w.need(isinstance(rows, list) and bool(rows), 'session expectations is empty or malformed')
    keys = [row.get('case') if isinstance(row, dict) else None for row in rows]
    w.need(all(isinstance(key, str) for key in keys), 'session expectations case key must be a string')
    w.need(len(set(keys)) == len(keys), 'session expectations has duplicate cases')
    # ``row.get`` not ``row[side]``: a missing side must reach the w.need below
    # as a structured Incomplete instead of a bare KeyError.
    sides = [row.get(side) for row in rows for side in ('ts', 'rust')]
    w.need(all(isinstance(side, dict) for side in sides),
           'session expectations row is missing ts/rust side')
    w.need(all(isinstance(side.get('frames'), list) for side in sides),
           'session expectations row is missing ts/rust frames list')
    w.need(all(isinstance(item, str) and bool(item)
               for side in sides for item in side['frames']),
           'session expectation frames entry must be a non-empty string')
    w.need(all(isinstance(side.get('registry'), dict)
               and isinstance(side['registry'].get('applicable'), bool) for side in sides),
           'session expectations row is missing ts/rust registry.applicable boolean')
    w.need(all(isinstance(side.get('durableStatus'), list) for side in sides),
           'session expectations row is missing ts/rust durableStatus list')
    # Elements are hashed into the label set below: a non-string element would
    # raise an uncaught TypeError instead of the structured Incomplete.
    w.need(all(isinstance(event, str) for side in sides for event in side['durableStatus']),
           'session expectation durableStatus entry must be a string')
    labels = {event for side in sides for event in side['durableStatus']}
    w.need(labels == SESSION_TERMINAL_DURABLE_EVENTS,
           'session expectation durable label family drifted: ' + repr(sorted(labels)))
    return {row['case']: row['rust'] for row in rows}


session_key = w.native.session_key


def case_plan(case):
    """Full input plan plus transport timing, never proof of execution.

    #2396: the registered orders are driven through the real controlled loop
    (#1371), which never calls the provider, so `delay` stays 0 for every
    case. unknown-first/known-final-first release the outcome while the turn
    is still open, so their timer must be slow enough to stay unfired (the TS
    timer.clear edge); every timeout-first family — including after-complete
    and known-final-before-timeout-pending, whose TS counterpart fires the
    timer by hand while the return/outcome is held — must let the real 400 ms
    timer expire during the hold, exactly like TS `fire()`.
    """
    stimulus = stimuli.plan(case)
    if stimulus['order'] in ('unknown-first', 'known-final-first'):
        return {'timeoutMs': SESSION_ORDERING_TIMEOUT_MS, 'delay': 0.0, 'stimulus': stimulus}
    return {'timeoutMs': SESSION_TIMEOUT_MS, 'delay': 0.0, 'stimulus': stimulus}


def readable_registry_evidence(observation, source):
    """Raw readback only. Never turns an unsettled read into a scored row."""
    readable = observation if observation is not None else source.get('lastReadableObservation')
    return readable.get('registry', {}) if isinstance(readable, dict) else {}


def session_row(case, identity, observation, source, evidence=None):
    execution = stimuli.execution(case_plan(case)['stimulus'], evidence)
    row = {'case': case, 'identity': identity, 'stimulus': execution,
           'observation': observation}
    if execution['status'] != 'WITNESSED':
        row['observation'] = None
        row['reason'] = execution['reason'] + ': ' + ', '.join(execution['missing'])
    if observation is None:
        row['reason'] = source['collectionError'] + '; ' + row.get('reason', '')
    return row


def read_observation(home, project, session, run_id, received, request_id=None, *, retain=False):
    """Read the exact project's originals and derive this request's observation."""
    home = Path(home).resolve()
    markers = [p for p in (home / 'projects').glob('*/.cwd') if p.read_text().strip() == project]
    w.need(len(markers) == 1, 'expected exactly one project marker for ' + project)
    marker = markers[0].resolve()
    transcript = marker.parent / 'chats' / (session + '.jsonl')
    w.need(transcript.is_file(), 'expected exactly one transcript for session ' + session)
    request_id = request_id or run_id
    identity = {'projectKey': project, 'sessionKey': session, 'runId': run_id,
                'requestId': request_id, 'transcriptPath': str(transcript)}
    document = home / 'runs.json'
    source = {'projectMarkerPath': str(marker), 'projectMarkerText': marker.read_text(),
              'transcriptPath': str(transcript), 'transcriptText': transcript.read_text(),
              'registryPath': str(document), 'registryText': document.read_text() if document.is_file() else None}
    observation = w.native.disk_observation(source, identity, str(home))
    # The recorder keeps whole envelopes; the frozen projection owns the
    # request-identity check and the yield/terminal split, so the collector and
    # the frozen audit can never disagree about what an in-process frame is.
    # `received` can also carry a frame keyed to another request — e.g. one
    # left in the socket buffer by the previous case's turn — which the
    # strict projection would reject as a whole-read failure (an honest
    # retry, then a collection error) instead of scoring this request's
    # frames.  Drop only frames that positively identify as foreign (a
    # well-formed non-empty string id different from this request's);
    # non-object entries and dict entries with a missing or non-string id
    # still reach the projection and fail closed there — dropping those
    # would widen the filter past what it claims (#2396 fix-r3, DS sibling
    # P1; fix-r4, DS concurrency P2#2).
    own = [frame for frame in received
           if not isinstance(frame, dict)
           or not (isinstance(frame.get('id'), str) and frame.get('id')
                   and frame.get('id') != request_id)]
    observation['frames'] = w.native.project_envelopes(request_id, own)['frames']
    return (observation, identity, source) if retain else observation


def file_signatures(paths):
    signatures = []
    for path in paths:
        try:
            stat = path.stat()
        except FileNotFoundError:
            signatures.append(None)
        else:
            signatures.append((stat.st_size, stat.st_mtime_ns))
    return tuple(signatures)


def wait_observation(home, project, session, run_id, received, request_id, deadline, *, expectation=None):
    """Wait for applicable durable completion and three stable samples after final.

    No durable-completion frame exists in the current gateway protocol. Poll
    the exact files the observation reads, bracketing each read with stats.
    Cancellation propagates to capture's existing owned-process cleanup.
    """
    end = min(deadline, time.monotonic() + SESSION_SETTLE_LIMIT_SECONDS)
    previous, stable, last, last_error = None, 0, None, None
    # #1363 (#5): read at least once even when the settle window is already
    # zero-length. A caller whose deadline elapsed must observe the actual
    # files instead of claiming nothing was readable without a single read.
    while True:
        try:
            result = read_observation(home, project, session, run_id, received, request_id, retain=True)
            observation, identity, source = result
            paths = (Path(source['transcriptPath']), Path(source['registryPath']))
            before = file_signatures(paths)
            # Re-read between stat samples so the retained originals, not just
            # the filename metadata, belong to the final stable interval.
            result = read_observation(home, project, session, run_id, received, request_id, retain=True)
            after = file_signatures(paths)
            observation, identity, source = result
            last = result
            last_error = None
            record = observation['registry']['record']
            # Every signal below is recomputed from *this* round's observation
            # and this round's first recording of `received`.  Nothing is
            # carried into the next round: a registry that goes back to
            # `running` must stop settling even if an earlier round saw a
            # terminal state (test_settle_terminal_state_is_per_round).
            final_seen = any(frame.get('id') == request_id and frame.get('final') is True
                             for frame in received)
            # A failed registry is still appendable during a turn. After this
            # request's final error it is a finished failed attempt; the final
            # may be result_unknown while the durable error is turn_timeout.
            # Retain both verbatim for #1242 scoring rather than requiring them
            # equal (which would hide a stable product difference as NOT_RUN).
            registry_terminal = record.get('state') in SESSION_TERMINAL_STATES or (
                record.get('state') == 'failed'
                and isinstance(record.get('result'), dict) and record['result'].get('type') == 'error'
                and any(frame.get('id') == request_id and frame.get('final') is True
                        and isinstance(frame.get('event'), dict) and frame['event'].get('type') == 'error'
                        for frame in received))
            terminal = registry_terminal
            if expectation is not None and expectation['registry']['applicable'] is False:
                # No registry is promised for this registered case. A terminal
                # transcript status (or a registered empty status surface) and
                # this request's final frame suffice. Do not compare status
                # values with the oracle: stable product differences must score.
                # #1363 (#9): this only *adds* a terminal signal; a terminal
                # registry the product did write must not be overwritten into
                # an honest FAIL degrading to NOT_RUN. The request's final frame
                # is still required: without it the turn is not delimited and
                # the case must not settle (test_settle_inapplicable_transcript_pending).
                # Known boundary: a terminal registry can now satisfy this
                # branch before the transcript's durable entries land; the
                # three-sample window (two 0.05 s intervals, i.e. >=0.10 s of
                # unchanged bytes) is what keeps that narrow, and a late entry
                # shows as an ordered product difference rather than NOT_RUN.
                # Measured (#1715): such an early settle can only score a
                # `durableStatus` difference (FAIL), never a PASS, because the
                # missing entry is still missing in the returned observation
                # (test_settle_unlanded_durable_scores_difference_not_pass).
                durable_terminal = (not expectation['durableStatus']
                                    or any(entry.get('event') in SESSION_TERMINAL_DURABLE_EVENTS
                                           for entry in observation['durableStatus']['entries'])
                                    or any(json.loads(line).get('type') == 'turn_result'
                                           for line in w.native.jsonl_lines(source['transcriptText'])
                                           if line.strip()))
                # Full evaluation, both operands derived this round: the
                # terminal registry a product did write and the durable
                # evidence are read from the same observation, and the final
                # frame is still required (without it the turn is not
                # delimited: test_settle_inapplicable_transcript_pending).
                terminal = final_seen and (registry_terminal or durable_terminal)
            if terminal and before == after:
                stable = stable + 1 if after == previous else 1
                previous = after
                if stable >= 3:
                    return result
            else:
                previous, stable = None, 0
        except (OSError, ValueError, KeyError, TypeError) as error:
            # #1363 (#4): keep the last cause. Incomplete (a ValueError) covers
            # both transient partial writes and structural contract violations;
            # a blind retry used to end in a bare "落盘未完成" that erased which.
            last_error = error
            previous, stable = None, 0
        if time.monotonic() >= end:
            break
        time.sleep(min(SESSION_SETTLE_INTERVAL_SECONDS, max(0, end - time.monotonic())))
    if last is None:
        detail = '' if last_error is None else f'; last error: {last_error!r}'
        raise Blocked('#1140 落盘未完成: no readable transcript/registry before deadline' + detail)
    retained, identity, source = last
    detail = '' if last_error is None else f' (last error: {last_error!r})'
    return None, identity, dict(source, collectionError='落盘未完成' + detail,
                                lastReadableObservation=retained)


def publish_session_domain(output, observer, rows, requests, endpoint, driver, sources=None):
    """Atomically publish the session-domain artifacts, then extend the receipt.

    Every artifact lands through a staging directory and os.replace; the
    receipt is rewritten last, so a partial publish can never score: the
    #1140 frozen loader fails closed on the first missing or altered hash.
    The driver record (#1140 fix-r3) is the driver's own hello answer, kept
    separate from the observer rows to detect an observer-only rewrite (wire
    bytes and receipt hashes recomputed). #1206: both records and the receipt
    can be rewritten together; this is not an independently signed anchor.
    """
    w.need(isinstance(driver.get('binding'), dict), 'driver hello binding record missing')
    folder, staging = output / 'session-domain', output / 'session-domain.partial'
    staging.mkdir(exist_ok=False)
    w.save(staging / 'observer.json', observer)
    w.save(staging / 'cases.json', {'cases': rows})
    w.save(staging / 'provider.json', {'endpoint': endpoint, 'requests': requests})
    w.save(staging / 'driver.json', driver)
    w.save(staging / 'sources.json', sources if sources is not None else [])
    folder.mkdir(exist_ok=False)
    for name in ('observer.json', 'cases.json', 'provider.json', 'driver.json', 'sources.json'):
        os.replace(staging / name, folder / name)
    staging.rmdir()
    receipt = json.loads((output / 'capture/receipt.json').read_text())
    receipt['status'] = 'WITNESS_CAPTURED'
    receipt.update({key: w.sha(folder / name)
                    for name, key in w.native.receipt_artifacts('session-domain').items()})
    temporary = output / 'capture/receipt.partial.json'
    w.save(temporary, receipt)
    os.replace(temporary, output / 'capture/receipt.json')


def validate_session(launch, evidence, rows, requests, cases):
    """Pre-receipt invariants specific to a session-domain capture.

    The frozen audit already rejects a missing sidecar role or an undeclared
    egress target; here the driver ledger must attribute any provider traffic and
    the observed rows must cover the frozen manifest exactly. Each row's
    transcript was already required to exist while the turn was driven.
    """
    w.need([row['case'] for row in rows] == cases,
           'session-domain rows must cover the manifest exactly; no denominator shrink')
    w.native.session_profile(launch, cases)
    w.native.session_windows(evidence, cases, [row['identity']['transcriptPath'] for row in rows])
    w.native.provider_cases(requests, cases)


def wait_ready(tracer, folder, port, deadline):
    while True:
        if tracer.poll() is not None:
            # A non-participant may claim the gateway port after preflight.
            # Probe the same endpoint; only its bind maps EADDRINUSE to BLOCKED.
            probe_bind(port, reuse_address=False)
            w.need(False, 'native gateway exited before readiness')
        stderr = (folder / 'stderr.log').read_text()
        if f'native listening 127.0.0.1:{port}\n' in stderr:
            return
        w.need(time.monotonic() < deadline, 'readiness deadline')
        time.sleep(0.01)


def _native_run(gateway, sidecar, output, port, session_domain=False, registry='on',
                ts_sidecar_baseline=None, session_cases=()):
    # #1314: the only alternate collector profile starts the original TS CLI
    # with an explicit, pinned argv; never an arbitrary shell/wrapper/factory.
    sidecar_args = []
    if ts_sidecar_baseline is not None:
        w.need(session_domain, 'controlled TS factory only belongs to original session-domain stimuli')
        ts_sidecar_baseline = str(Path(ts_sidecar_baseline).resolve(strict=True))
        adapters = Path(__file__).resolve().parents[1] / 'a5-19'
        ts_sidecar_factory = str((adapters / 'host-sidecar-stimulus.mjs').resolve(strict=True))
        loader = (adapters / 'host-sidecar-source-loader.mjs').resolve(strict=True)
        sidecar_args = ['--import', str(loader), str(Path(ts_sidecar_baseline) / 'src/cli/pilotdeck-agent-loop-sidecar.ts')]
        baseline_head = subprocess.check_output(['git', '-C', ts_sidecar_baseline, 'rev-parse', 'HEAD'], text=True).strip()
        w.need(baseline_head == 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85', 'fixed TS baseline identity')
        w.need(not subprocess.check_output(['git', '-C', ts_sidecar_baseline, 'status', '--porcelain', '--untracked-files=no'], text=True).strip(), 'fixed TS baseline must be tracked clean')
        versions = json.loads(subprocess.check_output([str(Path(sidecar).resolve(strict=True)), '-p', 'JSON.stringify(process.versions)'], text=True, timeout=5))
        w.need([versions.get(key) for key in ('node', 'icu', 'uv')] == ['22.22.0', '78.2', '1.52.1'], 'fixed Node/ICU/libuv versions required')
        factory_sha, loader_sha = w.sha(ts_sidecar_factory), w.sha(loader)
    output = Path(output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    home, project = output / 'home', output / 'project'
    home.mkdir()
    project.mkdir()
    requests = []
    cases = session_manifest() if session_domain else None
    if session_cases:
        # Only the #1314 capture adapter may shard the fixed trace byte budget.
        # The original deadline consumer still requires the full registered 72.
        w.need(cases is not None and len(session_cases) == len(set(session_cases)) and set(session_cases) <= set(cases),
               'host capture partition must contain unique original cases')
        w.need(all(case.startswith('registry-') == (registry == 'on') for case in session_cases),
               'host capture partition must agree with the process-frozen registry setting')
        cases = [case for case in cases if case in session_cases]
    expectations = session_expectations() if session_domain else {}
    rows, observer_rows, sources = [], [], []
    # The driver's own record of the gateway's hello answer (#1140 fix-r3),
    # written at receive time and published as its own receipt-pinned
    # artifact; #1206 this is a same-collector consistency copy of the rows.
    driver_hello = {}
    # #2396 s2-s4 stimulus driver: one loop directory the gateway validates at
    # startup and every sidecar freezes at its own spawn, and one storage
    # control directory armed per case (plan.md section 1). Case 1 is
    # prepared before launch, so loop.json is already valid when the gateway
    # resolves its process-wide entry; later cases are relayed between turns.
    # The registry switch is process-frozen, so one capture carries exactly
    # one on/off setting; both captures still drive the full 72-case manifest.
    control = stimulus_driver.StimulusDriver(output / 'stimulus' / 'loop',
                                             output / 'stimulus' / 'storage',
                                             registry_on=(registry == 'on')) \
        if session_domain else None
    if control is not None:
        control.prepare(cases[0], 1, stimuli.plan(cases[0]), 'run-01')

    class Provider(http.server.BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_POST(self):
            length = int(self.headers.get('Content-Length', '0'))
            w.need(0 < length <= 1024 * 1024, 'provider request size')
            raw = self.rfile.read(length)
            body = json.loads(raw)
            # The driver's own request ledger: keyed by the case marker the
            # submitted message carries, recorded before any delay, so a
            # request the gateway abandoned after a timeout is still ledged.
            marker = re.search(r'\[native-witness:([^\]]+)\]'.encode(), raw)
            case = marker.group(1).decode() if marker else None
            requests.append({'case': case, 'path': self.path, 'raw': raw.decode('utf-8')})
            if case is not None:
                time.sleep(case_plan(case)['delay'])
            if body.get('stream'):
                data = ''.join('data: ' + json.dumps({'id': 'witness', 'choices': [{'index': 0, 'delta': delta,
                                        'finish_reason': finish}]}) + '\n\n'
                               for delta, finish in [({'role': 'assistant', 'content': 'native witness text'}, None), ({}, 'stop')]) + 'data: [DONE]\n\n'
                mime = 'text/event-stream'
            else:
                data = json.dumps({'id': 'title', 'choices': [{'message': {'role': 'assistant',
                        'content': '{"title":"Witness"}'}, 'finish_reason': 'stop'}]})
                mime = 'application/json'
            encoded = data.encode()
            self.send_response(200)
            self.send_header('Content-Type', mime)
            self.send_header('Content-Length', str(len(encoded)))
            self.end_headers()
            self.wfile.write(encoded)

    server = worker = None
    started = stopped = False
    def stop_provider():
        nonlocal stopped
        # The provider thread inherits the blocked mask below; no other thread
        # can deliver a Python cancellation while the main thread cleans it up.
        with common.deferred_signals():
            if not stopped and server is not None:
                if started:
                    server.shutdown()
                server.server_close()
                if started:
                    worker.join(timeout=6)
                    w.need(not worker.is_alive(), 'provider did not stop')
                stopped = True
                common.LIVE_GROUPS.discard('native-witness-provider')
    try:
        # Publish ownership before cancellation can escape. Only this short
        # window is masked; capture/Popen inherits the original main-thread mask.
        with common.deferred_signals():
            server = BoundedHTTPServer(('127.0.0.1', port + 1), Provider)
            common.LIVE_GROUPS.add('native-witness-provider')
            worker = threading.Thread(target=server.serve_forever, kwargs={'poll_interval': 0.05}, daemon=True)
            worker.start()
            started = True
        (home / 'pilotdeck.yaml').write_text(
            # The preregistered profile has no external MCP tools. Disable
            # unrelated default plugins explicitly, as the TS controlled host
            # also constructs its loop without a project plugin manager.
            # Default-plugin functionality is verified by its own consumers.
            'schemaVersion: 1\nagent: {model: fixture/mock}\n'
            'extension:\n  builtinPluginsEnabled:\n'
            '    browser-use: false\n    funasr: false\n    windows-skills: false\n'
            'model:\n  providers:\n    fixture:\n'
            f'      protocol: openai\n      url: http://127.0.0.1:{port + 1}/v1\n'
            '      apiKey: local-fixture\n      models:\n        mock: {}\n')
        (home / 'token').write_text('native-witness-fixture\n')
        gateway, sidecar = str(Path(gateway).resolve(strict=True)), str(Path(sidecar).resolve(strict=True))
        argv = [gateway, '--native', '--home', str(home), '--project', str(project), '--listen',
                f'127.0.0.1:{port}', '--token-file', str(home / 'token'), '--sidecar', sidecar, '--drain-ms', '1500']
        for value in sidecar_args:
            argv.extend(['--sidecar-arg', value])
        # The real gateway assembly spawns one sidecar per session, and the
        # session-domain driver gives every case its own session, so the
        # declared roles must be host x1 plus one sidecar per manifest case —
        # the exact multiset the frozen audit compares Counter against (#1140
        # cap-fix; the old ['host', 'sidecar'] failed closed on real captures).
        contract = {'host': 'rust', 'project': str(project),
                    'processRoles': (['host'] + ['sidecar'] * len(cases)) if session_domain else ['host', 'sidecar'],
                    'binaries': {'host': w.identity(gateway, argv), 'sidecar': w.identity(sidecar, [sidecar, *sidecar_args])},
                    'externalEndpoints': [f'127.0.0.1:{port + 1}'],
                    'connections': 1,
                    'cases': cases if session_domain else ['native-witness-smoke'],
                    'capabilities': {'provider': 'loopback-mock', 'tools': []}}

        def exercise(tracer, folder, deadline):
            wait_ready(tracer, folder, port, deadline)
            client = WireClient(port, deadline)
            try:
                client.send({'type': 'hello', 'protocolVersion': '1.2', 'clientName': 'ui',
                             'clientVersion': 'native-witness', 'token': 'native-witness-fixture'})
                hello = client.receive()
                w.need(hello.get('type') == 'hello_ok', 'native hello failed')
                if session_domain:
                    driver_hello['binding'] = hello['interactionBinding']
                    driver_hello['identities'] = []
                    # One connection, one hello, 72 sequential turns: every
                    # case gets its own session and run, so per-case surfaces
                    # never alias. #2396: the stimulus driver owns the receive
                    # loop, multiplexing the socket with the product's witness
                    # receipts; transport timing alone still does not execute a
                    # registered stimulus — session_row withholds the scoring
                    # observation until every boundary has raw receipts.
                    for index, case in enumerate(cases, start=1):
                        run_id = f'run-{index:02d}'
                        request_id = f'case-{index:02d}'
                        key = session_key(case)
                        plan = case_plan(case)
                        if index > 1:
                            # Relay the frozen sidecar plan between cases:
                            # sidecar N spawns during submit N, so it freezes
                            # case N's plan at its own startup (plan.md §1).
                            control.prepare(case, index, plan['stimulus'], run_id)
                        client.send({'type': 'request', 'id': request_id, 'method': 'submit_turn', 'params': {
                            'projectKey': str(project), 'sessionKey': key, 'runId': run_id,
                            'channelKey': 'session-domain', 'message': f'[native-witness:{case}] controlled wait',
                            'allowedTools': [], 'timeoutMs': plan['timeoutMs']}})
                        driven = control.drive(case, plan['stimulus'], client, request_id,
                                               deadline, timeout_ms=plan['timeoutMs'])
                        # Disarm before the settle window, not after it: this
                        # case's receipts are already in the witness
                        # directories the evidence harvest reads, and a
                        # barrier left armed during the settle would hold the
                        # product's later durable writes (#2396 fix-r1, DS
                        # boundary). The next case arms only at its own
                        # prepare.
                        control.disarm()
                        received = driven['received']
                        # #1363 (#10): a manifest case absent from the frozen
                        # expectations is a structural drift, not a bare KeyError
                        # that aborts the whole batch without a rule.
                        expectation = expectations.get(case)
                        w.need(expectation is not None,
                               'session expectations missing manifest case: ' + case)
                        settle = deadline
                        if (registry == 'off' and expectation['registry']['applicable']):
                            # Bounded by the capture deadline too: the window
                            # must never extend past what the lease owns
                            # (#2396 fix-r1, DS boundary).
                            settle = min(deadline, time.monotonic()
                                         + SESSION_OFF_REGISTRY_SETTLE_SECONDS)
                        observation, identity, source = wait_observation(
                            home, str(project), key, run_id, received, request_id, settle,
                            expectation=expectation)
                        # Registry surfaces come from the same settled
                        # observation the scoring reads, unwrapped from their
                        # seq envelopes. The registry-off closure evidence is
                        # the product's own stderr receipt plus the collector's
                        # file-absence observation — never the absence alone.
                        # Retain raw readable evidence even when durability never settled.
                        # Only `observation` reaches scoring; the retained read is
                        # diagnostic evidence and cannot satisfy the settle gate.
                        surfaces = readable_registry_evidence(observation, source)
                        evidence = control.evidence(
                            case, run_id, request_id, driven,
                            (folder / 'stderr.log').read_text(errors='replace'),
                            [entry.get('event') for entry in surfaces.get('events', [])
                             if isinstance(entry, dict)],
                            surfaces.get('record') or None,
                            source.get('registryText') is None)
                        # Keep failed-target raw evidence too: execution() only
                        # exposes complete target witnesses, so that projection
                        # alone cannot reproduce an honest NOT_RUN.
                        source['stimulusEvidence'] = evidence
                        row = session_row(case, identity, observation, source, evidence=evidence)
                        rows.append(row)
                        driver_hello['identities'].append(identity)
                        sources.append(source)
                else:
                    client.send({'type': 'request', 'id': 'witness', 'method': 'submit_turn', 'params': {
                        'projectKey': str(project), 'sessionKey': 'witness-session', 'runId': 'witness-run',
                        'channelKey': 'probe', 'message': 'hello native witness', 'allowedTools': []}})
                    frames = []
                    while True:
                        frame = client.receive()
                        frames.append(frame)
                        w.need(frame.get('event', {}).get('type') != 'error', 'native submit reported error')
                        if frame.get('id') == 'witness' and frame.get('final'):
                            w.need(frame.get('event', {}).get('finishReason') == 'completed', 'native turn incomplete')
                            break
                    w.need(any(f.get('event', {}).get('text') == 'native witness text' for f in frames), 'provider text absent')
                    w.save(output / 'driver.json', {'binding': hello['interactionBinding'], 'rows': client.rows,
                                                   'upgrade': client.upgrade, 'requests': requests})
            finally:
                client.socket.close()
            root_exec = re.search(r'^(\d+)\s+execve\(', w.read_trace(folder / 'trace.log'), re.M)
            w.need(root_exec is not None, 'host exec is absent from trace')
            w.stop_host(tracer, int(root_exec[1]))
            observer_rows.extend(client.rows)

        def validate(launch, evidence):
            if session_domain:
                validate_session(launch, evidence, rows, requests, cases)
            else:
                validate_smoke(launch, evidence, requests, home)
            # Complete owned provider cleanup before capture publishes success.
            stop_provider()

        environment = {'HOME': str(home), 'PILOT_HOME': str(home),
                       'PATH': '/usr/bin:/bin', 'LANG': 'C.UTF-8'}
        if control is not None:
            # The three s2-s4 entries resolve once per process (#1369/#1370/
            # #1371); sidecars inherit them from the gateway.
            environment.update(control.environment())
        if ts_sidecar_baseline is not None:
            environment.update(PILOTDECK_AGENT_LOOP_FACTORY=ts_sidecar_factory,
                               A19_HOST_BASELINE=ts_sidecar_baseline,
                               A19_HOST_TIME='2026-10-14T08:00:00.000Z',
                               A52B_CHILD_LOADLOG=str(output / 'sidecar-loaded.ndjson'))
        launch, evidence = w.capture(contract, output / 'capture',
                                      environment,
                                      timeout=SESSION_LIMIT_SECONDS if session_domain else w.LIMIT_SECONDS,
                                      limit=SESSION_LIMIT_SECONDS, defer_receipt=session_domain,
                                      exercise=exercise, validate=validate, ownership=common.LIVE_GROUPS)
        if session_domain:
            try:
                if ts_sidecar_baseline is not None:
                    w.need(w.sha(ts_sidecar_factory) == factory_sha and w.sha(loader) == loader_sha,
                           'TS controlled factory/loader changed during capture')
                    loaded = [json.loads(line) for line in (output / 'sidecar-loaded.ndjson').read_text().splitlines() if line]
                    sidecar_pids = {process['pid'] for process in evidence['processes'] if process['role'] == 'sidecar'}
                    w.need(bool(loaded) and {row['pid'] for row in loaded} == sidecar_pids,
                           'each actual TS sidecar must load the original source')
                    sources_by_path = {}
                    for row in loaded:
                        path = Path(row['path'])
                        w.need(not path.is_absolute() and '..' not in path.parts, 'TS source load outside fixed baseline')
                        w.need(sources_by_path.setdefault(path, row['sha256']) == row['sha256'], 'same source loaded with different bytes')
                    for path, digest in sources_by_path.items():
                        original = subprocess.check_output(['git', '-C', ts_sidecar_baseline, 'show', baseline_head + ':' + str(path)])
                        w.need(hashlib.sha256(original).hexdigest() == digest == w.sha(Path(ts_sidecar_baseline) / path), 'TS source differs from fixed committed baseline')
                    w.save(output / 'ts-sidecar-provenance.json', {'baselineCommit': baseline_head,
                           'factory': {'path': ts_sidecar_factory, 'sha256': factory_sha},
                           'loader': {'path': str(loader), 'sha256': loader_sha}, 'loaded': loaded})
                publish_session_domain(output, {'connections': 1, 'rows': observer_rows},
                                       rows, requests, f'127.0.0.1:{port + 1}',
                                       driver_hello, sources)
            except BaseException as error:
                try:
                    w.save(output / 'capture/failure.json', {'status': 'INCOMPLETE',
                           'error': repr(error), 'cleanupErrors': [],
                           'cleanupPending': sorted(common.LIVE_GROUPS)})
                except BaseException as publish_error:
                    error.add_note('#1140 session publication failure: ' + repr(publish_error))
                raise
        return launch, evidence
    finally:
        original_error = sys.exception()
        try:
            stop_provider()
        except BaseException as cleanup_error:
            if original_error is None:
                raise
            original_error.add_note('#1140 provider cleanup: ' + repr(cleanup_error))
            failure_path = output / 'capture/failure.json'
            try:
                failure = json.loads(failure_path.read_text()) if failure_path.is_file() else {
                    'status': 'INCOMPLETE', 'error': repr(original_error), 'cleanupErrors': []}
                failure['cleanupErrors'].append('provider: ' + repr(cleanup_error))
                failure['cleanupPending'] = sorted(common.LIVE_GROUPS)
                w.save(failure_path, failure)
            except BaseException as publish_error:
                original_error.add_note('#1140 provider failure publication: ' + repr(publish_error))
        else:
            # capture records its own cleanup before this outer provider cleanup.
            # Refresh the diagnostic while still inside the caller's lease.
            failure_path = output / 'capture/failure.json'
            if original_error is not None and failure_path.is_file():
                try:
                    failure = json.loads(failure_path.read_text())
                    failure['cleanupPending'] = sorted(common.LIVE_GROUPS)
                    temporary = failure_path.with_suffix('.partial.json')
                    w.save(temporary, failure)
                    os.replace(temporary, failure_path)
                except BaseException as publish_error:
                    original_error.add_note('#1140 final cleanup publication: ' + repr(publish_error))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--gateway', required=True)
    parser.add_argument('--sidecar', required=True)
    parser.add_argument('--ts-sidecar-baseline', type=Path,
                        help='#1314 fixed TS checkout for the original CLI controlled factory')
    parser.add_argument('--output', required=True)
    parser.add_argument('--port', type=int, default=28999)
    parser.add_argument('--session-domain', action='store_true',
                        help='#1140 drive the frozen 72-case E1 manifest through one gateway '
                             'and publish observer rows plus per-case three-surface observations '
                             '(session-domain/) for the deadline scoring suite')
    parser.add_argument('--session-case', action='append', default=[],
                        help='#1314 named original case in this trace partition (repeatable)')
    parser.add_argument('--registry', choices=('on', 'off'), default='on',
                        help='#2396 process-frozen run-registry capture group: the on capture '
                             'witnesses the registry cells, the off capture the no-registry '
                             'cells; both drive the full 72-case manifest')
    args = parser.parse_args()
    try:
        native_run(args.gateway, args.sidecar, args.output, args.port,
                   session_domain=args.session_domain, registry=args.registry,
                   ts_sidecar_baseline=args.ts_sidecar_baseline, session_cases=args.session_case)
    except Blocked as error:
        print(json.dumps({'status': 'BLOCKED', 'scoring': 'NOT_RUN', 'error': str(error)}))
        return 2
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
