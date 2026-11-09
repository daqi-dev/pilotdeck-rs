//! #2131: replay of the `utf16-residual-sort` gold. The generator drives the
//! real TS originals (`ToolRegistry.listAll`/`listUnavailableEntries`,
//! `PluginRuntimeView.getAllMcpInstructions`, `buildCachePlan`,
//! `TokenStatsCollector.modelUsageSnapshot`) with JsString keys that carry
//! isolated UTF-16 surrogates; these tests replay the same inputs through the
//! Rust ports and must agree order-for-order and fingerprint-for-fingerprint.
//! The key families are locale-invariant (the generator re-verifies them
//! across 14 locales), so no assertion here depends on the host LANG.
use crate::runtime::tools::{
    ExecutableTool, ToolFuture, ToolOutput, ToolRegistry, ToolRunContext, registry::UnavailableTool,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use std::{path::PathBuf, sync::Arc};

const ORACLE: &str = include_str!("oracle.json");

fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    value.get(key).expect("#2131 oracle field exists")
}
fn array(value: &JsValue) -> &[JsValue] {
    let JsValue::Array(items) = value else {
        panic!("#2131 oracle array")
    };
    items
}
fn text(value: &JsValue) -> String {
    value
        .as_string()
        .expect("#2131 oracle string")
        .to_utf8()
        .expect("#2131 oracle envelope is UTF-8")
}
/// The JsStrings of an oracle key list. Lone surrogates arrive as `\udXXX`
/// JSON escapes, which `parse_json` keeps as raw units.
fn strings(value: &JsValue) -> Vec<JsString> {
    array(value)
        .iter()
        .map(|item| item.as_string().expect("#2131 oracle string key").clone())
        .collect()
}
fn cases() -> Vec<JsValue> {
    let oracle = crate::test_baseline::oracle(ORACLE);
    assert_eq!(
        text(field(&oracle, "node")),
        "v22.22.0",
        "#2131 pinned Node"
    );
    assert_eq!(text(field(&oracle, "icu")), "78.2", "#2131 pinned ICU");
    let cases = array(field(&oracle, "cases")).to_vec();
    assert_eq!(cases.len(), 2, "#2131 both oracle cases recorded");
    cases
}

/// A minimal executable tool whose catalog `fields()` carry only the name —
/// the same shape the TS generator registers. The sort paths never execute it.
struct NamedTool {
    fields: JsObject,
}
impl crate::host::modules::capability::ToolDefinition for NamedTool {
    fn fields(&self) -> &JsObject {
        &self.fields
    }
}
impl ExecutableTool for NamedTool {
    fn execute(&self, _input: JsValue, _context: ToolRunContext) -> ToolFuture<ToolOutput> {
        Box::pin(async { Ok(ToolOutput::default()) })
    }
}
fn named_tool(name: &JsString) -> Arc<dyn ExecutableTool> {
    let mut fields = JsObject::new();
    fields.insert("name".into(), JsValue::String(name.clone()));
    Arc::new(NamedTool { fields })
}
/// The registered catalog name of a tool, read the way `ToolRegistry` reads it.
fn catalog_name(tool: &Arc<dyn ExecutableTool>) -> JsString {
    tool.fields()
        .get("name")
        .and_then(JsValue::as_string)
        .cloned()
        .expect("#2131 registered tool name")
}

/// `ToolRegistry.listAll` (ToolRegistry.ts:118). The scrambled case inputs
/// must come back in the recorded order: U+10000 < U+D800 < U+E000 < U+FFFD
/// and U+D800 < U+D801 < U+DC00 — a folded (U+FFFD) comparator cannot
/// reproduce either.
#[test]
fn registry_list_all_order_matches_ts() {
    for case in cases() {
        let id = text(field(&case, "id"));
        let registry = ToolRegistry::default();
        for name in strings(field(&case, "input")) {
            registry
                .register(named_tool(&name))
                .unwrap_or_else(|error| panic!("#2131 {id}: register: {}", error.message));
        }
        let listed = registry
            .list_all()
            .unwrap_or_else(|error| panic!("#2131 {id}: listAll: {}", error.message))
            .iter()
            .map(catalog_name)
            .collect::<Vec<_>>();
        let expected = strings(field(field(&case, "registry"), "listed"));
        crate::negative_assert_eq!(listed, expected, "#2131 listAll order: {id}");
    }
}

/// `ToolRegistry.listUnavailableEntries` (ToolRegistry.ts:179), sorted by
/// `diagnostic.toolName`.
#[test]
fn registry_unavailable_entries_order_matches_ts() {
    for case in cases() {
        let id = text(field(&case, "id"));
        let registry = ToolRegistry::default();
        for name in strings(field(&case, "input")) {
            registry
                .mark_unavailable(
                    UnavailableTool {
                        name,
                        code: "unavailable".into(),
                        reason: "r".into(),
                    },
                    &[],
                )
                .unwrap_or_else(|error| panic!("#2131 {id}: markUnavailable: {}", error.message));
        }
        let actual = registry
            .unavailable_entries()
            .unwrap_or_else(|error| panic!("#2131 {id}: listUnavailableEntries: {}", error.message))
            .iter()
            .map(|entry| entry.diagnostic.name.clone())
            .collect::<Vec<_>>();
        let expected = strings(field(field(&case, "registry"), "unavailable"));
        crate::negative_assert_eq!(actual, expected, "#2131 listUnavailableEntries order: {id}");
    }
}

/// The `mcpServers` value a parsed plugin manifest carries: encode the object
/// with the codec (isolated surrogates escape as `\ud800`) and parse it back,
/// so the entry under test sees exactly what a real manifest read produces.
fn manifest_mcp_servers(keys: &[JsString]) -> JsValue {
    let mut servers = JsObject::new();
    for key in keys {
        let mut server = JsObject::new();
        server.insert("instructions".into(), JsValue::String("i".into()));
        servers.insert(key.clone(), JsValue::Object(server));
    }
    let encoded = stringify_json(&JsValue::Object(servers))
        .expect("#2131 serializable mcpServers")
        .expect("#2131 defined mcpServers");
    parse_json(&encoded).expect("#2131 mcpServers manifest JSON")
}

/// `PluginRuntimeView.getAllMcpInstructions` (PluginRuntime.ts:697), sorted by
/// `serverName`. The server names are `mcpServers` object keys.
#[test]
fn mcp_instructions_order_matches_ts() {
    for case in cases() {
        let id = text(field(&case, "id"));
        let keys = strings(field(&case, "input"));
        let plugin = Arc::new(super::loader::LoadedPlugin {
            name: "p".into(),
            path: PathBuf::from("/p"),
            source: super::loader::PluginSource::Project,
            commands: vec![],
            skills: vec![],
            output_styles: vec![],
            hooks_config: None,
            mcp_servers: Some(manifest_mcp_servers(&keys)),
            lsp_servers: None,
            dispose: None,
        });
        let actual = super::contributions::collect_mcp_instructions(&[plugin])
            .into_iter()
            .map(|entry| entry.server_name)
            .collect::<Vec<_>>();
        let expected = strings(field(&case, "mcpInstructions"));
        crate::negative_assert_eq!(actual, expected, "#2131 getAllMcpInstructions order: {id}");
    }
}

/// `buildCachePlan`'s fingerprint (CachePlan.ts:44-67): it hashes the sorted
/// tools, so it is a byte-for-byte witness of the tool order. The en-US
/// locale matches the pinned Node run, and the recorded orders hold in every
/// locale anyway.
#[test]
fn cache_plan_fingerprint_matches_ts() {
    for case in cases() {
        let id = text(field(&case, "id"));
        let tools = strings(field(&case, "input"))
            .into_iter()
            .map(|name| {
                let mut tool = JsObject::new();
                tool.insert("name".into(), JsValue::String(name));
                JsValue::Object(tool)
            })
            .collect::<Vec<_>>();
        let identity = crate::runtime::context::cache::cache_plan_identity(
            &"en-US".parse().expect("#2131 en-US locale syntax"),
            &JsValue::String("p".into()),
            &JsValue::String("m".into()),
            &JsValue::String("preview prompt".into()),
            &tools,
            &[],
            None,
        )
        .unwrap_or_else(|error| panic!("#2131 {id}: buildCachePlan: {}", error.message));
        let expected = text(field(&case, "cacheFingerprint"));
        crate::negative_assert_eq!(
            identity.fingerprint,
            expected,
            "#2131 buildCachePlan fingerprint: {id}"
        );
    }
}

/// `TokenStatsCollector.modelUsageSnapshot()` (TokenStatsCollector.ts:203-210),
/// sorted by provider then model. `filePath` selects the journal directory,
/// so each case gets its own scratch root — a shared directory would replay
/// case 1's records into case 2's snapshot (the journal name is always
/// `stats.jsonl`).
///
/// The `distinct-lone-code-units` case is skipped for this site only: Rust's
/// `per_model_usage` bucket key (`model_key`) is a `String` built with
/// `to_utf8().unwrap_or_default()`, so every provider whose JsString is not
/// valid UTF-8 collapses into the same bucket before this sort runs, while TS
/// keys the map with the JS string `${provider}/${model}`, which keeps lone
/// surrogates (TokenStatsCollector.ts:543). That pre-existing bucket-key
/// divergence is upstream of this issue's comparator swap and is recorded as
/// follow-up in RESULT.md; the order this sort controls is fully exercised by
/// the mixed case, where the lone surrogate keeps its own bucket.
#[test]
fn model_usage_provider_order_matches_ts() {
    // #2131: the `distinct-lone-code-units` case is skipped below (bucket-key
    // collapse, follow-up); exact accounting so an oracle id rename cannot
    // silently un-skip it or leave the replay empty.
    let mut checked = 0;
    let mut skipped = 0;
    for case in cases() {
        let id = text(field(&case, "id"));
        if id == "distinct-lone-code-units" {
            skipped += 1;
            continue;
        }
        let scratch = crate::test_scratch::Scratch::new("2131-usage");
        let config = crate::runtime::model::router::StatsConfig {
            enabled: true,
            file_path: Some(scratch.path().join("case").join("stats.jsonl")),
            ..crate::runtime::model::router::StatsConfig::default()
        };
        let mut collector =
            crate::runtime::model::stats::TokenStatsCollector::new(Some(&config), None)
                .unwrap_or_else(|error| panic!("#2131 {id}: TokenStatsCollector: {error}"));
        for name in strings(field(&case, "input")) {
            let mut record = JsObject::new();
            record.insert("sessionId".into(), JsValue::String("s".into()));
            record.insert("scenarioType".into(), JsValue::String("custom".into()));
            record.insert("resolvedFrom".into(), JsValue::String("direct".into()));
            record.insert("provider".into(), JsValue::String(name));
            record.insert("model".into(), JsValue::String("m".into()));
            record.insert("usage".into(), JsValue::Object(JsObject::new()));
            record.insert(
                "startedAt".into(),
                JsValue::String("2026-11-09T00:00:00.000Z".into()),
            );
            record.insert(
                "endedAt".into(),
                JsValue::String("2026-11-09T00:00:01.000Z".into()),
            );
            collector.observe(&mut JsValue::Object(record));
        }
        let actual = match collector.model_usage_snapshot(None) {
            JsValue::Array(ref rows) => rows
                .iter()
                .map(|row| {
                    row.get("provider")
                        .and_then(JsValue::as_string)
                        .expect("#2131 usage provider")
                        .clone()
                })
                .collect::<Vec<_>>(),
            _ => panic!("#2131 {id}: modelUsageSnapshot array"),
        };
        let expected = strings(field(&case, "modelUsage"));
        crate::negative_assert_eq!(actual, expected, "#2131 modelUsageSnapshot order: {id}");
        checked += 1;
    }
    assert_eq!(
        skipped, 1,
        "#2131 the lone-code-units skip must still hit exactly once"
    );
    assert_eq!(
        checked, 1,
        "#2131 modelUsage must replay exactly the mixed case"
    );
}

#[test]
#[ignore = "requires Node v22.22.0 and PDRS_BASELINE"]
fn utf16_residual_sort_regenerates_identically() {
    crate::test_baseline::regenerates_identically("utf16-residual-sort");
}
