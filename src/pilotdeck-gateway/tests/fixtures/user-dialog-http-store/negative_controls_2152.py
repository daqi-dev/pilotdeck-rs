#!/usr/bin/env python3
"""#2152 fix-r2 negative controls: parser-rejection keep-alive finishing.

Run after committing the candidate (the mutating_sources guard refuses a dirty
tree). Each mutant reverts one #2152 fix-r2 behaviour — the finishing a parser
rejection runs (idle timer armed, in-flight latch cleared) and the lifecycle
reset that supersedes the previous response's keep-alive advertisement — and
the bound libtest must fail at its own `#2152` assertion. Mutants are applied
to the original one at a time, never cumulatively, and every mutation is
restored and byte-verified afterwards. The full write-blocked pipeline replay
the review asked about is a registered P2 follow-up and stays out of these
state-level mutants.

    CARGO_TARGET_DIR=... python3 negative_controls_2152.py NEW_OUTPUT_DIR [name ...]
"""
import argparse
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import (CARGO_SLOTS_WAIT_SECONDS, mutating_sources,
                               run_owned, wait_for_slot)
from negative_suite import Case, classify, passed

root = Path(__file__).resolve().parents[5]
source = root / 'src/pilotdeck-gateway/src/user_dialog/http_store.rs'
TEST_PREFIX = 'user_dialog::http_store::http_store_tests::'

# (name, source, before, after, bound test, bound assertion). Every bound
# assertion is a `#2152` message owned by the named test, so only that test's
# own panic payload can produce a KILLED verdict.
cases = [
    # fix-r2: without the finishing, the timer stays in the request phase
    # (where expired() is None) with both deadlines cleared and the in-flight
    # latch on — a rejection whose 400 never drains then pins its connection
    # permit with no timer at all.
    ('rejection-finishing-removed', source,
     '        timer.response_rejected();\n'
     '        Ok(Some(rejection))',
     '        Ok(Some(rejection))',
     'parser_rejection_finishes_the_keepalive_lifecycle',
     '#2152 the rejection must finish the exchange'),
    # fix-r2: the reset is what supersedes the previous response's
    # still-unflushed keep-alive advertisement (the pipelined interleaving);
    # response_finished alone keeps the lifecycle's stale fields.
    ('rejection-lifecycle-reset-removed', source,
     '    fn response_rejected(&self) {\n'
     '        {\n'
     '            let mut lifecycle = self.lifecycle.lock().unwrap();\n'
     '            lifecycle.advertised_keep_alive = false;\n'
     '            lifecycle.close_after_response = false;\n'
     '            lifecycle.response_write = ResponseWrite::default();\n'
     '        }\n'
     '        self.response_finished();\n'
     '    }',
     '    fn response_rejected(&self) {\n'
     '        self.response_finished();\n'
     '    }',
     'parser_rejection_finishes_the_keepalive_lifecycle',
     '#2152 the rejection must supersede the previous keep-alive advertisement'),
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
        # #1908: the wait budget must exceed the single-sample cap
        slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=CARGO_SLOTS_WAIT_SECONDS)
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
