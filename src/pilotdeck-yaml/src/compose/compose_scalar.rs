// Port of yaml@2.9.0 dist/compose/compose-scalar.js (ISC).
use super::{
    Context, resolve_block_scalar::resolve_block_scalar, resolve_flow_scalar::resolve_flow_scalar,
};
use crate::{
    errors::token_pos,
    nodes::{Node, NodeId, NodeKind, ScalarValue},
    parse::cst::{Token, TokenType},
    schema::{Tag, tags},
    string::message,
};
use pilotdeck_protocol::JsString;
pub(super) fn compose_scalar(
    ctx: &mut Context,
    token: &Token,
    tag_token: Option<&Token>,
) -> NodeId {
    let resolved = if token.kind == TokenType::BlockScalar {
        resolve_block_scalar(ctx, token)
    } else {
        resolve_flow_scalar(ctx, token)
    };
    let tag_name = tag_token.and_then(|t| {
        let (name, errors) = ctx.directives.tag_name(t.source.as_ref().unwrap());
        for msg in errors {
            ctx.token_error(t, "TAG_RESOLVE_FAILED", msg);
        }
        name
    });
    let tag = if let Some(name) = &tag_name {
        find_scalar_tag_by_name(ctx, &resolved.value, name, tag_token.unwrap())
    } else if token.kind == TokenType::Scalar {
        find_scalar_tag_by_test(ctx, &resolved.value)
    } else {
        ctx.schema.scalar()
    };
    let value = match ctx.schema.resolve_value(&tag, &resolved.value) {
        Ok(value) => value,
        Err(msg) => {
            ctx.token_error(tag_token.unwrap_or(token), "TAG_RESOLVE_FAILED", msg);
            ScalarValue::String(resolved.value.clone())
        }
    };
    let mut scalar = Node::new(NodeKind::Scalar(value));
    scalar.range = resolved.range;
    scalar.source = resolved.value;
    scalar.style = resolved.style;
    scalar.tag = tag_name;
    scalar.format = tag.format;
    scalar.comment = resolved.comment;
    ctx.add(scalar)
}
fn find_scalar_tag_by_name(
    ctx: &mut Context,
    value: &JsString,
    tag_name: &JsString,
    tag_token: &Token,
) -> Tag {
    if tag_name.equals_str("!") {
        return ctx.schema.scalar();
    }
    let mut with_test = Vec::new();
    for tag in &ctx.schema.tags {
        if tag.collection.is_none() && tag_name.equals_str(tag.name) {
            if (tag.default || tag.key_only) && tag.pattern.is_some() {
                with_test.push(tag.clone());
            } else {
                return tag.clone();
            }
        }
    }
    for tag in with_test {
        if tag.test(value) {
            return tag;
        }
    }
    if ctx.schema.known_tags
        && let Some(tag) = tags::known(tag_name).filter(|t| t.collection.is_none())
    {
        let mut added = tag.clone();
        added.default = false;
        added.pattern = None;
        ctx.schema.tags.push(added);
        return tag;
    }
    let msg = message("Unresolved tag: ", tag_name);
    if tag_name.equals_str("tag:yaml.org,2002:str") {
        ctx.token_error(tag_token, "TAG_RESOLVE_FAILED", msg);
    } else {
        ctx.warn(token_pos(tag_token), "TAG_RESOLVE_FAILED", msg);
    }
    ctx.schema.scalar()
}
fn find_scalar_tag_by_test(ctx: &Context, value: &JsString) -> Tag {
    ctx.schema
        .tags
        .iter()
        .find(|t| (t.default || (ctx.at_key && t.key_only)) && t.test(value))
        .cloned()
        .unwrap_or_else(|| ctx.schema.scalar())
}
