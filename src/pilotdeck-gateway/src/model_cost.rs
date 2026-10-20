//! Pure model pricing, usage-cost composition and local o200k request accounting.
//!
//! Ported against PilotDeck `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`:
//! - `src/router/utils/modelPricing.ts` (rate table, configured overrides, cost helpers)
//! - `src/router/stats/TokenStatsCollector.ts:183-186,378-421` (`estimateCost`)
//! - `src/agent/loop/AgentLoop.ts:2390-2403` (router estimate then native cost)
//! - `src/agent/loop/nativeAgentTurnCapabilitiesAdapter.ts:41-58` (budget port)
//! - `src/context/budget/TokenAccountingRuntime.ts:167-190` (local branch)
//! - `src/context/budget/TokenBudgetManager.ts` (block/message accounting)
//! - `src/model/protocol/toolResultContent.ts` (tool-result flattening)
//!
//! This module performs no IO and is not wired into Gateway dispatch yet.

use pilotdeck_protocol::model::{
    CanonicalModelRequest, ContentBlock, ContentBody, MediaSource, Message, ToolResult,
    ToolResultContent, ToolResultItem, ToolSchema, Usage,
};
use pilotdeck_protocol::{JsString, JsValue, stringify_json};
use pilotdeck_tokenizer::{TokenizeError, count_tokens};
use regex::Regex;
use std::sync::OnceLock;

/// Fixed multimedia placeholder from `IMAGE_MAX_TOKEN_SIZE` / T6-T8/T13.
pub const MULTIMEDIA_TOKENS: usize = 2_000;
/// `DEFAULT_PER_MESSAGE_OVERHEAD` in `TokenBudgetManager`.
pub const PER_MESSAGE_OVERHEAD: usize = 4;

/// A resolved rate card. `unit` is display metadata and never affects arithmetic.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ModelPricing {
    pub input: Option<f64>,
    pub output: Option<f64>,
    pub cache_read: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PricingSource {
    ConfiguredPrice,
    BuiltInEstimate,
    FallbackEstimate,
}

impl PricingSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ConfiguredPrice => "configured_price",
            Self::BuiltInEstimate => "built_in_estimate",
            Self::FallbackEstimate => "fallback_estimate",
        }
    }
}

/// Operator-supplied rates keyed by `provider/model` (or a model substring).
///
/// JavaScript property enumeration order is observable in `lookupModelPricing`'s
/// substring scan. Like `Object.entries`, array-index keys (canonical decimal
/// strings in `0..=4294967294`) enumerate first in ascending numeric order, and
/// every other key follows in insertion order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PricingMap {
    entries: Vec<(String, ModelPricing)>,
}

impl PricingMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Same replacement semantics as assigning an existing object property:
    /// the first insertion keeps its scan position.
    pub fn insert(
        &mut self,
        key: impl Into<String>,
        pricing: ModelPricing,
    ) -> Option<ModelPricing> {
        let key = key.into();
        if let Some((_, value)) = self.entries.iter_mut().find(|(name, _)| *name == key) {
            return Some(std::mem::replace(value, pricing));
        }
        self.entries.push((key, pricing));
        None
    }

    pub fn get(&self, key: &str) -> Option<&ModelPricing> {
        self.entries
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &ModelPricing)> {
        let mut indexed: Vec<(u32, &str, &ModelPricing)> = self
            .entries
            .iter()
            .filter_map(|(key, pricing)| {
                array_index(key).map(|index| (index, key.as_str(), pricing))
            })
            .collect();
        indexed.sort_unstable_by_key(|(index, _, _)| *index);
        indexed
            .into_iter()
            .map(|(_, key, pricing)| (key, pricing))
            .chain(
                self.entries
                    .iter()
                    .filter(|(key, _)| array_index(key).is_none())
                    .map(|(key, pricing)| (key.as_str(), pricing)),
            )
    }
}

/// ECMAScript "array index" property key: a canonical decimal integer in
/// `0..=4294967294`. `Object.entries` enumerates these before every other key,
/// in ascending numeric order, which visibly changes the substring scan order.
fn array_index(key: &str) -> Option<u32> {
    pilotdeck_protocol::JsString::from(key).array_index()
}

struct DefaultRate {
    pattern: Regex,
    input: f64,
    output: f64,
    cache_read: Option<f64>,
}

/// `DEFAULT_PRICING` in `modelPricing.ts`, in match order.
const DEFAULT_PRICING: &[(&str, f64, f64, Option<f64>)] = &[
    // DeepSeek
    (r"deepseek.*flash", 0.20, 0.60, None),
    (r"deepseek.*chat", 0.50, 1.50, None),
    (r"deepseek.*reasoner", 0.80, 2.00, None),
    (r"deepseek.*v3", 0.27, 1.10, None),
    // Anthropic Claude
    (r"claude.*opus", 15.00, 75.00, Some(1.50)),
    (r"claude.*sonnet", 3.00, 15.00, Some(0.30)),
    (r"claude.*haiku", 0.80, 4.00, Some(0.08)),
    // OpenAI
    (r"gpt-4o-mini", 0.15, 0.60, Some(0.075)),
    (r"gpt-4o", 2.50, 10.00, Some(1.25)),
    (r"gpt-4\.1", 2.00, 8.00, Some(0.50)),
    (r"gpt-5", 2.00, 8.00, Some(0.50)),
    (r"o[134]-mini", 1.10, 4.40, None),
    (r"o[134]-pro", 10.00, 40.00, None),
    (r"o[134]", 2.50, 10.00, None),
    // Google Gemini
    (r"gemini.*flash", 0.10, 0.40, None),
    (r"gemini.*pro", 1.25, 5.00, None),
    // GLM / ChatGLM / Zhipu
    (r"glm", 0.50, 1.00, None),
    // Qwen / Tongyi
    (r"qwen.*turbo", 0.30, 0.60, None),
    (r"qwen.*plus", 0.80, 2.00, None),
    (r"qwen.*max", 2.00, 6.00, None),
    (r"qwen", 0.50, 1.50, None),
    // Llama / Meta
    (r"llama.*70b", 0.80, 0.80, None),
    (r"llama.*405b", 3.00, 3.00, None),
    (r"llama", 0.20, 0.20, None),
    // Mistral
    (r"mistral.*large", 2.00, 6.00, None),
    (r"mistral.*small", 0.10, 0.30, None),
    (r"mistral", 0.25, 0.25, None),
    // Yi / 01.AI
    (r"yi-", 0.30, 0.30, None),
    // Moonshot / Kimi
    (r"moonshot|kimi", 1.00, 2.00, None),
    // Doubao / ByteDance
    (r"doubao", 0.40, 0.80, None),
];

const FALLBACK_PRICING: ModelPricing = ModelPricing {
    input: Some(0.50),
    output: Some(1.50),
    cache_read: None,
};

fn default_pricing() -> &'static [DefaultRate] {
    static RATES: OnceLock<Vec<DefaultRate>> = OnceLock::new();
    RATES.get_or_init(|| {
        DEFAULT_PRICING
            .iter()
            .map(|(pattern, input, output, cache_read)| DefaultRate {
                // `(?i)` would apply Unicode simple folding: it makes `K`
                // (U+212A) match `k` and `ſ` (U+017F) match `s`, while JS `/i`
                // without `u` refuses to fold a non-ASCII code unit onto an
                // ASCII one. In addition, JS `.` excludes all four line
                // terminators, whereas the regex crate's `.` excludes only
                // `\n`. Rewrite the source to the exact JS semantics instead.
                pattern: Regex::new(&js_regex_source(pattern))
                    .expect("static model pricing pattern must compile"),
                input: *input,
                output: *output,
                cache_read: *cache_read,
            })
            .collect()
    })
}

/// Translate the ASCII-only `/i` (no `u`) sources in `DEFAULT_PRICING` to the
/// regex crate: ASCII letters become explicit two-case classes (JS
/// `Canonicalize` in non-Unicode mode never folds a non-ASCII code point onto
/// an ASCII one), and `.` becomes the four-line-terminator-excluding class.
/// Every current pattern uses `.` only inside `.*`; a single `.` would differ
/// for non-BMP input because JS counts UTF-16 code units, not scalar values.
/// The only character classes in the table are digit lists; an alphabetic
/// class or range would need the same explicit two-case expansion here.
fn js_regex_source(source: &str) -> String {
    let mut output = String::new();
    let mut chars = source.chars();
    let mut in_class = false;
    while let Some(character) = chars.next() {
        match character {
            '\\' => {
                output.push(character);
                if let Some(escaped) = chars.next() {
                    output.push(escaped);
                }
            }
            '[' => {
                in_class = true;
                output.push(character);
            }
            ']' => {
                in_class = false;
                output.push(character);
            }
            '.' if !in_class => output.push_str("[^\\n\\r\\u{2028}\\u{2029}]"),
            letter if letter.is_ascii_alphabetic() && !in_class => {
                output.push('[');
                output.push(letter.to_ascii_lowercase());
                output.push(letter.to_ascii_uppercase());
                output.push(']');
            }
            _ => output.push(character),
        }
    }
    output
}

pub fn lookup_model_pricing(
    provider: &str,
    model: &str,
    configured: Option<&PricingMap>,
) -> ModelPricing {
    lookup_model_pricing_with_source(provider, model, configured).0
}

/// Exact key first, then the first insertion-ordered substring match, then the
/// built-in estimate table, then `FALLBACK_PRICING`.
pub fn lookup_model_pricing_with_source(
    provider: &str,
    model: &str,
    configured: Option<&PricingMap>,
) -> (ModelPricing, PricingSource) {
    let combined = format!("{provider}/{model}");
    if let Some(rates) = configured {
        if let Some(pricing) = rates.get(&combined) {
            return (*pricing, PricingSource::ConfiguredPrice);
        }
        for (key, pricing) in rates.iter() {
            if model.contains(key) || key.contains(model) {
                return (*pricing, PricingSource::ConfiguredPrice);
            }
        }
    }
    for rate in default_pricing() {
        if rate.pattern.is_match(&combined) || rate.pattern.is_match(model) {
            return (
                ModelPricing {
                    input: Some(rate.input),
                    output: Some(rate.output),
                    cache_read: rate.cache_read,
                },
                PricingSource::BuiltInEstimate,
            );
        }
    }
    (FALLBACK_PRICING, PricingSource::FallbackEstimate)
}

pub fn calculate_input_cost(
    tokens: f64,
    provider: &str,
    model: &str,
    configured: Option<&PricingMap>,
) -> f64 {
    let pricing = lookup_model_pricing(provider, model, configured);
    (tokens / 1_000_000.0) * pricing.input.unwrap_or(0.0)
}

/// This exported helper falls back to the input rate when `cacheRead` is absent.
/// `estimate_cost_breakdown` intentionally does not: `TokenStatsCollector`
/// charges a missing cache-read rate as zero. Keep both TS behaviours distinct.
pub fn calculate_cache_read_cost(
    tokens: f64,
    provider: &str,
    model: &str,
    configured: Option<&PricingMap>,
) -> f64 {
    let pricing = lookup_model_pricing(provider, model, configured);
    (tokens / 1_000_000.0) * pricing.cache_read.or(pricing.input).unwrap_or(0.0)
}

/// Per-request USD estimate; matches the persisted `record.cost` projection
/// (cache writes are charged at the input rate and folded into `total`).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CostBreakdown {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub total: f64,
}

/// Mirrors `TokenStatsCollector.estimateCostBreakdown`: a finite non-negative
/// provider-reported `nativeCost` wins outright; otherwise the rate card applies.
pub fn estimate_cost_breakdown(
    usage: &Usage,
    provider: &str,
    model: &str,
    configured: Option<&PricingMap>,
) -> CostBreakdown {
    if let Some(native) = usage
        .native_cost
        .filter(|value| value.is_finite() && *value >= 0.0)
    {
        return CostBreakdown {
            total: native,
            ..CostBreakdown::default()
        };
    }
    let pricing = lookup_model_pricing(provider, model, configured);
    let input = (usage.input_tokens.unwrap_or(0.0) / 1_000_000.0) * pricing.input.unwrap_or(0.0);
    let output = (usage.output_tokens.unwrap_or(0.0) / 1_000_000.0) * pricing.output.unwrap_or(0.0);
    let cache_read =
        (usage.cache_read_tokens.unwrap_or(0.0) / 1_000_000.0) * pricing.cache_read.unwrap_or(0.0);
    let cache_write =
        (usage.cache_write_tokens.unwrap_or(0.0) / 1_000_000.0) * pricing.input.unwrap_or(0.0);
    CostBreakdown {
        input,
        output,
        cache_read,
        total: input + output + cache_read + cache_write,
    }
}

/// `TokenStatsCollector.estimateCost`: an absent usage payload costs `0`.
pub fn estimate_cost(
    usage: Option<&Usage>,
    provider: &str,
    model: &str,
    configured: Option<&PricingMap>,
) -> f64 {
    match usage {
        None => 0.0,
        Some(usage) => estimate_cost_breakdown(usage, provider, model, configured).total,
    }
}

/// The AgentLoop composition: the router estimate is used when it is finite and
/// non-negative, otherwise a finite non-negative `usage.nativeCost`, else none
/// (the host module returns `{costUsd: null}` for that case).
pub fn estimate_usage_cost(
    usage: Option<&Usage>,
    provider: &str,
    model: &str,
    configured: Option<&PricingMap>,
) -> Option<f64> {
    let router_estimate = estimate_cost(usage, provider, model, configured);
    if router_estimate.is_finite() && router_estimate >= 0.0 {
        return Some(router_estimate);
    }
    usage
        .and_then(|usage| usage.native_cost)
        .filter(|value| value.is_finite() && *value >= 0.0)
}

/// Additive local accounting buckets. `system` excludes `mcp` and `memory`, so
/// `system + tools + messages + mcp + memory == total`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenBudgetBreakdown {
    pub total: usize,
    pub system: usize,
    pub tools: usize,
    pub messages: usize,
    pub mcp: usize,
    pub memory: usize,
}

/// `TokenBudgetManager.estimateTextTokens`; empty text short-circuits to `0`.
pub fn estimate_text_tokens(text: &str) -> Result<usize, TokenizeError> {
    count_tokens(&JsString::from(text))
}

pub fn estimate_messages_tokens(messages: &[Message]) -> Result<usize, TokenizeError> {
    let mut total = 0;
    for message in messages {
        total += estimate_message_tokens(message)?;
    }
    Ok(total)
}

/// `TokenAccountingRuntime.estimateRequestInput` through the local branch only.
pub fn estimate_request_input(request: &CanonicalModelRequest) -> Result<usize, TokenizeError> {
    Ok(estimate_request_breakdown(request)?.total)
}

/// `TokenAccountingRuntime.estimateRequestBreakdown` (T1-T13 + system/tool
/// accounting), with MCP/memory blocks clamped to the full system count.
pub fn estimate_request_breakdown(
    request: &CanonicalModelRequest,
) -> Result<TokenBudgetBreakdown, TokenizeError> {
    let system_prompt = request.system_prompt.as_ref();
    let system_tokens = match system_prompt {
        Some(prompt) if !prompt.is_empty() => count_tokens(prompt)?,
        _ => 0,
    };
    let estimated_mcp = match system_prompt {
        Some(prompt) => estimate_delimited_blocks(prompt, "mcp-instructions")?,
        None => 0,
    };
    let estimated_memory = match system_prompt {
        Some(prompt) => estimate_delimited_blocks(prompt, "memory-context")?,
        None => 0,
    };
    let mcp = system_tokens.min(estimated_mcp);
    let memory = (system_tokens - mcp).min(estimated_memory);
    let messages = estimate_messages_tokens(&request.messages)?;
    let tools = estimate_tool_schemas(request.tools.as_deref().unwrap_or(&[]))?;
    Ok(TokenBudgetBreakdown {
        total: system_tokens + messages + tools,
        system: system_tokens - mcp - memory,
        tools,
        messages,
        mcp,
        memory,
    })
}

fn estimate_message_tokens(message: &Message) -> Result<usize, TokenizeError> {
    let mut total = PER_MESSAGE_OVERHEAD;
    for block in &message.content {
        total += estimate_block_tokens(block)?;
    }
    Ok(total)
}

pub(crate) fn estimate_block_tokens(block: &ContentBlock) -> Result<usize, TokenizeError> {
    match &block.body {
        ContentBody::Text(text) => count_tokens(&text.text),
        ContentBody::Thinking(thinking) => count_tokens(
            thinking
                .reasoning_content
                .as_ref()
                .unwrap_or(&thinking.text),
        ),
        ContentBody::Image(_)
        | ContentBody::Pdf(_)
        | ContentBody::Audio(_)
        | ContentBody::MediaReference(_) => Ok(MULTIMEDIA_TOKENS),
        ContentBody::ToolCall(call) => {
            let mut units = call.name.units().to_vec();
            units.extend(tool_input_json(&call.input).encode_utf16());
            count_tokens(&JsString::from_utf16(units))
        }
        ContentBody::ToolResult(result) => count_tokens(&flatten_tool_result_text(result)),
        ContentBody::ToolResultReference(reference) => count_tokens(&reference.preview),
    }
}

fn estimate_tool_schemas(tools: &[ToolSchema]) -> Result<usize, TokenizeError> {
    let mut total = 0;
    for tool in tools {
        let mut units = tool.name.units().to_vec();
        if let Some(description) = &tool.description {
            units.extend_from_slice(description.units());
        }
        let schema = JsValue::Object(tool.input_schema.clone());
        units.extend(schema_json(&schema).encode_utf16());
        total += count_tokens(&JsString::from_utf16(units))?;
    }
    Ok(total)
}

/// `TokenBudgetManager.ts:261-268` `safeJsonStringify` (tool-call input):
/// `undefined`/`null` cost the name string only, and serialization failures
/// degrade to `""`. This is NOT the `TokenAccountingRuntime.ts:480-486`
/// variant, which has no null special case (`JSON.stringify(null) == "null"`).
fn tool_input_json(value: &JsValue) -> String {
    if matches!(value, JsValue::Undefined | JsValue::Null) {
        return String::new();
    }
    json_or_empty(value)
}

/// `TokenAccountingRuntime.ts:480-486` `safeJsonStringify` (tool schema): the
/// only special case is a serialization failure, which degrades to `""`.
fn schema_json(value: &JsValue) -> String {
    json_or_empty(value)
}

fn json_or_empty(value: &JsValue) -> String {
    match stringify_json(value) {
        Ok(Some(text)) => text,
        _ => String::new(),
    }
}

fn flatten_tool_result_text(result: &ToolResult) -> JsString {
    let mut units = Vec::new();
    for (index, item) in result.content.iter().enumerate() {
        if index > 0 {
            units.push(u16::from(b'\n'));
        }
        append_tool_result_item(&mut units, item);
    }
    JsString::from_utf16(units)
}

fn append_tool_result_item(units: &mut Vec<u16>, item: &ToolResultItem) {
    match &item.body {
        ToolResultContent::Text(text) => units.extend_from_slice(text.text.units()),
        ToolResultContent::Image(image) => {
            units.extend("[Image: ".encode_utf16());
            units.extend_from_slice(image.mime_type.units());
            units.extend(", ".encode_utf16());
            if image.source == MediaSource::Url {
                units.extend_from_slice(image.data.units());
            } else {
                units.extend(image.data.len_utf16().to_string().encode_utf16());
                units.extend(" base64 characters".encode_utf16());
            }
            units.push(u16::from(b']'));
        }
        ToolResultContent::Pdf(pdf) => {
            units.extend("[PDF: application/pdf, ".encode_utf16());
            units.extend(pdf.data.len_utf16().to_string().encode_utf16());
            units.extend(" base64 characters".encode_utf16());
            if let Some(pages) = pdf.pages.filter(|pages| *pages != 0.0 && !pages.is_nan()) {
                units.extend(format!(", {} pages", js_number_string(pages)).encode_utf16());
            }
            units.push(u16::from(b']'));
        }
    }
}

fn js_number_string(value: f64) -> String {
    pilotdeck_protocol::number::number_to_string(value)
}

fn estimate_delimited_blocks(text: &JsString, name: &str) -> Result<usize, TokenizeError> {
    let open: Vec<u16> = format!("<{name}>").encode_utf16().collect();
    let close: Vec<u16> = format!("</{name}>").encode_utf16().collect();
    let units = text.units();
    let mut total = 0;
    let mut cursor = 0;
    while cursor < units.len() {
        let Some(start) = find_units(units, &open, cursor) else {
            break;
        };
        let Some(end) = find_units(units, &close, start + open.len()) else {
            break;
        };
        let end = end + close.len();
        total += count_tokens(&JsString::from_utf16(units[start..end].to_vec()))?;
        cursor = end;
    }
    Ok(total)
}

/// JS `indexOf` over UTF-16 code units (never splits surrogate pairs early).
fn find_units(haystack: &[u16], needle: &[u16], from: usize) -> Option<usize> {
    if from > haystack.len() {
        return None;
    }
    if needle.is_empty() {
        return Some(from);
    }
    let mut at = from;
    while at + needle.len() <= haystack.len() {
        if haystack[at..at + needle.len()] == *needle {
            return Some(at);
        }
        at += 1;
    }
    None
}

#[cfg(test)]
#[path = "model_cost_tests.rs"]
mod tests;
