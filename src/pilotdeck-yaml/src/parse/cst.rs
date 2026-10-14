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
#[derive(Clone)]
pub struct Token {
    pub kind: TokenType,
    pub offset: usize,
    pub indent: Option<usize>,
    pub source: Option<JsString>,
    pub start: Vec<Token>,
    pub flow_start: Option<std::sync::Arc<Token>>,
    pub items: Vec<CollectionItem>,
    pub props: Vec<Token>,
    pub end: Option<Vec<Token>>,
    pub value: Option<std::sync::Arc<Token>>,
    pub message: Option<JsString>,
}

// Derived Eq/Debug would traverse Arc children recursively too. Equality keeps
// every CST field; Debug prints the current frame without walking descendants.
impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Token")
            .field("kind", &self.kind)
            .field("offset", &self.offset)
            .field("indent", &self.indent)
            .field("source", &self.source)
            .field("items", &self.items.len())
            .field("message", &self.message)
            .finish_non_exhaustive()
    }
}
impl Eq for Token {}
impl PartialEq for Token {
    fn eq(&self, other: &Self) -> bool {
        fn list<'a>(
            pending: &mut Vec<(&'a Token, &'a Token)>,
            a: &'a [Token],
            b: &'a [Token],
        ) -> bool {
            if a.len() != b.len() {
                return false;
            }
            pending.extend(a.iter().zip(b));
            true
        }
        fn optional<'a>(
            pending: &mut Vec<(&'a Token, &'a Token)>,
            a: Option<&'a Token>,
            b: Option<&'a Token>,
        ) -> bool {
            match (a, b) {
                (Some(a), Some(b)) => {
                    pending.push((a, b));
                    true
                }
                (None, None) => true,
                _ => false,
            }
        }
        let mut pending = vec![(self, other)];
        while let Some((a, b)) = pending.pop() {
            if a.kind != b.kind
                || a.offset != b.offset
                || a.indent != b.indent
                || a.source != b.source
                || a.message != b.message
                || a.items.len() != b.items.len()
                || a.end.is_some() != b.end.is_some()
                || !list(&mut pending, &a.start, &b.start)
                || !list(&mut pending, &a.props, &b.props)
                || !list(
                    &mut pending,
                    a.end.as_deref().unwrap_or_default(),
                    b.end.as_deref().unwrap_or_default(),
                )
                || !optional(
                    &mut pending,
                    a.flow_start.as_deref(),
                    b.flow_start.as_deref(),
                )
                || !optional(&mut pending, a.value.as_deref(), b.value.as_deref())
            {
                return false;
            }
            for (a, b) in a.items.iter().zip(&b.items) {
                if a.explicit_key != b.explicit_key
                    || a.key.is_some() != b.key.is_some()
                    || a.sep.is_some() != b.sep.is_some()
                    || a.key.as_ref().is_some_and(Option::is_some)
                        != b.key.as_ref().is_some_and(Option::is_some)
                    || !list(&mut pending, &a.start, &b.start)
                    || !list(
                        &mut pending,
                        a.sep.as_deref().unwrap_or_default(),
                        b.sep.as_deref().unwrap_or_default(),
                    )
                    || !optional(
                        &mut pending,
                        a.key.as_ref().and_then(|k| k.as_deref()),
                        b.key.as_ref().and_then(|k| k.as_deref()),
                    )
                    || !optional(&mut pending, a.value.as_deref(), b.value.as_deref())
                {
                    return false;
                }
            }
        }
        true
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CollectionItem {
    pub start: Vec<Token>,
    /// None = missing property; Some(None) = explicit null key.
    pub key: Option<Option<std::sync::Arc<Token>>>,
    pub sep: Option<Vec<Token>>,
    pub value: Option<std::sync::Arc<Token>>,
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

// CST children are shared just as JS srcToken references are shared. Cloning a
// collection does not recursively copy its subtree. Drain every owned edge
// before Rust drops fields, including when a partially built Parser is dropped.
impl Drop for Token {
    fn drop(&mut self) {
        fn detach(token: &mut Token, pending: &mut Vec<Token>) {
            pending.append(&mut token.start);
            pending.append(&mut token.props);
            if let Some(mut end) = token.end.take() {
                pending.append(&mut end);
            }
            for child in [token.flow_start.take(), token.value.take()]
                .into_iter()
                .flatten()
            {
                if let Some(child) = std::sync::Arc::into_inner(child) {
                    pending.push(child);
                }
            }
            for mut item in std::mem::take(&mut token.items) {
                pending.append(&mut item.start);
                if let Some(mut sep) = item.sep.take() {
                    pending.append(&mut sep);
                }
                for child in [item.key.take().flatten(), item.value.take()]
                    .into_iter()
                    .flatten()
                {
                    if let Some(child) = std::sync::Arc::into_inner(child) {
                        pending.push(child);
                    }
                }
            }
        }
        let mut pending = Vec::new();
        detach(self, &mut pending);
        while let Some(mut token) = pending.pop() {
            detach(&mut token, &mut pending);
        }
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
