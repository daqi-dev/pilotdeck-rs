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
    ('unbounded-provider-read', collector, 'connection.settimeout(5)', 'connection.settimeout(None)', 'test_provider_read_deadline'),
    ('omit-connect', source, "calls.append({'ownerPid': owners[tid]", "[].append({'ownerPid': owners[tid]", 'test_process_and_network'),
    ('wrong-thread-owner', source, "calls.append({'ownerPid': owners[tid]", "calls.append({'ownerPid': tid", 'test_process_and_network'),
    ('trust-exec-ledger', source, "and evidence['execWitness'] == replay['execWitness']", 'and True', 'test_replay_exec'),
    ('trust-network-ledger', source, "network_equal = evidence['networkWitness'] == replay['networkWitness']", 'network_equal = True', 'test_replay_network'),
    ('omit-exit-status', source, "'-f', '-q', '-xx'", "'-f', '-qq', '-xx'", 'test_command'),
]
messages = {
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
    if len(sys.argv) != 2:
        raise SystemExit('usage: negative.py NEW_OUTPUT | --check-anchors')
    if not os.environ.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller (never create a second target)')
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
            for name, path, before, after, test in cases:
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
            results = [{'name': name, 'status': 'INVALID', 'compiled': False, 'reason': 'guard refusal: ' + str(error)} for name, *_ in cases]
    # Publish only after mutating_sources has verified restoration.
    temporary = out / 'summary.json.tmp'
    temporary.write_text(json.dumps({'controls': results}, indent=2) + '\n')
    temporary.replace(out / 'summary.json')
    print(json.dumps({'controls': results}, indent=2))
    return 0 if len(results) == len(cases) and all(row['status'] == 'KILLED' for row in results) else 1


if __name__ == '__main__':
    raise SystemExit(main())
