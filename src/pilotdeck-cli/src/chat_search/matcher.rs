//! Search matching and JS diagnostic spelling for invalid patterns.
use crate::Failure;
use fancy_regex::{Error, ParseError, Regex, RegexBuilder};

pub(super) enum Matcher {
    Literal { needle: String, sensitive: bool },
    Regex { pattern: Regex, sensitive: bool },
}
impl Matcher {
    pub(super) fn new(query: &str, regex: bool, sensitive: bool) -> Result<Self, Failure> {
        if !regex {
            return Ok(Self::Literal {
                needle: if sensitive {
                    query.into()
                } else {
                    lower(query)
                },
                sensitive,
            });
        }
        let pattern = RegexBuilder::new(&translate(query, sensitive))
            .build()
            .map_err(|error| {
                let detail = match &error {
                    Error::ParseError(
                        _,
                        ParseError::InvalidRepeat | ParseError::TargetNotRepeatable,
                    ) => "Nothing to repeat",
                    Error::ParseError(_, ParseError::UnclosedOpenParen) => "Unterminated group",
                    Error::ParseError(_, ParseError::InvalidClass) => {
                        "Unterminated character class"
                    }
                    _ => {
                        let message = format!("{error:?}").to_lowercase();
                        if message.contains("range") {
                            "Range out of order in character class"
                        } else if message.contains("repetitionmissing")
                            || message.contains("repetition operator missing expression")
                        {
                            "Nothing to repeat"
                        } else if message.contains("unclosed character class") {
                            "Unterminated character class"
                        } else {
                            "Invalid regular expression"
                        }
                    }
                };
                Failure::Error(format!(
                    "Invalid regular expression: /{query}/{}: {detail}",
                    if sensitive { "" } else { "i" }
                ))
            })?;
        Ok(Self::Regex { pattern, sensitive })
    }
    pub(super) fn find(&self, text: &str) -> Result<Option<usize>, Failure> {
        match self {
            Self::Literal { needle, sensitive } => {
                let haystack = if *sensitive { text.into() } else { lower(text) };
                Ok(haystack
                    .find(needle)
                    .map(|i| haystack[..i].encode_utf16().count()))
            }
            Self::Regex { pattern, sensitive } => {
                let subject: String = text.encode_utf16().map(|u| unit(u, *sensitive)).collect();
                pattern
                    .find(&subject)
                    .map(|found| found.map(|m| subject[..m.start()].chars().count()))
                    .map_err(|e| Failure::Error(e.to_string()))
            }
        }
    }
}
fn lower(text: &str) -> String {
    icu_casemap::CaseMapper::new()
        .lowercase_to_string(text, &icu_locale_core::LanguageIdentifier::UNKNOWN)
        .into_owned()
}

// Map surrogate code units to a disjoint scalar range so the regex engine
// consumes exactly one JS code unit per character, even without the /u flag.
fn unit(value: u16, sensitive: bool) -> char {
    let c = char::from_u32(u32::from(value))
        .unwrap_or_else(|| char::from_u32(0xf0000 + u32::from(value)).unwrap());
    if sensitive {
        return c;
    }
    let mut upper = c.to_uppercase();
    let first = upper.next().unwrap();
    if upper.next().is_some() || (!c.is_ascii() && first.is_ascii()) {
        c
    } else {
        first
    }
}
const SPACE: &str =
    r"\x09-\x0d\x20\xa0\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}";
fn escaped_unit(value: u16, sensitive: bool) -> String {
    format!("\\x{{{:x}}}", u32::from(unit(value, sensitive)))
}
fn translate(pattern: &str, sensitive: bool) -> String {
    let units: Vec<_> = pattern.encode_utf16().collect();
    let mut out = String::new();
    let (mut index, mut class) = (0, false);
    while index < units.len() {
        let c = units[index];
        index += 1;
        match c {
            92 if index < units.len() => {
                let c = units[index];
                index += 1;
                match c {
                    100 => out.push_str("[0-9]"),
                    68 => out.push_str("[^0-9]"),
                    119 => out.push_str("[A-Za-z0-9_]"),
                    87 => out.push_str("[^A-Za-z0-9_]"),
                    115 => out.push_str(&format!("[{SPACE}]")),
                    83 => out.push_str(&format!("[^{SPACE}]")),
                    98 if class => out.push_str(r"\x08"),
                    98 | 66 => {
                        let word = "[A-Za-z0-9_]";
                        let boundary = format!("(?:(?<!{word})(?={word})|(?<={word})(?!{word}))");
                        if c == 98 {
                            out.push_str(&boundary);
                        } else {
                            out.push_str(&format!("(?!{boundary})"));
                        }
                    }
                    120 | 117 => {
                        let count = if c == 120 { 2 } else { 4 };
                        let digits: String = units[index..]
                            .iter()
                            .take(count)
                            .map(|u| char::from_u32(u32::from(*u)).unwrap_or(' '))
                            .collect();
                        if digits.len() == count
                            && let Ok(value) = u16::from_str_radix(&digits, 16)
                        {
                            index += count;
                            out.push_str(&escaped_unit(value, sensitive));
                        } else {
                            out.push_str(&escaped_unit(c, sensitive));
                        }
                    }
                    99 if units
                        .get(index)
                        .is_some_and(|c| matches!(*c,65..=90 | 97..=122)) =>
                    {
                        out.push_str(&escaped_unit(units[index] % 32, true));
                        index += 1;
                    }
                    48..=57 | 110 | 114 | 116 | 118 | 102 | 107 => {
                        out.push('\\');
                        out.push(char::from_u32(u32::from(c)).unwrap());
                    }
                    _ => out.push_str(&escaped_unit(c, sensitive)),
                }
            }
            91 if !class => {
                if units.get(index) == Some(&93) {
                    out.push_str("(?!)");
                    index += 1;
                } else if units.get(index..index + 2) == Some(&[94, 93]) {
                    out.push_str("[\\s\\S]");
                    index += 2;
                } else {
                    class = true;
                    out.push('[');
                }
            }
            93 if class => {
                class = false;
                out.push(']');
            }
            46 if !class => out.push_str(r"[^\n\r\x{2028}\x{2029}]"),
            94 if !class => out.push_str(r"\A"),
            36 if !class => out.push_str(r"(?=\z|[\n\r\x{2028}\x{2029}]\z|\r\n\z)"),
            // Preserve grammar punctuation; fold only literal units.
            40..=43 | 45 | 63 | 123..=125 | 94 => out.push(char::from_u32(u32::from(c)).unwrap()),
            92 => out.push('\\'),
            _ => out.push(unit(c, sensitive)),
        }
    }
    out
}
