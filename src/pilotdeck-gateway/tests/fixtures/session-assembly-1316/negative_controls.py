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
import contextlib
import difflib
import fcntl
import hashlib
import json
import math
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_panic import named_test_failed, panic_evidence
from negative_selection import baseline_passed
from negative_process import (CARGO_SLOTS_WAIT_SECONDS, mutating_sources, run_owned,
                              tree_lock, wait_for_slot)

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
# One source of truth for the slot wait: the shared constant (#1692 §2/r14).
SLOT_TIMEOUT_SECONDS = CARGO_SLOTS_WAIT_SECONDS


def acquire_slot(script=None, timeout=SLOT_TIMEOUT_SECONDS):
    """Wait for a cargo slot; the deadline is an explicit exit, never an assert."""
    if not math.isfinite(timeout) or timeout <= 0:
        # float('inf')/nan 会让 deadline 比较永远成立（或永远不成立），必须先拒。
        sys.exit(f'slot timeout must be finite and positive: {timeout!r}')
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


# One cargo call: 600 s is ~4x the measured 2 min 22 s per call in a green CI
# run (GitHub Actions run 42125640997) and still leaves the overall deadline as
# the binding bound (#1692 §1).
CARGO_TEST_TIMEOUT_SECONDS = 600

# Overall wall-clock budget for the whole fixture (#1692 §1). The job's
# `timeout-minutes` must stay ABOVE this plus the upload. Measured on this repo
# (run 42176640534, probe job `timeout-minutes: 1`): a job timeout cancels the
# running step but `if: always()` steps still run and the artifact still
# uploads - so the evidence is NOT lost, and the real damage is different: the
# runner kills the process wherever it is, so no source is restored, no
# progress.jsonl / summary.json is written, and the failure reads as
# `cancelled` instead of a bounded red. Python's default SIGTERM handling has
# the same shape. Both are bounded here instead: every cargo / slot call is
# capped by the remaining budget, and a deadline or SIGTERM restores the
# sources, publishes a TIMEOUT summary and exits non-zero. Default 90 min;
# CI job 120 min.
DEADLINE_SECONDS = 5400
DEADLINE_ENV = 'PDRS_NEGCTL_DEADLINE'

# Set in the main run (and by the self-check probe); `bounded` refuses every
# subprocess after it expires. None means "no deadline" (self-check modes that
# run before the main path).
deadline = None
deadline_budget = DEADLINE_SECONDS

# A runner cancels with SIGINT first and SIGTERM shortly after; both must be
# blocked while the mutated sources are restored and the summary published,
# otherwise the restore is cut in half and the TIMEOUT summary hides it.
STOP_SIGNALS = (signal.SIGTERM, signal.SIGINT)


def mask_stop_signals():
    """Stop a cancellation from preempting the restore / summary window."""
    signal.pthread_sigmask(signal.SIG_BLOCK, STOP_SIGNALS)


@contextlib.contextmanager
def restore_window():
    """Mask cancellation as soon as the body ends.

    Enters the `with` innermost, so on the way out it runs BEFORE
    `mutating_sources.__exit__`: the sources are restored and re-verified with
    both stop signals blocked, and `stop_run` unblocks them afterwards.
    """
    try:
        yield
    finally:
        mask_stop_signals()


class RunStopped(BaseException):
    """Stop the run without being swallowed by the per-mutant `except Exception`."""


class RunDeadlineExceeded(RunStopped):
    pass


class RunTerminated(RunStopped):
    pass


def deadline_seconds(value=None):
    """Parse the overall budget from PDRS_NEGCTL_DEADLINE; fail closed."""
    raw = os.environ.get(DEADLINE_ENV) if value is None else value
    if raw is None or raw == '':
        return DEADLINE_SECONDS
    # Plain ASCII digits only: `int()` would also take '1_000', ' 120 ', '+120'
    # and non-ASCII digits, which is wider than the "whole seconds" contract.
    if not isinstance(raw, str) or not raw.isascii() or not raw.isdigit():
        raise ValueError(f'{DEADLINE_ENV} must be whole seconds, got {raw!r}')
    seconds = int(raw)
    if seconds <= 0:
        raise ValueError(f'{DEADLINE_ENV} must be positive, got {raw!r}')
    return seconds


def bounded(nominal, what):
    """Cap one subprocess by the remaining overall budget; refuse past it."""
    left = deadline - time.monotonic()
    if left <= 0:
        raise RunDeadlineExceeded(
            f'{what}: overall deadline of {deadline_budget}s reached')
    return min(nominal, left)


def _stop_on_signal(signum, _frame):
    # Python's default SIGTERM/SIGINT handling terminates immediately: the
    # `finally` that restores a mutated source never runs. Raising a
    # BaseException runs the restore and the TIMEOUT summary instead (#1692 §1).
    # A cancellation is normally SIGINT followed by SIGTERM, so the FIRST one
    # must disarm both: a second raise inside the restore would leave the tree
    # mutated while still publishing a summary (review P1).
    for sig in STOP_SIGNALS:
        signal.signal(sig, signal.SIG_IGN)
    raise RunTerminated(f'signal {signum} before the run finished')


def run(full):
    # No -q/--quiet: libtest must print `test <name> ... ok` lines (#273, #978).
    # --locked: the gate runs on the committed lockfile, like the shared entry
    # that builds the scanner (`negative_process.py` uses --locked for the same
    # reason). A timeout keeps the cargo output it collected: a bare
    # TimeoutExpired traceback would leave the run red with no evidence at all
    # (#1632 review). The per-call cap is the binding bound; `idle_timeout`
    # would never fire below it, so a silent compile is bounded by the cap.
    try:
        result = run_owned(
            ['cargo', 'test', '--locked', '--manifest-path', 'src/Cargo.toml', '-p',
             'pilotdeck-gateway', '--lib', full, '--', '--exact'],
            cwd=root, env=env,
            timeout=bounded(CARGO_TEST_TIMEOUT_SECONDS, 'cargo test'))
        return result.returncode, result.stdout + result.stderr
    except subprocess.TimeoutExpired as error:
        # If the cap WAS the remaining budget, the deadline is what ended this
        # call: report TIMEOUT rather than a plain INVALID mutant (review P1).
        if deadline - time.monotonic() <= 0:
            raise RunDeadlineExceeded(
                f'cargo test: overall deadline of {deadline_budget}s reached')
        collected = (error.output or '') + (error.stderr or '')
        return 124, f'{collected}\nTIMEOUT after {error.timeout}s\n'


def take_slot(name):
    try:
        slots = acquire_slot(timeout=bounded(SLOT_TIMEOUT_SECONDS, 'cargo slot wait'))
    except SystemExit:
        # `acquire_slot` exits on its own for a broken / busy gate. When the
        # overall budget is what ran out, that is a TIMEOUT, not a gate fault:
        # route it through the graceful path so a SUMMARY is still published.
        if deadline - time.monotonic() <= 0:
            raise RunDeadlineExceeded(
                f'{name}: overall deadline of {deadline_budget}s reached while '
                'waiting for a cargo slot')
        raise
    (out / f'{name}.slots.log').write_text(slots.stdout + slots.stderr)


# The append-only progress journal (#1692 §4). summary.json stays atomic and is
# still published only after every mutant finished; a cancelled run keeps this
# journal so its coverage is auditable. Flush + fsync per line: a killed job
# must not lose the last records in Python's buffer. Both start as None because
# the self-check modes run before the output directory exists.
out = None
progress = None
env = None


def append_progress(**fields):
    if progress is None:
        return
    row = dict(timestamp=round(time.time(), 3), **fields)
    progress.write(json.dumps(row, ensure_ascii=False) + '\n')
    progress.flush()
    os.fsync(progress.fileno())


@contextlib.contextmanager
def stop_run():
    """Record a deadline / SIGTERM as `timed_out` instead of killing the run.

    Entered OUTSIDE `mutating_sources`, so the sources are restored and
    re-verified before this handler runs: a stopped run never publishes a
    summary while a mutant is still applied (#979 P0 / #1076).
    """
    global timed_out
    try:
        yield
    except RunStopped as error:
        timed_out = f'{type(error).__name__}: {error}'
        print(f'run stopped before finishing: {timed_out}', file=sys.stderr, flush=True)
    finally:
        # The restore is done and verified by now (the body masks signals while
        # it unwinds). From here on a cancellation would only truncate the
        # summary, so ignore both and drop anything that arrived while masked.
        for sig in STOP_SIGNALS:
            signal.signal(sig, signal.SIG_IGN)
        signal.pthread_sigmask(signal.SIG_UNBLOCK, STOP_SIGNALS)


def publish_summary(rows, timed_out=None):
    """Publish summary.json atomically; a deadline adds an explicit TIMEOUT row."""
    published = list(rows)
    if timed_out is not None:
        published.append(dict(name='run', status='TIMEOUT', reason=timed_out))
    temporary = out / 'summary.json.tmp'
    temporary.write_text(json.dumps(published, ensure_ascii=False, indent=2) + '\n')
    temporary.replace(out / 'summary.json')
    counts = {state: sum(row['status'] == state for row in published)
              for state in ('KILLED', 'SURVIVED', 'INVALID', 'TIMEOUT')}
    print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()),
          flush=True)
    return counts


def deadline_probe():
    """#1692 §1/§4: the deadline refuses to spawn and still leaves evidence.

    Runs without cargo: an expired budget must raise before `run_owned` /
    `wait_for_slot` are reached (deleting the bound would really spawn cargo),
    progress.jsonl must be readable immediately after a write, and a deadline
    must publish a TIMEOUT summary atomically.
    """
    global deadline, deadline_budget, out, progress, env, run_owned, wait_for_slot
    problems = []
    saved = (deadline, deadline_budget, out, progress, env, run_owned, wait_for_slot)
    with tempfile.TemporaryDirectory(prefix='negctl-1316-deadline-') as directory:
        try:
            out = Path(directory)
            env = os.environ.copy()
            progress = (out / 'progress.jsonl').open('a', encoding='utf-8')
            deadline_budget = DEADLINE_SECONDS
            deadline = time.monotonic()
            spawned = []

            def refuse(*args, **kwargs):
                spawned.append(args)
                raise AssertionError('a subprocess was spawned after the deadline')

            # Swap the two spawning helpers for a refusal without any dynamic
            # module access: `anchor_selfcheck` rejects setattr/globals()/
            # sys.modules[...] in checked scripts (#996 fix-r2), so this is a
            # plain rebinding of two module-level names, restored below.
            run_owned = refuse
            wait_for_slot = refuse
            for label, call in (('cargo test', lambda: run('probe::test')),
                                ('slot wait', lambda: take_slot('probe'))):
                try:
                    call()
                except RunDeadlineExceeded as error:
                    print(f'deadline probe {label}: refused ({error})', flush=True)
                except AssertionError as error:
                    problems.append(f'{label}: {error}')
                except SystemExit as error:
                    # A different fail-closed path caught it (e.g. a negative
                    # timeout in `acquire_slot`). Still red, but not proof
                    # that THIS refusal works, so it is not a pass.
                    problems.append(f'{label}: exited instead of refusing: {error}')
                else:
                    problems.append(f'{label}: an expired deadline did not refuse')
            if spawned:
                problems.append(f'{len(spawned)} subprocess start(s) slipped past the deadline')
            append_progress(event='probe', status='OK')
            lines = (out / 'progress.jsonl').read_text().splitlines()
            if not lines or json.loads(lines[-1]).get('event') != 'probe':
                problems.append('progress.jsonl: the appended record was not readable at once')
            else:
                print('deadline probe progress.jsonl: one flushed record readable', flush=True)
            publish_summary([dict(name='probe', status='KILLED')], timed_out='probe deadline')
            published = json.loads((out / 'summary.json').read_text())
            if not any(row.get('status') == 'TIMEOUT' for row in published):
                problems.append('summary.json: a deadline published no TIMEOUT row')
            if (out / 'summary.json.tmp').exists():
                problems.append('summary.json.tmp: the summary was not renamed atomically')
            else:
                print('deadline probe summary.json: TIMEOUT row published atomically', flush=True)
            for bad in ('0', '-3', 'abc', '1.5'):
                try:
                    deadline_seconds(bad)
                except ValueError:
                    pass
                else:
                    problems.append(f'{DEADLINE_ENV}={bad!r} was not refused')
            if deadline_seconds('120') != 120:
                problems.append(f'{DEADLINE_ENV} did not parse whole seconds')
        finally:
            if progress is not None:
                progress.close()
            deadline, deadline_budget, out, progress, env, run_owned, wait_for_slot = saved
    return problems


def self_check(timeout=2, wall=60):
    """Negative control for the slot guard: `-O` must not change the exit (#1632).

    `scripts/cargo-slots.sh` documents 0/1/2/3/4/64/69 (see its 退出码 table) and
    `wait_for_slot` splits them three ways: 0 and 69 (EX_UNAVAILABLE = no unified
    gate on this machine, which is exactly the CI runner) proceed, 1 retries, and
    2/3/4/64 abort. Every one of those codes is driven through a stub gate here,
    in child interpreters both plain and under `-O`. The old assert-based deadline
    never returned under `-O`, so the bounded child would have to be killed at
    `wall` and this check would report the hang instead of passing; likewise,
    reading a broken gate as "0 slots in use" would silently run cargo anyway.
    """
    # name -> (gate script, expected exit, expected stderr fragment)
    gates = {
        'free': ('#!/bin/sh\necho "cargo-slots: 0 / cap 1"\nexit 0\n', 0, 'slot free'),
        # 69 = no unified gate here: no shared quota to wait for, so proceed. This is
        # the code a CI runner really hits, so it must not be left untested.
        'no-gate': ('#!/bin/sh\nexit 69\n', 0, 'slot free'),
        'busy': ('#!/bin/sh\necho "cargo-slots: 1 / cap 1"\nexit 1\n',
                 1, 'cargo slots unavailable'),
        # 2 is what bash itself returns for a broken script; cargo-slots.sh documents
        # it as reserved ("本脚本从不主动使用"), so the stub stands in for bash.
        'syntax-error': ('#!/bin/sh\necho "脚本语法错" >&2\nexit 2\n',
                         1, 'cargo slot gate failed with rc=2'),
        'ps-failed': ('#!/bin/sh\necho "ps 取样失败" >&2\nexit 3\n',
                      1, 'cargo slot gate failed with rc=3'),
        'gate-broken': ('#!/bin/sh\necho "闸给不出名额" >&2\nexit 4\n',
                        1, 'cargo slot gate failed with rc=4'),
        'usage': ('#!/bin/sh\necho "用法错" >&2\nexit 64\n',
                  1, 'cargo slot gate failed with rc=64'),
    }
    # #1692 §8: an emptied table made the loop below run zero times, `problems`
    # stayed empty and self-check printed "handles free/full/broken gates" with
    # exit 0. Fail closed at the use site, like the other literal tables here.
    if not gates:
        sys.exit('no slot guard cases defined: self-check would pass without testing the guard')
    problems = []
    with tempfile.TemporaryDirectory(prefix='negctl-1316-slot-') as directory:
        for name, (text, expected, needle) in gates.items():
            gate = Path(directory) / f'cargo-slots-{name}.sh'
            gate.write_text(text)
            gate.chmod(0o755)
            for flags in ([], ['-O']):
                label = f'{name} ' + ' '.join(['python3'] + flags)
                started = time.monotonic()
                try:
                    result = run_owned(
                        [sys.executable, '-I', *flags, str(Path(__file__).resolve()),
                         '--slot-guard-probe', str(gate), str(timeout)],
                        timeout=wall)
                except subprocess.TimeoutExpired:
                    problems.append(f'{label}: the slot guard never returned within {wall}s')
                    continue
                elapsed = time.monotonic() - started
                haystack = result.stdout + result.stderr
                if result.returncode != expected:
                    problems.append(f'{label}: expected rc={expected}, '
                                    f'got rc={result.returncode}')
                elif needle not in haystack:
                    problems.append(f'{label}: exit did not report {needle!r}')
                elif elapsed >= timeout + 3:
                    # "Bounded" must mean bounded: the child may only get the
                    # declared deadline plus process-startup slack (#1692 §6).
                    problems.append(f'{label}: took {elapsed:.1f}s for a {timeout}s deadline')
                else:
                    print(f'self-check {label}: exit rc={expected} in {elapsed:.1f}s', flush=True)
        # #1692 §6: prove the wait is still bounded while this process holds the
        # source-tree lock, which is exactly the state the fixture runs in. The
        # second flock also proves the lock was really held during the child.
        lock_gate = Path(directory) / 'cargo-slots-locked.sh'
        lock_gate.write_text('#!/bin/sh\necho "cargo-slots: 1 / cap 1"\nexit 1\n')
        lock_gate.chmod(0o755)
        started = time.monotonic()
        result = None
        with tree_lock(root, timeout=10):
            held_fd = os.open(root / '.negative-source.lock', os.O_RDWR)
            try:
                try:
                    fcntl.flock(held_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
                except BlockingIOError:
                    pass
                else:
                    problems.append('tree-lock case: the tree lock was not actually held')
                try:
                    result = run_owned(
                        [sys.executable, '-I', str(Path(__file__).resolve()),
                         '--slot-guard-probe', str(lock_gate), str(timeout)],
                        timeout=wall)
                except subprocess.TimeoutExpired:
                    result = None
            finally:
                os.close(held_fd)
        elapsed = time.monotonic() - started
        if result is None:
            problems.append(f'tree-lock case: the guard never returned within {wall}s')
        else:
            haystack = result.stdout + result.stderr
            if result.returncode != 1 or 'cargo slots unavailable' not in haystack:
                problems.append('tree-lock case: the guard was not bounded while the tree '
                                f'lock was held (rc={result.returncode})')
            elif elapsed >= timeout + 3:
                problems.append(f'tree-lock case: took {elapsed:.1f}s for a {timeout}s '
                                'deadline while holding the tree lock')
            else:
                print(f'self-check tree-lock probe: exit rc=1 in {elapsed:.1f}s '
                      'while the lock was held', flush=True)
        # #1692 §8 (with the §7 gap): the self-check must drive the REAL
        # `scripts/cargo-slots.sh` at least once, not only stubs that duplicate
        # its exit-code contract. rc alone is not a verdict - 1 is also what a
        # crashed or missing gate produces - so each code must carry its own
        # diagnostic (review P2).
        started = time.monotonic()
        try:
            result = run_owned(
                [sys.executable, '-I', str(Path(__file__).resolve()), '--slot-guard-probe',
                 str(root / 'scripts/cargo-slots.sh'), str(timeout)],
                timeout=wall)
        except subprocess.TimeoutExpired:
            result = None
        elapsed = time.monotonic() - started
        if result is None:
            problems.append(f'real gate: never returned within {wall}s')
        elif result.returncode == 0:
            if 'slot free:' not in result.stdout + result.stderr:
                problems.append('real gate: rc=0 without the guard reporting a slot')
            elif elapsed >= timeout + 3:
                problems.append(f'real gate: took {elapsed:.1f}s for a {timeout}s deadline')
            else:
                print(f'self-check real cargo-slots.sh: exit rc=0 in {elapsed:.1f}s', flush=True)
        elif result.returncode == 1:
            if 'cargo slots unavailable' not in result.stdout + result.stderr:
                problems.append('real gate: rc=1 without the bounded-busy verdict '
                                '(gate broken rather than busy)')
            elif elapsed >= timeout + 3:
                problems.append(f'real gate: took {elapsed:.1f}s for a {timeout}s deadline')
            else:
                print(f'self-check real cargo-slots.sh: exit rc=1 (busy) in '
                      f'{elapsed:.1f}s', flush=True)
        else:
            problems.append('real gate: expected a bounded 0/1 verdict, '
                            f'got rc={result.returncode}')
    problems.extend(deadline_probe())
    for problem in problems:
        print('self-check FAILED: ' + problem, file=sys.stderr)
    if problems:
        return 1
    print(f'self-check: the slot guard handles free/full/broken gates with and without -O '
          f'({timeout}s deadline)', flush=True)
    return 0


# --- Meta negative controls (#1692) ---------------------------------------
# `--self-check` is itself a guard, so "delete the fix and it goes red" has to
# be shown from outside it: each case below writes a COPY of this file with one
# guard removed and runs the copy's `--self-check` in a bounded child. The copy
# rewires `sys.path` / `root` back to this checkout, so nothing in the working
# tree is touched. Each row is (name, replacements, message-expected, message):
# the guard is load-bearing exactly when removing it flips whether its own
# message appears. Any other failure of the copy is noise, not a kill.
SELF = Path(__file__).resolve()
FIXTURES = SELF.parents[1]
# Rewiring a copy: the import path goes right after the shebang (before any
# `from negative_process import` runs), and the module-level `root` / `source`
# binding is replaced outright. It has to be: the pristine line walks
# `Path(__file__).resolve().parents[5]`, which raises IndexError for a copy in a
# shallow temp directory (a Linux runner's `/tmp/xxx/copy.py` has 3 parents), so
# the copy would die on import and every case would read as a red copy. The
# rebinding also has to happen before the copy's own `mutating_sources` import
# path is used. (review P1, both families)
COPY_PATH_LINE = 'import sys as _copy_sys\n_copy_sys.path.insert(0, {!r})\n'
COPY_ROOT_ANCHOR = ("root = Path(__file__).resolve().parents[5]\n"
                    "source = root / 'src/pilotdeck-gateway/src'\n")
COPY_ROOT_LINE = ('root = Path({root!r})\n'
                  "source = root / 'src/pilotdeck-gateway/src'\n")
# The `empty-gates-guard-removed` case has to show the HOLE - a green
# `--self-check` despite an empty table - so it also points the real-gate probe
# at a stub. Otherwise an unrelated `scripts/cargo-slots.sh` verdict (busy /
# broken under load) would decide the case instead of the guard (review P2).
# The anchor carries the preceding call fragment because the bare
# `str(root / 'scripts/cargo-slots.sh')` text also appears in this very
# definition, and a non-unique anchor is INVALID, not a kill.
REAL_GATE_ARG = ("'--slot-guard-probe',\n"
                 "                 str(root / 'scripts/cargo-slots.sh'), str(timeout)],")
STUB_GATE_ARG = ("'--slot-guard-probe',\n"
                 "                 str(Path(__file__).resolve().parent "
                 "/ 'cargo-slots-free.sh'), str(timeout)],")
STUB_GATE_TEXT = '#!/bin/sh\necho "cargo-slots: 0 / cap 1"\nexit 0\n'
# Each meta-control copy runs a full `--self-check` (measured 12-13 s per copy,
# 79 s for control + 5 cases on this machine). Six copies at this cap stay well
# inside the slack the structural guard pins (`job >= deadline + 900 s`).
CONTROL_TIMEOUT_SECONDS = 120
# The emptied-table cases rewrite a 17-line literal table. Locate it by its
# opening line and the anchor line that follows it, by plain text surgery: the
# anchor self-check rejects a file that mixes literal and regex consumers.
EMPTY_GATES_TABLE = object()
GATES_TABLE_OPEN = '    gates = {\n'
GATES_TABLE_END = '    }\n    # #1692 §8:'


def _empty_gates_table(text):
    """Replace the whole slot-guard case table with `{}`; returns (text, hits)."""
    start = text.find(GATES_TABLE_OPEN)
    end = text.find(GATES_TABLE_END, start if start >= 0 else 0)
    if start < 0 or end < 0:
        return text, 0
    end += len('    }\n')
    return text[:start] + '    gates = {}\n' + text[end:], 1

self_check_controls = [
    # §1: without the refusal, `bounded` returns a cap and the probe really
    # spawns a subprocess on an expired budget.
    ('deadline-refusal-removed',
     [('    if left <= 0:\n'
       '        raise RunDeadlineExceeded(\n'
       "            f'{what}: overall deadline of {deadline_budget}s reached')\n",
       '')],
     True, 'a subprocess was spawned after the deadline', None),
    # §4: unflushed records never reach the file, so a killed job loses them.
    ('progress-flush-removed',
     [('    progress.flush()\n', '')],
     True, 'the appended record was not readable at once', None),
    # §6: the case only means something while the lock is held; dropping
    # `tree_lock` makes the parent's own LOCK_NB probe succeed.
    ('tree-lock-not-held',
     [('        with tree_lock(root, timeout=10):\n', '        if True:\n')],
     True, 'the tree lock was not actually held', None),
    # §8: an emptied table with the guard in place must be red ...
    ('empty-gates-table',
     [(EMPTY_GATES_TABLE, None)],
     True, 'no slot guard cases defined', None),
    # ... and without the guard it never fires at all: the emptied table then
    # looks exactly like a healthy run, which is the hole the guard closes. The
    # verdict is therefore "the copy is GREEN" - expecting the message to be
    # absent alone would be a tautology, since the case deletes the text that
    # prints it (review P2).
    ('empty-gates-guard-removed',
     [(EMPTY_GATES_TABLE, None),
      ('    if not gates:\n'
       "        sys.exit('no slot guard cases defined: self-check would pass "
       "without testing the guard')\n",
       ''),
      (REAL_GATE_ARG, STUB_GATE_ARG)],
     False, 'no slot guard cases defined', 0),
]

if not self_check_controls:
    sys.exit('no meta control cases defined: --self-check-controls would pass '
             'without testing a single guard')


def _self_check_variant(name, replacements, directory):
    """Write a mutated copy of this file that still points at this checkout."""
    text = SELF.read_text()
    first, rest = text.split('\n', 1)
    if not first.startswith('#!'):
        raise ValueError(f'{name}: the script no longer starts with a shebang')
    text = first + '\n' + COPY_PATH_LINE.format(str(FIXTURES)) + rest
    if text.count(COPY_ROOT_ANCHOR) != 1:
        raise ValueError(f'{name}: the module-level root binding is not unique')
    text = text.replace(COPY_ROOT_ANCHOR, COPY_ROOT_LINE.format(root=str(root)), 1)
    for old, new in replacements:
        if old is EMPTY_GATES_TABLE:
            text, count = _empty_gates_table(text)
        else:
            count = text.count(old)
            if count == 1:
                text = text.replace(old, new, 1)
        if count != 1:
            raise ValueError(f'{name}: anchor {old!r} matches {count} times')
    path = directory / f'{name}.py'
    path.write_text(text)
    return path


def self_check_controls_run():
    """Prove every self-check guard is load-bearing; returns process problems."""
    problems = []
    with tempfile.TemporaryDirectory(prefix='negctl-1316-selfcheck-') as directory:
        directory = Path(directory)
        # Free-gate stub for the case that has to end GREEN (see STUB_GATE_ARG).
        stub = directory / 'cargo-slots-free.sh'
        stub.write_text(STUB_GATE_TEXT)
        stub.chmod(0o755)
        # Control of the control: an unmutated copy must still pass, otherwise
        # every "the copy went red" verdict below would be meaningless.
        try:
            control = _self_check_variant('control', [], directory)
        except ValueError as error:
            return [str(error)]
        try:
            result = run_owned([sys.executable, '-I', str(control), '--self-check'],
                               timeout=CONTROL_TIMEOUT_SECONDS)
        except subprocess.TimeoutExpired:
            return ['control copy: --self-check did not finish within '
                    f'{CONTROL_TIMEOUT_SECONDS}s']
        if result.returncode != 0:
            return [f'control copy: an unmutated copy failed (rc={result.returncode}): '
                    + (result.stdout + result.stderr)[-400:]]
        print('self-check control copy: PASS (rc=0)', flush=True)

        for name, replacements, expect_message, message, expect_rc in self_check_controls:
            try:
                variant = _self_check_variant(name, replacements, directory)
            except ValueError as error:
                problems.append(f'{name}: INVALID {error}')
                continue
            try:
                result = run_owned([sys.executable, '-I', str(variant), '--self-check'],
                                   timeout=CONTROL_TIMEOUT_SECONDS)
            except subprocess.TimeoutExpired:
                problems.append(f'{name}: INVALID (no verdict within '
                                f'{CONTROL_TIMEOUT_SECONDS}s)')
                continue
            output = result.stdout + result.stderr
            # The verdict is the presence of the guard's own message, plus the
            # exit code when the case asserts a GREEN copy (the guard-removed
            # hole). An unrelated probe may also make a copy red, and that must
            # not be counted as this guard firing.
            found = message in output
            if found == expect_message and (expect_rc is None
                                            or result.returncode == expect_rc):
                status = 'KILLED'
            else:
                status = 'SURVIVED'
                problems.append(
                    f'{name}: SURVIVED (rc={result.returncode}, message '
                    f'{message!r} was {"present" if found else "absent"}, '
                    f'expected rc={expect_rc})')
            print(f'{name}: {status} rc={result.returncode} message='
                  f'{"present" if found else "absent"}', flush=True)
    return problems


if sys.argv[1:2] == ['--slot-guard-probe']:
    # Child mode driven by `self_check`: run the real guard against a gate that
    # is always busy. Returning here at all would mean the wait was unbounded.
    if len(sys.argv) != 4:
        sys.exit('usage: --slot-guard-probe BUSY_SLOT_SCRIPT TIMEOUT_SECONDS')
    try:
        probe_timeout = float(sys.argv[3])
    except ValueError:
        sys.exit(f'--slot-guard-probe: not a timeout: {sys.argv[3]!r}')
    slots = acquire_slot(script=Path(sys.argv[2]), timeout=probe_timeout)
    print(f'slot free: {slots.stdout.strip()}', flush=True)
    sys.exit(0)
if sys.argv[1:] == ['--self-check']:
    sys.exit(self_check())
if sys.argv[1:] == ['--self-check-controls']:
    control_problems = self_check_controls_run()
    for problem in control_problems:
        print('self-check control FAILED: ' + problem, file=sys.stderr)
    sys.exit(1 if control_problems else 0)

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

# #1692 §1: the fixture bounds ITSELF instead of trusting the job's
# `timeout-minutes`. Measured (run 42176640534): a job timeout leaves the
# `if: always()` upload working, so the damage is not a lost artifact - it is
# that the process dies between two mutants, with a mutation still applied, no
# restore, no progress journal, no summary, and a `cancelled` job that says
# nothing about coverage. So: parse the budget, install handlers that unwind
# through the restores, and publish a TIMEOUT summary + non-zero exit.
try:
    deadline_budget = deadline_seconds()
except ValueError as error:
    sys.exit(str(error))
deadline = time.monotonic() + deadline_budget
progress = (out / 'progress.jsonl').open('a', encoding='utf-8')
signal.signal(signal.SIGTERM, _stop_on_signal)
signal.signal(signal.SIGINT, _stop_on_signal)
append_progress(event='start', mutations=len(mutations),
                deadlineSeconds=deadline_budget,
                cargoTimeoutSeconds=CARGO_TEST_TIMEOUT_SECONDS)

timed_out = None
with stop_run(), mutating_sources(
        root, sorted({source / filename for _, filename, _, _, _, _ in mutations})) as snapshots, \
        restore_window():
    # #819: an unmutated baseline of every named target must be green first.
    baseline_ok = set()
    for full in dict.fromkeys(test for *_, test, _ in mutations):
        take_slot('baseline-' + full.rsplit('::', 1)[-1])
        code, log = run(full)
        (out / f'baseline-{full.rsplit("::", 1)[-1]}.log').write_text(log)
        ok = code == 0 and baseline_passed(log, full)
        print(f'baseline {full}: {"PASS" if ok else "INVALID"}', flush=True)
        append_progress(event='baseline', test=full,
                        status='PASS' if ok else 'INVALID', returncode=code)
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
            append_progress(event='mutation-start', name=name)
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
            append_progress(event='mutation', name=name, test=full, status=status,
                            returncode=code)
            if status != 'KILLED':
                failures.append(name)
        except Exception as error:
            summary.append(dict(name=name, test=full, status='INVALID',
                                reason=f'{type(error).__name__}: {error}',
                                sourceSha256=before))
            append_progress(event='mutation', name=name, test=full, status='INVALID',
                            reason=f'{type(error).__name__}: {error}')
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
# (#979 P0 / #1076). A cancellation that could not unwind the guarded window
# (a failed restore raises out of `__exit__`) still leaves no summary at all,
# and a missing summary is fail-closed. A deadline or SIGTERM DOES reach here:
# the restores already ran, so the coverage so far is published with an
# explicit TIMEOUT row and a non-zero exit (#1692 §1/§4).
counts = publish_summary(summary, timed_out=timed_out)
append_progress(event='published', **counts)
progress.close()
if timed_out:
    print(f'not finished: {timed_out}', file=sys.stderr)
    sys.exit(1)
if failures:
    print(f'not killed: {", ".join(failures)}', file=sys.stderr)
    sys.exit(1)
print(f'{len(summary)}/{len(mutations)} killed; every source restored byte-for-byte', flush=True)
