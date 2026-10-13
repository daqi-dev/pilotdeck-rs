//! Immutable Source ordinary o200k candidate. Embedded ranks only; no IO.
use crate::{SOURCE_PATTERN, TokenizeError};
use base64::{Engine, engine::general_purpose::STANDARD};
use pilotdeck_protocol::JsString;
use rustc_hash::FxHashMap;
use std::{collections::HashSet, sync::OnceLock};
use tiktoken_rs::CoreBPE;

const RANKS: &str = include_str!("../../vendor/tiktoken-rs/assets/o200k_base.tiktoken");
// ECMAScript WhiteSpace + LineTerminator, unlike Rust Unicode White_Space:
// U+FEFF belongs here; U+0085 does not. Nested regex character classes retain
// the surrounding Source union/negation without changing branch precedence.
const JS_SPACE: &str = r"[\u{0009}-\u{000d}\u{0020}\u{00a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}]";
const JS_NONSPACE: &str = r"[^\u{0009}-\u{000d}\u{0020}\u{00a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}]";
static BPE: OnceLock<Result<CoreBPE, TokenizeError>> = OnceLock::new();

fn source_pattern() -> String {
    SOURCE_PATTERN
        .replace(r"\s", JS_SPACE)
        .replace(r"\S", JS_NONSPACE)
}
fn initialize() -> Result<CoreBPE, TokenizeError> {
    let mut encoder = FxHashMap::default();
    let mut seen = HashSet::new();
    for (index, line) in RANKS.lines().enumerate() {
        let mut fields = line.split_whitespace();
        let bytes = fields.next().ok_or_else(|| {
            TokenizeError::Initialization(format!("Missing token bytes on line {}", index + 1))
        })?;
        let rank = fields.next().ok_or_else(|| {
            TokenizeError::Initialization(format!("Missing rank on line {}", index + 1))
        })?;
        if fields.next().is_some() {
            return Err(TokenizeError::Initialization(format!(
                "Extra fields on line {}",
                index + 1
            )));
        }
        let bytes = STANDARD
            .decode(bytes)
            .map_err(|e| TokenizeError::Initialization(e.to_string()))?;
        let rank = rank
            .parse::<u32>()
            .map_err(|e| TokenizeError::Initialization(e.to_string()))?;
        if bytes.is_empty() || !seen.insert(rank) || encoder.insert(bytes, rank).is_some() {
            return Err(TokenizeError::Initialization(format!(
                "Duplicate or empty rank entry on line {}",
                index + 1
            )));
        }
    }
    if encoder.len() != 199_998
        || (0..199_998).any(|rank| !seen.contains(&rank))
        || (0..=255).any(|byte| !encoder.contains_key(&vec![byte]))
    {
        return Err(TokenizeError::Initialization(
            "Embedded o200k rank dictionary is incomplete.".into(),
        ));
    }
    let mut specials = FxHashMap::default();
    specials.insert("<|endoftext|>".into(), 199_999);
    specials.insert("<|endofprompt|>".into(), 200_018);
    CoreBPE::new(encoder, specials, &source_pattern())
        .map_err(|e| TokenizeError::Initialization(e.to_string()))
}
fn text_encoder_input(text: &JsString) -> String {
    // TextEncoder uses USVString: replace each lone surrogate, retain valid
    // pairs as one scalar. No trim, NFC normalization or whitespace rewrite.
    char::decode_utf16(text.units().iter().copied())
        .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}
pub(crate) fn count_tokens(text: &JsString) -> Result<usize, TokenizeError> {
    let bpe = BPE.get_or_init(initialize).as_ref().map_err(Clone::clone)?;
    let text = text_encoder_input(text);
    let encoded =
        std::panic::catch_unwind(|| bpe.encode(&text, &HashSet::new())).map_err(|_| {
            TokenizeError::Encoding("Tokenizer panicked while encoding ordinary text.".into())
        })?;
    encoded
        .map(|(tokens, _)| tokens.len())
        .map_err(|e| TokenizeError::Encoding(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_whitespace_is_explicit_and_no_unicode_casefold() {
        let pattern = source_pattern();
        assert!(!pattern.contains(r"\s"));
        assert!(!pattern.contains(r"\S"));
        assert!(pattern.contains(r"\u{feff}"));
        assert!(!pattern.contains(r"\u{0085}"));
        assert!(pattern.contains("'s|'S"));
        assert!(!pattern.contains("(?i)"));
    }
    #[test]
    fn text_encoder_replaces_lone_surrogates_and_retains_pairs() {
        assert_eq!(
            text_encoder_input(&JsString::from_utf16(vec![0xd800, 0xd800, 0xdc00, 0xdc00])),
            "\u{fffd}\u{10000}\u{fffd}"
        );
    }
    fn artificial_count(piece: &str, text: &str) -> usize {
        let mut encoder = FxHashMap::default();
        for byte in 0..=255 {
            encoder.insert(vec![byte], u32::from(byte));
        }
        encoder.insert(piece.as_bytes().to_vec(), 300);
        CoreBPE::new(encoder, FxHashMap::default(), &source_pattern())
            .unwrap()
            .encode_ordinary(text)
            .len()
    }
    #[test]
    fn source_feff_is_whitespace_before_symbol_branch() {
        assert_eq!(artificial_count("!\u{feff}!", "!\u{feff}!"), 5);
    }
    #[test]
    fn source_nel_is_not_whitespace_before_symbol_branch() {
        assert_eq!(artificial_count("!\u{85}!", "!\u{85}!"), 1);
    }
    #[test]
    fn source_contractions_use_ascii_cases_not_long_s_fold() {
        assert_eq!(artificial_count("a'ſ", "a'ſ"), 4);
        assert_eq!(artificial_count("a'S", "a'S"), 1);
    }
    #[test]
    fn source_three_digit_branch_prevents_long_numeric_merge() {
        assert_eq!(artificial_count("1234", "1234"), 4);
    }
}
