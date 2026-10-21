//! Arena rope for Turndown's join/trim/line-prefix operations. Boundary
//! newlines are metadata; ancestor indentation is emitted only at flattening.
//! Both traversal and destruction are flat, including very wide documents.
use crate::metadata::js_space;

#[derive(Clone, Copy, Default)]
pub(super) struct Content {
    pub leading: usize,
    pub body: Option<usize>,
    pub trailing: usize,
}
impl Content {
    pub fn trim_newlines(self) -> Self {
        Self {
            leading: 0,
            trailing: 0,
            ..self
        }
    }
    pub fn prepend_newlines(mut self, n: usize) -> Self {
        self.leading += n;
        self
    }
    pub fn append_newlines(mut self, n: usize) -> Self {
        if self.body.is_some() {
            self.trailing += n;
        } else {
            self.leading += n;
        }
        self
    }
    pub fn wrap_block(self) -> Self {
        self.prepend_newlines(2).append_newlines(2)
    }
    fn empty(self) -> bool {
        self.body.is_none() && self.leading == 0
    }
}
enum Part {
    Text(String),
    Pair(Content, Content),
    Indent(usize, String),
}
struct Node {
    part: Part,
    blank: bool,
}
#[derive(Default)]
pub(super) struct Rope {
    nodes: Vec<Node>,
}
impl Rope {
    fn push(&mut self, part: Part, blank: bool) -> usize {
        let id = self.nodes.len();
        self.nodes.push(Node { part, blank });
        id
    }
    pub fn text(&mut self, text: &str) -> Content {
        let leading = text.len() - text.trim_start_matches('\n').len();
        let rest = &text[leading..];
        let body = rest.trim_end_matches('\n');
        Content {
            leading,
            body: (!body.is_empty())
                .then(|| self.push(Part::Text(body.into()), body.chars().all(js_space))),
            trailing: rest.len() - body.len(),
        }
    }
    pub fn blank(&self, content: Content) -> bool {
        content.body.is_none_or(|id| self.nodes[id].blank)
    }
    /// Concatenation without Turndown's boundary newline normalization.
    pub fn concat(&mut self, mut left: Content, mut right: Content) -> Content {
        if left.body.is_none() {
            right.leading += left.leading;
            return right;
        }
        if right.body.is_none() {
            left.trailing += right.leading;
            return left;
        }
        let leading = left.leading;
        let trailing = right.trailing;
        left.leading = 0;
        right.trailing = 0;
        let blank = self.blank(left) && self.blank(right);
        Content {
            leading,
            trailing,
            body: Some(self.push(Part::Pair(left, right), blank)),
        }
    }
    pub fn join(&mut self, mut left: Content, mut right: Content) -> Content {
        if left.empty() {
            right.leading = right.leading.min(2);
            return right;
        }
        let trailing = if left.body.is_none() {
            left.leading
        } else {
            left.trailing
        };
        let boundary = trailing.max(right.leading).min(2);
        if left.body.is_none() {
            left.leading = boundary;
        } else {
            left.trailing = boundary;
        }
        right.leading = 0;
        self.concat(left, right)
    }
    pub fn indent(&mut self, content: Content, prefix: &str) -> Content {
        // Isolate boundary newlines: their inserted prefixes become body text.
        let mut result = Content::default();
        if content.leading > 0 {
            let text = format!("\n{prefix}").repeat(content.leading);
            result = self.text(&text);
        }
        if let Some(id) = content.body {
            let blank = self.nodes[id].blank && prefix.chars().all(js_space);
            let body = Content {
                body: Some(self.push(Part::Indent(id, prefix.into()), blank)),
                ..Content::default()
            };
            result = self.concat(result, body);
        }
        if content.trailing > 0 {
            let text = self.text(&format!("\n{prefix}").repeat(content.trailing));
            result = self.concat(result, text);
        }
        result
    }
    pub fn flatten(&self, content: Content) -> String {
        enum Task {
            Content(Content),
            Body(usize),
            Newlines(usize),
            PopPrefix,
        }
        let mut work = vec![Task::Content(content)];
        let mut prefixes: Vec<&str> = Vec::new();
        let mut out = String::new();
        while let Some(task) = work.pop() {
            match task {
                Task::Content(content) => {
                    work.push(Task::Newlines(content.trailing));
                    if let Some(id) = content.body {
                        work.push(Task::Body(id));
                    }
                    work.push(Task::Newlines(content.leading));
                }
                Task::Newlines(n) => {
                    for _ in 0..n {
                        out.push('\n');
                        for prefix in &prefixes {
                            out.push_str(prefix);
                        }
                    }
                }
                Task::PopPrefix => {
                    prefixes.pop();
                }
                Task::Body(id) => match &self.nodes[id].part {
                    Part::Text(text) => {
                        for line in text.split_inclusive('\n') {
                            out.push_str(line);
                            if line.ends_with('\n') {
                                for prefix in &prefixes {
                                    out.push_str(prefix);
                                }
                            }
                        }
                    }
                    Part::Pair(left, right) => {
                        work.push(Task::Content(*right));
                        work.push(Task::Content(*left));
                    }
                    Part::Indent(id, prefix) => {
                        prefixes.push(prefix);
                        work.push(Task::PopPrefix);
                        work.push(Task::Body(*id));
                    }
                },
            }
        }
        out
    }
}
