//! SkillManager.ts:550–627: `---` fence detection, npm `yaml` parse (default
//! options, throws on the first error) and the line-based compatibility parser.
use pilotdeck_protocol::JsString;
use pilotdeck_yaml::{
    Options, parse_document,
    value::{Container, Value, ValueGraph},
};
use regex::Regex;
use std::sync::{Arc, LazyLock};

const NL: u16 = b'\n' as u16;
const CR: u16 = b'\r' as u16;
const DASH: u16 = b'-' as u16;

/// The parsed frontmatter object. Values keep yaml's toJS graph (alias
/// sharing and cycles included); the root is always a plain object.
#[derive(Clone, Debug)]
pub(crate) struct Frontmatter {
    pub graph: ValueGraph,
    /// TS `usedCompatibilityFallback`.
    pub used_compat: bool,
}

impl Frontmatter {
    fn object(entries: Vec<(JsString, Value)>, used_compat: bool) -> Self {
        Self {
            graph: ValueGraph {
                root: Value::Ref(0),
                nodes: vec![Container::Object(entries)],
            },
            used_compat,
        }
    }
    fn entries(&self) -> &[(JsString, Value)] {
        match (&self.graph.root, &self.graph.nodes[..]) {
            (Value::Ref(id), nodes) => match &nodes[*id] {
                Container::Object(entries) => entries.as_slice(),
                _ => &[],
            },
            _ => &[],
        }
    }
    /// `Object.keys(fm).length === 0`.
    pub fn is_empty(&self) -> bool {
        self.entries().is_empty()
    }
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.entries()
            .iter()
            .find(|(k, _)| k.equals_str(key))
            .map(|(_, v)| v)
    }
    /// `typeof fm[key] === "string" ? fm[key] : undefined`.
    pub fn string(&self, key: &str) -> Option<&JsString> {
        match self.get(key)? {
            Value::String(s) => Some(s.as_ref()),
            _ => None,
        }
    }
}

/// SkillManager.ts:566 parseSkillFrontmatterWithMeta.
pub(crate) fn parse(content: &JsString) -> Frontmatter {
    let units = content.units();
    if !units.starts_with(&[DASH; 3]) {
        return Frontmatter::object(vec![], false);
    }
    // `content.slice(3).search(/\r?\n---/)`: leftmost match, so a CR directly
    // before the LF starts the match.
    let rest = &units[3..];
    let end = (0..rest.len()).find(|&i| {
        rest[i..].starts_with(&[NL, DASH, DASH, DASH])
            || rest[i..].starts_with(&[CR, NL, DASH, DASH, DASH])
    });
    let Some(end) = end else {
        return Frontmatter::object(vec![], false);
    };
    let mut raw = &rest[..end];
    // `.replace(/^\r?\n/, "")`
    if raw.starts_with(&[CR, NL]) {
        raw = &raw[2..];
    } else if raw.starts_with(&[NL]) {
        raw = &raw[1..];
    }
    let raw = JsString::from_utf16(raw.to_vec());
    match parse_yaml(&raw) {
        Some(graph) => {
            let object = matches!(
                (&graph.root, &graph.nodes[..]),
                (Value::Ref(id), nodes) if matches!(nodes[*id], Container::Object(_))
            );
            if object {
                Frontmatter {
                    graph,
                    used_compat: false,
                }
            } else {
                Frontmatter::object(vec![], false)
            }
        }
        None => {
            let entries = parse_compat(&String::from_utf16_lossy(raw.units()));
            let used = !entries.is_empty();
            Frontmatter::object(entries, used)
        }
    }
}

/// npm yaml `parse(src)`: throws `doc.errors[0]` when any error was recorded,
/// and `toJS` may throw (alias limits); both reach the TS catch as `None`.
/// Without a %YAML 1.1 directive (unreachable inside a fence) toJS and toJSON
/// produce the same values.
fn parse_yaml(raw: &JsString) -> Option<ValueGraph> {
    let doc = parse_document(raw, Options::default());
    if !doc.errors.is_empty() {
        return None;
    }
    doc.to_json().ok()
}

// JS `\s` and `.` (non-unicode RegExp), spelled out for the regex crate.
const WS: &str = r"[\t\n\x0B\x0C\r \u{a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}]";
const DOT: &str = r"[^\n\r\u{2028}\u{2029}]";
static SCALAR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"^([A-Za-z][A-Za-z0-9_-]*){WS}*:{WS}*({DOT}+?){WS}*$"
    ))
    .unwrap()
});
static KEY_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"^[A-Za-z][A-Za-z0-9_-]*{WS}*:")).unwrap());

/// SkillManager.ts:595 parseCompatFrontmatter. Lone surrogates in the fence
/// reach this path as U+FFFD (TS keeps them); see the module README.
fn parse_compat(raw: &str) -> Vec<(JsString, Value)> {
    let mut result: Vec<(JsString, Value)> = vec![];
    let mut set = |key: &str, value: &str| {
        let key = JsString::from(key);
        let value = Value::String(Arc::new(JsString::from(value)));
        match result.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = value,
            None => result.push((key, value)),
        }
    };
    let lines = split_lines(raw);
    let mut i = 0;
    while i < lines.len() {
        let Some(caps) = SCALAR.captures(lines[i]) else {
            i += 1;
            continue;
        };
        let key = caps.get(1).unwrap().as_str();
        let value = caps.get(2).unwrap().as_str();
        // `rawValue.match(/^([>|])\s*$/)`: the lazy capture already shed trailing \s.
        if value == ">" || value == "|" {
            let mut block: Vec<&str> = vec![];
            i += 1;
            while i < lines.len() {
                let next = lines[i];
                if KEY_LINE.is_match(next) {
                    break;
                }
                // `.replace(/^ {1,2}/, "")`
                let stripped = next.strip_prefix(' ').unwrap_or(next);
                let stripped = if stripped.len() < next.len() {
                    stripped.strip_prefix(' ').unwrap_or(stripped)
                } else {
                    stripped
                };
                block.push(stripped);
                i += 1;
            }
            set(key, crate::metadata::js_trim(&block.join("\n")));
            continue;
        }
        // `.trim().replace(/^["']|["']$/g, "")`
        let value = crate::metadata::js_trim(value);
        let mut chars = value.char_indices();
        let start = match chars.next() {
            Some((_, '"' | '\'')) => 1,
            _ => 0,
        };
        let end = match value[start..].chars().next_back() {
            Some('"' | '\'') => value.len() - 1,
            _ => value.len(),
        };
        set(key, &value[start..end.max(start)]);
        i += 1;
    }
    result
}

/// `split(/\r?\n/)`: a CR is part of the separator only directly before LF.
fn split_lines(raw: &str) -> Vec<&str> {
    let pieces: Vec<&str> = raw.split('\n').collect();
    let last = pieces.len() - 1;
    pieces
        .into_iter()
        .enumerate()
        .map(|(i, p)| {
            if i < last {
                p.strip_suffix('\r').unwrap_or(p)
            } else {
                p
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(fm: &Frontmatter, key: &str) -> Option<String> {
        fm.string(key).map(|s| s.to_utf8().unwrap())
    }

    #[test]
    fn compat_parser_matches_ts_regex_semantics() {
        let entries = parse_compat(
            "name: Compat: broken\ndescription: >\n  folded one\n   three spaces\nversion: '1'\nq: \"\nr: 'x\"\n9bad: no\nsp :  v  \r\nlone: a\rb",
        );
        let get = |k: &str| {
            entries
                .iter()
                .find(|(key, _)| key.equals_str(k))
                .map(|(_, v)| match v {
                    Value::String(s) => s.to_utf8().unwrap(),
                    _ => unreachable!(),
                })
        };
        assert_eq!(get("name").as_deref(), Some("Compat: broken"));
        assert_eq!(
            get("description").as_deref(),
            Some("folded one\n three spaces")
        );
        assert_eq!(get("version").as_deref(), Some("1"));
        // A lone quote is removed by the leading alternative only.
        assert_eq!(get("q").as_deref(), Some(""));
        assert_eq!(get("r").as_deref(), Some("x"));
        assert_eq!(get("9bad"), None);
        assert_eq!(get("sp").as_deref(), Some("v"));
        // `.` stops at a lone CR, so the line does not match at all.
        assert_eq!(get("lone"), None);
    }

    #[test]
    fn fences_and_fallback() {
        let fm = parse(&"---\r\nname: CRLF\r\n---\r\nbody".into());
        assert_eq!(text(&fm, "name").as_deref(), Some("CRLF"));
        assert!(parse(&"---\nname: x\n".into()).is_empty());
        assert!(parse(&"\u{feff}---\nname: x\n---".into()).is_empty());
        let fm = parse(&"---\nname: A\nname: B\n---".into());
        assert!(fm.used_compat);
        assert_eq!(text(&fm, "name").as_deref(), Some("B"));
        assert!(parse(&"---\n- a\n---".into()).is_empty());
        let fm = parse(&"---\nname: Cyc\na: &x\n  b: *x\n---".into());
        assert_eq!(text(&fm, "name").as_deref(), Some("Cyc"));
    }
}
