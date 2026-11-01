#!/usr/bin/env python3
"""#1232 negative controls: user_dialog HTTP store 408/431 wire-shape guards.

Run after committing the candidate (the mutating_sources guard refuses a dirty
tree). Each mutant reverts one #1232 behaviour to its pre-fix shape — hyper's
123-byte error encoding, a guard that refuses to scan request bodies, a
connection-wide header budget — and the bound libtest must fail at its own
`#1232` assertion. The fix-r1 additions revert the step-aside semantics: a
guard that scans on past its step-aside, a zero-capacity read latched as the
peer's EOF, and an empty `Content-Length` value read as zero. Mutants are
applied to the original one at a time, never cumulatively, and every mutation
is restored and byte-verified afterwards.

    CARGO_TARGET_DIR=... python3 negative_controls_1232.py NEW_OUTPUT_DIR [name ...]
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
# assertion is a `#1232` message owned by the named test, so only that test's
# own panic payload can produce a KILLED verdict. An assertion may be a list:
# the bound test then dies at any one of its own assertions (the budget-reset
# mutant can hit either response depending on where the false 431 lands, #1232
# fix-r1 P1-2).
cases = [
    ('body-408-hyper-shape', source,
     '            timer.force_node_response(NODE_408_RESPONSE);\n'
     '            return empty_response(StatusCode::REQUEST_TIMEOUT);',
     '            return empty_response(StatusCode::REQUEST_TIMEOUT);',
     'slow_request_bodies_are_closed_after_request_timeout',
     "#1232 the body requestTimeout must be Node's 51-byte 408"),
    ('backstop-431-hyper-shape', source,
     '        timer.force_node_response(NODE_431_RESPONSE);\n'
     '        return empty_response(StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE);',
     '        return empty_response(StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE);',
     'duplicate_content_length_pipelined_head_hits_node_431_backstop',
     "#1232 the backstop must still answer an oversized head with Node's 431 bytes"),
    ('forced-write-wiring', source,
     '        if let Some(raw) = this.timer.take_forced_response() {\n'
     '            // #1232: `serve` replaced this response\'s bytes with Node\'s own\n'
     '            // (the body `requestTimeout` 408 and the `header_bytes` 431\n'
     '            // backstop). Write them, then break hyper\'s write so none of its\n'
     '            // own encoding reaches the socket.\n'
     '            this.raw_response = Some(raw);\n'
     '            this.raw_written = 0;\n'
     '            if this.poll_raw_response(cx).is_pending() {\n'
     '                return Poll::Pending;\n'
     '            }\n'
     '            return Poll::Ready(Err(io::Error::new(\n'
     '                io::ErrorKind::BrokenPipe,\n'
     '                "connection closed",\n'
     '            )));\n'
     '        }',
     '',
     'slow_request_bodies_are_closed_after_request_timeout',
     "#1232 the body requestTimeout must be Node's 51-byte 408"),
    # fix-r1: begin() is gone, so the phase-blind mutant now skips the guard
    # scan itself while the timer reports the request-body phase.
    ('guard-request-phase-blind', source,
     '                    this.timer.read_activity();\n'
     '                    if this.header_guard.push(filled) {',
     '                    this.timer.read_activity();\n'
     '                    if !matches!(this.timer.phase(), TimeoutPhase::Request)\n'
     '                        && this.header_guard.push(filled)\n'
     '                    {',
     'pipelined_oversized_head_during_body_writes_node_431_only',
     '#1232 a pipelined oversized head during a body answers with only the Node 431'),
    ('body-bytes-counted', source,
     '        for &byte in bytes {\n'
     '            if !self.active {\n'
     '                return false;\n'
     '            }\n'
     '            if matches!(self.body, BodyFraming::None) {\n'
     '                if self.push_head(byte) {\n'
     '                    return true;\n'
     '                }\n'
     '            } else {\n'
     '                self.push_body(byte);\n'
     '            }\n'
     '        }',
     '        for &byte in bytes {\n'
     '            if !self.active {\n'
     '                return false;\n'
     '            }\n'
     '            if true {\n'
     '                if self.push_head(byte) {\n'
     '                    return true;\n'
     '                }\n'
     '            } else {\n'
     '                self.push_body(byte);\n'
     '            }\n'
     '        }',
     'large_bodies_do_not_trip_the_header_guard',
     '#1232 a 40 KB body must be served, not counted against the header budget'),
    ('chunked-body-guard-blind', source,
     '        if self.transfer_chunked {\n'
     '            self.body = BodyFraming::ChunkSize {',
     '        if self.transfer_chunked {\n'
     '            self.active = false;\n'
     '            self.body = BodyFraming::ChunkSize {',
     'chunked_pipelined_oversized_head_writes_node_431_only',
     '#1232 a pipelined oversized head behind a chunked body answers with only the Node 431'),
    # fix-r1 P1-2: the false 431 can land before either response, so the
    # mutant binds both of the test's own assertions.
    ('per-message-budget-not-reset', source,
     '    fn begin_next_head(&mut self) {\n'
     '        self.body = BodyFraming::None;\n'
     '        self.line = GuardLine::RequestLine;\n'
     '        self.counted_bytes = 0;\n'
     '        self.raw_bytes = 0;',
     '    fn begin_next_head(&mut self) {\n'
     '        self.body = BodyFraming::None;\n'
     '        self.line = GuardLine::RequestLine;\n'
     '        self.raw_bytes = 0;',
     'pipelined_head_budget_resets_per_message',
     ['#1232 the first ~10 K head plus its 20 K body must be served, not 431',
      '#1232 the second head must start from a fresh per-message budget, not 431']),
    ('content-length-capture-broken', source,
     '            NameMatch::Full(FramingField::ContentLength) => FramingValue::ContentLength {',
     '            NameMatch::Full(FramingField::ContentLength) if false => FramingValue::ContentLength {',
     'pipelined_head_budget_resets_per_message',
     '#1232 the first ~10 K head plus its 20 K body must be served, not 431'),
    ('read-ahead-stops-at-room', source,
     '            } else if this.ahead_eof {\n'
     '                // EOF is latched and the caller has no room for the waiting\n'
     '                // bytes; the next poll delivers them and then the EOF.\n'
     '                break;\n'
     '            }',
     '            } else if this.ahead_eof {\n'
     '                // EOF is latched and the caller has no room for the waiting\n'
     '                // bytes; the next poll delivers them and then the EOF.\n'
     '                break;\n'
     '            } else {\n'
     '                break;\n'
     '            }',
     'pipelined_oversized_head_single_burst_writes_node_431_only',
     '#1232 a single-burst pipelined oversized head answers with only the Node 431'),
    # fix-r1: a guard that keeps scanning once it stepped aside reverts the
    # never-re-arm half of P1-1 — the bytes behind an untrackable head (here a
    # 48 KB chunk behind a multi-token Transfer-Encoding) get counted again.
    ('step-aside-stops-scanning', source,
     '        for &byte in bytes {\n'
     '            if !self.active {\n'
     '                return false;\n'
     '            }\n',
     '        for &byte in bytes {\n',
     'keep_alive_multi_token_te_large_chunked_body_is_served',
     '#1232 a keep-alive multi-token Transfer-Encoding request with a large chunk must be served, not 431'),
    # fix-r1 (P2-4): without the zero-room park, the 0-capacity read's empty
    # `filled` is latched as the peer's EOF while bytes are still in flight.
    ('zero-read-latches-eof', source,
     '            if limit == 0 {',
     '            if limit == 0 && false {',
     'zero_room_poll_does_not_latch_a_fake_eof',
     '#1232 a zero-capacity read is not the peer\'s EOF: the waiting read-ahead bytes must stay delivered, not fake a 400'),
    # fix-r1 (opus-4): an empty `Content-Length:` value read as zero lets the
    # trailing bytes be counted as the next head instead of stepping aside.
    ('empty-content-length-is-zero', source,
     '                if invalid || !digits || self.content_length.is_some() {',
     '                if invalid || self.content_length.is_some() {',
     'header_guard_steps_aside_on_empty_content_length',
     '#1232 the bytes behind an empty Content-Length value are not counted as head bytes'),
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
