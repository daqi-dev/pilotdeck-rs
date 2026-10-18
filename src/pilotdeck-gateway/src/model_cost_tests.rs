//! Golden replay of `tests/fixtures/model-cost/oracle.json`.
//!
//! The oracle is produced by the fixed TypeScript baseline; the generator is
//! `tests/fixtures/model-cost/generate.mts`. Numbers are compared exactly (no
//! epsilon), including the order of the floating-point additions.

use super::*;
use pilotdeck_protocol::JsValue;
use pilotdeck_protocol::model::{
    AudioBlock, CanonicalModelRequest, ContentBlock, ContentBody, ImageBlock, MediaSource,
    MediaType, Message, PdfBlock, Role, TextBlock, ThinkingBlock, ToolCall, ToolResult,
    ToolResultContent, ToolResultItem, ToolResultReference, ToolSchema, Usage,
};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const ORACLE: &str = include_str!("../tests/fixtures/model-cost/oracle.json");
const BASELINE_COMMIT: &str = "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85";

static UNDEFINED: JsValue = JsValue::Undefined;
static PARSED_ORACLE: OnceLock<JsValue> = OnceLock::new();

fn oracle() -> &'static JsValue {
    PARSED_ORACLE.get_or_init(|| crate::test_baseline::oracle(ORACLE))
}

fn field<'a>(value: &'a JsValue, name: &str) -> &'a JsValue {
    value.get(name).unwrap_or(&UNDEFINED)
}

fn items(value: &JsValue) -> &[JsValue] {
    match value {
        JsValue::Array(items) => items,
        other => panic!("expected array, got {other:?}"),
    }
}

fn text(value: &JsValue) -> String {
    value
        .as_string()
        .unwrap_or_else(|| panic!("expected string, got {value:?}"))
        .to_utf8()
        .expect("fixture strings are valid UTF-8")
}

fn optional_text(value: &JsValue) -> Option<JsString> {
    match value {
        JsValue::Undefined | JsValue::Null => None,
        JsValue::String(value) => Some(value.clone()),
        other => panic!("expected optional string, got {other:?}"),
    }
}

fn number(value: &JsValue) -> f64 {
    match value {
        JsValue::Number(value) => *value,
        other => panic!("expected number, got {other:?}"),
    }
}

fn optional_number(value: &JsValue) -> Option<f64> {
    match value {
        JsValue::Undefined | JsValue::Null => None,
        JsValue::Number(value) => Some(*value),
        other => panic!("expected optional number, got {other:?}"),
    }
}

fn boolean(value: &JsValue) -> bool {
    match value {
        JsValue::Bool(value) => *value,
        other => panic!("expected boolean, got {other:?}"),
    }
}

/// Exact equality: every rate/cost pair must agree bit-for-bit, not within an
/// epsilon. `-0.0 == 0.0` is intentional because JSON has no signed zero.
fn check_amount(actual: f64, expected: f64, context: &str) {
    assert!(
        actual == expected,
        "{context}: expected {expected:?}, got {actual:?}"
    );
}

fn check_optional_amount(actual: Option<f64>, expected: Option<f64>, context: &str) {
    match (actual, expected) {
        (None, None) => {}
        (Some(actual), Some(expected)) => check_amount(actual, expected, context),
        other => panic!("{context}: expected {expected:?}, got {other:?}"),
    }
}

fn pricing_map(value: &JsValue) -> Option<PricingMap> {
    match value {
        JsValue::Undefined | JsValue::Null => None,
        JsValue::Object(object) => {
            let mut map = PricingMap::new();
            for (key, body) in object.entries() {
                map.insert(
                    key.to_utf8().expect("pricing key is UTF-8"),
                    ModelPricing {
                        input: optional_number(field(body, "input")),
                        output: optional_number(field(body, "output")),
                        cache_read: optional_number(field(body, "cacheRead")),
                    },
                );
            }
            Some(map)
        }
        other => panic!("expected configured pricing object, got {other:?}"),
    }
}

fn usage(value: &JsValue) -> Option<Usage> {
    match value {
        JsValue::Undefined | JsValue::Null => None,
        JsValue::Object(_) => Some(Usage {
            input_tokens: optional_number(field(value, "inputTokens")),
            output_tokens: optional_number(field(value, "outputTokens")),
            cache_read_tokens: optional_number(field(value, "cacheReadTokens")),
            cache_write_tokens: optional_number(field(value, "cacheWriteTokens")),
            total_tokens: optional_number(field(value, "totalTokens")),
            native_cost: optional_number(field(value, "nativeCost")),
        }),
        other => panic!("expected usage object, got {other:?}"),
    }
}

fn parse_request(value: &JsValue) -> CanonicalModelRequest {
    let tools = match field(value, "tools") {
        JsValue::Undefined | JsValue::Null => None,
        JsValue::Array(_) => Some(
            items(field(value, "tools"))
                .iter()
                .map(parse_tool)
                .collect(),
        ),
        other => panic!("expected tools array, got {other:?}"),
    };
    CanonicalModelRequest {
        model: JsString::from(text(field(value, "model"))),
        provider: JsString::from(text(field(value, "provider"))),
        messages: items(field(value, "messages"))
            .iter()
            .map(parse_message)
            .collect(),
        system_prompt: optional_text(field(value, "systemPrompt")),
        tools,
        tool_choice: None,
        max_output_tokens: None,
        temperature: None,
        speed: None,
        thinking: None,
        stream: None,
        metadata: None,
        output_schema: None,
        cache_breakpoints: None,
        cache_plan: None,
    }
}

fn parse_message(value: &JsValue) -> Message {
    let role = match text(field(value, "role")).as_str() {
        "user" => Role::User,
        "assistant" => Role::Assistant,
        other => panic!("unknown role {other}"),
    };
    Message {
        role,
        content: items(field(value, "content"))
            .iter()
            .map(parse_block)
            .collect(),
        metadata: None,
    }
}

fn parse_tool(value: &JsValue) -> ToolSchema {
    ToolSchema {
        name: JsString::from(text(field(value, "name"))),
        description: optional_text(field(value, "description")),
        input_schema: field(value, "inputSchema")
            .as_object()
            .expect("tool inputSchema must be an object")
            .clone(),
    }
}

fn parse_block(value: &JsValue) -> ContentBlock {
    let kind = text(field(value, "type"));
    let body = match kind.as_str() {
        "text" => ContentBody::Text(TextBlock {
            text: text_block(value),
            block_id: None,
        }),
        "thinking" => ContentBody::Thinking(ThinkingBlock {
            text: text_block(value),
            block_id: None,
            signature: None,
            reasoning_content: optional_text(field(value, "reasoningContent")),
        }),
        "image" => ContentBody::Image(image_block(value)),
        "pdf" => ContentBody::Pdf(PdfBlock {
            data: JsString::from(text(field(value, "data"))),
            bytes: number(field(value, "bytes")),
            pages: optional_number(field(value, "pages")),
        }),
        "audio" => ContentBody::Audio(AudioBlock {
            source: media_source(field(value, "source")),
            data: JsString::from(text(field(value, "data"))),
            mime_type: JsString::from(text(field(value, "mimeType"))),
            bytes: optional_number(field(value, "bytes")),
            duration_seconds: None,
        }),
        "tool_call" => ContentBody::ToolCall(ToolCall {
            timeline: None,
            id: JsString::from(text(field(value, "id"))),
            name: JsString::from(text(field(value, "name"))),
            input: field(value, "input").clone(),
            raw: None,
        }),
        "tool_result" => ContentBody::ToolResult(ToolResult {
            tool_call_id: JsString::from(text(field(value, "toolCallId"))),
            content: items(field(value, "content"))
                .iter()
                .map(parse_tool_result_item)
                .collect(),
            is_error: None,
            raw: None,
        }),
        "tool_result_reference" => ContentBody::ToolResultReference(ToolResultReference {
            tool_call_id: JsString::from(text(field(value, "toolCallId"))),
            is_error: None,
            path: JsString::from(text(field(value, "path"))),
            read_file_path: None,
            original_bytes: number(field(value, "originalBytes")),
            preview: JsString::from(text(field(value, "preview"))),
            has_more: boolean(field(value, "hasMore")),
            mime_type: optional_text(field(value, "mimeType")),
            reason: None,
        }),
        "media_reference" => {
            ContentBody::MediaReference(pilotdeck_protocol::model::MediaReference {
                tool_call_id: None,
                path: JsString::from(text(field(value, "path"))),
                original_bytes: number(field(value, "originalBytes")),
                preview: JsString::from(text(field(value, "preview"))),
                has_more: boolean(field(value, "hasMore")),
                mime_type: JsString::from(text(field(value, "mimeType"))),
                media_type: match text(field(value, "mediaType")).as_str() {
                    "image" => MediaType::Image,
                    "pdf" => MediaType::Pdf,
                    "audio" => MediaType::Audio,
                    other => panic!("unknown media type {other}"),
                },
                pages: optional_number(field(value, "pages")),
                detail: None,
                reason: None,
            })
        }
        other => panic!("unknown content block {other}"),
    };
    ContentBlock {
        timeline: None,
        body,
    }
}

fn text_block(value: &JsValue) -> JsString {
    JsString::from(text(field(value, "text")))
}

fn image_block(value: &JsValue) -> ImageBlock {
    ImageBlock {
        source: media_source(field(value, "source")),
        data: JsString::from(text(field(value, "data"))),
        mime_type: JsString::from(text(field(value, "mimeType"))),
        bytes: optional_number(field(value, "bytes")),
        detail: None,
    }
}

fn media_source(value: &JsValue) -> MediaSource {
    match text(value).as_str() {
        "base64" => MediaSource::Base64,
        "url" => MediaSource::Url,
        other => panic!("unknown media source {other}"),
    }
}

fn parse_tool_result_item(value: &JsValue) -> ToolResultItem {
    let body = match text(field(value, "type")).as_str() {
        "text" => ToolResultContent::Text(TextBlock {
            text: text_block(value),
            block_id: None,
        }),
        "image" => ToolResultContent::Image(image_block(value)),
        "pdf" => ToolResultContent::Pdf(PdfBlock {
            data: JsString::from(text(field(value, "data"))),
            bytes: number(field(value, "bytes")),
            pages: optional_number(field(value, "pages")),
        }),
        other => panic!("unknown tool-result content {other}"),
    };
    ToolResultItem {
        timeline: None,
        body,
    }
}

const SOURCE_PATHS: [&str; 9] = [
    "src/router/utils/modelPricing.ts",
    "src/router/stats/TokenStatsCollector.ts",
    "src/context/budget/TokenAccountingRuntime.ts",
    "src/context/budget/TokenBudgetManager.ts",
    "src/context/budget/tokenizer.ts",
    "src/model/protocol/toolResultContent.ts",
    "src/model/catalog/providers.ts",
    "src/agent/loop/AgentLoop.ts",
    "src/agent/loop/nativeAgentTurnCapabilitiesAdapter.ts",
];

#[test]
fn oracle_comes_from_the_fixed_baseline() {
    assert_eq!(text(field(oracle(), "sourceCommit")), BASELINE_COMMIT);
    let hashes = field(oracle(), "sourceSha256");
    for source in SOURCE_PATHS {
        let hash = text(field(hashes, source));
        assert_eq!(hash.len(), 64, "missing or malformed hash for {source}");
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }
    assert!(!ORACLE.contains(".generate-"));
}

/// Regenerates the oracle from the real TS modules and compares bytes.
#[test]
#[ignore = "requires PDRS_BASELINE (PilotDeck cc7c4e6 with node_modules)"]
fn model_cost_fixtures_regenerate_identically_from_ts() {
    let baseline = PathBuf::from(
        std::env::var("PDRS_BASELINE").expect("set PDRS_BASELINE to the PilotDeck checkout"),
    );
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/model-cost");
    let scratch = std::env::temp_dir().join(format!("pdrs-model-cost-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&scratch).unwrap();
    let output = std::process::Command::new("node")
        .arg("--disable-warning=ExperimentalWarning")
        .arg("--import")
        .arg(baseline.join("node_modules/tsx/dist/loader.mjs"))
        .arg(fixtures.join("generate.mts"))
        .arg(&scratch)
        .env("PDRS_BASELINE", &baseline)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let generated = std::fs::read(scratch.join("oracle.json")).unwrap();
    let committed = std::fs::read(fixtures.join("oracle.json")).unwrap();
    assert_eq!(generated, committed, "oracle.json");
    assert!(
        !scratch.join("oracle.json.tmp").exists(),
        "generator left a temporary file behind"
    );
    let _ = std::fs::remove_dir_all(&scratch);
}

#[test]
fn real_ts_pricing_lookups_match() {
    let lookups = items(field(oracle(), "pricingLookups"));
    // Floors pinned to the current real-TS output (273); a shrunken catalog or
    // a truncated generation loop must fail instead of replaying a stale subset.
    assert!(lookups.len() >= 273);
    for case in lookups {
        let provider = text(field(case, "provider"));
        let model = text(field(case, "model"));
        let configured = pricing_map(field(case, "configured"));
        let (pricing, source) =
            lookup_model_pricing_with_source(&provider, &model, configured.as_ref());
        let expected = field(case, "pricing");
        let context = format!("{provider}/{model}");
        check_optional_amount(
            pricing.input,
            optional_number(field(expected, "input")),
            &format!("{context} input"),
        );
        check_optional_amount(
            pricing.output,
            optional_number(field(expected, "output")),
            &format!("{context} output"),
        );
        check_optional_amount(
            pricing.cache_read,
            optional_number(field(expected, "cacheRead")),
            &format!("{context} cacheRead"),
        );
        assert_eq!(source.as_str(), text(field(case, "source")), "{context}");
    }
}

#[test]
fn real_ts_cost_helpers_match() {
    let costs = items(field(oracle(), "costs"));
    assert!(costs.len() >= 885);
    for case in costs {
        let provider = text(field(case, "provider"));
        let model = text(field(case, "model"));
        let configured = pricing_map(field(case, "configured"));
        let tokens = number(field(case, "tokens"));
        let context = format!("{provider}/{model} tokens={tokens}");
        check_amount(
            calculate_input_cost(tokens, &provider, &model, configured.as_ref()),
            number(field(case, "input")),
            &format!("{context} input"),
        );
        check_amount(
            calculate_cache_read_cost(tokens, &provider, &model, configured.as_ref()),
            number(field(case, "cacheRead")),
            &format!("{context} cacheRead"),
        );
    }
}

#[test]
fn real_ts_usage_costs_match() {
    let cases = items(field(oracle(), "usageCosts"));
    assert!(cases.len() >= 1653);
    let mut saw_native = false;
    let mut saw_negative_router_estimate = false;
    for case in cases {
        let provider = text(field(case, "provider"));
        let model = text(field(case, "model"));
        let configured = pricing_map(field(case, "configured"));
        let usage = usage(field(case, "usage"));
        let context = format!("{provider}/{model} usage={:?}", field(case, "usage"));
        if usage.as_ref().is_some_and(|usage| {
            usage
                .native_cost
                .is_some_and(|native| native.is_finite() && native >= 0.0)
        }) {
            saw_native = true;
        }
        let expected_cost = optional_number(field(case, "costUsd"));
        let actual_cost =
            estimate_usage_cost(usage.as_ref(), &provider, &model, configured.as_ref());
        if expected_cost.is_none() {
            saw_negative_router_estimate = true;
        }
        check_optional_amount(actual_cost, expected_cost, &format!("{context} costUsd"));
        check_amount(
            estimate_cost(usage.as_ref(), &provider, &model, configured.as_ref()),
            number(field(case, "routerEstimate")),
            &format!("{context} routerEstimate"),
        );
        let Some(usage) = usage.as_ref() else {
            continue;
        };
        let breakdown = estimate_cost_breakdown(usage, &provider, &model, configured.as_ref());
        let expected = field(case, "breakdown");
        check_amount(
            breakdown.input,
            number(field(expected, "input")),
            &format!("{context} breakdown.input"),
        );
        check_amount(
            breakdown.output,
            number(field(expected, "output")),
            &format!("{context} breakdown.output"),
        );
        check_amount(
            breakdown.cache_read,
            number(field(expected, "cacheRead")),
            &format!("{context} breakdown.cacheRead"),
        );
        check_amount(
            breakdown.total,
            number(field(expected, "total")),
            &format!("{context} breakdown.total"),
        );
    }
    assert!(saw_native, "oracle must cover provider-reported nativeCost");
    assert!(
        saw_negative_router_estimate,
        "oracle must cover the nativeCost fallback after an invalid router estimate"
    );
}

#[test]
fn real_ts_local_input_estimates_match() {
    let requests = items(field(oracle(), "requests"));
    assert!(
        requests.len() >= 51,
        "at least 51 request shapes are required"
    );
    for case in requests {
        let name = text(field(case, "name"));
        let request = parse_request(field(case, "request"));
        let breakdown =
            estimate_request_breakdown(&request).expect("local tokenizer must initialize");
        let expected = field(case, "breakdown");
        for (key, actual) in [
            ("total", breakdown.total),
            ("system", breakdown.system),
            ("tools", breakdown.tools),
            ("messages", breakdown.messages),
            ("mcp", breakdown.mcp),
            ("memory", breakdown.memory),
        ] {
            assert_eq!(actual as f64, number(field(expected, key)), "{name} {key}");
        }
        assert_eq!(
            breakdown.system
                + breakdown.tools
                + breakdown.messages
                + breakdown.mcp
                + breakdown.memory,
            breakdown.total,
            "{name} additive invariant"
        );
        assert_eq!(
            estimate_request_input(&request).expect("local tokenizer must initialize"),
            breakdown.total,
            "{name} estimate_request_input"
        );
    }
}

#[test]
fn pricing_map_iterates_like_object_entries() {
    let property_order = field(oracle(), "propertyOrder");

    let check = |section: &JsValue, lookup_model: Option<&str>| {
        let entries = items(field(section, "entries"));
        let mut map = PricingMap::new();
        for key in items(field(section, "insertionOrder")) {
            let key = text(key);
            let entry = entries
                .iter()
                .find(|entry| text(field(entry, "key")) == key)
                .unwrap_or_else(|| panic!("missing entry {key}"));
            let pricing = field(entry, "pricing");
            map.insert(
                key,
                ModelPricing {
                    input: optional_number(field(pricing, "input")),
                    output: optional_number(field(pricing, "output")),
                    cache_read: optional_number(field(pricing, "cacheRead")),
                },
            );
        }
        let expected: Vec<String> = items(field(section, "objectEntries"))
            .iter()
            .map(text)
            .collect();
        let actual: Vec<String> = map.iter().map(|(key, _)| key.to_string()).collect();
        assert_eq!(actual, expected, "enumeration order");
        if let Some(model) = lookup_model {
            let lookup = field(section, "lookup");
            let (pricing, source) = lookup_model_pricing_with_source("custom", model, Some(&map));
            let expected_pricing = field(lookup, "pricing");
            check_optional_amount(
                pricing.input,
                optional_number(field(expected_pricing, "input")),
                "integer-key input",
            );
            check_optional_amount(
                pricing.output,
                optional_number(field(expected_pricing, "output")),
                "integer-key output",
            );
            assert_eq!(source.as_str(), text(field(lookup, "source")));
        }
        map
    };

    // Inserting "12" then "2" must still scan "2" first: JS array-index keys
    // enumerate in ascending numeric order, so `model12` selects the 2/4 card.
    check(field(property_order, "integerKeys"), Some("model12"));
    // Non-index keys keep insertion order, after the numeric keys.
    check(field(property_order, "mixedKeys"), None);
}

#[test]
fn array_index_matches_ecmascript_boundaries() {
    assert_eq!(array_index("0"), Some(0));
    assert_eq!(array_index("12"), Some(12));
    assert_eq!(array_index("4294967294"), Some(u32::MAX - 1));
    assert_eq!(array_index("4294967295"), None);
    for key in ["", "01", "-1", "+1", "1.0", "1e2", " 1", "0x10", "١"] {
        assert_eq!(array_index(key), None, "{key:?}");
    }
}

#[test]
fn built_in_pricing_regex_matches_js_non_unicode_semantics() {
    for (model, expected) in [
        // JS `/i` without `u` never folds a non-ASCII code unit onto ASCII.
        ("\u{212A}imi", PricingSource::FallbackEstimate),
        ("mi\u{17F}stral-large", PricingSource::FallbackEstimate),
        // JS `.` excludes exactly \n, \r, U+2028 and U+2029.
        ("claude\rsonnet", PricingSource::FallbackEstimate),
        ("claude\nsonnet", PricingSource::FallbackEstimate),
        ("deepseek\u{2028}flash", PricingSource::FallbackEstimate),
        ("deepseek\u{2029}flash", PricingSource::FallbackEstimate),
        // Vertical tab, form feed and tab are not line terminators.
        ("deepseek\u{0B}flash", PricingSource::BuiltInEstimate),
        ("deepseek\tflash", PricingSource::BuiltInEstimate),
        // A non-BMP or non-ASCII scalar between the literals is still one `.`.
        ("deepseek中flash", PricingSource::BuiltInEstimate),
    ] {
        let (pricing, source) = lookup_model_pricing_with_source("unknown", model, None);
        assert_eq!(source, expected, "{model:?}");
        if expected == PricingSource::BuiltInEstimate {
            assert_eq!(pricing.input, Some(0.20), "{model:?}");
            assert_eq!(pricing.output, Some(0.60), "{model:?}");
        }
    }
}

#[test]
fn pdf_pages_use_js_number_to_string() {
    let text = |pages: f64| {
        let result = ToolResult {
            tool_call_id: JsString::from("call"),
            content: vec![ToolResultItem {
                timeline: None,
                body: ToolResultContent::Pdf(PdfBlock {
                    data: JsString::from("x"),
                    bytes: 1.0,
                    pages: Some(pages),
                }),
            }],
            is_error: None,
            raw: None,
        };
        flatten_tool_result_text(&result)
            .to_utf8()
            .expect("fixture text is UTF-8")
    };
    assert_eq!(
        text(1e-7),
        "[PDF: application/pdf, 1 base64 characters, 1e-7 pages]"
    );
    assert_eq!(
        text(1e21),
        "[PDF: application/pdf, 1 base64 characters, 1e+21 pages]"
    );
    assert_eq!(
        text(2.5e21),
        "[PDF: application/pdf, 1 base64 characters, 2.5e+21 pages]"
    );
    assert_eq!(
        text(0.1 + 0.2),
        "[PDF: application/pdf, 1 base64 characters, 0.30000000000000004 pages]"
    );
    assert_eq!(
        text(f64::INFINITY),
        "[PDF: application/pdf, 1 base64 characters, Infinity pages]"
    );
    // 0, -0 and NaN are falsy in the TS template guard: no pages suffix.
    for falsy in [0.0, -0.0, f64::NAN] {
        assert_eq!(
            text(falsy),
            "[PDF: application/pdf, 1 base64 characters]",
            "{falsy:?}"
        );
    }
}
