#!/usr/bin/env python3
"""Bounded product-source mutation checks, using only the launcher's target.

The small rustc harness includes the real session_domain.rs (and its golden
unit test). Its dependencies are extracted verbatim from product files; no
validator, predicate, or expected result is reimplemented in this harness.
Full gateway Cargo tests independently verify the adapter/store path.
"""
import os
from pathlib import Path
import signal
import subprocess
import sys

REPO = Path(__file__).resolve().parents[5]
SRC = REPO / 'src'
TARGET = Path(os.environ['CARGO_TARGET_DIR']).resolve()
assert TARGET == SRC / 'target', f'wrong build directory: {TARGET}'
OUT = REPO / '_tmp/20261023-issue-222-domain-log/negative'
OUT.mkdir(parents=True, exist_ok=True)
DOMAIN = SRC / 'pilotdeck-gateway/src/storage/session_domain.rs'
RECORDER = SRC / 'pilotdeck-gateway/src/storage/recorder.rs'
ADAPTER = SRC / 'pilotdeck-gateway/src/storage/transcript_adapter.rs'
original = DOMAIN.read_text()

def stop(signum, frame):
    raise SystemExit(f'interrupted by signal {signum}')

signal.signal(signal.SIGTERM, stop)
signal.signal(signal.SIGINT, stop)

def bounded(args, name, seconds=90):
    with (OUT / f'{name}.log').open('w') as log:
        child = subprocess.Popen(args, cwd=SRC, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
        try:
            return child.wait(timeout=seconds)
        except BaseException:
            os.killpg(child.pid, signal.SIGKILL)
            child.wait()
            raise

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
    for crate in ('pilotdeck_protocol', 'sha2', 'uuid'):
        candidates = sorted(deps.glob(f'lib{crate}-*.rlib'), key=lambda path: path.stat().st_mtime_ns)
        assert candidates, f'build {crate} in the launcher target first'
        externs += ['--extern', f'{crate}={candidates[-1]}']
    os.environ['CARGO_MANIFEST_DIR'] = str(SRC / 'pilotdeck-gateway')
    return ['rustc', '--edition=2024', '--test', str(path), *externs, '-L', f'dependency={deps}', '-A', 'dead_code', '-A', 'unused_variables', '-o', str(TARGET / 'pdrs222-negative')]

def run(name, red):
    assert bounded(harness(), name + '-build') == 0, f'{name}: compile failure is not a killed mutant'
    code = bounded([str(TARGET / 'pdrs222-negative'), '--exact', 'storage::session_domain::tests::real_ts_domain_logs_and_events_match_bytes', '--nocapture'], name)
    text = (OUT / f'{name}.log').read_text()
    assert 'running 1 test' in text
    assert ('test result: FAILED' in text and code != 0) if red else ('test result: ok. 1 passed' in text and code == 0), name
    print(f'{name}: {"KILLED" if red else "PASS"}', flush=True)

def guard(marker, occurrence=0):
    start = -1
    for _ in range(occurrence+1):
        start = original.index(marker, start+1)
    brace = original.index('{', start)
    condition = original[start+3:brace].strip()
    assert not condition.startswith('let '), marker
    return original[:start] + 'if false && (' + condition + ') ' + original[brace:]

# Every new state transition/validation family has an adversarial mutation.
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

try:
    run('baseline', False)
    for spec in checks:
        name, marker, *rest = spec
        DOMAIN.write_text(guard(marker, rest[0] if rest else 0))
        run(name, True)
        DOMAIN.write_text(original)
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
    for name, before, after in replacements:
        assert original.count(before) == 1, (name, original.count(before))
        DOMAIN.write_text(original.replace(before, after))
        run(name, True)
        DOMAIN.write_text(original)
    run('restored', False)
    print(f'PASS: {len(checks)+len(replacements)} / {len(checks)+len(replacements)} domain mutants killed', flush=True)
finally:
    DOMAIN.write_text(original)
