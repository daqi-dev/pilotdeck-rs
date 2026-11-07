#!/usr/bin/env python3
# anchor-selfcheck: unsupported build-artifact signal injection, no source mutation table
"""#1261 fix-r2 / #1401 item 1: early exits split into INVALID and KILLED.

The bound tests (tests/native_process.rs) answer a child that exits with
**neither** the rejection nor its ``native listening`` advertisement on stderr
-- an environmental early exit (dyld, OOM, assembly) -- with an unmarked panic,
so it can never fake a kill (INVALID). A child that exits **after** advertising
``native listening`` has already proven the entry gate accepted the inherited
authority, so the marked assertion kills (KILLED). This control injects both
exits without mutating any source: the built gateway binary in the
caller-supplied ``CARGO_TARGET_DIR`` is atomically replaced by a stub that
either exits immediately with empty output, or prints
``native listening 127.0.0.1:1`` to stderr first and then exits. The scrub
test gets the INVALID half as well, so its own unjudgeable branch is observed
rather than assumed (#1401 review, both lineages).

The negative step runs the already-built test executable directly, not
through cargo: every ``cargo test`` re-publishes ``debug/pilotdeck-gateway``
from the ``debug/deps`` artifact before the integration test starts, which
would silently revert the stub. The positive and restored steps still go
through cargo (and its uplift), so a clean tree is proven on both sides of
the injection. The shared ``negative_suite.classify`` must return the
expected verdict per stub with the expected payload inside the named test's
own panic (the unmarked early-exit panic for INVALID, a bound marker for
KILLED; any other failure is not the proof), every row must have *proved*
its own verdict -- a row folded to INVALID by an exception is not proof, or
the eof row would be satisfied by its own failure (#1401 review P0) -- the
original binary is restored and SHA-256 verified, and the restored positive
controls must pass. No source mutation happens here, so unlike
``negative_controls.py`` this runs on an uncommitted tree.

The stub window rewrites a *shared* ``$CARGO_TARGET_DIR/debug`` path, so the
whole run holds the same tree lease ``mutating_sources`` takes: another
control on this tree can neither publish over the stub nor capture it as its
own binary (#1401 review, both lineages). No ``-q``: the judge needs the
named per-test lines.
"""
from pathlib import Path
from types import SimpleNamespace
import hashlib
import os
import re
import stat
import sys
import tempfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
sys.path.insert(0, str(Path(__file__).resolve().parent))
from negative_panic import panic_evidence
from negative_process import new_output_directory, run_owned, tree_lock, wait_for_slot
from negative_source import squeeze, test_body
from negative_suite import classify, invalidate_completed, passed, publish_summary
from native_entry_binding import (
    MARKERS,
    MARKER_TEMPLATE,
    SCRUB_EARLY_EXIT,
    SCRUB_TEST,
    TEST,
    binding_errors,
)

ROOT = Path(__file__).resolve().parents[5]
TEST_FILE = ROOT / 'src/pilotdeck-gateway/tests/native_process.rs'
# The unmarked early-exit panics must stay distinct from every kill marker:
# the shared judge kills only on a marker inside the named test's panic
# payload, and the INVALID branch deliberately offers none.
EARLY_EXIT = ('#1261 native entry child exited early; no rejection and '
              'no listening advertisement for')
# The gate that separates the unmarked early-exit panic from the marked kill.
# This one is specific to the rejects test, so it stays here (#1989 item 6).
GATE = 'if !stderr.contains(&rejection) {'
# One row per stub shape *and* bound test: an environmental exit prints
# nothing and can only be judged INVALID, while a child that advertised
# listening before exiting proved the gate accepted the authority, so the
# marked assertion must kill. `payloads` is what must appear in the named
# test's own panic payload -- a status alone is not proof, or any other
# failure of a row that expects INVALID would satisfy it. The advertisement
# text is the child's own stderr line ("native listening <addr>").
ROWS = (
    ('eof', TEST, '#!/bin/sh\nexit 86\n', 'INVALID', (EARLY_EXIT,)),
    ('listening', TEST,
     '#!/bin/sh\necho "native listening 127.0.0.1:1" >&2\nexit 86\n',
     'KILLED', tuple(MARKERS)),
    ('scrub-eof', SCRUB_TEST, '#!/bin/sh\nexit 86\n', 'INVALID', (SCRUB_EARLY_EXIT,)),
)
# Both tests need a positive control before and after the injection.
TESTS = (('rejects', TEST), ('scrub', SCRUB_TEST))
EXPECTED = {f'child-early-exit-stub-{name}': expected
            for name, _, _, expected, _ in ROWS}
TEST_BINARY = re.compile(r'Running tests/native_process\.rs \(([^)]+)\)')


def command(test):
    return ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
            '-p', 'pilotdeck-gateway', '--test', 'native_process',
            test, '--', '--exact']


def main():
    if sys.argv[1:] == ['--check']:
        names = [name for name, *_ in ROWS]
        if len(set(names)) != len(names):
            # `proved`, `restored` and the verdict are keyed by mutation name,
            # so a duplicate row would let one run satisfy both (#1401 review
            # P3).
            raise SystemExit(f'#1261 duplicate stub row names: {names}')
        test_source = TEST_FILE.read_text()
        # Each row's proof must live in the test that row names, or the
        # runtime verdict for that row could never be reached.
        for name, test, _, _, payloads in ROWS:
            body = test_body(test_source, test)
            for payload in payloads:
                if payload in body:
                    continue
                # The kill markers are formatted from one template in the
                # test, so the source carries the template, not each message.
                if payload in MARKERS and MARKER_TEMPLATE in body:
                    continue
                raise SystemExit(f'#1261 row {name}: missing {test} payload: {payload}')
        # Every binding shared with `negative_controls.py` lives in
        # `native_entry_binding` (#1989 item 6); the gate needle is this
        # test's own and stays here.
        rejects = squeeze(test_body(test_source, TEST))
        errors = binding_errors(test_source)
        if GATE not in rejects:
            errors.append(f'missing rejection gate from {TEST}')
        for marker in MARKERS:
            if marker in EARLY_EXIT or EARLY_EXIT in marker:
                errors.append(f'early-exit panic collides with marker {marker}')
        if errors:
            raise SystemExit('#1261 ' + '; '.join(errors))
        print(f'#1261 {len(ROWS)} early-exit rows OK')
        return 0
    if len(sys.argv) != 2:
        raise SystemExit('usage: early_exit_invalid.py --check | NEW_OUTPUT')
    env = dict(os.environ)
    if not env.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
    out = new_output_directory(Path(sys.argv[1]).resolve())
    env.update(CARGO_PROFILE_DEV_DEBUG='line-tables-only',
               CARGO_PROFILE_TEST_DEBUG='line-tables-only', LANG='en_US.UTF-8')
    env.pop('LC_ALL', None)
    env.pop('LC_MESSAGES', None)
    binary = Path(env['CARGO_TARGET_DIR']).resolve() / 'debug/pilotdeck-gateway'

    def run_cargo(label, test):
        slot = wait_for_slot(ROOT / 'scripts/cargo-slots.sh', timeout=900)
        (out / (label + '.slots.log')).write_text(slot.stdout + slot.stderr)
        result = run_owned(command(test), cwd=ROOT, env=env, timeout=900)
        log = result.stdout + result.stderr
        (out / (label + '.log')).write_text(log)
        return result, log

    def publish(content, mode):
        # Atomic publish for the stub and the restore alike (#166 r8): a
        # uniquely named sibling from mkstemp (a fixed name could let two
        # publishes into the same target dir clobber each other's pending
        # bytes, #1401 item 2), written, chmod'ed, then replaced, so nothing
        # ever reads a half-written executable.
        handle, pending = tempfile.mkstemp(
            dir=binary.parent, prefix=binary.name + '.early-exit.')
        try:
            try:
                stream = os.fdopen(handle, 'wb')
            except BaseException:
                # fdopen takes ownership only when it succeeds.
                os.close(handle)
                raise
            with stream:
                stream.write(content)
            os.chmod(pending, mode)
            os.replace(pending, binary)
        except BaseException:
            try:
                os.unlink(pending)
            except FileNotFoundError:
                # Already replaced (e.g. an interrupt between the two calls):
                # never mask the original exception with a cleanup error.
                pass
            raise

    def new_row(mutation, test):
        # One schema literal for every row, and only fields the shared
        # comparator knows: `_negkit` rejects unknown row fields, so the
        # internal restore bookkeeping below stays out of the row (#1401
        # review P1).
        return dict(mutation=mutation, test=test, assertion=MARKERS,
                    status='INVALID', reason='', restoredPass=False)

    rows = []
    # Only a row that proved its own verdict counts. Folding a failure into
    # `status` is not enough: the eof row expects INVALID, so any exception
    # (including SURVIVED, i.e. the stub never took effect) would otherwise
    # be indistinguishable from a real INVALID and the control could exit 0
    # without ever observing the early-exit panic (#1401 review P0).
    proved = set()
    restored = set()
    restored_pass = False
    # Same tree lease as `mutating_sources`: the stub window below rewrites a
    # shared binary path, so no other control on this tree may run inside it.
    with tree_lock(ROOT):
        try:
            positive_logs = {}
            for tag, test in TESTS:
                positive, log = run_cargo(f'early-exit-positive-{tag}', test)
                if not passed(positive, test):
                    raise RuntimeError(f'positive control failed: {test}')
                positive_logs[tag] = log
            found = TEST_BINARY.search(positive_logs['rejects'])
            if found is None or not Path(found.group(1)).is_file():
                raise RuntimeError('could not locate the built native_process test binary')
            if not binary.is_file():
                raise RuntimeError(f'missing built gateway binary: {binary}')
            original = binary.read_bytes()
            mode = stat.S_IMODE(binary.stat().st_mode)
            digest = hashlib.sha256(original).hexdigest()
            for name, test, stub, expected, payloads in ROWS:
                mutation = f'child-early-exit-stub-{name}'
                row = new_row(mutation, test)
                rows.append(row)
                try:
                    publish(stub.encode(), 0o755)
                    try:
                        # No cargo here (see the module docstring) and
                        # therefore no slot: this runs one already-built test
                        # executable to completion.
                        negative = run_owned([found.group(1), test, '--', '--exact'],
                                             cwd=ROOT, env=env, timeout=900)
                        log = negative.stdout + negative.stderr
                        (out / f'early-exit-neg-{name}.log').write_text(log)
                        # The judge's assertion list is the *kill* set, never
                        # the row's expected payload: handing it the
                        # early-exit message would make the unmarked INVALID
                        # panic look like a kill (#1401 review P0 -- caught by
                        # the live eof row).
                        case = SimpleNamespace(test=test, assertion=MARKERS)
                        status, reason = classify(negative, case)
                        evidence = panic_evidence(log, test)
                        if status != expected:
                            raise RuntimeError(f'{name} early exit judged {status}, '
                                               f'not {expected} ({reason})')
                        if not any(payload in evidence for payload in payloads):
                            raise RuntimeError(f'{name} early exit did not fail at its '
                                               f'own payload '
                                               f'(evidence: {evidence[:200]!r})')
                        row.update(status=status,
                                   reason=f'{name} early exit judged {status} '
                                           f'at the expected payload')
                        proved.add(mutation)
                    finally:
                        publish(original, mode)
                        # One read, both checks (#1401 item 2): the bytes and
                        # their digest must come from the same read.
                        restored_bytes = binary.read_bytes()
                        if (restored_bytes != original
                                or hashlib.sha256(restored_bytes).hexdigest() != digest):
                            raise RuntimeError('gateway binary restoration failed')
                        restored.add(mutation)
                except Exception as error:
                    row.update(status='INVALID',
                               reason=f'{type(error).__name__}: {error}')
                if mutation not in restored:
                    # The stub is still in place: every later row would judge
                    # a binary nobody chose. Abort the whole run rather than
                    # publish rows the tree cannot vouch for.
                    raise RuntimeError('gateway binary not restored; aborting')
            for tag, test in TESTS:
                restored_run, _ = run_cargo(f'early-exit-restored-{tag}', test)
                if not passed(restored_run, test):
                    raise RuntimeError(f'restored positive control failed: {test}')
            # Only after both controls passed: raising with the flag already
            # set would let the summary line publish `restoredPass: true` for
            # a run whose restored binary does not pass (#1401 review P1).
            restored_pass = True
        except BaseException as error:
            # Any global failure (positive control, restore, uplift) invalidates
            # every verdict already recorded: a KILLED that was observed before
            # a later breakage is no longer trustworthy as a row of its own
            # (#1401 item 1 P3; single implementation: negative_suite).
            message = f'{type(error).__name__}: {error}'
            invalidate_completed(rows, message)
            if not rows:
                # Failures before the first stub row (positive control, binary
                # lookup) must still publish their reason: an empty `controls`
                # list hides why the control is red (#1401 review).
                rows.append(new_row('early-exit-global', TEST))
                invalidate_completed(rows, message)
    for row in rows:
        row['restoredPass'] = restored_pass and row['mutation'] in restored
    publish_summary(out, rows)
    for row in rows:
        print(f"{row['mutation']}: {row['status']}: {row['reason']}", flush=True)
        print(f"restored={row['mutation'] in restored} "
              f"restoredPass={row['restoredPass']}", flush=True)
    return int(not (len(rows) == len(ROWS)
                    and all(row['mutation'] in proved for row in rows)
                    and all(row['mutation'] in restored for row in rows)
                    and all(row['status'] == EXPECTED[row['mutation']] for row in rows)
                    and all(row['restoredPass'] for row in rows)))


if __name__ == '__main__':
    raise SystemExit(main())
