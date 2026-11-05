#!/usr/bin/env python3
"""#1316 A7.8-pr1 session-domain assembly: product mutations for the wire-up.

Every mutant removes one piece of the assembly (a registry entry, a describe
capability, an always-on method message, the always-on family registration,
the explicit HTTP same-domain entry) and must be killed by the named oracle
test in the same process, with the marker of the mutant inside that test's own
panic payload. Original source bytes and hashes are verified after every
mutation; the summary is emitted only after all mutants have run.

Modes: `--self-check` proves the slot guard still exits bounded under
`python3 -O` (#1632); `--slot-guard-probe BUSY_SCRIPT TIMEOUT` is the child
mode that self-check drives. Both run before any output directory is created,
so neither needs `CARGO_TARGET_DIR`.
"""
import difflib
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_panic import named_test_failed, panic_evidence
from negative_selection import baseline_passed
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
source = root / 'src/pilotdeck-gateway/src'

# (name, file under src/pilotdeck-gateway/src, old bytes, replacement,
#  full libtest name, required panic-payload marker). Each mutant is applied
# to the original tree independently, never cumulatively.
mutations = [
    ('registry-drops-reconnect-interaction',
     'native_process/methods/interaction.rs',
     '    "reconnect_interaction",\n    "elicitation_respond",',
     '    "elicitation_respond",',
     'native_process::methods::tests::session_domain_reconciles_with_coverage_manifest',
     '#1316 A7 method reconnect_interaction is dispatched'),
    ('describe-drops-permission-decide',
     'native_process/requests.rs',
     '                            "permission_decide",\n'
     '                            "async_hook_result",',
     '                            "async_hook_result",',
     'native_process::management_tests::production_host_management_and_restart',
     '#1231 exact wired capability set'),
    # Dropping the registry entry sends `always_on_apply` to the
    # `metadata::unavailable` fallback, whose code is the SAME
    # CAPABILITY_UNAVAILABLE the family returns (metadata.rs:69). The code
    # assertion therefore passes on this mutant; the family-specific message
    # assertion is the discriminator that must panic, so the required marker
    # names it. The test still asserts the code first for every other mutant
    # (the marker here is evidence, not a weakening: any registry drop still
    # fails the run).
    ('registry-drops-always-on',
     'native_process/methods/mod.rs',
     '    interaction: interaction::METHODS,\n    always_on: always_on::METHODS,',
     '    interaction: interaction::METHODS,',
     'native_process::methods::always_on::tests::always_on_methods_answer_capability_unavailable',
     '#1316 always_on_apply message'),
    ('http-store-branch-removed',
     'native_http.rs',
     '    if path.starts_with(crate::user_dialog::http_store::STORE_PATH_PREFIX) {\n'
     '        return Ok(handler_response(\n'
     '            hyper::StatusCode::SERVICE_UNAVAILABLE,\n'
     '            "application/json",\n'
     '            "{\\"error\\":\\"not_yet_open\\",\\"store\\":\\"gateway-user-dialog-store\\"}"'
     '.to_owned(),\n'
     '            keep_alive,\n'
     '        ));\n'
     '    }\n',
     '',
     'native_http::tests::user_dialog_store_entry_is_explicitly_not_yet_open',
     '#1316 user-dialog store entry is a 503'),
    # #1632 P2-1: the wire-up has ten mutation points - three interaction
    # registry entries, three always-on per-method messages, two describe
    # capabilities and the always-on family registration - plus the explicit
    # HTTP store entry. Four were covered above, so the seven below cover the
    # rest. Each row names the assertion message the product test already
    # prints, so the kill is attributable to that one assertion.
    ('registry-drops-elicitation-respond',
     'native_process/methods/interaction.rs',
     '    "elicitation_respond",\n',
     '',
     'native_process::methods::tests::session_domain_reconciles_with_coverage_manifest',
     '#1316 A7 method elicitation_respond is dispatched'),
    ('registry-drops-permission-decide',
     'native_process/methods/interaction.rs',
     '    "permission_decide",\n',
     '',
     'native_process::methods::tests::session_domain_reconciles_with_coverage_manifest',
     '#1316 A7 method permission_decide is dispatched'),
    # The three always-on methods share one code path and differ only in the
    # message they return, so the per-method message is the only thing that can
    # tell them apart. Copying one neighbour's text into another arm is exactly
    # the copy-paste slip these three rows have to kill; each arm is mutated in
    # turn, so a redundant arm cannot hide behind a sibling.
    ('always-on-abort-copies-apply-message',
     'native_process/methods/always_on.rs',
     '                "always_on_abort" => "Always-On abort is not configured on this gateway.",',
     '                "always_on_abort" => "Always-On apply is not configured on this gateway.",',
     'native_process::methods::always_on::tests::always_on_methods_answer_capability_unavailable',
     '#1316 always_on_abort message'),
    ('always-on-rerun-copies-apply-message',
     'native_process/methods/always_on.rs',
     '                "always_on_rerun_plan" => "Always-On rerun is not configured on this gateway.",',
     '                "always_on_rerun_plan" => "Always-On apply is not configured on this gateway.",',
     'native_process::methods::always_on::tests::always_on_methods_answer_capability_unavailable',
     '#1316 always_on_rerun_plan message'),
    ('always-on-apply-copies-abort-message',
     'native_process/methods/always_on.rs',
     '                _ => "Always-On apply is not configured on this gateway.",',
     '                _ => "Always-On abort is not configured on this gateway.",',
     'native_process::methods::always_on::tests::always_on_methods_answer_capability_unavailable',
     '#1316 always_on_apply message'),
    ('describe-drops-async-hook-result',
     'native_process/requests.rs',
     '                            "async_hook_result",\n'
     '                            "delete_session",\n',
     '                            "delete_session",\n',
     'native_process::management_tests::production_host_management_and_restart',
     '#1231 exact wired capability set'),
    ('describe-drops-delete-session',
     'native_process/requests.rs',
     '                            "delete_session",\n'
     '                        ]\n',
     '                        ]\n',
     'native_process::management_tests::production_host_management_and_restart',
     '#1231 exact wired capability set'),
]
# Fail closed on an empty run: with no mutants the loops below sweep nothing and
# the script would print KILLED=0 and exit 0, so "the table was emptied" would
# look exactly like "the control is healthy". ci.yml runs this fixture itself
# (#1632), but an emptied table still has to fail here rather than rely on a
# caller noticing that a green run covered nothing.
if not mutations:
    sys.exit('no mutations defined: the negative control would pass without testing anything')


# #1632 P2-2: the slot wait and its deadline were both `assert`s. `python3 -O`
# strips assertions, so the deadline disappeared and the loop waited forever -
# while `mutating_sources` still held the tree lock, which blocks every other
# negative control in the repository. The wait now goes through the shared
# bounded helper (`run_owned`: own session, group kill, wall-clock timeout) and
# ends in an explicit `sys.exit`, which `-O` cannot strip.
SLOT_TIMEOUT_SECONDS = 1800


def acquire_slot(script=None, timeout=SLOT_TIMEOUT_SECONDS):
    """Wait for a cargo slot; the deadline is an explicit exit, never an assert."""
    script = root / 'scripts/cargo-slots.sh' if script is None else script
    try:
        return wait_for_slot(script, timeout=timeout)
    except subprocess.TimeoutExpired:
        sys.exit(f'cargo slots unavailable for {timeout}s: refusing to run cargo '
                 'while the source tree lock is held')
    except subprocess.CalledProcessError as error:
        # A slot gate that cannot be sampled (cargo-slots.sh rc 2/3) must abort
        # the run instead of being read as "0 slots in use".
        sys.exit(f'cargo slot gate failed with rc={error.returncode}: {error.stderr}')


CARGO_TEST_TIMEOUT_SECONDS = 1800


def run(full):
    # No -q/--quiet: libtest must print `test <name> ... ok` lines (#273, #978).
    # --locked: the gate runs on the committed lockfile, like the shared entry
    # that builds the scanner (`negative_process.py` uses --locked for the same
    # reason). A timeout keeps the cargo output it collected: a bare
    # TimeoutExpired traceback would leave the run red with no evidence at all
    # (#1632 review). The bound is generous because the first call on a cold CI
    # target also compiles every dependency.
    try:
        result = run_owned(
            ['cargo', 'test', '--locked', '--manifest-path', 'src/Cargo.toml', '-p',
             'pilotdeck-gateway', '--lib', full, '--', '--exact'],
            cwd=root, env=env, timeout=CARGO_TEST_TIMEOUT_SECONDS)
        return result.returncode, result.stdout + result.stderr
    except subprocess.TimeoutExpired as error:
        collected = (error.output or '') + (error.stderr or '')
        return 124, f'{collected}\nTIMEOUT after {CARGO_TEST_TIMEOUT_SECONDS}s\n'


def take_slot(name):
    slots = acquire_slot()
    (out / f'{name}.slots.log').write_text(slots.stdout + slots.stderr)


def self_check(timeout=2, wall=60):
    """Negative control for the slot guard: `-O` must not change the exit (#1632).

    A permanently busy gate forces the deadline. The guard runs in child
    interpreters both plain and under `-O`; the old assert-based deadline never
    returned under `-O`, so the bounded child would have to be killed at `wall`
    and this check would report the hang instead of passing.
    """
    problems = []
    with tempfile.TemporaryDirectory(prefix='negctl-1316-slot-') as directory:
        busy = Path(directory) / 'cargo-slots-busy.sh'
        busy.write_text('#!/bin/sh\necho "cargo-slots: 1 / cap 1"\nexit 1\n')
        busy.chmod(0o755)
        for flags in ([], ['-O']):
            label = ' '.join(['python3'] + flags)
            started = time.monotonic()
            try:
                result = run_owned(
                    [sys.executable, '-I', *flags, str(Path(__file__).resolve()),
                     '--slot-guard-probe', str(busy), str(timeout)],
                    timeout=wall)
            except subprocess.TimeoutExpired:
                problems.append(f'{label}: the slot guard never returned within {wall}s')
                continue
            elapsed = time.monotonic() - started
            if result.returncode != 1:
                problems.append(f'{label}: expected the bounded exit rc=1, got rc={result.returncode}')
            elif 'cargo slots unavailable' not in result.stderr:
                problems.append(f'{label}: exit did not report the slot timeout')
            else:
                print(f'self-check {label}: bounded exit rc=1 in {elapsed:.1f}s', flush=True)
    for problem in problems:
        print('self-check FAILED: ' + problem, file=sys.stderr)
    if problems:
        return 1
    print(f'self-check: the slot guard exits bounded with and without -O ({timeout}s deadline)',
          flush=True)
    return 0


if sys.argv[1:2] == ['--slot-guard-probe']:
    # Child mode driven by `self_check`: run the real guard against a gate that
    # is always busy. Returning here at all would mean the wait was unbounded.
    if len(sys.argv) != 4:
        sys.exit('usage: --slot-guard-probe BUSY_SLOT_SCRIPT TIMEOUT_SECONDS')
    try:
        probe_timeout = float(sys.argv[3])
    except ValueError:
        sys.exit(f'--slot-guard-probe: not a timeout: {sys.argv[3]!r}')
    acquire_slot(script=Path(sys.argv[2]), timeout=probe_timeout)
    sys.exit('the slot guard returned instead of exiting at its deadline')
if sys.argv[1:] == ['--self-check']:
    sys.exit(self_check())

selected = sys.argv[2:]
if selected:
    mutations = [mutation for mutation in mutations if mutation[0] in selected]
    # Explicit exit, not an assert: `python3 -O` strips asserts and the unknown
    # name would then silently narrow the run (or empty it) instead of failing.
    if len(mutations) != len(selected):
        sys.exit(f'unknown mutation name(s): {sorted(set(selected) - {m[0] for m in mutations})}')
    if not mutations:
        sys.exit('no mutations selected: the negative control would pass without testing anything')
summary = []
failures = []

out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=False)
env = os.environ.copy()
if not env.get('CARGO_TARGET_DIR'):
    sys.exit('CARGO_TARGET_DIR must be supplied by the caller')


with mutating_sources(root, sorted({source / filename for _, filename, _, _, _, _ in mutations})) as snapshots:
    # #819: an unmutated baseline of every named target must be green first.
    baseline_ok = set()
    for full in dict.fromkeys(test for *_, test, _ in mutations):
        take_slot('baseline-' + full.rsplit('::', 1)[-1])
        code, log = run(full)
        (out / f'baseline-{full.rsplit("::", 1)[-1]}.log').write_text(log)
        ok = code == 0 and baseline_passed(log, full)
        print(f'baseline {full}: {"PASS" if ok else "INVALID"}', flush=True)
        if ok:
            baseline_ok.add(full)
        else:
            failures.append('baseline-' + full)

    for name, filename, old, new, full, marker in mutations:
        path = source / filename
        original = snapshots[path]
        before = hashlib.sha256(original).hexdigest()
        content = original.decode()
        try:
            # A stale anchor must abort as INVALID, never count as SURVIVED.
            # Explicit raise, not an assert: `python3 -O` strips asserts, and the
            # replace below would then change nothing and be recorded as
            # SURVIVED. Same class as the slot deadline (#1632 P2-2).
            if content.count(old) != 1:
                raise ValueError(f'{name}: anchor matches {content.count(old)} times')
            changed = content.replace(old, new, 1)
            (out / f'{name}.patch').write_text(''.join(difflib.unified_diff(
                content.splitlines(True), changed.splitlines(True),
                fromfile=filename, tofile=filename)))
            take_slot(name)
            path.write_text(changed)
            code, log = run(full)
            (out / f'{name}.log').write_text(log)
            evidence = panic_evidence(log, full)
            killed = (full in baseline_ok and code == 101
                      and named_test_failed(log, full)
                      and marker in evidence
                      and 'could not compile' not in log)
            failure = next((line.strip() for line in log.splitlines()
                            if 'panicked at' in line or 'assertion' in line), '')
            status = 'KILLED' if killed else (
                'SURVIVED' if code == 0 else 'INVALID')
            summary.append(dict(name=name, test=full, status=status,
                                returncode=code, failure=failure,
                                sourceSha256=before))
            print(f'{name}: {status} rc={code} {failure}', flush=True)
            if status != 'KILLED':
                failures.append(name)
        except Exception as error:
            summary.append(dict(name=name, test=full, status='INVALID',
                                reason=f'{type(error).__name__}: {error}',
                                sourceSha256=before))
            failures.append(name)
        finally:
            path.write_bytes(original)
            # A restore that does not verify must fail loudly; an assert here is
            # stripped by `python3 -O` (#1632 P2-2). `mutating_sources` re-checks
            # the whole guarded set on exit, this is the per-mutant check.
            restored = hashlib.sha256(path.read_bytes()).hexdigest()
            if restored != before:
                raise SystemExit(f'{name}: restore left {restored} != {before}')
# The guarded window closed here: `mutating_sources` restored and re-verified
# every source on context exit. Only now publish the single summary, atomically
# (temp + rename, #166 r8), so no reader can see a completed-looking KILLED
# list while a later mutant is still applied or a restore is still unverified
# (#979 P0 / #1076). A cancellation before this point leaves no summary at all,
# and a missing summary is fail-closed.
temporary = out / 'summary.json.tmp'
temporary.write_text(json.dumps(summary, ensure_ascii=False, indent=2) + '\n')
temporary.replace(out / 'summary.json')
counts = {state: sum(row['status'] == state for row in summary)
          for state in ('KILLED', 'SURVIVED', 'INVALID')}
print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()), flush=True)
if failures:
    print(f'not killed: {", ".join(failures)}', file=sys.stderr)
    sys.exit(1)
print(f'{len(summary)}/{len(mutations)} killed; every source restored byte-for-byte', flush=True)
