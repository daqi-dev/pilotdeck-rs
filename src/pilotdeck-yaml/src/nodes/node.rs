// Port of yaml@2.9.0 dist/nodes/Node.js (ISC), parse metadata.
use super::NodeKind;
use crate::parse::cst::Token;
use pilotdeck_protocol::JsString;
#[derive(Clone, Debug)]
pub struct Node {
    pub kind: NodeKind,
    pub range: [usize; 3],
    pub anchor: Option<JsString>,
    pub tag: Option<JsString>,
    pub source: JsString,
    pub style: &'static str,
    pub format: Option<&'static str>,
    pub flow: bool,
    pub space_before: bool,
    pub comment: JsString,
    pub comment_before: JsString,
    pub src_token: Option<Token>,
}
impl Node {
    pub(crate) fn new(kind: NodeKind) -> Self {
        Self {
            kind,
            range: [0; 3],
            anchor: None,
            tag: None,
            source: JsString::default(),
            style: "",
            format: None,
            flow: false,
            space_before: false,
            comment: JsString::default(),
            comment_before: JsString::default(),
            src_token: None,
        }
    }
    pub(crate) fn is_scalar(&self) -> bool {
        matches!(self.kind, NodeKind::Scalar(_))
    }
}
