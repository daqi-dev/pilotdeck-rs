#!/usr/bin/env python3
"""#1277 negative controls: user_dialog HTTP store 431/408 P2 follow-ups.

Run after committing the candidate (the mutating_sources guard refuses a dirty
tree). Each mutant reverts one #1277 behaviour — the serve backstop that
answers a stepped-aside oversized head with Node's bytes, the step-aside that
stops the guard from scanning on, and the timer wake that unparks a zero-room
read — and the bound libtest must fail at its own `#1277`/`#1232` assertion.
Mutants are applied to the original one at a time, never cumulatively, and
every mutation is restored and byte-verified afterwards.

    CARGO_TARGET_DIR=... python3 negative_controls_1277.py NEW_OUTPUT_DIR [name ...]
"""
import argparse
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import Case, classify, passed

root = Path(__file__).resolve().parents[5]
source = root / 'src/pilotdeck-gateway/src/user_dialog/http_store.rs'
TEST_PREFIX = 'user_dialog::http_store::http_store_tests::'

# (name, source, before, after, bound test, bound assertion). Every bound
# assertion is a `#1277`/`#1232` message owned by the named test, so only that
# test's own panic payload can produce a KILLED verdict.
cases = [
    # The serve backstop is the only thing that answers B after the guard
    # stepped aside on A: push the threshold out of reach and B is served as a
    # normal (body-less) request instead of ending the wire with Node's 431.
    ('backstop-threshold-never-trips', source,
     '    if header_bytes >= max_header_bytes {',
     '    if header_bytes >= max_header_bytes + 1_000_000 {',
     'pipelined_oversized_after_step_aside_te_single_burst_writes_served_response_then_node_431',
     '#1277 the single-burst oversized head behind a stepped-aside request ends the connection with the Node 431'),
    # A guard that keeps scanning past its step-aside counts B's 200 KB head
    # during the read-ahead and answers Node's 67 bytes itself, so the stepped-
    # aside connection no longer shows hyper's registered shape.
    ('step-aside-rescans-read-chunk', source,
     '        for &byte in bytes {\n'
     '            if !self.active {\n'
     '                return false;\n'
     '            }\n',
     '        for &byte in bytes {\n',
     'oversized_head_over_hyper_buffer_after_step_aside_writes_hyper_431',
     '#1232 after a step-aside an oversized head beyond hyper\'s buffer cap is hyper\'s own registered 431 shape, not Node\'s bytes'),
    # Without the timer's wake on write activity, the parked zero-room read is
    # never woken by the connection's in-band events.
    ('write-activity-drops-wake', source,
     '            if matches!(state.phase, TimeoutPhase::Idle | TimeoutPhase::NextHeaders) {\n'
     '                state.idle_deadline = Some(now + self.keep_alive_timeout);\n'
     '            }\n'
     '        }\n'
     '        self.wake();\n'
     '    }\n',
     '            if matches!(state.phase, TimeoutPhase::Idle | TimeoutPhase::NextHeaders) {\n'
     '                state.idle_deadline = Some(now + self.keep_alive_timeout);\n'
     '            }\n'
     '        }\n'
     '    }\n',
     'zero_room_park_is_woken_by_connection_activity',
     '#1277 the connection\'s next write activity must wake the parked zero-room read'),
]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('out', type=Path)
    parser.add_argument('mutations', nargs='*')
    args = parser.parse_args()
    env = os.environ.copy()
    if not env.get('CARGO_TARGET_DIR'):
        parser.error('CARGO_TARGET_DIR must be provided by the caller')
    unknown = set(args.mutations) - {row[0] for row in cases}
    if unknown:
        parser.error(f'unknown mutations: {sorted(unknown)}')
    selected = [row for row in cases if not args.mutations or row[0] in args.mutations]
    args.out.mkdir(parents=True, exist_ok=False)
    command = ['cargo', 'test', '--manifest-path', 'src/Cargo.toml',
               '-p', 'pilotdeck-gateway', '--lib']
    results = []

    def run(label, full):
        slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=300)
        (args.out / f'{label}.slots.log').write_text(slot.stdout + slot.stderr)
        result = run_owned(command + [full, '--', '--exact'],
                           cwd=root, env=env, timeout=900)
        (args.out / f'{label}.log').write_text(result.stdout + result.stderr)
        return result

    def record(name, full, assertion, status, reason):
        results.append(dict(mutation=name, test=full, assertion=assertion,
                            status=status, reason=reason))
        print(f'{name}: {status}: {reason}', flush=True)

    try:
        with mutating_sources(root, [source]) as snapshots:
            for name, path, old, new, test, assertion in selected:
                full = TEST_PREFIX + test
                status, reason = 'INVALID', 'not run'
                try:
                    original = snapshots[path].decode()
                    count = original.count(old)
                    if count != 1:
                        raise ValueError(
                            f'{name}: expected one source anchor, found {count}')
                    if not passed(run(name + '-positive', full), full):
                        reason = 'bound positive test did not pass'
                    else:
                        path.write_text(original.replace(old, new, 1))
                        status, reason = classify(run(name + '-negative', full),
                                                  Case(name, path, old, new,
                                                       full, assertion))
                except Exception as error:
                    status, reason = 'INVALID', f'{type(error).__name__}: {error}'
                finally:
                    path.write_bytes(snapshots[path])
                try:
                    if not passed(run(name + '-restored', full), full):
                        status, reason = 'INVALID', reason + '; restored bound test did not pass'
                except Exception as error:
                    status, reason = 'INVALID', reason + f'; restored: {error}'
                record(name, full, assertion, status, reason)
    except Exception as error:
        reason = f'source guard: {error}'
        for row in results:
            row.update(status='INVALID', reason=row['reason'] + '; ' + reason)
        for name, _path, _old, _new, test, assertion in selected[len(results):]:
            record(name, TEST_PREFIX + test, assertion, 'INVALID', reason)
    counts = {state: sum(row['status'] == state for row in results)
              for state in ('KILLED', 'SURVIVED', 'INVALID')}
    (args.out / 'summary.json').write_text(
        json.dumps(dict(controls=results, counts=counts), indent=2) + '\n')
    print(json.dumps(counts), flush=True)
    return int(not results or any(row['status'] != 'KILLED' for row in results))


if __name__ == '__main__':
    if sys.argv[1:] == ['--check-anchors']:
        stale = []
        for name, path, old, _new, _test, _assertion in cases:
            count = path.read_text().count(old)
            if count != 1:
                stale.append((name, count))
        if stale:
            raise SystemExit(f'STALE ANCHORS: {stale}')
        print(f'{len(cases)} anchors: ok')
        raise SystemExit(0)
    raise SystemExit(main())
