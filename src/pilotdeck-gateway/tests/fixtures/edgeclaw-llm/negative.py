#!/usr/bin/env python3
"""#763 source mutations. Run only after the new source is committed.

Every anchor must occur exactly once, otherwise exit 2 (never SURVIVED).
CARGO_TARGET_DIR must be provided; no alternate source tree or target is made.
No compiler error counts as KILLED. Each mutant has a restored positive run.
Use --check-anchors without mutation, or OUT_DIR [NAME ...] after commit.
"""
import os
import re
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
source = root / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/llm/transport.rs'
cases = [
    ('key-surrogate', 'header_value(&value, "set")', 'header_value(&text(&value).into(), "set")', 'wire_matrix'),
    ('google-surrogate', '&selection.model.to_utf8().map_err(|_| LlmError {', '&Ok::<String, std::string::FromUtf16Error>(text(&selection.model)).map_err(|_| LlmError {', 'wire_matrix'),
    ('header-deadline-order', 'wait_deadline(deadline),', 'wait_deadline(None),', 'timer_order_matrix'),
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
    ('body-deadline', '            response.1,\n            input,', '            None,\n            input,', 'body_deadline_matrix'),
    ('response-text', 'return Ok(value);', 'return Ok(trimmed(&value));', 'wire_matrix'),
    ('content-empties', 'key == "content"', 'key == "content" || key == "output"', 'wire_matrix'),
    ('max-tokens', 'JsValue::Number(32768.0)', 'JsValue::Number(32767.0)', 'wire_matrix'),
    ('debug-raw', '("rawResponse", string(&raw))', '("rawResponse", string(&JsString::default()))', 'wire_matrix'),
    ('debug-timeout', 'error.timeout()\n                            || text(&error.message).to_lowercase().contains("timed out")', 'error.timeout()', 'timeout_matrix'),
    ('url-version', 's == "v1" || s == "v1beta"', 's == "v1beta"', 'wire_matrix'),
    ('transport-default', 'transport_timeout: Duration::from_secs(300)', 'transport_timeout: Duration::from_secs(600)', 'headers_transport_deadline_matrix'),
    ('transport-override', 'self.transport_timeout = timeout;', 'self.transport_timeout = timeout / 2;', 'headers_transport_deadline_matrix'),
    ('headers-no-bound', 'Some(Instant::now() + self.transport_timeout),', 'None,', 'headers_transport_deadline_matrix'),
    ('headers-error-shape', 'Exchange::Timeout { .. } => Err(type_error("fetch failed")),', 'Exchange::Timeout { .. } => Err(type_error("terminated")),', 'headers_transport_deadline_matrix'),
    ('headers-late-parent', 'wait_deadline(deadline),', 'wait_deadline(deadline.map(|(_, ms)| (Instant::now(), ms))),', 'headers_transport_deadline_matrix'),
    ('header-early-wire-check', "if bytes.iter().any(|b| matches!(b, 0 | b'\\r' | b'\\n')) {", 'if HeaderValue::from_bytes(bytes).is_err() {', 'header_construction_vs_send_matrix'),
    ('header-drop-cr-validation', "matches!(b, 0 | b'\\r' | b'\\n')", "matches!(b, 0 | b'\\n')", 'header_construction_vs_send_matrix'),
    ('header-wire-permitted', 'HeaderValue::from_bytes(value).map_err(|_| type_error("fetch failed"))?', 'HeaderValue::from_bytes(value).unwrap_or_else(|_| HeaderValue::from_static("mutant"))', 'header_construction_vs_send_matrix'),
    ('header-wire-error-shape', 'HeaderValue::from_bytes(value).map_err(|_| type_error("fetch failed"))?', 'HeaderValue::from_bytes(value).map_err(|_| type_error("bad header"))?', 'header_construction_vs_send_matrix'),
    ('header-skip-forced-mode', 'headers.insert("sec-fetch-mode", b"cors".to_vec());', 'header(&mut headers, "sec-fetch-mode", "cors".into())?;', 'header_construction_vs_send_matrix'),
    ('header-untrimmed-error', 'bytes.iter().map(|b| char::from(*b)).collect::<String>()', '"wrong header value".to_owned()', 'header_construction_vs_send_matrix'),
    ('header-conversion-order', '.own_entries()\n            .into_iter()\n            .map(|(name, value)| {', '.own_entries()\n            .into_iter()\n            .map(|(name, value)| {\n                HeaderName::from_bytes(&byte_string(name)?).map_err(|_| type_error("invalid name"))?;', 'header_construction_vs_send_matrix'),
    ('encoding-scheme', 'let secure = request.uri().scheme_str() == Some("https");', 'let secure = url.starts_with("https:");', 'encoding_matrix'),
    ('encoding-range', 'if headers.contains_key("range") {', 'if false && headers.contains_key("range") {', 'encoding_matrix'),
    ('encoding-explicit-range', 'bytes.extend(b", identity");', 'bytes.clear(); bytes.extend(b"identity");', 'encoding_matrix'),
    ('encoding-redirect', 'let secure = request.uri().scheme_str() == Some("https");', 'let secure = url::Url::parse(url).is_ok_and(|parsed| parsed.scheme() == "https");', 'encoding_matrix'),
    ('body-idle-limit', 'Some(idle),\n        deadline.map', 'Some(idle * 2),\n        deadline.map', 'body_idle_deadline_matrix'),
    ('body-config-wins-always', 'deadline.is_some_and(|at| idle.is_none_or(|idle| at <= now + idle))', 'deadline.is_some()', 'body_idle_deadline_matrix'),
    ('body-config-never-wins', 'deadline.is_some_and(|at| idle.is_none_or(|idle| at <= now + idle))', 'false', 'body_idle_deadline_matrix'),
    ('body-tie-idle-wins', 'at <= now + idle', 'at < now + idle', 'body_idle_deadline_matrix'),
    ('body-idle-not-reset', '    let mut too_long = false;\n    loop {\n        // Ledger: specs/gateway-parity/diffs/A6.4-NETWORK-TIMING-PARENT-TIE-001.md\n        let now = tokio::time::Instant::now();', '    let mut too_long = false;\n    let start = tokio::time::Instant::now();\n    loop {\n        // Ledger: specs/gateway-parity/diffs/A6.4-NETWORK-TIMING-PARENT-TIE-001.md\n        let now = start;', 'body_idle_resets_and_drop_cancels_predecessor'),
    ('headers-tie-transport-wins', 'match exchange_with_prearmed_parent(', 'match crate::runtime::network::exchange(', 'headers_transport_deadline_matrix'),
]

# A mutant is killed only by its named assertion in its exact test's panic block.
messages = {
    'key-surrogate': 'result key-surrogate-chat',
    'google-surrogate': 'result google-surrogate-model',
    'header-deadline-order': 'result timer-order-7',
    'responses-array-record': 'result extract-responses-2',
    'message-array-record': 'result extract-chat-2',
    'status-425': 'sleeps chat-attempt-0-event-3',
    'last-attempt': 'sleeps chat-attempt-0-event-1',
    'no-fallback': 'sleeps chat-attempt-0-event-1',
    'fallback-body': 'chat-attempt-0-event-1',
    'backoff': 'sleeps chat-attempt-0-event-1',
    'header-case': 'headers-chat',
    'header-combine': 'headers-chat',
    'header-bytes': 'result header-boundary-wide',
    'default-timeout': 'result chat-attempt-0-event-27',
    'disabled-timeout': 'disabled deadline remains pending',
    'body-deadline': 'D-2 bounded body',
    'response-text': 'result responses-attempt-0-event-0',
    'content-empties': 'result extract-responses-4',
    'max-tokens': 'protocol-anthropic',
    'debug-raw': 'debug chat-attempt-0-event-0',
    'debug-timeout': 'debug chat-attempt-0-event-27',
    'url-version': 'protocol-google',
    'transport-default': 'headers transport boundary',
    'transport-override': 'headers before boundary sends',
    'headers-no-bound': 'headers transport boundary',
    'headers-error-shape': 'headers result headers-responses-300000-0',
    'headers-late-parent': 'headers initial pending',
    'header-early-wire-check': 'header phase result header-control-responses',
    'header-drop-cr-validation': 'header phase result header-cr-responses',
    'header-wire-permitted': 'header phase result header-control-responses',
    'header-wire-error-shape': 'header phase result header-control-responses',
    'header-skip-forced-mode': 'header phase result header-overwritten-responses',
    'header-untrimmed-error': 'header phase result header-nul-responses',
    'header-conversion-order': 'header phase result header-conversion-responses',
    'encoding-scheme': 'encoding encoding-https-upper',
    'encoding-range': 'encoding encoding-range',
    'encoding-explicit-range': 'encoding encoding-range-explicit',
    'encoding-redirect': 'encoding encoding-redirect-upgrade',
    'body-idle-limit': 'body result body-responses-200-300000-400000',
    'body-config-wins-always': 'body result body-responses-200-300000-400000',
    'body-config-never-wins': 'body result body-responses-200-300000-200000',
    'body-tie-idle-wins': 'body result body-responses-200-300000-300000',
    'body-idle-not-reset': 'idle reset retains pending body',
    'headers-tie-transport-wins': 'headers result headers-responses-300000-300000',
}

reader = root / 'src/pilotdeck-gateway/src/fetch_redirect.rs'
case_sources = {name: reader for name in ('body-config-wins-always', 'body-config-never-wins', 'body-tie-idle-wins', 'body-idle-not-reset')}

def classify(completed, test, message):
    log = completed.stdout + completed.stderr
    if 'could not compile' in log or not re.search(r'^running 1 test$', log, re.M):
        return 'INVALID'
    if completed.returncode == 0:
        return 'PASS' if (re.search(r'^test ' + re.escape(test) + r' \.\.\. ok$', log, re.M)
                          and 'test result: ok. 1 passed; 0 failed;' in log) else 'INVALID'
    if completed.returncode != 101 or not re.search(r'^test ' + re.escape(test) + r' \.\.\. FAILED$', log, re.M):
        return 'INVALID'
    if 'test result: FAILED. 0 passed; 1 failed;' not in log:
        return 'INVALID'
    blocks = re.findall(r'^---- (.+) stdout ----\n(.*?)(?=^---- |^failures:|\Z)', log, re.M | re.S)
    if len(blocks) != 1 or blocks[0][0] != test:
        return 'INVALID'
    panic = blocks[0][1]
    threads = re.findall(r"^thread '([^']+)'(?: \(\d+\))? panicked at .+:\d+:\d+:$", panic, re.M)
    if threads != [test]:
        return 'INVALID'
    # Match the assertion line, never an expected string embedded in left/right data.
    return 'KILLED' if re.search(r'^(?:assertion .* failed: )?' + re.escape(message) + r'$', panic, re.M) else 'INVALID'

def anchors(text):
    stale = []
    for name, before, _, _ in cases:
        contents = case_sources[name].read_text() if name in case_sources else text
        if contents.count(before) != 1:
            stale.append((name, contents.count(before)))
    assert set(messages) == {case[0] for case in cases}, 'every mutant needs its own named assertion'
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

def run(name, test, message):
    slots = wait_for_slot(root / 'scripts/cargo-slots.sh')
    completed = run_owned(command + [test, '--', '--exact'], env=dict(os.environ), cwd=root, timeout=600)
    log = slots.stdout + slots.stderr + completed.stdout + completed.stderr
    with (out / f'{name}.log').open('x') as f: f.write(log)
    return classify(completed, test, message)

failed = []
# This guard intentionally rejects a new uncommitted source file. Do not bypass it.
with mutating_sources(root, [source, reader]) as originals:
    original = originals[source].decode()
    anchors(original)
    for name, before, after, test in cases:
        if selected and name not in selected: continue
        path = case_sources.get(name, source)
        original = originals[path].decode()
        assert run(name + '-before', test, messages[name]) == 'PASS', 'positive baseline failed'
        try:
            path.write_text(original.replace(before, after))
            outcome = run(name, test, messages[name])
        finally:
            path.write_bytes(originals[path])
        restored = run(name + '-restored', test, messages[name])
        print(f'{name}: {"SURVIVED" if outcome == "PASS" else outcome}; restored={restored}', flush=True)
        if outcome != 'KILLED' or restored != 'PASS': failed.append(name)
raise SystemExit(1 if failed else 0)
