// Port of yaml@2.9.0 dist/parse/cst.js (ISC).
use pilotdeck_protocol::JsString;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenType {
    ByteOrderMark,
    DocMode,
    FlowErrorEnd,
    Scalar,
    DocStart,
    DocEnd,
    Newline,
    SeqItemInd,
    ExplicitKeyInd,
    MapValueInd,
    FlowMapStart,
    FlowMapEnd,
    FlowSeqStart,
    FlowSeqEnd,
    Comma,
    Space,
    Comment,
    DirectiveLine,
    Alias,
    Anchor,
    Tag,
    SingleQuotedScalar,
    DoubleQuotedScalar,
    BlockScalarHeader,
    Directive,
    Document,
    Error,
    BlockScalar,
    BlockMap,
    BlockSeq,
    FlowCollection,
}
use TokenType::*;
impl TokenType {
    pub fn as_str(self) -> &'static str {
        match self {
            ByteOrderMark => "byte-order-mark",
            DocMode => "doc-mode",
            FlowErrorEnd => "flow-error-end",
            Scalar => "scalar",
            DocStart => "doc-start",
            DocEnd => "doc-end",
            Newline => "newline",
            SeqItemInd => "seq-item-ind",
            ExplicitKeyInd => "explicit-key-ind",
            MapValueInd => "map-value-ind",
            FlowMapStart => "flow-map-start",
            FlowMapEnd => "flow-map-end",
            FlowSeqStart => "flow-seq-start",
            FlowSeqEnd => "flow-seq-end",
            Comma => "comma",
            Space => "space",
            Comment => "comment",
            DirectiveLine => "directive-line",
            Alias => "alias",
            Anchor => "anchor",
            Tag => "tag",
            SingleQuotedScalar => "single-quoted-scalar",
            DoubleQuotedScalar => "double-quoted-scalar",
            BlockScalarHeader => "block-scalar-header",
            Directive => "directive",
            Document => "document",
            Error => "error",
            BlockScalar => "block-scalar",
            BlockMap => "block-map",
            BlockSeq => "block-seq",
            FlowCollection => "flow-collection",
        }
    }
}
pub fn token_type(source: &JsString) -> Option<TokenType> {
    Some(match source.units() {
        [0xfeff] => ByteOrderMark,
        [2] => DocMode,
        [24] => FlowErrorEnd,
        [31] => Scalar,
        [45, 45, 45] => DocStart,
        [46, 46, 46] => DocEnd,
        [] | [10] | [13, 10] => Newline,
        [45] => SeqItemInd,
        [63] => ExplicitKeyInd,
        [58] => MapValueInd,
        [123] => FlowMapStart,
        [125] => FlowMapEnd,
        [91] => FlowSeqStart,
        [93] => FlowSeqEnd,
        [44] => Comma,
        [32 | 9, ..] => Space,
        [35, ..] => Comment,
        [37, ..] => DirectiveLine,
        [42, ..] => Alias,
        [38, ..] => Anchor,
        [33, ..] => Tag,
        [39, ..] => SingleQuotedScalar,
        [34, ..] => DoubleQuotedScalar,
        [124 | 62, ..] => BlockScalarHeader,
        _ => return None,
    })
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenType,
    pub offset: usize,
    pub indent: Option<usize>,
    pub source: Option<JsString>,
    pub start: Vec<Token>,
    pub flow_start: Option<Box<Token>>,
    pub items: Vec<CollectionItem>,
    pub props: Vec<Token>,
    pub end: Option<Vec<Token>>,
    pub value: Option<Box<Token>>,
    pub message: Option<JsString>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CollectionItem {
    pub start: Vec<Token>,
    /// None = missing property; Some(None) = explicit null key.
    pub key: Option<Option<Box<Token>>>,
    pub sep: Option<Vec<Token>>,
    pub value: Option<Box<Token>>,
    pub explicit_key: bool,
}
impl Token {
    pub(crate) fn new(kind: TokenType, offset: usize) -> Self {
        Self {
            kind,
            offset,
            indent: None,
            source: None,
            start: Vec::new(),
            flow_start: None,
            items: Vec::new(),
            props: Vec::new(),
            end: None,
            value: None,
            message: None,
        }
    }
    pub(crate) fn nesting_depth_exceeds(&self, limit: usize) -> bool {
        let mut stack = vec![(self, 0)];
        while let Some((token, depth)) = stack.pop() {
            let depth = depth + usize::from(token.is_collection());
            if depth > limit {
                return true;
            }
            if let Some(value) = token.value.as_deref() {
                stack.push((value, depth));
            }
            for item in &token.items {
                if let Some(Some(key)) = &item.key {
                    stack.push((key, depth));
                }
                if let Some(value) = item.value.as_deref() {
                    stack.push((value, depth));
                }
            }
        }
        false
    }
    pub fn is_collection(&self) -> bool {
        matches!(self.kind, BlockMap | BlockSeq | FlowCollection)
    }
    pub fn is_scalar(&self) -> bool {
        matches!(
            self.kind,
            Scalar | SingleQuotedScalar | DoubleQuotedScalar | BlockScalar
        )
    }
}

// prettyToken is a debugging helper; its result is also UTF-16.
pub fn pretty_token(token: &JsString) -> JsString {
    match token.units() {
        [0xfeff] => "<BOM>".into(),
        [2] => "<DOC>".into(),
        [24] => "<FLOW_END>".into(),
        [31] => "<SCALAR>".into(),
        _ => crate::string::json(token),
    }
}
