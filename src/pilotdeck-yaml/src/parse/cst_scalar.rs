// Port of yaml@2.9.0 dist/parse/cst-scalar.js (ISC), resolveAsScalar.
// Scalar creation/replacement uses the public stringifier and is deferred with it.
use super::cst::{Token, TokenType};
use crate::{Options, compose, errors::Diagnostic};
use pilotdeck_protocol::JsString;
#[derive(Clone, Debug)]
pub struct ScalarResolution {
    pub value: JsString,
    pub style: &'static str,
    pub comment: JsString,
    pub range: [usize; 3],
    pub errors: Vec<Diagnostic>,
}
/// Resolve a CST scalar using the baseline's strict=true default. Diagnostics
/// correspond to resolveAsScalar's onError callback, with one-unit positions.
pub fn resolve_as_scalar(token: &Token) -> Option<ScalarResolution> {
    let mut composer = compose::composer::Composer::new(Options::default());
    let mut ctx = composer.context();
    ctx.at_root = false;
    let scalar = match token.kind {
        TokenType::Scalar | TokenType::SingleQuotedScalar | TokenType::DoubleQuotedScalar => {
            compose::resolve_flow_scalar::resolve_flow_scalar(&mut ctx, token)
        }
        TokenType::BlockScalar => {
            compose::resolve_block_scalar::resolve_block_scalar(&mut ctx, token)
        }
        _ => return None,
    };
    for error in &mut ctx.errors {
        error.pos[1] = error.pos[0] + 1;
    }
    Some(ScalarResolution {
        value: scalar.value,
        style: scalar.style,
        comment: scalar.comment,
        range: scalar.range,
        errors: ctx.errors,
    })
}
