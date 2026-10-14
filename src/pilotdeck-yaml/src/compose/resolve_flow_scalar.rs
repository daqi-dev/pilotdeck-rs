// Port of yaml@2.9.0 dist/compose/resolve-flow-scalar.js (ISC).
use super::{Context, resolve_end::resolve_end};
use crate::{
    parse::cst::{Token, TokenType::*},
    string::{message, slice},
};
use pilotdeck_protocol::JsString;
pub(crate) struct ResolvedScalar {
    pub value: std::sync::Arc<JsString>,
    pub style: &'static str,
    pub comment: JsString,
    pub range: [usize; 3],
}
pub(crate) fn resolve_flow_scalar(ctx: &mut Context, scalar: &Token) -> ResolvedScalar {
    let offset = scalar.offset;
    let source = scalar.source.as_ref().unwrap();
    let (style, value) = match scalar.kind {
        Scalar => ("PLAIN", plain_value(ctx, source, offset)),
        SingleQuotedScalar => (
            "QUOTE_SINGLE",
            std::sync::Arc::new(single_quoted_value(ctx, source, offset)),
        ),
        DoubleQuotedScalar => (
            "QUOTE_DOUBLE",
            std::sync::Arc::new(double_quoted_value(ctx, source, offset)),
        ),
        _ => {
            ctx.token_error(
                scalar,
                "UNEXPECTED_TOKEN",
                format!(
                    "Expected a flow scalar value, but found: {}",
                    scalar.kind.as_str()
                ),
            );
            ("", Default::default())
        }
    };
    let value_end = offset + source.len_utf16();
    let re = resolve_end(
        ctx,
        scalar.end.as_deref().unwrap_or_default(),
        value_end,
        true,
    );
    ResolvedScalar {
        value,
        style,
        comment: re.comment,
        range: [offset, value_end, re.offset],
    }
}
fn plain_value(
    ctx: &mut Context,
    source: &std::sync::Arc<JsString>,
    offset: usize,
) -> std::sync::Arc<JsString> {
    let bad = match source.units().first() {
        Some(9) => Some("a tab character".to_owned()),
        Some(44) => Some("flow indicator character ,".into()),
        Some(37) => Some("directive indicator character %".into()),
        Some(c @ (124 | 62)) => Some(format!(
            "block scalar indicator {}",
            char::from_u32(*c as u32).unwrap()
        )),
        Some(c @ (64 | 96)) => Some(format!(
            "reserved character {}",
            char::from_u32(*c as u32).unwrap()
        )),
        _ => None,
    };
    if let Some(bad) = bad {
        ctx.at(
            offset,
            "BAD_SCALAR_START",
            format!("Plain value cannot start with {bad}"),
        );
    }
    if source.units().contains(&10) {
        std::sync::Arc::new(fold_lines(source))
    } else {
        source.clone()
    }
}
fn single_quoted_value(ctx: &mut Context, source: &JsString, offset: usize) -> JsString {
    if source.units().last() != Some(&39) || source.len_utf16() == 1 {
        ctx.at(
            offset + source.len_utf16(),
            "MISSING_CHAR",
            "Missing closing 'quote",
        );
    }
    let folded = fold_lines(&slice(source, 1, source.len_utf16().saturating_sub(1)));
    let mut res = Vec::new();
    let mut i = 0;
    while i < folded.len_utf16() {
        let ch = folded.units()[i];
        res.push(ch);
        i += 1;
        if ch == 39 && folded.units().get(i) == Some(&39) {
            i += 1;
        }
    }
    JsString::from_utf16(res)
}
fn fold_lines(source: &JsString) -> JsString {
    let s = source.units();
    let Some(nl) = s.iter().position(|c| *c == 10) else {
        return source.clone();
    };
    let mut end = nl;
    if end > 0 && s[end - 1] == 13 {
        end -= 1;
    }
    while end > 0 && matches!(s[end - 1], 32 | 9) {
        end -= 1;
    }
    let mut res = s[..end].to_vec();
    let mut sep = 32;
    let mut pos = nl + 1;
    while let Some(n) = s[pos..].iter().position(|c| *c == 10).map(|n| n + pos) {
        let mut a = pos;
        while a < n && matches!(s[a], 32 | 9) {
            a += 1;
        }
        let mut b = n;
        if b > a && s[b - 1] == 13 {
            b -= 1;
        }
        while b > a && matches!(s[b - 1], 32 | 9) {
            b -= 1;
        }
        if a == b {
            if sep == 10 {
                res.push(10);
            } else {
                sep = 10;
            }
        } else {
            res.push(sep);
            res.extend_from_slice(&s[a..b]);
            sep = 32;
        }
        pos = n + 1;
    }
    while pos < s.len() && matches!(s[pos], 32 | 9) {
        pos += 1;
    }
    res.push(sep);
    res.extend_from_slice(&s[pos..]);
    JsString::from_utf16(res)
}
fn double_quoted_value(ctx: &mut Context, source: &JsString, offset: usize) -> JsString {
    let s = source.units();
    let mut res = Vec::new();
    let mut i = 1;
    while i + 1 < s.len() {
        let ch = s[i];
        if ch == 13 && s.get(i + 1) == Some(&10) {
            i += 1;
            continue;
        }
        if ch == 10 {
            let (fold, end) = fold_newline(s, i);
            res.extend(fold);
            i = end;
        } else if ch == 92 {
            i += 1;
            let next = s.get(i).copied();
            let cc = match next {
                Some(48) => Some(0),
                Some(97) => Some(7),
                Some(98) => Some(8),
                Some(101) => Some(27),
                Some(102) => Some(12),
                Some(110) => Some(10),
                Some(114) => Some(13),
                Some(116 | 9) => Some(9),
                Some(118) => Some(11),
                Some(78) => Some(0x85),
                Some(95) => Some(0xa0),
                Some(76) => Some(0x2028),
                Some(80) => Some(0x2029),
                Some(c @ (32 | 34 | 47 | 92)) => Some(c),
                _ => None,
            };
            if let Some(cc) = cc {
                res.push(cc);
            } else if next == Some(10) {
                while matches!(s.get(i + 1), Some(32 | 9)) {
                    i += 1;
                }
            } else if next == Some(13) && s.get(i + 1) == Some(&10) {
                i += 1;
                while matches!(s.get(i + 1), Some(32 | 9)) {
                    i += 1;
                }
            } else if matches!(next, Some(120 | 117 | 85)) {
                let len = if next == Some(120) {
                    2
                } else if next == Some(117) {
                    4
                } else {
                    8
                };
                res.extend(parse_char_code(ctx, source, i + 1, len, offset));
                i += len;
            } else {
                let raw = slice(source, i - 1, i + 1);
                ctx.at(
                    offset + i - 1,
                    "BAD_DQ_ESCAPE",
                    message("Invalid escape sequence ", &raw),
                );
                res.extend_from_slice(raw.units());
            }
        } else if matches!(ch, 32 | 9) {
            let ws_start = i;
            while matches!(s.get(i + 1), Some(32 | 9)) {
                i += 1;
            }
            if s.get(i + 1) != Some(&10)
                && !(s.get(i + 1) == Some(&13) && s.get(i + 2) == Some(&10))
            {
                res.extend_from_slice(&s[ws_start..=i]);
            }
        } else {
            res.push(ch);
        }
        i += 1;
    }
    if s.last() != Some(&34) || s.len() == 1 {
        ctx.at(offset + s.len(), "MISSING_CHAR", "Missing closing \"quote");
    }
    JsString::from_utf16(res)
}
fn fold_newline(s: &[u16], mut offset: usize) -> (Vec<u16>, usize) {
    let mut fold = Vec::new();
    while let Some(ch) = s.get(offset + 1) {
        if !matches!(ch, 32 | 9 | 10 | 13) || (*ch == 13 && s.get(offset + 2) != Some(&10)) {
            break;
        }
        if *ch == 10 {
            fold.push(10);
        }
        offset += 1;
    }
    if fold.is_empty() {
        fold.push(32);
    }
    (fold, offset)
}
fn parse_char_code(
    ctx: &mut Context,
    source: &JsString,
    offset: usize,
    length: usize,
    base: usize,
) -> Vec<u16> {
    let cc = slice(source, offset, offset + length);
    let code = if cc.len_utf16() == length {
        cc.units().iter().try_fold(0u32, |n, c| {
            char::from_u32(*c as u32)?.to_digit(16).map(|d| n * 16 + d)
        })
    } else {
        None
    };
    if let Some(code) = code.filter(|c| *c <= 0x10ffff) {
        if code <= 0xffff {
            return vec![code as u16];
        }
        let n = code - 0x10000;
        return vec![0xd800 + (n >> 10) as u16, 0xdc00 + (n & 1023) as u16];
    }
    let raw = slice(source, offset - 2, offset + length);
    ctx.at(
        base + offset - 2,
        "BAD_DQ_ESCAPE",
        message("Invalid escape sequence ", &raw),
    );
    raw.units().to_vec()
}
