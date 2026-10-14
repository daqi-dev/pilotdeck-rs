// Port of yaml@2.9.0 dist/compose/resolve-block-map.js (ISC).
use super::{
    Context,
    compose_node::{compose_empty_node, compose_node},
    resolve_props::{PropOptions, resolve_props},
    util_contains_newline::contains_newline,
    util_map_includes::MapKeys,
};
use crate::{
    nodes::{Node, NodeId, NodeKind, Pair},
    parse::cst::{CollectionItem, Token, TokenType::*},
    string::concat,
};
const START_COL_MSG: &str = "All mapping items must start at the same column";
pub(super) fn resolve_block_map(ctx: &mut Context, bm: &Token) -> NodeId {
    resolve_block_map_items(ctx, bm, bm.items.iter().cloned())
}
pub(super) fn resolve_block_map_items(
    ctx: &mut Context,
    bm: &Token,
    input: impl IntoIterator<Item = CollectionItem>,
) -> NodeId {
    let input = input.into_iter();
    let mut keys = MapKeys::new(input.size_hint().0);
    let mut collection_comment = Vec::new();
    let mut map = Node::new(NodeKind::Map(Vec::new()));
    let mut items = Vec::new();
    ctx.at_root = false;
    let mut offset = bm.offset;
    let mut comment_end = None;
    for it in input {
        let key = it.key.as_ref().and_then(|k| k.as_deref());
        let sep = it.sep.as_deref().unwrap_or_default();
        let value = it.value.as_deref();
        let kp = resolve_props(
            ctx,
            &it.start,
            PropOptions {
                flow: None,
                indicator: ExplicitKeyInd,
                next: key.or_else(|| sep.first()),
                offset,
                parent_indent: bm.indent.unwrap(),
                start_on_newline: true,
            },
        );
        let implicit = kp.found.is_none();
        if implicit {
            if let Some(key) = key {
                if key.kind == BlockSeq {
                    ctx.at(
                        offset,
                        "BLOCK_AS_IMPLICIT_KEY",
                        "A block sequence may not be used as an implicit map key",
                    );
                } else if key.indent.is_some() && key.indent != bm.indent {
                    ctx.at(offset, "BAD_INDENT", START_COL_MSG);
                }
            }
            if kp.anchor.is_none() && kp.tag.is_none() && it.sep.is_none() {
                comment_end = Some(kp.end);
                if !kp.comment.is_empty() {
                    if !collection_comment.is_empty() {
                        collection_comment.push(10);
                    }
                    collection_comment.extend_from_slice(kp.comment.units());
                }
                continue;
            }
            if kp.newline_after_prop.is_some() || contains_newline(key) {
                ctx.token_error(
                    key.unwrap_or_else(|| it.start.last().unwrap()),
                    "MULTILINE_IMPLICIT_KEY",
                    "Implicit keys need to be on a single line",
                );
            }
        } else if kp.found.as_ref().unwrap().indent != bm.indent {
            ctx.at(offset, "BAD_INDENT", START_COL_MSG);
        }
        ctx.at_key = true;
        let key_start = kp.end;
        let key_node = if let Some(key) = key {
            compose_node(ctx, key, &kp)
        } else {
            compose_empty_node(ctx, key_start, &it.start, &kp)
        };
        ctx.at_key = false;
        if keys.includes(ctx, &items, key_node) {
            ctx.at(key_start, "DUPLICATE_KEY", "Map keys must be unique");
        }
        let vp = resolve_props(
            ctx,
            sep,
            PropOptions {
                flow: None,
                indicator: MapValueInd,
                next: value,
                offset: ctx.nodes[key_node].range[2],
                parent_indent: bm.indent.unwrap(),
                start_on_newline: key.is_none_or(|k| k.kind == BlockScalar),
            },
        );
        offset = vp.end;
        if let Some(found) = &vp.found {
            if implicit {
                if value.is_some_and(|v| v.kind == BlockMap) && !vp.has_newline {
                    ctx.at(
                        offset,
                        "BLOCK_AS_IMPLICIT_KEY",
                        "Nested mappings are not allowed in compact mappings",
                    );
                }
                if found.offset > kp.start + 1024 {
                    let r = ctx.nodes[key_node].range;
                    ctx.error([r[0],r[1]],"KEY_OVER_1024_CHARS","The : indicator must be at most 1024 chars after the start of an implicit block mapping key");
                }
            }
            let value_node = if let Some(value) = value {
                compose_node(ctx, value, &vp)
            } else {
                compose_empty_node(ctx, offset, sep, &vp)
            };
            offset = ctx.nodes[value_node].range[2];
            items.push(Pair {
                key: key_node,
                value: Some(value_node),
            });
        } else {
            if implicit {
                let r = ctx.nodes[key_node].range;
                ctx.error(
                    [r[0], r[1]],
                    "MISSING_CHAR",
                    "Implicit map keys need to be followed by map values",
                );
            }
            if !vp.comment.is_empty() {
                let node = &mut ctx.nodes[key_node];
                node.comment = if node.comment.is_empty() {
                    vp.comment
                } else {
                    concat(&[&node.comment, &"\n".into(), &vp.comment])
                };
            }
            items.push(Pair {
                key: key_node,
                value: None,
            });
        }
    }
    if let Some(c) = comment_end.filter(|c| *c > 0 && *c < offset) {
        ctx.at(c, "IMPOSSIBLE", "Map comment with trailing content");
    }
    if !collection_comment.is_empty() {
        map.comment = pilotdeck_protocol::JsString::from_utf16(collection_comment);
    }
    map.kind = NodeKind::Map(items);
    map.range = [bm.offset, offset, comment_end.unwrap_or(offset)];
    ctx.add(map)
}
