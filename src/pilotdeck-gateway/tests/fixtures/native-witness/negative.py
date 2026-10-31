#!/usr/bin/env python3
"""#1140 source mutations; HEAD guard is mandatory, all cases are summarized."""
import sys
if not sys.flags.isolated:
    raise SystemExit('run with python3 -I')
sys.dont_write_bytecode = True
import json
import os
from pathlib import Path
import re

root = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

source = root / 'scripts/native-witness/witness.py'
collector = root / 'scripts/native-witness/collect.py'
fixture = Path(__file__).with_name('test_witness.py')
cases = [
    ('probe-scope-too-wide', collector, 'with socket.socket() as probe:', 'with fixed_port_bind(), socket.socket() as probe:', 'test_bind_error_scope'),
    ('all-bind-errors-blocked', collector, 'if error.errno == errno.EADDRINUSE:', 'if True:', 'test_bind_error_scope'),
    ('trace-cap-changed', source, 'TRACE_LIMIT = 64 * 1024 * 1024', 'TRACE_LIMIT = 63 * 1024 * 1024', 'test_trace_size_precheck'),
    ('client-frame-overflow', collector, "        if size < 126:\n            header = bytes([0x81, 0x80 | size])\n        elif size <= 65535:\n            header = b'\\x81\\xfe' + size.to_bytes(2, 'big')\n        else:\n            header = b'\\x81\\xff' + size.to_bytes(8, 'big')", "        header = bytes([0x81, 0x80 | size]) if size < 126 else b'\\x81\\xfe' + size.to_bytes(2, 'big')", 'test_large_client_frames'),
    ('envelope-frame-overflow', fixture, "    if length < 126:\n        header = bytes([129, length])\n    elif length <= 65535:\n        header = b'\\x81\\x7e' + length.to_bytes(2, 'big')\n    else:\n        header = b'\\x81\\x7f' + length.to_bytes(8, 'big')", "    header = bytes([129, length]) if length < 126 else b'\\x81\\x7e' + length.to_bytes(2, 'big')", 'test_large_compatibility_frame'),
    ('envelope-length-code', fixture, "'lengthCode': header[1]", "'lengthCode': min(length, 126)", 'test_large_compatibility_frame'),
    ('connect-is-blocked', collector, '        result = _native_run(gateway, sidecar, output, port, session_domain=session_domain)', '        with fixed_port_bind():\n            result = _native_run(gateway, sidecar, output, port, session_domain=session_domain)', 'test_connect_eaddrinuse_not_blocked'),
    ('provider-bind-unclassified', collector, '        with fixed_port_bind():\n            super().server_bind()', '        super().server_bind()', 'test_provider_bind_conflict'),
    ('utf8-string-escapes', source, "raise Incomplete('#1169 non-UTF-8 trace string') from error", 'raise error', 'test_non_utf8_string_is_incomplete'),
    ('sendmmsg-accepted', source, "need(False, 'sendmmsg batches are unsupported, including connected sockets')", "need(True, 'sendmmsg batches are unsupported, including connected sockets')", 'test_sendmmsg_explicitly_unsupported'),
    ('trace-no-precheck', source, "need(os.fstat(stream.fileno()).st_size <= TRACE_LIMIT, 'trace exceeds 64 MiB')", "need(True, 'trace exceeds 64 MiB')", 'test_trace_size_precheck'),
    ('trace-no-growth-check', source, "need(len(raw) <= TRACE_LIMIT, 'trace exceeds 64 MiB')", "need(True, 'trace exceeds 64 MiB')", 'test_trace_size_growth_and_read_bound'),
    ('trace-unbounded-read', source, 'raw = stream.read(TRACE_LIMIT + 1)', 'raw = stream.read()', 'test_trace_size_growth_and_read_bound'),
    ('trace-reject-exact-limit', source, "need(os.fstat(stream.fileno()).st_size <= TRACE_LIMIT, 'trace exceeds 64 MiB')", "need(os.fstat(stream.fileno()).st_size < TRACE_LIMIT, 'trace exceeds 64 MiB')", 'test_trace_size_growth_and_read_bound'),
    ('utf8-file-escapes', source, "raise Incomplete('#1169 non-UTF-8 trace file') from error", 'raise error', 'test_trace_size_growth_and_read_bound'),
    ('capture-unbounded-trace', source, "raw = read_trace(output / 'trace.log')", "raw = (output / 'trace.log').read_text()", 'test_capture_trace_limit_wired'),
    ('reload-unbounded-trace', source, "raw = read_trace(folder / 'trace.log')", "raw = (folder / 'trace.log').read_text()", 'test_load_trace_limit_wired'),
    ('driver-unbounded-trace', collector, "w.read_trace(folder / 'trace.log')", "(folder / 'trace.log').read_text()", 'test_collector_trace_limit_wired'),
    ('publish-before-validation', source, "        if validate is not None:\n            validate(launch, evidence)\n        save(output / 'evidence.json', evidence)\n        save(output / 'receipt.json', {'status': 'WITNESS_CAPTURED', 'scoring': 'NOT_RUN',\n                                      'traceSha256': sha(output / 'trace.log'),\n                                      'launchSha256': sha(output / 'launch.json'),\n                                      'prelaunchSha256': sha(output / 'prelaunch.json'),\n                                      'evidenceSha256': sha(output / 'evidence.json'), 'tracerExitCode': result})\n", "        save(output / 'evidence.json', evidence)\n        save(output / 'receipt.json', {'status': 'WITNESS_CAPTURED', 'scoring': 'NOT_RUN',\n                                      'traceSha256': sha(output / 'trace.log'),\n                                      'launchSha256': sha(output / 'launch.json'),\n                                      'prelaunchSha256': sha(output / 'prelaunch.json'),\n                                      'evidenceSha256': sha(output / 'evidence.json'), 'tracerExitCode': result})\n        if validate is not None:\n            validate(launch, evidence)\n", 'test_smoke_before_receipt'),
    ('ignore-gateway-bind-race', collector, '            probe_bind(port, reuse_address=False)', '            pass', 'test_gateway_bind_race'),
    ('preflight-gateway-reuse', collector, '((port, False), (port + 1, True))', '((port, True), (port + 1, True))', 'test_preflight_listener_policies'),
    ('preflight-provider-no-reuse', collector, '((port, False), (port + 1, True))', '((port, False), (port + 1, False))', 'test_preflight_listener_policies'),
    ('readiness-gateway-reuse', collector, 'probe_bind(port, reuse_address=False)', 'probe_bind(port, reuse_address=True)', 'test_wait_ready_gateway_policy'),
    ('real-time-wait-no-reuse', collector, 'probe.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)', 'probe.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 0)', 'test_real_probe_time_wait_and_listener'),
    ('resumed-argument-space', source, '        args = args.rstrip()', '        args = args', 'test_cb8_interleaved_sendto'),
    ('unpaired-raw-lines', source, '    rows = records(raw)', '    rows = [(int(line.split(None, 1)[0]), line.split(None, 1)[1]) for line in raw.splitlines()]', 'test_cb8_interleaved_sendto'),
    ('wrong-resumed-name', source, "need(prefix.startswith(resumed[1] + '('), 'resumed syscall name changed')", "need(True, 'resumed syscall name changed')", 'test_trace_pairing_guards'),
    ('ignore-orphan-resumed', source, "need(pid in pending, 'resumed syscall without entry')", 'if pid not in pending:\n                continue', 'test_trace_pairing_guards'),
    ('duplicate-unfinished', source, "need(pid not in pending, 'duplicate unfinished syscall')", "need(True, 'duplicate unfinished syscall')", 'test_trace_pairing_guards'),
    ('allow-unfinished-eof', source, "need(not pending, 'truncated unfinished syscall')", "need(True, 'truncated unfinished syscall')", 'test_trace_pairing_guards'),
    ('drop-resumed-destination', source, 'rows[index] = (pid, prefix + resumed[2])', "rows[index] = (pid, prefix + (') = 0' if prefix.startswith('connect(') else resumed[2]))", 'test_interleaved_connect'),
    ('omit-lease-signals', collector, 'signal.signal(sig, cancel)', 'None', 'test_signal_lease_phases'),
    ('omit-lease-signal-restore', collector, 'signal.signal(sig, handler)', 'None', 'test_signal_lease_phases'),
    ('interrupt-provider-start', collector, 'with common.deferred_signals():\n            server = BoundedHTTPServer', 'with ExitStack():\n            server = BoundedHTTPServer', 'test_signal_lease_phases'),
    ('interrupt-provider-cleanup', collector, 'with common.deferred_signals():\n            if not stopped', 'with ExitStack():\n            if not stopped', 'test_signal_lease_phases'),
    ('omit-provider-pending', collector, "common.LIVE_GROUPS.add('native-witness-provider')", 'None', 'test_provider_cleanup_pending'),
    ('retain-cleaned-provider', collector, "common.LIVE_GROUPS.discard('native-witness-provider')", 'None', 'test_signal_lease_phases'),
    ('ignore-unstarted-provider', collector, 'if not stopped and server is not None:', 'if not stopped and server is not None and started:', 'test_provider_start_failure'),
    ('blocked-stderr-only', collector, "print(json.dumps({'status': 'BLOCKED', 'scoring': 'NOT_RUN', 'error': str(error)}))", "print(json.dumps({'status': 'BLOCKED', 'scoring': 'NOT_RUN', 'error': str(error)}), file=sys.stderr)", 'test_blocked_cli_status'),
    ('ignore-provider-shutdown-deadline', collector, "w.need(not stopper.is_alive(), 'provider shutdown deadline')", "w.need(True, 'provider shutdown deadline')", 'test_provider_shutdown_deadline'),
    ('unmask-provider-thread', collector, 'with common.deferred_signals():\n            server = BoundedHTTPServer', 'with ExitStack():\n            server = BoundedHTTPServer', 'test_provider_thread_signal_mask'),
    ('unbounded-provider-read', collector, 'connection.settimeout(5)', 'connection.settimeout(None)', 'test_provider_read_deadline'),
    ('omit-connect', source, "calls.append({'ownerPid': owners[tid]", "[].append({'ownerPid': owners[tid]", 'test_process_and_network'),
    ('wrong-thread-owner', source, "calls.append({'ownerPid': owners[tid]", "calls.append({'ownerPid': tid", 'test_process_and_network'),
    ('trust-exec-ledger', source, "and evidence['execWitness'] == replay['execWitness']", 'and True', 'test_replay_exec'),
    ('trust-network-ledger', source, "network_equal = evidence['networkWitness'] == replay['networkWitness']", 'network_equal = True', 'test_replay_network'),
    ('omit-exit-status', source, "'-f', '-q', '-xx'", "'-f', '-qq', '-xx'", 'test_command'),
    ('bypass-parity-lock', collector, 'lease.enter_context(parity_lock(timeout=1))', 'lease.enter_context(ExitStack())', 'test_parity_lock_busy'),
    ('ignore-busy-port', collector, '            probe_bind(candidate, reuse_address=reuse_address)', '            pass', 'test_ports_blocked'),
    ('omit-provider-port-check', collector, '((port, False), (port + 1, True)):', '((port, False),):', 'test_ports_blocked'),
    ('misclassify-busy-port', collector, 'if error.errno == errno.EADDRINUSE:', 'if False:', 'test_ports_blocked'),
    ('blocked-exit-zero', collector, 'return 2', 'return 0', 'test_blocked_cli_status'),
    ('blocked-status-success', collector, "'status': 'BLOCKED'", "'status': 'WITNESS_CAPTURED'", 'test_blocked_cli_status'),
    ('unwire-smoke-validator', collector, 'exercise=exercise, validate=validate)', 'exercise=exercise, validate=None)', 'test_native_validator_wired'),
    ('skip-collector-validation', collector, 'validate_smoke(launch, evidence, requests, home)', 'None', 'test_native_validator_wired'),
    ('late-provider-cleanup', collector, '            stop_provider()', '            pass', 'test_native_validator_wired'),
    ('bypass-smoke-validation', source, 'if validate is not None:', 'if False:', 'test_smoke_before_receipt'),
    ('omit-sidecar-check', collector, "any(p['role'] == 'sidecar' for p in evidence['processes'])", 'True', 'test_smoke_before_receipt'),
    ('omit-egress-check', collector, "bool(evidence['networkWitness']['calls']) and bool(requests)", 'bool(requests)', 'test_smoke_before_receipt'),
    ('omit-request-check', collector, "bool(evidence['networkWitness']['calls']) and bool(requests)", "bool(evidence['networkWitness']['calls'])", 'test_smoke_before_receipt'),
    ('omit-target-check', collector, "all(c['target'] in contract['externalEndpoints'] for c in evidence['networkWitness']['calls'])", 'True', 'test_smoke_before_receipt'),
    ('omit-durable-check', collector, "bool(list(home.rglob('*.jsonl')))", 'True', 'test_smoke_before_receipt'),
    ('accept-nonzero-tracer', source, 'need(result == 0,', 'need(True or result == 0,', 'test_tracer_nonzero'),
]
messages = {
    'probe-scope-too-wide': '#1169 socket creation error is not BLOCKED',
    'all-bind-errors-blocked': '#1169 non-conflict bind error propagates',
    'trace-cap-changed': '#1169 trace production cap is 64 MiB',
    'client-frame-overflow': '#1169 client sends 64-bit frame lengths',
    'envelope-frame-overflow': '#1169 envelope supports 64-bit frame lengths',
    'envelope-length-code': '#1169 envelope wire metadata matches bytes',
    'connect-is-blocked': '#1169 connect EADDRINUSE is not fixed-port BLOCKED',
    'provider-bind-unclassified': '#1169 actual provider bind race is BLOCKED',
    'utf8-string-escapes': '#1169 invalid UTF8 becomes Incomplete',
    'sendmmsg-accepted': '#1169 sendmmsg explicitly refuses every batch',
    'trace-no-precheck': '#1169 oversized trace rejected before read',
    'trace-no-growth-check': '#1169 trace growth past cap is rejected',
    'trace-unbounded-read': '#1169 trace read requests at most cap plus one',
    'trace-reject-exact-limit': '#1169 trace cap is inclusive in bytes',
    'utf8-file-escapes': '#1169 trace file decode is controlled',
    'capture-unbounded-trace': '#1169 capture enforces trace limit before receipt',
    'reload-unbounded-trace': '#1169 reload enforces trace limit',
    'driver-unbounded-trace': '#1169 driver enforces trace limit before host lookup',
    'preflight-gateway-reuse': '#1140 gateway preflight never enables reuse',
    'preflight-provider-no-reuse': '#1140 provider preflight enables reuse before bind',
    'readiness-gateway-reuse': '#1140 readiness gateway never enables reuse',
    'resumed-argument-space': '#1140 cb8 interleaved sendto parses complete call',
    'unpaired-raw-lines': '#1140 cb8 interleaved sendto parses complete call',
    'wrong-resumed-name': '#1140 trace pairing rejects wrong syscall',
    'ignore-orphan-resumed': '#1140 trace pairing rejects orphan',
    'duplicate-unfinished': '#1140 trace pairing rejects duplicate',
    'allow-unfinished-eof': '#1140 trace pairing rejects unterminated',
    'drop-resumed-destination': '#1140 resumed connect preserves destination',
    'real-time-wait-no-reuse': '#1140 reusable probe accepts real TIME_WAIT',
    'omit-lease-signals': '#1140 signal lease child survives cancellation',
    'omit-lease-signal-restore': '#1140 lease signal handlers restored',
    'interrupt-provider-start': '#1140 signals cannot interrupt provider cleanup',
    'interrupt-provider-cleanup': '#1140 signal releases cleaned lease',
    'omit-provider-pending': '#1140 failed provider cleanup retains lease',
    'retain-cleaned-provider': '#1140 signal releases cleaned lease',
    'ignore-unstarted-provider': '#1140 unstarted provider socket is closed',
    'blocked-stderr-only': '#1140 BLOCKED CLI status is on stdout',
    'ignore-provider-shutdown-deadline': '#1140 provider shutdown cannot wait forever',
    'unmask-provider-thread': '#1140 provider thread defers cancellation delivery',
    'publish-before-validation': '#1140 smoke runs before receipt publication',
    'ignore-gateway-bind-race': '#1140 gateway bind race reports BLOCKED',
    'bypass-parity-lock': '#1140 busy parity lease blocks before bind',
    'ignore-busy-port': '#1140 occupied fixed port reports BLOCKED',
    'omit-provider-port-check': '#1140 occupied fixed port reports BLOCKED',
    'misclassify-busy-port': '#1140 occupied fixed port reports BLOCKED',
    'blocked-exit-zero': '#1140 BLOCKED CLI has exit two',
    'blocked-status-success': '#1140 BLOCKED CLI emits explicit status',
    'unwire-smoke-validator': '#1140 collector wires prepublication validator',
    'skip-collector-validation': '#1140 collector invokes smoke validation',
    'late-provider-cleanup': '#1140 provider stops before receipt',
    'bypass-smoke-validation': '#1140 valid smoke runs validation',
    'omit-sidecar-check': '#1140 smoke rejects sidecar',
    'omit-egress-check': '#1140 smoke rejects calls',
    'omit-request-check': '#1140 smoke rejects requests',
    'omit-target-check': '#1140 smoke rejects target',
    'omit-durable-check': '#1140 smoke rejects durable',
    'accept-nonzero-tracer': '#1140 nonzero tracer cannot publish success',
    'unbounded-provider-read': '#1140 provider headers have a read deadline',
    'omit-connect': '#1140 every connect retained',
    'wrong-thread-owner': '#1140 thread egress owner is TGID',
    'trust-exec-ledger': '#1140 replay-exec detects omitted short child',
    'trust-network-ledger': '#1140 replay-network detects omitted connect',
    'omit-exit-status': '#1140 retain strace exit records',
}


def preflight():
    # Read-only: this never executes a mutant or bypasses the HEAD guard.
    errors = []
    for name, path, before, after, _ in cases:
        text = path.read_text()
        if text.count(before) != 1:
            errors.append(f'STALE ANCHOR {name}: {text.count(before)} matches')
        else:
            try:
                compile(text.replace(before, after, 1), str(path), 'exec')
            except SyntaxError as error:
                errors.append(f'INVALID compile {name}: {error}')
    if errors:
        print('\n'.join(errors), file=sys.stderr)
        raise SystemExit(2)
    print(f'#1140 {len(cases)} unique anchors; every Python mutant compiles')


def passed(result):
    log = result.stdout + result.stderr
    return result.returncode == 0 and re.search(r'^Ran 1 test in ', log, re.M) is not None and re.search(r'^OK$', log, re.M) is not None


def classify(result, test, marker):
    log = result.stdout + result.stderr
    if passed(result):
        return 'SURVIVED'
    if (result.returncode == 1 and re.search(r'^FAIL: ' + re.escape(test)
            + r' \(__main__\.WitnessTests(?:\.' + re.escape(test) + r')?\)$', log, re.M)
            and re.search(r'^AssertionError:[^\n]*' + re.escape(marker) + r'[^\n]*$', log, re.M)
            and re.search(r'^Ran 1 test in ', log, re.M) and 'FAILED (failures=1)' in log):
        return 'KILLED'
    return 'INVALID'


def main():
    preflight()
    if sys.argv[1:] == ['--check-anchors']:
        return 0
    if len(sys.argv) not in (2, 4) or (len(sys.argv) == 4 and sys.argv[2] != '--case'):
        raise SystemExit('usage: negative.py NEW_OUTPUT [--case NAME] | --check-anchors')
    if not os.environ.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller (never create a second target)')
    selected = cases if len(sys.argv) == 2 else [row for row in cases if row[0] == sys.argv[3]]
    if not selected:
        raise SystemExit('unknown negative case')
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    results = []
    def run(name, test):
        # This suite executes Python only. wait_for_slot is intentionally not
        # called: there is no cargo process or Rust product-source mutation.
        result = run_owned([sys.executable, '-I', str(Path(__file__).with_name('test_witness.py')),
                            'WitnessTests.' + test], cwd=root, env=os.environ.copy(), timeout=30)
        (out / (name + '.log')).write_text(result.stdout + result.stderr)
        return result
    try:
        with mutating_sources(root, [source, collector, fixture, Path(__file__).resolve()]) as originals:
            for name, path, before, after, test in selected:
                status, reason, compiled = 'INVALID', '', False
                try:
                    text = originals[path].decode()
                    if text.count(before) != 1:
                        raise ValueError('STALE ANCHOR ' + name)
                    changed = text.replace(before, after, 1)
                    compile(changed, str(path), 'exec')
                    compiled = True
                    if not passed(run(name + '-positive', test)):
                        raise ValueError('positive bound test did not pass')
                    path.write_text(changed)
                    status = classify(run(name + '-negative', test), test, messages[name])
                    reason = {'KILLED': 'named assertion matched', 'SURVIVED': 'bound test passed',
                              'INVALID': 'exit/test identity/assertion marker did not match'}[status]
                except Exception as error:
                    status, reason = 'INVALID', str(error)
                finally:
                    path.write_bytes(originals[path])
                try:
                    if not passed(run(name + '-restored', test)):
                        status, reason = 'INVALID', reason + '; restored test failed'
                except Exception as error:
                    status, reason = 'INVALID', reason + '; restore: ' + str(error)
                results.append({'name': name, 'status': status, 'compiled': compiled, 'reason': reason})
    except Exception as error:
        for row in results:
            row.update(status='INVALID', compiled=False, reason='guard failure: ' + str(error))
        if not results:
            results = [{'name': name, 'status': 'INVALID', 'compiled': False, 'reason': 'guard refusal: ' + str(error)} for name, *_ in selected]
    # Publish only after mutating_sources has verified restoration.
    temporary = out / 'summary.json.tmp'
    temporary.write_text(json.dumps({'controls': results}, indent=2) + '\n')
    temporary.replace(out / 'summary.json')
    print(json.dumps({'controls': results}, indent=2))
    return 0 if len(results) == len(selected) and all(row['status'] == 'KILLED' for row in results) else 1


if __name__ == '__main__':
    raise SystemExit(main())
