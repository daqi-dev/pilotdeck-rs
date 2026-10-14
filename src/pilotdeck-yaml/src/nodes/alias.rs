// Port of yaml@2.9.0 dist/nodes/Alias.js (ISC); parse/conversion operations.
use crate::nodes::to_js::Context;
use crate::{
    nodes::{NodeId, NodeKind},
    string::message,
    value::{ConversionError, Value},
};
use pilotdeck_protocol::JsString;
impl Context<'_> {
    pub(crate) fn resolve_alias(&self, id: NodeId) -> Option<NodeId> {
        if matches!(self.doc.nodes[id].kind, NodeKind::Alias(_)) {
            self.alias_sources.get(&id).copied()
        } else {
            Some(id)
        }
    }
    pub(crate) fn alias(&mut self, id: NodeId, name: &JsString) -> Result<Value, ConversionError> {
        let Some(source) = self.resolve_alias(id) else {
            return Err(ConversionError::new(
                "ReferenceError",
                message(
                    "Unresolved alias (the anchor must be set before the alias): ",
                    name,
                ),
            ));
        };
        if !self.anchors.contains_key(&source) {
            self.convert(Some(source))?;
        }
        let Some(data) = self.anchors.get_mut(&source) else {
            return Err(ConversionError::new(
                "ReferenceError",
                "This should not happen: Alias anchor was not resolved?",
            ));
        };
        data.count += 1;
        if data.alias_count == 0 {
            let count = self.alias_count(source)?;
            self.anchors.get_mut(&source).unwrap().alias_count = count;
        }
        let data = self.anchors.get(&source).unwrap();
        if data.count.saturating_mul(data.alias_count) > 100 {
            return Err(ConversionError::new(
                "ReferenceError",
                "Excessive alias count indicates a resource exhaustion attack",
            ));
        }
        Ok(data.res.clone())
    }
    pub(crate) fn alias_count(&self, id: NodeId) -> Result<usize, ConversionError> {
        let mut maximum = 0;
        let mut pending = vec![(id, 0)];
        while let Some((id, depth)) = pending.pop() {
            if depth >= crate::stack::CONVERSION_DEPTH {
                return Err(crate::stack::conversion_exhausted());
            }
            let count = match &self.doc.nodes[id].kind {
                NodeKind::Alias(_) => self
                    .resolve_alias(id)
                    .and_then(|id| self.anchors.get(&id))
                    .map_or(0, |a| a.count.saturating_mul(a.alias_count)),
                NodeKind::Seq(items) => {
                    pending.extend(items.iter().map(|id| (*id, depth + 1)));
                    0
                }
                NodeKind::Map(pairs)
                | NodeKind::Set(pairs)
                | NodeKind::OMap(pairs)
                | NodeKind::Pairs(pairs) => {
                    for p in pairs {
                        pending.push((p.key, depth + 1));
                        if let Some(v) = p.value {
                            pending.push((v, depth + 1));
                        } else {
                            maximum = maximum.max(1);
                        }
                    }
                    0
                }
                _ => 1,
            };
            maximum = maximum.max(count);
        }
        Ok(maximum)
    }
}
