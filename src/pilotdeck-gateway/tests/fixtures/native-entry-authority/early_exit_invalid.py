#!/usr/bin/env python3
# anchor-selfcheck: unsupported build-artifact signal injection, no source mutation table
"""#1261 fix-r2 / #1401 item 1: early exits split into INVALID and KILLED.

The bound test (tests/native_process.rs) answers a child that exits with
**neither** the rejection nor its ``native listening`` advertisement on stderr
-- an environmental early exit (dyld, OOM, assembly) -- with an unmarked panic,
so it can never fake a kill (INVALID). A child that exits **after** advertising
``native listening`` has already proven the entry gate accepted the inherited
authority, so the marked assertion kills (KILLED). This control injects both
exits without mutating any source: the built gateway binary in the
caller-supplied ``CARGO_TARGET_DIR`` is atomically replaced by a stub that
either exits immediately with empty output, or prints
``native listening 127.0.0.1:1`` to stderr first and then exits.

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
control must pass. No source mutation happens here, so unlike
``negative_controls.py`` this runs on an uncommitted tree. No ``-q``: the
judge needs the named per-test lines.
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
from negative_panic import panic_evidence
from negative_process import new_output_directory, run_owned, wait_for_slot
from negative_suite import classify, passed, publish_summary

ROOT = Path(__file__).resolve().parents[5]
TEST_FILE = ROOT / 'src/pilotdeck-gateway/tests/native_process.rs'
TEST = 'native_entry_process_rejects_inherited_authority'
MARKERS = [
    '#1261 native entry rejects inherited PILOTDECK_STORAGE_ROOT',
    '#1261 native entry rejects inherited PILOTDECK_SNAPSHOT_ROOT',
    '#1261 native entry rejects inherited PILOTDECK_MANAGED_CONFIG',
]
# The unmarked early-exit panic below must stay distinct from every kill
# marker: the shared judge kills only on a marker inside the named test's
# panic payload, and the INVALID branch deliberately offers none.
EARLY_EXIT = ('#1261 native entry child exited early; no rejection and '
              'no listening advertisement for')
# The gate that separates the unmarked early-exit panic from the marked
# kill, and the observation that the advertisement is watched at all.
GATE = 'if !stderr.contains(&rejection) {'
ADVERTISED = 'read_stderr_until(stderr, "native listening"'
# Two stub shapes, two verdicts (#1401 item 1): an environmental exit prints
# nothing and can only be judged INVALID, while a child that advertised
# listening before exiting proved the gate accepted the authority, so the
# marked assertion must kill. The advertisement text is the child's own
# stderr line ("native listening <addr>"), not the rejection.
STUBS = (
    ('eof', '#!/bin/sh\nexit 86\n', 'INVALID'),
    ('listening', '#!/bin/sh\necho "native listening 127.0.0.1:1" >&2\nexit 86\n',
     'KILLED'),
)
CASE = SimpleNamespace(test=TEST, assertion=MARKERS)
COMMAND = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
           '-p', 'pilotdeck-gateway', '--test', 'native_process',
           TEST, '--', '--exact']
TEST_BINARY = re.compile(r'Running tests/native_process\.rs \(([^)]+)\)')


def test_body(source, name):
    """The named test function's body, up to the next test attribute.

    A file-level needle cannot say *which* test observes what: after #1401
    item 6 added a second test with the same ``native listening`` string, a
    file-level check passed even with the rejects test's observation deleted
    (#1401 review P2, found by both lineages).
    """
    start = re.search(rf'^async fn {re.escape(name)}\(', source, re.M)
    if start is None:
        start = re.search(rf'^fn {re.escape(name)}\(', source, re.M)
    if start is None:
        raise SystemExit(f'#1261 test missing from native_process.rs: {name}')
    following = re.search(r'^#\[(?:tokio::)?test\]', source[start.end():], re.M)
    end = start.end() + following.start() if following else len(source)
    return source[start.end():end]


def main():
    if sys.argv[1:] == ['--check']:
        test_source = TEST_FILE.read_text()
        body = test_body(test_source, TEST)
        for needle, what in ((EARLY_EXIT, 'early-exit panic template'),
                             (GATE, 'rejection gate'),
                             (ADVERTISED, 'listening advertisement observation'),
                             ('#1261 native entry rejects inherited {key}',
                              'marker template')):
            if needle not in body:
                raise SystemExit(f'#1261 missing {what} from {TEST}')
        for key in ('PILOTDECK_STORAGE_ROOT', 'PILOTDECK_SNAPSHOT_ROOT',
                    'PILOTDECK_MANAGED_CONFIG'):
            if f'"{key}"' not in body:
                raise SystemExit(f'#1261 key missing from {TEST}: {key}')
        for marker in MARKERS:
            if marker in EARLY_EXIT or EARLY_EXIT in marker:
                raise SystemExit(f'#1261 early-exit panic collides with marker {marker}')
        print(f'#1261 {TEST} early-exit anchors OK')
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

    def run_cargo(label):
        slot = wait_for_slot(ROOT / 'scripts/cargo-slots.sh', timeout=900)
        (out / (label + '.slots.log')).write_text(slot.stdout + slot.stderr)
        result = run_owned(COMMAND, cwd=ROOT, env=env, timeout=900)
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

    rows = []
    # Per-row expectations live here, not in the published row: the summary
    # schema is shared with the other fixtures (`_negkit` compares it), so an
    # ad-hoc `expected` key would be a schema drift (#1401 review).
    expected_of = {f'child-early-exit-stub-{name}': expected
                   for name, _, expected in STUBS}
    # Only a row that proved its own verdict counts. Folding a failure into
    # `status` is not enough: the eof row expects INVALID, so any exception
    # (including SURVIVED, i.e. the stub never took effect) would otherwise
    # be indistinguishable from a real INVALID and the control could exit 0
    # without ever observing the early-exit panic (#1401 review P0).
    proved = set()
    try:
        positive, positive_log = run_cargo('early-exit-positive')
        if not passed(positive, TEST):
            raise RuntimeError('positive control failed')
        found = TEST_BINARY.search(positive_log)
        if found is None or not Path(found.group(1)).is_file():
            raise RuntimeError('could not locate the built native_process test binary')
        if not binary.is_file():
            raise RuntimeError(f'missing built gateway binary: {binary}')
        original = binary.read_bytes()
        mode = stat.S_IMODE(binary.stat().st_mode)
        digest = hashlib.sha256(original).hexdigest()
        for name, stub, expected in STUBS:
            row = dict(mutation=f'child-early-exit-stub-{name}', test=TEST,
                       assertion=MARKERS, status='INVALID', reason='',
                       stubRestored=False, restoredPass=False)
            rows.append(row)
            try:
                publish(stub.encode(), 0o755)
                try:
                    # No cargo here (see the module docstring) and therefore
                    # no slot: this runs one already-built test executable to
                    # completion.
                    negative = run_owned([found.group(1), TEST, '--', '--exact'],
                                         cwd=ROOT, env=env, timeout=900)
                    log = negative.stdout + negative.stderr
                    (out / f'early-exit-neg-{name}.log').write_text(log)
                    status, reason = classify(negative, CASE)
                    evidence = panic_evidence(log, TEST)
                    if status != expected:
                        raise RuntimeError(f'{name} early exit judged {status}, '
                                           f'not {expected} ({reason})')
                    if expected == 'INVALID' and EARLY_EXIT not in evidence:
                        raise RuntimeError('named test did not fail at the early-exit '
                                           f'panic (evidence: {evidence[:200]!r})')
                    if expected == 'KILLED' and not any(marker in evidence
                                                        for marker in MARKERS):
                        raise RuntimeError('named test did not fail at a bound '
                                           f'marker (evidence: {evidence[:200]!r})')
                    row.update(status=status,
                                reason=f'{name} early exit judged {status} '
                                        f'at the expected payload')
                    proved.add(row['mutation'])
                finally:
                    publish(original, mode)
                    # One read, both checks (#1401 item 2): the bytes and their
                    # digest must come from the same read.
                    restored_bytes = binary.read_bytes()
                    if (restored_bytes != original
                            or hashlib.sha256(restored_bytes).hexdigest() != digest):
                        raise RuntimeError('gateway binary restoration failed')
                    row['stubRestored'] = True
            except Exception as error:
                row.update(status='INVALID',
                           reason=f'{type(error).__name__}: {error}')
        restored, _ = run_cargo('early-exit-restored')
        restored_pass = passed(restored, TEST)
        for row in rows:
            row['restoredPass'] = restored_pass
        if not restored_pass:
            # A failed restored positive control makes every recorded KILLED
            # untrustworthy, exactly like a global failure (#1401 review).
            for row in rows:
                row['status'] = 'INVALID'
                row['reason'] = (row['reason'] + '; ' if row['reason'] else '') \
                    + 'restored positive control failed'
    except BaseException as error:
        # Any global failure (positive control, restore, uplift) invalidates
        # every verdict already recorded: a KILLED that was observed before a
        # later breakage is no longer trustworthy as a row of its own
        # (#1401 item 1 P3).
        message = f'{type(error).__name__}: {error}'
        for row in rows:
            row['status'] = 'INVALID'
            row['reason'] = (row['reason'] + '; ' if row['reason'] else '') + message
        if not rows:
            # Failures before the first stub row (positive control, binary
            # lookup) must still publish their reason: an empty `controls`
            # list hides why the control is red (#1401 review).
            rows.append(dict(mutation='early-exit-global', test=TEST,
                             assertion=MARKERS, status='INVALID',
                             reason=message, stubRestored=False,
                             restoredPass=False))
    publish_summary(out, rows)
    for row in rows:
        print(f"{row['mutation']}: {row['status']}: {row['reason']}", flush=True)
        print(f"stubRestored={row['stubRestored']} restoredPass={row['restoredPass']}",
              flush=True)
    return int(not (len(rows) == len(STUBS)
                    and all(row['mutation'] in proved for row in rows)
                    and all(row['status'] == expected_of[row['mutation']] for row in rows)
                    and all(row['stubRestored'] and row['restoredPass']
                            for row in rows)))


if __name__ == '__main__':
    raise SystemExit(main())
