#!/usr/bin/env python3
# anchor-selfcheck: unsupported build-artifact signal injection, no source mutation table
"""#1261 fix-r2: a child that exits early must judge INVALID, never KILLED.

The bound test's ``Ok(Ok(_eof))`` branch (tests/native_process.rs) answers a
child that exits before the entry gate -- or between the gate and its
``native listening`` advertisement -- with an unmarked panic, so an
environmental early exit (dyld, OOM, assembly) can never fake a kill. This
control injects exactly that exit without mutating any source: the built
gateway binary in the caller-supplied ``CARGO_TARGET_DIR`` is atomically
replaced by a stub that exits immediately with empty output.

The negative step runs the already-built test executable directly, not
through cargo: every ``cargo test`` re-publishes ``debug/pilotdeck-gateway``
from the ``debug/deps`` artifact before the integration test starts, which
would silently revert the stub. The positive and restored steps still go
through cargo (and its uplift), so a clean tree is proven on both sides of
the injection. The shared ``negative_suite.classify`` must return INVALID
with the early-exit panic inside the named test's own payload (any other
unmarked failure is not the proof), the original binary is restored and
SHA-256 verified, and no source mutation happens here, so unlike
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

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_panic import panic_evidence
from negative_process import run_owned, wait_for_slot
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
# panic payload, and this branch deliberately offers none.
EARLY_EXIT = ('#1261 native entry child exited early; no rejection and '
              'no listening advertisement for')
GATE = 'if !stderr.contains(&rejection) {'
# The stub mimics an environmental early exit (dyld, OOM, assembly): no
# stderr at all, so the bound test can only observe an immediate EOF.
STUB = '#!/bin/sh\nexit 86\n'
CASE = SimpleNamespace(test=TEST, assertion=MARKERS)
COMMAND = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
           '-p', 'pilotdeck-gateway', '--test', 'native_process',
           TEST, '--', '--exact']
TEST_BINARY = re.compile(r'Running tests/native_process\.rs \(([^)]+)\)')


def main():
    if sys.argv[1:] == ['--check']:
        test_source = TEST_FILE.read_text()
        for needle, what in ((EARLY_EXIT, 'early-exit panic template'),
                             (GATE, 'rejection gate'),
                             ('#1261 native entry rejects inherited {key}',
                              'marker template')):
            if needle not in test_source:
                raise SystemExit(f'#1261 missing {what} from {TEST}')
        for key in ('PILOTDECK_STORAGE_ROOT', 'PILOTDECK_SNAPSHOT_ROOT',
                    'PILOTDECK_MANAGED_CONFIG'):
            if f'"{key}"' not in test_source:
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
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)
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
        # Atomic publish for the stub and the restore alike (#166 r8): write
        # a sibling, then replace, so nothing ever reads a half-written
        # executable.
        pending = binary.with_name(binary.name + '.early-exit.tmp')
        with pending.open('wb') as stream:
            stream.write(content)
        os.chmod(pending, mode)
        pending.replace(binary)

    row = dict(mutation='child-early-exit-stub', test=TEST, assertion=MARKERS,
               status='INVALID', reason='', stubRestored=False, restoredPass=False)
    original = None
    mode = None
    proved = False
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
        publish(STUB.encode(), 0o755)
        try:
            # No cargo here (see the module docstring) and therefore no slot:
            # this runs one already-built test executable to completion.
            negative = run_owned([found.group(1), TEST, '--', '--exact'],
                                 cwd=ROOT, env=env, timeout=900)
            log = negative.stdout + negative.stderr
            (out / 'early-exit-negative.log').write_text(log)
            status, reason = classify(negative, CASE)
            evidence = panic_evidence(log, TEST)
            row.update(status=status)
            if status != 'INVALID':
                raise RuntimeError(f'early exit judged {status}, not INVALID ({reason})')
            if EARLY_EXIT not in evidence:
                raise RuntimeError('named test did not fail at the early-exit panic '
                                   f'(evidence: {evidence[:200]!r})')
            row.update(reason='child early exit judged INVALID at the unmarked panic')
            proved = True
        finally:
            publish(original, mode)
            if (binary.read_bytes() != original
                    or hashlib.sha256(binary.read_bytes()).hexdigest() != digest):
                raise RuntimeError('gateway binary restoration failed')
            row['stubRestored'] = True
        restored, _ = run_cargo('early-exit-restored')
        row['restoredPass'] = passed(restored, TEST)
    except BaseException as error:
        proved = False
        row['reason'] = (row['reason'] + '; ' if row['reason'] else '') + f'{type(error).__name__}: {error}'
    publish_summary(out, [row])
    print(f"child-early-exit-stub: {row['status']}: {row['reason']}", flush=True)
    print(f"stubRestored={row['stubRestored']} restoredPass={row['restoredPass']}", flush=True)
    return int(not (proved and row['stubRestored'] and row['restoredPass']))


if __name__ == '__main__':
    raise SystemExit(main())
