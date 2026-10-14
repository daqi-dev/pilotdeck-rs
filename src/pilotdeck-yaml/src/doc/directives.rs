// Port of yaml@2.9.0 dist/doc/directives.js (ISC); parsing operations.
use crate::string::{concat, message, slice, trim};
use pilotdeck_protocol::JsString;
use std::{collections::HashMap, ops::Deref, sync::Arc};

/// Ordered declarations with a handle index. Updating a handle keeps its slot,
/// matching assignment to an existing property in directives.js.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TagTable {
    entries: Vec<(JsString, JsString)>,
    index: HashMap<JsString, usize>,
}
impl Default for TagTable {
    fn default() -> Self {
        Self {
            entries: vec![("!!".into(), "tag:yaml.org,2002:".into())],
            index: HashMap::from([("!!".into(), 0)]),
        }
    }
}
impl Deref for TagTable {
    type Target = [(JsString, JsString)];
    fn deref(&self) -> &Self::Target {
        &self.entries
    }
}
impl TagTable {
    pub fn insert(&mut self, handle: JsString, prefix: JsString) {
        if let Some(&slot) = self.index.get(&handle) {
            self.entries[slot].1 = prefix;
        } else {
            self.index.insert(handle.clone(), self.entries.len());
            self.entries.push((handle, prefix));
        }
    }
    fn prefix(&self, handle: &JsString) -> Option<&JsString> {
        self.index.get(handle).map(|&slot| &self.entries[slot].1)
    }
}
#[derive(Clone, Debug)]
pub struct Directives {
    pub doc_start: bool,
    pub doc_end: bool,
    pub explicit: bool,
    pub version: &'static str,
    pub tags: Arc<TagTable>,
    at_next_document: bool,
}
impl Default for Directives {
    fn default() -> Self {
        Self {
            doc_start: false,
            doc_end: false,
            explicit: false,
            version: "1.2",
            tags: {
                static DEFAULT: std::sync::LazyLock<Arc<TagTable>> =
                    std::sync::LazyLock::new(|| Arc::new(TagTable::default()));
                DEFAULT.clone()
            },
            at_next_document: false,
        }
    }
}
impl Directives {
    pub(crate) fn at_document(&mut self) -> Self {
        let mut res = self.clone();
        res.doc_start = false;
        res.doc_end = false;
        res.at_next_document = false;
        if self.version == "1.1" {
            self.at_next_document = true;
        } else {
            *self = Self::default();
        }
        res
    }
    pub(crate) fn add(&mut self, line: &JsString) -> Vec<(usize, JsString, bool)> {
        if self.at_next_document {
            *self = Self {
                version: "1.1",
                ..Self::default()
            };
        }
        let trimmed = trim(line);
        let parts: Vec<JsString> = trimmed
            .units()
            .split(|c| matches!(c, 32 | 9))
            .filter(|s| !s.is_empty())
            .map(|s| JsString::from_utf16(s.to_vec()))
            .collect();
        let mut errors = Vec::new();
        let Some(name) = parts.first() else {
            return errors;
        };
        let args = &parts[1..];
        if name.equals_str("%TAG") {
            if args.len() != 2 {
                errors.push((
                    0,
                    "%TAG directive should contain exactly two parts".into(),
                    false,
                ));
                if args.len() < 2 {
                    return errors;
                }
            }
            Arc::make_mut(&mut self.tags).insert(args[0].clone(), args[1].clone());
        } else if name.equals_str("%YAML") {
            self.explicit = true;
            if args.len() != 1 {
                errors.push((
                    0,
                    "%YAML directive should contain exactly one part".into(),
                    false,
                ));
                return errors;
            }
            if args[0].equals_str("1.1") {
                self.version = "1.1";
            } else if args[0].equals_str("1.2") {
                self.version = "1.2";
            } else {
                let split: Vec<_> = args[0].units().split(|c| *c == 46).collect();
                let valid = split.len() == 2
                    && split
                        .iter()
                        .all(|s| !s.is_empty() && s.iter().all(|c| matches!(c, 48..=57)));
                errors.push((6, message("Unsupported YAML version ", &args[0]), valid));
            }
        } else {
            errors.push((0, message("Unknown directive ", name), true));
        }
        errors
    }
    pub(crate) fn tag_name(&self, source: &JsString) -> (Option<JsString>, Vec<JsString>) {
        let mut errors = Vec::new();
        if source.equals_str("!") {
            return (Some(source.clone()), errors);
        }
        if source.units().first() != Some(&33) {
            errors.push(message("Not a valid tag: ", source));
            return (None, errors);
        }
        if source.units().get(1) == Some(&60) {
            let verbatim = slice(source, 2, source.len_utf16().saturating_sub(1));
            if verbatim.equals_str("!") || verbatim.equals_str("!!") {
                errors.push(concat(&[
                    &"Verbatim tags aren't resolved, so ".into(),
                    source,
                    &" is invalid.".into(),
                ]));
                return (None, errors);
            }
            if source.units().last() != Some(&62) {
                errors.push("Verbatim tags must end with a >".into());
            }
            return (Some(verbatim), errors);
        }
        let i = source.units().iter().rposition(|c| *c == 33).unwrap() + 1;
        let handle = slice(source, 0, i);
        let suffix = slice(source, i, source.len_utf16());
        if suffix.is_empty() {
            errors.push(concat(&[
                &"The ".into(),
                source,
                &" tag has no suffix".into(),
            ]));
        }
        if let Some(prefix) = self.tags.prefix(&handle).filter(|p| !p.is_empty()) {
            if let Some(suffix) = decode_uri_component(&suffix) {
                return (Some(concat(&[prefix, &suffix])), errors);
            }
            errors.push("URIError: URI malformed".into());
            return (None, errors);
        }
        if handle.equals_str("!") {
            return (Some(source.clone()), errors);
        }
        errors.push(message("Could not resolve tag: ", source));
        (None, errors)
    }
}
fn decode_uri_component(s: &JsString) -> Option<JsString> {
    let mut res = Vec::new();
    let units = s.units();
    let mut i = 0;
    while i < units.len() {
        if units[i] != 37 {
            res.push(units[i]);
            i += 1;
            continue;
        }
        let mut bytes = Vec::new();
        while units.get(i) == Some(&37) {
            let a = char::from_u32(*units.get(i + 1)? as u32)?.to_digit(16)?;
            let b = char::from_u32(*units.get(i + 2)? as u32)?.to_digit(16)?;
            bytes.push((a * 16 + b) as u8);
            i += 3;
        }
        res.extend(std::str::from_utf8(&bytes).ok()?.encode_utf16());
    }
    Some(JsString::from_utf16(res))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_handles_keep_order_and_last_prefix_in_cloned_documents() {
        let mut stream = Directives::default();
        for line in [
            "%YAML 1.1",
            "%TAG !a! tag:first:",
            "%TAG !b! tag:b:",
            "%TAG !a! tag:last:",
        ] {
            assert!(stream.add(&line.into()).is_empty());
        }
        let first = stream.at_document();
        let second = stream.at_document();
        assert!(Arc::ptr_eq(&first.tags, &second.tags));
        assert_eq!(
            first
                .tags
                .iter()
                .map(|(h, _)| h.to_utf8().unwrap())
                .collect::<Vec<_>>(),
            ["!!", "!a!", "!b!"]
        );
        assert_eq!(
            first.tag_name(&"!a!x%20y".into()),
            (Some("tag:last:x y".into()), vec![])
        );
        assert!(stream.add(&"%TAG !a! tag:next:".into()).is_empty());
        let third = stream.at_document();
        assert_eq!(third.tags.len(), 2);
        assert_eq!(third.tag_name(&"!a!x".into()).0, Some("tag:next:x".into()));
        assert_eq!(first.tag_name(&"!a!x".into()).0, Some("tag:last:x".into()));
        assert_eq!(
            third.tag_name(&"!b!x".into()).1,
            [JsString::from("Could not resolve tag: !b!x")]
        );
    }

    #[test]
    fn yaml12_resets_handles_and_index_preserves_raw_utf16() {
        let mut stream = Directives::default();
        let handle = JsString::from_utf16(vec![33, 0xd800, 33]);
        Arc::make_mut(&mut stream.tags).insert(handle.clone(), "tag:raw:".into());
        let mut source = handle.units().to_vec();
        source.push(120);
        let source = JsString::from_utf16(source);
        assert_eq!(stream.tag_name(&source).0, Some("tag:raw:x".into()));
        let doc = stream.at_document();
        assert_eq!(doc.tags.len(), 2);
        assert_eq!(stream.tags.len(), 1);
        assert!(stream.tag_name(&source).0.is_none());
        assert_eq!(
            doc.tag_name(&"!!str".into()).0,
            Some("tag:yaml.org,2002:str".into())
        );
    }
}
