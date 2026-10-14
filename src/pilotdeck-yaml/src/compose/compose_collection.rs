// Port of yaml@2.9.0 dist/compose/compose-collection.js (ISC).
use super::{
    Context, resolve_block_map::resolve_block_map, resolve_block_seq::resolve_block_seq,
    resolve_flow_collection::resolve_flow_collection, resolve_props::Props,
};
use crate::{
    errors::token_pos,
    nodes::NodeId,
    parse::cst::{Token, TokenType::*},
    schema::{Resolve, tags},
    string::message,
};
fn resolve_collection(ctx: &mut Context, token: &Token) -> Result<NodeId, ()> {
    let limit = ctx.depth_limit;
    if ctx.depth >= limit.catch_depth
        && token.nesting_depth_exceeds(limit.first_exhausted.saturating_sub(ctx.depth))
    {
        return Err(());
    }
    Ok(match token.kind {
        BlockMap => resolve_block_map(ctx, token),
        BlockSeq => resolve_block_seq(ctx, token),
        _ => resolve_flow_collection(ctx, token),
    })
}
pub(super) fn compose_collection(
    ctx: &mut Context,
    token: &Token,
    props: &Props,
) -> Result<NodeId, ()> {
    let tag_token = props.tag.as_ref();
    let tag_name = tag_token.and_then(|t| {
        let (name, errors) = ctx.directives.tag_name(t.source.as_ref().unwrap());
        for msg in errors {
            ctx.token_error(t, "TAG_RESOLVE_FAILED", msg);
        }
        name
    });
    if token.kind == BlockSeq {
        let last_prop = props
            .anchor
            .iter()
            .chain(props.tag.iter())
            .max_by_key(|t| t.offset);
        if let Some(p) = last_prop
            && props
                .newline_after_prop
                .as_ref()
                .is_none_or(|nl| nl.offset < p.offset)
        {
            ctx.token_error(
                p,
                "MISSING_CHAR",
                "Missing newline after block sequence props",
            );
        }
    }
    let exp_type = if token.kind == BlockMap
        || (token.kind == FlowCollection && token.flow_start.as_ref().unwrap().kind == FlowMapStart)
    {
        "map"
    } else {
        "seq"
    };
    let generic = if exp_type == "map" {
        "tag:yaml.org,2002:map"
    } else {
        "tag:yaml.org,2002:seq"
    };
    if tag_name
        .as_ref()
        .is_none_or(|s| s.equals_str("!") || s.equals_str(generic))
    {
        let id = resolve_collection(ctx, token)?;
        ctx.nodes[id].tag = tag_name.map(|_| generic.into());
        return Ok(id);
    }
    let name = tag_name.as_ref().unwrap();
    let tag_token = tag_token.unwrap();
    let mut tag = ctx
        .schema
        .tags
        .iter()
        .find(|t| name.equals_str(t.name) && t.collection == Some(exp_type))
        .cloned();
    if tag.is_none() {
        let known = if ctx.schema.known_tags {
            tags::known(name)
        } else {
            None
        };
        if let Some(kt) = known.filter(|t| t.collection == Some(exp_type)) {
            let mut added = kt.clone();
            added.default = false;
            ctx.schema.tags.push(added);
            tag = Some(kt);
        } else {
            if let Some(kt) = if ctx.schema.known_tags {
                tags::known(name)
            } else {
                None
            } {
                ctx.warn(
                    token_pos(tag_token),
                    "BAD_COLLECTION_TYPE",
                    format!(
                        "{} used for {} collection, but expects {}",
                        kt.name,
                        exp_type,
                        kt.collection.unwrap_or("scalar")
                    ),
                );
            } else {
                ctx.warn(
                    token_pos(tag_token),
                    "TAG_RESOLVE_FAILED",
                    message("Unresolved tag: ", name),
                );
            }
            let id = resolve_collection(ctx, token)?;
            ctx.nodes[id].tag = tag_name;
            return Ok(id);
        }
    }
    let id = resolve_collection(ctx, token)?;
    let tag = tag.unwrap();
    match tag.resolve {
        Resolve::Set => crate::schema::yaml_1_1::set::resolve(ctx, id, tag_token),
        Resolve::OMap => crate::schema::yaml_1_1::omap::resolve(ctx, id, tag_token),
        Resolve::Pairs => crate::schema::yaml_1_1::pairs::resolve(ctx, id, tag_token),
        _ => (),
    }
    ctx.nodes[id].tag = tag_name;
    ctx.nodes[id].format = tag.format;
    Ok(id)
}
