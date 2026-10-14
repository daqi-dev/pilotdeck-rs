// Port of yaml@2.9.0 dist/compose/util-map-includes.js (ISC).
use super::Context;
use crate::nodes::{NodeId, NodeKind, Pair, ScalarValue};
pub(super) fn map_includes(ctx: &Context, items: &[Pair], search: NodeId) -> bool {
    items.iter().any(|p| {
        p.key == search
            || match (&ctx.nodes[p.key].kind, &ctx.nodes[search].kind) {
                (NodeKind::Scalar(a), NodeKind::Scalar(b)) => match (a, b) {
                    (ScalarValue::Merge, _) | (_, ScalarValue::Merge) => false,
                    (ScalarValue::Timestamp(_) | ScalarValue::Binary(_), _) => false,
                    _ => a == b,
                },
                _ => false,
            }
    })
}

#[derive(Hash, PartialEq, Eq)]
enum PrimitiveKey {
    Null,
    Bool(bool),
    Number(u64),
    String(std::sync::Arc<pilotdeck_protocol::JsString>),
}
pub(super) struct MapKeys(Option<std::collections::HashSet<PrimitiveKey>>);
impl MapKeys {
    pub fn new(count: usize) -> Self {
        Self((count >= 16).then(|| std::collections::HashSet::with_capacity(count)))
    }
    pub fn includes(&mut self, ctx: &Context, items: &[Pair], search: NodeId) -> bool {
        let Some(keys) = &mut self.0 else {
            return map_includes(ctx, items, search);
        };
        let key = match &ctx.nodes[search].kind {
            NodeKind::Scalar(ScalarValue::Null) => PrimitiveKey::Null,
            NodeKind::Scalar(ScalarValue::Bool(value)) => PrimitiveKey::Bool(*value),
            NodeKind::Scalar(ScalarValue::String(value)) => PrimitiveKey::String(value.clone()),
            NodeKind::Scalar(ScalarValue::Number(value)) if !value.is_nan() => {
                PrimitiveKey::Number(if *value == 0.0 { 0 } else { value.to_bits() })
            }
            _ => return false,
        };
        !keys.insert(key)
    }
}
