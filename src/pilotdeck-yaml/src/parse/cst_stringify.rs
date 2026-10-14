// Port of yaml@2.9.0 dist/parse/cst-stringify.js (ISC).
// Lossless CST source concatenation, not YAML value stringification.
use super::cst::{CollectionItem, Token, TokenType::*};
use crate::string::append;
use pilotdeck_protocol::JsString;
pub fn stringify(token: &Token) -> JsString {
    let mut res = JsString::default();
    match token.kind {
        BlockScalar => {
            for tok in &token.props {
                append(&mut res, &stringify(tok));
            }
            append(&mut res, token.source.as_ref().unwrap());
        }
        BlockMap | BlockSeq => {
            for item in &token.items {
                append(&mut res, &stringify_item(item));
            }
        }
        FlowCollection => {
            append(
                &mut res,
                token.flow_start.as_ref().unwrap().source.as_ref().unwrap(),
            );
            for item in &token.items {
                append(&mut res, &stringify_item(item));
            }
            for st in token.end.as_deref().unwrap_or_default() {
                append(&mut res, st.source.as_ref().unwrap());
            }
        }
        Document => {
            for st in &token.start {
                append(&mut res, st.source.as_ref().unwrap());
            }
            if let Some(value) = &token.value {
                append(&mut res, &stringify(value));
            }
            for st in token.end.as_deref().unwrap_or_default() {
                append(&mut res, st.source.as_ref().unwrap());
            }
        }
        _ => {
            if let Some(source) = &token.source {
                append(&mut res, source);
            }
            for st in token.end.as_deref().unwrap_or_default() {
                append(&mut res, st.source.as_ref().unwrap());
            }
        }
    }
    res
}
pub fn stringify_item(item: &CollectionItem) -> JsString {
    let mut res = JsString::default();
    for st in &item.start {
        append(&mut res, st.source.as_ref().unwrap());
    }
    if let Some(Some(key)) = &item.key {
        append(&mut res, &stringify(key));
    }
    if let Some(sep) = &item.sep {
        for st in sep {
            append(&mut res, st.source.as_ref().unwrap());
        }
    }
    if let Some(value) = &item.value {
        append(&mut res, &stringify(value));
    }
    res
}
