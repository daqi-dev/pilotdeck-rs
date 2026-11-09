//! Session-local SDK catalog extensions. Every reveal keeps the final native
//! policy view; the search tool holds only a weak registry reference.
use crate::{
    host::{HostError, ModuleError, modules::capability::ToolDefinition},
    metadata::object,
    runtime::{
        tools::{
            ExecutableTool, ToolError, ToolFuture, ToolOutput, ToolRegistry, ToolRunContext,
            registry::WeakToolRegistry,
        },
        turn::submit::SubmitError,
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::sync::Arc;

fn trim(s: &JsString) -> JsString {
    let space = |v: &u16| matches!(*v,0x0009..=0x000d|0x0020|0x00a0|0x1680|0x2000..=0x200a|0x2028|0x2029|0x202f|0x205f|0x3000|0xfeff);
    let a = s
        .units()
        .iter()
        .position(|v| !space(v))
        .unwrap_or(s.len_utf16());
    let b = s
        .units()
        .iter()
        .rposition(|v| !space(v))
        .map_or(a, |i| i + 1);
    JsString::from_utf16(s.units()[a..b].to_vec())
}
fn fail(message: impl Into<JsString>) -> HostError {
    ModuleError::new(message).into()
}
fn named(prefix: &str, name: &JsString, suffix: &str) -> JsString {
    let mut s = prefix.encode_utf16().collect::<Vec<_>>();
    s.extend_from_slice(name.units());
    s.extend(suffix.encode_utf16());
    JsString::from_utf16(s)
}
fn contains(a: &JsString, b: &JsString) -> bool {
    b.is_empty() || a.units().windows(b.len_utf16()).any(|s| s == b.units())
}
#[derive(Clone)]
struct Entry {
    name: JsString,
    description: JsString,
    hint: Option<JsString>,
}
impl Entry {
    fn value(&self) -> JsValue {
        let mut o = object([
            ("name", JsValue::String(self.name.clone())),
            ("description", JsValue::String(self.description.clone())),
        ]);
        if let JsValue::Object(ref mut o) = o
            && let Some(h) = &self.hint
        {
            o.insert("searchHint".into(), JsValue::String(h.clone()));
        }
        o
    }
    fn search(&self) -> JsString {
        let mut s = self.name.units().to_vec();
        s.push(32);
        s.extend_from_slice(self.description.units());
        s.push(32);
        if let Some(h) = &self.hint {
            s.extend_from_slice(h.units());
        }
        JsString::from_utf16(s)
    }
}
struct Search {
    fields: JsObject,
    registry: WeakToolRegistry,
    entries: Vec<Entry>,
}
impl Search {
    fn run(&self, input: &JsValue) -> Result<ToolOutput, ToolError> {
        let query = input
            .get("query")
            .and_then(JsValue::as_string)
            .map(trim)
            .unwrap_or_default();
        if query.is_empty() {
            return Err(ToolError::new(
                "invalid_tool_input",
                "search_tools requires a non-empty query.",
            ));
        }
        let locale = crate::project_files::node_locale(|k| std::env::var(k).ok());
        let normalized = crate::model_catalog::lower(&query, &locale.id, false);
        let lower = self
            .entries
            .iter()
            .map(|e| crate::model_catalog::lower(&e.search(), &locale.id, false))
            .collect::<Vec<_>>();
        let mut matches = self
            .entries
            .iter()
            .zip(&lower)
            .filter(|(_, text)| contains(text, &normalized))
            .map(|(e, _)| e)
            .collect::<Vec<_>>();
        if matches.is_empty() {
            let tokens = normalized
                .units()
                .split(|u| trim(&JsString::from_utf16(vec![*u])).is_empty())
                .filter(|s| !s.is_empty())
                .map(|s| JsString::from_utf16(s.to_vec()))
                .collect::<Vec<_>>();
            if !tokens.is_empty() {
                matches = self
                    .entries
                    .iter()
                    .zip(&lower)
                    .filter(|(_, text)| tokens.iter().all(|token| contains(text, token)))
                    .map(|(e, _)| e)
                    .collect();
            }
        }
        let registry = self.registry.upgrade().ok_or_else(|| {
            ToolError::new("tool_unavailable", "The session tool catalog is closed.")
        })?;
        // Policy may have removed a previously indexed name. Search can never
        // reveal or advertise a tool absent from this exact scoped view.
        matches.retain(|e| registry.has(&e.name));
        for entry in &matches {
            registry.reveal(&entry.name);
        }
        let mut text = if matches.is_empty() {
            let quoted = pilotdeck_protocol::stringify_json(&JsValue::String(query.clone()))
                .ok()
                .flatten()
                .unwrap_or_default();
            format!("No deferred tools matched {quoted}.")
                .encode_utf16()
                .collect::<Vec<_>>()
        } else {
            format!(
                "Loaded {} deferred tool{}:",
                matches.len(),
                if matches.len() == 1 { "" } else { "s" }
            )
            .encode_utf16()
            .collect()
        };
        for entry in &matches {
            text.extend("\n- ".encode_utf16());
            text.extend_from_slice(entry.name.units());
            text.extend(": ".encode_utf16());
            text.extend_from_slice(entry.description.units());
        }
        if !matches.is_empty() {
            text.extend("\nUse the loaded tool names in a subsequent tool call.".encode_utf16());
        }
        Ok(ToolOutput {
            content: vec![object([
                ("type", "text".into()),
                ("text", JsValue::String(JsString::from_utf16(text))),
            ])],
            data: Some(object([
                ("query", JsValue::String(query)),
                (
                    "tools",
                    JsValue::Array(matches.iter().map(|e| e.value()).collect()),
                ),
            ])),
            ..Default::default()
        })
    }
}
impl ToolDefinition for Search {
    fn fields(&self) -> &JsObject {
        &self.fields
    }
    fn is_read_only(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(true))
    }
    fn is_concurrency_safe(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(true))
    }
}
impl ExecutableTool for Search {
    fn execute(&self, input: JsValue, _: ToolRunContext) -> ToolFuture<ToolOutput> {
        let result = self.run(&input);
        Box::pin(async move { result })
    }
}
/// Merge per-session SDK MCP declarations into the same deferred catalog as
/// the explicit SDK list. Project/plugin-only servers are intentionally not
/// included: the baseline consumes `sdkMcpServers` here.
pub(super) fn merge_mcp_deferred(
    config: &mut JsObject,
    servers: &JsObject,
) -> Result<(), HostError> {
    let mut rows = match config.get("deferredTools") {
        Some(JsValue::Array(rows)) => rows.clone(),
        _ => Vec::new(),
    };
    for (server, server_config) in servers.own_entries() {
        let Some(JsValue::Array(entries)) = server_config.get("deferredTools") else {
            continue;
        };
        for entry in entries {
            if rows.len() >= 1024 {
                return Err(fail("Native SDK deferred tool budget exceeded."));
            }
            let Some(name) = entry.get("name").and_then(JsValue::as_string) else {
                return Err(fail("SDK MCP deferred tool name is invalid."));
            };
            let wire = crate::runtime::mcp::naming::build_mcp_tool_wire_name(server, name);
            let mut row = JsObject::new();
            row.insert("name".into(), JsValue::String(wire));
            if let Some(hint) = entry.get("searchHint").filter(|v| !v.is_undefined()) {
                row.insert("searchHint".into(), hint.clone());
            }
            rows.push(JsValue::Object(row));
        }
    }
    if !rows.is_empty() {
        config.insert("deferredTools".into(), JsValue::Array(rows));
    }
    Ok(())
}

pub(super) fn install(registry: &ToolRegistry, config: &JsObject) -> Result<(), HostError> {
    if let Some(schema) = config.get("outputFormat").and_then(|v| v.get("schema")) {
        registry.unregister(&"structured_output".into())?;
        registry.register(Arc::new(
            crate::runtime::tools::builtin::structured::StructuredOutputTool::with_schema(
                schema.clone(),
            ),
        ))?;
    }
    let Some(JsValue::Array(rows)) = config.get("deferredTools") else {
        return Ok(());
    };
    let available = registry.list_all()?;
    let known = registry.registered_names();
    let mut entries: Vec<Entry> = vec![];
    for row in rows {
        let Some(name) = row.get("name").and_then(JsValue::as_string) else {
            return Err(fail("SDK deferred tool name is invalid."));
        };
        let tool = available
            .iter()
            .find(|tool| tool.fields().get("name") == Some(&JsValue::String(name.clone())));
        let Some(tool) = tool else {
            if known.contains(name) {
                continue;
            }
            return Err(fail(named(
                "SDK deferred tool ",
                name,
                " is not available in this session.",
            )));
        };
        let entry = Entry {
            name: name.clone(),
            description: tool
                .fields()
                .get("description")
                .and_then(JsValue::as_string)
                .cloned()
                .unwrap_or_default(),
            hint: row
                .get("searchHint")
                .and_then(JsValue::as_string)
                .filter(|s| !s.is_empty())
                .cloned(),
        };
        if let Some(index) = entries.iter().position(|e| e.name == *name) {
            entries[index] = entry;
        } else {
            entries.push(entry);
        }
    }
    if entries.is_empty() {
        return Ok(());
    }
    if registry.has(&"search_tools".into()) {
        return Err(fail(
            "Deferred SDK tools conflict with an existing search_tools registration.",
        ));
    }
    let collator = crate::project_files::node_collator(&crate::project_files::node_locale(|k| {
        std::env::var(k).ok()
    }))
    .map_err(|(_, s)| fail(s))?;
    entries.sort_by(|a, b| collator.compare_utf16(a.name.units(), b.name.units()));
    for entry in &entries {
        registry.hide(&entry.name);
    }
    let fields=object([("name","search_tools".into()),("description","Search deferred tools by capability. Matching tools become available for the next model request.".into()),("kind","session".into()),("inputSchema",pilotdeck_protocol::parse_json(r#"{"type":"object","required":["query"],"additionalProperties":false,"properties":{"query":{"type":"string","description":"The capability or task to search for."}}}"#).unwrap())]);
    registry.register(Arc::new(Search {
        fields: fields.as_object().unwrap().clone(),
        registry: registry.downgrade(),
        entries,
    }))?;
    Ok(())
}
pub(super) fn validate(config: &JsValue) -> Result<(), SubmitError> {
    let invalid = |s| SubmitError::new("INVALID_SDK_SESSION_CONFIG", s);
    if let Some(aliases) = config.get("toolAliases").filter(|v| !v.is_undefined())
        && !aliases.as_object().is_some_and(|o| {
            o.entries().iter().all(|(k, v)| {
                !trim(k).is_empty() && v.as_string().is_some_and(|s| !trim(s).is_empty())
            })
        })
    {
        return Err(invalid(
            "toolAliases must be a non-empty string-to-string map.",
        ));
    }
    if let Some(rows) = config.get("deferredTools").filter(|v| !v.is_undefined())
        && !matches!(rows,JsValue::Array(rows) if rows.len()<=1024 && rows.iter().all(|v|v.get("name").and_then(JsValue::as_string).is_some_and(|s|!trim(s).is_empty())&&v.get("searchHint").is_none_or(|s|s.is_undefined()||s.as_string().is_some())))
    {
        return Err(invalid(
            "deferredTools must contain canonical tool names and optional string search hints.",
        ));
    }
    if let Some(format) = config.get("outputFormat").filter(|v| !v.is_undefined())
        && (format.get("type") != Some(&JsValue::from("json_schema"))
            || !format.get("schema").is_some_and(schema_shape))
    {
        return Err(invalid(
            "outputFormat must be a json_schema object using PilotDeck's JSON-schema subset.",
        ));
    }
    Ok(())
}
fn schema_shape(value: &JsValue) -> bool {
    let mut pending = vec![(value, 0usize)];
    let mut work = 0;
    while let Some((value, depth)) = pending.pop() {
        work += 1;
        if depth > 128 || work > 100_000 {
            return false;
        }
        let Some(o) = value.as_object() else {
            return false;
        };
        if let Some(v) = o.get("type").filter(|v| !v.is_undefined())
            && v.as_string().is_none()
            && !matches!(v,JsValue::Array(a) if a.iter().all(|v|v.as_string().is_some()))
        {
            return false;
        }
        if let Some(v) = o.get("required").filter(|v| !v.is_undefined())
            && !matches!(v,JsValue::Array(a) if a.iter().all(|v|v.as_string().is_some()))
        {
            return false;
        }
        if o.get("additionalProperties")
            .is_some_and(|v| !v.is_undefined() && v.as_bool().is_none())
            || o.get("enum")
                .is_some_and(|v| !v.is_undefined() && !matches!(v, JsValue::Array(_)))
        {
            return false;
        }
        if let Some(v) = o.get("properties").filter(|v| !v.is_undefined()) {
            let Some(p) = v.as_object() else {
                return false;
            };
            pending.extend(p.entries().iter().map(|(_, v)| (v, depth + 1)));
        }
        if let Some(v) = o.get("items").filter(|v| !v.is_undefined()) {
            pending.push((v, depth + 1));
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::modules::tool_context::{HostToolCheckpoint, ToolRuntimeContext};
    struct Dummy(JsObject);
    impl ToolDefinition for Dummy {
        fn fields(&self) -> &JsObject {
            &self.0
        }
    }
    impl ExecutableTool for Dummy {
        fn execute(&self, _: JsValue, _: ToolRunContext) -> ToolFuture<ToolOutput> {
            Box::pin(async { Ok(ToolOutput::default()) })
        }
    }
    fn context() -> ToolRunContext {
        ToolRunContext::new(Arc::new(ToolRuntimeContext {
            fields: JsObject::new(),
            files: HostToolCheckpoint::default().tool_context_state(),
            abort_signal: None,
            model: None,
            elicitation: None,
            user_dialog: None,
            audit_recorder: None,
            file_history: None,
            file_update_notifier: None,
            now: None,
            progress: None,
            subagent: None,
            plan_todo: None,
            goal: None,
            plan_directory: None,
        }))
    }
    #[tokio::test]
    async fn real_typescript_search_oracle_reveals_the_same_scoped_catalog() {
        let fixture = pilotdeck_protocol::parse_json(include_str!(
            "../../tests/fixtures/native-sdk-deferred/oracle.json"
        ))
        .unwrap();
        let JsValue::Array(tools) = fixture.get("tools").unwrap() else {
            panic!()
        };
        let JsValue::Array(rows) = fixture.get("records").unwrap() else {
            panic!()
        };
        for row in rows {
            let registry = ToolRegistry::default();
            for tool in tools {
                registry
                    .register(Arc::new(Dummy(tool.as_object().unwrap().clone())))
                    .unwrap();
            }
            install(
                &registry,
                object([("deferredTools", JsValue::Array(tools.clone()))])
                    .as_object()
                    .unwrap(),
            )
            .unwrap();
            assert!(registry.is_hidden(&"archive".into()));
            let search = registry.get(&"search_tools".into()).unwrap();
            let answer = search
                .execute(
                    object([("query", row.get("query").unwrap().clone())]),
                    context(),
                )
                .await;
            if let Some(expected) = row.get("error") {
                let error = answer.err().unwrap();
                assert_eq!(
                    Some(&JsValue::String(error.message)),
                    expected.get("message")
                );
            } else {
                let answer = answer.unwrap();
                let actual = object([
                    ("content", JsValue::Array(answer.content)),
                    ("data", answer.data.unwrap()),
                ]);
                assert_eq!(Some(&actual), row.get("result"), "{:?}", row.get("query"));
            }
            let visible = JsValue::Array(
                registry
                    .list()
                    .unwrap()
                    .into_iter()
                    .filter_map(|t| t.fields().get("name").cloned())
                    .filter(|n| *n != JsValue::from("search_tools"))
                    .collect(),
            );
            assert_eq!(Some(&visible), row.get("visible"));
            let weak = registry.downgrade();
            drop(registry);
            assert!(
                weak.upgrade().is_none(),
                "registered search must not keep session catalog alive"
            );
            assert_eq!(
                search
                    .execute(object([("query", "history".into())]), context())
                    .await
                    .err()
                    .unwrap()
                    .code,
                "tool_unavailable"
            );
        }
    }
    #[test]
    fn deferred_search_cannot_restore_a_denied_tool_and_unknown_names_fail() {
        let base = ToolRegistry::default();
        base.register(Arc::new(Dummy(
            object([
                ("name", "write_file".into()),
                ("description", "write".into()),
            ])
            .as_object()
            .unwrap()
            .clone(),
        )))
        .unwrap();
        let scoped = base.scoped(Arc::new(|_| false)).unwrap();
        install(
            &scoped,
            object([(
                "deferredTools",
                JsValue::Array(vec![object([("name", "write_file".into())])]),
            )])
            .as_object()
            .unwrap(),
        )
        .unwrap();
        assert!(scoped.list_all().unwrap().is_empty());
        assert!(!scoped.has(&"search_tools".into()));
        assert!(
            install(
                &scoped,
                object([(
                    "deferredTools",
                    JsValue::Array(vec![object([("name", "missing".into())])])
                )])
                .as_object()
                .unwrap()
            )
            .is_err()
        );
    }
}

#[cfg(test)]
mod native_mcp_deferred_tests {
    use super::*;
    #[test]
    fn mcp_deferred_names_merge_after_explicit_sdk_entries_and_keep_utf16() {
        let mut config = object([(
            "deferredTools",
            JsValue::Array(vec![object([
                ("name", "mcp__a_b__echo".into()),
                ("searchHint", "old".into()),
            ])]),
        )])
        .as_object()
        .unwrap()
        .clone();
        let server = JsString::from_utf16(vec![97, 47, 98]);
        let mut servers = JsObject::new();
        servers.insert(
            server,
            object([(
                "deferredTools",
                JsValue::Array(vec![object([
                    ("name", "echo".into()),
                    ("searchHint", "new".into()),
                ])]),
            )]),
        );
        merge_mcp_deferred(&mut config, &servers).unwrap();
        let Some(JsValue::Array(rows)) = config.get("deferredTools") else {
            panic!("rows")
        };
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].get("name"), Some(&"mcp__a_b__echo".into()));
        assert_eq!(rows[1].get("searchHint"), Some(&"new".into()));
    }
    #[test]
    fn mcp_deferred_budget_fails_instead_of_dropping_tools() {
        let mut config = JsObject::new();
        let mut servers = JsObject::new();
        let rows = (0..1025).map(|_| object([("name", "x".into())])).collect();
        servers.insert(
            "s".into(),
            object([("deferredTools", JsValue::Array(rows))]),
        );
        assert!(merge_mcp_deferred(&mut config, &servers).is_err());
        assert!(config.get("deferredTools").is_none());
    }
}
