#!/usr/bin/env python3
"""#1555 negative controls: leading-OWS drop and field-count alignment.

Run after committing the candidate (the mutating_sources guard refuses a dirty
tree). Each mutant reverts one #1555 behaviour — the guard's after-colon
leading-OWS drop (not applied, still raw-counted, not removed from the stream,
the chunk-end run left un-finalized, folding whitespace wrongly dropped too)
and hyper's field-count cap sized from `max_header_bytes` — and the bound
libtest must fail at its own `#1555` assertion. Mutants are applied to the
original one at a time, never cumulatively, and every mutation is restored and
byte-verified afterwards.

    CARGO_TARGET_DIR=... python3 negative_controls_1555.py NEW_OUTPUT_DIR [name ...]
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
# assertion is a `#1555` message owned by the named test, so only that test's
# own panic payload can produce a KILLED verdict.
cases = [
    # The drop is never decided: after-colon OWS is raw-counted again and the
    # 90 KB pad trips the raw cap, writing the 431 the R3 entry registered.
    ('ows-still-raw-counted', source,
     '        if self.droppable_ows\n'
     '            && self.discarding_leading_whitespace\n'
     '            && (byte == b\' \' || byte == b\'\\t\')\n'
     '        {\n',
     '        if false\n'
     '            && self.droppable_ows\n'
     '            && self.discarding_leading_whitespace\n'
     '            && (byte == b\' \' || byte == b\'\\t\')\n'
     '        {\n',
     'leading_value_ows_beyond_the_raw_cap_reaches_the_handler',
     '#1555 leading value OWS must reach the handler like Node'),
    # The bytes are dropped from the stream but still charged to the raw cap:
    # the pad trips the bound even though hyper never sees the whitespace.
    ('ows-dropped-but-raw-counted', source,
     '            self.drop_start.get_or_insert(index);\n'
     '            return false;\n'
     '        }\n'
     '        if let Some(start) = self.drop_start.take() {\n',
     '            self.drop_start.get_or_insert(index);\n'
     '            self.raw_bytes = self.raw_bytes.saturating_add(1);\n'
     '            if self.raw_bytes >= self.raw_limit {\n'
     '                self.active = false;\n'
     '                return true;\n'
     '            }\n'
     '            return false;\n'
     '        }\n'
     '        if let Some(start) = self.drop_start.take() {\n',
     'leading_value_ows_beyond_the_raw_cap_reaches_the_handler',
     '#1555 leading value OWS must reach the handler like Node'),
    # The compaction keeps every byte: the pad flows into hyper's read buffer,
    # which answers its own 123-byte 431 once `max_buf_size` is exceeded.
    ('ows-not-dropped-from-stream', source,
     '        let dropped = std::mem::take(&mut self.dropped);\n',
     '        let dropped: Vec<(usize, usize)> = Vec::new();\n'
     '        let _ = std::mem::take(&mut self.dropped);\n',
     'leading_value_ows_beyond_the_raw_cap_reaches_the_handler',
     '#1555 leading value OWS must reach the handler like Node'),
    # Folding whitespace is dropped too: the raw cap no longer trips on the
    # continuation pad and hyper sees a colon-less `b` field line instead of
    # the guard's Node bytes.
    ('obs-fold-ows-also-dropped', source,
     '                    self.line = GuardLine::FieldValue;\n'
     '                    self.discarding_leading_whitespace = true;\n'
     '                    self.droppable_ows = false;\n',
     '                    self.line = GuardLine::FieldValue;\n'
     '                    self.discarding_leading_whitespace = true;\n'
     '                    self.droppable_ows = true;\n',
     'header_guard_raw_cap_uses_node_431_bytes',
     '#1555 the guard must own the 431 bytes for whitespace it still forwards'),
    # The field count is capped at hyper's old 4096 again: 4,097 one-byte
    # fields never reach the handler.
    ('max-headers-back-to-4096', source,
     '                            .max_headers(max_header_bytes)\n',
     '                            .max_headers(4096)\n',
     'four_thousand_ninety_seven_small_fields_reach_the_handler',
     '#1555 4,097 one-byte fields must reach the handler like Node'),
    # A dropped run still open at the end of a read chunk is never closed, so
    # every all-whitespace chunk is forwarded whole and hyper's buffer cap
    # answers instead of the handler.
    ('open-run-not-finalized', source,
     '        if let Some(start) = self.drop_start.take() {\n'
     '            // The chunk ended in the middle of a dropped run; it extends to\n'
     '            // every byte the guard scanned (`scanned` == the chunk length on\n'
     '            // the normal path).\n'
     '            self.dropped.push((start, self.scanned));\n'
     '        }\n',
     '        let _ = self.drop_start.take();\n',
     'leading_value_ows_beyond_the_raw_cap_reaches_the_handler',
     '#1555 leading value OWS must reach the handler like Node'),
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
