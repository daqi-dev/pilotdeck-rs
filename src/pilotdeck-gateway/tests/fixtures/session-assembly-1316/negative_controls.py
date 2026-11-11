#!/usr/bin/env python3
"""#1316 A7.8-pr1 session-domain assembly: product mutations for the wire-up.

Every mutant breaks one piece of the assembly (removes a registry entry or a
describe capability, alters an always-on method message or its envelope code,
folds the abort echo's empty string into the missing-key case, registers the
always-on family, or advertises an always-on capability no
control backs) and must
be killed by the named oracle test in the same process, with the marker of the
mutant inside that test's own panic payload. Original source bytes and hashes
are verified after every mutation; the summary is emitted only after all
mutants have run.

Run with no other writers in this worktree: the runner rewrites the guarded
sources in place (compound `with stop_run(), mutating_sources(...)`), and
`CARGO_TARGET_DIR` only isolates build artifacts, not source files -- a plain
cargo test / build sharing this tree during the mutation window compiles a
mutant and produces unrelated false failures. The exclusive
`.negative-source.lock` orders negative-control `mutating_sources` writers
among themselves; Rust read-lease consumers (`tests/common/read_lease.rs` in
the gateway and cli crates) take a shared lock on the same file, so a writer
also excludes those readers. It does not block ordinary cargo runs or
lock-free Python checkers, and its release (e.g. after a SIGKILLed runner)
does not prove the tree is clean.

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
import threading
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_panic import named_test_failed, panic_evidence
from negative_selection import baseline_passed
from negative_process import (CARGO_SLOTS_WAIT_SECONDS, mutating_sources, run_owned,
                              tree_lock, wait_for_slot, write_source_atomic)

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
    # #1352: the advertised table moved out of `requests.rs` into
    # `native_process/capabilities.rs`, so the describe anchors below follow it.
    # Renaming a row drops the name from the advertised set exactly like
    # deleting a literal line did.
    ('describe-drops-permission-decide',
     'native_process/capabilities.rs',
     'Capability { name: "permission_decide", methods: &[], wired: compiled_in },',
     'Capability { name: "unwired_permission_decide", methods: &[], wired: compiled_in },',
     'native_process::management_tests::production_host_management_and_restart',
     '#1231 exact wired capability set'),
    # Dropping the registry entry sends `always_on_apply` to the
    # `metadata::unavailable` fallback, an Err where the family answers the TS
    # not-configured result envelope (#1313 r1: the 2026-11-05
    # CAPABILITY_UNAVAILABLE rescope was lifted on 2026-11-08). The envelope
    # test's unwrap of the Ok is the discriminator that must panic, so the
    # required marker names it.
    ('registry-drops-always-on',
     'native_process/methods/mod.rs',
     '    interaction: interaction::METHODS,\n    always_on: always_on::METHODS,',
     '    interaction: interaction::METHODS,',
     'native_process::methods::always_on::tests::always_on_methods_answer_not_configured_envelopes',
     '#1313 always_on_apply message'),
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
    # #1632 P2-1 / #1476 / #1313 r1: the wire-up mutation points are the
    # interaction registry entries, the always-on per-method messages, the
    # abort arm's sessionKey read and its empty-string echo, the describe
    # capability rows, the always-on family registration, the explicit HTTP
    # store entry, the always-on envelope code and the advertised always-on
    # capability. The rows above already cover their share; the rest are
    # below. Counts live only in the pinned name table
    # (`test_metadata_io_driver.py`), never here — a hand-written count is a
    # duplicate that drifts. Each row names the assertion message the product
    # test already prints, so the kill is attributable to that one assertion.
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
     '                not_configured_error("Always-On abort is not configured on this gateway."),',
     '                not_configured_error("Always-On apply is not configured on this gateway."),',
     'native_process::methods::always_on::tests::always_on_methods_answer_not_configured_envelopes',
     '#1313 always_on_abort message'),
    ('always-on-rerun-copies-apply-message',
     'native_process/methods/always_on.rs',
     '                not_configured_error("Always-On rerun is not configured on this gateway."),',
     '                not_configured_error("Always-On apply is not configured on this gateway."),',
     'native_process::methods::always_on::tests::always_on_methods_answer_not_configured_envelopes',
     '#1313 always_on_rerun_plan message'),
    ('always-on-apply-copies-abort-message',
     'native_process/methods/always_on.rs',
     '                not_configured_error("Always-On apply is not configured on this gateway."),',
     '                not_configured_error("Always-On abort is not configured on this gateway."),',
     'native_process::methods::always_on::tests::always_on_methods_answer_not_configured_envelopes',
     '#1313 always_on_apply message'),
    # #1476 / #1313 r1: the honesty claims themselves, not just their message
    # text. The envelope row keeps the family messages and swaps only the
    # envelope's `error.code`, so it isolates the code half (the envelope test
    # compares the wire JSON byte for byte, so code and message share one
    # assertion - the marker is that assertion's message); the describe row
    # advertises one method no control backs, which is the cheapest false
    # capability claim (`assert_ts_capability_subset` only bounds the set from
    # above, so an over-claim has to be killed here).
    # #1313 r1-fix1 (DS/MM boundary P2): only the abort arms read
    # `input.sessionKey`; the nullish-params test is otherwise driven by no
    # mutation. Replacing the read with an empty object drops the boundary
    # TypeError, and the test's expect_err panic is the kill.
    # #2294 fix-r3c: the bound test drives the *unconfigured* branch (no
    # manager), so the anchor is that arm's read — the 12-space `params` line
    # inside `not_configured`'s "always_on_abort" arm. The previous 20-space
    # anchor pinned the manager arm's `&params` read, which the test never
    # reaches: CI judged the mutant SURVIVED.
    ('always-on-abort-skips-the-input-read',
     'native_process/methods/always_on.rs',
     '            let session_key = super::input_property(params, "sessionKey")?;',
     '            let session_key = super::input_property(&JsValue::Object(Default::default()), "sessionKey")?;',
     'native_process::methods::always_on::tests::always_on_abort_nullish_params_hit_the_session_key_read',
     '#1313 abort nullish answers the TypeError'),
    ('always-on-envelope-code-rewritten',
     'native_process/methods/always_on.rs',
     '        ("code", "not_configured".into()),',
     '        ("code", "NOT_CONFIGURED".into()),',
     'native_process::methods::always_on::tests::always_on_methods_answer_not_configured_envelopes',
     '#1313 always_on_apply message'),
    # #2302: the envelope test's empty-string row pins the echo predicate —
    # `!== undefined`, never a truthiness judgement that would fold `""` into
    # the missing-key case. Folding it in is the mutation; the kill is that
    # row's own assertion (the same marker the message-copy rows use, with
    # the abort method interpolated).
    ('always-on-abort-drops-the-empty-string-echo',
     'native_process/methods/always_on.rs',
     '            let mut envelope = vec![("aborted", JsValue::Bool(false))];\n'
     '            if let Some(value) = session_key.filter(|value| !value.is_undefined()) {',
     '            let mut envelope = vec![("aborted", JsValue::Bool(false))];\n'
     '            if let Some(value) = session_key.filter(|value| !value.is_undefined() '
     '&& *value != JsValue::from("")) {',
     'native_process::methods::always_on::tests::always_on_methods_answer_not_configured_envelopes',
     '#1313 always_on_abort message'),
    ('describe-advertises-always-on-apply',
     'native_process/capabilities.rs',
     'Capability { name: "supported_agents", methods: &[], wired: compiled_in },',
     'Capability { name: "supported_agents", methods: &[], wired: compiled_in },\n'
     '    Capability { name: "always_on_apply", methods: &[], wired: compiled_in },',
     'native_process::methods::always_on::tests::always_on_methods_are_not_advertised_without_a_manager',
     '#2294 always_on_apply must not be advertised without an always-on manager'),
    ('describe-drops-async-hook-result',
     'native_process/capabilities.rs',
     'name: "async_hook_result",',
     'name: "unwired_async_hook_result",',
     'native_process::management_tests::production_host_management_and_restart',
     '#1231 exact wired capability set'),
    ('describe-drops-delete-session',
     'native_process/capabilities.rs',
     'Capability { name: "delete_session", methods: &[], wired: compiled_in },',
     'Capability { name: "unwired_delete_session", methods: &[], wired: compiled_in },',
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
# CI job 150 min (deadline + the two self-check steps + upload).
DEADLINE_SECONDS = 5400
DEADLINE_ENV = 'PDRS_NEGCTL_DEADLINE'
# Upper bound for the override (#1692 review P2): `time.monotonic() + budget`
# raises OverflowError on a huge value (after the output dir was created, so the
# run dies with a traceback and no summary), and a budget past the job's own
# timeout would hand the platform the kill this deadline exists to preempt. One
# day is far above any real run and far below float overflow.
MAX_DEADLINE_SECONDS = 86400

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


# #1843 A: the mask above makes SIGTERM/SIGINT undeliverable while the sources
# are written back. Restoring is a handful of writes plus a `git show` hash
# check, but "a handful" is not a bound: a hung write or a git that blocks on a
# textconv filter would hold the masked window open with no signal left to
# interrupt it, and the runner's only remaining move is a second, SIGKILL
# attempt. SIGALRM is NOT in STOP_SIGNALS, so a watchdog handler still fires
# inside the window and can interrupt the blocking call in the main thread.
# Same class as the measured job-timeout behaviour in `DEADLINE_SECONDS`: bound
# it here, in a way the runner cannot preempt.
RESTORE_TIMEOUT_SECONDS = 120
RESTORE_ALARM = signal.SIGALRM
_armed_restore_timeout = RESTORE_TIMEOUT_SECONDS


class RestoreStalled(BaseException):
    """The masked restore window outlived its bound."""


def _restore_stalled(_signum, _frame):
    raise RestoreStalled(
        f'the source restore did not finish within {_armed_restore_timeout}s')


def arm_restore_watchdog(timeout=RESTORE_TIMEOUT_SECONDS):
    """Start the restore bound; `stop_run` disarms it on every exit path."""
    global _armed_restore_timeout
    _armed_restore_timeout = timeout
    signal.signal(RESTORE_ALARM, _restore_stalled)
    signal.setitimer(signal.ITIMER_REAL, timeout)


def disarm_restore_watchdog():
    signal.setitimer(signal.ITIMER_REAL, 0)
    signal.signal(RESTORE_ALARM, signal.SIG_DFL)


@contextlib.contextmanager
def restore_window(timeout=RESTORE_TIMEOUT_SECONDS):
    """Mask cancellation as soon as the body ends, and bound the restore.

    Enters the `with` innermost, so on the way out it runs BEFORE
    `mutating_sources.__exit__`: the sources are restored and re-verified with
    both stop signals blocked, and `stop_run` unblocks them afterwards. The
    watchdog armed here is what keeps that window finite (#1843 A): a restore
    that never returns would otherwise be unreachable by SIGTERM and end in
    the runner's SIGKILL, leaving the tree mutated.
    """
    try:
        yield
    finally:
        mask_stop_signals()
        arm_restore_watchdog(timeout)


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
    if seconds > MAX_DEADLINE_SECONDS:
        raise ValueError(f'{DEADLINE_ENV} must be at most {MAX_DEADLINE_SECONDS}s, '
                         f'got {raw!r}')
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


def run(full, log_path=None):
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
        collected = (error.output or '') + (error.stderr or '')
        # If the cap WAS the remaining budget, the deadline is what ended this
        # call: report TIMEOUT rather than a plain INVALID mutant (review P1).
        if deadline - time.monotonic() <= 0:
            # The caller's own `write_text(log)` never runs once this raises,
            # so the output collected before the kill has to be written here or
            # the deadline path loses exactly the evidence §4 exists to keep
            # (#1692 review P2).
            if log_path is not None:
                log_path.write_text(collected)
            raise RunDeadlineExceeded(
                f'cargo test: overall deadline of {deadline_budget}s reached')
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
    except RestoreStalled as error:
        # #1843 A: the restore ran out of its bound. Publishing a summary here
        # would claim a verified restore while the tree may still hold a mutant
        # (#979 P0 / #1076), so the contract is the same as any other failed
        # restore: no summary at all, non-zero exit, loud diagnostic.
        raise SystemExit(f'restore did not finish before the run was cut off: {error}; '
                         'no summary published, sources may still be mutated')
    finally:
        # The restore is done and verified by now (the body masks signals while
        # it unwinds). From here on a cancellation would only truncate the
        # summary, so ignore both and drop anything that arrived while masked.
        # The restore watchdog is disarmed FIRST: it guards the masked window
        # only, and leaving it armed would turn a slow summary write into a
        # `RestoreStalled` after the sources were already restored (#1843 A).
        disarm_restore_watchdog()
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
            # The deadline path must keep the output the killed cargo call had
            # already produced: the caller's own `write_text(log)` never runs
            # once `run()` raises, so `run(log_path=...)` has to write it
            # itself or the deadline loses exactly the evidence §4 keeps
            # (#1692 review P2). `bounded()` must see a live deadline; the
            # fake call then moves the clock past it and times out with output.
            def killed_cargo(*_args, **_kwargs):
                global deadline
                deadline = time.monotonic() - 1
                raise subprocess.TimeoutExpired(
                    ['cargo', 'test'], 300, output='partial stdout\n',
                    stderr='partial stderr\n')

            deadline = time.monotonic() + 5
            run_owned = killed_cargo
            probe_log = out / 'probe-deadline.log'
            try:
                run('probe::deadline-cut', log_path=probe_log)
            except RunDeadlineExceeded:
                pass
            else:
                problems.append('log probe: an expired deadline did not raise')
            # #1938 r2（GLM sibling P1）：与 `dispatch_probe` 同一口径。原来
            # `exists()` 与 `read_text()` 是两次系统调用，中间被删就抛
            # `FileNotFoundError`，回溯穿出 `--self-check`——那是假 FAIL，不是诊断。
            # 读不到等同于「没写」，正好是这条断言要报的东西。
            try:
                written = probe_log.read_text()
            except FileNotFoundError:
                written = None
            if written is None:
                problems.append('log probe: no log was written before the deadline raised')
            elif 'partial stdout' not in written or 'partial stderr' not in written:
                problems.append('log probe: the collected output was not written before '
                                'the deadline raised')
            else:
                print('deadline probe log: collected output written before raising', flush=True)
            # #1843 B (the third fix-r1 P1 guard): `take_slot` re-routes the
            # slot guard's own `sys.exit` into TIMEOUT only while the OVERALL
            # budget is what ran out. Without that branch a deadline that
            # expires while waiting for a slot leaves through `acquire_slot`'s
            # plain SystemExit - no summary, no TIMEOUT row - which is exactly
            # the outcome §1/§4 exist to prevent. The refusal above cannot
            # cover it: that path raises AssertionError, not SystemExit.
            def gate_exit(*_args, **_kwargs):
                # Live when `bounded()` caps the wait, expired by the time the
                # gate's own exit is handled: the only shape in which the
                # re-route branch is what decides the outcome.
                time.sleep(0.1)
                raise SystemExit('cargo slots unavailable for 600s')

            wait_for_slot = gate_exit
            deadline = time.monotonic() + 0.05
            try:
                take_slot('probe')
            except RunDeadlineExceeded as error:
                print(f'deadline probe slot-guard exit: re-routed to TIMEOUT ({error})',
                      flush=True)
            except SystemExit as error:
                problems.append('slot-guard exit: exited instead of reporting TIMEOUT: '
                                f'{error}')
            else:
                problems.append('slot-guard exit: neither TIMEOUT nor exit')
            run_owned = refuse
            wait_for_slot = refuse
            deadline = time.monotonic()
            append_progress(event='probe', status='OK')
            # #1938 r2（GLM sibling P1）：同上。文件是 `append_progress` 刚写的；
            # 读不到就是「没能立刻读到」，下面那条断言正是要报它。
            try:
                lines = (out / 'progress.jsonl').read_text().splitlines()
            except FileNotFoundError:
                lines = []
            if not lines or json.loads(lines[-1]).get('event') != 'probe':
                problems.append('progress.jsonl: the appended record was not readable at once')
            else:
                print('deadline probe progress.jsonl: one flushed record readable', flush=True)
            publish_summary([dict(name='probe', status='KILLED')], timed_out='probe deadline')
            # #1938 r2（GLM sibling P1）：同上。空清单走同一条诊断（没发布 TIMEOUT 行）。
            try:
                published = json.loads((out / 'summary.json').read_text())
            except FileNotFoundError:
                published = []
            if not any(row.get('status') == 'TIMEOUT' for row in published):
                problems.append('summary.json: a deadline published no TIMEOUT row')
            if (out / 'summary.json.tmp').exists():
                problems.append('summary.json.tmp: the summary was not renamed atomically')
            else:
                print('deadline probe summary.json: TIMEOUT row published atomically', flush=True)
            # Every rejecting rule of `deadline_seconds` needs its own pair, or
            # reverting it to a plain `int()` would keep the probe green
            # (#1692 review P2): underscores, signs, surrounding spaces,
            # non-ASCII digits and the new upper bound.
            for bad in ('0', '-3', 'abc', '1.5', '1_000', ' 120 ', '+120',
                        '１２０', str(MAX_DEADLINE_SECONDS + 1), '9' * 400):
                try:
                    deadline_seconds(bad)
                except ValueError:
                    pass
                else:
                    problems.append(f'{DEADLINE_ENV}={bad!r} was not refused')
            if deadline_seconds('120') != 120:
                problems.append(f'{DEADLINE_ENV} did not parse whole seconds')
            if deadline_seconds(str(MAX_DEADLINE_SECONDS)) != MAX_DEADLINE_SECONDS:
                problems.append(f'{DEADLINE_ENV} refused its own upper bound')
        finally:
            if progress is not None:
                progress.close()
            deadline, deadline_budget, out, progress, env, run_owned, wait_for_slot = saved
    return problems


def slot_timeout_probe():
    """#1692 r15-1: `acquire_slot` refuses non-finite / non-positive budgets.

    `deadline_probe`'s bad-value table only covers the STRING form of the env
    override (`deadline_seconds`). The float guard on the slot wait itself was
    never exercised: every caller passes a positive literal, so `math.isfinite`
    and the `<= 0` test could both be deleted and every self-check case stayed
    green. inf / nan / -1 are exactly the values that break `wait_for_slot`'s
    deadline arithmetic (inf: the comparison is always true; nan: never true;
    -1: expired before the first sample), so each needs its own refusal, and a
    finite positive budget must still reach the shared helper unchanged.
    """
    global wait_for_slot
    problems = []
    saved = wait_for_slot
    seen = []
    gate = Path('/nonexistent/negctl-1316-slot-guard.sh')

    def recorder(script, *, timeout=SLOT_TIMEOUT_SECONDS):
        seen.append((Path(script), timeout))
        return subprocess.CompletedProcess([], 0, 'cargo-slots: 0 / cap 1\n', '')

    try:
        wait_for_slot = recorder
        for bad in (float('inf'), float('-inf'), float('nan'), -1, 0):
            try:
                acquire_slot(script=gate, timeout=bad)
            except SystemExit as error:
                if 'must be finite and positive' not in str(error):
                    problems.append(f'slot timeout {bad!r}: exited without the refusal '
                                    f'message: {error}')
            except BaseException as error:  # noqa: BLE001 - any other path is red
                problems.append(f'slot timeout {bad!r}: {type(error).__name__} instead of '
                                'the explicit sys.exit')
            else:
                problems.append(f'slot timeout {bad!r} was not refused')
        if seen:
            problems.append(f'slot timeout: a refused budget still reached the slot wait: '
                            f'{seen!r}')
        acquire_slot(script=gate, timeout=5)
        if seen != [(gate, 5)]:
            problems.append('slot timeout: a finite positive budget did not reach the '
                            f'shared helper unchanged: {seen!r}')
        elif not problems:
            print('slot timeout probe: inf / -inf / nan / -1 / 0 refused, 5 s passed through',
                  flush=True)
    finally:
        wait_for_slot = saved
    return problems


def _send_signals_after(signals, record):
    """Send process-directed signals from a helper thread, main thread delivery.

    `pthread_sigmask` is per thread: the helper blocks the stop signals in
    itself so the kernel delivers them to the main thread, which is the thread
    the fixture's handlers run on. The monotonic send time goes into `record`,
    so the caller can prove the signal actually landed inside the window it
    means to exercise instead of trusting a sleep.
    """
    def run():
        signal.pthread_sigmask(signal.SIG_BLOCK, STOP_SIGNALS)
        for sig, delay in signals:
            time.sleep(delay)
            record['sent_at'] = time.monotonic()
            os.kill(os.getpid(), sig)
    return threading.Thread(target=run, daemon=True)


def restore_probe():
    """#1843 A/B: the masked restore window, the double-signal disarm, the bound.

    Three invariants, each with its own meta control:
      * a stop signal that arrives while the sources are being written back
        must not interrupt the restore (the window masks it; `stop_run` turns
        the pending one into an ignored signal afterwards);
      * the FIRST stop signal disarms BOTH, so a second one cannot raise again
        inside the restore - the mask is per thread, so this is the only
        process-wide guarantee that a second delivery cannot land on a thread
        where the signal is still unblocked;
      * a restore that never returns is cut off by the SIGALRM watchdog
        instead of waiting for the runner's second, SIGKILL attempt.
    """
    problems = []
    saved_handlers = {sig: signal.getsignal(sig) for sig in STOP_SIGNALS}

    @contextlib.contextmanager
    def simulated_restore(delay, record, cap=5.0):
        """Stand-in for `mutating_sources.__exit__`'s write-back."""
        try:
            yield
        finally:
            started = time.monotonic()
            try:
                time.sleep(min(delay, cap))
            except BaseException as error:  # noqa: BLE001 - recording is the point
                record['interrupted'] = f'{type(error).__name__}: {error}'
                raise
            record['elapsed'] = time.monotonic() - started

    try:
        with tempfile.TemporaryDirectory(prefix='negctl-1316-restore-'):
            # (1) A cancellation that lands DURING the restore is masked: the
            # restore runs to completion and the signal is dropped afterwards.
            # Deleting `mask_stop_signals()` makes the handler raise inside the
            # write-back, which is the "tree left mutated" failure.
            record = {}
            signal.signal(signal.SIGTERM, _stop_on_signal)
            signal.signal(signal.SIGINT, _stop_on_signal)
            mask_start = time.monotonic()
            sender = _send_signals_after([(signal.SIGTERM, 0.3)], record)
            sender.start()
            with stop_run(), simulated_restore(0.8, record), restore_window(timeout=10):
                pass
            sender.join(timeout=5)
            # A green result only means something if the signal really landed
            # INSIDE the masked window (r1 GLM boundary P2 / MiniMax concurrency
            # P2): a sender starved past the 0.8 s restore would otherwise make
            # "the restore was not interrupted" true by never sending, and the
            # `restore-mask-removed` control would look SURVIVED for the same
            # reason. Three waypoints are checked instead of trusted.
            sent = record.get('sent_at')
            if sent is None:
                problems.append('restore window: the helper thread never sent the signal')
            elif not mask_start <= sent <= mask_start + 0.7:
                problems.append('restore window: the signal missed the masked window '
                                f'(sent at +{sent - mask_start:.2f}s of a 0.8 s restore)')
            elif 'interrupted' in record:
                problems.append('restore window: the stop signal interrupted the restore ('
                                f"{record['interrupted']})")
            elif record.get('elapsed', 0) < 0.7:
                problems.append('restore window: the simulated restore did not run to '
                                f"completion ({record.get('elapsed')!r}s)")
            else:
                print('restore probe: a stop signal during the restore did not '
                      'interrupt it', flush=True)
            # (2) One stop signal disarms both. Called directly, with both
            # handlers first armed for real: probe (1) ends in `stop_run`, whose
            # `finally` turns them into SIG_IGN, so inheriting that state would
            # make this case pass no matter what the handler does.
            signal.signal(signal.SIGTERM, _stop_on_signal)
            signal.signal(signal.SIGINT, _stop_on_signal)
            try:
                _stop_on_signal(signal.SIGINT, None)
            except RunTerminated:
                pass
            else:
                problems.append('stop signal: the handler did not raise RunTerminated')
            still_armed = [sig.name for sig in STOP_SIGNALS
                           if signal.getsignal(sig) is not signal.SIG_IGN]
            if still_armed:
                problems.append('stop signal: a second delivery would raise again inside '
                                f'the restore ({", ".join(still_armed)} still handled)')
            else:
                print('restore probe: the first stop signal disarmed both', flush=True)
            # (3) The restore is bounded. `simulated_restore` caps its sleep at
            # 5 s, so an unarmed watchdog shows up as a 5 s window, not a hang.
            record = {}
            started = time.monotonic()
            try:
                with simulated_restore(30, record), restore_window(timeout=0.3):
                    pass
            except RestoreStalled as error:
                elapsed = time.monotonic() - started
                if elapsed >= 3:
                    problems.append(f'restore watchdog: fired after {elapsed:.1f}s for a '
                                    '0.3 s bound')
                else:
                    print(f'restore probe: a stalled restore was cut off after '
                          f'{elapsed:.1f}s ({error})', flush=True)
            else:
                problems.append('restore watchdog: a stalled restore was not cut off '
                                f'after {time.monotonic() - started:.1f}s')
            # (4) And the bound must leave through the fail-closed path: a
            # non-zero exit with no summary. Swallowing `RestoreStalled` would
            # let the run publish a summary that claims a verified restore.
            record = {}
            try:
                with stop_run(), simulated_restore(30, record), restore_window(timeout=0.2):
                    pass
            except SystemExit as error:
                if 'no summary published' not in str(error):
                    problems.append('restore watchdog: exited without the fail-closed '
                                    f'diagnostic: {error}')
                else:
                    print('restore probe: a stalled restore exits non-zero with no '
                          'summary', flush=True)
            except BaseException as error:  # noqa: BLE001 - any other path is red
                problems.append('restore watchdog: left via '
                                f'{type(error).__name__} instead of the fail-closed exit')
            else:
                problems.append('restore watchdog: a stalled restore returned normally')
    finally:
        disarm_restore_watchdog()
        for sig, handler in saved_handlers.items():
            # getsignal returns None when a non-Python handler holds the
            # signal; signal.signal refuses None, so map that back to the
            # default disposition (same guard as the #2552 r9 legs in
            # test_safety.py, DS r10 sibling#2 / Claude r9 sibling#1).
            signal.signal(sig, handler if handler is not None
                          else signal.SIG_DFL)
        signal.pthread_sigmask(signal.SIG_UNBLOCK, STOP_SIGNALS)
    return problems


# #1938: the mark `dispatch_probe` puts on every child it starts, and the stub
# `PATH` those children get. A child that still reaches the main path has fallen
# through a usage guard; the mark is what stops it there, because the alternative
# is `out.mkdir`, this checkout's tree lock and real cargo - the meta-control
# copies keep `root` pointing at this checkout, so a fall-through is not local.
DISPATCH_PROBE_MARKER = 'PILOTDECK_NEGCTL_DISPATCH_PROBE'
DISPATCH_PROBE_STUB = ('#!/bin/sh\n'
                       'echo "$0 $*" >> "{calls}"\n'
                       'exit 1\n')

# #1938 r3（Codex boundary P2 / GLM boundary #3，取高判 P2）：桩的调用记录是
# 「dispatcher 试过起 cargo」的**唯一正面证据**，而它只在桩被调用时才产生——
# 「文件不存在」于是同时意味着「没调过」（正常）与「记录通道坏了」（假 PASS）。
# 先自己写一行哨兵：写不进去当场就红（通道自测），之后整份记录消失也一律红；
# 只有「只剩哨兵」才算没调过桩。
DISPATCH_PROBE_RECORD_SENTINEL = '# dispatch probe: no stub was invoked'


def _lock_snapshot(path):
    """#1938 r1（Claude boundary P0/P1）：锁的 mtime_ns（不存在即 `None`），一次 `stat`。

    `(path.exists(), path.stat().st_mtime_ns if path.exists() else None)` 有两个
    窗口：两次 `exists` 之间文件被删、或者 `exists` 与 `stat` 之间被删，后者抛
    `FileNotFoundError` 会让探针以回溯收场（假 FAIL）。这里只取一次 `stat`，
    删掉了就当作「不存在」，比较照样能看出「本来有、现在没了」。
    """
    try:
        return path.stat().st_mtime_ns
    except FileNotFoundError:
        return None


def dispatch_probe():
    """#1692 r15-4 / #1938: every mode takes an exact argument list, end to end.

    The structural pin in `test_metadata_io_driver.py` proves the dispatcher is
    still there; this proves what it does. Before the rewrite the two flags were
    matched differently, so `--self-check extra` fell through to the main path:
    it was read as an output directory plus a mutation name, and an unknown
    `--flag` could become a directory. Every case below runs the fixture itself
    in a throwaway cwd, so a fall-through shows up as the wrong diagnostic, a
    created path, a called `cargo` or a touched tree lock - never as a silent
    full run. `-x` alone used to be exactly that (#1938): with the guard gone it
    had no second argument to fail on, so it created a directory named `-x` and
    walked into the mutation suite. Three things fence the children in now: the
    marker the main path refuses, the `CARGO_TARGET_DIR` their environment no
    longer carries, and the `cargo` / `rustc` / `git` stubs that only record a
    call.
    """
    problems = []
    invocations = (
        ([], '(no arguments)', 'usage: negative_controls.py'),
        # The bare flag is the shape that fell through: no mutation name either,
        # so only the guard stood between it and `out.mkdir`. The second form is
        # the cheap extra layer (option b): it stops at the unknown mutation name.
        (['-x'], '-x', 'unknown mode'),
        (['-x', 'no-such-mutation'], '-x no-such-mutation', 'unknown mode'),
        (['--slot-guard-probe', 'only-one'], '--slot-guard-probe only-one',
         'takes exactly 2 arguments'),
        (['--slot-guard-probe', '/bin/true', 'not-a-timeout'],
         '--slot-guard-probe /bin/true not-a-timeout', 'not a timeout'),
        # A name that is not a mutation either, so a fall-through fails fast
        # instead of starting the whole mutation suite - and creates a path named
        # after the flag, which is what the `created` check below looks for.
        (['--made-up-mode', 'extra'], '--made-up-mode extra', 'unknown mode'),
    )
    # #1938 r1（GLM boundary P1）：`mutations` / `gates` / `self_check_controls`
    # 三张字面量表都有空表 fail-closed，唯独这张没有——清空它，下面的循环零次、
    # `problems` 仍是空的，探针照样打印通过。行数另有静态钉子
    # （`test_dispatch_case_table_cannot_shrink_silently`），这里管的是「整张表没了」。
    if not invocations:
        sys.exit('no dispatch cases defined: the probe would pass without '
                 'driving the dispatcher')
    # `--self-check extra` is NOT driven here: with its arity guard removed that
    # child would simply start a full `--self-check`, i.e. spawn this probe again
    # and recurse. The static pin in `test_metadata_io_driver.py` owns that line
    # (and the manual invocation in the PR body shows the verdict).
    with tempfile.TemporaryDirectory(prefix='negctl-1316-dispatch-') as directory, \
            tempfile.TemporaryDirectory(prefix='negctl-1316-probe-') as scratch:
        directory = Path(directory)
        scratch = Path(scratch)
        calls = scratch / 'cargo-calls.txt'
        calls.write_text(f'{DISPATCH_PROBE_RECORD_SENTINEL}\n')
        stub_dir = scratch / 'bin'
        stub_dir.mkdir()
        for stub_name in ('cargo', 'rustc', 'git'):
            stub = stub_dir / stub_name
            stub.write_text(DISPATCH_PROBE_STUB.format(calls=calls))
            stub.chmod(0o755)
        # #1938: `CARGO_TARGET_DIR` is a JOB-level variable in CI, so an inherited
        # one turns the fixture's own early exit off. Both stubs are needed
        # because a fall-through builds the workspace scanner before it collects.
        child_env = {key: value for key, value in os.environ.items()
                     if key != 'CARGO_TARGET_DIR'}
        child_env[DISPATCH_PROBE_MARKER] = '1'
        # #1938 r1（Claude boundary P2）：父环境没有 PATH 时上一个写法留一个尾部
        # 空项，空项按 cwd 解析；`os.defpath` 才是「没有 PATH 时的标准值」。
        child_env['PATH'] = (str(stub_dir) + os.pathsep
                             + (child_env.get('PATH') or os.defpath))
        lock = root / '.negative-source.lock'
        lock_before = _lock_snapshot(lock)
        for arguments, label, needle in invocations:
            try:
                result = run_owned([sys.executable, '-I', str(Path(__file__).resolve()),
                                    *arguments], cwd=directory, env=child_env, timeout=60)
            except subprocess.TimeoutExpired:
                problems.append(f'dispatch {label}: fell through to the main path '
                                'instead of refusing')
                continue
            text = result.stdout + result.stderr
            if result.returncode == 0:
                problems.append(f'dispatch {label}: exited 0')
            elif needle not in text:
                problems.append(f'dispatch {label}: no usage diagnostic: {text[-200:]}')
        # #1938: the marker gate on its own, driven with an argument list the main
        # path would otherwise accept (`OUT_DIR MUTATION`). Nothing but the gate
        # stands between this child and `out.mkdir`, so the verdict is the gate's
        # own diagnostic - and the directory that must not exist.
        try:
            result = run_owned([sys.executable, '-I', str(Path(__file__).resolve()),
                                'probe-marker-out', 'registry-drops-always-on'],
                               cwd=directory, env=child_env, timeout=60)
        except subprocess.TimeoutExpired:
            problems.append('dispatch marker: a probe child ran the main path '
                            '(no refusal within the probe timeout)')
        else:
            text = result.stdout + result.stderr
            if result.returncode == 0 or 'dispatch probe: refused' not in text:
                problems.append(f'dispatch marker: a probe child ran the main path: '
                                f'{text[-200:]}')
        # #1938 r1（Claude boundary P0）：`iterdir` 与 `read_text` 都是「先看后读」，
        # 中间被删就抛 `FileNotFoundError`，探针会以回溯收场而不是给出诊断——那是
        # 假 FAIL。这个 tempdir 是本函数自己建的、自己从不删，消失就意味**没法核**，
        # 所以照 fail-closed 报一条，而不是当成「什么都没建」。
        # #1938 r3：桩调用记录同理——上面先写好了哨兵，所以它现在**一定存在**；
        # 整份消失只可能是记录通道被破坏或被人删掉，那是「没法核」，不是「没调过」。
        # 读回来只去掉哨兵那一行，剩下的才是桩真正记下的调用。
        try:
            created = sorted(entry.name for entry in directory.iterdir())
        except FileNotFoundError:
            problems.append(f'dispatch: the probe output directory vanished before it '
                            f'could be checked: {directory}')
            created = []
        if created:
            problems.append(f'dispatch: an argument became an output path: {created}')
        # #1938: "the fall-through never reached this checkout" as a file the
        # check reads, not as an inference from the timing. `cargo` / `rustc`
        # mean real work started; `git` means `mutating_sources` got as far as
        # the tree lock (it takes the lock and only then runs the pristine
        # checks), so it is the one that covers a bare flock - a flock leaves no
        # mtime trace. The mtime check below still catches the case the stubs
        # cannot: a fall-through in a tree where the lock file does not exist yet.
        try:
            record = calls.read_text()
        except FileNotFoundError:
            record = None
        if record is None:
            problems.append(f'dispatch: the stub call record vanished before it could '
                            f'be checked: {calls}')
            stub_calls = ''
        else:
            stub_calls = ''.join(
                line for line in record.splitlines(keepends=True)
                if line.rstrip('\n') != DISPATCH_PROBE_RECORD_SENTINEL)
        if stub_calls:
            problems.append(f'dispatch: the probe started a stubbed cargo, rustc or '
                            f'git: {stub_calls.strip()[:200]}')
        lock_after = _lock_snapshot(lock)
        if lock_after != lock_before:
            problems.append(f'dispatch: the probe touched the real tree lock: {lock} '
                            f'went from {lock_before} to {lock_after}')
        if not problems:
            print('dispatch probe: extra arguments and unknown modes are usage errors',
                  flush=True)
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
        #
        # #1843 F: but the real gate's verdict is also this HOST's verdict -
        # busy, cold-bootstrapping, or an ops install that is mid-update all
        # change rc with nothing wrong with the guard, and under load that
        # turned a whole `--self-check-controls` run INVALID. CI is the one
        # environment that is fixed (a runner has no `~/ws/pdrs-ops`, so
        # cargo-slots.sh answers rc 69 -> "slot free"), so the real-gate probe
        # runs there and is reported as skipped everywhere else. The six stub
        # gates keep covering 0/1/2/3/4/64 on every host.
        if os.environ.get('CI') == 'true':
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
                    problems.append(f'real gate: rc=0 took {elapsed:.1f}s for a '
                                    f'{timeout}s deadline')
                else:
                    print(f'self-check real cargo-slots.sh: exit rc=0 in {elapsed:.1f}s',
                          flush=True)
            elif result.returncode == 1:
                if 'cargo slots unavailable' not in result.stdout + result.stderr:
                    problems.append('real gate: rc=1 without the bounded-busy verdict '
                                    '(gate broken rather than busy)')
                elif elapsed >= timeout + 3:
                    problems.append(f'real gate: rc=1 took {elapsed:.1f}s for a '
                                    f'{timeout}s deadline')
                else:
                    print(f'self-check real cargo-slots.sh: exit rc=1 (busy) in '
                          f'{elapsed:.1f}s', flush=True)
            else:
                problems.append('real gate: expected a bounded 0/1 verdict, '
                                f'got rc={result.returncode}')
        else:
            print('self-check real cargo-slots.sh: skipped outside CI '
                  '(this host\'s slot state is not a verdict about the guard)', flush=True)
    problems.extend(deadline_probe())
    problems.extend(slot_timeout_probe())
    problems.extend(restore_probe())
    problems.extend(dispatch_probe())
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
                 "                     str(root / 'scripts/cargo-slots.sh'), str(timeout)],")
STUB_GATE_ARG = ("'--slot-guard-probe',\n"
                 "                     str(Path(__file__).resolve().parent "
                 "/ 'cargo-slots-free.sh'), str(timeout)],")
STUB_GATE_TEXT = '#!/bin/sh\necho "cargo-slots: 0 / cap 1"\nexit 0\n'
# Each meta-control copy runs a full `--self-check` (measured 12-13 s per copy,
# ~100 s for control + 7 cases on this machine). The structural guard derives
# the job margin from `len(self_check_controls) + 1`, so adding a case widens it
# instead of silently eating the headroom.
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


# #1938 r1（GLM boundary P1）：`dispatch_probe` 的用例表同样要证明「清空它就红」。
# 与 gates 一样按整表文本手术，不靠逐行锚点：表里的参数写法随时会变，锚行会漂。
EMPTY_INVOCATIONS_TABLE = object()
INVOCATIONS_TABLE_OPEN = '    invocations = (\n'
INVOCATIONS_TABLE_END = '    )\n'


def _empty_invocations_table(text):
    """Replace the whole dispatch case table with `()`; returns (text, hits)."""
    start = text.find(INVOCATIONS_TABLE_OPEN)
    end = text.find(INVOCATIONS_TABLE_END, start if start >= 0 else 0)
    if start < 0 or end < 0:
        return text, 0
    end += len(INVOCATIONS_TABLE_END)
    return text[:start] + '    invocations = ()\n' + text[end:], 1

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
    # #1938 r1（GLM boundary P1）：同一种洞在 `dispatch_probe` 的用例表上——清空
    # `invocations`，循环零次、`problems` 仍空，探针照样打印通过。守卫在时必须红 ...
    ('empty-dispatch-cases',
     [(EMPTY_INVOCATIONS_TABLE, None)],
     True, 'no dispatch cases defined', None),
    # ... 守卫拿掉时必须绿（那张空表看起来和健康运行一模一样）。判据与
    # `empty-gates-guard-removed` 相同：只说「消息不见了」是同义反复，因为该
    # 变异删的正是打印它的那段文本，所以这里钉住 rc=0。
    ('empty-dispatch-guard-removed',
     [(EMPTY_INVOCATIONS_TABLE, None),
      ('    if not invocations:\n'
       "        sys.exit('no dispatch cases defined: the probe would pass without '\n"
       "                 'driving the dispatcher')\n",
       '')],
     False, 'no dispatch cases defined', 0),
    # The tightened deadline parser is only load-bearing together with its
    # extended bad-value pair: reverting to a plain `int()` (which accepts
    # '1_000', ' 120 ', '+120', full-width digits and 86401) must turn the
    # probe red through those very values (#1692 review P2, both families).
    ('deadline-parser-loosened',
     [('    if not isinstance(raw, str) or not raw.isascii() or not raw.isdigit():\n'
       "        raise ValueError(f'{DEADLINE_ENV} must be whole seconds, got {raw!r}')\n",
       ''),
      ('    if seconds > MAX_DEADLINE_SECONDS:\n'
       "        raise ValueError(f'{DEADLINE_ENV} must be at most {MAX_DEADLINE_SECONDS}s, '\n"
       "                         f'got {raw!r}')\n",
       '')],
     True, 'was not refused', None),
    # Dropping the log write loses the output of the cargo call the deadline
    # killed, which is the evidence §4 exists to keep (#1692 review P2).
    ('deadline-log-dropped',
     [('            if log_path is not None:\n'
       '                log_path.write_text(collected)\n', '')],
     True, 'no log was written before the deadline raised', None),
    # #1843 B: `take_slot`'s re-route is the only thing that turns the slot
    # guard's own `sys.exit` into a TIMEOUT row while the OVERALL budget is what
    # ran out. Drop it and the probe sees the plain exit instead.
    ('slot-guard-exit-not-rerouted',
     [('        if deadline - time.monotonic() <= 0:\n'
       '            raise RunDeadlineExceeded(\n'
       "                f'{name}: overall deadline of {deadline_budget}s reached while '\n"
       "                'waiting for a cargo slot')\n", '')],
     True, 'slot-guard exit: exited instead of reporting TIMEOUT', None),
    # #1692 r15-1: the finite-and-positive guard is the only thing standing
    # between inf/nan/-1/0 and a wait that never expires. Without it every bad
    # budget reaches the shared helper.
    ('slot-timeout-guard-removed',
     [('    if not math.isfinite(timeout) or timeout <= 0:\n'
       '        # float(\'inf\')/nan 会让 deadline 比较永远成立（或永远不成立），必须先拒。\n'
       "        sys.exit(f'slot timeout must be finite and positive: {timeout!r}')\n", '')],
     True, 'was not refused', None),
    # #1843 A: the mask is what keeps a cancellation from interrupting the
    # write-back; without it the signal lands inside the restore.
    ('restore-mask-removed',
     [('        mask_stop_signals()\n'
       '        arm_restore_watchdog(timeout)\n',
       '        arm_restore_watchdog(timeout)\n')],
     True, 'the stop signal interrupted the restore', None),
    # #1843 B: disarming BOTH signals on the first delivery is the process-wide
    # half of the fix; without it the probe sees a live handler afterwards.
    ('stop-signal-second-armed',
     [('    for sig in STOP_SIGNALS:\n'
       '        signal.signal(sig, signal.SIG_IGN)\n'
       "    raise RunTerminated(f'signal {signum} before the run finished')\n",
       "    raise RunTerminated(f'signal {signum} before the run finished')\n")],
     True, 'a second delivery would raise again inside the restore', None),
    # #1843 A: the SIGALRM watchdog is the only bound on the masked window.
    # Without it the stalled restore simply runs to the simulated cap.
    ('restore-watchdog-removed',
     [('        mask_stop_signals()\n'
       '        arm_restore_watchdog(timeout)\n',
       '        mask_stop_signals()\n')],
     True, 'a stalled restore was not cut off', None),
    # #1843 A: and the bound has to leave fail-closed. Without the translation
    # the stalled restore escapes as a bare RestoreStalled instead of the
    # "no summary published" exit.
    ('restore-failclosed-removed',
     [('    except RestoreStalled as error:\n'
       '        # #1843 A: the restore ran out of its bound. Publishing a summary here\n'
       '        # would claim a verified restore while the tree may still hold a mutant\n'
       '        # (#979 P0 / #1076), so the contract is the same as any other failed\n'
       '        # restore: no summary at all, non-zero exit, loud diagnostic.\n'
       "        raise SystemExit(f'restore did not finish before the run was cut off: "
       "{error}; '\n"
       "                         'no summary published, sources may still be mutated')\n",
       '')],
     True, 'left via RestoreStalled instead of the fail-closed exit', None),
    # #1692 r15-4: without the unknown-mode guard a `--flag` becomes a main-path
    # argument instead of a usage error.
    ('unknown-mode-guard-removed',
     [("if mode.startswith('-'):\n"
       "    sys.exit(f'unknown mode: {mode}\\n{USAGE}')\n",
       'if False:\n    pass\n')],
     True, 'dispatch --made-up-mode extra: no usage diagnostic', None),
    # 零参数（裸跑）同样必须是用法错误，而不是主路径上的 IndexError。
    ('bare-invocation-guard-removed',
     [('if mode is None:\n    sys.exit(USAGE)\n', 'if False:\n    pass\n')],
     True, 'dispatch (no arguments): no usage diagnostic', None),
    # #1938: the marker gate is the LAST thing between a deleted usage guard and
    # this checkout's tree lock plus real cargo. Drop it and a probe child holding
    # a valid `OUT_DIR MUTATION` list creates its output directory and walks on,
    # so the copy's dispatch probe reports it and the copy goes red. Without this
    # case the marker could be deleted with every other control still green.
    ('dispatch-probe-marker-removed',
     [("if os.environ.get(DISPATCH_PROBE_MARKER):\n"
       "    sys.exit(f'dispatch probe: refused to run the main path for {mode!r}; the '\n"
       "             f'probe child must fail on a usage guard\\n{USAGE}')\n",
       '')],
     True, 'dispatch marker: a probe child ran the main path', None),
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
        elif old is EMPTY_INVOCATIONS_TABLE:
            text, count = _empty_invocations_table(text)
        else:
            count = text.count(old)
            if count == 1:
                text = text.replace(old, new, 1)
        if count != 1:
            raise ValueError(f'{name}: anchor {old!r} matches {count} times')
    # Named `copy_path`, not `path`: the only `path.write_*` in this file must be
    # the source writes, so the §9 structural guard can forbid them by name.
    copy_path = directory / f'{name}.py'
    copy_path.write_text(text)
    return copy_path


def self_check_controls_run():
    """Prove every self-check guard is load-bearing; returns process problems."""
    problems = []
    with tempfile.TemporaryDirectory(prefix='negctl-1316-selfcheck-') as directory:
        directory = Path(directory)
        # Free-gate stub for the case that has to end GREEN (see STUB_GATE_ARG).
        stub = directory / 'cargo-slots-free.sh'
        stub.write_text(STUB_GATE_TEXT)
        stub.chmod(0o755)
        # #1938: the copies run WITHOUT the job's `CARGO_TARGET_DIR` (CI sets it
        # job-wide, `.github/workflows/ci.yml`). A copy whose guard is gone then
        # dies at the fixture's own "CARGO_TARGET_DIR must be supplied by the
        # caller" exit - the second gate - instead of taking this checkout's
        # tree lock and running real cargo. The marker is deliberately not set
        # for these children: they are `--self-check` runs, not dispatch-probe
        # children, and a fall-through here must stay loud rather than be
        # absorbed by the marker's refusal.
        child_env = {key: value for key, value in os.environ.items()
                     if key != 'CARGO_TARGET_DIR'}
        # #1938 r1（Claude boundary P2）：与 `dispatch_probe` 里同一处口径——
        # 父环境没有 PATH 时写死标准值 `os.defpath`，不让「没有 PATH」这件事
        # 悄悄变成子进程的默认搜索路径以外的行为。
        child_env.setdefault('PATH', os.defpath)
        # Control of the control: an unmutated copy must still pass, otherwise
        # every "the copy went red" verdict below would be meaningless.
        # #1692 §9 r3 review (MiniMax boundary P1): these three exits carry the
        # whole verdict, so they must go through `problems.append` like every
        # other witness -- a bare `return [...]` is invisible to the #1843 C
        # witness pins, and `return []` would turn a broken baseline copy into
        # `--self-check-controls` exit 0 (a false PASS for the CI step).
        try:
            control = _self_check_variant('control', [], directory)
        except ValueError as error:
            problems.append(f'control copy: {error}')
            return problems
        try:
            result = run_owned([sys.executable, '-I', str(control), '--self-check'],
                               env=child_env, timeout=CONTROL_TIMEOUT_SECONDS)
        except subprocess.TimeoutExpired:
            problems.append(f'control copy: --self-check did not finish within {CONTROL_TIMEOUT_SECONDS}s')
            return problems
        if result.returncode != 0:
            tail = (result.stdout + result.stderr)[-400:]
            problems.append(f'control copy: an unmutated copy failed (rc={result.returncode}): {tail}')
            return problems
        print('self-check control copy: PASS (rc=0)', flush=True)

        for name, replacements, expect_message, message, expect_rc in self_check_controls:
            try:
                variant = _self_check_variant(name, replacements, directory)
            except ValueError as error:
                problems.append(f'{name}: INVALID {error}')
                continue
            try:
                result = run_owned([sys.executable, '-I', str(variant), '--self-check'],
                                   env=child_env, timeout=CONTROL_TIMEOUT_SECONDS)
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


# #1692 r15-4: ONE dispatch rule for every mode. The two flags used to be
# matched differently - `--slot-guard-probe` by prefix plus an arity check,
# `--self-check` by whole-argv equality - so `negative_controls.py --self-check
# extra` fell through to the main path and created a DIRECTORY named
# `--self-check`. Every mode now takes an exact argument list, extra arguments
# are a usage error, and an unknown `--flag` can never become an output path.
USAGE = ('usage: negative_controls.py OUT_DIR [MUTATION ...] | --self-check | '
         '--self-check-controls | --slot-guard-probe SLOT_SCRIPT TIMEOUT_SECONDS')
mode, arguments = (sys.argv[1], sys.argv[2:]) if len(sys.argv) > 1 else (None, [])
if mode == '--slot-guard-probe':
    # Child mode driven by `self_check`: run the guard against a gate. Returning
    # here at all would mean the wait was unbounded.
    if len(arguments) != 2:
        sys.exit(f'--slot-guard-probe takes exactly 2 arguments\n{USAGE}')
    try:
        probe_timeout = float(arguments[1])
    except ValueError:
        sys.exit(f'--slot-guard-probe: not a timeout: {arguments[1]!r}')
    slots = acquire_slot(script=Path(arguments[0]), timeout=probe_timeout)
    print(f'slot free: {slots.stdout.strip()}', flush=True)
    sys.exit(0)
if mode in ('--self-check', '--self-check-controls'):
    if arguments:
        sys.exit(f'{mode} takes no arguments: {arguments!r}\n{USAGE}')
    if mode == '--self-check':
        sys.exit(self_check())
    control_problems = self_check_controls_run()
    for problem in control_problems:
        print('self-check control FAILED: ' + problem, file=sys.stderr)
    sys.exit(1 if control_problems else 0)
# 任何以 `-` 开头的首参都是模式位，不是输出目录：单横线（`-x`）与零参数（裸跑）此前都会
# 滑到主路径——`-x` 被当成目录名建出来、裸跑直接 IndexError 回溯。两种都不是「用法错误」
# 该有的样子，所以在这里一并封住（r1 评审 GLM boundary P3 / MiniMax boundary P2）。
if mode is None:
    sys.exit(USAGE)
if mode.startswith('-'):
    sys.exit(f'unknown mode: {mode}\n{USAGE}')
# #1938: `dispatch_probe` drives this very file as a child. A child that gets
# here has fallen through a usage guard, and the next steps are `out.mkdir`, the
# tree lock and real cargo - on THIS checkout, because the meta-control copies
# keep `root` pointing at it. So the probe marks its children and the main path
# refuses the mark before its first side effect. The check sits AFTER the mode
# dispatch on purpose: a legitimate usage error still reports its own diagnostic
# (`-x` stays `unknown mode`), and only a fall-through ever sees the mark.
if os.environ.get(DISPATCH_PROBE_MARKER):
    sys.exit(f'dispatch probe: refused to run the main path for {mode!r}; the '
             f'probe child must fail on a usage guard\n{USAGE}')

selected = arguments
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
        log_path = out / f'baseline-{full.rsplit("::", 1)[-1]}.log'
        code, log = run(full, log_path=log_path)
        log_path.write_text(log)
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
            # #1692 §9: one rename, never a truncating in-place write. The
            # publish helper is the shared one (#1903): the same bytes-shape
            # guard that covers `mutating_sources`' restore covers this call.
            write_source_atomic(root, path, changed.encode())
            log_path = out / f'{name}.log'
            code, log = run(full, log_path=log_path)
            log_path.write_text(log)
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
            # #1692 §9: same rule on the way back. A KILL inside a truncating
            # `write_bytes` leaves an empty source that reads as "differs from
            # HEAD" on the next run; the rename leaves the old bytes instead.
            write_source_atomic(root, path, original)
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
