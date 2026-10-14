// Port of yaml@2.9.0 dist/schema/Schema.js (ISC); parsing-only options.
mod common;
pub(crate) mod core;
pub(crate) mod failsafe;
pub(crate) mod json;
pub(crate) mod tags;
pub(crate) mod yaml_1_1;
use crate::nodes::ScalarValue;
use pilotdeck_protocol::JsString;
use regex::Regex;
use std::sync::LazyLock;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Resolve {
    String,
    Null,
    Bool,
    True,
    False,
    Int(usize, u32),
    Int11(usize, u32),
    Float,
    Float11,
    FloatNaN,
    IntTime,
    FloatTime,
    Timestamp,
    Binary,
    Merge,
    Map,
    Seq,
    Set,
    OMap,
    Pairs,
    JsonError,
}
#[derive(Clone, Debug)]
pub(crate) struct Tag {
    pub name: &'static str,
    pub default: bool,
    pub key_only: bool,
    pub pattern: Option<&'static LazyLock<Regex>>,
    pub resolve: Resolve,
    pub format: Option<&'static str>,
    pub collection: Option<&'static str>,
}
impl Tag {
    pub fn test(&self, s: &JsString) -> bool {
        self.pattern
            .is_some_and(|p| s.to_utf8().is_ok_and(|s| p.is_match(&s)))
    }
}
#[derive(Clone, Debug)]
pub(crate) struct Schema {
    pub tags: std::sync::Arc<Vec<Tag>>,
    pub known_tags: bool,
    pub yaml11: bool,
}
impl Schema {
    pub fn new(yaml11: bool) -> Self {
        Self {
            tags: {
                static CORE: LazyLock<std::sync::Arc<Vec<Tag>>> =
                    LazyLock::new(|| std::sync::Arc::new(tags::get_tags("core")));
                static YAML11: LazyLock<std::sync::Arc<Vec<Tag>>> =
                    LazyLock::new(|| std::sync::Arc::new(tags::get_tags("yaml-1.1")));
                if yaml11 { YAML11.clone() } else { CORE.clone() }
            },
            known_tags: !yaml11,
            yaml11,
        }
    }
    pub fn scalar(&self) -> Tag {
        common::string::tag()
    }
    pub fn resolve_value(
        &self,
        tag: &Tag,
        source: &std::sync::Arc<JsString>,
    ) -> Result<ScalarValue, JsString> {
        use Resolve::*;
        if tag.resolve == Resolve::String {
            return Ok(ScalarValue::String(source.clone()));
        }
        let text = source.to_utf8().unwrap_or_default();
        let number = match tag.resolve {
            String => return Ok(ScalarValue::String(source.clone())),
            Null => return Ok(ScalarValue::Null),
            Bool => return Ok(ScalarValue::Bool(text.starts_with(['t', 'T']))),
            True => return Ok(ScalarValue::Bool(true)),
            False => return Ok(ScalarValue::Bool(false)),
            Int(offset, radix) => parse_int(&text[offset..], radix),
            Int11(mut offset, radix) => {
                let sign = text.starts_with('-');
                if text.starts_with(['-', '+']) {
                    offset += 1;
                }
                let s = text.get(offset..).unwrap_or("").replace('_', "");
                let n = parse_int(&s, radix);
                if sign { -n } else { n }
            }
            Float => text.parse().unwrap_or(f64::NAN),
            Float11 => text.replace('_', "").parse().unwrap_or(f64::NAN),
            FloatNaN => {
                if text.to_ascii_lowercase().ends_with("nan") {
                    f64::NAN
                } else if text.starts_with('-') {
                    f64::NEG_INFINITY
                } else {
                    f64::INFINITY
                }
            }
            IntTime | FloatTime => yaml_1_1::timestamp::parse_sexagesimal(&text),
            Timestamp => return yaml_1_1::timestamp::resolve(&text).map(ScalarValue::Timestamp),
            Binary => return Ok(ScalarValue::Binary(yaml_1_1::binary::resolve(source))),
            Merge => return Ok(ScalarValue::Merge),
            JsonError => {
                return Err(crate::string::message(
                    "Unresolved plain scalar ",
                    &crate::string::json(source),
                ));
            }
            Map | Seq | Set | OMap | Pairs => return Ok(ScalarValue::String(source.clone())),
        };
        Ok(ScalarValue::Number(number))
    }
}
fn parse_int(s: &str, radix: u32) -> f64 {
    if radix == 10 {
        return s.parse().unwrap_or(f64::NAN);
    }
    let mut digits = s.chars().peekable();
    let neg = digits.peek() == Some(&'-');
    if matches!(digits.peek(), Some('-' | '+')) {
        digits.next();
    }
    let mut bits = Vec::new();
    let mut had_digit = false;
    let width = radix.trailing_zeros();
    for c in digits {
        let Some(n) = c.to_digit(radix) else { break };
        had_digit = true;
        for i in (0..width).rev() {
            if n >> i & 1 == 1 || !bits.is_empty() {
                bits.push(n >> i & 1);
            }
        }
    }
    if !had_digit {
        return f64::NAN;
    }
    if bits.is_empty() {
        return if neg { -0.0 } else { 0.0 };
    }
    let n = bits.len();
    let mut mantissa = bits
        .iter()
        .take(53)
        .fold(0u64, |a, b| a * 2 + u64::from(*b));
    if n > 53 && bits[53] == 1 && (bits[54..].contains(&1) || mantissa & 1 == 1) {
        mantissa += 1;
    }
    let value = mantissa as f64 * 2f64.powi(n.saturating_sub(53).min(i32::MAX as usize) as i32);
    if neg { -value } else { value }
}
