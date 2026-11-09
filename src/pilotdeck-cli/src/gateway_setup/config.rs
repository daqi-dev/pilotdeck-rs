//! Setup deliberately has its own `env || join` paths, unlike resolvePilotHome.
use super::prompt::SetupResult;
use crate::{
    Failure,
    paths::{self, Env},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, number::number_to_string};
use pilotdeck_yaml::value::{Container, Value, ValueGraph};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub(super) struct SetupPaths {
    pub config: PathBuf,
    pub credentials: PathBuf,
    pub credentials_label: PathBuf,
    pub home_pilot: Option<PathBuf>,
    pub home_pilot_label: Option<PathBuf>,
    pub home_input: paths::HomeInput,
    pub cwd: PathBuf,
}
impl SetupPaths {
    pub fn new(env: &Env, cwd: &Path) -> Result<Self, Failure> {
        let home_input = paths::HomeInput::capture(env);
        // Explicit PILOT_HOME short-circuits homedir in the baseline. In
        // particular Feishu/WeCom/cancel do not require a future Weixin home.
        let home = match env.string("PILOT_HOME").filter(|s| !s.is_empty()) {
            Some(home) => PathBuf::from(home),
            None => {
                PathBuf::from(home_input.resolve()?.to_string_lossy().as_ref()).join(".pilotdeck")
            }
        };
        let config = env
            .string("PILOTDECK_CONFIG_PATH")
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("pilotdeck.yaml"));
        let credentials_label = join_clean(&home, "weixin-credentials.json");
        let home_pilot_label = env
            .string("HOME")
            .map(|home| join_clean(Path::new(&home), ".pilotdeck"));
        let home_pilot = home_pilot_label
            .as_ref()
            .map(|home| paths::resolve(cwd, home));
        Ok(Self {
            credentials_label,
            home_pilot,
            home_pilot_label,
            home_input,
            cwd: cwd.to_path_buf(),
            config: paths::resolve(cwd, &config),
            credentials: paths::resolve(cwd, &home.join("weixin-credentials.json")),
        })
    }

    pub fn weixin_home(&self) -> Result<(PathBuf, PathBuf), Failure> {
        if let (Some(home), Some(label)) = (&self.home_pilot, &self.home_pilot_label) {
            return Ok((home.clone(), label.clone()));
        }
        let home = self.home_input.resolve()?;
        let label = join_clean(Path::new(home.to_string_lossy().as_ref()), ".pilotdeck");
        Ok((paths::resolve(&self.cwd, &label), label))
    }
}

fn join_clean(base: &Path, name: &str) -> PathBuf {
    let mut out = PathBuf::new();
    for part in base.join(name).components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if out.file_name().is_some_and(|name| name != "..") {
                    out.pop();
                } else if !out.has_root() {
                    out.push("..");
                }
            }
            part => out.push(part.as_os_str()),
        }
    }
    out
}
fn empty_graph() -> ValueGraph {
    ValueGraph {
        root: Value::Null,
        nodes: Vec::new(),
    }
}
fn read_graph(path: &Path) -> SetupResult<ValueGraph> {
    let bytes = match pilotdeck_gateway::read_session_bytes(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(empty_graph()),
        Err(error) => {
            return Err(
                Failure::Error(paths::node_fs_message_or_unknown(&error, "open", path)).into(),
            );
        }
    };
    let invalid =
        || Failure::Error("Cannot save setup configuration: existing YAML is invalid.".into());
    let raw = String::from_utf8(bytes).map_err(|_| invalid())?;
    let document = pilotdeck_yaml::parse_document(&JsString::from(raw), Default::default());
    if !document.errors.is_empty() {
        return Err(invalid().into());
    }
    document.to_json().map_err(|_| invalid().into())
}
fn load_graph(path: &Path) -> ValueGraph {
    read_graph(path).unwrap_or_else(|_| empty_graph())
}
pub(super) fn load(path: &Path) -> JsValue {
    let graph = load_graph(path);
    project_graph(&graph)
}
pub(super) fn wecom_defaults(path: &Path) -> (JsValue, Option<ValueGraph>) {
    let graph = load_graph(path);
    let groups=graph_field(&graph,&["adapters","wecom","extra","groups"]).filter(|value|matches!(value,Value::Ref(id) if matches!(&graph.nodes[*id],Container::Object(_)|Container::Map(_)|Container::Set(_)))).cloned();
    let root = project_graph(&graph);
    (
        root,
        groups.map(|root| ValueGraph {
            root,
            nodes: graph.nodes,
        }),
    )
}
fn project_graph(graph: &ValueGraph) -> JsValue {
    // This projection is used only to read prompt defaults. Saving reloads and
    // edits the original graph, so aliases and cycles are never discarded.
    fn project(graph: &ValueGraph, value: &Value, active: &mut Vec<usize>) -> JsValue {
        match value {
            Value::Undefined => JsValue::Undefined,
            Value::Null => JsValue::Null,
            Value::Bool(v) => JsValue::Bool(*v),
            Value::Number(v) => JsValue::Number(*v),
            Value::String(v) => JsValue::String(v.as_ref().clone()),
            Value::Symbol { .. } => JsValue::Undefined,
            Value::Ref(id) => {
                if active.contains(id) {
                    return JsValue::Undefined;
                }
                active.push(*id);
                let result = match &graph.nodes[*id] {
                    Container::Object(entries) => {
                        let mut object = JsObject::new();
                        for (key, value) in entries {
                            object.insert(key.clone(), project(graph, value, active));
                        }
                        JsValue::Object(object)
                    }
                    Container::Array(values) => JsValue::Array(
                        values
                            .iter()
                            .map(|value| project(graph, value, active))
                            .collect(),
                    ),
                    Container::Map(_) | Container::Set(_) => JsValue::Object(JsObject::new()),
                };
                active.pop();
                result
            }
        }
    }
    project(graph, &graph.root, &mut Vec::new())
}
static UNDEFINED: JsValue = JsValue::Undefined;
pub(super) fn get<'a>(value: &'a JsValue, path: &[&str]) -> &'a JsValue {
    path.iter()
        .fold(value, |value, key| value.get(key).unwrap_or(&UNDEFINED))
}
pub(super) fn truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(b) => *b,
        JsValue::Number(n) => *n != 0.0 && !n.is_nan(),
        JsValue::String(s) => !s.is_empty(),
        _ => true,
    }
}
pub(super) fn text(value: &JsValue) -> String {
    match value {
        JsValue::Undefined => "undefined".into(),
        JsValue::Null => "null".into(),
        JsValue::String(s) => String::from_utf16_lossy(s.units()),
        JsValue::Number(n) => number_to_string(*n),
        JsValue::Bool(b) => b.to_string(),
        JsValue::Array(a) => a
            .iter()
            .map(|v| {
                if matches!(v, JsValue::Null | JsValue::Undefined) {
                    String::new()
                } else {
                    text(v)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        JsValue::Object(_) => "[object Object]".into(),
    }
}
pub(super) fn object(fields: impl IntoIterator<Item = (&'static str, JsValue)>) -> JsValue {
    let mut object = JsObject::new();
    for (key, value) in fields {
        object.insert(key.into(), value);
    }
    JsValue::Object(object)
}
pub(super) fn string(value: impl Into<String>) -> JsValue {
    JsValue::String(value.into().into())
}
pub(super) fn fallback(value: &JsValue, default: &str) -> String {
    if truthy(value) {
        text(value)
    } else {
        default.into()
    }
}

pub(super) fn write_adapter(path: &Path, platform: &str, adapter: JsValue) -> SetupResult<()> {
    write_adapter_inner(path, platform, adapter, None)
}
pub(super) fn write_wecom_adapter(
    path: &Path,
    adapter: JsValue,
    groups: Option<ValueGraph>,
) -> SetupResult<()> {
    write_adapter_inner(path, "wecom", adapter, groups)
}
fn write_adapter_inner(
    path: &Path,
    platform: &str,
    adapter: JsValue,
    groups: Option<ValueGraph>,
) -> SetupResult<()> {
    let target = super::save::Target::prepare(path)?;
    let mut graph = read_graph(path)?;
    if matches!(graph.root, Value::Null | Value::Undefined) {
        let id = graph.nodes.len();
        graph.nodes.push(Container::Object(Vec::new()));
        graph.root = Value::Ref(id);
    }
    let Value::Ref(root) = graph.root else {
        return Err(Failure::Error(format!(
            "Cannot create property 'adapters' on {} '{}'",
            js_type(&graph.root),
            value_text(&graph.root)
        ))
        .into());
    };
    if !matches!(&graph.nodes[root], Container::Object(_)) {
        return Err(Failure::Error(
            "Cannot save setup configuration: root must be a YAML mapping.".into(),
        )
        .into());
    }
    // setup reads its prompt defaults once, then writeWeComConfig reloads a
    // fresh graph. Import the captured groups as a distinct graph: preserving
    // aliases across those two independent YAML parses would invent sharing.
    let groups = groups.map(|mut captured| {
        let offset = graph.nodes.len();
        fn shift(value: &mut Value, offset: usize) {
            if let Value::Ref(id) = value {
                *id += offset;
            }
        }
        shift(&mut captured.root, offset);
        for node in &mut captured.nodes {
            match node {
                Container::Object(entries) => {
                    for (_, value) in entries {
                        shift(value, offset);
                    }
                }
                Container::Array(values) | Container::Set(values) => {
                    for value in values {
                        shift(value, offset);
                    }
                }
                Container::Map(entries) => {
                    for (key, value) in entries {
                        shift(key, offset);
                        shift(value, offset);
                    }
                }
            }
        }
        graph.nodes.extend(captured.nodes);
        captured.root
    });
    let adapter_value = from_js(&adapter, &mut graph.nodes);
    if let Some(groups) = groups
        && let Value::Ref(id) = adapter_value
        && let Some(Value::Ref(extra)) = match &graph.nodes[id] {
            Container::Object(entries) => entries
                .iter()
                .find(|(key, _)| key.equals_str("extra"))
                .map(|(_, value)| value.clone()),
            _ => None,
        }
        && let Container::Object(entries) = &mut graph.nodes[extra]
    {
        entries.push(("groups".into(), groups));
    }

    if let Container::Object(entries) = &mut graph.nodes[root] {
        let index = entries
            .iter()
            .position(|(key, _)| key.equals_str("adapters"));
        let existing = index
            .map(|i| entries[i].1.clone())
            .unwrap_or(Value::Undefined);
        let adapters = if value_truthy(&existing) {
            existing
        } else {
            let id = graph.nodes.len();
            graph.nodes.push(Container::Object(Vec::new()));
            Value::Ref(id)
        };
        let Value::Ref(id) = adapters else {
            return Err(Failure::Error(format!(
                "Cannot create property '{platform}' on {} '{}'",
                js_type(&adapters),
                value_text(&adapters)
            ))
            .into());
        };
        if !matches!(&graph.nodes[id], Container::Object(_)) {
            return Err(Failure::Error(
                "Cannot save setup configuration: adapters must be a YAML mapping.".into(),
            )
            .into());
        }
        if let Container::Object(values) = &mut graph.nodes[id] {
            if let Some((_, value)) = values.iter_mut().find(|(key, _)| key.equals_str(platform)) {
                *value = adapter_value;
            } else {
                values.push((platform.into(), adapter_value));
            }
        }
        if let Container::Object(entries) = &mut graph.nodes[root] {
            if let Some(index) = index {
                entries[index].1 = adapters;
            } else {
                entries.push(("adapters".into(), adapters));
            }
        }
    }
    let yaml = emit_graph(&graph)?;
    target.publish(yaml.as_bytes())?;
    Ok(())
}
fn graph_field<'a>(graph: &'a ValueGraph, path: &[&str]) -> Option<&'a Value> {
    let mut value = &graph.root;
    for key in path {
        let Value::Ref(id) = value else { return None };
        let Container::Object(entries) = &graph.nodes[*id] else {
            return None;
        };
        value = &entries.iter().find(|(name, _)| name.equals_str(key))?.1;
    }
    Some(value)
}
fn js_type(value: &Value) -> &'static str {
    match value {
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        _ => "object",
    }
}
fn value_text(value: &Value) -> String {
    match value {
        Value::Bool(v) => v.to_string(),
        Value::Number(v) => number_to_string(*v),
        Value::String(v) => String::from_utf16_lossy(v.units()),
        _ => "null".into(),
    }
}
fn value_truthy(value: &Value) -> bool {
    match value {
        Value::Undefined | Value::Null => false,
        Value::Bool(v) => *v,
        Value::Number(v) => *v != 0.0 && !v.is_nan(),
        Value::String(v) => !v.is_empty(),
        _ => true,
    }
}
fn from_js(value: &JsValue, nodes: &mut Vec<Container>) -> Value {
    match value {
        JsValue::Undefined => Value::Undefined,
        JsValue::Null => Value::Null,
        JsValue::Bool(v) => Value::Bool(*v),
        JsValue::Number(v) => Value::Number(*v),
        JsValue::String(v) => Value::String(std::sync::Arc::new(v.clone())),
        JsValue::Object(object) => {
            let values = entries(object)
                .into_iter()
                .map(|(key, value)| (key.clone(), from_js(value, nodes)))
                .collect();
            let id = nodes.len();
            nodes.push(Container::Object(values));
            Value::Ref(id)
        }
        JsValue::Array(values) => {
            let values = values.iter().map(|value| from_js(value, nodes)).collect();
            let id = nodes.len();
            nodes.push(Container::Array(values));
            Value::Ref(id)
        }
    }
}

fn graph_entries(values: &[(JsString, Value)]) -> Vec<&(JsString, Value)> {
    let mut entries: Vec<_> = values.iter().collect();
    entries.sort_by(|(a, _), (b, _)| match (a.array_index(), b.array_index()) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        _ => std::cmp::Ordering::Equal,
    });
    entries
}
fn emit_graph(graph: &ValueGraph) -> SetupResult<String> {
    // YAML allocates an anchor when it encounters an already seen object,
    // before rendering. A nested shared child may be a1 while its enclosing
    // self-referential object becomes a2.
    let mut anchors = HashMap::new();
    let mut visited = vec![false; graph.nodes.len()];
    let mut pending = vec![&graph.root];
    while let Some(value) = pending.pop() {
        if let Value::Ref(id) = value {
            if visited[*id] {
                let next = anchors.len() + 1;
                anchors.entry(*id).or_insert(next);
                continue;
            }
            visited[*id] = true;
            match &graph.nodes[*id] {
                Container::Object(entries) => pending.extend(
                    graph_entries(entries)
                        .into_iter()
                        .rev()
                        .map(|(_, value)| value),
                ),
                Container::Array(values) | Container::Set(values) => {
                    pending.extend(values.iter().rev())
                }
                Container::Map(entries) => {
                    for (key, value) in entries.iter().rev() {
                        pending.push(value);
                        pending.push(key);
                    }
                }
            }
        }
    }
    struct Emitter<'a> {
        graph: &'a ValueGraph,
        anchors: HashMap<usize, usize>,
        emitted: Vec<bool>,
    }
    impl Emitter<'_> {
        fn render(&mut self, value: &Value, depth: usize) -> SetupResult<String> {
            let Value::Ref(id) = value else {
                return Ok(match value {
                    Value::Undefined | Value::Null => "null".into(),
                    Value::Bool(v) => v.to_string(),
                    Value::Number(v) => scalar(&JsValue::Number(*v), false),
                    Value::String(v) => {
                        let mut out = String::new();
                        emit_scalar(&JsValue::String(v.as_ref().clone()), depth, &mut out);
                        out
                    }
                    Value::Symbol { .. } => {
                        return Err(Failure::Error("Cannot stringify a Symbol".into()).into());
                    }
                    Value::Ref(_) => unreachable!(),
                });
            };
            if self.emitted[*id] {
                return Ok(format!(
                    "*a{}",
                    self.anchors.get(id).expect("repeated object has anchor")
                ));
            }
            self.emitted[*id] = true;
            let anchor = self
                .anchors
                .get(id)
                .map_or_else(String::new, |anchor| format!("&a{anchor}"));
            let mut out = String::new();
            let graph = self.graph;
            match &graph.nodes[*id] {
                Container::Object(values) => {
                    let ordered = graph_entries(values);
                    if ordered.is_empty() {
                        return Ok(if anchor.is_empty() {
                            "{}".into()
                        } else {
                            format!("{anchor} {{}}")
                        });
                    }
                    if !anchor.is_empty() {
                        out.push_str(&anchor);
                        out.push('\n');
                    }
                    for (key, value) in ordered {
                        out.push_str(&" ".repeat(depth));
                        out.push_str(&scalar(&JsValue::String(key.clone()), true));
                        out.push(':');
                        let child = self.render(value, depth + 2)?;
                        self.child(&mut out, &child, depth + 2, false);
                    }
                }
                Container::Array(values) | Container::Set(values) => {
                    if values.is_empty() {
                        return Ok(if anchor.is_empty() {
                            "[]".into()
                        } else {
                            format!("{anchor} []")
                        });
                    }
                    if !anchor.is_empty() {
                        out.push_str(&anchor);
                        out.push('\n');
                    }
                    for value in values {
                        out.push_str(&" ".repeat(depth));
                        out.push('-');
                        let child = self.render(value, depth + 2)?;
                        self.child(&mut out, &child, depth + 2, true);
                    }
                }
                Container::Map(values) => {
                    if values.is_empty() {
                        return Ok(if anchor.is_empty() {
                            "{}".into()
                        } else {
                            format!("{anchor} {{}}")
                        });
                    }
                    if !anchor.is_empty() {
                        out.push_str(&anchor);
                        out.push('\n');
                    }
                    for (key, value) in values {
                        out.push_str(&" ".repeat(depth));
                        let rendered = self.render(key, depth + 2)?;
                        if matches!(key, Value::Ref(_)) || rendered.contains('\n') {
                            out.push_str("? ");
                            out.push_str(&rendered);
                            out.push('\n');
                            out.push_str(&" ".repeat(depth));
                            out.push(':');
                        } else {
                            out.push_str(&rendered);
                            out.push(':');
                        }
                        let child = self.render(value, depth + 2)?;
                        self.child(&mut out, &child, depth + 2, false);
                    }
                }
            }
            if out.ends_with('\n') {
                out.pop();
            }
            Ok(out)
        }
        fn child(&self, out: &mut String, child: &str, depth: usize, compact: bool) {
            let indent = " ".repeat(depth);
            if child.starts_with(&indent) && !child.is_empty() {
                if compact {
                    out.push(' ');
                    out.push_str(child.strip_prefix(&indent).unwrap());
                } else {
                    out.push('\n');
                    out.push_str(child);
                }
            } else {
                out.push(' ');
                out.push_str(child);
            }
            out.push('\n');
        }
    }
    let mut emitter = Emitter {
        graph,
        anchors,
        emitted: vec![false; graph.nodes.len()],
    };
    let mut out = emitter.render(&graph.root, 0)?;
    out.push('\n');
    Ok(out)
}

// YAML's default block layout with singleQuote:false and lineWidth:0. Parsed
// values are kept ordered and strings that could resolve as another type quote.
#[cfg(test)]
pub(super) fn emit(value: &JsValue) -> String {
    let mut nodes = Vec::new();
    let root = from_js(value, &mut nodes);
    emit_graph(&ValueGraph { root, nodes }).expect("fixture value serializes")
}
fn entries(object: &JsObject) -> Vec<(&JsString, &JsValue)> {
    let mut entries: Vec<_> = object
        .entries()
        .iter()
        .map(|(key, value)| (key, value))
        .collect();
    entries.sort_by(|(a, _), (b, _)| match (a.array_index(), b.array_index()) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        _ => std::cmp::Ordering::Equal,
    });
    entries
}
fn emit_scalar(value: &JsValue, depth: usize, out: &mut String) {
    if let JsValue::String(value) = value {
        let text = String::from_utf16_lossy(value.units());
        if text.contains('\n')
            && !text
                .chars()
                .any(|c| c == '\r' || (c < ' ' && c != '\n' && c != '\t'))
        {
            let trailing = text.bytes().rev().take_while(|b| *b == b'\n').count();
            out.push('|');
            if text
                .chars()
                .take_while(|c| matches!(c, ' ' | '\n'))
                .any(|c| c == ' ')
            {
                out.push(if depth == 0 { '1' } else { '2' });
            }
            out.push_str(if trailing == 0 {
                "-"
            } else if trailing == 1 && text != "\n" {
                ""
            } else {
                "+"
            });
            out.push('\n');
            let content = if trailing > 0 {
                &text[..text.len() - 1]
            } else {
                &text
            };
            for (index, line) in content.split('\n').enumerate() {
                if index > 0 {
                    out.push('\n');
                }
                if index == 0 || !line.is_empty() {
                    out.push_str(&" ".repeat(depth));
                    out.push_str(line);
                }
            }
            return;
        }
    }
    out.push_str(&scalar(value, false));
}
fn scalar(value: &JsValue, _key: bool) -> String {
    match value {
        JsValue::Null | JsValue::Undefined => "null".into(),
        JsValue::Bool(b) => b.to_string(),
        JsValue::Number(n) => {
            if n.is_nan() {
                ".nan".into()
            } else if n.is_infinite() {
                if *n < 0.0 {
                    "-.inf".into()
                } else {
                    ".inf".into()
                }
            } else {
                number_to_string(*n)
            }
        }
        JsValue::Object(_) => "{}".into(),
        JsValue::Array(_) => "[]".into(),
        JsValue::String(s) => {
            let value = String::from_utf16_lossy(s.units());
            let trimmed = value.trim();
            let unsafe_plain = value.is_empty()
                || trimmed != value
                || value
                    .chars()
                    .any(|c| (c < ' ' && c != '\t') || c == '\u{7f}')
                || value.starts_with(|c: char| "!&*{}[],#|>@`\"'%".contains(c))
                || value == "-"
                || value == "?"
                || value == ":"
                || value.starts_with("- ")
                || value.starts_with("? ")
                || value.starts_with(": ")
                || value.contains(": ")
                || value.contains(" #")
                || value.ends_with(':');
            let parsed = pilotdeck_yaml::parse_document(s, Default::default());
            let resolves_string = parsed.errors.is_empty()
                && parsed
                    .to_json()
                    .and_then(|g| g.to_js_value())
                    .is_ok_and(|v| matches!(v, JsValue::String(_)));
            if unsafe_plain || !resolves_string {
                yaml_quote(s)
            } else {
                value
            }
        }
    }
}

fn yaml_quote(value: &JsString) -> String {
    let units = value.units();
    let mut out = String::from("\"");
    let mut index = 0;
    while index < units.len() {
        let unit = units[index];
        match unit {
            0 => out.push_str("\\0"),
            7 => out.push_str("\\a"),
            11 => out.push_str("\\v"),
            27 => out.push_str("\\e"),
            8 => out.push_str("\\b"),
            9 => out.push_str("\\t"),
            10 => out.push_str("\\n"),
            12 => out.push_str("\\f"),
            13 => out.push_str("\\r"),
            1..=31 => out.push_str(&format!("\\x{unit:02x}")),
            34 => out.push_str("\\\""),
            92 => out.push_str("\\\\"),
            32 if units.get(index + 1) == Some(&10) => out.push_str("\\ "),
            0xd800..=0xdbff
                if units
                    .get(index + 1)
                    .is_some_and(|next| (0xdc00..=0xdfff).contains(next)) =>
            {
                let code = 0x10000
                    + ((u32::from(unit) - 0xd800) << 10)
                    + (u32::from(units[index + 1]) - 0xdc00);
                out.push(char::from_u32(code).expect("valid surrogate pair"));
                index += 1;
            }
            0xd800..=0xdfff => out.push_str(&format!("\\u{unit:04x}")),
            _ => out.push(char::from_u32(u32::from(unit)).expect("non-surrogate code unit")),
        }
        index += 1;
    }
    out.push('"');
    out
}
