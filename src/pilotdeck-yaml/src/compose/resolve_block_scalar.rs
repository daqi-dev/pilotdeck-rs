// Port of yaml@2.9.0 dist/compose/resolve-block-scalar.js (ISC).
use super::{Context, resolve_flow_scalar::ResolvedScalar};
use crate::{
    parse::cst::{Token, TokenType::*},
    string::{message, slice},
};
use pilotdeck_protocol::JsString;
struct Header {
    mode: u16,
    indent: usize,
    chomp: u16,
    comment: JsString,
    length: usize,
}
pub(crate) fn resolve_block_scalar(ctx: &mut Context, scalar: &Token) -> ResolvedScalar {
    let start = scalar.offset;
    let header = parse_block_scalar_header(ctx, scalar);
    let style = if header.mode == 62 {
        "BLOCK_FOLDED"
    } else {
        "BLOCK_LITERAL"
    };
    let source = scalar.source.as_ref().unwrap();
    let lines = if source.is_empty() {
        Vec::new()
    } else {
        split_lines(source)
    };
    let mut chomp_start = lines.len();
    for (i, (_, content)) in lines.iter().enumerate().rev() {
        if content.is_empty() || *content == [13] {
            chomp_start = i;
        } else {
            break;
        }
    }
    let end = start + header.length + source.len_utf16();
    if chomp_start == 0 {
        let value = if header.chomp == 43 && !lines.is_empty() {
            JsString::from_utf16(vec![10; lines.len().saturating_sub(1).max(1)])
        } else {
            JsString::default()
        };
        return ResolvedScalar {
            value,
            style,
            comment: header.comment,
            range: [start, end, end],
        };
    }
    let mut trim_indent = scalar.indent.unwrap_or(0) + header.indent;
    let mut offset = scalar.offset + header.length;
    let mut content_start = 0;
    for (i, (indent, content)) in lines.iter().enumerate().take(chomp_start) {
        if content.is_empty() || *content == [13] {
            if header.indent == 0 && indent.len() > trim_indent {
                trim_indent = indent.len();
            }
        } else {
            if indent.len() < trim_indent {
                ctx.at(offset+indent.len(),"MISSING_CHAR","Block scalars with more-indented leading empty lines must use an explicit indentation indicator");
            }
            if header.indent == 0 {
                trim_indent = indent.len();
            }
            content_start = i;
            if trim_indent == 0 && !ctx.at_root {
                ctx.at(
                    offset,
                    "BAD_INDENT",
                    "Block scalar values in collections must be indented",
                );
            }
            break;
        }
        offset += indent.len() + content.len() + 1;
    }
    for (i, (indent, _)) in lines.iter().enumerate().skip(chomp_start).rev() {
        if indent.len() > trim_indent {
            chomp_start = i + 1;
        }
    }
    let mut value = Vec::new();
    let mut sep: Vec<u16> = Vec::new();
    let mut prev_more_indented = false;
    for (indent, _) in lines.iter().take(content_start) {
        value.extend_from_slice(&indent[trim_indent.min(indent.len())..]);
        value.push(10);
    }
    for (ind, con) in lines.iter().take(chomp_start).skip(content_start) {
        let mut indent = *ind;
        let mut content = *con;
        offset += indent.len() + content.len() + 1;
        let crlf = content.last() == Some(&13);
        if crlf {
            content = &content[..content.len() - 1];
        }
        if !content.is_empty() && indent.len() < trim_indent {
            let src = if header.indent > 0 {
                "explicit indentation indicator"
            } else {
                "first line"
            };
            ctx.at(
                offset - content.len() - if crlf { 2 } else { 1 },
                "BAD_INDENT",
                format!("Block scalar lines must not be less indented than their {src}"),
            );
            indent = &[];
        }
        if style == "BLOCK_LITERAL" {
            value.extend(&sep);
            value.extend_from_slice(&indent[trim_indent.min(indent.len())..]);
            value.extend_from_slice(content);
            sep = vec![10];
        } else if indent.len() > trim_indent || content.first() == Some(&9) {
            if sep == [32] {
                sep = vec![10];
            } else if !prev_more_indented && sep == [10] {
                sep = vec![10, 10];
            }
            value.extend(&sep);
            value.extend_from_slice(&indent[trim_indent.min(indent.len())..]);
            value.extend_from_slice(content);
            sep = vec![10];
            prev_more_indented = true;
        } else if content.is_empty() {
            if sep == [10] {
                value.push(10);
            } else {
                sep = vec![10];
            }
        } else {
            value.extend(&sep);
            value.extend_from_slice(content);
            sep = vec![32];
            prev_more_indented = false;
        }
    }
    match header.chomp {
        45 => (),
        43 => {
            for (indent, _) in lines.iter().skip(chomp_start) {
                value.push(10);
                value.extend_from_slice(&indent[trim_indent.min(indent.len())..]);
            }
            if value.last() != Some(&10) {
                value.push(10);
            }
        }
        _ => value.push(10),
    }
    ResolvedScalar {
        value: JsString::from_utf16(value),
        style,
        comment: header.comment,
        range: [start, end, end],
    }
}
fn parse_block_scalar_header(ctx: &mut Context, scalar: &Token) -> Header {
    let props = &scalar.props;
    let source = props[0].source.as_ref().unwrap();
    let mode = source.units()[0];
    let mut indent = 0;
    let mut chomp = 0;
    let mut error = None;
    for (i, ch) in source.units().iter().enumerate().skip(1) {
        if chomp == 0 && matches!(ch, 45 | 43) {
            chomp = *ch;
        } else if indent == 0 && matches!(ch, 49..=57) {
            indent = (*ch - 48) as usize;
        } else if error.is_none() {
            error = Some(scalar.offset + i);
        }
    }
    if let Some(pos) = error {
        ctx.at(
            pos,
            "UNEXPECTED_TOKEN",
            message("Block scalar header includes extra characters: ", source),
        );
    }
    let mut has_space = false;
    let mut comment = JsString::default();
    let mut length = source.len_utf16();
    for token in props.iter().skip(1) {
        let source = token.source.as_ref().unwrap();
        match token.kind {
            Space => has_space = true,
            Newline => (),
            Comment => {
                if !has_space {
                    ctx.token_error(
                        token,
                        "MISSING_CHAR",
                        "Comments must be separated from other tokens by white space characters",
                    );
                }
                comment = slice(source, 1, source.len_utf16());
            }
            Error => ctx.token_error(token, "UNEXPECTED_TOKEN", token.message.clone().unwrap()),
            _ => ctx.token_error(
                token,
                "UNEXPECTED_TOKEN",
                format!(
                    "Unexpected token in block scalar header: {}",
                    token.kind.as_str()
                ),
            ),
        }
        length += source.len_utf16();
    }
    Header {
        mode,
        indent,
        chomp,
        comment,
        length,
    }
}
fn split_lines(source: &JsString) -> Vec<(&[u16], &[u16])> {
    source
        .units()
        .split(|c| *c == 10)
        .map(|s| {
            let n = s.iter().take_while(|c| **c == 32).count();
            (&s[..n], &s[n..])
        })
        .collect()
}
