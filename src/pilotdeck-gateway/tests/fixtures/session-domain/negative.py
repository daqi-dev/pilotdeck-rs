#!/usr/bin/env python3
# anchor-selfcheck: unsupported imperative nth-occurrence and sliced guard mutations
"""Bounded product-source mutation checks, using only the launcher's target.

The small rustc harness includes the real session_domain.rs (and its golden
unit test). Its dependencies are extracted verbatim from product files; no
validator, predicate, or expected result is reimplemented in this harness.
Full gateway Cargo tests independently verify the adapter/store path.
"""
import json
import os
from pathlib import Path
import signal
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

REPO = Path(__file__).resolve().parents[5]
SRC = REPO / 'src'
if not os.environ.get('CARGO_TARGET_DIR'):
    raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
TARGET = Path(os.environ['CARGO_TARGET_DIR']).resolve()
args = sys.argv[1:]
compile_only = '--compile-only' in args
args = [arg for arg in args if arg != '--compile-only']
OUT = Path(args[0]).resolve() if args else REPO / '_tmp/20261023-issue-222-domain-log/negative'
OUT.mkdir(parents=True, exist_ok=False)
selected = set(args[1:])
DOMAIN = SRC / 'pilotdeck-gateway/src/storage/session_domain.rs'
RECORDER = SRC / 'pilotdeck-gateway/src/storage/recorder.rs'
ADAPTER = SRC / 'pilotdeck-gateway/src/storage/transcript_adapter.rs'
checks = [
 ('queue-input', 'if !truthy(item)\n'),
 ('queue-reused-item', 'if contains(&next.used_items, item)'),
 ('queue-reused-turn', 'if next.turns.iter().any(|(v, _)| same_id(v, id)) ||'),
 ('queue-discard', 'if !truthy(item)\n', 1),
 ('durable-inbox-claim', 'if turn.closed || at.is_none_or'),
 ('duplicate-turn', 'if next.turns.iter().any(|(v, _)| same_id(v, id)) {'),
 ('queue-claim', 'if !truthy(claim) ||'),
 ('queue-fifo', 'if at != Some(0)'),
 ('closed-turn', 'if turn.closed {'),
 ('step-matching', 'if turn.open_step.is_none()'),
 ('step-increase', 'if !safe_integer(step) ||'),
 ('context-duplicate', 'if turn.context_recorded'),
 ('context-order', 'if turn.model_requested {', 0),
 ('request-duplicate', 'if turn.model_requested {', 1),
 ('permission-request', 'if !turn.model_requested {', 0),
 ('tool-request', 'if !turn.model_requested {', 1),
 ('tool-duplicate', 'if contains(&turn.open_tool_calls, call)'),
 ('open-tool-on-step-close', 'if text_is(field(entry, "outcome"), "completed")'),
 ('lifecycle-metadata', 'if invalid {'),
 ('lifecycle-duplicate', 'if contains(ids, operation)'),
 ('inbox-empty-item', 'if !truthy(item) {'),
 ('inbox-insert', 'if !truthy(field(entry, "message"))'),
 ('inbox-cancel', 'if current == Some("claimed")'),
 ('inbox-claim', 'if current != Some("pending")'),
 ('inbox-discard', 'if !matches!(current, Some("pending" | "claimed"))'),
 ('plan-value', 'if !matches!(plan, JsValue::Null)'),
 ('plan-progress', 'if !trimmed(tool)'),
 ('todos-metadata', 'if !(text_is(field(entry, "mode"), "markdown")'),
 ('todo-item', 'if !nonblank(id)', 0),
 ('goal-revision-integer', 'if !safe_integer(number)'),
 ('goal-revision-sequence', 'if number != Some(*revision + 1.0)'),
 ('goal-snapshot', 'if !nonblank(id)', 1),
 ('goal-blocked-reason', 'if text_is(field(goal, "phase"), "blocked")'),
 ('turn-pending-lifecycles', 'if !ids.is_empty()'),
 ('turn-pending-inbox', 'if turn\n'),
]

replacements = [
  ('null-entry-property', 'let kind = call_id(entry, "type")?', 'let kind = field(entry, "type")'),
  ('discard-undefined-lookup', '|| !same_id(queued_turn, id)\n', '|| pending.is_none_or(|at| !same_id(&next.queue[at].1, id))\n'),
  ('claim-undefined-lookup', 'if !truthy(claim) || !same_id(queued_turn, id)', 'if !truthy(claim) || at.is_none_or(|at| !same_id(&next.queue[at].1, id))'),
  ('instructions-order', '"agent_instructions" if turn.model_requested', '"agent_instructions" if false && turn.model_requested'),
  ('model-event-order', '"model_stream_event" if !turn.model_requested', '"model_stream_event" if false && !turn.model_requested'),
  ('step-already-open', 'if let Some(open) = turn.open_step', 'if let Some(open) = None::<u64>'),
  ('turn-open-step', 'if let Some(step) = turn.open_step', 'if let Some(step) = None::<u64>'),
  ('lifecycle-terminal', '} else if !remove(ids, operation) {', '} else if false && !remove(ids, operation) {'),
  ('tool-result-pair', 'if !remove(&mut turn.open_tool_calls, call)', 'if false && !remove(&mut turn.open_tool_calls, call)'),
  ('queue-must-claim', '} else if let Some(at) = queued {', '} else if let Some(at) = None::<usize> {'),
  ('context-state', 'turn.context_recorded = true;', 'turn.context_recorded = false;'),
  ('request-state', 'turn.model_requested = true;', 'turn.model_requested = false;'),
  ('closed-state', 'turn.closed = true;', 'turn.closed = false;'),
  ('goal-state', '*revision = number.unwrap();', '*revision = 0.0;'),
]

definitions = [(spec[0], ('guard', spec)) for spec in checks]
definitions += [(name, ('replace', (before, after))) for name, before, after in replacements]
unknown = selected - {name for name, _ in definitions}
if unknown:
    raise SystemExit(f'unknown mutations: {sorted(unknown)}')
chosen = [(name, action) for name, action in definitions if not selected or name in selected]
results = []
try:
    with mutating_sources(REPO, [DOMAIN]) as snapshots:
        original = snapshots[DOMAIN].decode()

        def stop(signum, frame):
            raise SystemExit(f'interrupted by signal {signum}')

        signal.signal(signal.SIGTERM, stop)
        signal.signal(signal.SIGINT, stop)

        def bounded(args, name, seconds=90):
            slot = wait_for_slot(REPO / 'scripts/cargo-slots.sh', timeout=600)
            with (OUT / f'{name}.slots.log').open('x') as log:
                log.write(slot.stdout + slot.stderr)
            try:
                result = run_owned(args, cwd=SRC, env=os.environ.copy(), timeout=seconds)
            except subprocess.TimeoutExpired as error:
                raw = ''.join(value.decode(errors='replace') if isinstance(value, bytes) else value or ''
                              for value in (error.stdout, error.stderr))
                with (OUT / f'{name}.log').open('x') as log:
                    log.write(raw + f'\nTimeoutExpired: {error}\n')
                raise
            with (OUT / f'{name}.log').open('x') as log:
                log.write(result.stdout + result.stderr)
            return result.returncode

        def function(text, marker):
            at = text.index(marker)
            brace = text.index('{', at)
            depth = 1
            end = brace + 1
            while depth:
                depth += (text[end] == '{') - (text[end] == '}')
                end += 1
            return text[at:end]

        def harness():
            recorder = RECORDER.read_text()
            adapter = ADAPTER.read_text()
            ledger = (SRC / 'pilotdeck-gateway/src/storage/ledger.rs').read_text()
            metadata = (SRC / 'pilotdeck-gateway/src/metadata.rs').read_text()
            js = (SRC / 'pilotdeck-gateway/src/js_value.rs').read_text().split('#[cfg(test)]')[0]
            # include! would interpret leading //! as inner docs in the wrong context.
            js = '\n'.join(line for line in js.splitlines() if not line.startswith('//!'))
            enum = recorder[recorder.index('#[derive(Debug)]'):recorder.index('impl fmt::Display')]
            helper_start = recorder.index('pub(super) fn call_id')
            helper_end = recorder.index('/// `stripRawFields`')
            helpers = recorder[helper_start:helper_end]
            types_start = adapter.index('pub(super) const DOMAIN_TYPES')
            types_end = adapter.index('];', types_start) + 2
            code = 'mod metadata {\n' + function(metadata, 'fn js_space') + '\n' + function(metadata, 'pub(crate) fn js_trim(') + '\n}\n'
            code += 'mod js_value {\n' + js + '\n}\n'
            baseline = SRC / 'pilotdeck-gateway/tests/common/baseline.rs'
            code += '#[path = ' + repr(str(baseline)).replace("'", '"') + '] mod test_baseline;\n'
            code += 'mod storage {\ntype LedgerError = std::io::Error;\n'
            code += 'mod ledger {use pilotdeck_protocol::JsValue;\n' + function(ledger, 'pub(super) fn field') + '\n}\n'
            code += 'mod recorder {use pilotdeck_protocol::JsValue; use super::ledger::field;\n'
            code += 'pub type RecorderResult<T> = Result<T, RecorderError>;\n' + enum
            code += function(recorder, 'pub(super) fn domain(') + '\n' + helpers + '\n}\n'
            code += 'mod transcript_adapter {\n' + adapter[types_start:types_end] + '\n' + function(adapter, 'pub(super) fn js_trim_units') + '\n}\n'
            code += '#[path = ' + repr(str(DOMAIN)).replace("'", '"') + '] mod session_domain;\n}\n'
            path = OUT / 'harness.rs'
            path.write_text(code)
            deps = TARGET / 'debug/deps'
            externs = []
            for crate in ('pilotdeck_protocol', 'sha2', 'uuid', 'serde_json'):
                candidates = sorted(deps.glob(f'lib{crate}-*.rlib'), key=lambda path: path.stat().st_mtime_ns)
                assert candidates, f'build {crate} in the launcher target first'
                externs += ['--extern', f'{crate}={candidates[-1]}']
            os.environ['CARGO_MANIFEST_DIR'] = str(SRC / 'pilotdeck-gateway')
            return ['rustc', '--edition=2024', '--test', str(path), *externs, '-L', f'dependency={deps}', '-A', 'dead_code', '-A', 'unused_variables', '-o', str(TARGET / 'pdrs222-negative')]

        def run(name, red):
            assert bounded(harness(), name + '-build') == 0, f'{name}: compile failure is not a killed mutant'
            if red and compile_only:
                return
            code = bounded([str(TARGET / 'pdrs222-negative'), '--exact', 'storage::session_domain::tests::real_ts_domain_logs_and_events_match_bytes', '--nocapture'], name)
            text = (OUT / f'{name}.log').read_text()
            assert 'running 1 test' in text
            assert ('test result: FAILED' in text and code != 0) if red else ('test result: ok. 1 passed' in text and code == 0), name
            if red:
                assert 'test storage::session_domain::tests::real_ts_domain_logs_and_events_match_bytes ... FAILED' in text, f'{name}: missing named failure'
                assert '#743 domain transition outcome' in text or '#743 domain log outcome' in text, f'{name}: unrelated failure'
            print(f'{name}: {"KILLED" if red else "PASS"}', flush=True)

        def guard(marker, occurrence=0):
            start = -1
            for _ in range(occurrence+1):
                start = original.index(marker, start+1)
            brace = original.index('{', start)
            condition = original[start+3:brace].strip()
            assert not condition.startswith('let '), marker
            return original[:start] + 'if false && (' + condition + ') ' + original[brace:]

        baseline_error = None
        try:
            run('baseline', False)
        except Exception as error:
            baseline_error = f'baseline: {type(error).__name__}: {error}'
        for name, (kind, spec) in chosen:
            status, reason = 'INVALID', baseline_error or ''
            try:
                if baseline_error:
                    raise RuntimeError(baseline_error)
                if kind == 'guard':
                    _, marker, *rest = spec
                    changed = guard(marker, rest[0] if rest else 0)
                else:
                    before, after = spec
                    assert original.count(before) == 1, (name, original.count(before))
                    changed = original.replace(before, after, 1)
                DOMAIN.write_text(changed)
                run(name, True)
                status = 'COMPILED' if compile_only else 'KILLED'
                reason = 'mutant compiled' if compile_only else 'bound domain assertion failed'
            except Exception as error:
                reason = f'{type(error).__name__}: {error}'
                if (OUT / f'{name}.log').exists() and 'test result: ok. 1 passed' in (OUT / f'{name}.log').read_text():
                    status = 'SURVIVED'
            finally:
                DOMAIN.write_bytes(snapshots[DOMAIN])
            if not baseline_error:
                try:
                    run(name + '-restored', False)
                except Exception as error:
                    status, reason = 'INVALID', reason + f'; restored: {error}'
            results.append(dict(mutation=name, status=status, reason=reason))
            print(f'{name}: {status}: {reason}', flush=True)
except Exception as error:
    reason = f'source guard/restoration: {type(error).__name__}: {error}'
    results = [dict(mutation=name, status='INVALID', reason=reason) for name, _ in chosen]
counts = {state: sum(row['status'] == state for row in results)
          for state in ('KILLED', 'SURVIVED', 'INVALID', 'COMPILED')}
with (OUT / 'summary.json').open('x') as log:
    json.dump(dict(controls=results, counts=counts), log, indent=2)
    log.write('\n')
print(json.dumps(counts), flush=True)
raise SystemExit(int(not results or any(row['status'] != ('COMPILED' if compile_only else 'KILLED') for row in results)))
