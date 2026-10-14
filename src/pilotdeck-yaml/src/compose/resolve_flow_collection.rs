// Port of yaml@2.9.0 dist/compose/resolve-flow-collection.js (ISC).
use super::{
    Context,
    compose_node::{compose_empty_node, compose_node},
    resolve_end::resolve_end,
    resolve_props::{PropOptions, resolve_props},
    util_contains_newline::contains_newline,
    util_map_includes::map_includes,
};
use crate::{
    nodes::{Node, NodeId, NodeKind, Pair},
    parse::cst::{Token, TokenType::*},
    string::{append, slice},
};
const BLOCK_MSG: &str = "Block collections are not allowed within flow collections";
fn is_block(t: Option<&Token>) -> bool {
    t.is_some_and(|t| matches!(t.kind, BlockMap | BlockSeq))
}
pub(super) fn resolve_flow_collection(ctx: &mut Context, fc: &Token) -> NodeId {
    let is_map = fc.flow_start.as_ref().unwrap().kind == FlowMapStart;
    let fc_name = if is_map { "flow map" } else { "flow sequence" };
    let mut coll = Node::new(NodeKind::Seq(Vec::new()));
    coll.flow = true;
    let mut pairs = Vec::new();
    let mut values: Vec<NodeId> = Vec::new();
    let at_root = ctx.at_root;
    ctx.at_root = false;
    ctx.at_key = false;
    let mut offset = fc.offset
        + fc.flow_start
            .as_ref()
            .unwrap()
            .source
            .as_ref()
            .unwrap()
            .len_utf16();
    for (i, it) in fc.items.iter().enumerate() {
        let key = it.key.as_ref().and_then(|k| k.as_deref());
        let sep = it.sep.as_deref().unwrap_or_default();
        let value = it.value.as_deref();
        let mut props = resolve_props(
            ctx,
            &it.start,
            PropOptions {
                flow: Some(fc_name),
                indicator: ExplicitKeyInd,
                next: key.or_else(|| sep.first()),
                offset,
                parent_indent: fc.indent.unwrap(),
                start_on_newline: false,
            },
        );
        if props.found.is_none() {
            if props.anchor.is_none() && props.tag.is_none() && it.sep.is_none() && value.is_none()
            {
                if i == 0
                    && let Some(comma) = &props.comma
                {
                    ctx.token_error(
                        comma,
                        "UNEXPECTED_TOKEN",
                        format!("Unexpected , in {fc_name}"),
                    );
                } else if i < fc.items.len() - 1 {
                    ctx.at(
                        props.start,
                        "UNEXPECTED_TOKEN",
                        format!("Unexpected empty item in {fc_name}"),
                    );
                }
                if !props.comment.is_empty() {
                    if !coll.comment.is_empty() {
                        append(&mut coll.comment, &"\n".into());
                    }
                    append(&mut coll.comment, &props.comment);
                }
                offset = props.end;
                continue;
            }
            if !is_map && contains_newline(key) {
                ctx.token_error(
                    key.unwrap(),
                    "MULTILINE_IMPLICIT_KEY",
                    "Implicit keys of flow sequence pairs need to be on a single line",
                );
            }
        }
        if i == 0 {
            if let Some(c) = &props.comma {
                ctx.token_error(c, "UNEXPECTED_TOKEN", format!("Unexpected , in {fc_name}"));
            }
        } else {
            if props.comma.is_none() {
                ctx.at(
                    props.start,
                    "MISSING_CHAR",
                    format!("Missing , between {fc_name} items"),
                );
            }
            if !props.comment.is_empty() {
                let mut prev_comment = None;
                for st in &it.start {
                    match st.kind {
                        Comma | Space => (),
                        Comment => {
                            let s = st.source.as_ref().unwrap();
                            prev_comment = Some(slice(s, 1, s.len_utf16()));
                            break;
                        }
                        _ => break,
                    }
                }
                if let Some(c) = prev_comment.filter(|c| !c.is_empty()) {
                    let prev = if is_map {
                        pairs.last().map(|p: &Pair| p.value.unwrap_or(p.key))
                    } else {
                        values.last().copied()
                    };
                    if let Some(prev) = prev {
                        let node = &mut ctx.nodes[prev];
                        if !node.comment.is_empty() {
                            append(&mut node.comment, &"\n".into());
                        }
                        append(&mut node.comment, &c);
                    }
                    props.comment =
                        slice(&props.comment, c.len_utf16() + 1, props.comment.len_utf16());
                }
            }
        }
        if !is_map && it.sep.is_none() && props.found.is_none() {
            let value_node = if let Some(value) = value {
                compose_node(ctx, value, &props)
            } else {
                compose_empty_node(ctx, props.end, sep, &props)
            };
            values.push(value_node);
            let range = ctx.nodes[value_node].range;
            offset = range[2];
            if is_block(value) {
                ctx.error([range[0], range[1]], "BLOCK_IN_FLOW", BLOCK_MSG);
            }
        } else {
            ctx.at_key = true;
            let key_start = props.end;
            let key_node = if let Some(key) = key {
                compose_node(ctx, key, &props)
            } else {
                compose_empty_node(ctx, key_start, &it.start, &props)
            };
            if is_block(key) {
                let r = ctx.nodes[key_node].range;
                ctx.error([r[0], r[1]], "BLOCK_IN_FLOW", BLOCK_MSG);
            }
            ctx.at_key = false;
            let vp = resolve_props(
                ctx,
                sep,
                PropOptions {
                    flow: Some(fc_name),
                    indicator: MapValueInd,
                    next: value,
                    offset: ctx.nodes[key_node].range[2],
                    parent_indent: fc.indent.unwrap(),
                    start_on_newline: false,
                },
            );
            if let Some(found) = &vp.found {
                if !is_map && props.found.is_none() {
                    for st in sep {
                        if st.offset == found.offset && st.kind == found.kind {
                            break;
                        }
                        if st.kind == Newline {
                            ctx.token_error(
                                st,
                                "MULTILINE_IMPLICIT_KEY",
                                "Implicit keys of flow sequence pairs need to be on a single line",
                            );
                            break;
                        }
                    }
                    if found.offset > props.start + 1024 {
                        ctx.token_error(found,"KEY_OVER_1024_CHARS","The : indicator must be at most 1024 chars after the start of an implicit flow sequence key");
                    }
                }
            } else if let Some(value) = value {
                if value
                    .source
                    .as_ref()
                    .is_some_and(|s| s.units().first() == Some(&58))
                {
                    ctx.token_error(
                        value,
                        "MISSING_CHAR",
                        format!("Missing space after : in {fc_name}"),
                    );
                } else {
                    ctx.at(
                        vp.start,
                        "MISSING_CHAR",
                        format!("Missing , or : between {fc_name} items"),
                    );
                }
            }
            let value_node = if let Some(value) = value {
                Some(compose_node(ctx, value, &vp))
            } else if vp.found.is_some() {
                Some(compose_empty_node(ctx, vp.end, sep, &vp))
            } else {
                None
            };
            if let Some(id) = value_node {
                if is_block(value) {
                    let r = ctx.nodes[id].range;
                    ctx.error([r[0], r[1]], "BLOCK_IN_FLOW", BLOCK_MSG);
                }
            } else if !vp.comment.is_empty() {
                let node = &mut ctx.nodes[key_node];
                if !node.comment.is_empty() {
                    append(&mut node.comment, &"\n".into());
                }
                append(&mut node.comment, &vp.comment);
            }
            let pair = Pair {
                key: key_node,
                value: value_node,
            };
            if is_map {
                if map_includes(ctx, &pairs, key_node) {
                    ctx.at(key_start, "DUPLICATE_KEY", "Map keys must be unique");
                }
                pairs.push(pair);
            } else {
                let end_range = ctx.nodes[value_node.unwrap_or(key_node)].range;
                let mut map = Node::new(NodeKind::Map(vec![pair]));
                map.flow = true;
                map.range = [ctx.nodes[key_node].range[0], end_range[1], end_range[2]];
                values.push(ctx.add(map));
            }
            offset = value_node.map_or(vp.end, |id| ctx.nodes[id].range[2]);
        }
    }
    let expected_end = if is_map { "}" } else { "]" };
    let end = fc.end.as_deref().unwrap_or_default();
    let ce = end.first();
    let mut ee = end.get(1..).unwrap_or_default();
    let mut ce_pos = offset;
    if ce.is_some_and(|ce| {
        ce.source
            .as_ref()
            .is_some_and(|s| s.equals_str(expected_end))
    }) {
        let ce = ce.unwrap();
        ce_pos = ce.offset + ce.source.as_ref().unwrap().len_utf16();
    } else {
        let name = if is_map { "Flow map" } else { "Flow sequence" };
        let msg = if at_root {
            format!("{name} must end with a {expected_end}")
        } else {
            format!(
                "{name} in block collection must be sufficiently indented and end with a {expected_end}"
            )
        };
        ctx.at(
            offset,
            if at_root {
                "MISSING_CHAR"
            } else {
                "BAD_INDENT"
            },
            msg,
        );
        if ce.is_some_and(|c| c.source.as_ref().unwrap().len_utf16() != 1) {
            ee = end;
        }
    }
    if !ee.is_empty() {
        let end = resolve_end(ctx, ee, ce_pos, true);
        if !end.comment.is_empty() {
            if !coll.comment.is_empty() {
                append(&mut coll.comment, &"\n".into());
            }
            append(&mut coll.comment, &end.comment);
        }
        coll.range = [fc.offset, ce_pos, end.offset];
    } else {
        coll.range = [fc.offset, ce_pos, ce_pos];
    }
    coll.kind = if is_map {
        NodeKind::Map(pairs)
    } else {
        NodeKind::Seq(values)
    };
    ctx.add(coll)
}
