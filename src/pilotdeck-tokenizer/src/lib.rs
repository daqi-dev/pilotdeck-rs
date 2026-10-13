//! Immutable ordinary-text o200k_base counter; no files, network or Host authority.
use pilotdeck_protocol::JsString;
mod implementation;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenizeError {
    Initialization(String),
    Encoding(String),
}
/// Original js-tiktoken1.0.21 contractions; do not replace with (?i) Unicode fold.
pub const SOURCE_PATTERN: &str = "[^\\r\\n\\p{L}\\p{N}]?[\\p{Lu}\\p{Lt}\\p{Lm}\\p{Lo}\\p{M}]*[\\p{Ll}\\p{Lm}\\p{Lo}\\p{M}]+('s|'S|'t|'T|'re|'rE|'Re|'RE|'ve|'vE|'Ve|'VE|'m|'M|'ll|'lL|'Ll|'LL|'d|'D)?|[^\\r\\n\\p{L}\\p{N}]?[\\p{Lu}\\p{Lt}\\p{Lm}\\p{Lo}\\p{M}]+[\\p{Ll}\\p{Lm}\\p{Lo}\\p{M}]*('s|'S|'t|'T|'re|'rE|'Re|'RE|'ve|'vE|'Ve|'VE|'m|'M|'ll|'lL|'Ll|'LL|'d|'D)?|\\p{N}{1,3}| ?[^\\s\\p{L}\\p{N}]+[\\r\\n/]*|\\s*[\\r\\n]+|\\s+(?!\\S)|\\s+";

pub fn count_tokens(text: &JsString) -> Result<usize, TokenizeError> {
    if text.is_empty() {
        return Ok(0);
    }
    implementation::count_tokens(text)
}
