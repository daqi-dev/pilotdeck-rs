// Port of yaml@2.9.0 dist/nodes/Pair.js (ISC), parse representation.
use super::NodeId;
#[derive(Clone, Debug)]
pub struct Pair {
    pub key: NodeId,
    pub value: Option<NodeId>,
}
