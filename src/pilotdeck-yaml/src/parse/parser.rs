// Port of yaml@2.9.0 dist/parse/parser.js (ISC).
// Handler-local ownership replaces JS references; put-back precedes every pop/step.
use super::cst::{CollectionItem, SmallVecPush, Token, TokenType, token_type};
use super::lexer::Lexer;
use super::line_counter::LineCounter;
use TokenType::*;
use pilotdeck_protocol::JsString;

fn includes_token(list: &[Token], kind: TokenType) -> bool {
    list.iter().any(|t| t.kind == kind)
}
fn find_non_empty_index(list: &[Token]) -> Option<usize> {
    list.iter()
        .position(|t| !matches!(t.kind, Space | Comment | Newline))
}
fn is_flow_token(token: Option<&Token>) -> bool {
    token.is_some_and(|t| {
        matches!(
            t.kind,
            Alias | Scalar | SingleQuotedScalar | DoubleQuotedScalar | FlowCollection
        )
    })
}
fn get_prev_props(parent: &mut Token) -> &mut Vec<Token> {
    match parent.kind {
        Document => &mut parent.start,
        BlockMap => {
            let it = parent.items.last_mut().unwrap();
            if let Some(ref mut sep) = it.sep {
                sep
            } else {
                &mut it.start
            }
        }
        BlockSeq => &mut parent.items.last_mut().unwrap().start,
        _ => &mut parent.start, // unused for flow-collection
    }
}
fn get_first_key_start_props(prev: &mut Vec<Token>) -> Vec<Token> {
    let mut i = prev
        .iter()
        .rposition(|t| {
            matches!(
                t.kind,
                DocStart | ExplicitKeyInd | MapValueInd | SeqItemInd | Newline
            )
        })
        .map_or(0, |i| i + 1);
    while prev.get(i).is_some_and(|t| t.kind == Space) {
        i += 1;
    }
    prev.split_off(i)
}
fn fix_flow_seq_items(fc: &mut Token) {
    if fc
        .flow_start
        .as_ref()
        .is_some_and(|t| t.kind == FlowSeqStart)
    {
        for it in &mut fc.items {
            if it.sep.is_some()
                && it.value.is_none()
                && !includes_token(&it.start, ExplicitKeyInd)
                && !includes_token(it.sep.as_ref().unwrap(), MapValueInd)
            {
                if let Some(Some(key)) = it.key.take() {
                    it.value = Some(key);
                }
                let sep = it.sep.take().unwrap();
                if is_flow_token(it.value.as_deref()) {
                    std::sync::Arc::make_mut(it.value.as_mut().unwrap())
                        .end
                        .get_or_insert_with(Vec::new)
                        .extend_small(sep);
                } else {
                    it.start.extend_small(sep);
                }
            }
        }
    }
}
fn item(start: Vec<Token>) -> CollectionItem {
    CollectionItem {
        start,
        ..Default::default()
    }
}
fn pair(start: Vec<Token>, key: Option<std::sync::Arc<Token>>, sep: Vec<Token>) -> CollectionItem {
    CollectionItem {
        start,
        key: Some(key),
        sep: Some(sep),
        ..Default::default()
    }
}

#[derive(Debug)]
pub struct Parser {
    record_lines: bool,
    at_new_line: bool,
    at_scalar: bool,
    indent: usize,
    offset: usize,
    on_key_line: bool,
    stack: Vec<Token>,
    source: std::sync::Arc<JsString>,
    kind: TokenType,
    lexer: Lexer<'static>,
    pub line_counter: LineCounter,
    output: Vec<Token>,
    stepping: bool,
    step_pending: bool,
}
impl Default for Parser {
    fn default() -> Self {
        Self {
            record_lines: true,
            at_new_line: true,
            at_scalar: false,
            indent: 0,
            offset: 0,
            on_key_line: false,
            stack: Vec::new(),
            source: Default::default(),
            kind: Error,
            lexer: Lexer::default(),
            line_counter: LineCounter::default(),
            output: Vec::new(),
            stepping: false,
            step_pending: false,
        }
    }
}
impl Parser {
    pub(crate) fn defer_lines(&mut self) {
        self.record_lines = false;
    }
    pub fn parse(&mut self, source: &JsString, incomplete: bool) -> Vec<Token> {
        if self.offset == 0 {
            self.line_counter.add_new_line(0);
        }
        for lexeme in self.lexer.lex(source, incomplete) {
            self.advance(lexeme);
        }
        if !incomplete {
            self.finish();
        }
        std::mem::take(&mut self.output)
    }
    pub fn next(&mut self, source: JsString) -> Vec<Token> {
        self.advance(source);
        std::mem::take(&mut self.output)
    }
    pub fn end(&mut self) -> Vec<Token> {
        self.finish();
        std::mem::take(&mut self.output)
    }
    fn advance(&mut self, source: JsString) {
        self.source = std::sync::Arc::new(source);
        if self.at_scalar {
            self.at_scalar = false;
            self.step();
            self.offset += self.source.len_utf16();
            return;
        }
        match token_type(&self.source) {
            None => {
                let mut message: Vec<_> = "Not a YAML token: ".encode_utf16().collect();
                message.extend_from_slice(self.source.units());
                let error = self.error(JsString::from_utf16(message));
                self.pop(Some(error));
                self.offset += self.source.len_utf16();
            }
            Some(Scalar) => {
                self.at_new_line = false;
                self.at_scalar = true;
                self.kind = Scalar;
            }
            Some(kind) => {
                self.kind = kind;
                self.step();
                match kind {
                    Newline => {
                        self.at_new_line = true;
                        self.indent = 0;
                        if self.record_lines {
                            self.line_counter
                                .add_new_line(self.offset + self.source.len_utf16());
                        }
                    }
                    Space if self.at_new_line && self.source.units().first() == Some(&32) => {
                        self.indent += self.source.len_utf16()
                    }
                    Space => (),
                    ExplicitKeyInd | MapValueInd | SeqItemInd => {
                        if self.at_new_line {
                            self.indent += self.source.len_utf16();
                        }
                    }
                    DocMode | FlowErrorEnd => return,
                    _ => self.at_new_line = false,
                }
                self.offset += self.source.len_utf16();
            }
        }
    }
    fn finish(&mut self) {
        while !self.stack.is_empty() {
            self.pop(None);
        }
    }
    fn source_token(&self) -> Token {
        let mut st = Token::new(self.kind, self.offset);
        st.indent = Some(self.indent);
        st.source = Some(self.source.clone());
        st
    }
    fn error(&self, message: JsString) -> Token {
        let mut token = Token::new(Error, self.offset);
        token.message = Some(message);
        token.source = Some(self.source.clone());
        token
    }
    // Upstream tail-calls step when a lexeme closes several block frames.
    // Trampoline those calls; a dedent may close a million frames at once.
    fn step(&mut self) {
        self.step_pending = true;
        if self.stepping {
            return;
        }
        self.stepping = true;
        while self.step_pending {
            self.step_pending = false;
            self.step_inner();
        }
        self.stepping = false;
    }
    fn step_inner(&mut self) {
        if self.kind == DocEnd && self.stack.last().is_none_or(|t| t.kind != DocEnd) {
            self.finish();
            let mut token = Token::new(DocEnd, self.offset);
            token.source = Some(self.source.clone());
            self.stack.push_small(token);
            return;
        }
        let Some(top) = self.stack.pop() else {
            self.stream();
            return;
        };
        match top.kind {
            Document => self.document(top),
            Alias | Scalar | SingleQuotedScalar | DoubleQuotedScalar => self.scalar(top),
            BlockScalar => self.block_scalar(top),
            BlockMap => self.block_map(top),
            BlockSeq => self.block_sequence(top),
            FlowCollection => self.flow_collection(top),
            DocEnd => self.document_end(top),
            _ => {
                self.stack.push_small(top);
                self.pop(None);
            }
        }
    }
    fn pop(&mut self, error: Option<Token>) {
        let mut pending = Vec::new();
        self.pop_inner(error, &mut pending);
        while let Some(error) = pending.pop() {
            self.pop_inner(error, &mut pending);
        }
    }
    fn pop_inner(&mut self, error: Option<Token>, pending: &mut Vec<Option<Token>>) {
        let Some(mut token) = error.or_else(|| self.stack.pop()) else {
            let mut token = self.error("Tried to pop an empty stack".into());
            token.source = Some(Default::default());
            self.output.push_small(token);
            return;
        };
        let Some(top) = self.stack.last_mut() else {
            self.output.push_small(token);
            return;
        };
        if token.kind == BlockScalar {
            token.indent = Some(top.indent.unwrap_or(0));
        } else if token.kind == FlowCollection && top.kind == Document {
            token.indent = Some(0);
        }
        if token.kind == FlowCollection {
            fix_flow_seq_items(&mut token);
        }
        // Upstream moves a trailing empty item after attaching the child by reference.
        // Detach it first here, then append it to the parent after attachment.
        let trailing = if matches!(top.kind, Document | BlockMap | BlockSeq)
            && !(top.kind == BlockMap
                && top
                    .items
                    .last()
                    .is_some_and(|it| it.value.is_some() || it.sep.is_none()))
            && matches!(token.kind, BlockMap | BlockSeq)
        {
            if token.items.last().is_some_and(|last| {
                last.sep.is_none()
                    && last.value.is_none()
                    && !last.start.is_empty()
                    && find_non_empty_index(&last.start).is_none()
                    && (token.indent == Some(0)
                        || last
                            .start
                            .iter()
                            .all(|st| st.kind != Comment || st.indent < token.indent))
            }) {
                Some(token.items.pop().unwrap().start)
            } else {
                None
            }
        } else {
            None
        };
        match top.kind {
            Document => top.value = Some(std::sync::Arc::new(token)),
            BlockScalar => top.props.push_small(token),
            BlockMap => {
                let it = top.items.last_mut().unwrap();
                if it.value.is_some() {
                    top.items.push_small(pair(
                        Vec::new(),
                        Some(std::sync::Arc::new(token)),
                        Vec::new(),
                    ));
                    self.on_key_line = true;
                    return;
                } else if it.sep.is_some() {
                    it.value = Some(std::sync::Arc::new(token));
                } else {
                    it.key = Some(Some(std::sync::Arc::new(token)));
                    it.sep = Some(Vec::new());
                    self.on_key_line = !it.explicit_key;
                    return;
                }
            }
            BlockSeq => {
                let it = top.items.last_mut().unwrap();
                if it.value.is_some() {
                    top.items.push_small(CollectionItem {
                        value: Some(std::sync::Arc::new(token)),
                        ..Default::default()
                    });
                } else {
                    it.value = Some(std::sync::Arc::new(token));
                }
            }
            FlowCollection => {
                if top.items.last().is_none_or(|it| it.value.is_some()) {
                    top.items.push_small(pair(
                        Vec::new(),
                        Some(std::sync::Arc::new(token)),
                        Vec::new(),
                    ));
                } else {
                    let it = top.items.last_mut().unwrap();
                    if it.sep.is_some() {
                        it.value = Some(std::sync::Arc::new(token));
                    } else {
                        it.key = Some(Some(std::sync::Arc::new(token)));
                        it.sep = Some(Vec::new());
                    }
                }
                return;
            }
            _ => {
                pending.push_small(Some(token));
                pending.push_small(None);
                return;
            }
        }
        if let Some(start) = trailing {
            if top.kind == Document {
                top.end = Some(start);
            } else {
                top.items.push_small(item(start));
            }
        }
    }
    fn stream(&mut self) {
        match self.kind {
            DirectiveLine => {
                let mut t = Token::new(Directive, self.offset);
                t.source = Some(self.source.clone());
                self.output.push_small(t);
            }
            ByteOrderMark | Space | Comment | Newline => {
                self.output.push_small(self.source_token())
            }
            DocMode | DocStart => {
                let mut doc = Token::new(Document, self.offset);
                if self.kind == DocStart {
                    doc.start.push_small(self.source_token());
                }
                self.stack.push_small(doc);
            }
            _ => {
                self.output.push_small(self.error(
                    format!("Unexpected {} token in YAML stream", self.kind.as_str()).into(),
                ))
            }
        }
    }
    fn document(&mut self, mut doc: Token) {
        if doc.value.is_some() {
            self.line_end(doc);
            return;
        }
        match self.kind {
            DocStart => {
                if find_non_empty_index(&doc.start).is_some() {
                    self.stack.push_small(doc);
                    self.pop(None);
                    self.step();
                } else {
                    doc.start.push_small(self.source_token());
                    self.stack.push_small(doc);
                }
            }
            Anchor | Tag | Space | Comment | Newline => {
                doc.start.push_small(self.source_token());
                self.stack.push_small(doc);
            }
            _ => {
                let bv = self.start_block_value(&mut doc);
                self.stack.push_small(doc);
                if let Some(bv) = bv {
                    self.stack.push_small(bv);
                } else {
                    self.output.push_small(self.error(
                        format!("Unexpected {} token in YAML document", self.kind.as_str()).into(),
                    ));
                }
            }
        }
    }
    fn scalar(&mut self, mut scalar: Token) {
        if self.kind == MapValueInd {
            let start = get_first_key_start_props(get_prev_props(self.stack.last_mut().unwrap()));
            let mut sep = scalar.end.take().unwrap_or_default();
            sep.push_small(self.source_token());
            let mut map = Token::new(BlockMap, scalar.offset);
            map.indent = scalar.indent;
            map.items
                .push_small(pair(start, Some(std::sync::Arc::new(scalar)), sep));
            self.on_key_line = true;
            self.stack.push_small(map);
        } else {
            self.line_end(scalar);
        }
    }
    fn scalar_newlines(&mut self) {
        for (i, ch) in self.source.units().iter().enumerate() {
            if *ch == 10 && self.record_lines {
                self.line_counter.add_new_line(self.offset + i + 1);
            }
        }
    }
    fn block_scalar(&mut self, mut scalar: Token) {
        match self.kind {
            Space | Comment | Newline => {
                scalar.props.push_small(self.source_token());
                self.stack.push_small(scalar);
            }
            Scalar => {
                scalar.source = Some(self.source.clone());
                self.at_new_line = true;
                self.indent = 0;
                self.scalar_newlines();
                self.stack.push_small(scalar);
                self.pop(None);
            }
            _ => {
                self.stack.push_small(scalar);
                self.pop(None);
                self.step();
            }
        }
    }
    fn block_map(&mut self, mut map: Token) {
        let st = self.source_token();
        let map_indent = map.indent.unwrap();
        let it = map.items.last_mut().unwrap();
        match self.kind {
            Newline => {
                self.on_key_line = false;
                if let Some(value) = &mut it.value {
                    if value
                        .end
                        .as_ref()
                        .and_then(|e| e.last())
                        .is_some_and(|t| t.kind == Comment)
                    {
                        std::sync::Arc::make_mut(value)
                            .end
                            .as_mut()
                            .unwrap()
                            .push_small(st);
                    } else {
                        map.items.push_small(item(vec![st]));
                    }
                } else if let Some(sep) = &mut it.sep {
                    sep.push_small(st);
                } else {
                    it.start.push_small(st);
                }
                self.stack.push_small(map);
                return;
            }
            Space | Comment => {
                if it.value.is_some() {
                    map.items.push_small(item(vec![st]));
                } else if let Some(sep) = &mut it.sep {
                    sep.push_small(st);
                } else {
                    if self.at_indented_comment(&it.start, map_indent)
                        && Self::append_indented_comment(&mut map, st.clone())
                    {
                        self.stack.push_small(map);
                        return;
                    }
                    map.items.last_mut().unwrap().start.push_small(st);
                }
                self.stack.push_small(map);
                return;
            }
            _ => (),
        }
        if self.indent >= map_indent {
            let it = map.items.last_mut().unwrap();
            let at_map_indent = !self.on_key_line && self.indent == map_indent;
            let at_next_item =
                at_map_indent && (it.sep.is_some() || it.explicit_key) && self.kind != SeqItemInd;
            let mut start = Vec::new();
            if at_next_item
                && it.value.is_none()
                && let Some(sep) = it.sep.as_mut()
            {
                let mut nl = Vec::new();
                for (i, st) in sep.iter().enumerate() {
                    match st.kind {
                        Newline => nl.push_small(i),
                        Space => (),
                        Comment => {
                            if st.indent.unwrap_or(0) > map_indent {
                                nl.clear();
                            }
                        }
                        _ => nl.clear(),
                    }
                }
                if nl.len() >= 2 {
                    start = sep.split_off(nl[1]);
                }
            }
            let mut child = None;
            match self.kind {
                Anchor | Tag => {
                    if at_next_item || it.value.is_some() {
                        start.push_small(st);
                        map.items.push_small(item(start));
                        self.on_key_line = true;
                    } else if let Some(sep) = &mut it.sep {
                        sep.push_small(st);
                    } else {
                        it.start.push_small(st);
                    }
                }
                ExplicitKeyInd => {
                    if it.sep.is_none() && !it.explicit_key {
                        it.start.push_small(st);
                        it.explicit_key = true;
                    } else if at_next_item || it.value.is_some() {
                        start.push_small(st);
                        let mut it = item(start);
                        it.explicit_key = true;
                        map.items.push_small(it);
                    } else {
                        let mut it = item(vec![st]);
                        it.explicit_key = true;
                        child = Some(self.block_map_token(it));
                    }
                    self.on_key_line = true;
                }
                MapValueInd => {
                    if it.explicit_key {
                        if it.sep.is_none() {
                            if includes_token(&it.start, Newline) {
                                it.key = Some(None);
                                it.sep = Some(vec![st]);
                            } else {
                                let start = get_first_key_start_props(&mut it.start);
                                child = Some(self.block_map_token(pair(start, None, vec![st])));
                            }
                        } else if it.value.is_some() {
                            map.items.push_small(pair(Vec::new(), None, vec![st]));
                        } else if includes_token(it.sep.as_ref().unwrap(), MapValueInd) {
                            child = Some(self.block_map_token(pair(start, None, vec![st])));
                        } else if is_flow_token(it.key.as_ref().and_then(|k| k.as_deref()))
                            && !includes_token(it.sep.as_ref().unwrap(), Newline)
                        {
                            let start = get_first_key_start_props(&mut it.start);
                            let key = it.key.take().flatten();
                            let mut sep = it.sep.take().unwrap();
                            sep.push_small(st);
                            child = Some(self.block_map_token(pair(start, key, sep)));
                        } else if !start.is_empty() {
                            let sep = it.sep.as_mut().unwrap();
                            sep.extend_small(start);
                            sep.push_small(st);
                        } else {
                            it.sep.as_mut().unwrap().push_small(st);
                        }
                    } else if it.sep.is_none() {
                        it.key = Some(None);
                        it.sep = Some(vec![st]);
                    } else if it.value.is_some() || at_next_item {
                        map.items.push_small(pair(start, None, vec![st]));
                    } else if includes_token(it.sep.as_ref().unwrap(), MapValueInd) {
                        child = Some(self.block_map_token(pair(Vec::new(), None, vec![st])));
                    } else {
                        it.sep.as_mut().unwrap().push_small(st);
                    }
                    self.on_key_line = true;
                }
                Alias | Scalar | SingleQuotedScalar | DoubleQuotedScalar => {
                    let fs = self.flow_scalar();
                    if at_next_item || it.value.is_some() {
                        map.items.push_small(pair(
                            start,
                            Some(std::sync::Arc::new(fs)),
                            Vec::new(),
                        ));
                        self.on_key_line = true;
                    } else if it.sep.is_some() {
                        child = Some(fs);
                    } else {
                        it.key = Some(Some(std::sync::Arc::new(fs)));
                        it.sep = Some(Vec::new());
                        self.on_key_line = true;
                    }
                }
                _ => {
                    let bv = self.start_block_value(&mut map);
                    if let Some(bv) = bv {
                        if bv.kind == BlockSeq {
                            let it = map.items.last().unwrap();
                            if !it.explicit_key
                                && it
                                    .sep
                                    .as_ref()
                                    .is_some_and(|sep| !includes_token(sep, Newline))
                            {
                                self.stack.push_small(map);
                                self.pop(Some(self.error(
                                    "Unexpected block-seq-ind on same line with key".into(),
                                )));
                                return;
                            }
                        } else if at_map_indent {
                            map.items.push_small(item(start));
                        }
                        child = Some(bv);
                    } else {
                        self.stack.push_small(map);
                        self.pop(None);
                        self.step();
                        return;
                    }
                }
            }
            self.stack.push_small(map);
            if let Some(child) = child {
                self.stack.push_small(child);
            }
            return;
        }
        self.stack.push_small(map);
        self.pop(None);
        self.step();
    }
    fn block_map_token(&self, it: CollectionItem) -> Token {
        let mut token = Token::new(BlockMap, self.offset);
        token.indent = Some(self.indent);
        token.items.push_small(it);
        token
    }
    fn append_indented_comment(collection: &mut Token, st: Token) -> bool {
        let len = collection.items.len();
        if len < 2
            || collection.items[len - 2]
                .value
                .as_ref()
                .is_none_or(|v| v.end.is_none())
        {
            return false;
        }
        let start = collection.items.pop().unwrap().start;
        let end =
            std::sync::Arc::make_mut(collection.items.last_mut().unwrap().value.as_mut().unwrap())
                .end
                .as_mut()
                .unwrap();
        end.extend_small(start);
        end.push_small(st);
        true
    }
    fn block_sequence(&mut self, mut seq: Token) {
        let st = self.source_token();
        let seq_indent = seq.indent.unwrap();
        let it = seq.items.last_mut().unwrap();
        match self.kind {
            Newline => {
                if let Some(value) = &mut it.value {
                    if value
                        .end
                        .as_ref()
                        .and_then(|e| e.last())
                        .is_some_and(|t| t.kind == Comment)
                    {
                        std::sync::Arc::make_mut(value)
                            .end
                            .as_mut()
                            .unwrap()
                            .push_small(st);
                    } else {
                        seq.items.push_small(item(vec![st]));
                    }
                } else {
                    it.start.push_small(st);
                }
                self.stack.push_small(seq);
                return;
            }
            Space | Comment => {
                if it.value.is_some() {
                    seq.items.push_small(item(vec![st]));
                } else {
                    if self.at_indented_comment(&it.start, seq_indent)
                        && Self::append_indented_comment(&mut seq, st.clone())
                    {
                        self.stack.push_small(seq);
                        return;
                    }
                    seq.items.last_mut().unwrap().start.push_small(st);
                }
                self.stack.push_small(seq);
                return;
            }
            Anchor | Tag if it.value.is_none() && self.indent > seq_indent => {
                it.start.push_small(st);
                self.stack.push_small(seq);
                return;
            }
            SeqItemInd if self.indent == seq_indent => {
                if it.value.is_some() || includes_token(&it.start, SeqItemInd) {
                    seq.items.push_small(item(vec![st]));
                } else {
                    it.start.push_small(st);
                }
                self.stack.push_small(seq);
                return;
            }
            _ => (),
        }
        if self.indent > seq_indent
            && let Some(bv) = self.start_block_value(&mut seq)
        {
            self.stack.push_small(seq);
            self.stack.push_small(bv);
            return;
        }
        self.stack.push_small(seq);
        self.pop(None);
        self.step();
    }
    fn flow_collection(&mut self, mut fc: Token) {
        if self.kind == FlowErrorEnd {
            self.stack.push_small(fc);
            loop {
                self.pop(None);
                if self.stack.last().is_none_or(|t| t.kind != FlowCollection) {
                    break;
                }
            }
        } else if fc.end.as_ref().unwrap().is_empty() {
            let st = self.source_token();
            let mut child = None;
            match self.kind {
                Comma | ExplicitKeyInd => {
                    if fc.items.last().is_none_or(|it| it.sep.is_some()) {
                        fc.items.push_small(item(vec![st]));
                    } else {
                        fc.items.last_mut().unwrap().start.push_small(st);
                    }
                }
                MapValueInd => {
                    if fc.items.last().is_none_or(|it| it.value.is_some()) {
                        fc.items.push_small(pair(Vec::new(), None, vec![st]));
                    } else {
                        let it = fc.items.last_mut().unwrap();
                        if let Some(sep) = &mut it.sep {
                            sep.push_small(st);
                        } else {
                            it.key = Some(None);
                            it.sep = Some(vec![st]);
                        }
                    }
                }
                Space | Comment | Newline | Anchor | Tag => {
                    if fc.items.last().is_none_or(|it| it.value.is_some()) {
                        fc.items.push_small(item(vec![st]));
                    } else {
                        let it = fc.items.last_mut().unwrap();
                        if let Some(sep) = &mut it.sep {
                            sep.push_small(st);
                        } else {
                            it.start.push_small(st);
                        }
                    }
                }
                Alias | Scalar | SingleQuotedScalar | DoubleQuotedScalar => {
                    let fs = self.flow_scalar();
                    if fc.items.last().is_none_or(|it| it.value.is_some()) {
                        fc.items.push_small(pair(
                            Vec::new(),
                            Some(std::sync::Arc::new(fs)),
                            Vec::new(),
                        ));
                    } else {
                        let it = fc.items.last_mut().unwrap();
                        if it.sep.is_some() {
                            child = Some(fs);
                        } else {
                            it.key = Some(Some(std::sync::Arc::new(fs)));
                            it.sep = Some(Vec::new());
                        }
                    }
                }
                FlowMapEnd | FlowSeqEnd => fc.end.as_mut().unwrap().push_small(st),
                _ => {
                    child = self.start_block_value(&mut fc);
                    if child.is_none() {
                        self.stack.push_small(fc);
                        self.pop(None);
                        self.step();
                        return;
                    }
                }
            }
            self.stack.push_small(fc);
            if let Some(child) = child {
                self.stack.push_small(child);
            }
        } else {
            let parent = self.stack.last_mut().unwrap();
            if parent.kind == BlockMap
                && ((self.kind == MapValueInd && parent.indent == fc.indent)
                    || (self.kind == Newline && parent.items.last().unwrap().sep.is_none()))
            {
                self.stack.push_small(fc);
                self.pop(None);
                self.step();
            } else if self.kind == MapValueInd && parent.kind != FlowCollection {
                let start = get_first_key_start_props(get_prev_props(parent));
                fix_flow_seq_items(&mut fc);
                let mut sep = fc.end.as_mut().unwrap().split_off(1);
                sep.push_small(self.source_token());
                let mut map = Token::new(BlockMap, fc.offset);
                map.indent = fc.indent;
                map.items
                    .push_small(pair(start, Some(std::sync::Arc::new(fc)), sep));
                self.on_key_line = true;
                self.stack.push_small(map);
            } else {
                self.line_end(fc);
            }
        }
    }
    fn flow_scalar(&mut self) -> Token {
        self.scalar_newlines();
        self.source_token()
    }
    fn start_block_value(&mut self, parent: &mut Token) -> Option<Token> {
        Some(match self.kind {
            Alias | Scalar | SingleQuotedScalar | DoubleQuotedScalar => self.flow_scalar(),
            BlockScalarHeader => {
                let mut t = Token::new(BlockScalar, self.offset);
                t.indent = Some(self.indent);
                t.props.push_small(self.source_token());
                t.source = Some(Default::default());
                t
            }
            FlowMapStart | FlowSeqStart => {
                let mut t = Token::new(FlowCollection, self.offset);
                t.indent = Some(self.indent);
                t.flow_start = Some(std::sync::Arc::new(self.source_token()));
                t.end = Some(Vec::new());
                t
            }
            SeqItemInd => {
                let mut t = Token::new(BlockSeq, self.offset);
                t.indent = Some(self.indent);
                t.items.push_small(item(vec![self.source_token()]));
                t
            }
            ExplicitKeyInd => {
                self.on_key_line = true;
                let mut start = get_first_key_start_props(get_prev_props(parent));
                start.push_small(self.source_token());
                let mut it = item(start);
                it.explicit_key = true;
                self.block_map_token(it)
            }
            MapValueInd => {
                self.on_key_line = true;
                let start = get_first_key_start_props(get_prev_props(parent));
                self.block_map_token(pair(start, None, vec![self.source_token()]))
            }
            _ => return None,
        })
    }
    fn at_indented_comment(&self, start: &[Token], indent: usize) -> bool {
        self.kind == Comment
            && self.indent > indent
            && start.iter().all(|st| matches!(st.kind, Newline | Space))
    }
    fn document_end(&mut self, mut doc_end: Token) {
        if self.kind != DocMode {
            doc_end
                .end
                .get_or_insert_with(Vec::new)
                .push_small(self.source_token());
        }
        self.stack.push_small(doc_end);
        if self.kind == Newline {
            self.pop(None);
        }
    }
    fn line_end(&mut self, mut token: Token) {
        match self.kind {
            Comma | DocStart | DocEnd | FlowSeqEnd | FlowMapEnd | MapValueInd => {
                self.stack.push_small(token);
                self.pop(None);
                self.step();
            }
            _ => {
                if self.kind == Newline {
                    self.on_key_line = false;
                }
                token
                    .end
                    .get_or_insert_with(Vec::new)
                    .push_small(self.source_token());
                self.stack.push_small(token);
                if self.kind == Newline {
                    self.pop(None);
                }
            }
        }
    }
}
