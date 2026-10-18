#[path = "common/baseline.rs"]
mod baseline;

use pilotdeck_gateway::{
    host::{
        HostError,
        modules::{
            capability::ToolDefinition,
            tool_context::{HostToolCheckpoint, ToolRuntimeContext},
        },
    },
    runtime::tools::{
        ExecutableTool, ToolError, ToolFuture, ToolOutput, ToolRegistration, ToolRegistry,
        ToolRunContext, build_tool_error_recovery, create_tool_error_result,
        format_tool_error_content,
        registry::{UnavailableTool, repair_tool_name},
        with_operation_deadline_budget,
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};
use std::{collections::HashMap, sync::Arc};

const ORACLE: &str = include_str!("fixtures/tool-runtime-values/oracle.json");
const INSTANT: &str = "2026-09-11T00:00:00.000Z";
static UNDEFINED: JsValue = JsValue::Undefined;
fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    value.get(key).unwrap_or(&UNDEFINED)
}
fn string(value: &JsValue) -> &JsString {
    value.as_string().expect("fixture string")
}
fn text(value: &JsValue) -> String {
    string(value).to_utf8().expect("ASCII fixture selector")
}
fn array(value: &JsValue) -> &[JsValue] {
    match value {
        JsValue::Array(values) => values,
        _ => panic!("fixture array"),
    }
}
fn object(values: impl IntoIterator<Item = (&'static str, JsValue)>) -> JsObject {
    let mut object = JsObject::new();
    for (key, value) in values {
        object.insert(key.into(), value);
    }
    object
}
fn js(value: &JsString) -> JsValue {
    JsValue::String(value.clone())
}
fn equal(actual: &JsValue, expected: &JsValue, label: &str) {
    assert_eq!(
        stringify_json(actual).unwrap(),
        stringify_json(expected).unwrap(),
        "{label}"
    );
}
fn context(fields: JsObject, now: JsValue) -> ToolRunContext {
    ToolRunContext::new(Arc::new(ToolRuntimeContext {
        fields,
        files: HostToolCheckpoint::default().tool_context_state(),
        abort_signal: None,
        model: None,
        elicitation: None,
        user_dialog: None,
        audit_recorder: None,
        file_history: None,
        file_update_notifier: None,
        now: Some(Arc::new(move || now.clone())),
        progress: None,
        subagent: None,
        plan_todo: None,
        goal: None,
        plan_directory: None,
    }))
}

#[test]
fn recovery_and_raw_results_match_real_ts() {
    let oracle = baseline::oracle(ORACLE);
    let rows = array(field(&oracle, "recovery"));
    assert_eq!(rows.len(), 71);
    for row in rows {
        let input = field(row, "input");
        let id = text(field(input, "id"));
        let code = text(field(input, "code"));
        let name = string(field(input, "toolName"));
        let message = string(field(input, "message"));
        let cwd = string(field(input, "cwd"));
        let mode = string(field(input, "permissionMode"));
        let details = field(input, "details").as_object();
        let actual = build_tool_error_recovery(&code, name, message, cwd, mode, details);
        equal(&JsValue::Object(actual.clone()), field(row, "output"), &id);
        let error = ToolError {
            code,
            message: message.clone(),
            details: details.cloned(),
        };
        let runtime = context(
            object([("cwd", js(cwd)), ("permissionMode", js(mode))]),
            INSTANT.into(),
        );
        let result =
            create_tool_error_result(&id.clone().into(), name, &error, &INSTANT.into(), &runtime)
                .unwrap();
        equal(
            &JsValue::Object(result),
            field(row, "result"),
            &format!("{id}: result"),
        );
        let formatted =
            format_tool_error_content(actual.get("message").unwrap().as_string().unwrap(), details);
        let expected_content = array(field(field(row, "result"), "content"));
        equal(
            &JsValue::String(formatted),
            field(&expected_content[0], "text"),
            &format!("{id}: raw"),
        );
    }
}

#[test]
fn name_repair_matches_all_builtin_spellings_and_ambiguities() {
    let oracle = baseline::oracle(ORACLE);
    let rows = array(field(&oracle, "names"));
    assert_eq!(rows.len(), 126);
    for row in rows {
        let input = field(row, "input");
        let mut targets = array(field(input, "tools")).to_vec();
        if let JsValue::Array(additional) = field(input, "additional") {
            targets.extend(additional.clone());
        }
        let targets: Vec<_> = targets
            .iter()
            .map(|target| {
                let aliases = match field(target, "aliases") {
                    JsValue::Array(aliases) => {
                        aliases.iter().map(|alias| string(alias).clone()).collect()
                    }
                    _ => vec![],
                };
                (string(field(target, "name")).clone(), aliases)
            })
            .collect();
        let repaired = repair_tool_name(
            string(field(input, "raw")),
            &targets,
            field(input, "configured").as_object(),
        )
        .expect("name-repair corpus has no throwing inputs");
        let actual = repaired
            .map(|result| {
                JsValue::Object(object([
                    ("name", js(&result.name)),
                    ("reason", result.reason.into()),
                ]))
            })
            .unwrap_or(JsValue::Null);
        equal(&actual, field(row, "output"), &text(field(input, "id")));
    }
}

fn fixture_number(value: &JsValue) -> JsValue {
    match value
        .as_string()
        .and_then(|value| value.to_utf8().ok())
        .as_deref()
    {
        Some("NaN") => JsValue::Number(f64::NAN),
        Some("Infinity") => JsValue::Number(f64::INFINITY),
        _ => value.clone(),
    }
}
fn encoded_number(value: &JsValue) -> JsValue {
    match value {
        JsValue::Number(value) if value.is_nan() => "NaN".into(),
        JsValue::Number(value) if *value == f64::INFINITY => "Infinity".into(),
        JsValue::Number(value) if *value == f64::NEG_INFINITY => "-Infinity".into(),
        _ => value.clone(),
    }
}
#[test]
fn operation_deadline_only_narrows_the_child_budget() {
    let oracle = baseline::oracle(ORACLE);
    let rows = array(field(&oracle, "deadlines"));
    assert_eq!(rows.len(), 20);
    for row in rows {
        let input = field(row, "input");
        let mut fields = JsObject::new();
        if !field(input, "configured").is_undefined() {
            fields.insert(
                "subagentTimeoutMs".into(),
                fixture_number(field(input, "configured")),
            );
        }
        if let Some(raw) = input.get("configuredRaw") {
            fields.insert("subagentTimeoutMs".into(), raw.clone());
        }
        let now = if field(input, "now").is_undefined() {
            INSTANT.into()
        } else {
            field(input, "now").clone()
        };
        let mut runtime = context(fields.clone(), now);
        let execution = object([("operationDeadline", field(input, "deadline").clone())]);
        with_operation_deadline_budget(&mut runtime, &execution).unwrap();
        // Derived call fields must not mutate the original host-owned context.
        equal(
            &JsValue::Object(runtime.host.fields.clone()),
            &JsValue::Object(fields),
            "host context retained",
        );
        let mut actual = object([(
            "hasTimeout",
            JsValue::Bool(runtime.fields.contains_key("subagentTimeoutMs")),
        )]);
        if let Some(value) = runtime.fields.get("subagentTimeoutMs") {
            actual.insert("timeout".into(), encoded_number(value));
        }
        equal(
            &JsValue::Object(actual),
            field(row, "output"),
            &text(field(input, "id")),
        );
    }
}

struct FakeTool(JsObject);
impl ToolDefinition for FakeTool {
    fn fields(&self) -> &JsObject {
        &self.0
    }
    fn is_read_only(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(true))
    }
    fn is_concurrency_safe(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(true))
    }
}
impl ExecutableTool for FakeTool {
    fn execute(&self, _: JsValue, _: ToolRunContext) -> ToolFuture<ToolOutput> {
        Box::pin(async { Ok(ToolOutput::default()) })
    }
}
fn fake(spec: &JsValue) -> Arc<dyn ExecutableTool> {
    let name = field(spec, "name").clone();
    let description = if field(spec, "description").is_undefined() {
        name.clone()
    } else {
        field(spec, "description").clone()
    };
    let aliases = if field(spec, "aliases").is_undefined() {
        JsValue::Array(vec![])
    } else {
        field(spec, "aliases").clone()
    };
    Arc::new(FakeTool(object([
        ("name", name),
        ("description", description),
        ("aliases", aliases),
        ("kind", "custom".into()),
        (
            "inputSchema",
            JsValue::Object(object([("type", "object".into())])),
        ),
    ])))
}
fn descriptor(tool: &dyn ExecutableTool) -> JsValue {
    JsValue::Object(object(
        ["name", "description", "aliases"]
            .map(|key| (key, tool.fields().get(key).unwrap().clone())),
    ))
}
fn diagnostic(value: UnavailableTool) -> JsValue {
    JsValue::Object(object([
        ("toolName", js(&value.name)),
        ("code", js(&value.code)),
        ("reason", js(&value.reason)),
    ]))
}
fn handle_value(handle: &ToolRegistration) -> JsValue {
    JsValue::Object(object([
        ("name", js(&handle.name)),
        ("active", JsValue::Bool(handle.is_active())),
    ]))
}
fn registry_step(
    input: &JsValue,
    registries: &mut HashMap<String, ToolRegistry>,
    handles: &mut HashMap<String, ToolRegistration>,
) -> Result<JsValue, HostError> {
    let op = text(field(input, "op"));
    if op == "handle" {
        return Ok(handle_value(&handles[&text(field(input, "handle"))]));
    }
    if op == "handleDispose" {
        handles[&text(field(input, "handle"))].dispose();
        return Ok(JsValue::Null);
    }
    let id = text(field(input, "r"));
    if op == "create" {
        registries.insert(id, ToolRegistry::default());
        return Ok(JsValue::Null);
    }
    if op == "scope" || op == "snapshot" {
        let parent = &registries[&text(field(input, "parent"))];
        let child = if op == "snapshot" {
            parent.snapshot()?
        } else {
            let denied = match field(input, "denied") {
                JsValue::Array(values) => values.clone(),
                _ => vec![],
            };
            parent.scoped(Arc::new(move |tool| {
                !denied.contains(tool.fields().get("name").unwrap())
            }))?
        };
        registries.insert(id, child);
        return Ok(JsValue::Null);
    }
    let registry = &registries[&id];
    let name = field(input, "name").as_string();
    Ok(match op.as_str() {
        "register" | "replace" | "upsert" => {
            let tool = fake(field(input, "tool"));
            let handle = match op.as_str() {
                "register" => registry.register(tool)?,
                "replace" => registry.replace(tool)?,
                _ => registry.register_or_replace(tool)?,
            };
            let value = handle_value(&handle);
            if !field(input, "handle").is_undefined() {
                handles.insert(text(field(input, "handle")), handle);
            }
            value
        }
        "get" => registry
            .get(name.unwrap())
            .map(|tool| descriptor(tool.as_ref()))
            .unwrap_or(JsValue::Null),
        "has" => JsValue::Bool(registry.has(name.unwrap())),
        "hide" => JsValue::Bool(registry.hide(name.unwrap())),
        "reveal" => JsValue::Bool(registry.reveal(name.unwrap())),
        "isHidden" => JsValue::Bool(registry.is_hidden(name.unwrap())),
        "unregister" => JsValue::Bool(registry.unregister(name.unwrap())?),
        "markUnavailable" => {
            let value = field(input, "diagnostic");
            let diagnostic = UnavailableTool {
                name: string(field(value, "toolName")).clone(),
                code: string(field(value, "code")).clone(),
                reason: string(field(value, "reason")).clone(),
            };
            let aliases = match field(input, "aliases") {
                JsValue::Array(values) => values
                    .iter()
                    .map(|value| string(value).clone())
                    .collect::<Vec<_>>(),
                _ => vec![],
            };
            registry.mark_unavailable(diagnostic, &aliases)?;
            JsValue::Null
        }
        "unavailable" => registry
            .get_unavailable(name.unwrap())
            .map(diagnostic)
            .unwrap_or(JsValue::Null),
        "schemas" => JsValue::Array(
            registry
                .canonical_schemas()?
                .into_iter()
                .map(JsValue::Object)
                .collect(),
        ),
        "inspect" => JsValue::Object(object([
            (
                "state",
                if registry.is_disposed() {
                    "disposed"
                } else {
                    "active"
                }
                .into(),
            ),
            (
                "list",
                JsValue::Array(
                    registry
                        .list()?
                        .iter()
                        .map(|tool| descriptor(tool.as_ref()))
                        .collect(),
                ),
            ),
            (
                "all",
                JsValue::Array(
                    registry
                        .list_all()?
                        .iter()
                        .map(|tool| descriptor(tool.as_ref()))
                        .collect(),
                ),
            ),
            (
                "unavailable",
                JsValue::Array(
                    registry
                        .unavailable_entries()?
                        .into_iter()
                        .map(|entry| {
                            JsValue::Object(object([
                                ("diagnostic", diagnostic(entry.diagnostic)),
                                (
                                    "aliases",
                                    JsValue::Array(entry.aliases.iter().map(js).collect()),
                                ),
                            ]))
                        })
                        .collect(),
                ),
            ),
        ])),
        "dispose" => {
            registry.dispose();
            JsValue::Null
        }
        _ => panic!("unknown registry fixture operation {op}"),
    })
}

#[test]
fn registry_views_aliases_visibility_and_handles_match_real_ts() {
    let oracle = baseline::oracle(ORACLE);
    let rows = array(field(&oracle, "registry"));
    assert_eq!(rows.len(), 66);
    let mut registries = HashMap::new();
    let mut handles = HashMap::new();
    for (index, row) in rows.iter().enumerate() {
        let input = field(row, "input");
        let actual = match registry_step(input, &mut registries, &mut handles) {
            Ok(value) => object([("ok", JsValue::Bool(true)), ("value", value)]),
            Err(error) => object([
                ("ok", JsValue::Bool(false)),
                (
                    "error",
                    error
                        .module_error
                        .map(|error| js(&error.message))
                        .unwrap_or_else(|| JsValue::String(error.message.into())),
                ),
            ]),
        };
        equal(
            &JsValue::Object(actual),
            field(row, "output"),
            &format!("registry step {index}: {}", text(field(input, "op"))),
        );
    }
}
