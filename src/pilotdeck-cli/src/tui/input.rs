use unicode_segmentation::UnicodeSegmentation;

pub(super) fn previous(text: &str, cursor: usize) -> usize {
    text.grapheme_indices(true)
        .map(|(i, _)| i)
        .take_while(|i| *i < cursor)
        .last()
        .unwrap_or(0)
}
pub(super) fn next(text: &str, cursor: usize) -> usize {
    text.grapheme_indices(true)
        .map(|(i, _)| i)
        .find(|i| *i > cursor)
        .unwrap_or(text.len())
}
fn boundary_at_or_after(text: &str, offset: usize) -> usize {
    text.grapheme_indices(true)
        .map(|(i, _)| i)
        .find(|i| *i >= offset)
        .unwrap_or(text.len())
}
pub(super) fn insert(text: &mut String, cursor: &mut usize, value: &str) -> bool {
    if text.len().saturating_add(value.len()) > 65536 {
        return false;
    }
    text.insert_str(*cursor, value);
    *cursor = boundary_at_or_after(text, *cursor + value.len());
    true
}
pub(super) fn delete_previous(text: &mut String, cursor: &mut usize) {
    let start = previous(text, *cursor);
    text.replace_range(start..*cursor, "");
    *cursor = boundary_at_or_after(text, start);
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn edits_preserve_graphemes_and_limits() {
        let mut s = "A😀B".to_owned();
        let mut c = previous(&s, s.len());
        delete_previous(&mut s, &mut c);
        assert_eq!(s, "AB");
        let mut s = "Ae\u{301}B".to_owned();
        let mut c = previous(&s, s.len());
        delete_previous(&mut s, &mut c);
        assert_eq!(s, "AB");
        let mut s = "A😀B".to_owned();
        let mut c = previous(&s, previous(&s, s.len()));
        assert!(insert(&mut s, &mut c, "X"));
        assert_eq!(s, "AX😀B");
        let mut s = "e".to_owned();
        let mut c = 1;
        assert!(insert(&mut s, &mut c, "\u{301}"));
        assert_eq!(previous(&s, c), 0);
        let mut s = "a".repeat(65536);
        let mut c = s.len();
        assert!(!insert(&mut s, &mut c, "x"));
        assert_eq!(s.len(), 65536);
    }
}
