#!/usr/bin/env python3
"""#763 source mutations. Run only after the new source is committed.

Every anchor must occur exactly once, otherwise exit 2 (never SURVIVED).
CARGO_TARGET_DIR must be provided; no alternate source tree or target is made.
No compiler error counts as KILLED. Each mutant has a restored positive run.
Use --check-anchors without mutation, or OUT_DIR [NAME ...] after commit.
"""
import os
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
source = root / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/llm/transport.rs'
cases = [
    ('key-surrogate', 'header_value(&value, "set")', 'header_value(&text(&value).into(), "set")', 'wire_matrix'),
    ('google-surrogate', '&selection.model.to_utf8().map_err(|_| LlmError {', '&Ok::<String, std::string::FromUtf16Error>(text(&selection.model)).map_err(|_| LlmError {', 'wire_matrix'),
    ('header-deadline-order', '                            deadline.map(|(at, _)| at),\n                        )', '                            None,\n                        )', 'timer_order_matrix'),
    ('responses-array-record', '!is_record(payload)', 'payload.as_object().is_none()', 'wire_matrix'),
    ('message-array-record', '.filter(|v| is_record(v))', '.filter(|v| v.as_object().is_some())', 'wire_matrix'),
    ('status-425', '408 | 409 | 425 | 429 | 500 | 502 | 503 | 504', '408 | 409 | 429 | 500 | 502 | 503 | 504', 'wire_matrix'),
    ('last-attempt', 'attempt == 2', 'attempt == 1', 'wire_matrix'),
    ('no-fallback', 'Err(_) if body.get("response_format").is_some() => {', 'Err(_) if false && body.get("response_format").is_some() => {', 'wire_matrix'),
    ('fallback-body', 'if !key.equals_str("response_format") {', 'if true {', 'wire_matrix'),
    ('backoff', 'self.runtime.sleep(1000 * (1 << attempt)).await', 'self.runtime.sleep(1001 * (1 << attempt)).await', 'wire_matrix'),
    ('header-case', 'map.contains_key(name)', 'map.contains_key("mutant-header")', 'wire_matrix'),
    ('header-combine', 'joined.extend(b", ");', 'joined.extend(b";");', 'wire_matrix'),
    ('header-bytes', 'u8::try_from(unit)', 'u8::try_from(unit.min(255))', 'wire_matrix'),
    ('default-timeout', '.unwrap_or(30_000.0)', '.unwrap_or(30_001.0)', 'timeout_matrix'),
    ('disabled-timeout', '(timeout > 0.0).then(', '(timeout >= 0.0).then(', 'pending_and_cancellation_matrix'),
    ('body-deadline', 'read_body(response.0.into_body(), response.1, input)', 'read_body(response.0.into_body(), None, input)', 'body_deadline_matrix'),
    ('response-text', 'return Ok(value);', 'return Ok(trimmed(&value));', 'wire_matrix'),
    ('content-empties', 'key == "content"', 'key == "content" || key == "output"', 'wire_matrix'),
    ('max-tokens', 'JsValue::Number(32768.0)', 'JsValue::Number(32767.0)', 'wire_matrix'),
    ('debug-raw', '("rawResponse", string(&raw))', '("rawResponse", string(&JsString::default()))', 'wire_matrix'),
    ('debug-timeout', 'error.timeout()\n                            || text(&error.message).to_lowercase().contains("timed out")', 'error.timeout()', 'timeout_matrix'),
    ('url-version', 's == "v1" || s == "v1beta"', 's == "v1beta"', 'wire_matrix'),
]

def anchors(text):
    stale = [(name, text.count(before)) for name, before, _, _ in cases if text.count(before) != 1]
    if stale:
        for name, count in stale: print(f'STALE ANCHOR {name}: {count} matches', file=sys.stderr)
        raise SystemExit(2)

anchors(source.read_text())
if sys.argv[1:] == ['--check-anchors']:
    print(f'{len(cases)} unique anchors; no mutation')
    raise SystemExit(0)
assert os.environ.get('CARGO_TARGET_DIR'), 'CARGO_TARGET_DIR must be supplied by caller'
assert len(sys.argv) >= 2, 'usage: negative.py OUT_DIR [NAME ...] | --check-anchors'
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=True)
selected = set(sys.argv[2:])
assert selected <= {c[0] for c in cases}, 'unknown case'
command = ['cargo', 'test', '--manifest-path', str(root / 'src/Cargo.toml'), '-p', 'pilotdeck-gateway', '--test', 'edgeclaw_llm_transport']

def run(name, test):
    slots = wait_for_slot(root / 'scripts/cargo-slots.sh')
    completed = run_owned(command + [test, '--', '--exact'], env=dict(os.environ), cwd=root, timeout=600)
    log = slots.stdout + slots.stderr + completed.stdout + completed.stderr
    with (out / f'{name}.log').open('x') as f: f.write(log)
    if 'could not compile' in log or 'running 1 test' not in log: return 'INVALID'
    if completed.returncode == 0 and 'test result: ok.' in log: return 'PASS'
    if completed.returncode != 0 and 'test result: FAILED.' in log: return 'KILLED'
    return 'INVALID'

failed = []
# This guard intentionally rejects a new uncommitted source file. Do not bypass it.
with mutating_sources(root, [source]) as originals:
    original = originals[source].decode()
    anchors(original)
    for name, before, after, test in cases:
        if selected and name not in selected: continue
        assert run(name + '-before', test) == 'PASS', 'positive baseline failed'
        try:
            source.write_text(original.replace(before, after))
            outcome = run(name, test)
        finally:
            source.write_bytes(originals[source])
        restored = run(name + '-restored', test)
        print(f'{name}: {"SURVIVED" if outcome == "PASS" else outcome}; restored={restored}', flush=True)
        if outcome != 'KILLED' or restored != 'PASS': failed.append(name)
raise SystemExit(1 if failed else 0)
