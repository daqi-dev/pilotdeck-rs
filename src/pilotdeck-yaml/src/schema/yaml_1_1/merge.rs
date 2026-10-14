// Port of yaml@2.9.0 dist/schema/yaml-1.1/merge.js (ISC); parse/conversion operations.
use crate::nodes::to_js::Context;
use crate::{
    nodes::{NodeId, NodeKind},
    value::{Container, ConversionError, Value},
};
impl Context<'_> {
    pub(crate) fn add_merge(
        &mut self,
        index: usize,
        value: Option<NodeId>,
    ) -> Result<(), ConversionError> {
        let source = value.and_then(|v| self.resolve_alias(v));
        if let Some(source) = source
            && let NodeKind::Seq(items) = self.doc.nodes[source].kind.clone()
        {
            for item in items {
                self.merge_value(index, Some(item))?;
            }
            return Ok(());
        }
        self.merge_value(index, source)
    }
    pub(crate) fn merge_value(
        &mut self,
        index: usize,
        value: Option<NodeId>,
    ) -> Result<(), ConversionError> {
        let source = value.and_then(|v| self.resolve_alias(v));
        let Some(source) = source else {
            return Err(ConversionError::new(
                "Error",
                "Merge sources must be maps or map aliases",
            ));
        };
        let pairs = match &self.doc.nodes[source].kind {
            NodeKind::Map(p) | NodeKind::Set(p) => p.clone(),
            _ => {
                return Err(ConversionError::new(
                    "Error",
                    "Merge sources must be maps or map aliases",
                ));
            }
        };
        // JS recurses until RangeError for a cyclic merge. Detect that cycle
        // explicitly because Rust cannot recover from native stack overflow.
        if self.merge_stack.contains(&source)
            || self.depth + self.merge_stack.len() >= crate::stack::CONVERSION_DEPTH
        {
            return Err(ConversionError::new(
                "RangeError",
                "Maximum call stack size exceeded",
            ));
        }
        self.merge_stack.push(source);
        let map = self.allocate(Container::Map(Vec::new()));
        let Value::Ref(map_id) = map else {
            unreachable!()
        };
        for pair in &pairs {
            self.add_pair(map_id, pair)?;
        }
        let Container::Map(entries) = self.nodes[map_id].clone() else {
            unreachable!()
        };
        for (key, value) in entries {
            let key = if matches!(self.nodes[index], Container::Object(_)) {
                Value::String(std::sync::Arc::new(self.js_string(&key)))
            } else {
                key
            };
            self.insert(index, key, value, true);
        }
        self.merge_stack.pop();
        Ok(())
    }
}
