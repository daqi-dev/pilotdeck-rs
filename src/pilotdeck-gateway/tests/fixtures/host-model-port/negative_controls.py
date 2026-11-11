#!/usr/bin/env python3
"""#2069: the #1804 turn_abort wiring drop as a runnable roster control.

NEW_OUTPUT [mutation ...]. --check validates anchors and marker bindings
without changing sources. Every mutant must compile, kill its exact bound
test, and pass after restoration. The bound tests live in the integration
target `--test host_model_port` (tests/host_model_port.rs), not the lib, so
the shared matrix_main `--lib` face cannot run them — this self-managed
runner follows the settings/mode-routing precedent and always picks the
integration target by the roster, never guessed from the test name.

The verdict invariants are the shared ones (specs/negative-controls/README.md):
the denominator is the static EXPECTED_NAMES roster checked at startup,
refusals exit 2 ("never started"), the mutant is published with one atomic
rename, and the closing exit code comes from negative_selection.verdict_exit.
The full kill round needs the committed tree (`mutating_sources` HEAD guard),
so the coordinator runs it after landing; the fixture ships with the patch.
"""
import difflib
import os
import re
import signal
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import (defer_signal_exit, mutating_sources, run_owned,
                               wait_for_slot, write_source_atomic)
from negative_selection import (claim_dir, missing_target, refuse,
                                refuse_unusable_target, registered, select,
                                verdict_exit)
from negative_suite import Case, classify, passed, publish_summary

ROOT = Path(__file__).resolve().parents[5]
MODEL_PORT = ROOT / 'src/pilotdeck-gateway/src/host/modules/model_port.rs'
INTEGRATION = ROOT / 'src/pilotdeck-gateway/tests/host_model_port.rs'

# #2069: the row the #1804 review ran by hand, now in a runnable roster. The
# mutation drops the `turn_abort` wiring line of the `RouterExecuteContext`
# literal in `RouterModelPort::stream`; the bound test (single threaded on
# purpose — see its doc comment) then observes the provider event leaked
# through the port in the aborting call stack. The `None` spelling compiles:
# the kill is the assertion's, not the compiler's.
cases = [
    ('turn-abort-wiring-dropped', MODEL_PORT,
     '            turn_abort: input.abort.clone(),',
     '            turn_abort: None,',
     'a_turn_abort_in_the_provider_call_stack_drops_the_event_at_the_port',
     '#1804 a turn cancelled in the provider call stack yields no event'),
]

# #1666: the denominator is this literal roster, not `len(cases)`. With a
# runtime denominator a deleted row shrinks the run silently and a table that
# lost every row reports `0/0 killed` and exits 0. Renaming or dropping a
# mutant here is a deliberate edit that must update this tuple too, in the
# same order as the table.
EXPECTED_NAMES = (
    'turn-abort-wiring-dropped',
)


def function_body(name, text):
    """One top-level function's source, cut at its column-zero `}`.

    The aborting-invoker test declares a nested `impl` block, so the body ends
    at the first column-zero closing brace — a sibling test's assertion cannot
    satisfy the binding (the mode-routing precedent).
    """
    match = re.search(r'^(?:async )?fn ' + re.escape(name) + r'\(', text, re.M)
    if not match:
        return ''
    end = text.find('\n}', match.start())
    return text[match.start():end + 2] if end >= 0 else ''


def check_errors():
    """The `--check` verdict: every anchor and marker binding is intact."""
    errors = []
    port = MODEL_PORT.read_text(encoding='utf-8')
    tests = INTEGRATION.read_text(encoding='utf-8')
    for name, path, old, new, test, assertion in cases:
        if port.count(old) != 1:
            errors.append(f'{name}: anchor occurs {port.count(old)} times')
        if old == new:
            errors.append(f'{name}: anchor equals replacement')
        body = function_body(test, tests)
        if not body:
            errors.append(f'{name}: bound test {test} not found exactly once')
        elif assertion not in body:
            errors.append(f'{name}: marker not in the bound test body')
    return errors


def terminate_cleanly(signum, _frame):
    """Unwind on SIGTERM/SIGHUP/SIGQUIT so the finally blocks restore the
    sources; the default disposition would leave a mutant in the tree."""
    if defer_signal_exit(signum):
        return
    raise SystemExit(128 + signum)


def install_terminate_handlers():
    signal.signal(signal.SIGTERM, terminate_cleanly)
    if hasattr(signal, 'SIGHUP'):
        signal.signal(signal.SIGHUP, terminate_cleanly)
    if hasattr(signal, 'SIGQUIT'):
        signal.signal(signal.SIGQUIT, terminate_cleanly)


def main():
    # The roster is validated before any branch, so the `--check` static gate
    # (CI) refuses roster drift — a renamed or dropped row with a stale
    # EXPECTED_NAMES must not print "anchors OK". The roster and the subset
    # selection are both ordinary raises, so a typo is refused as "never
    # started" (exit 2), never mistaken for a failed control (exit 1).
    try:
        registered(cases, EXPECTED_NAMES)
    except ValueError as error:
        refuse(str(error))
    if sys.argv[1:2] == ['--check']:
        errors = check_errors()
        if errors:
            raise SystemExit('#2069 ' + '; '.join(errors))
        print(f'#2069 {len(cases)} mutation anchors OK')
        return 0
    if len(sys.argv) < 2:
        raise SystemExit('usage: negative_controls.py --check | NEW_OUTPUT [mutation ...]')
    # `select` returns the table's own rows (tuples); the runner below speaks
    # `Case`, whose field order is the table's column order.
    try:
        run_rows = [Case(*row) for row in select(cases, sys.argv[2:], always=())]
    except ValueError as error:
        refuse(str(error))
    if missing_target():
        refuse('set CARGO_TARGET_DIR to the caller-owned build tree before running')
    refuse_unusable_target()
    out = claim_dir(sys.argv[1])
    install_terminate_handlers()
    env = dict(os.environ)
    env.update(CARGO_PROFILE_DEV_DEBUG='line-tables-only',
               CARGO_PROFILE_TEST_DEBUG='line-tables-only', LANG='en_US.UTF-8')
    env.pop('LC_ALL', None)
    env.pop('LC_MESSAGES', None)
    # Every bound test is an integration test of the `host_model_port` target:
    # the shared `--lib` face would filter to nothing, and a vacuous green is
    # exactly what this roster exists to refuse.
    base = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
            '-p', 'pilotdeck-gateway', '--test', 'host_model_port']
    results = []

    def run(label, case, args):
        slot = wait_for_slot(ROOT / 'scripts/cargo-slots.sh', timeout=900)
        (out / (label + '.slots.log')).write_text(slot.stdout + slot.stderr)
        # No -q: cargo forwards it to libtest as --quiet, which drops the
        # per-test `test <name> ... FAILED` line the classifier requires.
        result = run_owned(base + [case.test] + args, cwd=ROOT, env=env,
                           timeout=900)
        (out / (label + '.log')).write_text(result.stdout + result.stderr)
        return result

    # One whole-run window, the family convention (`settings`,
    # `mode-routing`): the tree lock is taken once around every case, so a
    # sibling suite serializes against this run instead of interleaving
    # between its cases.
    guarded = [MODEL_PORT, INTEGRATION,
               Path(__file__).resolve(),
               Path(__file__).resolve().parents[1] / 'negative_process.py',
               Path(__file__).resolve().parents[1] / 'negative_suite.py',
               Path(__file__).resolve().parents[1] / 'negative_selection.py']
    with mutating_sources(ROOT, guarded) as originals:
        for case in run_rows:
            row = dict(mutation=case.mutation, test=case.test, assertion=case.assertion,
                       status='INVALID', reason='', compiled=False)
            args = ['--', '--exact']
            try:
                try:
                    original = originals[case.path].decode()
                    if original.count(case.old) != 1 or case.old == case.new:
                        raise RuntimeError(f'stale anchor: {case.mutation}')
                    changed = original.replace(case.old, case.new, 1)
                    if not passed(run(case.mutation + '-positive', case, args), case.test):
                        raise RuntimeError('positive control failed')
                    (out / (case.mutation + '.patch')).write_text(''.join(difflib.unified_diff(
                        original.splitlines(True), changed.splitlines(True),
                        fromfile=str(case.path), tofile=str(case.path))))
                    # One atomic rename, never a truncating in-place write: a
                    # SIGKILL in the write-back window must not leave a
                    # half-written source behind (#1903).
                    write_source_atomic(ROOT, case.path, changed.encode())
                    compiled = run(case.mutation + '-compile', case, ['--no-run'])
                    row['compiled'] = compiled.returncode == 0
                    if not row['compiled']:
                        raise RuntimeError('mutant did not compile')
                    row['status'], row['reason'] = classify(
                        run(case.mutation + '-negative', case, args), case)
                finally:
                    # Restore per case: the whole-run context below is only
                    # the backstop (a crash between cases), not the per-case
                    # restore — the next case's positive control must see the
                    # original bytes.
                    write_source_atomic(ROOT, case.path, originals[case.path])
                if not passed(run(case.mutation + '-restored', case, args), case.test):
                    raise RuntimeError('restored positive control failed')
            except Exception as error:
                row.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
            results.append(row)
            print(f"{case.mutation}: {row['status']}: {row['reason']}", flush=True)
    publish_summary(out, results)
    return verdict_exit(results, key='status')


if __name__ == '__main__':
    raise SystemExit(main())
