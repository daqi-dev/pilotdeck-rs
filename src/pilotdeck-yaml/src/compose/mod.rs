// Shared context of yaml@2.9.0 dist/compose/{compose-doc,composer}.js (ISC).
use crate::parse::cst::Token;
use crate::{
    ParseOptions,
    doc::directives::Directives,
    errors::{Diagnostic, token_pos},
    nodes::{Node, NodeId},
    schema::Schema,
};
use pilotdeck_protocol::JsString;
mod compose_collection;
mod compose_doc;
mod compose_node;
mod compose_scalar;
pub(crate) mod composer;
mod resolve_block_map;
pub(crate) mod resolve_block_scalar;
mod resolve_block_seq;
mod resolve_end;
mod resolve_flow_collection;
pub(crate) mod resolve_flow_scalar;
mod resolve_props;
mod util_contains_newline;
mod util_empty_scalar_position;
mod util_map_includes;
#[derive(Clone, Debug)]
pub(crate) struct Context {
    pub at_key: bool,
    pub at_root: bool,
    pub directives: Directives,
    pub options: ParseOptions,
    pub schema: Schema,
    pub nodes: Vec<Node>,
    pub errors: Vec<Diagnostic>,
    pub warnings: Vec<Diagnostic>,
}
impl Context {
    pub fn error(&mut self, pos: [usize; 2], code: &'static str, msg: impl Into<JsString>) {
        self.errors.push(Diagnostic::new(pos, code, msg, false));
    }
    pub fn at(&mut self, pos: usize, code: &'static str, msg: impl Into<JsString>) {
        self.error([pos, pos + 1], code, msg);
    }
    pub fn token_error(&mut self, t: &Token, code: &'static str, msg: impl Into<JsString>) {
        self.error(token_pos(t), code, msg);
    }
    pub fn warn(&mut self, pos: [usize; 2], code: &'static str, msg: impl Into<JsString>) {
        self.warnings.push(Diagnostic::new(pos, code, msg, true));
    }
    pub fn add(&mut self, node: Node) -> NodeId {
        let id = self.nodes.len();
        self.nodes.push(node);
        id
    }
}
