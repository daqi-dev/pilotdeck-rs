// Port of yaml@2.9.0 dist/nodes/Node.js (ISC), parse metadata.
use super::NodeKind;
use crate::parse::cst::Token;
use pilotdeck_protocol::JsString;
#[derive(Clone, Debug)]
pub struct Node {
    pub kind: NodeKind,
    pub range: [usize; 3],
    pub source: std::sync::Arc<JsString>,
    pub style: &'static str,
    pub format: Option<&'static str>,
    pub flow: bool,
    pub space_before: bool,
    metadata: Option<Box<NodeMetadata>>,
}
/// Optional node decoration. Plain nodes do not allocate this storage.
#[derive(Clone, Debug, Default)]
pub struct NodeMetadata {
    pub anchor: Option<JsString>,
    pub tag: Option<JsString>,
    pub comment: JsString,
    pub comment_before: JsString,
    pub src_token: Option<std::sync::Arc<Token>>,
}
impl std::ops::Deref for Node {
    type Target = NodeMetadata;
    fn deref(&self) -> &NodeMetadata {
        static EMPTY: std::sync::LazyLock<NodeMetadata> =
            std::sync::LazyLock::new(NodeMetadata::default);
        self.metadata.as_deref().unwrap_or(&EMPTY)
    }
}
impl std::ops::DerefMut for Node {
    fn deref_mut(&mut self) -> &mut NodeMetadata {
        self.metadata.get_or_insert_with(Default::default)
    }
}
impl Node {
    pub(crate) fn new(kind: NodeKind) -> Self {
        Self {
            kind,
            range: [0; 3],
            source: Default::default(),
            style: "",
            format: None,
            flow: false,
            space_before: false,
            metadata: None,
        }
    }
    pub(crate) fn is_scalar(&self) -> bool {
        matches!(self.kind, NodeKind::Scalar(_))
    }
}
