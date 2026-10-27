#!/usr/bin/env python3
"""#867: commit first, then OUT [NAME ...]; --check-anchors is read-only.

Use the caller's CARGO_TARGET_DIR. Each mutant runs its exact regression test;
compile errors, unrelated panics, timeout and stale anchors are INVALID, never
KILLED. Run every selected case and summarize only at the end. Long full runs
belong on the coordinator's host, not in a background worker session.
"""
import json
import os
from pathlib import Path
import re
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
source = root / 'src/pilotdeck-gateway/src/runtime/model/http/encoding.rs'
http = root / 'src/pilotdeck-gateway/src/runtime/model/http.rs'
CASES = [
    ('skip-decoding', source, '.all(|s| matches!(s.as_str(), "gzip" | "x-gzip" | "deflate" | "br"))', '.all(|_| false)', 'decoded_wire_matches_node'),
    ('gzip-codec', source, '_ => Kind::Gzip,', '_ => Kind::Deflate,', 'decoded_wire_matches_node'),
    ('deflate-sniff', source, 'first & 0x0f == 8', 'first & 0x0f != 8', 'decoded_wire_matches_node'),
    ('brotli-codec', source, 'Kind::Brotli => Decoder::Brotli(BrotliDecoder::new(input)),', 'Kind::Brotli => Decoder::Deflate(DeflateDecoder::new(input)),', 'decoded_wire_matches_node'),
    ('stack-order', source, 'encodings.iter().rev()', 'encodings.iter()', 'decoded_wire_matches_node'),
    ('unknown-chain', source, '.all(|s| matches!(s.as_str(), "gzip" | "x-gzip" | "deflate" | "br"))', '.any(|s| matches!(s.as_str(), "gzip" | "x-gzip" | "deflate" | "br"))', 'decoded_wire_matches_node'),
    ('gzip-alias', source, '"gzip" | "x-gzip" | "deflate" | "br"', '"gzip" | "deflate" | "br"', 'decoded_wire_matches_node'),
    ('encoding-case', source, 's.trim().to_ascii_lowercase()', 's.trim().to_owned()', 'decoded_wire_matches_node'),
    ('gzip-members', source, 'decoder.multiple_members(true);', 'decoder.multiple_members(false);', 'decoded_wire_matches_node'),
    ('truncated-tail', source, 'error.kind() == io::ErrorKind::UnexpectedEof', 'false && error.kind() == io::ErrorKind::UnexpectedEof', 'decoded_wire_matches_node'),
    ('empty-body', source, 'return Poll::Ready(Ok(()));\n            };\n            let input', 'return Poll::Ready(Err(io::Error::other("#867 mutant empty")));\n            };\n            let input', 'decoded_wire_matches_node'),
    ('swallow-decode-error', source, 'Poll::Ready(Some(Err(BodyFailure::Terminated)))', 'Poll::Ready(None)', 'decoded_wire_matches_node'),
    ('header-utf8', http, 'hyper::header::HeaderValue::from_bytes(bytes).map_err(|_| failed())?', 'hyper::header::HeaderValue::from_bytes(bytes.iter().map(|b| char::from(*b)).collect::<String>().as_bytes()).map_err(|_| failed())?', 'header_byte_string_matches_node'),
    ('header-wide', http, 'u8::try_from(unit).map_err', 'u8::try_from(unit.min(255)).map_err', 'header_byte_string_matches_node'),
    ('header-surrogate-loss', http, 'header_byte_string(&units)?', 'header_byte_string(&String::from_utf16_lossy(&units).encode_utf16().collect::<Vec<_>>())?', 'header_byte_string_matches_node'),
    ('header-join', http, 'joined.extend_from_slice(b", ");', 'joined.extend_from_slice(b";");', 'header_byte_string_matches_node'),
    ('header-trim', http, 'let bytes = &value[start..end];', 'let bytes = &value[..];', 'header_byte_string_matches_node'),
    ('header-crlf-error', http, 'if bytes.iter().any(|b| matches!(b, 0 | b\'\\r\' | b\'\\n\')) {', 'if false && bytes.iter().any(|b| matches!(b, 0 | b\'\\r\' | b\'\\n\')) {', 'header_byte_string_matches_node'),
    ('entries-decode', source, '.all(|s| matches!(s.as_str(), "gzip" | "x-gzip" | "deflate" | "br"))', '.all(|_| false)', 'both_provider_entries_decode_node_responses'),
    ('stream-decode', source, '.all(|s| matches!(s.as_str(), "gzip" | "x-gzip" | "deflate" | "br"))', '.all(|_| false)', 'compressed_body_yields_and_cancels'),
    ('flate-truncation', source, 'error.to_string() == "unexpected BufError"', 'error.to_string() == "never ignored"', 'decoded_wire_matches_node'),
    ('http-eof-drain', source, '        loop {\n            let input = decoder.input();', '        if this.done { return Poll::Ready(Ok(())); }\n        loop {\n            let input = decoder.input();', 'compression_eof_waits_for_http_eof'),
    ('wire-progress', source, 'Poll::Pending if this.progress.swap(false, Ordering::Relaxed)', 'Poll::Pending if false && this.progress.swap(false, Ordering::Relaxed)', 'compressed_wire_progress_preserves_both_idle_clocks'),
    ('application-idle-reset', http, 'idle_deadline.saturating_duration_since(tokio::time::Instant::now())', 'duration(idle_ms)', 'compressed_wire_progress_preserves_both_idle_clocks'),
    ('null-status', source, '101 | 204 | 205 | 304', '101 | 204 | 304', 'decoded_wire_matches_node'),

    ('wire-error', source, 'Some(Err(error)) => return Poll::Ready(Err(io::Error::other(error))),', 'Some(Err(error)) => { let _ = error; return Poll::Ready(Ok(&[])); },', 'interrupted_compressed_wire_is_not_a_truncated_representation'),
    ('header-name-error', http, 'is an invalid header name.', 'is an invalid header.', 'header_byte_string_matches_node'),
    ('header-record-order', http, 'let units = crate::js_value::to_string_units(value).map_err(|_| failed())?;', 'hyper::header::HeaderName::from_bytes(&header_byte_string(name.units())?).map_err(|_| failed())?;\n            let units = crate::js_value::to_string_units(value).map_err(|_| failed())?;', 'header_byte_string_matches_node'),

]
MESSAGES = {
    'interrupted_compressed_wire_is_not_a_truncated_representation': ['#867 incomplete HTTP body stays an error'],
    'compression_eof_waits_for_http_eof': ['#867 compression EOF waits for HTTP EOF', '#867 wire error after compression EOF'],
    'compressed_wire_progress_preserves_both_idle_clocks': ['#867 wire activity keeps body idle alive', '#867 wire activity does not reset application idle'],
    'decoded_wire_matches_node': ['#867 raw response matches Node', '#867 raw response error matches Node'],
    'header_byte_string_matches_node': ['#867 header bytes match Node', '#867 header errors match Node'],
    'both_provider_entries_decode_node_responses': ['#867 both entries decode response'],
    'compressed_body_yields_and_cancels': ['#867 decoded prefix before EOF'],
}

def check_anchors():
    errors = []
    for name, path, before, _, _ in CASES:
        count = path.read_text().count(before)
        if count != 1:
            errors.append(f'{name}: anchor count={count}, expected=1')
    if errors:
        raise ValueError('; '.join(errors))


def cargo(test, env, out, label):
    slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=600)
    (out / f'{label}.slots.log').write_text(slot.stdout + slot.stderr)
    full = f'runtime::model::http::encoding_tests::{test}'
    result = run_owned(['cargo', 'test', '--offline', '--manifest-path', str(root / 'src/Cargo.toml'),
                        '-p', 'pilotdeck-gateway', '--lib', full, '--', '--exact'],
                       cwd=root, env=env, timeout=600)
    log = result.stdout + result.stderr
    (out / f'{label}.log').write_text(log)
    return result, log, full


def main():
    try:
        check_anchors()
    except Exception as error:
        print(f'INVALID: {error}', file=sys.stderr)
        return 2
    if sys.argv[1:] == ['--check-anchors']:
        print(f'#867 {len(CASES)} anchors valid')
        return 0
    if len(sys.argv) < 2 or not os.environ.get('CARGO_TARGET_DIR'):
        print('OUT and caller-provided CARGO_TARGET_DIR required', file=sys.stderr)
        return 2
    selected = sys.argv[2:]
    cases = [case for case in CASES if not selected or case[0] in selected]
    if selected and (len(set(selected)) != len(selected) or {c[0] for c in cases} != set(selected)):
        print('unknown or duplicate mutation name', file=sys.stderr)
        return 2
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    env = os.environ.copy()
    summary = []
    for name, path, before, after, test in cases:
        row = dict(name=name, status='INVALID', reason='not run')
        try:
            with mutating_sources(root, [path]) as snapshots:
                result, log, full = cargo(test, env, out, f'{name}-positive')
                if result.returncode != 0 or '1 passed; 0 failed' not in log:
                    raise RuntimeError('pristine exact test failed or did not run')
                original = snapshots[path].decode()
                if original.count(before) != 1:
                    raise ValueError('stale committed anchor')
                path.write_text(original.replace(before, after, 1))
                result, log, full = cargo(test, env, out, f'{name}-mutant')
                block = re.search(r'---- ' + re.escape(full) + r' stdout ----\n(.*?)(?=\n---- |\nfailures:|\Z)', log, re.S)
                if result.returncode == 101 and f'{full} ... FAILED' in log and block and any(message in block[1] for message in MESSAGES[test]):
                    row.update(status='KILLED', reason='named regression assertion')
                elif result.returncode == 0 and '1 passed; 0 failed' in log:
                    row.update(status='SURVIVED', reason='exact regression still passed')
                else:
                    row.update(reason='compile, timeout, signal, or unrelated assertion')
            restored, log, _ = cargo(test, env, out, f'{name}-restored')
            if restored.returncode != 0 or '1 passed; 0 failed' not in log:
                row.update(status='INVALID', reason='restored positive test failed')
        except Exception as error:
            row.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
        summary.append(row)
        print(f"{name}: {row['status']} — {row['reason']}", flush=True)
        (out / 'summary.json').write_text(json.dumps(summary, ensure_ascii=False, indent=2) + '\n')
    print(json.dumps({status: sum(r['status'] == status for r in summary) for status in ['KILLED', 'SURVIVED', 'INVALID']}))
    return 0 if all(row['status'] == 'KILLED' for row in summary) else 1

if __name__ == '__main__':
    sys.exit(main())
