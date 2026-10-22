//! Search matching and JS diagnostic spelling for invalid patterns.
use super::Text;
use crate::Failure;
use fancy_regex::{Error, ParseError, Regex, RegexBuilder};

pub(super) enum Matcher {
    Literal {
        needle: Vec<u16>,
        fallback: Vec<usize>,
        sensitive: bool,
    },
    Regex {
        pattern: Regex,
        sensitive: bool,
    },
}
impl Matcher {
    pub(super) fn new(query: &str, regex: bool, sensitive: bool) -> Result<Self, Failure> {
        if !regex {
            let needle = if sensitive {
                query.encode_utf16().collect()
            } else {
                lower(&query.encode_utf16().collect::<Vec<_>>())
            };
            // KMP's failure links bound repeated-prefix literal searches by
            // O(query + text), including large pasted keywords.
            let mut fallback = vec![0; needle.len()];
            let mut matched = 0;
            for at in 1..needle.len() {
                while matched > 0 && needle[at] != needle[matched] {
                    matched = fallback[matched - 1];
                }
                if needle[at] == needle[matched] {
                    matched += 1;
                }
                fallback[at] = matched;
            }
            return Ok(Self::Literal {
                needle,
                fallback,
                sensitive,
            });
        }

        let invalid = |detail: &str| {
            Failure::Error(format!(
                "Invalid regular expression: /{query}/{}: {detail}",
                if sensitive { "" } else { "i" }
            ))
        };
        let translated = translate(query, sensitive).map_err(invalid)?;
        let pattern = RegexBuilder::new(&translated).build().map_err(|error| {
            let detail = match &error {
                Error::ParseError(
                    _,
                    ParseError::InvalidRepeat | ParseError::TargetNotRepeatable,
                ) => "Nothing to repeat",
                Error::ParseError(_, ParseError::UnclosedOpenParen) => "Unterminated group",
                Error::ParseError(_, ParseError::InvalidClass) => "Unterminated character class",
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
    pub(super) fn find(&self, text: &Text) -> Result<Option<usize>, Failure> {
        match self {
            Self::Literal {
                needle,
                fallback,
                sensitive,
            } => {
                let haystack = if *sensitive {
                    text.0.units().to_vec()
                } else {
                    lower(text.0.units())
                };
                if needle.is_empty() {
                    return Ok(Some(0));
                }
                let mut matched = 0;
                for (at, value) in haystack.iter().enumerate() {
                    while matched > 0 && *value != needle[matched] {
                        matched = fallback[matched - 1];
                    }
                    if *value == needle[matched] {
                        matched += 1;
                    }
                    if matched == needle.len() {
                        return Ok(Some(at + 1 - matched));
                    }
                }
                Ok(None)
            }
            Self::Regex { pattern, sensitive } => {
                let subject: String = text
                    .0
                    .units()
                    .iter()
                    .map(|u| unit(*u, *sensitive))
                    .collect();
                pattern
                    .find(&subject)
                    .map(|found| found.map(|m| subject[..m.start()].chars().count()))
                    .map_err(|e| {
                        Failure::Error(match e {
                            Error::RuntimeError(
                                fancy_regex::RuntimeError::BacktrackLimitExceeded,
                            ) => "Chat search regex exceeded backtracking limit".into(),
                            _ => e.to_string(),
                        })
                    })
            }
        }
    }
}
fn lower(units: &[u16]) -> Vec<u16> {
    // Lowercase valid Unicode runs while retaining each unpaired surrogate.
    let mut out = Vec::new();
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut Vec<u16>| {
        out.extend(
            icu_casemap::CaseMapper::new()
                .lowercase_to_string(run, &icu_locale_core::LanguageIdentifier::UNKNOWN)
                .encode_utf16(),
        );
        run.clear();
    };
    for decoded in char::decode_utf16(units.iter().copied()) {
        match decoded {
            Ok(c) => run.push(c),
            Err(e) => {
                flush(&mut run, &mut out);
                out.push(e.unpaired_surrogate());
            }
        }
    }
    flush(&mut run, &mut out);
    out
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
fn escaped_unit(value: u16, sensitive: bool) -> String {
    format!("\\x{{{:x}}}", u32::from(unit(value, sensitive)))
}
// Parse classes in JS code-unit order before canonicalizing: folding the
// endpoints of [Z-a] first would reverse a valid range. Explicit ranges also
// prevent Rust's nested classes and &&/-- operators from changing JS grammar.
enum Atom {
    Dash,
    Unit(u16),
    Set(Vec<Range>),
    Syntax(String),
}
fn escape(
    units: &[u16],
    index: &mut usize,
    class: bool,
    captures: usize,
    named: bool,
) -> Result<Atom, &'static str> {
    let Some(&c) = units.get(*index) else {
        return Err("\\ at end of pattern");
    };
    *index += 1;
    Ok(match c {
        100 | 68 | 119 | 87 | 115 | 83 => {
            let ranges = match c {
                100 | 68 => vec![(48, 57)],
                119 | 87 => vec![(48, 57), (65, 90), (95, 95), (97, 122)],
                _ => vec![
                    (9, 13),
                    (32, 32),
                    (160, 160),
                    (0x1680, 0x1680),
                    (0x2000, 0x200a),
                    (0x2028, 0x2029),
                    (0x202f, 0x202f),
                    (0x205f, 0x205f),
                    (0x3000, 0x3000),
                    (0xfeff, 0xfeff),
                ],
            };
            Atom::Set(if matches!(c, 68 | 87 | 83) {
                complement(&ranges)
            } else {
                ranges
            })
        }
        98 if class => Atom::Unit(8),
        98 | 66 if !class => {
            let word = "[A-Za-z0-9_]";
            let boundary = format!("(?:(?<!{word})(?={word})|(?<={word})(?!{word}))");
            Atom::Syntax(if c == 98 {
                boundary
            } else {
                format!("(?!{boundary})")
            })
        }
        120 | 117 => {
            let count = if c == 120 { 2 } else { 4 };
            let digits: String = units[*index..]
                .iter()
                .take(count)
                .map(|u| char::from_u32(u32::from(*u)).unwrap_or(' '))
                .collect();
            if digits.len() == count
                && let Ok(value) = u16::from_str_radix(&digits, 16)
            {
                *index += count;
                Atom::Unit(value)
            } else {
                Atom::Unit(c)
            }
        }
        99 if units
            .get(*index)
            .is_some_and(|c| matches!(*c,65..=90 | 97..=122)) =>
        {
            let value = units[*index] % 32;
            *index += 1;
            Atom::Unit(value)
        }
        110 => Atom::Unit(10),
        114 => Atom::Unit(13),
        116 => Atom::Unit(9),
        118 => Atom::Unit(11),
        102 => Atom::Unit(12),
        48..=57 => {
            let begin = *index - 1;
            let mut end = *index;
            while units.get(end).is_some_and(|u| (48..=57).contains(u)) {
                end += 1;
            }
            let decimal: String = units[begin..end]
                .iter()
                .map(|u| char::from_u32(u32::from(*u)).unwrap())
                .collect();
            if !class && c != 48 && decimal.parse::<usize>().is_ok_and(|n| n <= captures) {
                *index = end;
                Atom::Syntax(format!("\\{decimal}"))
            } else if c <= 55 {
                // Annex B legacy octal consumes at most three digits (two if
                // the first is 4..7), even in a character class.
                let max = if c <= 51 { 3 } else { 2 };
                let mut value = c - 48;
                for _ in 1..max {
                    if let Some(&next) = units.get(*index).filter(|u| (48..=55).contains(*u)) {
                        value = value * 8 + next - 48;
                        *index += 1;
                    } else {
                        break;
                    }
                }
                Atom::Unit(value)
            } else {
                Atom::Unit(c)
            }
        }
        107 if !class && named => {
            if units.get(*index) != Some(&60) {
                return Err("Invalid named reference");
            }
            let begin = *index;
            while units.get(*index).is_some_and(|u| *u != 62) {
                *index += 1;
            }
            if *index == units.len() {
                return Err("Invalid named reference");
            }
            *index += 1;
            Atom::Syntax(format!(
                "\\k{}",
                String::from_utf16_lossy(&units[begin..*index])
            ))
        }
        _ => Atom::Unit(c),
    })
}
type Range = (u32, u32);

// Sort once per class, rather than inserting each atom into an ever-growing
// set. Work and storage depend on input ranges, never their expanded lengths.
fn union(mut ranges: Vec<Range>) -> Vec<Range> {
    ranges.sort_unstable();
    let mut merged: Vec<Range> = Vec::new();
    for (start, end) in ranges {
        if let Some(last) = merged.last_mut()
            && start <= last.1 + 1
        {
            last.1 = last.1.max(end);
        } else {
            merged.push((start, end));
        }
    }
    merged
}

// Input is a sorted, disjoint union of UTF-16 code-unit intervals.
fn complement(ranges: &[Range]) -> Vec<Range> {
    let mut out = Vec::new();
    let mut next = 0;
    for &(start, end) in ranges {
        if next < start {
            out.push((next, start - 1));
        }
        next = end + 1;
    }
    if next <= u32::from(u16::MAX) {
        out.push((next, u32::from(u16::MAX)));
    }
    out
}

fn emit_set(ranges: Vec<Range>, sensitive: bool, negative: bool) -> String {
    // Describe canonicalization as affine runs over the code-unit domain. The
    // fixed 65,536-unit scan happens once, not once per escape or class atom.
    static FOLDED: std::sync::OnceLock<Vec<(u32, u32, u32)>> = std::sync::OnceLock::new();
    let identity = [
        (0, 0xd7ff, 0),
        (0xd800, 0xdfff, 0xfd800),
        (0xe000, 0xffff, 0xe000),
    ];
    let mapping = if sensitive {
        identity.as_slice()
    } else {
        FOLDED.get_or_init(|| {
            let mut runs = Vec::new();
            let (mut start, mut mapped) = (0, u32::from(unit(0, false)));
            for value in 1..=u16::MAX {
                let value = u32::from(value);
                let folded = u32::from(unit(value as u16, false));
                if folded != mapped + value - start {
                    runs.push((start, value - 1, mapped));
                    start = value;
                    mapped = folded;
                }
            }
            runs.push((start, u32::from(u16::MAX), mapped));
            runs
        })
    };
    let mut values = Vec::new();
    let mut at = 0;
    for (start, end) in union(ranges) {
        while at < mapping.len() && mapping[at].1 < start {
            at += 1;
        }
        let mut run = at;
        while let Some(&(from, to, mapped)) = mapping.get(run) {
            if from > end {
                break;
            }
            values.push((mapped + start.max(from) - from, mapped + end.min(to) - from));
            run += 1;
        }
    }
    let values = union(values);
    if values.is_empty() {
        return if negative { "[\\s\\S]" } else { "(?!)" }.into();
    }
    let mut out = if negative { "[^" } else { "[" }.to_owned();
    for (start, end) in values {
        out.push_str(&format!("\\x{{{start:x}}}"));
        if end > start {
            out.push_str(&format!("-\\x{{{end:x}}}"));
        }
    }
    out.push(']');
    out
}
fn class_unit(atom: &Atom) -> Option<u16> {
    match atom {
        Atom::Unit(u) => Some(*u),
        Atom::Dash => Some(45),
        _ => None,
    }
}
fn character_class(
    units: &[u16],
    index: &mut usize,
    sensitive: bool,
) -> Result<String, &'static str> {
    let negative = units.get(*index) == Some(&94);
    if negative {
        *index += 1;
    }
    let mut atoms = Vec::new();
    while let Some(&c) = units.get(*index) {
        *index += 1;
        if c == 93 {
            let mut values = Vec::new();
            let mut at = 0;
            while at < atoms.len() {
                if at + 2 < atoms.len()
                    && matches!(atoms[at + 1], Atom::Dash)
                    && let (Some(start), Some(end)) =
                        (class_unit(&atoms[at]), class_unit(&atoms[at + 2]))
                {
                    if start > end {
                        return Err("Range out of order in character class");
                    }
                    values.push((u32::from(start), u32::from(end)));
                    at += 3;
                    continue;
                }
                match &atoms[at] {
                    Atom::Unit(u) => values.push((u32::from(*u), u32::from(*u))),
                    Atom::Dash => values.push((45, 45)),
                    Atom::Set(set) => values.extend(set),
                    Atom::Syntax(_) => unreachable!(),
                }
                at += 1;
            }
            return Ok(emit_set(values, sensitive, negative));
        }
        atoms.push(if c == 92 {
            escape(units, index, true, 0, false)?
        } else if c == 45 {
            Atom::Dash
        } else {
            Atom::Unit(c)
        });
    }
    Err("Unterminated character class")
}
fn quantifier(units: &[u16], start: usize) -> Option<usize> {
    let mut at = start + 1;
    let begin = at;
    while units.get(at).is_some_and(|u| (48..=57).contains(u)) {
        at += 1;
    }
    if at == begin {
        return None;
    }
    if units.get(at) == Some(&44) {
        at += 1;
        while units.get(at).is_some_and(|u| (48..=57).contains(u)) {
            at += 1;
        }
    }
    (units.get(at) == Some(&125)).then_some(at + 1)
}
// V8 saturates quantifier bounds at 2^31-1 before checking their order.
fn quantifier_out_of_order(units: &[u16]) -> bool {
    let Some(comma) = units.iter().position(|u| *u == 44) else {
        return false;
    };
    let upper = &units[comma + 1..units.len() - 1];
    if upper.is_empty() {
        return false;
    }
    let bound = |digits: &[u16]| {
        digits.iter().fold(0_u32, |n, digit| {
            n.saturating_mul(10)
                .saturating_add(u32::from(*digit - 48))
                .min(i32::MAX as u32)
        })
    };
    bound(&units[1..comma]) > bound(upper)
}
fn translate(pattern: &str, sensitive: bool) -> Result<String, &'static str> {
    let units: Vec<_> = pattern.encode_utf16().collect();
    // Count captures before resolving decimal escapes, including forward refs.
    let (mut captures, mut at, mut in_class) = (0, 0, false);
    let mut names = std::collections::HashSet::new();
    while at < units.len() {
        let c = units[at];
        at += 1;
        match c {
            92 => at += 1,
            91 => in_class = true,
            93 => in_class = false,
            40 if !in_class => {
                if units.get(at) != Some(&63) {
                    captures += 1;
                } else if units.get(at + 1) == Some(&60)
                    && !matches!(units.get(at + 2), Some(61 | 33))
                {
                    let begin = at + 2;
                    let Some(end) = units[begin..]
                        .iter()
                        .position(|u| *u == 62)
                        .map(|i| i + begin)
                    else {
                        return Err("Invalid capture group name");
                    };
                    if !names.insert(units[begin..end].to_vec()) {
                        return Err("Duplicate capture group name");
                    }
                    captures += 1;
                }
            }
            _ => {}
        }
    }
    let mut out = String::new();
    let (mut index, mut repeated) = (0, false);
    let mut assertion = None;
    let mut groups = Vec::new();
    while index < units.len() {
        let c = units[index];
        index += 1;
        match c {
            92 => {
                assertion =
                    matches!(units.get(index), Some(98 | 66)).then_some("Nothing to repeat");
                match escape(&units, &mut index, false, captures, !names.is_empty())? {
                    Atom::Unit(u) => out.push_str(&escaped_unit(u, sensitive)),
                    Atom::Dash => unreachable!(),
                    Atom::Set(set) => out.push_str(&emit_set(set, sensitive, false)),
                    Atom::Syntax(s) => out.push_str(&s),
                }
                repeated = false;
            }
            91 => {
                assertion = None;
                out.push_str(&character_class(&units, &mut index, sensitive)?);
                repeated = false;
            }
            40 => {
                // Annex B permits quantifying lookahead, but not lookbehind.
                groups.push(
                    units.get(index) == Some(&63)
                        && units.get(index + 1) == Some(&60)
                        && matches!(units.get(index + 2), Some(61 | 33)),
                );
                assertion = None;
                // fancy-regex rejects repeating a bare lookaround (even through
                // a noncapturing group). A successful empty lookahead makes a
                // concat without consuming text or adding a capture, preserving
                // JS's repeatable lookahead and groups containing assertions.
                out.push_str("(?:(?=)");
                out.push('(');
                if units.get(index) == Some(&63) {
                    out.push('?');
                    index += 1;
                    match units.get(index) {
                        Some(58 | 61 | 33) => {
                            out.push(char::from_u32(u32::from(units[index])).unwrap());
                            index += 1;
                        }
                        Some(60) => {
                            let begin = index;
                            if matches!(units.get(index + 1), Some(61 | 33)) {
                                index += 2;
                            } else {
                                while units.get(index).is_some_and(|u| *u != 62) {
                                    index += 1;
                                }
                                if index == units.len() {
                                    return Err("Invalid capture group name");
                                }
                                index += 1;
                            }
                            out.push_str(&String::from_utf16_lossy(&units[begin..index]));
                        }
                        _ => return Err("Invalid group"),
                    }
                }
                repeated = false;
            }
            123 if quantifier(&units, index - 1).is_some() => {
                if repeated {
                    return Err("Nothing to repeat");
                }
                if let Some(detail) = assertion {
                    return Err(detail);
                }
                let end = quantifier(&units, index - 1).unwrap();
                if quantifier_out_of_order(&units[index - 1..end]) {
                    return Err("numbers out of order in {} quantifier");
                }
                out.push_str(&String::from_utf16_lossy(&units[index - 1..end]));
                index = end;
                repeated = true;
            }
            42 | 43 => {
                if repeated {
                    return Err("Nothing to repeat");
                }
                if let Some(detail) = assertion {
                    return Err(detail);
                }
                out.push(char::from_u32(u32::from(c)).unwrap());
                repeated = true;
            }
            63 => {
                if let Some(detail) = assertion {
                    return Err(detail);
                }
                out.push('?');
                repeated = true;
            }
            41 | 124 => {
                assertion = if c == 41 {
                    groups
                        .pop()
                        .unwrap_or(false)
                        .then_some("Invalid quantifier")
                } else {
                    None
                };
                out.push(char::from_u32(u32::from(c)).unwrap());
                if c == 41 {
                    out.push(')');
                }
                repeated = false;
            }
            46 => {
                assertion = None;
                out.push_str(r"[^\n\r\x{2028}\x{2029}]");
                repeated = false;
            }
            94 => {
                assertion = Some("Nothing to repeat");
                out.push_str(r"\A");
                repeated = false;
            }
            36 => {
                assertion = Some("Nothing to repeat");
                out.push_str(r"(?=\z)");
                repeated = false;
            }
            _ => {
                assertion = None;
                out.push_str(&escaped_unit(c, sensitive));
                repeated = false;
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_failure_links_preserve_first_code_unit_offset() {
        // Exhaust the short repeated-prefix alphabet, using the former naive
        // search as an independent reference rather than copying KMP logic.
        for needle_len in 0..=5 {
            for bits in 0..1 << needle_len {
                let needle: String = (0..needle_len)
                    .map(|at| if bits & (1 << at) == 0 { 'a' } else { 'b' })
                    .collect();
                let matcher = Matcher::new(&needle, false, true).unwrap();
                for text_len in 0..=8 {
                    for bits in 0..1 << text_len {
                        let text: String = (0..text_len)
                            .map(|at| if bits & (1 << at) == 0 { 'a' } else { 'b' })
                            .collect();
                        assert_eq!(
                            matcher.find(&Text(text.clone().into())).unwrap(),
                            text.find(&needle)
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn escape_intervals_preserve_ecmascript_membership() {
        for code in [100, 68, 119, 87, 115, 83] {
            let Atom::Set(ranges) = escape(&[code], &mut 0, true, 0, false).unwrap() else {
                panic!("expected class intervals");
            };
            for value in 0..=u16::MAX {
                let positive = match code {
                    100 | 68 => (48..=57).contains(&value),
                    119 | 87 => matches!(value, 48..=57 | 65..=90 | 97..=122 | 95),
                    _ => super::super::space(value),
                };
                assert_eq!(
                    ranges
                        .iter()
                        .any(|&(from, to)| (from..=to).contains(&u32::from(value))),
                    positive != matches!(code, 68 | 87 | 83),
                    "escape={code} unit={value}"
                );
            }
        }
    }
}
