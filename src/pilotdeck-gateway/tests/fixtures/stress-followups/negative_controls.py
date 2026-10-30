#!/usr/bin/env python3
"""#1109: bounded rerun children must not re-verify the optional baseline.

Every mutation reverts one piece of the #1109 rewrite and must make the
bound parent test fail; restoring the bytes must make it pass again. The
two re-verification rows reproduce the #978 flake itself (children of a
bounded parent re-running the 650/728-source ``git show`` probe table under
load), so they are judged under concurrent direct-binary stress, on the raw
``test result:`` lines only — no retry, no ``-q``, no artificial CPU load:
the test binaries themselves are the load, the way #978's stress-bin.sh
applied it. The two bound rows, the receipt row and the three fix-r1
provenance rows are deterministic kills that only need one run. The #978
regression is itself load-proportional (observed at load 39-106; an idle
host needs the stress batch to supply the load), so a stress row is KILLED
when at least half its runs fail at the bound message and no run fails
anywhere else — a minority kill, or any failure outside the bound message,
keeps the row SURVIVED. Fix-r1 (review P0): "fails at the bound message" is
decided per failed test, from its own panic payload — a run where any other
test fails, or whose exit cannot be attributed (signal, deadline kill,
truncated report), counts as ambiguous and makes the row INVALID.

Fix-r1 provenance rows (review P0/P1): without ``PDRS_BASELINE`` the
rewritten ``oracle_bounded`` used to skip every check; two rows corrupt the
committed oracle and must fail through the now-unconditional static checks
(no baseline in the environment). A third row corrupts one recorded hash
and must fail through the parent's bounded git verification; its two
retraction runs re-apply the same corruption while deleting the parent's
``let _ = oracle();`` call, respectively swallowing the forwarded
verification panic, and must make the test pass again — proving each kill
is bound to the rewrite it names. A fourth row retracts the progress-aware
total bound back to the pre-#1109 flat 30 s and must reproduce the #978
kill under the same stress batch.

Expected runtime: ~20 min for the seven deterministic rows, ~10 min for the
transcript stress row (8x) and ~25 min each for the two worker stress rows
(48x). Run after committing the candidate (``mutating_sources`` proves every
byte against ``--baseline``; a pre-commit full run can pass the SHA printed
by ``git stash create``, which moves no ref). Never bypass the HEAD guard.
A run with zero KILLED rows exits nonzero; every row that is not KILLED
(INVALID/SURVIVED) is reported with its reason before the nonzero exit —
the runner never stops at the first failure.
"""
import argparse
import difflib
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

# Direct-binary stress shape (calibrated 2026-10-30, 10 CPUs; ambient load
# 5-83 across the runs): transcript 6x: reverted 6/6 killed, fixed 6/6 passed
# -> ship 8x; worker 24x killed 17/24 on an idle host (24/24 at load 46-83)
# -> ship 48x so the batch itself supplies #978's load, with the majority
# criterion below absorbing the rest of the load proportionality.
TRANSCRIPT_STRESS = 8
WORKER_STRESS = 48
# fix-r1 (review P0): below four runs the "at least half killed" majority is
# meaningless (two runs with one kill each would already pass), so a stress
# row asked for fewer runs is INVALID instead of weakly KILLED.
STRESS_FLOOR = 4
# fix-r1 (review P1): one stress batch never waits past RUN_DEADLINE, and
# every post-kill reap is bounded by REAP_GRACE. No wait in this runner is
# unbounded, so a hung run can no longer hold the source-mutation window.
RUN_DEADLINE = 1800
REAP_GRACE = 30
# A clean libtest failure exits 101. Anything else — a SIGKILL from this
# runner's own deadline, a crash, a harness error — cannot be attributed to
# a mutation and makes the run ambiguous (row INVALID), never KILLED.
LIBTEST_FAILURE = 101

TRANSCRIPT_BINARY = 'transcript_resolve'
WORKER_TEST = 'exit_statuses_with_inherited_ignored_and_blocked_signals'
WORKER_BINARY = 'background_tasks'

# ---- mutation anchors ------------------------------------------------------

TRANSCRIPT_REMOVE = (
    '                // #1109: the parent verified the baseline once (above); the\n'
    '                // child must not re-verify it under the 30 s bound below.\n'
    '                .env_remove("PDRS_BASELINE")\n')
WORKER_REMOVE = (
    '            // #1109: the calling parent verified the committed oracle once;\n'
    '            // the worker must not re-run the git probe table under the\n'
    '            // bounds below (the #978 scan killed it mid-verification at\n'
    '            // load 90-106). Contract: a worker launched here never needs\n'
    '            // PDRS_BASELINE, so a parent whose worker replays a committed\n'
    '            // oracle must verify that oracle itself before launching (as\n'
    '            // background_tasks and transcript_resolve do).\n'
    '            .env_remove("PDRS_BASELINE")\n')
RECEIPT_PRINT = (
    "        // #1109: progress receipt. Each step is real replay work; printing\n"
    "        // it lets the signal-injected worker's parent distinguish a slow\n"
    '        // but progressing run from a hung one instead of bounding the real\n'
    '        // work by wall clock alone.\n'
    '        println!("BACKGROUND_SEQUENCE_STEP {name} {index}");\n')
PARENT_VERIFY = (
    '    // #1109: verify the committed oracle here, once for this process,\n'
    '    // before launching the worker (which runs without PDRS_BASELINE).\n'
    '    let _ = oracle();\n')
STATIC_CHECK_ARM = (
    '        None => check(&oracle, None),')
SWALLOWED_ARM = (
    '            Ok(Err(message)) => panic!("#1109 baseline verification failed: {message}"),')
COMMIT_LINE = (
    '"sourceCommit": "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85"')
BAD_COMMIT_LINE = (
    '"sourceCommit": "0000000000000000000000000000000000000001"')
HASHES_OPEN = (
    '"sourceSha256": {')
HASHES_EMPTIED = (
    '"sourceSha256": {}, "unusedSha256": {')
HASHED_SOURCE = (
    '"src/adapters/channel/protocol/ChannelCommandRegistry.ts": '
    '"e93b583e0a5323bd35ca93c9568e484f876daa367057a2d423aabdc825867c55"')
TAMPERED_HASH = (
    '"src/adapters/channel/protocol/ChannelCommandRegistry.ts": '
    '"f93b583e0a5323bd35ca93c9568e484f876daa367057a2d423aabdc825867c55"')

# ---- strict libtest attribution (fix-r1, review P0) ------------------------

# The running header and the status lines are read from the report prefix
# only: a parent test reprints its worker's whole libtest summary inside a
# failure section, and those echoed lines must not disturb the outer run's
# own bookkeeping.
RUNNING_RE = re.compile(r'^running (\d+) tests?$', re.MULTILINE)
STATUS_RE = re.compile(r'^test (\S+) \.\.\. (ok|FAILED|ignored)\b', re.MULTILINE)
TOTALS_RE = re.compile(r'^test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; '
                       r'(\d+) ignored;', re.MULTILINE)
PANIC_RE = re.compile(r"^thread '([^']+)'(?: \(\d+\))? panicked at .+:$")
# A panic payload ends at the first libtest landmark; the bound message is
# always on the payload's first line, so a truncated payload cannot hide it.
LANDMARK_RE = re.compile(r'^(?:note:|stack backtrace:|failures:|---- |test |'
                         r'running \d+ tests?$|error: )')


def panic_payloads(log):
    """{thread name: [payload text]} for every libtest panic header.

    The payload runs from the header line to the next libtest landmark
    (note, backtrace, section header, failures list, result line), so
    unrelated output printed after the panic cannot extend it.
    """
    payloads = {}
    lines = log.splitlines()
    for index, line in enumerate(lines):
        match = PANIC_RE.match(line)
        if match is None:
            continue
        body = []
        for later in lines[index + 1:]:
            if LANDMARK_RE.match(later):
                break
            body.append(later)
        payloads.setdefault(match.group(1), []).append('\n'.join(body))
    return payloads


def classify_run(code, log, binding):
    """Strict verdict for one libtest run: 'pass', 'kill' or 'ambiguous'.

    Attribution is per failed test, never whole-log substring containment
    (#1109 fix-r1, review P0): the exit must be a clean 0 or 101, the status
    lines must be complete and agree with the single trailing ``test
    result:`` line, every failed test must own a panic header whose payload
    carries the bound message, and any further panic header (the bounded
    verifier thread panics before its panic is caught and forwarded, so a
    real kill legitimately prints two headers) must carry the bound message
    too. Anything else — a second failing test, a stray panic, an
    unattributable exit, a truncated report — is ambiguous, which the row
    loop records as INVALID.
    """
    lines = log.splitlines()
    report = next((index for index, line in enumerate(lines)
                   if line == 'failures:' or line.startswith('---- ')), len(lines))
    prefix = '\n'.join(lines[:report])
    running = RUNNING_RE.findall(prefix)
    status = STATUS_RE.findall(prefix)
    totals = TOTALS_RE.findall(log)
    last = next((line for line in reversed(lines) if line.strip()), '')
    if not last.startswith('test result:') or not totals:
        return 'ambiguous', 'no trailing test-result line'
    outcome, passed, failed, ignored = totals[-1]
    if len(running) != 1:
        return 'ambiguous', f'{len(running)} running headers in the report prefix'
    if len(status) != int(running[0]):
        return 'ambiguous', f'{len(status)} status lines for {running[0]} declared tests'
    seen = {}
    for _name, state in status:
        seen[state] = seen.get(state, 0) + 1
    if (seen.get('ok', 0), seen.get('FAILED', 0), seen.get('ignored', 0)) \
            != (int(passed), int(failed), int(ignored)):
        return 'ambiguous', 'status lines disagree with the test-result totals'
    if code == 0:
        if outcome == 'ok' and failed == '0':
            return 'pass', ''
        return 'ambiguous', f'exit 0 with result {outcome}, {failed} failed'
    if code != LIBTEST_FAILURE:
        return 'ambiguous', f'exit {code} is not a clean libtest failure'
    if outcome != 'FAILED':
        return 'ambiguous', f'exit {LIBTEST_FAILURE} with result {outcome}'
    failed_tests = [name for name, state in status if state == 'FAILED']
    if not failed_tests:
        return 'ambiguous', 'exit 101 without a failed test'
    payloads = panic_payloads(log)
    for name in failed_tests:
        if name not in payloads:
            return 'ambiguous', f'{name} failed without owning a panic header'
        if not any(binding in payload for payload in payloads[name]):
            return 'ambiguous', f'{name} panicked without the bound message'
    for name, bodies in payloads.items():
        if name in failed_tests:
            continue
        if not all(binding in payload for payload in bodies):
            return 'ambiguous', (f'panic header {name!r} outside the failed tests '
                                f'{failed_tests} does not carry the bound message')
    return 'kill', ''


def locate_binary(target_dir, crate):
    """Newest executable test binary for `crate`, as stress-bin.sh picks it."""
    deps = target_dir / 'debug' / 'deps'
    candidates = sorted(
        (path.stat().st_mtime, path)
        for path in deps.glob(f'{crate}-*')
        if path.is_file() and path.stat().st_mode & 0o111 and '.' not in path.name
    )
    if not candidates:
        raise RuntimeError(f'no {crate} test binary under {deps}; run cargo test --no-run')
    return candidates[-1][1]


def positive_int(text):
    value = int(text)
    if value < 1:
        raise argparse.ArgumentTypeError(f'must be a positive integer, got {text}')
    return value


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', type=Path, help='fresh evidence directory')
    parser.add_argument('mutations', nargs='*')
    parser.add_argument('--baseline', default='HEAD',
                        help='tree state the mutated files must match')
    parser.add_argument('--transcript-stress', type=positive_int,
                        default=TRANSCRIPT_STRESS)
    parser.add_argument('--worker-stress', type=positive_int, default=WORKER_STRESS)
    args = parser.parse_args()
    if not os.environ.get('CARGO_TARGET_DIR'):
        parser.error('CARGO_TARGET_DIR must be supplied by the caller')
    if not os.environ.get('PDRS_BASELINE'):
        parser.error('PDRS_BASELINE must point at the fixed TS checkout')
    target_dir = Path(os.environ['CARGO_TARGET_DIR']).resolve()
    root = Path(__file__).resolve().parents[5]
    baseline_path = Path(os.environ['PDRS_BASELINE'])
    transcript_source = root / 'src/pilotdeck-gateway/tests/transcript_resolve.rs'
    baseline_source = root / 'src/pilotdeck-gateway/tests/common/baseline.rs'
    signals_source = root / 'src/pilotdeck-gateway/tests/common/child_signals.rs'
    tasks_source = root / 'src/pilotdeck-gateway/tests/background_tasks.rs'
    tasks_fixture = root / 'src/pilotdeck-gateway/tests/fixtures/background-tasks/oracle.json'
    env = os.environ.copy()
    env['CARGO_PROFILE_DEV_DEBUG'] = 'line-tables-only'
    env['CARGO_PROFILE_TEST_DEBUG'] = 'line-tables-only'
    mutations = [
        # The regression itself: children of the bounded parent re-run the
        # git probe table; under load the 30 s bound kills them mid-verification.
        dict(name='transcript-child-reverification',
             changes=[(transcript_source, TRANSCRIPT_REMOVE, '')],
             crate=TRANSCRIPT_BINARY, extra=[], count=args.transcript_stress,
             binding='#1109 matrix child timed out', with_baseline=True),
        # The verification phase must be bounded and must fail loudly.
        dict(name='transcript-verify-bound',
             changes=[(baseline_source,
                       'const VERIFY_BOUND: Duration = Duration::from_secs(300);',
                       'const VERIFY_BOUND: Duration = Duration::from_secs(0);')],
             crate=TRANSCRIPT_BINARY, extra=['--exact', 'relative_base_normal_cwd'],
             count=1, binding='#1109 baseline verification exceeded', with_baseline=True),
        dict(name='worker-reverification',
             changes=[(signals_source, WORKER_REMOVE, '')],
             crate=WORKER_BINARY, extra=['--exact', WORKER_TEST],
             count=args.worker_stress,
             binding='signal-injected worker inherited_exit_statuses_worker silent for 30s',
             with_baseline=True),
        # Duration::from_secs(0) debug-formats as 0ns, not 0s.
        dict(name='worker-silent-window',
             changes=[(signals_source,
                       'let silent_bound = Duration::from_secs(30);',
                       'let silent_bound = Duration::from_secs(0);')],
             crate=WORKER_BINARY, extra=['--exact', WORKER_TEST], count=1,
             binding='silent for 0ns within the 300s total bound', with_baseline=True),
        dict(name='worker-total-bound',
             changes=[(signals_source,
                       'let total_bound = Duration::from_secs(300);',
                       'let total_bound = Duration::from_secs(0);')],
             crate=WORKER_BINARY, extra=['--exact', WORKER_TEST], count=1,
             binding='silent for 30s within the 0ns total bound', with_baseline=True),
        # The receipts must flow: severing them degrades the bounds back to
        # wall clock alone, and the parent must reject that.
        dict(name='progress-receipt-severed',
             changes=[(tasks_source, RECEIPT_PRINT, '')],
             crate=WORKER_BINARY, extra=['--exact', WORKER_TEST], count=1,
             binding='#1109 worker emitted no sequence progress receipts',
             with_baseline=True),
        # fix-r1 (review P0): without PDRS_BASELINE the static provenance
        # checks must still fire. A bad commit must fail the default run.
        # The retraction severs the new unconditional static check and must
        # re-open the hole (the same corruption then passes).
        dict(name='parent-static-commit',
             changes=[(tasks_fixture, COMMIT_LINE, BAD_COMMIT_LINE)],
             crate=WORKER_BINARY, extra=['--exact', WORKER_TEST], count=1,
             binding='oracle baseline commit', with_baseline=False,
             retractions=[dict(name='static-commit-check-severed',
                               changes=[(baseline_source, STATIC_CHECK_ARM,
                                         '        None => {}')])]),
        # fix-r1 (review P0): an emptied hash table must fail the default run.
        dict(name='parent-static-empty-hashes',
             changes=[(tasks_fixture, HASHES_OPEN, HASHES_EMPTIED)],
             crate=WORKER_BINARY, extra=['--exact', WORKER_TEST], count=1,
             binding='oracle source hashes must not be empty', with_baseline=False,
             retractions=[dict(name='static-empty-hashes-check-severed',
                               changes=[(baseline_source, STATIC_CHECK_ARM,
                                         '        None => {}')])]),
        # fix-r1 (review P1): a well-formed but wrong recorded hash is
        # invisible to the static checks; the parent's bounded git
        # verification must catch it. Both retractions must re-open the
        # hole: deleting the parent's verification, and swallowing the
        # forwarded verification panic, each make the same corruption pass.
        dict(name='parent-hash-mismatch',
             changes=[(tasks_fixture, HASHED_SOURCE, TAMPERED_HASH)],
             crate=WORKER_BINARY, extra=['--exact', WORKER_TEST], count=1,
             binding='oracle hash for', with_baseline=True,
             retractions=[dict(name='parent-verification-severed',
                               changes=[(tasks_source, PARENT_VERIFY, '')]),
                          dict(name='verification-panic-swallowed',
                               changes=[(baseline_source, SWALLOWED_ARM,
                                         '            Ok(Err(_message)) => {}')])]),
        # fix-r1 (review P1): retracting the progress-aware total bound back
        # to the pre-#1109 flat 30 s must reproduce the #978 kill under the
        # same stress batch (the worker's real replay work outgrows 30 s).
        dict(name='worker-flat-30s',
             changes=[(signals_source,
                       'let total_bound = Duration::from_secs(300);',
                       'let total_bound = Duration::from_secs(30);')],
             crate=WORKER_BINARY, extra=['--exact', WORKER_TEST],
             count=args.worker_stress, binding='within the 30s total bound',
             with_baseline=True),
    ]
    if args.mutations:
        selected = set(args.mutations)
        if not selected <= {row['name'] for row in mutations}:
            parser.error('unknown mutation name')
        mutations = [row for row in mutations if row['name'] in selected]
    args.output.mkdir(parents=True, exist_ok=False)

    def save(name, text):
        with (args.output / name).open('x') as stream:
            stream.write(text)

    def build(crate, label):
        slots = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=600, interval=10)
        save(f'{label}.slots.log', slots.stdout + slots.stderr)
        # No -q: a quiet cargo hides the compile failure that makes a row
        # INVALID instead of KILLED (#1109 rule 4: compile-check every mutant).
        result = run_owned([
            'cargo', 'test', '--manifest-path', 'src/Cargo.toml', '-p', 'pilotdeck-gateway',
            '--test', crate, '--no-run',
        ], cwd=root, env=env, timeout=900)
        log = result.stdout + result.stderr
        save(f'{label}.log', log)
        save(f'{label}.exit', f'{result.returncode}\n')
        return result.returncode == 0

    def apply_edits(edits):
        """Apply every (path, old, new) exactly once; refuse a moved anchor."""
        for path, old, new in edits:
            current = path.read_text()
            if current.count(old) != 1:
                raise RuntimeError(f'anchor count {current.count(old)} != 1 '
                                   f'for {old!r} in {path}')
            path.write_text(current.replace(old, new, 1))

    def save_diff(prefix, texts, edits):
        for path, old, new in edits:
            changed = texts[path].replace(old, new, 1)
            save(f'{prefix}--{path.name}.patch', ''.join(difflib.unified_diff(
                texts[path].splitlines(True), changed.splitlines(True),
                fromfile=str(path), tofile=str(path))))

    def terminate_group(process):
        """Kill the run's whole process group; it leads its own session."""
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except OSError:
            try:
                process.kill()
            except OSError:
                pass

    def stress(crate, extra, count, label, with_baseline):
        """`count` concurrent direct-binary runs; returns [(exit code, log)].

        fix-r1 (review P1): every run leads its own session, so its whole
        process group is this runner's to kill; the batch has one total
        deadline and every wait is bounded. On any exit path — deadline,
        spawn failure, Ctrl-C — the finally clause kills and reaps every
        started group before the caller restores the mutated sources, so no
        run outlives the mutation window it was proving. (A SIGKILLed runner
        cannot clean up; that residue is left for a human, like the source
        bytes it also leaves behind.)
        """
        binary = locate_binary(target_dir, crate)
        runs = []
        deadline = time.monotonic() + RUN_DEADLINE
        try:
            for index in range(count):
                home = args.output / f'{label}-run-{index}.home'
                home.mkdir()
                run_env = dict(os.environ)
                run_env.update(HOME=str(home), PILOT_HOME=str(home / '.pilotdeck'))
                # fix-r1 (review P0): the static-provenance rows must run
                # without PDRS_BASELINE to reach the no-baseline branch.
                if with_baseline:
                    run_env['PDRS_BASELINE'] = str(baseline_path)
                else:
                    run_env.pop('PDRS_BASELINE', None)
                log = args.output / f'{label}-run-{index}.log'
                with log.open('w') as sink:
                    process = subprocess.Popen(
                        [str(binary), *extra], cwd=root / 'src/pilotdeck-gateway',
                        env=run_env, stdout=sink, stderr=subprocess.STDOUT,
                        start_new_session=True)
                runs.append((process, log))
            results = []
            for process, log in runs:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise TimeoutError(
                        f'{label}: stress batch exceeded the {RUN_DEADLINE}s total deadline')
                code = process.wait(timeout=remaining)
                results.append((code, log.read_text()))
            return results
        finally:
            for process, _log in runs:
                if process.poll() is None:
                    terminate_group(process)
                try:
                    process.wait(timeout=REAP_GRACE)
                except subprocess.TimeoutExpired:
                    pass  # already SIGKILLed; init reaps it. Never unbounded.

    def judged(crate, extra, count, label, binding, with_baseline):
        """Run `count` stressed runs and classify each one strictly.

        fix-r1 (review P0): a run only counts as killed when every failed
        test in it panicked at the bound message; any other failure — or an
        unattributable exit — makes the run ambiguous, never killed. The
        executor must also have run exactly the requested positive count.
        """
        runs = stress(crate, extra, count, label, with_baseline)
        if count < 1 or len(runs) != count:
            raise RuntimeError(f'{label}: {len(runs)}/{count} runs executed')
        passed = killed = 0
        reasons = []
        for code, log in runs:
            state, reason = classify_run(code, log, binding)
            if state == 'pass':
                passed += 1
            elif state == 'kill':
                killed += 1
            else:
                reasons.append(f'exit {code}: {reason}')
        return runs, passed, killed, reasons

    summary = []
    for row in mutations:
        name = row['name']
        record = {'name': name, 'status': 'INVALID', 'reason': ''}
        summary.append(record)
        try:
            count = row['count']
            if count < 1:
                record['reason'] = f'count {count} is not a positive run count'
                continue
            if count > 1 and count < STRESS_FLOOR:
                record['reason'] = (f'stress row needs at least {STRESS_FLOOR} runs; '
                                    'the majority criterion is meaningless below')
                continue
            files = sorted({edit[0] for edit in row['changes']}
                           | {edit[0] for other in row.get('retractions', [])
                              for edit in other['changes']})
            with mutating_sources(root, files, baseline=args.baseline) as snapshots:
                pristine = dict(snapshots)
                texts = {path: blob.decode() for path, blob in pristine.items()}
                # Every anchor, negative and retraction, must hit exactly
                # once in the pristine tree; a stale anchor is a script
                # defect, never a SURVIVED row.
                stale = [f'{label}:{path.name}'
                         for label, edits in
                         [('negative', row['changes']),
                          *((f"retraction-{other['name']}", other['changes'])
                            for other in row.get('retractions', []))]
                         for path, old, _new in edits
                         if texts[path].count(old) != 1]
                if stale:
                    record['reason'] = f'anchor count != 1: {stale}'
                    continue
                if not build(row['crate'], f'{name}-positive-build'):
                    record['reason'] = 'positive build failed'
                    continue
                runs, passed, _killed, reasons = judged(
                    row['crate'], row['extra'], count, f'{name}-positive',
                    row['binding'], row['with_baseline'])
                # The stress batch itself is the load source, so a positive
                # run can flake under it (#978-class flakiness). That is a
                # rerun request (INVALID), never a pass — it cannot produce
                # a false KILLED verdict either way.
                if passed != len(runs):
                    record['reason'] = (f'positive control: {passed}/{len(runs)} runs passed'
                                       + (f'; first: {reasons[0]}' if reasons else ''))
                    continue
                apply_edits(row['changes'])
                save_diff(f'{name}-negative', texts, row['changes'])
                if not build(row['crate'], f'{name}-negative-build'):
                    record['reason'] = 'mutant does not compile'
                    continue
                runs, passed, killed, reasons = judged(
                    row['crate'], row['extra'], count, f'{name}-negative',
                    row['binding'], row['with_baseline'])
                record.update(runs=len(runs), positive=passed, negativeKilled=killed,
                              exitCodes=[code for code, _log in runs],
                              files={path.relative_to(root).as_posix():
                                     hashlib.sha256(blob).hexdigest()
                                     for path, blob in pristine.items()})
                ambiguous = len(runs) - killed - passed
                if ambiguous:
                    record['status'] = 'INVALID'
                    record['reason'] = (f'{ambiguous}/{len(runs)} runs failed outside '
                                        f'the bound message: {reasons[:2]}')
                elif killed == len(runs) or (count > 1 and killed * 2 >= len(runs)):
                    record['status'] = 'KILLED'
                    record['reason'] = (f'{killed}/{len(runs)} runs failed at the bound '
                                       f'message ({passed} passed under load)')
                    # Retraction attribution (fix-r1, review P1): with the
                    # named rewrite severed on top of the same mutation the
                    # test must pass again; otherwise the kill above is not
                    # bound to that rewrite.
                    for other in row.get('retractions', []):
                        for path, blob in pristine.items():
                            path.write_bytes(blob)
                        apply_edits(row['changes'] + other['changes'])
                        save_diff(f"{name}-{other['name']}", texts,
                                  row['changes'] + other['changes'])
                        if not build(row['crate'], f"{name}-{other['name']}-build"):
                            record['status'] = 'INVALID'
                            record['reason'] = (
                                f"retraction {other['name']} mutant does not compile")
                            break
                        _runs, passed, killed2, reasons = judged(
                            row['crate'], row['extra'], 1, f"{name}-{other['name']}",
                            row['binding'], row['with_baseline'])
                        if passed != 1:
                            record['status'] = 'INVALID'
                            record['reason'] = (f"retraction {other['name']}: expected the "
                                               f"severed rewrite to pass, got {passed}/1 "
                                               f"passed, {killed2}/1 killed"
                                               + (f'; {reasons[0]}' if reasons else ''))
                            break
                        record.setdefault('retractions', []).append(
                            f"{other['name']}: passed (hole re-opened as designed)")
                else:
                    record['status'] = 'SURVIVED'
                    record['reason'] = (f'{killed}/{len(runs)} runs failed at the bound '
                                       f'message, {passed} passed')
            # mutating_sources's finally restored and byte-checked the
            # sources; this rerun proves the restored tree passes again.
            with mutating_sources(root, files, baseline=args.baseline):
                if not build(row['crate'], f'{name}-restored-build'):
                    record['status'] = 'INVALID'
                    record['reason'] = 'restored build failed'
                    continue
                _runs, passed, _killed, _reasons = judged(
                    row['crate'], row['extra'], 1, f'{name}-restored',
                    row['binding'], row['with_baseline'])
                if passed != 1:
                    record['status'] = 'INVALID'
                    record['reason'] = f'restored control: {passed}/1 runs passed'
        except BaseException as error:  # noqa: BLE001 - recorded, never silent
            record['status'] = 'INVALID'
            record['reason'] = f'{type(error).__name__}: {error}'
        finally:
            save(f'{name}.json', json.dumps(record, indent=2) + '\n')
            print(f"{name}: {record['status']}: {record['reason']}", flush=True)
    counts = {state: sum(row['status'] == state for row in summary)
              for state in ('KILLED', 'SURVIVED', 'INVALID')}
    save('summary.json', json.dumps({'controls': summary, 'counts': counts}, indent=2) + '\n')
    print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()),
          flush=True)
    # Never stop at the first failure, never exit 0 while any row is not
    # KILLED, and never exit 0 from a run that killed nothing.
    return int(not summary or counts['KILLED'] == 0
               or any(row['status'] != 'KILLED' for row in summary))


if __name__ == '__main__':
    sys.exit(main())
