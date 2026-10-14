// Port of yaml@2.9.0 dist/doc/anchors.js, nodes/{Alias,toJS}.js and visit.js (ISC).
// Arena traversal and anchor registration preserve the JS pre-order and identity.
use crate::nodes::to_js::Context;
use crate::{
    nodes::{NodeId, NodeKind, to_js::Anchor},
    value::Value,
};
impl Context<'_> {
    pub(crate) fn visit_order(&mut self, id: NodeId) -> Result<(), crate::value::ConversionError> {
        let mut pending = vec![(id, 0)];
        let mut names = std::collections::HashMap::new();
        let mut visited = std::collections::HashSet::new();
        while let Some((id, depth)) = pending.pop() {
            if depth >= crate::stack::CONVERSION_DEPTH {
                return Err(crate::stack::conversion_exhausted());
            }
            // Public arenas may share a node. Alias.resolve stops at its first
            // occurrence in the original pre-order traversal.
            if let NodeKind::Alias(name) = &self.doc.nodes[id].kind
                && visited.insert(id)
                && let Some(&source) = names.get(name)
            {
                self.alias_sources.insert(id, source);
            }
            if let Some(name) = &self.doc.nodes[id].anchor {
                names.insert(name.clone(), id);
            }
            match &self.doc.nodes[id].kind {
                NodeKind::Seq(items) => {
                    pending.extend(items.iter().rev().map(|id| (*id, depth + 1)))
                }
                NodeKind::Map(pairs)
                | NodeKind::Set(pairs)
                | NodeKind::OMap(pairs)
                | NodeKind::Pairs(pairs) => {
                    for p in pairs.iter().rev() {
                        if let Some(v) = p.value {
                            pending.push((v, depth + 1));
                        }
                        pending.push((p.key, depth + 1));
                    }
                }
                _ => (),
            }
        }
        Ok(())
    }
    pub(crate) fn anchored(&mut self, id: NodeId, value: &Value) {
        if self.doc.nodes[id]
            .anchor
            .as_ref()
            .is_some_and(|s| !s.is_empty())
        {
            self.anchor_names
                .insert(self.doc.nodes[id].anchor.as_ref().unwrap().clone());
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
