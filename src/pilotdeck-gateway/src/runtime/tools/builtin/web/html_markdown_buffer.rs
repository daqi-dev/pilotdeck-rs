//! Arena rope for Turndown's join/trim/line-prefix operations. Boundary
//! newlines are metadata; ancestor indentation is emitted only at flattening.
//! Both traversal and destruction are flat, including very wide documents.
use crate::metadata::js_space;

/// Node v22 / V8's maximum string length, in UTF-16 code units.
pub const MAX_STRING_LENGTH: usize = crate::fetch_redirect::MAX_STRING_LENGTH;
pub const STRING_LIMIT_ERROR: &str = "Invalid string length";
pub(super) type Result<T> = std::result::Result<T, String>;

pub(super) fn check_length(length: usize) -> Result<()> {
    check_budget(length, MAX_STRING_LENGTH)
}
fn check_budget(length: usize, limit: usize) -> Result<()> {
    if length > limit {
        Err(STRING_LIMIT_ERROR.into())
    } else {
        Ok(())
    }
}
/// Validate the complete result before any allocation or append.
pub(super) fn concatenate(parts: &[&str]) -> Result<String> {
    check_length(parts.iter().map(|s| s.encode_utf16().count()).sum())?;
    Ok(parts.concat())
}

pub(super) fn repeat_string(text: &str, n: usize) -> Result<String> {
    check_length(Size::text(text).times(n).utf16)?;
    Ok(text.repeat(n))
}

#[derive(Clone, Copy, Default)]
struct Size {
    bytes: usize,
    utf16: usize,
    newlines: usize,
}
impl Size {
    fn text(s: &str) -> Self {
        Self {
            bytes: s.len(),
            utf16: s.encode_utf16().count(),
            newlines: s.bytes().filter(|&b| b == b'\n').count(),
        }
    }
    fn add(self, other: Self) -> Self {
        Self {
            bytes: self.bytes.saturating_add(other.bytes),
            utf16: self.utf16.saturating_add(other.utf16),
            newlines: self.newlines.saturating_add(other.newlines),
        }
    }
    fn times(self, n: usize) -> Self {
        Self {
            bytes: self.bytes.saturating_mul(n),
            utf16: self.utf16.saturating_mul(n),
            newlines: self.newlines.saturating_mul(n),
        }
    }
}

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
    Released,
    Text(String),
    Repeat(String, usize),
    Pair(Content, Content),
    Indent(usize, String),
}
struct Node {
    part: Part,
    blank: bool,
    size: Size,
}
pub(super) struct Rope {
    limit: usize,
    nodes: Vec<Node>,
    #[cfg(test)]
    text_bytes: usize,
    #[cfg(test)]
    peak_text_bytes: usize,
}
impl Default for Rope {
    fn default() -> Self {
        Self {
            limit: MAX_STRING_LENGTH,
            nodes: Vec::new(),
            #[cfg(test)]
            text_bytes: 0,
            #[cfg(test)]
            peak_text_bytes: 0,
        }
    }
}
impl Rope {
    fn size(&self, content: Content) -> Size {
        let body = content
            .body
            .map_or(Size::default(), |id| self.nodes[id].size);
        body.add(Size {
            bytes: content.leading.saturating_add(content.trailing),
            utf16: content.leading.saturating_add(content.trailing),
            newlines: content.leading.saturating_add(content.trailing),
        })
    }
    pub fn check(&self, content: Content) -> Result<Content> {
        check_budget(self.size(content).utf16, self.limit)?;
        Ok(content)
    }
    pub fn utf16_len(&self, content: Content) -> usize {
        self.size(content).utf16
    }
    fn push(&mut self, part: Part, blank: bool, size: Size) -> Result<usize> {
        check_budget(size.utf16, self.limit)?;
        #[cfg(test)]
        {
            self.text_bytes += Self::part_bytes(&part);
            self.peak_text_bytes = self.peak_text_bytes.max(self.text_bytes);
        }
        let id = self.nodes.len();
        self.nodes.push(Node { part, blank, size });
        Ok(id)
    }
    #[cfg(test)]
    fn part_bytes(part: &Part) -> usize {
        match part {
            Part::Text(text) | Part::Indent(_, text) | Part::Repeat(text, _) => text.capacity(),
            _ => 0,
        }
    }
    #[cfg(test)]
    pub fn peak_text_bytes(&self) -> usize {
        self.peak_text_bytes
    }
    #[cfg(test)]
    pub fn with_limit(limit: usize) -> Self {
        Self {
            limit,
            ..Self::default()
        }
    }
    /// Materialize a subtree that the renderer is about to replace, then drop
    /// its old strings. The renderer consumes each child output once; aliases
    /// such as emphasis's repeated marker are confined to this same subtree.
    /// IDs stay stable, and visiting a shared marker twice is harmless.
    pub fn flatten_and_release(&mut self, content: Content) -> Result<String> {
        self.check(content)?;
        // Inline code commonly replaces one Text node. Move its allocation
        // into the next replacement instead of copying the whole payload.
        if content.leading == 0
            && content.trailing == 0
            && let Some(id) = content.body
            && matches!(self.nodes[id].part, Part::Text(_))
        {
            let Part::Text(text) = std::mem::replace(&mut self.nodes[id].part, Part::Released)
            else {
                unreachable!()
            };
            #[cfg(test)]
            {
                self.text_bytes -= text.capacity();
            }
            return Ok(text);
        }
        let text = self.flatten(content)?;
        let mut work: Vec<_> = content.body.into_iter().collect();
        while let Some(id) = work.pop() {
            let part = std::mem::replace(&mut self.nodes[id].part, Part::Released);
            #[cfg(test)]
            {
                self.text_bytes -= Self::part_bytes(&part);
            }
            match part {
                Part::Pair(left, right) => {
                    work.extend(left.body);
                    work.extend(right.body);
                }
                Part::Indent(child, _) => work.push(child),
                Part::Text(_) | Part::Repeat(_, _) | Part::Released => {}
            }
        }
        Ok(text)
    }
    pub fn text(&mut self, text: &str) -> Result<Content> {
        check_budget(text.encode_utf16().count(), self.limit)?;
        let leading = text.len() - text.trim_start_matches('\n').len();
        let rest = &text[leading..];
        let body = rest.trim_end_matches('\n');
        let body_id = if body.is_empty() {
            None
        } else {
            Some(self.push(
                Part::Text(body.into()),
                body.chars().all(js_space),
                Size::text(body),
            )?)
        };
        Ok(Content {
            leading,
            body: body_id,
            trailing: rest.len() - body.len(),
        })
    }
    /// Keep an already materialized replacement's allocation for reuse.
    pub fn text_owned(&mut self, mut text: String) -> Result<Content> {
        check_budget(text.encode_utf16().count(), self.limit)?;
        let leading = text.len() - text.trim_start_matches('\n').len();
        let end = text.trim_end_matches('\n').len().max(leading);
        let trailing = text.len() - end;
        text.truncate(end);
        text.drain(..leading);
        let body = if text.is_empty() {
            None
        } else {
            let blank = text.chars().all(js_space);
            let size = Size::text(&text);
            Some(self.push(Part::Text(text), blank, size)?)
        };
        Ok(Content {
            leading,
            trailing,
            body,
        })
    }
    /// Repeated markers stay symbolic until a validated final flatten. In
    /// particular, rejected headings never allocate hundreds of MiB first.
    pub fn repeat(&mut self, text: &str, n: usize) -> Result<Content> {
        let size = Size::text(text).times(n);
        check_budget(size.utf16, self.limit)?;
        if n == 0 || text.is_empty() {
            return Ok(Content::default());
        }
        debug_assert!(!text.contains('\n'));
        Ok(Content {
            body: Some(self.push(
                Part::Repeat(text.into(), n),
                text.chars().all(js_space),
                size,
            )?),
            ..Content::default()
        })
    }
    pub fn blank(&self, content: Content) -> bool {
        content.body.is_none_or(|id| self.nodes[id].blank)
    }
    /// Concatenation without Turndown's boundary newline normalization.
    pub fn concat(&mut self, mut left: Content, mut right: Content) -> Result<Content> {
        check_budget(self.size(left).add(self.size(right)).utf16, self.limit)?;
        if left.body.is_none() {
            right.leading += left.leading;
            return Ok(right);
        }
        if right.body.is_none() {
            left.trailing += right.leading;
            return Ok(left);
        }
        let leading = left.leading;
        let trailing = right.trailing;
        left.leading = 0;
        right.trailing = 0;
        let blank = self.blank(left) && self.blank(right);
        let size = self.size(left).add(self.size(right));
        Ok(Content {
            leading,
            trailing,
            body: Some(self.push(Part::Pair(left, right), blank, size)?),
        })
    }
    pub fn join(&mut self, mut left: Content, mut right: Content) -> Result<Content> {
        if left.empty() {
            right.leading = right.leading.min(2);
            return self.check(right);
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
    pub fn indent(&mut self, content: Content, prefix: &str) -> Result<Content> {
        let size = self.size(content);
        let prefix_size = Size::text(prefix);
        check_budget(size.add(prefix_size.times(size.newlines)).utf16, self.limit)?;
        // Isolate boundary newlines: their inserted prefixes become body text.
        let mut result = Content::default();
        if content.leading > 0 {
            let text = repeat_string(&concatenate(&["\n", prefix])?, content.leading)?;
            result = self.text(&text)?;
        }
        if let Some(id) = content.body {
            let blank = self.nodes[id].blank && prefix.chars().all(js_space);
            let body = Content {
                body: Some(
                    self.push(
                        Part::Indent(id, prefix.into()),
                        blank,
                        self.nodes[id]
                            .size
                            .add(prefix_size.times(self.nodes[id].size.newlines)),
                    )?,
                ),
                ..Content::default()
            };
            result = self.concat(result, body)?;
        }
        if content.trailing > 0 {
            let text = self.text(&repeat_string(
                &concatenate(&["\n", prefix])?,
                content.trailing,
            )?)?;
            result = self.concat(result, text)?;
        }
        Ok(result)
    }
    pub fn flatten(&self, content: Content) -> Result<String> {
        self.check(content)?;
        enum Task {
            Content(Content),
            Body(usize),
            Newlines(usize),
            PopPrefix,
        }
        let mut work = vec![Task::Content(content)];
        let mut prefixes: Vec<&str> = Vec::new();
        let mut out = String::with_capacity(self.size(content).bytes);
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
                    Part::Released => unreachable!("consumed rope subtree"),
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
                    Part::Repeat(text, n) => {
                        for _ in 0..*n {
                            out.push_str(text);
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
        debug_assert_eq!(out.len(), self.size(content).bytes);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_budget_checks_boundaries_before_materialization() {
        // All huge values below are symbolic: this test allocates only bytes.
        assert!(check_length(MAX_STRING_LENGTH).is_ok());
        assert_eq!(
            check_length(MAX_STRING_LENGTH + 1).unwrap_err(),
            STRING_LIMIT_ERROR
        );
        let mut rope = Rope::default();
        let max = rope.repeat("=", MAX_STRING_LENGTH).unwrap();
        assert_eq!(rope.utf16_len(max), MAX_STRING_LENGTH);
        let one = rope.text("x").unwrap();
        assert_eq!(rope.concat(max, one).err().unwrap(), STRING_LIMIT_ERROR);
        assert_eq!(
            rope.repeat("=", MAX_STRING_LENGTH + 1).err().unwrap(),
            STRING_LIMIT_ERROR
        );
        assert_eq!(
            rope.flatten(max.append_newlines(1)).unwrap_err(),
            STRING_LIMIT_ERROR
        );
        assert_eq!(
            rope.indent(max.append_newlines(1), "> ").err().unwrap(),
            STRING_LIMIT_ERROR
        );
        assert!(rope.peak_text_bytes() < 100);
    }

    #[test]
    fn budget_counts_utf16_including_non_ascii_and_indented_newlines() {
        let mut rope = Rope::with_limit(13);
        let text = rope.text("中😀\nx").unwrap(); // 9 UTF-8 bytes, 5 UTF-16 units
        assert_eq!(rope.utf16_len(text), 5);
        let text = rope
            .indent(text.prepend_newlines(1).append_newlines(1), "😀")
            .unwrap();
        assert_eq!(rope.utf16_len(text), 13); // 5 + 2 newlines + 3 * 2 prefix units
        let flat = rope.flatten(text).unwrap();
        assert_eq!(flat.encode_utf16().count(), 13);
        assert_eq!(flat, "\n😀中😀\n😀x\n😀");
        assert_eq!(rope.concat(text, text).err().unwrap(), STRING_LIMIT_ERROR);
    }
}
