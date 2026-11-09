//! Incremental hard wrapping at Unicode grapheme boundaries.
use pilotdeck_protocol::{JsString, JsValue};
use ratatui::{Frame, buffer::CellWidth, layout::Rect, text::Line, widgets::Paragraph};
use unicode_segmentation::UnicodeSegmentation;

const ALLOCATION_CHUNK: usize = 256;
struct Entry {
    role: String,
    text: String,
    units: usize,
    last_unit: Option<u16>,
    starts: Vec<usize>,
}
impl Entry {
    fn bytes(&self) -> usize {
        self.role
            .capacity()
            .saturating_add(self.text.capacity())
            .saturating_add(
                self.starts
                    .capacity()
                    .saturating_mul(std::mem::size_of::<usize>()),
            )
    }
    fn push_start(&mut self, value: usize, budget: usize) -> Result<(), ()> {
        if self.starts.len() == self.starts.capacity() {
            let add = ALLOCATION_CHUNK
                .min(budget.saturating_sub(self.bytes()) / std::mem::size_of::<usize>());
            if add == 0 {
                return Err(());
            }
            self.starts.try_reserve_exact(add).map_err(|_| ())?;
            if self.bytes() > budget {
                return Err(());
            }
        }
        self.starts.push(value);
        Ok(())
    }
    fn append(&mut self, units: &[u16], width: u16, budget: usize) -> Result<(), ()> {
        if units.len() < self.units {
            return Err(());
        }
        if units.len() == self.units && !self.starts.is_empty() {
            return Ok(());
        }
        let mut start = self.units;
        if self
            .last_unit
            .is_some_and(|u| (0xd800..=0xdbff).contains(&u))
            && units
                .get(start)
                .is_some_and(|u| (0xdc00..=0xdfff).contains(u))
        {
            self.text.pop();
            start -= 1;
        }
        let addition = String::from_utf16_lossy(&units[start..]);
        let needed = self.text.len().saturating_add(addition.len());
        if needed > self.text.capacity() {
            let extra = needed - self.text.len();
            let headroom = budget.saturating_sub(self.bytes());
            if extra > headroom {
                return Err(());
            }
            let reserve = extra.max(self.text.capacity().min(headroom));
            self.text.try_reserve_exact(reserve).map_err(|_| ())?;
            if self.bytes() > budget {
                return Err(());
            }
        }
        self.text.push_str(&addition);
        self.units = units.len();
        self.last_unit = units.last().copied();
        // An append may extend the last grapheme; reflow two complete old rows.
        let keep = self.starts.len().saturating_sub(2);
        let reflow = self.starts.get(keep).copied().unwrap_or(0);
        self.starts.truncate(keep);
        self.push_start(reflow, budget)?;
        let mut column = 0usize;
        let mut cursor = reflow;
        while cursor < self.text.len() {
            let next = {
                let suffix = &self.text[cursor..];
                suffix.graphemes(true).next().expect("nonempty suffix")
            };
            let length = next.len();
            let newline = next.contains('\n');
            let control = next.chars().any(char::is_control);
            let cells = if control {
                0
            } else {
                usize::from(next.cell_width())
            };
            if newline {
                cursor += length;
                self.push_start(cursor, budget)?;
                column = 0;
                continue;
            }
            if cells > 0 && column > 0 && column.saturating_add(cells) > usize::from(width.max(1)) {
                self.push_start(cursor, budget)?;
                column = 0;
            }
            column = column.saturating_add(cells);
            cursor += length;
            // A single enormous cluster/line cannot monopolize memory or layout.
            let row_start = *self.starts.last().unwrap_or(&0);
            if cursor.saturating_sub(row_start) > 65536 {
                return Err(());
            }
        }
        Ok(())
    }
}
#[derive(Default)]
pub(super) struct ChatLayout {
    width: u16,
    entries: Vec<Entry>,
}
impl ChatLayout {
    pub fn clear(&mut self) {
        self.entries.clear();
        self.entries.shrink_to_fit();
        self.width = 0;
    }
    pub fn bytes(&self) -> usize {
        self.entries
            .capacity()
            .saturating_mul(std::mem::size_of::<Entry>())
            .saturating_add(self.entries.iter().map(Entry::bytes).sum::<usize>())
    }
    pub fn sync_step(
        &mut self,
        messages: &[JsValue],
        width: u16,
        budget: usize,
    ) -> Result<bool, ()> {
        let width = width.max(1);
        if self.width != width || self.entries.len() > messages.len() {
            self.clear();
            self.width = width;
        }
        let mut used = self.bytes();
        let mut remaining = 8192usize;
        for (index, message) in messages.iter().enumerate() {
            let role = message
                .get("role")
                .and_then(JsValue::as_string)
                .map_or_else(String::new, |s| String::from_utf16_lossy(s.units()));
            let selected = if role == "tool"
                && message.get("expanded").and_then(JsValue::as_bool) == Some(true)
            {
                match message.get("fullText") {
                    Some(JsValue::Null | JsValue::Undefined) | None => message.get("text"),
                    value => value,
                }
            } else {
                message.get("text")
            };
            let units = selected
                .and_then(JsValue::as_string)
                .map_or(&[][..], JsString::units);
            if index < self.entries.len()
                && (self.entries[index].role != role || self.entries[index].units > units.len())
            {
                self.clear();
                self.width = width;
                return self.sync_step(messages, width, budget);
            }
            if index == self.entries.len() {
                let old_capacity = self.entries.capacity();
                if index == old_capacity {
                    self.entries.try_reserve_exact(16).map_err(|_| ())?;
                }
                used = used.saturating_add(
                    (self.entries.capacity() - old_capacity)
                        .saturating_mul(std::mem::size_of::<Entry>()),
                );
                self.entries.push(Entry {
                    role: role.clone(),
                    text: if role.is_empty() {
                        String::new()
                    } else {
                        format!("{role}: ")
                    },
                    units: 0,
                    last_unit: None,
                    starts: Vec::new(),
                });
                used = used.saturating_add(self.entries[index].bytes());
            }
            let old = self.entries[index].bytes();
            let allowed = budget.checked_sub(used.saturating_sub(old)).ok_or(())?;
            let old_units = self.entries[index].units;
            let mut end = old_units.saturating_add(remaining).min(units.len());
            if end < units.len()
                && end > old_units
                && (0xd800..=0xdbff).contains(&units[end - 1])
                && (0xdc00..=0xdfff).contains(&units[end])
            {
                end -= 1;
            }
            if end == old_units && end < units.len() {
                return Ok(false);
            }
            self.entries[index].append(&units[..end], width, allowed)?;
            remaining = remaining.saturating_sub(end - old_units);
            used = used
                .saturating_sub(old)
                .saturating_add(self.entries[index].bytes());
            if used > budget {
                return Err(());
            }
            if end < units.len() {
                return Ok(false);
            }
            if remaining == 0 && index + 1 < messages.len() {
                return Ok(false);
            }
        }
        Ok(true)
    }
    #[cfg(test)]
    pub(super) fn sync(
        &mut self,
        messages: &[JsValue],
        width: u16,
        budget: usize,
    ) -> Result<(), ()> {
        while !self.sync_step(messages, width, budget)? {}
        Ok(())
    }
    pub fn total_rows(&self) -> usize {
        self.entries.iter().map(|entry| entry.starts.len()).sum()
    }
    pub fn scroll_for_message(&self, index: usize, height: usize) -> usize {
        let before: usize = self
            .entries
            .iter()
            .take(index)
            .map(|entry| entry.starts.len())
            .sum();
        self.total_rows()
            .saturating_sub(height)
            .saturating_sub(before)
    }
    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, scroll: usize) {
        let top = self
            .total_rows()
            .saturating_sub(area.height as usize)
            .saturating_sub(scroll);
        self.render_top(frame, area, top);
    }
    pub fn render_top(&self, frame: &mut Frame<'_>, area: Rect, top: usize) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let skip = top.min(self.total_rows().saturating_sub(area.height as usize));
        let mut lines = Vec::with_capacity(area.height as usize);
        let mut ordinal = 0usize;
        for entry in &self.entries {
            if ordinal.saturating_add(entry.starts.len()) <= skip {
                ordinal += entry.starts.len();
                continue;
            }
            for (i, start) in entry.starts.iter().enumerate() {
                if lines.len() >= area.height as usize {
                    break;
                }
                if ordinal + i < skip {
                    continue;
                }
                let end = entry.starts.get(i + 1).copied().unwrap_or(entry.text.len());
                let row = entry.text[*start..end].trim_end_matches(['\r', '\n']);
                lines.push(Line::raw(row.to_owned()));
            }
            ordinal += entry.starts.len();
            if lines.len() >= area.height as usize {
                break;
            }
        }
        frame.render_widget(Paragraph::new(lines), area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pilotdeck_protocol::JsObject;
    fn message(value: &str) -> JsValue {
        let mut object = JsObject::new();
        object.insert("role".into(), "assistant".into());
        object.insert("text".into(), value.into());
        JsValue::Object(object)
    }
    #[test]
    fn long_line_is_continuous_across_old_chunk_boundary() {
        let mut layout = ChatLayout::default();
        let rows = vec![message(&"x".repeat(9000))];
        layout.sync(&rows, 80, 8 * 1024 * 1024).unwrap();
        let entry = &layout.entries[0];
        assert_eq!(entry.starts.len(), entry.text.len().div_ceil(80));
        assert!(entry.starts.iter().all(|position| position % 80 == 0));
    }
    #[test]
    fn incremental_unicode_clusters_and_resize_match_fresh_layout() {
        let mut layout = ChatLayout::default();
        let mut content = "a".repeat(4095);
        for part in ["👩", "\u{200d}", "💻", "e", "\u{301}", "🇺", "🇸", "\nEND"] {
            content.push_str(part);
            layout
                .sync(&[message(&content)], 80, 8 * 1024 * 1024)
                .unwrap();
        }
        let entry = &layout.entries[0];
        let boundaries: std::collections::HashSet<usize> = entry
            .text
            .grapheme_indices(true)
            .map(|(i, _)| i)
            .chain(std::iter::once(entry.text.len()))
            .collect();
        assert!(
            entry
                .starts
                .iter()
                .all(|offset| boundaries.contains(offset)),
            "line start splits grapheme"
        );
        layout
            .sync(&[message(&content)], 17, 8 * 1024 * 1024)
            .unwrap();
        let mut fresh = ChatLayout::default();
        fresh
            .sync(&[message(&content)], 17, 8 * 1024 * 1024)
            .unwrap();
        assert_eq!(layout.entries[0].text, fresh.entries[0].text);
        assert_eq!(layout.entries[0].starts, fresh.entries[0].starts);
    }
    #[test]
    fn incremental_regional_indicators_and_zwj_match_whole_text() {
        for width in [1, 2, 3, 4, 5, 17] {
            let mut incremental = ChatLayout::default();
            let mut content = String::new();
            for i in 0..90 {
                content.push_str(match i % 9 {
                    0 => "👩",
                    1 => "\u{200d}",
                    2 => "💻",
                    3 => "e",
                    4 => "\u{301}",
                    _ => "🇺",
                });
                let messages = [message(&content)];
                incremental.sync(&messages, width, 8 * 1024 * 1024).unwrap();
                let mut fresh = ChatLayout::default();
                fresh.sync(&messages, width, 8 * 1024 * 1024).unwrap();
                assert_eq!(
                    incremental.entries[0].starts, fresh.entries[0].starts,
                    "width {width}, input {i}"
                );
            }
        }
    }
    #[test]
    fn split_surrogate_pair_reflows_without_replacement() {
        let make = |units: Vec<u16>| {
            let mut object = JsObject::new();
            object.insert("role".into(), "assistant".into());
            object.insert("text".into(), JsValue::String(JsString::from_utf16(units)));
            JsValue::Object(object)
        };
        let mut incremental = ChatLayout::default();
        incremental
            .sync(&[make(vec![0xd83d])], 3, 8 * 1024 * 1024)
            .unwrap();
        incremental
            .sync(&[make(vec![0xd83d, 0xde00])], 3, 8 * 1024 * 1024)
            .unwrap();
        let mut fresh = ChatLayout::default();
        fresh
            .sync(&[make(vec![0xd83d, 0xde00])], 3, 8 * 1024 * 1024)
            .unwrap();
        assert_eq!(incremental.entries[0].text, fresh.entries[0].text);
        assert_eq!(incremental.entries[0].starts, fresh.entries[0].starts);
        assert!(incremental.entries[0].text.ends_with('😀'));
    }
    #[test]
    fn layout_steps_are_bounded_and_keep_split_surrogates_intact() {
        let text = format!("{}😀{}", "x".repeat(8191), "z".repeat(20000));
        let messages = [message(&text)];
        let mut layout = ChatLayout::default();
        assert!(!layout.sync_step(&messages, 80, 8 * 1024 * 1024).unwrap());
        assert_eq!(layout.entries[0].units, 8191);
        let mut previous = layout.entries[0].units;
        loop {
            let ready = layout.sync_step(&messages, 80, 8 * 1024 * 1024).unwrap();
            let current = layout.entries[0].units;
            assert!(current - previous <= 8192);
            previous = current;
            if ready {
                break;
            }
        }
        assert!(layout.entries[0].text.contains('😀'));
        let mut fresh = ChatLayout::default();
        fresh.sync(&messages, 80, 8 * 1024 * 1024).unwrap();
        assert_eq!(layout.entries[0].starts, fresh.entries[0].starts);
    }
    #[test]
    fn cache_cannot_exceed_its_budget() {
        let mut layout = ChatLayout::default();
        assert!(
            layout
                .sync(&[message(&"x\n".repeat(10000))], 5, 4096)
                .is_err()
        );
    }
}
