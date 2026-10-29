#!/usr/bin/env python3
"""#933 cli-parity mock-gateway apparatus mutations; require the bound unit
test to fail at its #933-N assertion, then restore byte-for-byte.

Mutates the Python apparatus and the mock gateway itself
(scripts/cli-parity/*.py, *.mjs), not Rust sources: the compile check is
``py_compile`` / ``node --check`` and the bound tests are the offline unit
tests of the same apparatus (``scripts/cli-parity/test_cli_parity.py``). Run
with no other writers in this worktree, after the apparatus change is
committed (``mutating_sources`` refuses files that differ from HEAD by design;
``--baseline`` points it at a ``git stash create`` tree for a local rehearsal).
Evidence goes in a new output folder. No cargo is involved, so no
CARGO_TARGET_DIR is required. Run all mutants to completion; any non-KILLED
row is a nonzero exit (a stale anchor or a broken run is INVALID, never
SURVIVED).

Kill criterion (the shared negative_panic standard, applied to unittest
output): the *named* test must have failed at its own assertion — its FAIL
block must carry an ``AssertionError:`` line containing the mutant's exclusive
#933 marker. A marker that merely appears somewhere in the log (an ERROR
traceback's source echo, another test's block, plain stdout) is not evidence,
and an abnormal exit (a signal, no unittest summary, an unexpected exit code)
is INVALID, never KILLED.
"""
import os as _boot_os
import sys as _boot_sys

# #1023: -I keeps the fixture search paths out of module resolution. Reject a
# non-isolated launch and any reinsertion of this directory or tests/fixtures
# (unittest discover does exactly that) before importing anything else, so a
# same-named module next to this file can never execute as a bootstrap.
# Both sides of the comparison must be realpaths. abspath keeps a POSIX
# double-slash prefix (``//Users/...``) while the sys.path side normalises it,
# so an aliased entry point slipped through the guard (fix-r1 P1-1).
_boot_dir = _boot_os.path.dirname(_boot_os.path.realpath(__file__))
_boot_roots = {_boot_dir, _boot_os.path.dirname(_boot_dir)}
if not _boot_sys.flags.isolated or any(
        _boot_os.path.realpath(entry or _boot_os.curdir) in _boot_roots
        for entry in _boot_sys.path):
    raise SystemExit(f'#1023: unsafe import path; run with -I by file: python3 -I {__file__}')

import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import py_compile
import re
import socket
import sys
import tempfile
import time

# Load the shared guard by explicit absolute path. A temporary meta-path
# finder makes the official from-import below resolve to that one file, without
# reinserting tests/fixtures into sys.path (the fix-r1 P1-1 bypass) and without
# hand-registering sys.modules. The finder is removed immediately after.
_boot_module_path = Path(__file__).resolve().parents[1] / 'negative_process.py'


class _BootFinder:
    def find_spec(self, fullname, path=None, target=None):
        if fullname != 'negative_process':
            return None
        return importlib.util.spec_from_file_location(fullname, _boot_module_path)


_boot_finder = _BootFinder()
sys.meta_path.insert(0, _boot_finder)
from negative_process import mutating_sources, run_owned  # noqa: E402
sys.meta_path.remove(_boot_finder)

root = Path(__file__).resolve().parents[5]
policy = root / 'scripts/cli-parity/policy.py'
common = root / 'scripts/cli-parity/common.py'
compare = root / 'scripts/cli-parity/compare.py'
run_round = root / 'scripts/cli-parity/run_round.py'
mock_gateway = root / 'scripts/cli-parity/mock_gateway.mjs'
isolation_probe = root / 'scripts/cli-parity/isolation_probe.mjs'


def pinned_mock_port():
    # Read the port from the same source the bound test uses on every run: a
    # hand-copied constant would drift, and the mock-port mutant deliberately
    # changes this value before its negative run.
    match = re.search(r'^MOCK_PORT = (\d+)$', common.read_text(), re.MULTILINE)
    if match is None:
        raise RuntimeError('scripts/cli-parity/common.py has no pinned MOCK_PORT')
    return int(match.group(1))

# Every mutant is applied to the original independently and names the unit
# test whose assertion it must turn red. Rows are (name, source, old, new) so
# the anchor_selfcheck preflight can statically resolve each row's path and
# verify the old anchor against the live file.
mutations = [
    # The allow rule loses its port restriction: every localhost port, the
    # user's 18789 PilotDeck included, would be reachable.
    ('allow-rule-drops-port',
     policy,
     'MOCK_GATEWAY_ALLOW = f\'(allow network-outbound (remote ip "localhost:{MOCK_PORT}"))\'',
     'MOCK_GATEWAY_ALLOW = f\'(allow network-outbound (remote ip "localhost"))\''),
    # The default deny is relaxed to an allow: the sandbox would permit
    # everything the profile does not explicitly deny.
    ('default-deny-relaxed',
     policy,
     "    rules = ['(version 1)', '(deny default)', '(allow sysctl-read)',",
     "    rules = ['(version 1)', '(allow default)', '(allow sysctl-read)',"),
    # The two deny rules are swapped: the profile bytes (and the mock allow
    # rule's insertion point) change while the permission set reads the same.
    ('deny-order-swapped',
     policy,
     "             '(deny network*)', '(deny process-fork)']",
     "             '(deny process-fork)', '(deny network*)']"),
    # The apparatus port drifts away from the pinned mock profiles. The
    # structural checks and the environment assertions all derive from the
    # same constant, so only the pinned digests can catch the drift.
    ('mock-port-changed',
     common,
     'MOCK_PORT = 28999',
     'MOCK_PORT = 28990'),
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
    # The mock's own early exit (an occupied 28999 included) no longer
    # refuses the round before any probe runs.
    ('mock-exit-accepted',
     run_round,
     "                    raise Blocked('mock gateway exited before listening: code '\n"
     "                                 + str(proc.returncode) + '; stderr: '\n"
     "                                 + (logs / 'stderr.log').read_text(errors='replace')[:200])",
     "                    print('mutant: mock early exit accepted')"),
    # A mock that dies right after its listen confirmation no longer fails the
    # post-probe aliveness re-check.
    ('mock-exit-after-readiness-accepted',
     run_round,
     "            if proc.poll() is not None:\n"
     "                raise Blocked(f'mock gateway exited after readiness: code {proc.returncode}')",
     "            if False:\n"
     "                raise Blocked(f'mock gateway exited after readiness: code {proc.returncode}')"),
    # A preflight failure after spawn() no longer stops the registered mock.
    ('post-spawn-failure-leaks-mock',
     run_round,
     "            if proc is not None:\n"
     "                audit = stop_group(proc)",
     "            if proc is None:\n"
     "                audit = stop_group(proc)"),
    # A queued signal delivered before spawn() returns no longer stops the
    # group it just registered (the pre-return window of fix-r3 P1-1).
    ('spawn-exception-leaks-group',
     run_round,
     "    except BaseException:\n"
     "        if proc is not None:\n"
     "            stop_group(proc)  # 回收失败时保持登记：锁保留，fail-closed\n"
     "        raise",
     "    except BaseException:\n"
     "        if proc is not None:\n"
     "            pass  # mutant: leak the group registered a moment ago\n"
     "        raise"),
    # run_round runs marked scenarios under the base profile instead of the
    # relaxed one.
    ('scenario-profile-ignored',
     run_round,
     "        scenario_profile = mock_profile if scenario['id'] in mock_ids else profile",
     "        scenario_profile = profile"),
    # A valid WebSocket ping is answered with a close instead of a pong.
    ('ping-disconnects',
     mock_gateway,
     "    if (opcode === 0x9) { // ping -> pong\n"
     "      this.socket.write(frame(0xa, payload));\n"
     "      return true;\n"
     "    }",
     "    if (opcode === 0x9) { // ping -> pong\n"
     "      sendClose(this.socket, 4001, 'mutant-ping-rejected');\n"
     "      this.destroy();\n"
     "      return false;\n"
     "    }"),
    # The mock's readiness line drifts away from the constant the preflight
    # waits on: a perfectly listening gateway would Block every round.
    ('listening-line-drifts',
     mock_gateway,
     '  console.log(`mock-gateway listening on 127.0.0.1:${port}`);',
     '  console.log(`mock gateway ready on 127.0.0.1:${port}`);'),
    # preflight stops passing the MOCK_PORT-derived port to the probe.
    ('probe-port-not-passed',
     run_round,
     "                          str(SCRIPT / 'isolation_probe.mjs'), str(sentinel), str(MOCK_PORT)],",
     "                          str(SCRIPT / 'isolation_probe.mjs'), str(sentinel)],"),
    # The probe goes back to hardcoding the port instead of reading argv[3].
    ('probe-port-hardcoded',
     isolation_probe,
     'const port = Number(process.argv[3]);',
     'const port = 28999;'),
    # fix-r5 P1 (the Codex repro): the metadata allow gains the checkout
    # scripts dir as a fresh ancestor on the rust side, where SCRIPT is never
    # read. With the pre-fix shared-/PDRS placeholder roots this mutant kept
    # the pinned bytes; the disjoint roots must make a rust digest flip.
    ('script-parent-metadata-allowed',
     policy,
     "    ancestors = {parent for path in read_dirs + read_files for parent in path.parents}",
     "    ancestors = {parent for path in read_dirs + read_files for parent in path.parents}\n"
     "    ancestors.add(SCRIPT.parent)"),
    # The same widening for another input: the baseline is only read on ts, so
    # its parent is new rust-side metadata no placeholder input contributes.
    ('baseline-parent-metadata-allowed',
     policy,
     "    ancestors = {parent for path in read_dirs + read_files for parent in path.parents}",
     "    ancestors = {parent for path in read_dirs + read_files for parent in path.parents}\n"
     "    ancestors.add(baseline.parent)"),
]
tests = {
    'allow-rule-drops-port': 'ContractTests.test_mock_profile_appends_only_the_loopback_allow',
    'marker-ignored': 'ContractTests.test_mock_profile_appends_only_the_loopback_allow',
    'marker-truthy': 'ContractTests.test_mock_gateway_marker_is_strict_boolean_true',
    'profile-binding-ignored': 'ContractTests.test_mock_scenario_binds_mock_profile_and_others_base',
    'gateway-probe-skipped': 'ContractTests.test_mock_gateway_isolation_evidence_required_and_tamper_detected',
    'presence-skipped': 'ContractTests.test_mock_gateway_isolation_evidence_required_and_tamper_detected',
    'rust-node-exec-skipped': 'ContractTests.test_mock_gateway_isolation_evidence_required_and_tamper_detected',
    'mock-exit-accepted': 'ContractTests.test_occupied_mock_port_blocks_round',
    'mock-exit-after-readiness-accepted': 'ContractTests.test_occupied_mock_port_blocks_round',
    'post-spawn-failure-leaks-mock': 'ContractTests.test_preflight_failure_after_spawn_stops_the_mock',
    'spawn-exception-leaks-group': 'ContractTests.test_spawn_stops_its_group_when_a_queued_signal_fires',
    'scenario-profile-ignored': 'ContractTests.test_run_round_selects_the_mock_profile_per_scenario',
    'ping-disconnects': 'ContractTests.test_mock_gateway_answers_ping_with_pong',
    'listening-line-drifts': 'ContractTests.test_mock_gateway_answers_ping_with_pong',
    'probe-port-not-passed': 'ContractTests.test_preflight_passes_the_mock_port_to_the_isolation_probe',
    'probe-port-hardcoded': 'ContractTests.test_preflight_passes_the_mock_port_to_the_isolation_probe',
    'default-deny-relaxed': 'ContractTests.test_mock_profile_appends_only_the_loopback_allow',
    'deny-order-swapped': 'ContractTests.test_mock_profile_appends_only_the_loopback_allow',
    'mock-port-changed': 'ContractTests.test_mock_profile_appends_only_the_loopback_allow',
    'script-parent-metadata-allowed': 'ContractTests.test_mock_profile_appends_only_the_loopback_allow',
    'baseline-parent-metadata-allowed': 'ContractTests.test_mock_profile_appends_only_the_loopback_allow',
}
# The exclusive assertion message each mutant must turn red. A string is one
# marker; a tuple lists the alternative markers of one assertion family (the
# pong check can fail either at the frame comparison or at the EOF helper).
failure_fragments = {
    # fix-r4: the pinned mock-profile digest fires before the structural
    # equality below, so the mutant binds to that digest assertion.
    'allow-rule-drops-port': '#933-2: the mock TS profile changed',
    'default-deny-relaxed': '#933-2: the base TS profile changed',
    'deny-order-swapped': '#933-2: the base TS profile changed',
    'mock-port-changed': '#933-2: the mock TS profile changed',
    # marker-ignored relaxes the default profile too, so the pinned base-profile
    # digest fires first (it subsumes the mock_gateway=False equality further
    # down, which can never fail before it); bind to that digest assertion.
    'marker-ignored': '#933-2: the base TS profile changed',
    'marker-truthy': 'must be refused, not accepted',
    'profile-binding-ignored': '#933-3: the untampered round must compare PASS',
    'gateway-probe-skipped': '#933-4: probe-allowed tamper not detected',
    'presence-skipped': '#933-4: mock evidence without a marked scenario must fail presence',
    'rust-node-exec-skipped': '#933-4: rust mock Node-exec denial not checked',
    'mock-exit-accepted': '#933-5: an occupied 28999 must make the owned mock exit and Block the round',
    'mock-exit-after-readiness-accepted':
        '#933-5: a dead mock after readiness must refuse the round, not pass the preflight',
    'post-spawn-failure-leaks-mock':
        '#933-8: a preflight failure after spawn must stop the mock before propagating',
    'scenario-profile-ignored': '#933-6: the marked scenario must run under the mock profile',
    'ping-disconnects': ('#933-7: a ping must be answered with a pong carrying the same '
                         'payload, not with a close or a dropped connection',
                         '#933-7: the mock closed the connection during the ping exchange'),
    'spawn-exception-leaks-group':
        '#933-8: a queued signal delivered before spawn() returns must stop the group it registered',
    'listening-line-drifts':
        '#933-5: the real mock must print the listening line the preflight waits for',
    'probe-port-not-passed':
        '#933-2: the isolation probe must be passed the port derived from MOCK_PORT',
    'probe-port-hardcoded':
        '#933-2: the probe must not hardcode the port; it reads argv[3]',
    # fix-r5: both metadata-widening mutants keep the ts bytes (on ts, the
    # probe file and the baseline subtree make those parents legitimate
    # ancestors); only the pinned rust digests can catch them.
    'script-parent-metadata-allowed': '#933-2: the base RUST profile changed',
    'baseline-parent-metadata-allowed': '#933-2: the base RUST profile changed',
}

SEPARATOR = '-' * 70
BLOCK_HEADER = re.compile(r'^(FAIL|ERROR): (\S+) \(([^)]+)\)')
# The bound test runs as a script (``__main__``) or, on older Pythons, under
# its module name with the bare class in the parentheses.
MODULES = ('test_cli_parity.', '__main__.')


def passed(result, test):
    log = result.stdout + result.stderr
    return result.returncode == 0 and 'Ran 1 test' in log and '\nOK\n' in log


def unittest_blocks(log):
    """(kind, method, qualname, body) for every unittest FAIL:/ERROR: block.

    A block starts at its header line (a subTest description may follow the
    parenthesized name), opens with a separator line, and closes at the next
    separator; the body in between holds the traceback and the assertion."""
    lines = log.splitlines()
    blocks = []
    for index, line in enumerate(lines):
        header = BLOCK_HEADER.match(line)
        if not header:
            continue
        start = index + 1
        if start < len(lines) and lines[start] == SEPARATOR:
            start += 1
        body = []
        for later in lines[start:]:
            if later == SEPARATOR:
                break
            body.append(later)
        blocks.append((header.group(1), header.group(2), header.group(3), body))
    return blocks


def bound_failure_bodies(blocks, test):
    """The FAIL-block bodies of the bound test only.

    Python >= 3.11 prints the full dotted qualname in the parentheses; older
    versions print the class and keep the method outside. A failure of any
    other test, however similar, never counts."""
    owner, method = test.split('.', 1)
    names = {module + test for module in MODULES}
    classes = {module + owner for module in MODULES}
    return [body for kind, name, qualname, body in blocks
            if kind == 'FAIL'
            and (qualname in names or (qualname in classes and name == method))]


def classify(result, test, fragments):
    """(status, reason) for one finished negative run.

    KILLED requires the named test's own FAIL block to carry an
    ``AssertionError:`` whose message (the line itself and its continuation
    lines to the block's end — unittest prints long diffs before the message)
    contains one of the mutant's exclusive markers: the marker must be the
    assertion's message, not a source echo elsewhere in the log. An abnormal
    exit — a signal, or no unittest summary — is INVALID, and so is a failure
    without the marker. ERROR subTest blocks inside a completed run (exit 1,
    normal summary) do not negate the bound assertion: the pinned assertion
    still fired, which is the kill."""
    if isinstance(fragments, str):
        fragments = (fragments,)
    log = result.stdout + result.stderr
    if passed(result, test):
        return 'SURVIVED', 'bound test still passed'
    if result.returncode != 1:
        return 'INVALID', f'unexpected exit code {result.returncode}; not an assertion failure'
    if 'Ran 1 test' not in log or not re.search(r'^FAILED', log, re.MULTILINE):
        return 'INVALID', 'no unittest summary; the bound test crashed or never ran'
    named = bound_failure_bodies(unittest_blocks(log), test)
    if not named:
        return 'INVALID', 'the bound test did not fail at an assertion (no FAIL block)'
    for body in named:
        for index, line in enumerate(body):
            # unittest prints long diffs before the message, as continuation
            # lines of the exception — everything from the AssertionError
            # line to the block's end is the assertion's own message.
            if line.startswith('AssertionError: ') and any(
                    fragment in '\n'.join(body[index:]) for fragment in fragments):
                return 'KILLED', 'bound test failed at its #933 assertion'
    return 'INVALID', 'bound test failed without the #933 assertion (wrong failure point)'


def wait_for_port_free(port, timeout=30, interval=0.1):
    """Wait until the pinned mock port can be bound.

    A killed predecessor's mock can outlive its test process for a moment, or
    a stale listener can still hold 28999. The host r1 run recorded two INVALID
    rows for exactly that reason (fix-r1 P1-2). Waiting keeps an owned release
    from turning the positive control into an environment artefact; a foreign
    owner still fails, but with a named reason after the bounded wait.
    """
    deadline = time.monotonic() + timeout
    while True:
        probe = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        try:
            probe.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            try:
                probe.bind(('127.0.0.1', port))
                return
            except OSError as error:
                if time.monotonic() >= deadline:
                    raise RuntimeError(
                        f'MOCK_PORT {port} still occupied after {timeout}s: {error}') from error
        finally:
            probe.close()
        time.sleep(interval)


def run_test(out, label, test):
    # A stale listener on the pinned port makes the bound test fail in setUp,
    # which the judge would record as INVALID (not a product observation).
    wait_for_port_free(pinned_mock_port())
    # -X pycache_prefix pins this invocation's bytecode cache to a fresh
    # directory: a pyc left next to a (restored) source can otherwise be
    # executed as the mutant whenever its recorded mtime and size match the
    # restored file — same-second, same-size mutants do exactly that, and a
    # "positive" or "restored" control would then judge mutant bytecode.
    with tempfile.TemporaryDirectory(prefix='negctl-pycache-') as pycache:
        result = run_owned([sys.executable, '-I', '-X', f'pycache_prefix={pycache}',
                            'scripts/cli-parity/test_cli_parity.py', test],
                           cwd=root, timeout=600)
    (out / f'{label}.log').write_text(result.stdout + result.stderr)
    return result


def compile_check(path):
    """A mutant that cannot even compile is not evidence.

    The bytecode goes to a throwaway file, never to the __pycache__ next to
    the source: that pyc outlives the mutant, and run_test's children must
    never find it (see run_test)."""
    if path.suffix == '.py':
        with tempfile.TemporaryDirectory(prefix='negctl-compile-') as holder:
            py_compile.compile(str(path), cfile=str(Path(holder) / 'mutant.pyc'), doraise=True)
    else:
        result = run_owned(['node', '--check', str(path)], cwd=root, timeout=60)
        if result.returncode != 0:
            raise RuntimeError('node --check rejected the mutant:\n' + result.stdout + result.stderr)


def persist(out, summary):
    counts = {state: sum(entry['status'] == state for entry in summary)
              for state in ('KILLED', 'SURVIVED', 'INVALID')}
    (out / 'summary.json').write_text(json.dumps(
        {'controls': summary, 'counts': counts}, indent=2) + '\n')


def record(out, summary, row):
    summary.append(row)
    persist(out, summary)
    print(f"{row['mutation']}: {row['status']}: {row['reason']}", flush=True)


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('output', type=Path)
    ap.add_argument('selection', nargs='*', help='mutation name; only those rows run')
    ap.add_argument('--baseline', default='HEAD',
                    help='tree to prove the sources pristine against '
                         '(default HEAD; a "git stash create" tree for a local rehearsal)')
    args = ap.parse_args()
    rows = mutations
    if args.selection:
        rows = [mutation for mutation in mutations if mutation[0] in args.selection]
        assert len(rows) == len(args.selection), 'unknown mutation name'
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    summary = []
    try:
        with mutating_sources(root, [policy, common, compare, run_round, mock_gateway,
                                     isolation_probe],
                              baseline=args.baseline) as snapshots:
            for name, path, old, new in rows:
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
                    if not passed(run_test(out, name + '-positive', tests[name]), tests[name]):
                        status, reason = 'INVALID', 'positive control did not pass the bound test'
                    else:
                        stage = 'compile'
                        path.write_text(content.replace(old, new, 1))
                        compile_check(path)
                        stage = 'negative'
                        status, reason = classify(run_test(out, name + '-negative', tests[name]),
                                                 tests[name], failure_fragments[name])
                except Exception as error:
                    status, reason = 'INVALID', f'{stage}: {type(error).__name__}: {error}'
                finally:
                    path.write_bytes(snapshots[path])
                try:
                    stage = 'restored'
                    if not passed(run_test(out, name + '-restored', tests[name]), tests[name]):
                        status, reason = 'INVALID', reason + '; restored bound test did not pass'
                except Exception as error:
                    status, reason = 'INVALID', reason + f'; restored: {type(error).__name__}: {error}'
                record(out, summary, {'mutation': name, 'test': tests[name], 'status': status,
                                      'reason': reason, 'sourceSha256': before})
    except Exception as error:
        # A baseline/lock/restoration failure makes further mutation unsafe.
        # The rows flushed so far are unproven too: their restores happened
        # inside a guard that failed to verify, so the summary.json already
        # written saying KILLED claims kills the sources never made (fix-r3
        # P1-3). Rewrite the persisted rows INVALID before anything else.
        reason = f'source guard: {type(error).__name__}: {error}'
        for row in summary:
            row.update(status='INVALID', reason=row['reason'] + '; ' + reason)
        persist(out, summary)
        for name, _path, _old, _new in rows[len(summary):]:
            record(out, summary, {'mutation': name, 'test': tests[name], 'status': 'INVALID',
                                  'reason': reason, 'sourceSha256': None})
    counts = {state: sum(row['status'] == state for row in summary)
              for state in ('KILLED', 'SURVIVED', 'INVALID')}
    print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()), flush=True)
    raise SystemExit(int(not summary or any(row['status'] != 'KILLED' for row in summary)))


if __name__ == '__main__':
    main()
