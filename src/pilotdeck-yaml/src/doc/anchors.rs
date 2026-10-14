// Port of yaml@2.9.0 dist/doc/anchors.js, nodes/{Alias,toJS}.js and visit.js (ISC).
// Arena traversal and anchor registration preserve the JS pre-order and identity.
use crate::nodes::to_js::Context;
use crate::{
    nodes::{NodeId, NodeKind, to_js::Anchor},
    value::Value,
};
impl Context<'_> {
    pub(crate) fn visit_order(&mut self, id: NodeId) {
        self.order.push(id);
        match &self.doc.nodes[id].kind {
            NodeKind::Seq(items) => {
                for item in items {
                    self.visit_order(*item);
                }
            }
            NodeKind::Map(pairs)
            | NodeKind::Set(pairs)
            | NodeKind::OMap(pairs)
            | NodeKind::Pairs(pairs) => {
                for p in pairs {
                    self.visit_order(p.key);
                    if let Some(v) = p.value {
                        self.visit_order(v);
                    }
                }
            }
            _ => (),
        }
    }
    pub(crate) fn anchored(&mut self, id: NodeId, value: &Value) {
        if self.doc.nodes[id]
            .anchor
            .as_ref()
            .is_some_and(|s| !s.is_empty())
        {
            self.anchors.insert(
                id,
                Anchor {
                    count: 1,
                    alias_count: 0,
                    res: value.clone(),
                },
            );
        }
    }
}

// anchorIsValid from dist/doc/anchors.js; used while coercing collection keys.
pub(crate) fn anchor_is_valid(
    anchor: &pilotdeck_protocol::JsString,
) -> Result<(), crate::value::ConversionError> {
    if anchor
        .units()
        .iter()
        .any(|c| *c <= 0x19 || crate::string::is_ws(*c) || matches!(c, 44 | 91 | 93 | 123 | 125))
    {
        return Err(crate::value::ConversionError::new(
            "Error",
            crate::string::message(
                "Anchor must not contain whitespace or control characters: ",
                &crate::string::json(anchor),
            ),
        ));
    }
    Ok(())
}
