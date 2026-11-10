//! #1766: the response-normalization parser the golden-fixture replays
//! compare their bytes through — the #1209 rules (Date masked, header names
//! case-folded, chunk sizes case-folded) with one seam: every consumer
//! injects its own body mask, so the api-web root/directory-size masks and
//! future fixtures' masks stay at their call sites, and the one
//! transport-diff drop (`content-length: 0`, #2153) is granted per call
//! through `parser_admission` — the api-web parser-admission gates only, so
//! an unregistered answer that grows the line replays red instead of
//! normalizing away. Extracted verbatim from the api-web replay (the third
//! copy of the #1209/#1350 rules); the module_host and approvals copies are
//! deliberately untouched and migrate on their own lanes.
//!
//! Included with `#[path]` from a test module (see the api-web replay), the
//! same pattern the lib's other `tests/common/` modules use.

/// Normalize one capture of raw wire bytes: parse each response head, mask
/// the Date value and case-fold header names (hyper renders Title-Case, TS's
/// `content-type` is literal lowercase — the registered transport diff),
/// drop the empty-body `content-length: 0` line on the `parser_admission`
/// responses only (#2153, the same scoping native_http's replay gave its
/// registered bare 400s), case-fold chunk sizes, and pass every body through
/// the caller's mask. A chunked body is re-framed as one chunk whose size
/// names the masked content, because the oracle's recorded chunk sizes name
/// the pre-mask bytes — the same recomputation on both sides makes them
/// agree.
pub(crate) fn normalize(
    raw: &[u8],
    parser_admission: bool,
    mask_body: &dyn Fn(&[u8]) -> Vec<u8>,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    let mut rest = raw;
    'responses: while !rest.is_empty() {
        let Some(head_end) = find(rest, b"\r\n\r\n") else {
            out.extend_from_slice(rest);
            break;
        };
        let head = &rest[..head_end];
        let mut chunked = false;
        let mut length = None;
        for line in split(head, b"\r\n").skip(1) {
            let Some((name, value)) = split_once(line, b":") else {
                continue;
            };
            let name = trim_ascii(name).to_ascii_lowercase();
            if name == b"transfer-encoding"
                && find(&value.to_ascii_lowercase(), b"chunked").is_some()
            {
                chunked = true;
            } else if name == b"content-length" {
                length = std::str::from_utf8(value)
                    .ok()
                    .and_then(|text| text.trim().parse::<usize>().ok());
            }
        }
        rest = &rest[head_end + 4..];
        if chunked {
            let Some(body) = chunked_content(&mut rest) else {
                emit_head(&mut out, head, parser_admission);
                out.extend_from_slice(rest);
                break 'responses;
            };
            let body = mask_body(&body);
            emit_head(&mut out, head, parser_admission);
            out.extend_from_slice(format!("{:x}\r\n", body.len()).as_bytes());
            out.extend_from_slice(&body);
            out.extend_from_slice(b"\r\n0\r\n\r\n");
            continue 'responses;
        }
        match length {
            Some(size) if rest.len() >= size => {
                let body = mask_body(&rest[..size]);
                rest = &rest[size..];
                emit_head(&mut out, head, parser_admission);
                out.extend_from_slice(&body);
            }
            _ => {
                emit_head(&mut out, head, parser_admission);
                out.extend_from_slice(rest);
                break 'responses;
            }
        }
    }
    out
}

/// The head lines under the comparison rules; the status line is verbatim
/// (both stacks write canonical casing, and the one code without a canonical
/// reason never appears on this surface). `parser_admission` scopes the one
/// registered transport-diff drop (#2153): hyper's implicit empty-body
/// `content-length: 0` on the parser bare 400s the golden records without
/// the line.
pub(crate) fn emit_head(out: &mut Vec<u8>, head: &[u8], parser_admission: bool) {
    for (index, line) in split(head, b"\r\n").enumerate() {
        if index == 0 {
            out.extend_from_slice(line);
            out.extend_from_slice(b"\r\n");
            continue;
        }
        let Some((name, value)) = split_once(line, b":") else {
            out.extend_from_slice(line);
            out.extend_from_slice(b"\r\n");
            continue;
        };
        if name.eq_ignore_ascii_case(b"date") {
            out.extend_from_slice(b"date: <masked>\r\n");
        } else if parser_admission
            && name.eq_ignore_ascii_case(b"content-length")
            && trim_ascii(value) == b"0"
        {
            // hyper's implicit empty-body length on the registered
            // parser-admission responses only.
        } else {
            out.extend_from_slice(&name.to_ascii_lowercase());
            out.extend_from_slice(b":");
            out.extend_from_slice(value);
            out.extend_from_slice(b"\r\n");
        }
    }
    out.extend_from_slice(b"\r\n");
}

/// One chunked body's content, single-chunk (both stacks write exactly one
/// data chunk per `writeHead`+`end` / single-frame response): the section
/// between the size line after the head and the terminal `0` chunk. The
/// oracle's recorded chunk sizes name the pre-mask content, so a size-line
/// walk cannot find the boundary there — the terminal marker can, and no
/// body carries a CRLF for it to hide inside. `None` when the bytes end
/// before the terminal chunk.
pub(crate) fn chunked_content(rest: &mut &[u8]) -> Option<Vec<u8>> {
    const TERMINAL: &[u8] = b"\r\n0\r\n\r\n";
    let end = find(rest, TERMINAL)?;
    let section = &rest[..end];
    // The first line is the (stale) size; the content is the rest.
    let content = split_once(section, b"\r\n").map_or(section, |(_, after)| after);
    *rest = &rest[end + TERMINAL.len()..];
    Some(content.to_vec())
}

/// Find the first occurrence of `needle` in `haystack`.
pub(crate) fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Split on every `separator`, the `str::split` shape (a trailing separator
/// yields a final empty slice; an empty haystack yields nothing).
pub(crate) fn split<'a>(haystack: &'a [u8], separator: &[u8]) -> impl Iterator<Item = &'a [u8]> {
    let mut rest = haystack;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        match find(rest, separator) {
            Some(at) => {
                let (line, tail) = rest.split_at(at);
                rest = &tail[separator.len()..];
                Some(line)
            }
            None => {
                let line = rest;
                rest = &[];
                Some(line)
            }
        }
    })
}

pub(crate) fn split_once<'a>(haystack: &'a [u8], separator: &[u8]) -> Option<(&'a [u8], &'a [u8])> {
    find(haystack, separator).map(|at| {
        let (before, after) = haystack.split_at(at);
        (before, &after[separator.len()..])
    })
}

/// Trim ASCII whitespace from both ends.
pub(crate) fn trim_ascii(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(start, |at| at + 1);
    &bytes[start..end]
}
