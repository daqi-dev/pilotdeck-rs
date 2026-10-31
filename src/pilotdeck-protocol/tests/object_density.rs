//! #1128 object-density harness for the `JsObject.indices` boxing (#983).
//!
//! `object_density_harness_selfcheck` is the millisecond-scale correctness
//! guard for the workload builders and the pretty pipeline below; it runs in
//! the normal test suite. `object_density_bench` is the measurement itself:
//! it is `#[ignore]`d so no CI run ever judges performance (#1128 asks for a
//! measured comparison, not a gate) and is driven by environment variables,
//! normally through `tests/fixtures/jsobject-density/measure.py`.
//!
//! The pretty step mirrors the product pipeline: `stringify_json` emits the
//! compact bytes, then the reindentation rewrite of `pilotdeck-cli`'s
//! `write_json_pretty_into` (two-space indent, `": "`, empty containers stay
//! inline) turns them into the streamed pretty output. The copy is local on
//! purpose: the CLI's own single implementation (#983 fix-r11) stays private
//! to the product binary, and this file must not grow product copies that
//! drift unmutated — `negative_controls.py` anchors this copy the same way
//! `sdk-decode-budget` anchors the CLI's.
use std::time::Instant;

use pilotdeck_protocol::{JsValue, parse_json, stringify_json};

/// `{"a":1}` plus one separator is exactly 8 bytes: the small-object grid is
/// sized by element count, matching the issue's 10^6-element anchor at 8 MiB.
const SMALL_ELEMENT: &str = "{\"a\":1}";
/// Spine padding key of the deep grid; each level pads `"p"` to the target size.
const DEEP_PAD_KEY: &str = "p";
/// Default spine depth of the deep grid: the issue's 10^4-level anchor.
const DEEP_DEFAULT_DEPTH: usize = 10_000;
const WIDE_KEY_DIGITS: usize = 6;

/// `[{"a":1},{"a":1},…]`: `target_bytes` of array elements, one small object
/// per 8 bytes of wire text.
fn build_small_objects(target_bytes: usize) -> String {
    let count = target_bytes / (SMALL_ELEMENT.len() + 1);
    let mut text = String::with_capacity(count * (SMALL_ELEMENT.len() + 1) + 2);
    text.push('[');
    for index in 0..count {
        if index > 0 {
            text.push(',');
        }
        text.push_str(SMALL_ELEMENT);
    }
    text.push(']');
    text
}

/// One object with zero-padded `k%06d` keys: the HashMap-heavy wide grid. Key
/// width is fixed so the size math is exact, unlike a growth-heavy key set.
fn build_wide_object(target_bytes: usize) -> String {
    let mut text = String::new();
    text.push('{');
    let mut index = 0usize;
    while text.len() < target_bytes {
        if index > 0 {
            text.push(',');
        }
        text.push('"');
        text.push_str(&format!("k{index:0WIDE_KEY_DIGITS$}"));
        text.push_str("\":1");
        index += 1;
    }
    text.push('}');
    text
}

/// `{"a":{"a":…},"p":"…"}`: `depth` nested objects, each padded by `"p"` so
/// the spine reaches `target_bytes`. The innermost child is the number 1.
fn build_deep_spine(target_bytes: usize, depth: usize) -> String {
    let depth = depth.max(1);
    // Each level contributes `{"a":` + `,"p":"<pad>"}` = 13 + pad bytes.
    let pad = (target_bytes.saturating_sub(1) / depth).saturating_sub(13);
    let mut text = String::with_capacity(depth * (16 + pad) + 1);
    for _ in 0..depth {
        text.push_str("{\"a\":");
    }
    text.push('1');
    for _ in 0..depth {
        text.push_str(",\"");
        text.push_str(DEEP_PAD_KEY);
        text.push_str("\":\"");
        for _ in 0..pad {
            text.push('x');
        }
        text.push_str("\"}");
    }
    text
}

/// FNV-1a 64 over bytes: a cheap cross-rep stability digest. Byte-identical
/// outputs across reps and across the boxed/unboxed builds must agree; the
/// cross-implementation check against Node uses a full dump plus sha256 in
/// `measure.py` instead of this hash.
fn fnv64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// The reindentation rewrite of `pilotdeck-cli`'s `write_json_pretty_into`,
/// collected into a buffer instead of streamed: two-space indent per depth,
/// `": "` after colons, and `{}`/`[]` stay inline. Whitespace inside strings
/// is preserved byte for byte, so `stringify_json` + this rewrite is the
/// product's pretty byte stream.
fn reindent_pretty(raw: &str, out: &mut Vec<u8>) {
    fn newline(out: &mut Vec<u8>, depth: usize) {
        out.push(b'\n');
        for _ in 0..depth {
            out.extend_from_slice(b"  ");
        }
    }
    let mut chars = raw.chars().peekable();
    let (mut depth, mut quoted, mut escaped) = (0usize, false, false);
    let mut utf8 = [0_u8; 4];
    while let Some(value) = chars.next() {
        if quoted {
            out.extend_from_slice(value.encode_utf8(&mut utf8).as_bytes());
            if escaped {
                escaped = false;
            } else if value == '\\' {
                escaped = true;
            } else if value == '"' {
                quoted = false;
            }
            continue;
        }
        match value {
            '"' => {
                quoted = true;
                out.extend_from_slice(value.encode_utf8(&mut utf8).as_bytes());
            }
            '{' | '[' => {
                out.extend_from_slice(value.encode_utf8(&mut utf8).as_bytes());
                let close = if value == '{' { '}' } else { ']' };
                if chars.peek() == Some(&close) {
                    out.extend_from_slice(close.encode_utf8(&mut utf8).as_bytes());
                    chars.next();
                } else {
                    depth += 1;
                    newline(out, depth);
                }
            }
            '}' | ']' => {
                depth -= 1;
                newline(out, depth);
                out.extend_from_slice(value.encode_utf8(&mut utf8).as_bytes());
            }
            ',' => {
                out.extend_from_slice(value.encode_utf8(&mut utf8).as_bytes());
                newline(out, depth);
            }
            ':' => out.extend_from_slice(b": "),
            _ => out.extend_from_slice(value.encode_utf8(&mut utf8).as_bytes()),
        }
    }
}

/// One full pipeline pass: decode, compact encode, pretty reindent. Returns
/// the phase durations in microseconds plus the compact/pretty digests.
fn pipeline(text: &str) -> (f64, f64, f64, u64, usize, u64) {
    let started = Instant::now();
    let value = parse_json(text).expect("#1128 workload parses");
    let decoded = started.elapsed().as_secs_f64() * 1000.0;
    let started = Instant::now();
    let compact = stringify_json(&value)
        .expect("#1128 workload stringifies")
        .expect("#1128 workload is not undefined");
    let encoded = started.elapsed().as_secs_f64() * 1000.0;
    let started = Instant::now();
    let mut pretty = Vec::new();
    reindent_pretty(&compact, &mut pretty);
    let reindented = started.elapsed().as_secs_f64() * 1000.0;
    (
        decoded,
        encoded,
        reindented,
        fnv64(compact.as_bytes()),
        pretty.len(),
        fnv64(&pretty),
    )
}

fn env_usize(name: &str) -> Option<usize> {
    std::env::var(name).ok().map(|value| {
        value
            .parse::<usize>()
            .expect("#1128 numeric environment value")
    })
}

fn atomic_write(path: &str, bytes: &[u8], what: &str) {
    let temporary = std::path::PathBuf::from(path).with_extension("tmp");
    std::fs::write(&temporary, bytes)
        .unwrap_or_else(|error| panic!("#1128 temporary write of {what}: {error}"));
    std::fs::rename(&temporary, path)
        .unwrap_or_else(|error| panic!("#1128 atomic publish of {what}: {error}"));
}

/// Correctness guard for the harness: exact builder bytes, exact pretty
/// bytes, and the pinned FNV digests. Anchored by
/// `tests/fixtures/jsobject-density/negative_controls.py`.
#[test]
fn object_density_harness_selfcheck() {
    assert_eq!(
        build_small_objects(26),
        "[{\"a\":1},{\"a\":1},{\"a\":1}]",
        "#1128 small-object builder exact bytes"
    );
    assert_eq!(
        build_wide_object(24),
        "{\"k000000\":1,\"k000001\":1}",
        "#1128 wide-object builder exact bytes"
    );
    let deep = build_deep_spine(47, 2);
    assert_eq!(
        deep, "{\"a\":{\"a\":1,\"p\":\"xxxxxxxxxx\"},\"p\":\"xxxxxxxxxx\"}",
        "#1128 deep-spine builder exact bytes"
    );
    let value = parse_json(&deep).expect("#1128 deep spine parses");
    let compact = stringify_json(&value)
        .expect("#1128 deep spine stringifies")
        .expect("#1128 deep spine is not undefined");
    assert_eq!(compact, deep, "#1128 deep spine round-trips compact");
    let mut pretty = Vec::new();
    reindent_pretty(&compact, &mut pretty);
    assert_eq!(
        String::from_utf8(pretty).expect("#1128 pretty bytes are UTF-8"),
        "{\n  \"a\": {\n    \"a\": 1,\n    \"p\": \"xxxxxxxxxx\"\n  },\n  \"p\": \"xxxxxxxxxx\"\n}",
        "#1128 deep-spine pretty bytes"
    );
    let empty = parse_json("{\"a\":[],\"b\":{}}").expect("#1128 empty containers parse");
    let compact = stringify_json(&empty)
        .expect("#1128 empty containers stringify")
        .expect("#1128 empty containers are not undefined");
    let mut pretty = Vec::new();
    reindent_pretty(&compact, &mut pretty);
    assert_eq!(
        String::from_utf8(pretty).expect("#1128 empty pretty bytes are UTF-8"),
        "{\n  \"a\": [],\n  \"b\": {}\n}",
        "#1128 empty containers stay inline in pretty output"
    );
    let (decoded, encoded, reindented, compact_digest, pretty_bytes, pretty_digest) =
        pipeline(&deep);
    assert_eq!(
        compact_digest, 0xdfe9_9f62_d4de_a2ec,
        "#1128 pinned compact FNV digest"
    );
    assert_eq!(
        pretty_digest, 0x2942_f34b_7277_6356,
        "#1128 pinned pretty FNV digest"
    );
    assert_eq!(pretty_bytes, 71, "#1128 pinned pretty byte count");
    assert!(
        decoded >= 0.0 && encoded >= 0.0 && reindented >= 0.0,
        "#1128 phase durations are nonnegative"
    );
}

/// The measurement itself: `#[ignore]`d — a CI run must never judge
/// performance (#1128 is a measured comparison, not a gate). Driven by
/// `OBJECT_DENSITY_KIND` (`small`|`wide`|`deep`|`file`), `OBJECT_DENSITY_TARGET_BYTES`,
/// `OBJECT_DENSITY_DEPTH`, `OBJECT_DENSITY_INPUT`, `OBJECT_DENSITY_EMIT`,
/// `OBJECT_DENSITY_DUMP_PRETTY`, `OBJECT_DENSITY_REPS`, `OBJECT_DENSITY_WARMUP`.
/// Prints one `#1128-BENCH` JSON line; `measure.py` aggregates the raw reps.
#[test]
#[ignore = "#1128 measurement harness; run through tests/fixtures/jsobject-density/measure.py"]
fn object_density_bench() {
    let kind = std::env::var("OBJECT_DENSITY_KIND")
        .expect("#1128 OBJECT_DENSITY_KIND (small|wide|deep|file) is required");
    let label = std::env::var("OBJECT_DENSITY_LABEL").unwrap_or_else(|_| kind.clone());
    let (text, input_path) = match kind.as_str() {
        "small" => (
            build_small_objects(
                env_usize("OBJECT_DENSITY_TARGET_BYTES")
                    .expect("#1128 OBJECT_DENSITY_TARGET_BYTES is required for small"),
            ),
            None,
        ),
        "wide" => (
            build_wide_object(
                env_usize("OBJECT_DENSITY_TARGET_BYTES")
                    .expect("#1128 OBJECT_DENSITY_TARGET_BYTES is required for wide"),
            ),
            None,
        ),
        "deep" => (
            build_deep_spine(
                env_usize("OBJECT_DENSITY_TARGET_BYTES")
                    .expect("#1128 OBJECT_DENSITY_TARGET_BYTES is required for deep"),
                env_usize("OBJECT_DENSITY_DEPTH").unwrap_or(DEEP_DEFAULT_DEPTH),
            ),
            None,
        ),
        "file" => {
            let path = std::env::var("OBJECT_DENSITY_INPUT")
                .expect("#1128 OBJECT_DENSITY_INPUT is required for file");
            (
                std::fs::read_to_string(&path)
                    .unwrap_or_else(|error| panic!("#1128 cannot read {path}: {error}")),
                Some(path),
            )
        }
        _ => panic!("#1128 unknown OBJECT_DENSITY_KIND {kind}"),
    };
    if let Ok(emit) = std::env::var("OBJECT_DENSITY_EMIT") {
        atomic_write(&emit, text.as_bytes(), "workload input");
    }
    let reps = env_usize("OBJECT_DENSITY_REPS").unwrap_or(7);
    let warmup = env_usize("OBJECT_DENSITY_WARMUP").unwrap_or(1);
    let input_digest = fnv64(text.as_bytes());
    if reps == 0 {
        println!(
            "#1128-BENCH {}",
            serde_json::json!({
                "kind": kind, "label": label, "inputBytes": text.len(),
                "inputFnv64": format!("{input_digest:016x}"),
                "valueBytes": std::mem::size_of::<JsValue>(), "reps": 0,
            })
        );
        return;
    }
    for _ in 0..warmup {
        let (decoded, encoded, reindented, _, _, _) = pipeline(&text);
        assert!(
            decoded >= 0.0 && encoded >= 0.0 && reindented >= 0.0,
            "#1128 warmup phase durations are nonnegative"
        );
    }
    let mut decoded_ms = Vec::with_capacity(reps);
    let mut encoded_ms = Vec::with_capacity(reps);
    let mut reindented_ms = Vec::with_capacity(reps);
    let mut dropped_ms = Vec::with_capacity(reps);
    let mut digests = Vec::with_capacity(reps);
    let mut pretty_bytes = 0;
    let mut pretty_digest = 0;
    for _ in 0..reps {
        let started = Instant::now();
        let value = parse_json(&text).expect("#1128 timed workload parses");
        decoded_ms.push(started.elapsed().as_secs_f64() * 1000.0);
        let started = Instant::now();
        let compact = stringify_json(&value)
            .expect("#1128 timed workload stringifies")
            .expect("#1128 timed workload is not undefined");
        encoded_ms.push(started.elapsed().as_secs_f64() * 1000.0);
        let started = Instant::now();
        let mut pretty = Vec::new();
        reindent_pretty(&compact, &mut pretty);
        reindented_ms.push(started.elapsed().as_secs_f64() * 1000.0);
        let started = Instant::now();
        drop(value);
        dropped_ms.push(started.elapsed().as_secs_f64() * 1000.0);
        let digest = fnv64(&pretty);
        if digests.is_empty() {
            pretty_bytes = pretty.len();
            pretty_digest = digest;
            if let Ok(dump) = std::env::var("OBJECT_DENSITY_DUMP_PRETTY") {
                atomic_write(&dump, &pretty, "pretty dump");
            }
        }
        digests.push(digest);
    }
    assert!(
        digests.iter().all(|digest| *digest == pretty_digest),
        "#1128 pretty digest must be stable across reps"
    );
    println!(
        "#1128-BENCH {}",
        serde_json::json!({
            "kind": kind, "label": label, "inputBytes": text.len(),
            "inputFnv64": format!("{input_digest:016x}"),
            "inputPath": input_path, "valueBytes": std::mem::size_of::<JsValue>(),
            "reps": reps,
            "decodeMs": decoded_ms, "compactMs": encoded_ms,
            "prettyMs": reindented_ms, "dropMs": dropped_ms,
            "prettyBytes": pretty_bytes, "prettyFnv64": format!("{pretty_digest:016x}"),
        })
    );
}
