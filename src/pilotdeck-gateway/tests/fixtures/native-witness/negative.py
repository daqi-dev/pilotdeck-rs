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
cases = [
    ('publish-before-validation', source, "        if validate is not None:\n            validate(launch, evidence)\n        save(output / 'evidence.json', evidence)\n        save(output / 'receipt.json', {'status': 'WITNESS_CAPTURED', 'scoring': 'NOT_RUN',\n                                      'traceSha256': sha(output / 'trace.log'),\n                                      'launchSha256': sha(output / 'launch.json'),\n                                      'prelaunchSha256': sha(output / 'prelaunch.json'),\n                                      'evidenceSha256': sha(output / 'evidence.json'), 'tracerExitCode': result})\n", "        save(output / 'evidence.json', evidence)\n        save(output / 'receipt.json', {'status': 'WITNESS_CAPTURED', 'scoring': 'NOT_RUN',\n                                      'traceSha256': sha(output / 'trace.log'),\n                                      'launchSha256': sha(output / 'launch.json'),\n                                      'prelaunchSha256': sha(output / 'prelaunch.json'),\n                                      'evidenceSha256': sha(output / 'evidence.json'), 'tracerExitCode': result})\n        if validate is not None:\n            validate(launch, evidence)\n", 'test_smoke_before_receipt'),
    ('ignore-gateway-bind-race', collector, "probe.bind(('127.0.0.1', port))", 'pass', 'test_gateway_bind_race'),
    ('unbounded-provider-read', collector, 'connection.settimeout(5)', 'connection.settimeout(None)', 'test_provider_read_deadline'),
    ('omit-connect', source, "calls.append({'ownerPid': owners[tid]", "[].append({'ownerPid': owners[tid]", 'test_process_and_network'),
    ('wrong-thread-owner', source, "calls.append({'ownerPid': owners[tid]", "calls.append({'ownerPid': tid", 'test_process_and_network'),
    ('trust-exec-ledger', source, "and evidence['execWitness'] == replay['execWitness']", 'and True', 'test_replay_exec'),
    ('trust-network-ledger', source, "network_equal = evidence['networkWitness'] == replay['networkWitness']", 'network_equal = True', 'test_replay_network'),
    ('omit-exit-status', source, "'-f', '-q', '-xx'", "'-f', '-qq', '-xx'", 'test_command'),
    ('bypass-parity-lock', collector, 'lease.enter_context(parity_lock(timeout=1))', 'lease.enter_context(ExitStack())', 'test_parity_lock_busy'),
    ('ignore-busy-port', collector, "probe.bind(('127.0.0.1', candidate))", 'pass', 'test_ports_blocked'),
    ('omit-provider-port-check', collector, '(port, port + 1):', '(port,):', 'test_ports_blocked'),
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
        with mutating_sources(root, [source, collector]) as originals:
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
