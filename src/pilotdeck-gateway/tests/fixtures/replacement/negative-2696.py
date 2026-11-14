#!/usr/bin/env python3
"""#2696 regression mutations; exclusively own this worktree while running.

Every mutant removes one half of the Drop-guard pair that releases
`replace_last_turn`'s self-held reservation (arm in the admission critical
section / release in `OwnedTranscriptWriteReservation::drop`), and must be
killed by `panicking_body_releases_reservation` — the test that poisons the
coordinator state lock mid-body and requires the reservation to be released
despite the JoinError. All anchors are checked before writes; compile errors
and timeouts are INVALID, never kills. Run all mutants, then summarize.
"""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_panic import named_test_failed, named_test_killed
from negative_process import (CARGO_SLOTS_WAIT_SECONDS, mutating_sources,
                               run_owned, wait_for_slot)

ROOT = Path(__file__).resolve().parents[5]
PRODUCT = ROOT / 'src/pilotdeck-gateway/src/runtime/replacement.rs'
TEST = 'runtime::replacement::tests::panicking_body_releases_reservation'
# (name, path, before, after, count, expected_failure_text). Each `before`
# anchors exactly once in the fixed source; a stale anchor fails the preflight
# below before anything is written (never recorded as SURVIVED).
MUTATIONS = [
    # 1. Drop guard neutered: the armed guard releases nothing, so the
    #    mid-body panic leaves the reservation held — the #2696 leak itself.
    ('drop_guard_neutered', PRODUCT,
     '        if let Some(key) = self.key.take() {\n            self.coordinator.release_transcript_write(&key);\n        }\n    }\n}\nimpl ReplacementCoordinator {',
     '        if let Some(key) = self.key.take() {\n            let _ = key;\n        }\n    }\n}\nimpl ReplacementCoordinator {',
     1, '#2696 mid-body panic leaked the replace_last_turn reservation'),
    # 2. Guard never armed: the admission's insert has no owner, so nothing
    #    ever releases the reservation (also the pre-fix shape).
    ('guard_never_armed', PRODUCT,
     '            state.reservations.insert(key.clone());\n            reservation.arm(&key);',
     '            state.reservations.insert(key.clone());\n            reservation.key = None;',
     1, '#2696 mid-body panic leaked the replace_last_turn reservation'),
    # 3. Guard dropped at body start: the reservation is released while the
    #    spawned body is still in flight (premature release).
    ('guard_released_eagerly', PRODUCT,
     '            let _reservation = reservation;',
     '            drop(reservation);',
     1, '#2696 reservation must be held while the spawned body is in flight'),
]
# A failed test alone is insufficient: dependency/host failures are INVALID.
EXPECTED = {name: text for name, _path, _before, _after, _count, text in MUTATIONS}


def invalid_reason(name, returncode, output):
    if returncode != 101:
        return f'test did not fail with exit 101 (rc={returncode})'
    if 'could not compile' in output:
        return 'test did not compile'
    if not named_test_failed(output, TEST):
        return f'test {TEST} ... FAILED is missing'
    if not named_test_killed(output, TEST, EXPECTED[name]):
        return f'panic block lacks the bound marker {EXPECTED[name]!r}'
    return None


def main():
    # Reject before any mutation, cargo invocation or evidence-directory creation.
    if not os.environ.get('CARGO_TARGET_DIR'):
        print('INVALID: set CARGO_TARGET_DIR before running #2696 mutations', file=sys.stderr)
        return 2
    evidence = Path(sys.argv[1]).resolve()
    evidence.mkdir(parents=True, exist_ok=True)
    with mutating_sources(ROOT, [PRODUCT]) as snapshots:
        original = snapshots[PRODUCT]
        # Guard every anchor before mutating anything; an unexpected source
        # version fails closed and is never recorded as a kill.
        for name, _path, before, after, count, _text in MUTATIONS:
            found = original.count(before.encode())
            if found != count:
                print(f'INVALID: {name}: anchor occurs {found} times (expected {count})', file=sys.stderr)
                return 2
            if after in original.decode():
                # A mutant that is already the current source proves nothing.
                print(f'INVALID: {name}: mutant text already present', file=sys.stderr)
                return 2
        summary = []
        baseline_log = evidence / 'baseline.log'
        slots = wait_for_slot(ROOT / 'scripts/cargo-slots.sh', timeout=CARGO_SLOTS_WAIT_SECONDS)
        baseline = run_owned(
            ['cargo', 'test', '--offline', '-p', 'pilotdeck-gateway', '--lib', TEST, '--', '--exact'],
            cwd=ROOT / 'src', timeout=300)
        baseline_log.write_text(slots.stdout + slots.stderr + baseline.stdout + baseline.stderr)
        if baseline.returncode != 0 or f'test {TEST} ... ok' not in (baseline.stdout + baseline.stderr):
            print(f'INVALID baseline failed; see {baseline_log}', file=sys.stderr)
            return 1
        for name, _path, before, after, _count, _text in MUTATIONS:
            slots = wait_for_slot(ROOT / 'scripts/cargo-slots.sh', timeout=CARGO_SLOTS_WAIT_SECONDS)
            assert PRODUCT.read_bytes() == original, 'concurrent source edit'
            mutated = original.replace(before.encode(), after.encode())
            log = evidence / f'{name}.log'
            print(f'RUN {name}', flush=True)
            record = {'mutation': name, 'status': 'INVALID', 'before': before, 'after': after,
                      'expectedFailureText': EXPECTED[name],
                      'originalSha256': hashlib.sha256(original).hexdigest()}
            try:
                PRODUCT.write_bytes(mutated)
                with log.open('w') as output:
                    output.write(slots.stdout + slots.stderr)
                try:
                    proc = run_owned(
                        ['cargo', 'test', '--offline', '-p', 'pilotdeck-gateway', '--lib', TEST, '--', '--exact'],
                        cwd=ROOT / 'src', timeout=300)
                except subprocess.TimeoutExpired as error:
                    with log.open('a') as output:
                        output.write((error.output or '') + (error.stderr or ''))
                    raise
                with log.open('a') as output:
                    output.write(proc.stdout + proc.stderr)
                output = log.read_text(errors='replace')
                record['exit'] = proc.returncode
                reason = invalid_reason(name, proc.returncode, output)
                if reason:
                    record['reason'] = reason
                    print(f'INVALID {name}: {reason}; {log}', flush=True)
                else:
                    record.update(status='KILLED')
                    print(f'RED {name}: killed by {TEST}', flush=True)
            except subprocess.TimeoutExpired:
                record['reason'] = 'test timed out'
                print(f'INVALID {name}: test timed out; {log}', flush=True)
            finally:
                assert PRODUCT.read_bytes() == mutated, f'concurrent edit: {PRODUCT}'
                PRODUCT.write_bytes(original)
                assert PRODUCT.read_bytes() == original
                summary.append(record)
                (evidence / 'summary.json').write_text(json.dumps(summary, ensure_ascii=False, indent=2) + '\n')
            if record['status'] != 'KILLED':
                print(f'INVALID {name}: continuing with the remaining controls', flush=True)
        counts = {state: sum(row['status'] == state for row in summary)
                  for state in ('KILLED', 'INVALID')}
        print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()), flush=True)
        killed = sum(1 for row in summary if row['status'] == 'KILLED')
        print(f'{killed}/{len(MUTATIONS)} killed; original bytes restored', flush=True)
        if any(row['status'] != 'KILLED' for row in summary):
            print(f'not killed: {", ".join(row["mutation"] for row in summary if row["status"] != "KILLED")}',
                  file=sys.stderr)
            return 1
        return 0


if __name__ == '__main__':
    sys.exit(main())
