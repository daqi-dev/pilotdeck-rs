use pilotdeck_protocol::JsValue;
use ratatui::buffer::CellWidth;
use std::cmp::Ordering;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum GroupBy {
    Project,
    Status,
}
struct Group {
    anchor: usize,
    items: Vec<usize>,
    collapsed: bool,
}
#[derive(Clone, Copy)]
pub(super) enum Row {
    Header(usize),
    Session(usize, usize),
}
pub(super) struct Sidebar {
    groups: Vec<Group>,
    rows: Vec<Row>,
    cursor: usize,
    by: GroupBy,
}
fn value(s: &JsValue, by: GroupBy) -> Option<&JsValue> {
    let v = s.get(match by {
        GroupBy::Project => "cwd",
        GroupBy::Status => "tag",
    });
    match v {
        Some(JsValue::Null | JsValue::Undefined) | None => None,
        _ => v,
    }
}
fn key(s: &JsValue) -> Option<&JsValue> {
    match s.get("sessionKey") {
        Some(JsValue::Null | JsValue::Undefined) | None => s.get("sessionId"),
        v => v,
    }
}
fn status_rank(s: &JsValue) -> usize {
    let units = s.get("tag").and_then(JsValue::as_string).map(|s| s.units());
    for (index, tag) in ["running", "waiting", "background", "idle", "done", "error"]
        .iter()
        .enumerate()
    {
        if units.is_some_and(|u| u.iter().copied().eq(tag.encode_utf16())) {
            return index;
        }
    }
    if units.is_none() { 3 } else { 6 }
}
fn same_group(a: &JsValue, b: &JsValue, by: GroupBy) -> bool {
    let (a, b) = (value(a, by), value(b, by));
    if a == b {
        return true;
    }
    let fallback = match by {
        GroupBy::Project => "(unknown)",
        GroupBy::Status => "idle",
    };
    let matches = |v: Option<&JsValue>| {
        v.and_then(JsValue::as_string)
            .is_some_and(|s| s.units().iter().copied().eq(fallback.encode_utf16()))
    };
    (a.is_none() && matches(b)) || (b.is_none() && matches(a))
}
pub fn valid(sessions: &[JsValue]) -> bool {
    sessions.len() <= 256
        && sessions.iter().all(|s| {
            ["cwd", "tag"].iter().all(|k| match s.get(k) {
                None | Some(JsValue::Null | JsValue::Undefined) => true,
                Some(JsValue::String(v)) => v.units().len() <= 4096,
                _ => false,
            })
        })
}
fn modified(s: &JsValue) -> f64 {
    s.get("lastModified")
        .and_then(JsValue::as_number)
        .filter(|v| v.is_finite())
        .unwrap_or(0.0)
}
fn preview(s: Option<&JsValue>, fallback: &str) -> String {
    let Some(JsValue::String(v)) = s else {
        return fallback.into();
    };
    let units = v.units();
    let end = units.len().min(256);
    let mut out = String::from_utf16_lossy(&units[..end]);
    if units.len() > end {
        out.push('…');
    }
    out
}
fn group_label(s: &JsValue, by: GroupBy) -> String {
    if by == GroupBy::Status {
        return preview(value(s, by), "idle");
    }
    let Some(JsValue::String(v)) = value(s, by) else {
        return "(unknown)".into();
    };
    let units = v.units();
    let start = units
        .iter()
        .rposition(|u| *u == b'/' as u16)
        .map_or(0, |i| i + 1);
    let tail = &units[start..];
    let mut result = String::from_utf16_lossy(&tail[..tail.len().min(256)]);
    if tail.len() > 256 {
        result.push('…');
    }
    result
}
fn title(s: &JsValue) -> String {
    for field in [
        "customTitle",
        "aiTitle",
        "firstPrompt",
        "summary",
        "sessionId",
    ] {
        if let Some(JsValue::String(value)) = s.get(field)
            && !value.units().is_empty()
        {
            return preview(s.get(field), "");
        }
    }
    String::new()
}
fn single_line(text: &str, width: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for grapheme in text.graphemes(true) {
        let safe = if grapheme.chars().any(char::is_control) {
            " "
        } else {
            grapheme
        };
        let cells = usize::from(safe.cell_width());
        if used + cells > width {
            break;
        }
        out.push_str(safe);
        used += cells;
    }
    out
}
pub fn summary(s: &JsValue) -> String {
    preview(s.get("summary"), "")
}
impl Default for Sidebar {
    fn default() -> Self {
        Self {
            groups: vec![],
            rows: vec![],
            cursor: 0,
            by: GroupBy::Project,
        }
    }
}
impl Sidebar {
    pub fn release(&mut self) {
        self.groups = Vec::new();
        self.rows = Vec::new();
        self.cursor = 0;
    }
    pub fn rebuild(&mut self, sessions: &[JsValue], active: &str) {
        self.groups.clear();
        self.rows.clear();
        self.cursor = 0;
        for (i, item) in sessions.iter().enumerate() {
            if let Some(group) = self
                .groups
                .iter_mut()
                .find(|g| same_group(&sessions[g.anchor], item, self.by))
            {
                group.items.push(i);
            } else {
                self.groups.push(Group {
                    anchor: i,
                    items: vec![i],
                    collapsed: false,
                });
            }
        }
        self.groups.sort_by(|a, b| match self.by {
            GroupBy::Project => {
                let max = |g: &Group| {
                    g.items
                        .iter()
                        .map(|i| modified(&sessions[*i]))
                        .fold(f64::NEG_INFINITY, f64::max)
                };
                max(b).partial_cmp(&max(a)).unwrap_or(Ordering::Equal)
            }
            GroupBy::Status => {
                status_rank(&sessions[a.anchor]).cmp(&status_rank(&sessions[b.anchor]))
            }
        });
        if self.groups.len() > 1 {
            for group in &mut self.groups {
                group.collapsed = !group.items.iter().any(|i| {
                    key(&sessions[*i])
                        .and_then(JsValue::as_string)
                        .is_some_and(|v| v.units().iter().copied().eq(active.encode_utf16()))
                });
            }
        }
        self.flatten();
    }
    fn flatten(&mut self) {
        self.rows.clear();
        let single = self.groups.len() == 1;
        for (i, g) in self.groups.iter().enumerate() {
            if !single {
                self.rows.push(Row::Header(i));
            }
            if !g.collapsed {
                self.rows
                    .extend(g.items.iter().map(|idx| Row::Session(i, *idx)));
            }
        }
        self.cursor = self.cursor.min(self.rows.len().saturating_sub(1));
    }
    pub fn cycle(&mut self, sessions: &[JsValue], active: &str) {
        self.by = if self.by == GroupBy::Project {
            GroupBy::Status
        } else {
            GroupBy::Project
        };
        self.rebuild(sessions, active);
    }
    pub fn up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }
    pub fn down(&mut self) {
        self.cursor = (self.cursor + 1).min(self.rows.len().saturating_sub(1));
    }
    pub fn activate(&mut self) -> Option<usize> {
        match self.rows.get(self.cursor).copied() {
            Some(Row::Header(g)) => {
                self.groups[g].collapsed = !self.groups[g].collapsed;
                self.flatten();
                None
            }
            Some(Row::Session(_, i)) => Some(i),
            None => None,
        }
    }
    pub fn right(&mut self) -> Option<usize> {
        match self.rows.get(self.cursor).copied() {
            Some(Row::Header(g)) => {
                self.groups[g].collapsed = false;
                self.flatten();
                None
            }
            Some(Row::Session(_, i)) => Some(i),
            None => None,
        }
    }
    pub fn left(&mut self) {
        match self.rows.get(self.cursor).copied() {
            Some(Row::Header(g)) => {
                self.groups[g].collapsed = true;
                self.flatten();
            }
            Some(Row::Session(g, _)) => {
                if let Some(i) = self
                    .rows
                    .iter()
                    .position(|r| matches!(r,Row::Header(n) if *n==g))
                {
                    self.cursor = i;
                }
            }
            None => {}
        }
    }
    pub fn bytes(&self) -> usize {
        self.groups.capacity() * std::mem::size_of::<Group>()
            + self.rows.capacity() * std::mem::size_of::<Row>()
            + self
                .groups
                .iter()
                .map(|g| g.items.capacity() * std::mem::size_of::<usize>())
                .sum::<usize>()
    }
    pub fn lines(&self, sessions: &[JsValue], height: usize, width: usize) -> Vec<String> {
        let start = self.cursor.saturating_add(1).saturating_sub(height);
        self.rows
            .iter()
            .enumerate()
            .skip(start)
            .take(height)
            .map(|(i, row)| {
                let p = if i == self.cursor { ">" } else { " " };
                match row {
                    Row::Header(g) => {
                        let group = &self.groups[*g];
                        let label = group_label(&sessions[group.anchor], self.by);
                        format!(
                            "{p} {} {label} ({})",
                            if group.collapsed { "+" } else { "-" },
                            group.items.len()
                        )
                    }
                    Row::Session(_, n) => {
                        let s = &sessions[*n];
                        format!("{p} {}", title(s))
                    }
                }
            })
            .map(|line| single_line(&line, width))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    fn serialized(side: &Sidebar, sessions: &[JsValue]) -> Value {
        Value::Array(
            side.rows
                .iter()
                .map(|r| match r {
                    Row::Header(i) => {
                        let g = &side.groups[*i];
                        let mut name = preview(value(&sessions[g.anchor], side.by), "idle");
                        if side.by == GroupBy::Status {
                            name = format!("status:{name}");
                        }
                        json!({"kind":"header","key":name,"collapsed":g.collapsed})
                    }
                    Row::Session(_, i) => {
                        json!({"kind":"session","key":preview(key(&sessions[*i]),"")})
                    }
                })
                .collect(),
        )
    }
    #[test]
    fn long_project_prefix_displays_real_basename() {
        let path = format!("/{}/short", "a".repeat(400));
        let input = pilotdeck_protocol::parse_json(&format!(r#"{{"cwd":"{path}"}}"#)).unwrap();
        assert_eq!(group_label(&input, GroupBy::Project), "short");
    }
    #[test]
    fn release_frees_sidebar_budget_after_large_list() {
        let sessions: Vec<_> = (0..256)
            .map(|i| {
                pilotdeck_protocol::parse_json(&format!(r#"{{"cwd":"/p/{i}","sessionId":"{i}"}}"#))
                    .unwrap()
            })
            .collect();
        let mut side = Sidebar::default();
        side.rebuild(&sessions, "0");
        assert!(side.bytes() > 0);
        side.release();
        assert_eq!(side.bytes(), 0);
        side.rebuild(&sessions[..1], "0");
        assert!(matches!(side.rows.first(), Some(Row::Session(_, 0))));
    }
    #[test]
    fn fixed_ts_title_priority_and_single_line_visibility() {
        let data=pilotdeck_protocol::parse_json(r#"[{"customTitle":"CUSTOM","aiTitle":"AI","firstPrompt":"FIRST","summary":"SUMMARY","sessionId":"ID"},{"customTitle":"","aiTitle":"AI","summary":"SUMMARY","sessionId":"ID"},{"aiTitle":"","firstPrompt":"FIRST","sessionId":"ID"},{"firstPrompt":"","summary":"SUMMARY","sessionId":"ID"},{"summary":"","sessionId":"ID"},{"summary":null,"sessionId":"ID"}]"#).unwrap();
        let JsValue::Array(items) = &data else {
            panic!("array")
        };
        let expected: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/tui-sidebar/expected.json"
        ))
        .unwrap();
        let actual: Vec<String> = items.iter().map(title).collect();
        assert_eq!(json!(actual), expected["titles"]);
        let mut side = Sidebar::default();
        side.rebuild(items, "");
        for _ in 0..5 {
            side.down();
        }
        let lines = side.lines(items, 2, 8);
        assert_eq!(lines.len(), 2);
        assert!(lines[1].starts_with("> ID"));
        assert!(lines.iter().all(|s| s.cell_width() <= 8));
    }
    #[test]
    fn fixed_ts_group_oracle_and_navigation() {
        let input=pilotdeck_protocol::parse_json(r#"[{"sessionKey":"a","cwd":"/p/alpha","tag":"running","lastModified":20},{"sessionKey":"b","cwd":"/p/beta","tag":"idle","lastModified":10},{"sessionKey":"c","cwd":"/p/alpha","tag":"waiting","lastModified":30}]"#).unwrap();
        let JsValue::Array(sessions) = &input else {
            panic!("array")
        };
        let expected: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/tui-sidebar/expected.json"
        ))
        .unwrap();
        let mut side = Sidebar::default();
        for (index, by) in [GroupBy::Project, GroupBy::Status].into_iter().enumerate() {
            side.by = by;
            side.rebuild(sessions, "a");
            assert_eq!(
                serialized(&side, sessions),
                expected["groups"][index]["before"]
            );
            for g in &mut side.groups {
                g.collapsed = false;
            }
            side.flatten();
            assert_eq!(
                serialized(&side, sessions),
                expected["groups"][index]["expanded"]
            );
        }
        side.by = GroupBy::Project;
        side.rebuild(sessions, "a");
        side.down();
        assert_eq!(side.activate(), Some(0));
        side.left();
        assert!(matches!(side.rows[side.cursor], Row::Header(0)));
        assert_eq!(side.activate(), None);
        assert_eq!(side.right(), None);
        side.down();
        assert_eq!(side.right(), Some(0));
    }
}
