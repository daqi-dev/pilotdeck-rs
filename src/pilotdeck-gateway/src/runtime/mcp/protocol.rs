//! MCP protocol types and wire validation, ported from the fixed TypeScript
//! baseline's `src/mcp/protocol/types.ts`, `src/mcp/config/*.ts`, and the
//! parts of `@modelcontextprotocol/sdk@1.29.0` (`types.js`, zod v4.4.3) that
//! decide what the client accepts and what it reports.
//!
//! ## Validation model
//!
//! The SDK parses every inbound frame and every result with zod schemas. The
//! parse output (not the raw JSON) is what the client sees: `z.object` keeps
//! only shape keys in shape order, `looseObject`/`catchall` append the other
//! keys in `Object.keys` order, `.default([])` fills `content`. A failed parse
//! surfaces as `ZodError.message`, i.e. `JSON.stringify(issues, null, 2)`.
//! [`Schema`] reproduces that subset of zod: the same issue codes, field
//! order, messages, abort flags, and the union rule (the first clean option
//! wins; a single non-aborted option reports its own issues; otherwise
//! `invalid_union` lists every option). Schema recursion is bounded by the
//! static schema depth, never by the input.

use super::naming::CallStackExceeded;
use crate::js_value;
use pilotdeck_protocol::{JsObject, JsString, JsValue, json_syntax, parse_json};
use std::{
    path::{Path, PathBuf},
    sync::LazyLock,
};

/// `LATEST_PROTOCOL_VERSION` of the SDK.
pub const LATEST_PROTOCOL_VERSION: &str = "2025-11-25";
/// `SUPPORTED_PROTOCOL_VERSIONS` of the SDK.
pub const SUPPORTED_PROTOCOL_VERSIONS: [&str; 5] = [
    LATEST_PROTOCOL_VERSION,
    "2025-06-18",
    "2025-03-26",
    "2024-11-05",
    "2024-10-07",
];
/// `MCP_CONFIG_FILE_NAME`.
pub const MCP_CONFIG_FILE_NAME: &str = "mcp.json";

/// `PilotDeckMcpServerSpec`.
#[derive(Clone, Debug, PartialEq)]
pub struct McpServerSpec {
    pub id: String,
    pub transport: McpTransportSpec,
    /// Optional per-server tool timeout for slow first-load operations.
    pub call_timeout_ms: Option<f64>,
}

/// The `transport` arms of `PilotDeckMcpServerSpec`. Maps keep JS insertion
/// order.
#[derive(Clone, Debug, PartialEq)]
pub enum McpTransportSpec {
    Stdio {
        command: String,
        args: Option<Vec<String>>,
        env: Option<Vec<(String, String)>>,
        cwd: Option<String>,
        /// Inject a unique `--user-data-dir` per spawn (removed on close).
        per_session: bool,
    },
    StreamableHttp {
        url: String,
        headers: Option<Vec<(String, String)>>,
    },
    /// Legacy Server-Sent Events transport.
    Sse {
        url: String,
        headers: Option<Vec<(String, String)>>,
    },
}

/// `PilotDeckMcpStatus`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpStatus {
    Idle,
    Connecting,
    Ready,
    Error,
    NeedsAuth,
}
impl McpStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Connecting => "connecting",
            Self::Ready => "ready",
            Self::Error => "error",
            Self::NeedsAuth => "needs-auth",
        }
    }
}

/// `PilotDeckMcpToolSpec`: already sanitized (M9), wire-named (M10) and
/// description-truncated (M11). `annotations` and `meta` are the parsed
/// values as the SDK returns them (annotations keep `title`, as in TS).
#[derive(Clone, Debug, PartialEq)]
pub struct McpToolSpec {
    pub server_id: JsString,
    pub tool_name: JsString,
    pub wire_name: JsString,
    pub description: JsString,
    pub input_schema: JsValue,
    pub annotations: Option<JsValue>,
    pub meta: Option<JsValue>,
}
impl McpToolSpec {
    /// The TS object literal, in its key order (`undefined` keys omitted).
    pub fn to_value(&self) -> JsValue {
        let mut object = JsObject::new();
        object.insert("serverId".into(), JsValue::String(self.server_id.clone()));
        object.insert("toolName".into(), JsValue::String(self.tool_name.clone()));
        object.insert("wireName".into(), JsValue::String(self.wire_name.clone()));
        object.insert(
            "description".into(),
            JsValue::String(self.description.clone()),
        );
        object.insert("inputSchema".into(), self.input_schema.clone());
        if let Some(annotations) = &self.annotations {
            object.insert("annotations".into(), annotations.clone());
        }
        if let Some(meta) = &self.meta {
            object.insert("meta".into(), meta.clone());
        }
        JsValue::Object(object)
    }
}

// ---------------------------------------------------------------------------
// zod subset
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum StringCheck {
    None,
    /// `z.string().refine(atob)` — "Invalid Base64 string".
    Base64,
    /// `z.iso.datetime({ offset: true })`.
    DateTime,
}

enum Rest {
    /// `z.object`: unknown keys dropped.
    Strip,
    /// `z.looseObject` / `.catchall(z.unknown())`: unknown keys appended.
    Loose,
    /// `.strict()`: unknown keys are an `unrecognized_keys` issue.
    Strict,
}

enum Presence {
    Required,
    Optional,
    /// `.default([])`.
    DefaultEmptyArray,
}

struct Field {
    name: &'static str,
    schema: Schema,
    presence: Presence,
}

enum Schema {
    Unknown,
    String(StringCheck),
    Number {
        min: Option<f64>,
        max: Option<f64>,
    },
    /// `z.number().int()`.
    Int,
    Bool,
    Literal(&'static str),
    Enum(&'static [&'static str]),
    /// `z.custom(v => v !== null && typeof v === 'object')`.
    AssertObject,
    Array(Box<Schema>),
    /// `z.record(z.string(), value)`.
    Record(Box<Schema>),
    Object(Vec<Field>, Rest),
    Union(Vec<Schema>),
}

#[derive(Clone, Debug)]
enum Segment {
    Key(JsString),
    Index(usize),
}

#[derive(Clone, Debug)]
enum IssueKind {
    InvalidType {
        expected: &'static str,
        received: &'static str,
        safeint: bool,
    },
    InvalidValue {
        values: &'static [&'static str],
        literal: bool,
    },
    UnrecognizedKeys(Vec<JsString>),
    Union(Vec<Vec<Issue>>),
    Custom(&'static str),
    TooBig(f64),
    TooSmall(f64),
    InvalidDateTime,
}

#[derive(Clone, Debug)]
struct Issue {
    kind: IssueKind,
    /// Relative to the value the issue was raised for; prefixed on the way up.
    path: Vec<Segment>,
    /// zod `continue !== true`: type errors abort, refinements do not.
    aborting: bool,
}

/// ISO datetime with offset, exactly as zod prints it in the issue.
const DATETIME_PATTERN: &str = r"/^(?:(?:\d\d[2468][048]|\d\d[13579][26]|\d\d0[48]|[02468][048]00|[13579][26]00)-02-29|\d{4}-(?:(?:0[13578]|1[02])-(?:0[1-9]|[12]\d|3[01])|(?:0[469]|11)-(?:0[1-9]|[12]\d|30)|(?:02)-(?:0[1-9]|1\d|2[0-8])))T(?:(?:[01]\d|2[0-3]):[0-5]\d(?::[0-5]\d(?:\.\d+)?)?(?:Z|([+-](?:[01]\d|2[0-3]):[0-5]\d)))$/";
static DATETIME: LazyLock<regex::Regex> = LazyLock::new(|| {
    // JS `\d` is ASCII-only; Rust's is Unicode, so spell it out.
    let source = &DATETIME_PATTERN[1..DATETIME_PATTERN.len() - 1];
    regex::Regex::new(&source.replace(r"\d", "[0-9]")).expect("datetime pattern")
});

fn received(value: &JsValue) -> &'static str {
    match value {
        JsValue::Undefined => "undefined",
        JsValue::Null => "null",
        JsValue::Bool(_) => "boolean",
        JsValue::Number(n) if n.is_nan() => "NaN",
        JsValue::Number(_) => "number",
        JsValue::String(_) => "string",
        JsValue::Array(_) => "array",
        JsValue::Object(_) => "object",
    }
}

fn invalid_type(expected: &'static str, value: &JsValue) -> Issue {
    Issue {
        kind: IssueKind::InvalidType {
            expected,
            received: received(value),
            safeint: false,
        },
        path: Vec::new(),
        aborting: true,
    }
}

fn check(kind: IssueKind) -> Issue {
    Issue {
        kind,
        path: Vec::new(),
        aborting: false,
    }
}

/// WHATWG forgiving-base64 decode acceptance (`atob` does not throw).
fn is_atob_input(value: &JsString) -> bool {
    let mut units: Vec<u16> = value
        .units()
        .iter()
        .copied()
        .filter(|unit| !matches!(unit, 0x09 | 0x0A | 0x0C | 0x0D | 0x20))
        .collect();
    let equals = u16::from(b'=');
    if units.len().is_multiple_of(4) {
        if units.ends_with(&[equals, equals]) {
            units.truncate(units.len() - 2);
        } else if units.ends_with(&[equals]) {
            units.truncate(units.len() - 1);
        }
    }
    if units.len() % 4 == 1 {
        return false;
    }
    units
        .iter()
        .all(|unit| matches!(unit, 0x30..=0x39 | 0x41..=0x5A | 0x61..=0x7A | 0x2B | 0x2F))
}

struct Parsed {
    value: JsValue,
    issues: Vec<Issue>,
}
impl Parsed {
    fn ok(value: JsValue) -> Self {
        Self {
            value,
            issues: Vec::new(),
        }
    }
    fn fail(value: JsValue, issue: Issue) -> Self {
        Self {
            value,
            issues: vec![issue],
        }
    }
    fn aborted(&self) -> bool {
        self.issues.iter().any(|issue| issue.aborting)
    }
}

fn prefixed(issues: Vec<Issue>, segment: &Segment) -> impl Iterator<Item = Issue> + '_ {
    issues.into_iter().map(move |mut issue| {
        issue.path.insert(0, segment.clone());
        issue
    })
}

impl Schema {
    fn parse(&self, value: &JsValue) -> Parsed {
        match self {
            Self::Unknown => Parsed::ok(value.clone()),
            Self::String(kind) => {
                let Some(text) = value.as_string() else {
                    return Parsed::fail(value.clone(), invalid_type("string", value));
                };
                let valid = match kind {
                    StringCheck::None => true,
                    StringCheck::Base64 => is_atob_input(text),
                    StringCheck::DateTime => {
                        text.to_utf8().is_ok_and(|text| DATETIME.is_match(&text))
                    }
                };
                if valid {
                    return Parsed::ok(value.clone());
                }
                let kind = match kind {
                    StringCheck::Base64 => IssueKind::Custom("Invalid Base64 string"),
                    _ => IssueKind::InvalidDateTime,
                };
                Parsed::fail(value.clone(), check(kind))
            }
            Self::Number { min, max } => {
                let Some(number) = value.as_number().filter(|n| n.is_finite()) else {
                    return Parsed::fail(value.clone(), invalid_type("number", value));
                };
                let mut parsed = Parsed::ok(value.clone());
                if let Some(min) = min.filter(|min| number < *min) {
                    parsed.issues.push(check(IssueKind::TooSmall(min)));
                }
                if let Some(max) = max.filter(|max| number > *max) {
                    parsed.issues.push(check(IssueKind::TooBig(max)));
                }
                parsed
            }
            Self::Int => {
                let Some(number) = value.as_number().filter(|n| n.is_finite()) else {
                    return Parsed::fail(value.clone(), invalid_type("number", value));
                };
                if number.fract() != 0.0 {
                    let mut issue = invalid_type("int", value);
                    if let IssueKind::InvalidType { safeint, .. } = &mut issue.kind {
                        *safeint = true;
                    }
                    return Parsed::fail(value.clone(), issue);
                }
                const MAX_SAFE: f64 = 9_007_199_254_740_991.0;
                if number > MAX_SAFE {
                    return Parsed::fail(value.clone(), check(IssueKind::TooBig(MAX_SAFE)));
                }
                if number < -MAX_SAFE {
                    return Parsed::fail(value.clone(), check(IssueKind::TooSmall(-MAX_SAFE)));
                }
                Parsed::ok(value.clone())
            }
            Self::Bool => match value {
                JsValue::Bool(_) => Parsed::ok(value.clone()),
                _ => Parsed::fail(value.clone(), invalid_type("boolean", value)),
            },
            Self::Literal(expected) => match value.as_string() {
                Some(text) if text.equals_str(expected) => Parsed::ok(value.clone()),
                _ => Parsed::fail(
                    value.clone(),
                    Issue {
                        kind: IssueKind::InvalidValue {
                            values: literal_values(expected),
                            literal: true,
                        },
                        path: Vec::new(),
                        aborting: true,
                    },
                ),
            },
            Self::Enum(values) => match value.as_string() {
                Some(text) if values.iter().any(|v| text.equals_str(v)) => {
                    Parsed::ok(value.clone())
                }
                _ => Parsed::fail(
                    value.clone(),
                    Issue {
                        kind: IssueKind::InvalidValue {
                            values,
                            literal: false,
                        },
                        path: Vec::new(),
                        aborting: true,
                    },
                ),
            },
            Self::AssertObject => match value {
                JsValue::Object(_) | JsValue::Array(_) => Parsed::ok(value.clone()),
                _ => Parsed::fail(value.clone(), check(IssueKind::Custom("Invalid input"))),
            },
            Self::Array(item) => {
                let JsValue::Array(items) = value else {
                    return Parsed::fail(value.clone(), invalid_type("array", value));
                };
                let mut out = Vec::with_capacity(items.len());
                let mut issues = Vec::new();
                for (index, element) in items.iter().enumerate() {
                    let parsed = item.parse(element);
                    issues.extend(prefixed(parsed.issues, &Segment::Index(index)));
                    out.push(parsed.value);
                }
                Parsed {
                    value: JsValue::Array(out),
                    issues,
                }
            }
            Self::Record(item) => {
                let JsValue::Object(object) = value else {
                    return Parsed::fail(value.clone(), invalid_type("record", value));
                };
                let mut out = JsObject::new();
                let mut issues = Vec::new();
                for (key, element) in object.own_entries() {
                    // ZodRecord skips this own key before validating its value.
                    if key.equals_str("__proto__") {
                        continue;
                    }
                    let parsed = item.parse(element);
                    issues.extend(prefixed(parsed.issues, &Segment::Key(key.clone())));
                    out.insert(key.clone(), parsed.value);
                }
                Parsed {
                    value: JsValue::Object(out),
                    issues,
                }
            }
            Self::Object(fields, rest) => {
                let JsValue::Object(object) = value else {
                    return Parsed::fail(value.clone(), invalid_type("object", value));
                };
                let mut out = JsObject::new();
                let mut issues = Vec::new();
                for field in fields {
                    let present = object.get(field.name).filter(|v| !v.is_undefined());
                    let input = match (present, &field.presence) {
                        (Some(value), _) => value.clone(),
                        (None, Presence::Optional) => continue,
                        (None, Presence::DefaultEmptyArray) => {
                            out.insert(field.name.into(), JsValue::Array(Vec::new()));
                            continue;
                        }
                        (None, Presence::Required) => JsValue::Undefined,
                    };
                    let parsed = field.schema.parse(&input);
                    issues.extend(prefixed(parsed.issues, &Segment::Key(field.name.into())));
                    if !parsed.value.is_undefined() {
                        out.insert(field.name.into(), parsed.value);
                    }
                }
                let extra: Vec<_> = object
                    .own_entries()
                    .into_iter()
                    .filter(|(key, _)| {
                        !key.equals_str("__proto__")
                            && !fields.iter().any(|f| key.equals_str(f.name))
                    })
                    .collect();
                match rest {
                    Rest::Strip => {}
                    Rest::Loose => {
                        for (key, element) in extra {
                            out.insert(key.clone(), element.clone());
                        }
                    }
                    Rest::Strict => {
                        if !extra.is_empty() {
                            issues.push(Issue {
                                kind: IssueKind::UnrecognizedKeys(
                                    extra.into_iter().map(|(key, _)| key.clone()).collect(),
                                ),
                                path: Vec::new(),
                                aborting: true,
                            });
                        }
                    }
                }
                Parsed {
                    value: JsValue::Object(out),
                    issues,
                }
            }
            Self::Union(options) => {
                let results: Vec<Parsed> = options.iter().map(|o| o.parse(value)).collect();
                if let Some(index) = results.iter().position(|r| r.issues.is_empty()) {
                    return results
                        .into_iter()
                        .nth(index)
                        .expect("position is in range");
                }
                let mut open = results.iter().enumerate().filter(|(_, r)| !r.aborted());
                if let (Some((index, _)), None) = (open.next(), open.next()) {
                    return results.into_iter().nth(index).expect("index is in range");
                }
                Parsed::fail(
                    value.clone(),
                    Issue {
                        kind: IssueKind::Union(results.into_iter().map(|r| r.issues).collect()),
                        path: Vec::new(),
                        aborting: true,
                    },
                )
            }
        }
    }
}

fn literal_values(expected: &'static str) -> &'static [&'static str] {
    // Literals are fixed protocol strings; keep one static slice per value.
    match expected {
        "2.0" => &["2.0"],
        "object" => &["object"],
        "text" => &["text"],
        "image" => &["image"],
        "audio" => &["audio"],
        "resource" => &["resource"],
        "resource_link" => &["resource_link"],
        _ => unreachable!("literal {expected} has no static slice"),
    }
}

fn quoted_values(values: &[&str], separator: &str) -> String {
    values
        .iter()
        .map(|value| format!("\"{value}\""))
        .collect::<Vec<_>>()
        .join(separator)
}

impl Issue {
    fn to_value(&self) -> JsValue {
        let mut object = JsObject::new();
        let mut put = |key: &str, value: JsValue| {
            object.insert(key.into(), value);
        };
        let path = || {
            JsValue::Array(
                self.path
                    .iter()
                    .map(|segment| match segment {
                        Segment::Key(key) => JsValue::String(key.clone()),
                        Segment::Index(index) => JsValue::Number(*index as f64),
                    })
                    .collect(),
            )
        };
        let text = |value: &str| JsValue::String(value.into());
        match &self.kind {
            IssueKind::InvalidType {
                expected,
                received,
                safeint,
            } => {
                put("expected", text(expected));
                if *safeint {
                    put("format", text("safeint"));
                }
                put("code", text("invalid_type"));
                put("path", path());
                put(
                    "message",
                    text(&format!(
                        "Invalid input: expected {expected}, received {received}"
                    )),
                );
            }
            IssueKind::InvalidValue { values, literal } => {
                put("code", text("invalid_value"));
                put(
                    "values",
                    JsValue::Array(values.iter().map(|v| text(v)).collect()),
                );
                put("path", path());
                let message = if *literal {
                    format!("Invalid input: expected {}", quoted_values(values, "|"))
                } else {
                    format!(
                        "Invalid option: expected one of {}",
                        quoted_values(values, "|")
                    )
                };
                put("message", text(&message));
            }
            IssueKind::UnrecognizedKeys(keys) => {
                put("code", text("unrecognized_keys"));
                put(
                    "keys",
                    JsValue::Array(keys.iter().cloned().map(JsValue::String).collect()),
                );
                put("path", path());
                let names: Vec<String> = keys
                    .iter()
                    .map(|key| String::from_utf16_lossy(key.units()))
                    .collect();
                let names: Vec<&str> = names.iter().map(String::as_str).collect();
                let plural = if keys.len() > 1 { "s" } else { "" };
                put(
                    "message",
                    text(&format!(
                        "Unrecognized key{plural}: {}",
                        quoted_values(&names, ", ")
                    )),
                );
            }
            IssueKind::Union(errors) => {
                put("code", text("invalid_union"));
                put(
                    "errors",
                    JsValue::Array(
                        errors
                            .iter()
                            .map(|issues| {
                                JsValue::Array(issues.iter().map(Issue::to_value).collect())
                            })
                            .collect(),
                    ),
                );
                put("path", path());
                put("message", text("Invalid input"));
            }
            IssueKind::Custom(message) => {
                put("code", text("custom"));
                put("path", path());
                put("message", text(message));
            }
            IssueKind::TooBig(maximum) | IssueKind::TooSmall(maximum) => {
                let big = matches!(self.kind, IssueKind::TooBig(_));
                put("origin", text("number"));
                put("code", text(if big { "too_big" } else { "too_small" }));
                put(
                    if big { "maximum" } else { "minimum" },
                    JsValue::Number(*maximum),
                );
                put("inclusive", JsValue::Bool(true));
                put("path", path());
                let message = format!(
                    "Too {}: expected number to be {}{}",
                    if big { "big" } else { "small" },
                    if big { "<=" } else { ">=" },
                    js_value::number_string(*maximum)
                );
                put("message", text(&message));
            }
            IssueKind::InvalidDateTime => {
                put("origin", text("string"));
                put("code", text("invalid_format"));
                put("format", text("datetime"));
                put("pattern", text(DATETIME_PATTERN));
                put("path", path());
                put("message", text("Invalid ISO datetime"));
            }
        }
        JsValue::Object(object)
    }
}

/// `JSON.stringify(value, null, 2)` for issue trees (bounded by schema depth).
fn pretty(value: &JsValue, indent: usize, out: &mut String) {
    let pad = |out: &mut String, depth: usize| {
        out.push('\n');
        out.extend(std::iter::repeat_n(' ', depth * 2));
    };
    match value {
        JsValue::Array(items) if !items.is_empty() => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                pad(out, indent + 1);
                pretty(item, indent + 1, out);
            }
            pad(out, indent);
            out.push(']');
        }
        JsValue::Object(object) if !object.is_empty() => {
            out.push('{');
            for (index, (key, item)) in object.own_entries().into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                pad(out, indent + 1);
                out.push_str(&json_text(&JsValue::String(key.clone())));
                out.push_str(": ");
                pretty(item, indent + 1, out);
            }
            pad(out, indent);
            out.push('}');
        }
        other => out.push_str(&json_text(other)),
    }
}

fn json_text(value: &JsValue) -> String {
    pilotdeck_protocol::stringify_json(value)
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// A failed zod parse: `ZodError.message`.
fn zod_message(issues: &[Issue]) -> String {
    let mut out = String::new();
    pretty(
        &JsValue::Array(issues.iter().map(Issue::to_value).collect()),
        0,
        &mut out,
    );
    out
}

// --- schemas ---------------------------------------------------------------

fn req(name: &'static str, schema: Schema) -> Field {
    Field {
        name,
        schema,
        presence: Presence::Required,
    }
}
fn opt(name: &'static str, schema: Schema) -> Field {
    Field {
        name,
        schema,
        presence: Presence::Optional,
    }
}
fn string() -> Schema {
    Schema::String(StringCheck::None)
}
fn bool_() -> Schema {
    Schema::Bool
}
fn array(item: Schema) -> Schema {
    Schema::Array(Box::new(item))
}
fn record(item: Schema) -> Schema {
    Schema::Record(Box::new(item))
}
fn unknown_record() -> Schema {
    record(Schema::Unknown)
}
fn request_id() -> Schema {
    Schema::Union(vec![string(), Schema::Int])
}
fn request_meta() -> Schema {
    Schema::Object(
        vec![
            opt("progressToken", request_id()),
            opt(
                "io.modelcontextprotocol/related-task",
                Schema::Object(vec![req("taskId", string())], Rest::Strip),
            ),
        ],
        Rest::Loose,
    )
}
/// `ResultSchema.extend(fields)`: loose, `_meta` first.
fn result_with(fields: Vec<Field>) -> Schema {
    let mut all = vec![opt("_meta", request_meta())];
    all.extend(fields);
    Schema::Object(all, Rest::Loose)
}
fn icon() -> Schema {
    Schema::Object(
        vec![
            req("src", string()),
            opt("mimeType", string()),
            opt("sizes", array(string())),
            opt("theme", Schema::Enum(&["light", "dark"])),
        ],
        Rest::Strip,
    )
}
fn annotations() -> Schema {
    Schema::Object(
        vec![
            opt("audience", array(Schema::Enum(&["user", "assistant"]))),
            opt(
                "priority",
                Schema::Number {
                    min: Some(0.0),
                    max: Some(1.0),
                },
            ),
            opt("lastModified", Schema::String(StringCheck::DateTime)),
        ],
        Rest::Strip,
    )
}
fn base64() -> Schema {
    Schema::String(StringCheck::Base64)
}
fn media_content(kind: &'static str) -> Schema {
    Schema::Object(
        vec![
            req("type", Schema::Literal(kind)),
            req("data", base64()),
            req("mimeType", string()),
            opt("annotations", annotations()),
            opt("_meta", unknown_record()),
        ],
        Rest::Strip,
    )
}
fn resource_contents(last: Field) -> Schema {
    Schema::Object(
        vec![
            req("uri", string()),
            opt("mimeType", string()),
            opt("_meta", unknown_record()),
            last,
        ],
        Rest::Strip,
    )
}
fn content_block() -> Schema {
    let text = Schema::Object(
        vec![
            req("type", Schema::Literal("text")),
            req("text", string()),
            opt("annotations", annotations()),
            opt("_meta", unknown_record()),
        ],
        Rest::Strip,
    );
    let resource_link = Schema::Object(
        vec![
            req("name", string()),
            opt("title", string()),
            opt("icons", array(icon())),
            req("uri", string()),
            opt("description", string()),
            opt("mimeType", string()),
            opt(
                "size",
                Schema::Number {
                    min: None,
                    max: None,
                },
            ),
            opt("annotations", annotations()),
            opt("_meta", Schema::Object(Vec::new(), Rest::Loose)),
            req("type", Schema::Literal("resource_link")),
        ],
        Rest::Strip,
    );
    let embedded = Schema::Object(
        vec![
            req("type", Schema::Literal("resource")),
            req(
                "resource",
                Schema::Union(vec![
                    resource_contents(req("text", string())),
                    resource_contents(req("blob", base64())),
                ]),
            ),
            opt("annotations", annotations()),
            opt("_meta", unknown_record()),
        ],
        Rest::Strip,
    );
    Schema::Union(vec![
        text,
        media_content("image"),
        media_content("audio"),
        resource_link,
        embedded,
    ])
}
fn object_schema() -> Schema {
    // `.catchall(z.unknown())` keeps unknown keys like `looseObject`.
    Schema::Object(
        vec![
            req("type", Schema::Literal("object")),
            opt("properties", record(Schema::AssertObject)),
            opt("required", array(string())),
        ],
        Rest::Loose,
    )
}
fn tool() -> Schema {
    Schema::Object(
        vec![
            req("name", string()),
            opt("title", string()),
            opt("icons", array(icon())),
            opt("description", string()),
            req("inputSchema", object_schema()),
            opt("outputSchema", object_schema()),
            opt(
                "annotations",
                Schema::Object(
                    vec![
                        opt("title", string()),
                        opt("readOnlyHint", bool_()),
                        opt("destructiveHint", bool_()),
                        opt("idempotentHint", bool_()),
                        opt("openWorldHint", bool_()),
                    ],
                    Rest::Strip,
                ),
            ),
            opt(
                "execution",
                Schema::Object(
                    vec![opt(
                        "taskSupport",
                        Schema::Enum(&["required", "optional", "forbidden"]),
                    )],
                    Rest::Strip,
                ),
            ),
            opt("_meta", unknown_record()),
        ],
        Rest::Strip,
    )
}
fn server_capabilities() -> Schema {
    let list_changed = || Schema::Object(vec![opt("listChanged", bool_())], Rest::Strip);
    Schema::Object(
        vec![
            opt("experimental", record(Schema::AssertObject)),
            opt("logging", Schema::AssertObject),
            opt("completions", Schema::AssertObject),
            opt("prompts", list_changed()),
            opt(
                "resources",
                Schema::Object(
                    vec![opt("subscribe", bool_()), opt("listChanged", bool_())],
                    Rest::Strip,
                ),
            ),
            opt("tools", list_changed()),
            opt(
                "tasks",
                Schema::Object(
                    vec![
                        opt("list", Schema::AssertObject),
                        opt("cancel", Schema::AssertObject),
                        opt(
                            "requests",
                            Schema::Object(
                                vec![opt(
                                    "tools",
                                    Schema::Object(
                                        vec![opt("call", Schema::AssertObject)],
                                        Rest::Loose,
                                    ),
                                )],
                                Rest::Loose,
                            ),
                        ),
                    ],
                    Rest::Loose,
                ),
            ),
            opt("extensions", record(Schema::AssertObject)),
        ],
        Rest::Strip,
    )
}
fn implementation() -> Schema {
    Schema::Object(
        vec![
            req("name", string()),
            opt("title", string()),
            opt("icons", array(icon())),
            req("version", string()),
            opt("websiteUrl", string()),
            opt("description", string()),
        ],
        Rest::Strip,
    )
}
fn jsonrpc_message() -> Schema {
    let params = || Schema::Object(vec![opt("_meta", request_meta())], Rest::Loose);
    let version = || req("jsonrpc", Schema::Literal("2.0"));
    Schema::Union(vec![
        Schema::Object(
            vec![
                version(),
                req("id", request_id()),
                req("method", string()),
                opt("params", params()),
            ],
            Rest::Strict,
        ),
        Schema::Object(
            vec![version(), req("method", string()), opt("params", params())],
            Rest::Strict,
        ),
        Schema::Object(
            vec![
                version(),
                req("id", request_id()),
                req("result", result_with(Vec::new())),
            ],
            Rest::Strict,
        ),
        Schema::Object(
            vec![
                version(),
                opt("id", request_id()),
                req(
                    "error",
                    Schema::Object(
                        vec![
                            req("code", Schema::Int),
                            req("message", string()),
                            opt("data", Schema::Unknown),
                        ],
                        Rest::Strip,
                    ),
                ),
            ],
            Rest::Strict,
        ),
    ])
}

static JSONRPC_MESSAGE: LazyLock<Schema> = LazyLock::new(jsonrpc_message);
static INITIALIZE_RESULT: LazyLock<Schema> = LazyLock::new(|| {
    result_with(vec![
        req("protocolVersion", string()),
        req("capabilities", server_capabilities()),
        req("serverInfo", implementation()),
        opt("instructions", string()),
    ])
});
static LIST_TOOLS_RESULT: LazyLock<Schema> = LazyLock::new(|| {
    result_with(vec![
        opt("nextCursor", string()),
        req("tools", array(tool())),
    ])
});
static CALL_TOOL_RESULT: LazyLock<Schema> = LazyLock::new(|| {
    result_with(vec![
        Field {
            name: "content",
            schema: array(content_block()),
            presence: Presence::DefaultEmptyArray,
        },
        opt("structuredContent", unknown_record()),
        opt("isError", bool_()),
    ])
});

/// Result schemas the client requests with.
#[derive(Clone, Copy, Debug)]
pub(crate) enum ResultSchema {
    Initialize,
    ListTools,
    CallTool,
}

/// `safeParse(resultSchema, result)`: the parsed value or `ZodError.message`.
pub(crate) fn parse_result(schema: ResultSchema, value: &JsValue) -> Result<JsValue, String> {
    let schema = match schema {
        ResultSchema::Initialize => &*INITIALIZE_RESULT,
        ResultSchema::ListTools => &*LIST_TOOLS_RESULT,
        ResultSchema::CallTool => &*CALL_TOOL_RESULT,
    };
    let parsed = schema.parse(value);
    if parsed.issues.is_empty() {
        Ok(parsed.value)
    } else {
        Err(zod_message(&parsed.issues))
    }
}

/// An inbound frame after `JSONRPCMessageSchema.parse(JSON.parse(line))`.
#[derive(Debug)]
pub(crate) enum Inbound {
    Request {
        id: JsValue,
        method: JsString,
    },
    Notification,
    Result {
        id: JsValue,
        result: JsValue,
    },
    Error {
        id: Option<JsValue>,
        code: f64,
        message: JsString,
    },
}

/// `deserializeMessage(line)`; `None` when `JSON.parse` or the schema throws
/// (the transport reports it to `onerror` and drops the line).
pub(crate) fn parse_inbound(line: &str) -> Option<Inbound> {
    parse_inbound_value(&parse_json(line).ok()?).ok()
}

/// `JSONRPCMessageSchema.parse(value)`: the message or `ZodError.message`.
pub(crate) fn parse_inbound_value(raw: &JsValue) -> Result<Inbound, String> {
    let parsed = JSONRPC_MESSAGE.parse(raw);
    if !parsed.issues.is_empty() {
        return Err(zod_message(&parsed.issues));
    }
    let message = parsed.value;
    let field = |name: &str| message.get(name).cloned();
    let shape = || "validated JSON-RPC message lost a field".to_owned();
    if let Some(result) = field("result") {
        return Ok(Inbound::Result {
            id: field("id").ok_or_else(shape)?,
            result,
        });
    }
    if let Some(error) = field("error") {
        return Ok(Inbound::Error {
            id: field("id"),
            code: error
                .get("code")
                .and_then(JsValue::as_number)
                .ok_or_else(shape)?,
            message: error
                .get("message")
                .and_then(JsValue::as_string)
                .cloned()
                .ok_or_else(shape)?,
        });
    }
    let method = field("method")
        .and_then(|method| method.as_string().cloned())
        .ok_or_else(shape)?;
    Ok(match field("id") {
        Some(id) => Inbound::Request { id, method },
        None => Inbound::Notification,
    })
}

// ---------------------------------------------------------------------------
// config
// ---------------------------------------------------------------------------

/// `process.env` / home directory for placeholder expansion. The SDK MCP
/// environment port is the same contract, so both share one trait.
pub use super::sdk::SdkMcpEnvironment as McpEnvironment;

/// `expandMcpString`.
pub fn expand_mcp_string(value: &JsString, environment: &dyn McpEnvironment) -> JsString {
    super::sdk::expand_mcp_string(value, environment)
}

/// `expandMcpConfig`: every string in the tree through [`expand_mcp_string`],
/// rebuilt with `Object.fromEntries(Object.entries(...))` (own `__proto__`
/// keys stay own properties). Past the shared depth bound it reports V8's
/// `RangeError` instead of recursing.
pub fn expand_mcp_config(
    value: &JsValue,
    environment: &dyn McpEnvironment,
) -> Result<JsValue, CallStackExceeded> {
    super::naming::map_tree(
        value,
        |key| Some(key.clone()),
        |text| JsValue::String(expand_mcp_string(text, environment)),
    )
}

/// One `{ path, message }` diagnostic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpConfigDiagnostic {
    pub path: String,
    pub message: String,
}

/// `LoadMcpServerConfigResult`.
#[derive(Clone, Debug, PartialEq)]
pub struct LoadMcpServerConfigResult {
    /// `{ ...global.mcpServers, ...project.mcpServers }`.
    pub servers: JsObject,
    pub diagnostics: Vec<McpConfigDiagnostic>,
}
impl LoadMcpServerConfigResult {
    pub fn to_value(&self) -> JsValue {
        let mut object = JsObject::new();
        object.insert("servers".into(), JsValue::Object(self.servers.clone()));
        object.insert(
            "diagnostics".into(),
            JsValue::Array(
                self.diagnostics
                    .iter()
                    .map(|diagnostic| {
                        let mut entry = JsObject::new();
                        entry.insert(
                            "path".into(),
                            JsValue::String(diagnostic.path.as_str().into()),
                        );
                        entry.insert(
                            "message".into(),
                            JsValue::String(diagnostic.message.as_str().into()),
                        );
                        JsValue::Object(entry)
                    })
                    .collect(),
            ),
        );
        JsValue::Object(object)
    }
}

/// `getGlobalMcpConfigFilePath`.
pub fn global_mcp_config_file_path(pilot_home: &Path) -> PathBuf {
    absolute(pilot_home).join(MCP_CONFIG_FILE_NAME)
}

/// `getProjectMcpConfigFilePath`.
pub fn project_mcp_config_file_path(project_root: &Path) -> PathBuf {
    absolute(project_root)
        .join(".pilotdeck")
        .join(MCP_CONFIG_FILE_NAME)
}

/// `path.resolve`: normalize dot segments relative to the process cwd.
fn absolute(path: &Path) -> PathBuf {
    crate::metadata::resolve_process_path(Path::new(""), path)
}

/// `loadMcpServerConfig(projectRoot, pilotHome)`. `Err` is the uncaught
/// `RangeError` of a too-deep `expandMcpConfig`.
pub fn load_mcp_server_config(
    project_root: &Path,
    pilot_home: &Path,
    environment: &dyn McpEnvironment,
) -> Result<LoadMcpServerConfigResult, CallStackExceeded> {
    let mut diagnostics = Vec::new();
    let global = read_mcp_config(
        &global_mcp_config_file_path(pilot_home),
        &mut diagnostics,
        environment,
    )?;
    let project = read_mcp_config(
        &project_mcp_config_file_path(project_root),
        &mut diagnostics,
        environment,
    )?;
    let mut servers = JsObject::new();
    for source in [global, project].into_iter().flatten() {
        for (key, value) in source.own_entries() {
            servers.insert(key.clone(), value.clone());
        }
    }
    Ok(LoadMcpServerConfigResult {
        servers,
        diagnostics,
    })
}

fn read_mcp_config(
    path: &Path,
    diagnostics: &mut Vec<McpConfigDiagnostic>,
    environment: &dyn McpEnvironment,
) -> Result<Option<JsObject>, CallStackExceeded> {
    let display = path.to_string_lossy().into_owned();
    let mut push = |message: String| {
        diagnostics.push(McpConfigDiagnostic {
            path: display.clone(),
            message,
        });
    };
    // `existsSync` follows symlinks and swallows every error.
    if std::fs::metadata(path).is_err() {
        return Ok(None);
    }
    // `readFileSync` reports an open error with its path and a descriptor
    // read error without one (node v22.22.0: reading a directory is
    // `EISDIR: illegal operation on a directory, read`), so keep the two
    // stages apart like the shared `readFile` ports.
    let text = match std::fs::File::open(path) {
        Ok(mut file) => {
            let mut bytes = Vec::new();
            match std::io::Read::read_to_end(&mut file, &mut bytes) {
                Ok(_) => String::from_utf8_lossy(&bytes).into_owned(),
                Err(error) => {
                    push(node_fs_message(&error, "read", path));
                    return Ok(None);
                }
            }
        }
        Err(error) => {
            push(node_fs_message(&error, "open", path));
            return Ok(None);
        }
    };
    if let Some(message) = json_syntax::parse_error(&text) {
        push(message);
        return Ok(None);
    }
    let Ok(parsed) = parse_json(&text) else {
        push("Invalid JSON".into());
        return Ok(None);
    };
    let Some(root) = parsed.as_object() else {
        push("MCP config root must be an object.".into());
        return Ok(None);
    };
    match root.get("mcpServers") {
        None | Some(JsValue::Undefined) => Ok(Some(JsObject::new())),
        Some(servers @ JsValue::Object(_)) => Ok(expand_mcp_config(servers, environment)?
            .as_object()
            .cloned()),
        Some(_) => {
            push("mcpServers must be an object.".into());
            Ok(Some(JsObject::new()))
        }
    }
}

/// Node's `fs` error message: `CODE: description, syscall 'path'`, through the
/// shared `pilotdeck_gateway::node_fs_message`. Callers keep the open and
/// read stages apart (a directory read is `read` without a path, as in
/// `readFileSync`); an errno outside the shared table keeps Node's synthetic
/// `UNKNOWN: unknown error` text (libuv's `UV_UNKNOWN`) under the same stage
/// rule, so a descriptor read/write error still carries no path — the
/// formatter only prints a path the call site provided, and Node's read/
/// write call sites provide none whatever the errno (node v22.22.0:
/// `_tmp/20261025-issue-656/node-probe/write-stages.out` has no `path` at all
/// on the EFBIG write error, and the constructor probe in
/// `_tmp/20261025-issue-656/fix-r2/empty-path-probe.out` shows a provided
/// path is kept for read/write too).
pub(crate) fn node_fs_message(error: &std::io::Error, syscall: &str, path: &Path) -> String {
    crate::node_fs_message_or_unknown(error, syscall, Some(&path.to_string_lossy()))
}

/// libuv's `uv_err_name` / `uv_strerror` from the shared Node v22 table.
/// Errnos outside that table keep Node's synthetic `UNKNOWN: unknown error`
/// text (libuv's `UV_UNKNOWN`), matching Node's `UVException` formatting.
/// The MCP fs formatting goes through `node_fs_message`; the wire layer uses
/// this for spawn diagnostics.
pub(crate) fn errno_text(error: &std::io::Error) -> (&'static str, &'static str) {
    crate::node_io::lookup(error).unwrap_or(("UNKNOWN", "unknown error"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_paths_use_shared_lexical_resolution() {
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(
            global_mcp_config_file_path(Path::new("a/../b/./")),
            cwd.join("b").join(MCP_CONFIG_FILE_NAME)
        );
        assert_eq!(
            project_mcp_config_file_path(Path::new("/a/../../中/./x/..")),
            Path::new("/中/.pilotdeck").join(MCP_CONFIG_FILE_NAME)
        );
        assert_eq!(absolute(Path::new("")), cwd);
        assert_eq!(absolute(Path::new("/../")), Path::new("/"));
    }

    /// The shared libuv table must reproduce every row the old hand-written
    /// table served, byte for byte.
    #[test]
    fn errno_text_keeps_the_old_hand_table_rows_byte_for_byte() {
        for (errno, code, description) in [
            (libc::ENOENT, "ENOENT", "no such file or directory"),
            (libc::EACCES, "EACCES", "permission denied"),
            (libc::EPERM, "EPERM", "operation not permitted"),
            (libc::ENOTDIR, "ENOTDIR", "not a directory"),
            (libc::EISDIR, "EISDIR", "illegal operation on a directory"),
            (libc::EEXIST, "EEXIST", "file already exists"),
            (libc::ELOOP, "ELOOP", "too many symbolic links encountered"),
            (libc::ENAMETOOLONG, "ENAMETOOLONG", "name too long"),
            (libc::EROFS, "EROFS", "read-only file system"),
            (libc::ENOEXEC, "ENOEXEC", "exec format error"),
            (libc::E2BIG, "E2BIG", "argument list too long"),
            (libc::EAGAIN, "EAGAIN", "resource temporarily unavailable"),
            (libc::ENOMEM, "ENOMEM", "not enough memory"),
            (libc::EMFILE, "EMFILE", "too many open files"),
        ] {
            assert_eq!(
                errno_text(&std::io::Error::from_raw_os_error(errno)),
                (code, description),
                "{code}"
            );
        }
    }

    /// EIO/ENOSPC were outside the old hand-written table and collapsed to
    /// `UNKNOWN`; libuv knows both. An errno outside libuv's map still keeps
    /// Node's synthetic `UNKNOWN: unknown error` text.
    #[test]
    fn errno_text_uses_the_shared_libuv_table_outside_the_old_hand_table() {
        for (errno, code, description) in [
            (libc::EIO, "EIO", "i/o error"),
            (libc::ENOSPC, "ENOSPC", "no space left on device"),
        ] {
            assert_eq!(
                errno_text(&std::io::Error::from_raw_os_error(errno)),
                (code, description),
                "{code}"
            );
        }
        assert_eq!(
            errno_text(&std::io::Error::from_raw_os_error(123456)),
            ("UNKNOWN", "unknown error")
        );
        assert_eq!(
            errno_text(&std::io::Error::other("not an errno")),
            ("UNKNOWN", "unknown error")
        );
    }

    /// The shared stage rules through the MCP wrapper: an open error keeps the
    /// path, a descriptor read error does not, and an errno outside the table
    /// keeps Node's synthetic text (node v22.22.0, `_tmp/20261025-issue-651/
    /// node-probe.out`).
    #[test]
    fn node_fs_message_routes_the_shared_stage_rules() {
        assert_eq!(
            node_fs_message(
                &std::io::Error::from_raw_os_error(libc::EACCES),
                "open",
                Path::new("/c/mcp.json")
            ),
            "EACCES: permission denied, open '/c/mcp.json'"
        );
        assert_eq!(
            node_fs_message(
                &std::io::Error::from_raw_os_error(libc::EIO),
                "read",
                Path::new("/c/mcp.json")
            ),
            "EIO: i/o error, read"
        );
        assert_eq!(
            node_fs_message(
                &std::io::Error::from_raw_os_error(libc::EISDIR),
                "open",
                Path::new("/c/mcp.json")
            ),
            "EISDIR: illegal operation on a directory, open '/c/mcp.json'"
        );
        // 123456 stays outside libuv's table on both supported targets
        // (macOS, Linux), so every use below exercises the `UV_UNKNOWN`
        // fallback rather than a table row.
        assert_eq!(
            node_fs_message(
                &std::io::Error::from_raw_os_error(123456),
                "mkdir",
                Path::new("/c")
            ),
            "UNKNOWN: unknown error, mkdir '/c'"
        );
        // An errno outside the shared table returns `None` there, so the
        // `UV_UNKNOWN` fallback fires — and it keeps the stage rule: a
        // descriptor read/write error carries no path there either.
        assert_eq!(
            node_fs_message(
                &std::io::Error::from_raw_os_error(123456),
                "read",
                Path::new("/c/mcp.json")
            ),
            "UNKNOWN: unknown error, read"
        );
        assert_eq!(
            node_fs_message(
                &std::io::Error::from_raw_os_error(123456),
                "write",
                Path::new("/c/mcp.json")
            ),
            "UNKNOWN: unknown error, write"
        );
        // An empty path stays quoted — the C++ binding formatter real fs
        // errors report through appends a known path verbatim, empty
        // included (node v22.22.0 `fs.openSync('')` → `… open ''`,
        // `_tmp/20261025-issue-656/fix-r2/empty-path-probe.out`).
        assert_eq!(
            node_fs_message(
                &std::io::Error::from_raw_os_error(libc::ENOENT),
                "open",
                Path::new("")
            ),
            "ENOENT: no such file or directory, open ''"
        );
        assert_eq!(
            node_fs_message(
                &std::io::Error::from_raw_os_error(123456),
                "open",
                Path::new("")
            ),
            "UNKNOWN: unknown error, open ''"
        );
    }

    /// A directory at the config path is the one reachable read-stage failure
    /// (`existsSync` passes, the open succeeds, the descriptor read fails):
    /// like `readFileSync` it must read `read` without the path, never the
    /// caller-supplied `open` label with it. POSIX-only: opening a directory
    /// read-only succeeds there, unlike Windows.
    #[cfg(unix)]
    #[test]
    fn read_mcp_config_reports_a_directory_at_the_read_stage() {
        struct Env;
        impl McpEnvironment for Env {
            fn env(&self, _name: &JsString) -> Option<JsString> {
                None
            }
            fn home(&self) -> JsString {
                JsString::from("/home/u")
            }
        }
        let dir = std::env::temp_dir().join(format!(
            "pdrs-mcp-eisdir-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&dir).unwrap();
        let mut diagnostics = Vec::new();
        let result = read_mcp_config(&dir, &mut diagnostics, &Env).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(result, None);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(
            diagnostics[0].message,
            "EISDIR: illegal operation on a directory, read"
        );
    }

    /// Real SDK/zod probe values are recorded in the round-4 evidence.
    /// Unknown values keep their own nested keys; only the schema's own
    /// object/record boundary removes the special key.
    #[test]
    fn zod_skips_proto_before_record_validation_and_object_extra_keys() {
        let raw = parse_json(r#"{"tools":[{"name":"x","inputSchema":{"type":"object","properties":{"__proto__":1,"constructor":{},"prototype":{},"q":{"__proto__":1}},"__proto__":1}}]}"#).unwrap();
        let parsed = parse_result(ResultSchema::ListTools, &raw).unwrap();
        assert_eq!(
            pilotdeck_protocol::stringify_json(&parsed)
                .unwrap()
                .unwrap(),
            r#"{"tools":[{"name":"x","inputSchema":{"type":"object","properties":{"constructor":{},"prototype":{},"q":{"__proto__":1}}}}]}"#
        );
        let parsed = JSONRPC_MESSAGE
            .parse(&parse_json(r#"{"jsonrpc":"2.0","id":1,"result":{},"__proto__":1}"#).unwrap());
        assert!(parsed.issues.is_empty());
        assert_eq!(
            pilotdeck_protocol::stringify_json(&parsed.value)
                .unwrap()
                .unwrap(),
            r#"{"jsonrpc":"2.0","id":1,"result":{}}"#
        );
        let parsed = parse_result(ResultSchema::CallTool, &parse_json(
            r#"{"content":[],"structuredContent":{"__proto__":1,"constructor":2,"q":{"__proto__":3}},"__proto__":1,"constructor":2,"prototype":3}"#
        ).unwrap()).unwrap();
        assert_eq!(
            pilotdeck_protocol::stringify_json(&parsed)
                .unwrap()
                .unwrap(),
            r#"{"content":[],"structuredContent":{"constructor":2,"q":{"__proto__":3}},"constructor":2,"prototype":3}"#
        );
        // Near misses must still be validated or rejected as extra keys.
        assert!(parse_result(ResultSchema::ListTools, &parse_json(
            r#"{"tools":[{"name":"x","inputSchema":{"type":"object","properties":{"__proto___":1}}}]}"#
        ).unwrap()).is_err());
        assert!(parse_inbound(r#"{"jsonrpc":"2.0","id":1,"result":{},"__proto___":1}"#).is_none());
    }

    #[test]
    fn atob_acceptance_follows_forgiving_base64() {
        for (input, ok) in [
            ("QQ==", true),
            ("QQ=", false),
            ("QQ", true),
            ("Q", false),
            (" Q Q = = ", true),
            ("QUJD", true),
            ("!!!", false),
            ("QQ==QQ==", false),
            ("", true),
            ("A===", false),
        ] {
            assert_eq!(is_atob_input(&input.into()), ok, "{input:?}");
        }
    }

    #[test]
    fn datetime_accepts_offsets_and_leap_days_only() {
        for (input, ok) in [
            ("2025-01-01T00:00:00Z", true),
            ("2024-02-29T10:00+08:00", true),
            ("2023-02-29T10:00Z", false),
            ("2025-01-01T00:00:00", false),
            ("2025-01-01T00:00:00.123Z", true),
            ("２025-01-01T00:00:00Z", false),
        ] {
            let parsed =
                Schema::String(StringCheck::DateTime).parse(&JsValue::String(input.into()));
            assert_eq!(parsed.issues.is_empty(), ok, "{input:?}");
        }
    }

    #[test]
    fn single_open_union_option_reports_its_own_issue() {
        let message = parse_result(
            ResultSchema::CallTool,
            &parse_json(r#"{"content":[{"type":"image","data":"!","mimeType":"x"}]}"#).unwrap(),
        )
        .unwrap_err();
        assert!(message.contains("\"Invalid Base64 string\""), "{message}");
        assert!(!message.contains("invalid_union"), "{message}");
    }

    #[test]
    fn inbound_classification_mirrors_the_union_order() {
        assert!(matches!(
            parse_inbound(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#),
            Some(Inbound::Result { .. })
        ));
        assert!(matches!(
            parse_inbound(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#),
            Some(Inbound::Request { .. })
        ));
        assert!(matches!(
            parse_inbound(r#"{"jsonrpc":"2.0","method":"x"}"#),
            Some(Inbound::Notification)
        ));
        assert!(matches!(
            parse_inbound(r#"{"jsonrpc":"2.0","error":{"code":1,"message":"m"}}"#),
            Some(Inbound::Error { id: None, .. })
        ));
        assert!(parse_inbound(r#"{"jsonrpc":"2.0","id":1,"result":{},"x":1}"#).is_none());
        assert!(parse_inbound(r#"{"jsonrpc":"2.0","id":1.5,"result":{}}"#).is_none());
        assert!(parse_inbound("not json").is_none());
    }
}
