// Port of yaml@2.9.0 dist/parse/cst-stringify.js (ISC).
// Lossless CST concatenation with an explicit work list and one UTF-16 buffer.
use super::cst::{CollectionItem, Token, TokenType::*};
use pilotdeck_protocol::JsString;

enum Part<'a> {
    Token(&'a Token),
    Item(&'a CollectionItem),
    Source(&'a JsString),
}
fn concatenate(mut pending: Vec<Part<'_>>) -> JsString {
    let mut output = Vec::new();
    while let Some(part) = pending.pop() {
        match part {
            Part::Source(source) => output.extend_from_slice(source.units()),
            Part::Item(item) => {
                if let Some(value) = item.value.as_deref() {
                    pending.push(Part::Token(value));
                }
                for t in item.sep.as_deref().unwrap_or_default().iter().rev() {
                    pending.push(Part::Source(t.source.as_ref().unwrap()));
                }
                if let Some(Some(key)) = &item.key {
                    pending.push(Part::Token(key));
                }
                for t in item.start.iter().rev() {
                    pending.push(Part::Source(t.source.as_ref().unwrap()));
                }
            }
            Part::Token(token) => match token.kind {
                BlockScalar => {
                    pending.push(Part::Source(token.source.as_ref().unwrap()));
                    pending.extend(token.props.iter().rev().map(Part::Token));
                }
                BlockMap | BlockSeq => pending.extend(token.items.iter().rev().map(Part::Item)),
                FlowCollection => {
                    for t in token.end.as_deref().unwrap_or_default().iter().rev() {
                        pending.push(Part::Source(t.source.as_ref().unwrap()));
                    }
                    pending.extend(token.items.iter().rev().map(Part::Item));
                    pending.push(Part::Source(
                        token.flow_start.as_ref().unwrap().source.as_ref().unwrap(),
                    ));
                }
                Document => {
                    for t in token.end.as_deref().unwrap_or_default().iter().rev() {
                        pending.push(Part::Source(t.source.as_ref().unwrap()));
                    }
                    if let Some(value) = token.value.as_deref() {
                        pending.push(Part::Token(value));
                    }
                    for t in token.start.iter().rev() {
                        pending.push(Part::Source(t.source.as_ref().unwrap()));
                    }
                }
                _ => {
                    for t in token.end.as_deref().unwrap_or_default().iter().rev() {
                        pending.push(Part::Source(t.source.as_ref().unwrap()));
                    }
                    if let Some(source) = &token.source {
                        pending.push(Part::Source(source));
                    }
                }
            },
        }
    }
    JsString::from_utf16(output)
}
pub fn stringify(token: &Token) -> JsString {
    concatenate(vec![Part::Token(token)])
}
pub fn stringify_item(item: &CollectionItem) -> JsString {
    concatenate(vec![Part::Item(item)])
}
