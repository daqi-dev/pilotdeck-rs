#!/usr/bin/env python3
"""#933 cli-parity mock-gateway apparatus mutations; require the bound unit
test to fail at its #933-N assertion, then restore byte-for-byte.

Mutates the Python apparatus (scripts/cli-parity/*.py), not Rust sources:
the compile check is ``py_compile`` and the bound tests are the offline unit
tests of the same apparatus (``scripts/cli-parity/test_cli_parity.py``).
Run with no other writers in this worktree, after the apparatus change is
committed (``mutating_sources`` refuses files that differ from HEAD by
design). Evidence goes in a new output folder. No cargo is involved, so no
CARGO_TARGET_DIR is required. Run all mutants to completion; any non-KILLED
row is a nonzero exit (a stale anchor or a broken run is INVALID, never
SURVIVED).
"""
import difflib
import hashlib
import json
from pathlib import Path
import py_compile
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned

root = Path(__file__).resolve().parents[5]
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=False)
policy = root / 'scripts/cli-parity/policy.py'
common = root / 'scripts/cli-parity/common.py'
compare = root / 'scripts/cli-parity/compare.py'
run_round = root / 'scripts/cli-parity/run_round.py'

# Every mutant is applied to the original independently and names the unit
# test whose assertion it must turn red. Rows are (name, source, old, new) so
# the anchor_selfcheck preflight can statically resolve each row's path and
# verify the old anchor against the live file.
mutations = [
    # The allow rule loses its port restriction: every localhost port, the
    # user's 18789 PilotDeck included, would be reachable.
    ('allow-rule-drops-port',
     policy,
     'MOCK_GATEWAY_ALLOW = \'(allow network-outbound (remote ip "localhost:28999"))\'',
     'MOCK_GATEWAY_ALLOW = \'(allow network-outbound (remote ip "localhost"))\''),
    # The marker is ignored: every scenario gets the relaxed profile.
    ('marker-ignored',
     policy,
     '    if mock_gateway:\n        # Probe-verified:',
     '    if True:\n        # Probe-verified:'),
    # The marker validation accepts any truthy value, widening the relaxation.
    ('marker-truthy',
     common,
     "        if scenario['mockGateway'] is not True:",
     "        if not scenario['mockGateway']:"),
    # compare forgets the per-scenario profile binding and validates every
    # case against the base isolation evidence.
    ('profile-binding-ignored',
     compare,
     "            binding = mock if (mock is not None and scenario['id'] in mock_ids) else isolation",
     "            binding = isolation"),
    # compare stops checking the five gateway-probe facts.
    ('gateway-probe-skipped',
     compare,
     "            require(probe.get('connectAllowed') is True\n"
     "                    and all(probe.get(key) in GATEWAY_DENIAL_CODES\n"
     "                            for key in GATEWAY_PROBE_CHECKS[1:]), 'round.gateway-probe')",
     "            require(True, 'round.gateway-probe')"),
    # compare stops requiring the evidence to be present exactly when the
    # registry marks scenarios.
    ('presence-skipped',
     compare,
     "        require((mock is not None) == bool(mock_ids), 'round.mock-gateway-presence')",
     "        require(True, 'round.mock-gateway-presence')"),
    # compare stops requiring Node exec to stay denied under rust-mock.
    ('rust-node-exec-skipped',
     compare,
     "                denial = mock['nodeExec']\n"
     "                require(denial['denied'] is True and denial['status'] == 'DONE' and denial['timedOut'] is False",
     "                denial = mock['nodeExec']\n"
     "                require(True and denial['status'] == 'DONE' and denial['timedOut'] is False"),
    # An occupied mock port no longer refuses the run.
    ('occupied-port-accepted',
     run_round,
     "        raise Blocked(f'mock gateway port {port} is occupied; refusing to run: {error}') from error",
     "        print('mutant: occupied-port refusal disabled')"),
    # The occupied-port preflight is not called before spawning the mock.
    ('occupied-port-unchecked',
     run_round,
     "    ensure_loopback_free(MOCK_PORT)\n    logs = output / 'mock-gateway'",
     "    logs = output / 'mock-gateway'"),
    # run_round runs marked scenarios under the base profile instead of the
    # relaxed one.
    ('scenario-profile-ignored',
     run_round,
     "        scenario_profile = mock_profile if scenario['id'] in mock_ids else profile",
     "        scenario_profile = profile"),
]
tests = {
    'allow-rule-drops-port': 'ContractTests.test_mock_profile_appends_only_the_loopback_allow',
    'marker-ignored': 'ContractTests.test_mock_profile_appends_only_the_loopback_allow',
    'marker-truthy': 'ContractTests.test_mock_gateway_marker_is_strict_boolean_true',
    'profile-binding-ignored': 'ContractTests.test_mock_scenario_binds_mock_profile_and_others_base',
    'gateway-probe-skipped': 'ContractTests.test_mock_gateway_isolation_evidence_required_and_tamper_detected',
    'presence-skipped': 'ContractTests.test_mock_gateway_isolation_evidence_required_and_tamper_detected',
    'rust-node-exec-skipped': 'ContractTests.test_mock_gateway_isolation_evidence_required_and_tamper_detected',
    'occupied-port-accepted': 'ContractTests.test_occupied_mock_port_blocks_round',
    'occupied-port-unchecked': 'ContractTests.test_occupied_mock_port_blocks_round',
    'scenario-profile-ignored': 'ContractTests.test_run_round_selects_the_mock_profile_per_scenario',
}
# A killed mutant must fail through its bound assertion, not through an
# incidental crash; the message fragment proves the intended assertion fired.
failure_fragments = {
    'allow-rule-drops-port': '#933-2: exactly one allow rule, directly after the network deny',
    # marker-ignored relaxes the default profile too, so the pinned base-profile
    # hash fires first (it subsumes the mock_gateway=False equality further
    # down, which can never fail before it); bind to that hash assertion.
    'marker-ignored': '#933-2: the base TS profile changed',
    'marker-truthy': '#933-1:',
    'profile-binding-ignored': '#933-3:',
    'gateway-probe-skipped': '#933-4:',
    'presence-skipped': '#933-4: mock evidence without a marked scenario must fail presence',
    'rust-node-exec-skipped': '#933-4: rust mock Node-exec denial not checked',
    'occupied-port-accepted': '#933-5:',
    'occupied-port-unchecked': '#933-5:',
    'scenario-profile-ignored': '#933-6: the marked scenario must run under the mock profile',
}
selected = sys.argv[2:]
if selected:
    mutations = [mutation for mutation in mutations if mutation[0] in selected]
    assert len(mutations) == len(selected), 'unknown mutation name'


def run_test(label, test):
    result = run_owned([sys.executable, '-I', 'scripts/cli-parity/test_cli_parity.py', test],
                       cwd=root, timeout=600)
    (out / f'{label}.log').write_text(result.stdout + result.stderr)
    return result


def passed(result, test):
    log = result.stdout + result.stderr
    return result.returncode == 0 and f'Ran 1 test' in log and '\nOK\n' in log


summary = []
try:
    with mutating_sources(root, [policy, common, compare, run_round]) as snapshots:
        for name, path, old, new in mutations:
            status, reason, stage, before = 'INVALID', '', 'anchor', None
            try:
                original = snapshots[path]
                before = hashlib.sha256(original).hexdigest()
                content = original.decode()
                # A stale anchor (source drifted, or a copy-paste duplicate)
                # must abort this row; recording it as SURVIVED would hide a
                # broken control.
                if content.count(old) != 1:
                    raise AssertionError(
                        f'{name}: anchor occurs {content.count(old)} times (expected 1); '
                        'the anchor is stale and must be updated')
                if not passed(run_test(name + '-positive', tests[name]), tests[name]):
                    reason = 'positive control did not pass the bound test'
                else:
                    stage = 'compile'
                    changed = content.replace(old, new, 1)
                    path.write_text(changed)
                    # A mutant that cannot even compile is not evidence.
                    py_compile.compile(str(path), doraise=True)
                    stage = 'negative'
                    result = run_test(name + '-negative', tests[name])
                    log = result.stdout + result.stderr
                    if passed(result, tests[name]):
                        status, reason = 'SURVIVED', 'bound test still passed'
                    elif result.returncode != 0 and f'FAILED' in log and failure_fragments[name] in log:
                        status, reason = 'KILLED', 'bound test failed at its #933 assertion'
                    else:
                        reason = 'bound test failed without the #933 assertion (wrong failure point)'
            except Exception as error:
                status, reason = 'INVALID', f'{stage}: {type(error).__name__}: {error}'
            finally:
                path.write_bytes(snapshots[path])
            try:
                stage = 'restored'
                if not passed(run_test(name + '-restored', tests[name]), tests[name]):
                    status, reason = 'INVALID', reason + '; restored bound test did not pass'
            except Exception as error:
                status, reason = 'INVALID', reason + f'; restored: {type(error).__name__}: {error}'
            summary.append({'mutation': name, 'test': tests[name], 'status': status, 'reason': reason,
                            'sourceSha256': before})
            (out / 'summary.json').write_text(json.dumps(
                {'controls': summary,
                 'counts': {state: sum(row['status'] == state for row in summary)
                            for state in ('KILLED', 'SURVIVED', 'INVALID')}}, indent=2) + '\n')
            print(f'{name}: {status}: {reason}', flush=True)
except Exception as error:
    # A baseline/lock/restoration failure makes further mutation unsafe.
    # Still account for every selected case and never leave an all-green summary.
    reason = f'source guard: {type(error).__name__}: {error}'
    for row in summary:
        row.update(status='INVALID', reason=row['reason'] + '; ' + reason)
    for name, _path, _old, _new in mutations[len(summary):]:
        summary.append({'mutation': name, 'test': tests[name], 'status': 'INVALID', 'reason': reason})
        print(f'{name}: INVALID: {reason}', flush=True)
    (out / 'summary.json').write_text(json.dumps(
        {'controls': summary,
         'counts': {state: sum(row['status'] == state for row in summary)
                    for state in ('KILLED', 'SURVIVED', 'INVALID')}}, indent=2) + '\n')
counts = {state: sum(row['status'] == state for row in summary)
          for state in ('KILLED', 'SURVIVED', 'INVALID')}
print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()), flush=True)
raise SystemExit(int(not summary or any(row['status'] != 'KILLED' for row in summary)))
