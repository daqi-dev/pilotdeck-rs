// Port of yaml@2.9.0 dist/doc/Document.js (ISC), parse/toJSON subset.
pub mod directives;
use crate::{
    ParseOptions,
    errors::Diagnostic,
    nodes::{Node, NodeId},
    parse::line_counter::LineCounter,
    schema::Schema,
    value::{ConversionError, ValueGraph},
};
use pilotdeck_protocol::JsString;
#[derive(Clone, Debug)]
pub struct Document {
    pub contents: Option<NodeId>,
    pub nodes: Vec<Node>,
    pub directives: directives::Directives,
    pub errors: Vec<Diagnostic>,
    pub warnings: Vec<Diagnostic>,
    pub range: [usize; 3],
    pub comment: JsString,
    pub comment_before: JsString,
    pub line_counter: std::sync::Arc<LineCounter>,
    pub(crate) options: ParseOptions,
    pub(crate) schema: Schema,
}
impl Document {
    pub fn to_json(&self) -> Result<ValueGraph, ConversionError> {
        crate::stack::with_large_stack(self.nodes.len() > 128, || crate::nodes::to_js::to_js(self))
    }
}
/// Empty-stream metadata returned by npm yaml's parseAllDocuments.
#[derive(Clone, Debug)]
pub struct StreamInfo {
    pub comment: JsString,
    pub directives: directives::Directives,
    pub errors: Vec<Diagnostic>,
    pub warnings: Vec<Diagnostic>,
}
#[derive(Clone, Debug)]
pub struct Documents {
    pub documents: Vec<Document>,
    pub stream_info: Option<StreamInfo>,
}
impl std::ops::Deref for Documents {
    type Target = [Document];
    fn deref(&self) -> &Self::Target {
        &self.documents
    }
}

pub(crate) mod anchors;
