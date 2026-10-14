// Parsing subset of yaml@2.9.0 dist/nodes/{Node,Scalar,Pair,YAMLMap,YAMLSeq,identity}.js (ISC).
use pilotdeck_protocol::JsString;
pub type NodeId = usize;
#[derive(Clone, Debug)]
pub enum NodeKind {
    Scalar(ScalarValue),
    Alias(JsString),
    Map(Vec<Pair>),
    Seq(Vec<NodeId>),
    Set(Vec<Pair>),
    OMap(Vec<Pair>),
    Pairs(Vec<Pair>),
}

mod stringify_key;
pub(crate) mod to_js;

mod fold_flow_lines;

mod add_pair_to_js_map;
mod alias;

mod node;
mod pair;
mod scalar;
pub use node::Node;
pub use pair::Pair;
pub use scalar::ScalarValue;

pub(crate) mod key;
