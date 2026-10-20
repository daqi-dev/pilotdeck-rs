//! `structuredContent` validation against a tool's `outputSchema`, as the
//! SDK's `AjvJsonSchemaValidator` does it (Ajv 8.20, draft-07,
//! `{ strict: false, validateSchema: false, allErrors: true,
//! validateFormats: true }` plus `ajv-formats` defaults). The result is
//! `ajv.errorsText(errors)`: `data<instancePath> <message>` joined with
//! `", "`.
//!
//! Measured against the real provider in
//! `_tmp/20261020-a65-pr10b/probes/sdk-ajv*.out`: `ajv-formats` defaults to
//! its **full** formats here (`addFormats(ajv)` with no `mode`), `nullable`
//! is a first-class Ajv keyword, `$id` fragment anchors resolve, and invalid
//! schemas throw while Ajv compiles them (the SDK does that inside
//! `tools/list`, so [`check_schema`] is called from `McpClient::list_tools`).
//!
//! Ajv evaluates keywords in rule groups — untyped, then number, string,
//! array, object — each in vocabulary order (measured:
//! `_tmp/20261020-a65-pr10b/probes/ajv-rules.out`). A schema's `type` is
//! checked up front unless it names exactly one group type the schema has
//! rules for; then a mismatch is reported in that group's `else` branch.
//!
//! Covered: `type`, `nullable`, `$ref` (local JSON pointers and `$id`
//! fragment anchors), `const`, `enum`, `not`, `anyOf`, `oneOf`, `allOf`,
//! `if`/`then`/`else`, number limits and `multipleOf`, string lengths (code
//! points), `pattern`, every `ajv-formats` full format, array limits,
//! `items`, `additionalItems`, `contains`, `uniqueItems`, object limits,
//! `required`, `propertyNames`, `additionalProperties`, `dependencies`,
//! `properties`, `patternProperties`.
//!
//! Schema compilation follows the SDK's Ajv compile gate, not the port's own
//! validation coverage. Syntax V8 rejects under `/u` is reported with V8's
//! `Invalid regular expression: ...` text before the Rust translator runs.
//! Syntax V8 accepts is accepted here even when this port cannot evaluate it
//! (lookarounds, backreferences, `\p{...}` values outside Rust's tables, ...);
//! that gap is reported later as [`ValidationError::Unsupported`] when the
//! pattern is actually used, never as a silent pass. Relative/remote `$ref`s
//! that need an `$id` base are reported as unresolvable; validation nesting
//! deeper than [`MAX_DEPTH`] is reported as [`ValidationError::Threw`] with an
//! explicit message. User patterns use the SDK's JavaScript `/u` character
//! classes: `\d`/`\w` and word boundaries are ASCII, whitespace includes BOM
//! but excludes NEL, and dot excludes all four JavaScript line terminators.
//! The JS escapes Rust spells differently (`\0`, `\cX`, `\xHH`, `\uXXXX`,
//! `\u{...}`, `\f`/`\n`/`\r`/`\t`/`\v`, class-local `\-`) are translated
//! instead of being refused; named groups without backreferences are
//! translated to ordinary capturing groups.

mod unicode;

#[cfg(test)]
use crate::test_baseline as baseline;

use crate::js_value;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

/// Validation recursion bound. V8/Ajv handles recursive data at depth 4000
/// (`probes/sdk-ajv3.out`); the recursive `$ref` in that shape costs two
/// `check` frames per data level, so the bound allows roughly 5000 data
/// levels. [`validate`] runs on a dedicated 64 MiB stack. Exceeding the bound
/// is an explicit error, never a silent pass.
const MAX_DEPTH: usize = 10_000;

/// Schema-structure recursion bound for [`check_schema`], which runs on the
/// caller's (2 MiB) worker stack. Ajv itself throws `RangeError` on a
/// 600-level nested schema (`probes/sdk-ajv2.out`), so an explicit error here
/// is the same shape of failure.
const MAX_SCHEMA_DEPTH: usize = 256;

/// V8 12.4 (Node 22.22.0) accepts 32767 captures and rejects the next one
/// with "Too many captures", with and without `/u`. This is a capture-count
/// limit, not a nesting limit: noncapturing groups can nest much deeper.
const MAX_REGEXP_CAPTURES: usize = 32_767;

/// The validator threw instead of returning errors, or hit a schema feature
/// this port cannot evaluate exactly.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ValidationError {
    /// Ajv/V8 `RangeError: Maximum call stack size exceeded`.
    Threw,
    /// A schema feature this port refuses to guess about.
    Unsupported(String),
}

/// The schema could not be compiled; the message mirrors Ajv's throw text for
/// the checks this port implements. Valid `/u` syntax this port cannot
/// evaluate is not a compile error (Ajv accepts it); it fails explicitly at
/// validation time instead.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct SchemaError(pub String);

/// `None` when valid, otherwise `ajv.errorsText(errors)`.
pub(crate) fn validate(
    schema: &JsValue,
    data: &JsValue,
) -> Result<Option<String>, ValidationError> {
    // Deeply nested recursive data is valid in Ajv (depth 4000 measured).
    // Tokio's worker stacks are 2 MiB, so run the recursive walk on a larger
    // dedicated stack instead of lowering the schema port's depth.
    std::thread::scope(|scope| {
        let handle = std::thread::Builder::new()
            .stack_size(64 * 1024 * 1024)
            .spawn_scoped(scope, || validate_inner(schema, data))
            .expect("spawn output-schema validator");
        match handle.join() {
            Ok(result) => result,
            Err(payload) => std::panic::resume_unwind(payload),
        }
    })
}

fn validate_inner(schema: &JsValue, data: &JsValue) -> Result<Option<String>, ValidationError> {
    check_schema(schema).map_err(|SchemaError(message)| ValidationError::Unsupported(message))?;
    let refs = SchemaRefs::new(schema);
    let mut validator = Validator {
        refs,
        errors: Vec::new(),
        depth: 0,
        seen: HashSet::new(),
    };
    validator.check(schema, data, "")?;
    if validator.errors.is_empty() {
        return Ok(None);
    }
    Ok(Some(
        validator
            .errors
            .iter()
            .map(|(path, message)| format!("data{path} {message}"))
            .collect::<Vec<_>>()
            .join(", "),
    ))
}

// ---------------------------------------------------------------------------
// Schema compile checks (`AjvJsonSchemaValidator.getValidator` throws)
// ---------------------------------------------------------------------------

/// Rejects the invalid schemas Ajv would throw on while compiling. The SDK
/// compiles every `outputSchema` during `tools/list`, so the client calls this
/// there; [`validate`] calls it again defensively.
pub(crate) fn check_schema(schema: &JsValue) -> Result<(), SchemaError> {
    let mut checker = SchemaChecker {
        refs: SchemaRefs::new(schema),
        visited: HashSet::new(),
        depth: 0,
    };
    checker.check(schema)
}

struct SchemaRefs<'a> {
    root: &'a JsValue,
    anchors: HashMap<String, Vec<&'a JsValue>>,
}

impl<'a> SchemaRefs<'a> {
    fn new(root: &'a JsValue) -> Self {
        let mut anchors = HashMap::new();
        collect_anchors(root, &mut anchors);
        Self { root, anchors }
    }

    fn resolve(&self, reference: &str) -> Result<&'a JsValue, SchemaError> {
        let Some(fragment) = reference.strip_prefix('#') else {
            return Err(SchemaError(format!(
                "can't resolve reference {reference} from id #"
            )));
        };
        if fragment.is_empty() {
            return Ok(self.root);
        }
        if !fragment.starts_with('/') {
            return match self.anchors.get(fragment) {
                Some(nodes) if nodes.len() == 1 => Ok(nodes[0]),
                Some(_) => Err(SchemaError(format!(
                    "reference \"#{fragment}\" resolves to more than one schema"
                ))),
                None => Err(SchemaError(format!(
                    "can't resolve reference {reference} from id #"
                ))),
            };
        }
        let mut node = self.root;
        for raw in fragment.split('/').skip(1) {
            let segment = raw.replace("~1", "/").replace("~0", "~");
            node = match node {
                JsValue::Object(object) => object.get(&segment).ok_or_else(|| {
                    SchemaError(format!("can't resolve reference {reference} from id #"))
                })?,
                JsValue::Array(items) => items
                    .get(segment.parse::<usize>().ok().ok_or_else(|| {
                        SchemaError(format!("can't resolve reference {reference} from id #"))
                    })?)
                    .ok_or_else(|| {
                        SchemaError(format!("can't resolve reference {reference} from id #"))
                    })?,
                _ => {
                    return Err(SchemaError(format!(
                        "can't resolve reference {reference} from id #"
                    )));
                }
            };
        }
        Ok(node)
    }
}

/// Every `$id` fragment anchor in the document. Ajv registers these while
/// parsing the schema; only anchors actually referenced are compiled.
fn collect_anchors<'a>(schema: &'a JsValue, anchors: &mut HashMap<String, Vec<&'a JsValue>>) {
    // This pass precedes SchemaChecker's depth guard, so it must not consume
    // one native stack frame per untrusted subschema.
    let mut pending = vec![schema];
    while let Some(schema) = pending.pop() {
        let JsValue::Object(object) = schema else {
            continue;
        };
        if let Some(JsValue::String(id)) = object.get("$id")
            && let Ok(id) = id.to_utf8()
            && let Some(fragment) = id.strip_prefix('#')
            && !fragment.is_empty()
        {
            anchors.entry(fragment.to_owned()).or_default().push(schema);
        }
        pending.extend(schema_children(object).into_iter().rev());
    }
}

/// Subschema positions Ajv walks. `const`/`enum`/`default`/`examples` hold
/// data, not schemas, so an `$id` inside them is not an anchor.
fn schema_children(object: &JsObject) -> Vec<&JsValue> {
    let mut out = Vec::new();
    for keyword in [
        "not",
        "if",
        "then",
        "else",
        "contains",
        "additionalProperties",
        "additionalItems",
        "propertyNames",
    ] {
        if let Some(value) = object.get(keyword)
            && is_schema(value)
        {
            out.push(value);
        }
    }
    for keyword in [
        "properties",
        "patternProperties",
        "definitions",
        "$defs",
        "dependentSchemas",
    ] {
        if let Some(JsValue::Object(map)) = object.get(keyword) {
            out.extend(
                map.entries()
                    .iter()
                    .map(|(_, value)| value)
                    .filter(|v| is_schema(v)),
            );
        }
    }
    if let Some(JsValue::Object(map)) = object.get("dependencies") {
        out.extend(
            map.entries()
                .iter()
                .map(|(_, value)| value)
                .filter(|v| is_schema(v)),
        );
    }
    for keyword in ["allOf", "anyOf", "oneOf"] {
        if let Some(JsValue::Array(items)) = object.get(keyword) {
            out.extend(items.iter().filter(|v| is_schema(v)));
        }
    }
    match object.get("items") {
        Some(JsValue::Array(items)) => out.extend(items.iter().filter(|v| is_schema(v))),
        Some(value) if is_schema(value) => out.push(value),
        _ => {}
    }
    out
}

fn is_schema(value: &JsValue) -> bool {
    matches!(value, JsValue::Object(_) | JsValue::Bool(_))
}

/// `it.self.RULES.all` from the SDK's Ajv 8.20 instance (measured with
/// `probes/ajv-always-valid-r4.out`). Ajv's `alwaysValidSchema` only looks at
/// these keys; metadata such as `title`, `$id`, `definitions`, `$defs` and
/// unknown keywords does not stop the `patternProperties` codegen early-exit.
const AJV_RULE_KEYWORDS: &[&str] = &[
    "$comment",
    "$ref",
    "additionalItems",
    "additionalProperties",
    "allOf",
    "anyOf",
    "const",
    "contains",
    "dependencies",
    "else",
    "enum",
    "exclusiveMaximum",
    "exclusiveMinimum",
    "format",
    "id",
    "if",
    "items",
    "maxItems",
    "maxLength",
    "maxProperties",
    "maximum",
    "minItems",
    "minLength",
    "minProperties",
    "minimum",
    "multipleOf",
    "not",
    "nullable",
    "oneOf",
    "pattern",
    "patternProperties",
    "properties",
    "propertyNames",
    "required",
    "then",
    "type",
    "uniqueItems",
];

/// `alwaysValidSchema(it, schema)` with the SDK's `strict: false` options.
/// Booleans and objects without any Ajv rule keyword are always valid.
fn always_valid_schema(schema: &JsValue) -> bool {
    match schema {
        JsValue::Bool(valid) => *valid,
        JsValue::Object(object) => object.entries().iter().all(|(key, _)| {
            let key = String::from_utf16_lossy(key.units());
            !AJV_RULE_KEYWORDS.contains(&key.as_str())
        }),
        // `Object.keys(5)` is `[]` in JavaScript, so Ajv treats every
        // non-boolean non-object as always valid here.
        _ => true,
    }
}

/// Whether Ajv's `patternProperties` keyword runs its codegen (and therefore
/// compiles every key with `new RegExp(key, "u")`). The keyword early-returns
/// when all children are always valid and no sibling keyword compiles the
/// patterns again. `additionalProperties` reuses the patterns to decide what
/// is "additional", but it early-returns too when its own schema is always
/// valid (`true` / `{}`).
fn pattern_properties_codegen(object: &JsObject) -> (bool, bool) {
    let additional_properties = object
        .get("additionalProperties")
        .is_some_and(|schema| !always_valid_schema(schema));
    let Some(JsValue::Object(patterns)) = object.get("patternProperties") else {
        return (false, additional_properties);
    };
    let entries = patterns.entries();
    if entries.is_empty() {
        return (false, additional_properties);
    }
    let all_always_valid = entries.iter().all(|(_, child)| always_valid_schema(child));
    (!all_always_valid, additional_properties)
}

struct SchemaChecker<'a> {
    refs: SchemaRefs<'a>,
    visited: HashSet<usize>,
    depth: usize,
}

impl<'a> SchemaChecker<'a> {
    fn check(&mut self, schema: &JsValue) -> Result<(), SchemaError> {
        let JsValue::Object(object) = schema else {
            return match schema {
                JsValue::Bool(_) => Ok(()),
                _ => Err(SchemaError("schema must be object or boolean".into())),
            };
        };
        // Recursive `$ref` graphs are legal; check each node once.
        if !self.visited.insert(schema as *const JsValue as usize) {
            return Ok(());
        }
        if self.depth >= MAX_SCHEMA_DEPTH {
            return Err(SchemaError(format!(
                "schema nesting is deeper than this port supports ({MAX_SCHEMA_DEPTH})"
            )));
        }
        self.depth += 1;
        let result = self.check_object(object);
        self.depth -= 1;
        result
    }

    fn check_object(&mut self, object: &JsObject) -> Result<(), SchemaError> {
        let declared = match object.get("type") {
            Some(value) => json_types(value)?,
            None => Vec::new(),
        };
        let mut effective = declared.clone();
        if let Some(nullable) = object.get("nullable") {
            let JsValue::Bool(flag) = nullable else {
                return Err(SchemaError("nullable value must be [\"boolean\"]".into()));
            };
            if effective.iter().any(|name| name == "null") {
                if !flag {
                    return Err(SchemaError("type: null contradicts nullable: false".into()));
                }
            } else if effective.is_empty() {
                return Err(SchemaError(
                    "\"nullable\" cannot be used without \"type\"".into(),
                ));
            } else if *flag {
                effective.push("null".into());
            }
        }

        if let Some(reference) = object.get("$ref") {
            let JsValue::String(reference) = reference else {
                return Err(SchemaError("$ref value must be [\"string\"]".into()));
            };
            let reference = utf8(reference, "$ref")?;
            let target = self.refs.resolve(&reference)?;
            self.check(target)?;
        }

        if let Some(JsValue::String(id)) = object.get("$id") {
            utf8(id, "$id")?;
        } else if object.get("$id").is_some() {
            // Ajv 8.20 calls `id.replace(...)` without a type check first.
            return Err(SchemaError("id.replace is not a function".into()));
        }

        if let Some(enumeration) = object.get("enum") {
            let JsValue::Array(values) = enumeration else {
                return Err(SchemaError("enum value must be [\"array\"]".into()));
            };
            if values.is_empty() {
                return Err(SchemaError("enum must have non-empty array".into()));
            }
        }

        for keyword in [
            "maximum",
            "minimum",
            "exclusiveMaximum",
            "exclusiveMinimum",
            "multipleOf",
            "maxLength",
            "minLength",
            "maxItems",
            "minItems",
            "maxProperties",
            "minProperties",
        ] {
            if let Some(value) = object.get(keyword)
                && !matches!(value, JsValue::Number(_))
            {
                return Err(SchemaError(format!("{keyword} value must be [\"number\"]")));
            }
        }

        if let Some(pattern) = object.get("pattern") {
            let JsValue::String(pattern) = pattern else {
                return Err(SchemaError("pattern value must be [\"string\"]".into()));
            };
            let pattern = utf8(pattern, "pattern")?;
            // Only V8-invalid syntax is a compile error. Valid `/u` syntax
            // this port cannot evaluate is accepted here and reported by
            // `regex_matches` if the pattern is ever used.
            if let JsRegex::Invalid(detail) = js_regex(&pattern) {
                return Err(SchemaError(format!(
                    "Invalid regular expression: /{pattern}/u: {detail}"
                )));
            }
        }
        if let Some(format) = object.get("format")
            && !matches!(format, JsValue::String(_))
        {
            return Err(SchemaError("format value must be [\"string\"]".into()));
        }
        if let Some(unique) = object.get("uniqueItems")
            && !matches!(unique, JsValue::Bool(_))
        {
            return Err(SchemaError(
                "uniqueItems value must be [\"boolean\"]".into(),
            ));
        }
        if let Some(required) = object.get("required")
            && !matches!(required, JsValue::Array(_))
        {
            return Err(SchemaError("required value must be [\"array\"]".into()));
        }

        if let Some(properties) = object.get("properties") {
            let JsValue::Object(properties) = properties else {
                return Err(SchemaError("properties value must be [\"object\"]".into()));
            };
            for (_, child) in properties.entries() {
                if !is_schema(child) {
                    return Err(SchemaError(
                        "properties value must be [\"object\",\"boolean\"]".into(),
                    ));
                }
                self.check(child)?;
            }
        }
        if let Some(patterns) = object.get("patternProperties") {
            let JsValue::Object(patterns) = patterns else {
                return Err(SchemaError(
                    "patternProperties value must be [\"object\"]".into(),
                ));
            };
            // Ajv 8.20 `patternProperties.js:14-20` returns before any
            // `usePattern` when every child is always valid (and
            // `unevaluated` is off, as the SDK's draft-07 instance has it).
            // `additionalProperties` can compile the same keys again for its
            // `isAdditional` test unless its own schema is always valid.
            let (pattern_properties, additional_properties) = pattern_properties_codegen(object);
            for (pattern, child) in patterns.entries() {
                if pattern_properties || additional_properties {
                    let pattern = utf8(pattern, "patternProperties")?;
                    if let JsRegex::Invalid(detail) = js_regex(&pattern) {
                        return Err(SchemaError(format!(
                            "Invalid regular expression: /{pattern}/u: {detail}"
                        )));
                    }
                }
                if !is_schema(child) {
                    return Err(SchemaError(
                        "patternProperties value must be [\"object\",\"boolean\"]".into(),
                    ));
                }
                self.check(child)?;
            }
        }
        if let Some(additional) = object.get("additionalProperties") {
            match additional {
                JsValue::Bool(_) => {}
                JsValue::Object(_) => self.check(additional)?,
                _ => {
                    return Err(SchemaError(
                        "additionalProperties value must be [\"boolean\",\"object\"]".into(),
                    ));
                }
            }
        }
        if let Some(items) = object.get("items") {
            match items {
                JsValue::Array(schemas) => {
                    for child in schemas {
                        if !is_schema(child) {
                            return Err(SchemaError(
                                "items value must be [\"object\",\"array\",\"boolean\"]".into(),
                            ));
                        }
                        self.check(child)?;
                    }
                }
                JsValue::Bool(_) | JsValue::Object(_) => self.check(items)?,
                _ => {
                    return Err(SchemaError(
                        "items value must be [\"object\",\"array\",\"boolean\"]".into(),
                    ));
                }
            }
        }
        if let Some(additional) = object.get("additionalItems") {
            match additional {
                JsValue::Bool(_) | JsValue::Object(_) => self.check(additional)?,
                _ => {
                    return Err(SchemaError(
                        "additionalItems value must be [\"object\",\"boolean\"]".into(),
                    ));
                }
            }
        }
        if let Some(dependencies) = object.get("dependencies") {
            let JsValue::Object(dependencies) = dependencies else {
                return Err(SchemaError(
                    "dependencies value must be [\"object\"]".into(),
                ));
            };
            for (_, child) in dependencies.entries() {
                if is_schema(child) {
                    self.check(child)?;
                }
            }
        }
        for keyword in ["not", "contains", "if", "then", "else", "propertyNames"] {
            if let Some(child) = object.get(keyword) {
                match child {
                    JsValue::Bool(_) | JsValue::Object(_) => self.check(child)?,
                    _ => {
                        return Err(SchemaError(format!(
                            "{keyword} value must be [\"object\",\"boolean\"]"
                        )));
                    }
                }
            }
        }
        for keyword in ["allOf", "anyOf", "oneOf"] {
            if let Some(options) = object.get(keyword) {
                let JsValue::Array(options) = options else {
                    return Err(SchemaError(format!("{keyword} value must be [\"array\"]")));
                };
                for option in options {
                    if is_schema(option) {
                        self.check(option)?;
                    } else {
                        return Err(SchemaError(format!("{keyword} value must be [\"array\"]")));
                    }
                }
            }
        }
        for keyword in ["definitions", "$defs"] {
            if let Some(map) = object.get(keyword)
                && !matches!(map, JsValue::Object(_))
            {
                return Err(SchemaError(format!("{keyword} value must be [\"object\"]")));
            }
        }

        Ok(())
    }
}

fn json_types(value: &JsValue) -> Result<Vec<String>, SchemaError> {
    // Ajv's `getJSONTypes` treats falsy `type` values as "no type", then its
    // keyword schemaType check rejects null/false/0 with a different message.
    match value {
        JsValue::Undefined => return Ok(Vec::new()),
        JsValue::Null | JsValue::Bool(false) => {
            return Err(SchemaError(
                "type value must be [\"string\",\"array\"]".into(),
            ));
        }
        JsValue::Number(number) if *number == 0.0 => {
            return Err(SchemaError(
                "type value must be [\"string\",\"array\"]".into(),
            ));
        }
        JsValue::String(name) if name.units().is_empty() => return Ok(Vec::new()),
        _ => {}
    }
    let values: Vec<&JsValue> = match value {
        JsValue::String(_) => vec![value],
        JsValue::Array(items) => items.iter().collect(),
        other => vec![other],
    };
    let rendered = values
        .iter()
        .map(|v| js_text(v))
        .collect::<Vec<_>>()
        .join(",");
    let mut types = Vec::new();
    for value in values {
        let JsValue::String(name) = value else {
            return Err(SchemaError(format!(
                "type must be JSONType or JSONType[]: {rendered}"
            )));
        };
        let name = String::from_utf16_lossy(name.units());
        if !matches!(
            name.as_str(),
            "null" | "boolean" | "object" | "array" | "number" | "string" | "integer"
        ) {
            return Err(SchemaError(format!(
                "type must be JSONType or JSONType[]: {rendered}"
            )));
        }
        types.push(name);
    }
    Ok(types)
}

fn js_text(value: &JsValue) -> String {
    js_value::display(value).unwrap_or_default()
}

fn utf8(value: &JsString, keyword: &str) -> Result<String, SchemaError> {
    value.to_utf8().map_err(|_| {
        SchemaError(format!(
            "unsupported {keyword}: string contains an unpaired surrogate"
        ))
    })
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

struct Validator<'a> {
    refs: SchemaRefs<'a>,
    errors: Vec<(String, String)>,
    depth: usize,
    /// `(schema, data)` pairs on the current recursion path. Re-entering one
    /// is Ajv's `Maximum call stack size exceeded` without consuming data
    /// (the `{$ref:"#"}` probe), so report it immediately instead of walking
    /// to the depth bound.
    seen: HashSet<(usize, usize)>,
}

const NUMBER_RULES: [&str; 6] = [
    "maximum",
    "minimum",
    "exclusiveMaximum",
    "exclusiveMinimum",
    "multipleOf",
    "format",
];
const STRING_RULES: [&str; 4] = ["maxLength", "minLength", "pattern", "format"];
const ARRAY_RULES: [&str; 6] = [
    "maxItems",
    "minItems",
    "additionalItems",
    "items",
    "contains",
    "uniqueItems",
];
const OBJECT_RULES: [&str; 8] = [
    "maxProperties",
    "minProperties",
    "required",
    "propertyNames",
    "additionalProperties",
    "dependencies",
    "properties",
    "patternProperties",
];

fn pointer_segment(key: &JsString) -> String {
    String::from_utf16_lossy(key.units())
        .replace('~', "~0")
        .replace('/', "~1")
}

fn data_type_matches(name: &str, data: &JsValue) -> bool {
    match name {
        "number" => matches!(data, JsValue::Number(_)),
        "integer" => matches!(data, JsValue::Number(n) if n.is_finite() && n.fract() == 0.0),
        "string" => matches!(data, JsValue::String(_)),
        "boolean" => matches!(data, JsValue::Bool(_)),
        "null" => matches!(data, JsValue::Null),
        "array" => matches!(data, JsValue::Array(_)),
        "object" => matches!(data, JsValue::Object(_)),
        _ => false,
    }
}

/// fast-deep-equal over JSON values (object key order is irrelevant).
fn deep_equal(left: &JsValue, right: &JsValue) -> bool {
    let mut pending = vec![(left, right)];
    while let Some((a, b)) = pending.pop() {
        match (a, b) {
            (JsValue::Array(x), JsValue::Array(y)) => {
                if x.len() != y.len() {
                    return false;
                }
                pending.extend(x.iter().zip(y));
            }
            (JsValue::Object(x), JsValue::Object(y)) => {
                if x.len() != y.len() {
                    return false;
                }
                for (key, value) in x.entries() {
                    match y.get_key(key) {
                        Some(other) => pending.push((value, other)),
                        None => return false,
                    }
                }
            }
            (JsValue::Number(x), JsValue::Number(y)) => {
                if x != y && !(x.is_nan() && y.is_nan()) {
                    return false;
                }
            }
            (x, y) => {
                if x != y {
                    return false;
                }
            }
        }
    }
    true
}

fn number(value: Option<&JsValue>) -> Option<f64> {
    value.and_then(JsValue::as_number)
}

fn code_points(text: &JsString) -> usize {
    char::decode_utf16(text.units().iter().copied()).count()
}

enum JsRegex {
    Compiled(Regex),
    /// Syntax this port cannot safely translate; refused as schema patterns.
    JsOnly,
    /// Rejected by JavaScript's `RegExp` too, with V8's reason.
    Invalid(&'static str),
}

fn js_regex(pattern: &str) -> JsRegex {
    // V8 decides validity first. Only a pattern that ECMAScript `/u` accepts
    // is allowed to become `JsOnly`: a Rust compile failure must never turn a
    // JavaScript-invalid pattern into a permissive "unsupported" path, and a
    // Rust success must never turn a JavaScript-invalid pattern into a
    // compiled validator.
    let parsed = match parse_js_pattern(pattern) {
        Ok(parsed) => parsed,
        Err(error) => return JsRegex::Invalid(error.0),
    };
    if parsed.unsupported || has_ambiguous_class_syntax(pattern) {
        return JsRegex::JsOnly;
    }
    match Regex::new(&translate_js_regex(pattern)) {
        Ok(regex) => JsRegex::Compiled(regex),
        // The pattern is valid ECMAScript; Rust's engine just cannot express
        // this one. Refuse it explicitly instead of claiming it is invalid.
        Err(_) => JsRegex::JsOnly,
    }
}

// Rust's nested classes/set operations are not JS `/u` character classes.
// Refuse the few combinations we do not translate (a raw `[` inside a class,
// `--`, `&&`, `~~`) rather than silently narrowing a `patternProperties`
// match. Shorthand-plus-hyphen forms such as `[\w-]` or `[a-z\s]` are
// translated below, matching V8's `/u` reading of the hyphen as a literal.
fn has_ambiguous_class_syntax(pattern: &str) -> bool {
    let mut chars = pattern.chars().peekable();
    let mut in_class = false;
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => {
                chars.next();
            }
            '[' if in_class => return true,
            '[' => {
                in_class = true;
                if matches!(chars.peek(), Some(']'))
                    || (chars.peek() == Some(&'^') && chars.clone().nth(1) == Some(']'))
                {
                    return true;
                }
            }
            ']' => in_class = false,
            '-' if in_class && chars.peek() == Some(&'-') => return true,
            '&' | '~' if in_class && chars.peek() == Some(&ch) => return true,
            _ => {}
        }
    }
    false
}

/// Translate the character atoms whose meanings differ between JS `/u`
/// and Rust's Unicode regex mode, plus the JS escapes Rust spells
/// differently. Escapes are consumed together: `\\w` matches a literal
/// backslash followed by `w`, not a word character. Inside a class, dot is
/// literal and `\b` is backspace rather than a boundary. Shorthands become
/// bare ranges inside a class (Rust nested classes with a following `-` can
/// turn a literal hyphen into a range) and `\p{...}`/`\u` are kept whole.
fn translate_js_regex(pattern: &str) -> String {
    const SPACE: &str = r"\x09-\x0d\x20\xa0\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}";
    let chars: Vec<char> = pattern.chars().collect();
    let mut output = String::with_capacity(pattern.len());
    let mut index = 0;
    let mut in_class = false;
    // The previous class atom was a multi-character class (`\d`, `\s`, ...),
    // so a following `-` is a literal, not a range operator.
    let mut class_escape = false;
    while index < chars.len() {
        let ch = chars[index];
        if ch == '\\' {
            let Some(&escaped) = chars.get(index + 1) else {
                output.push('\\');
                index += 1;
                continue;
            };
            let bracket = |ranges: &str| {
                if in_class {
                    ranges.to_string()
                } else {
                    format!("[{ranges}]")
                }
            };
            match escaped {
                'w' => {
                    output.push_str(&bracket("A-Za-z0-9_"));
                    class_escape = in_class;
                    index += 2;
                }
                'W' => {
                    // A negated shorthand stays a nested class inside a
                    // class; expanding it to a bare `^...` would invert the
                    // whole surrounding class.
                    output.push_str("[^A-Za-z0-9_]");
                    class_escape = in_class;
                    index += 2;
                }
                'd' => {
                    output.push_str(&bracket("0-9"));
                    class_escape = in_class;
                    index += 2;
                }
                'D' => {
                    output.push_str("[^0-9]");
                    class_escape = in_class;
                    index += 2;
                }
                's' => {
                    output.push_str(&bracket(SPACE));
                    class_escape = in_class;
                    index += 2;
                }
                'S' => {
                    output.push_str(&format!("[^{SPACE}]"));
                    class_escape = in_class;
                    index += 2;
                }
                'b' if in_class => {
                    output.push_str(r"\x08");
                    class_escape = false;
                    index += 2;
                }
                'b' => {
                    output.push_str(r"(?-u:\b)");
                    class_escape = false;
                    index += 2;
                }
                'B' if !in_class => {
                    output.push_str(r"(?-u:\B)");
                    class_escape = false;
                    index += 2;
                }
                '0' => {
                    output.push_str(r"\x00");
                    class_escape = false;
                    index += 2;
                }
                'c' => match chars.get(index + 2) {
                    Some(letter) if letter.is_ascii_alphabetic() => {
                        let code = u32::from(*letter) % 32;
                        output.push_str(&format!(r"\x{{{code:02X}}}"));
                        class_escape = false;
                        index += 3;
                    }
                    _ => {
                        output.push_str(r"\c");
                        class_escape = false;
                        index += 2;
                    }
                },
                'f' => {
                    output.push_str(r"\x{0C}");
                    class_escape = false;
                    index += 2;
                }
                'n' => {
                    output.push_str(r"\x{0A}");
                    class_escape = false;
                    index += 2;
                }
                'r' => {
                    output.push_str(r"\x{0D}");
                    class_escape = false;
                    index += 2;
                }
                't' => {
                    output.push_str(r"\x{09}");
                    class_escape = false;
                    index += 2;
                }
                'v' => {
                    output.push_str(r"\x{0B}");
                    class_escape = false;
                    index += 2;
                }
                'x' => {
                    let digits: String = chars[index + 2..(index + 4).min(chars.len())]
                        .iter()
                        .collect();
                    output.push_str(r"\x{");
                    output.push_str(&digits);
                    output.push('}');
                    class_escape = false;
                    index += 2 + digits.chars().count();
                }
                'u' => {
                    if let Some((mut code_point, mut next)) =
                        read_js_unicode_escape(&chars, index + 2)
                    {
                        // V8 combines a short `\uD800` + `\uDC00` pair into
                        // one code point under `/u`; brace-form escapes are
                        // not combined.
                        if (0xD800..=0xDBFF).contains(&code_point)
                            && chars.get(next) == Some(&'\\')
                            && chars.get(next + 1) == Some(&'u')
                            && chars.get(next + 2) != Some(&'{')
                            && let Some((low, after)) = read_js_unicode_escape(&chars, next + 2)
                            && (0xDC00..=0xDFFF).contains(&low)
                        {
                            code_point = 0x10000 + ((code_point - 0xD800) << 10) + (low - 0xDC00);
                            next = after;
                        }
                        output.push_str(&format!(r"\x{{{code_point:X}}}"));
                        class_escape = false;
                        index = next;
                    } else {
                        output.push_str(r"\u");
                        class_escape = false;
                        index += 2;
                    }
                }
                'p' | 'P' => {
                    let start = index;
                    index += 2;
                    if chars.get(index) == Some(&'{') {
                        while index < chars.len() {
                            index += 1;
                            if chars[index - 1] == '}' {
                                break;
                            }
                        }
                    }
                    output.extend(chars[start..index.min(chars.len())].iter());
                    class_escape = in_class;
                }
                '-' if in_class => {
                    output.push_str(r"\x{2D}");
                    class_escape = false;
                    index += 2;
                }
                '/' => {
                    output.push('/');
                    class_escape = false;
                    index += 2;
                }
                other => {
                    output.push('\\');
                    output.push(other);
                    class_escape = false;
                    index += 2;
                }
            }
            continue;
        }
        match ch {
            '(' if chars.get(index + 1) == Some(&'?')
                && chars.get(index + 2) == Some(&'<')
                && !matches!(chars.get(index + 3), Some('=' | '!'))
                && !in_class =>
            {
                // The syntax checker has validated the name and refuses
                // backreferences. Matching does not observe capture names,
                // so this also supports JS names Rust rejects (e.g. `$`).
                while chars[index] != '>' {
                    index += 1;
                }
                output.push('(');
            }
            '[' => {
                // A raw `[` inside a class is a literal in `/u`; it is
                // flagged as unsupported before this translator runs.
                if in_class {
                    output.push_str(r"\x{5B}");
                } else {
                    in_class = true;
                    output.push('[');
                }
                class_escape = false;
            }
            ']' => {
                in_class = false;
                output.push(']');
                class_escape = false;
            }
            '-' if in_class => {
                let previous = index.checked_sub(1).map(|previous| chars[previous]);
                let at_start = previous == Some('[')
                    || (previous == Some('^') && index >= 2 && chars[index - 2] == '[');
                let at_end = chars.get(index + 1) == Some(&']');
                let next_is_class = chars.get(index + 1) == Some(&'\\')
                    && chars.get(index + 2).is_some_and(|next| {
                        matches!(next, 'd' | 'D' | 's' | 'S' | 'w' | 'W' | 'p' | 'P')
                    });
                if at_start || at_end || class_escape || next_is_class {
                    output.push_str(r"\x{2D}");
                } else {
                    output.push('-');
                }
                class_escape = false;
            }
            '&' | '~' if in_class => {
                output.push_str(&format!(r"\x{{{:X}}}", ch as u32));
                class_escape = false;
            }
            '.' if !in_class => output.push_str(r"[^\r\n\x{2028}\x{2029}]"),
            _ => {
                output.push(ch);
                class_escape = false;
            }
        }
        index += 1;
    }
    output
}

/// Read the escape body after `\u`: four hex digits or `{hex}`. Returns the
/// code point and the index just past the escape, or `None` when the text is
/// not a complete `/u` escape (the caller then keeps `\u` literally).
fn read_js_unicode_escape(chars: &[char], start: usize) -> Option<(u32, usize)> {
    if chars.get(start) == Some(&'{') {
        let mut index = start + 1;
        let mut value = 0_u32;
        let first = index;
        while let Some(ch) = chars.get(index) {
            if *ch == '}' {
                break;
            }
            value = value.checked_mul(16)?.checked_add(ch.to_digit(16)?)?;
            index += 1;
        }
        if index == first || chars.get(index) != Some(&'}') || value > 0x10FFFF {
            return None;
        }
        return Some((value, index + 1));
    }
    let mut value = 0_u32;
    for offset in 0..4 {
        value = value * 16 + chars.get(start + offset)?.to_digit(16)?;
    }
    Some((value, start + 4))
}

/// Syntax-only result for one pattern. `unsupported` means ECMAScript `/u`
/// accepts the pattern, but this port refuses to translate it (lookarounds,
/// backreferences, named groups, Unicode/property escapes, ...).
struct ParsedJsPattern {
    unsupported: bool,
}

struct JsPatternSyntaxError(&'static str);

/// An iterative checker for the ECMAScript `RegExp` pattern grammar
/// under the `/u` flag. Ajv builds `new RegExp(pattern, "u")` for both
/// `pattern` and `patternProperties` (observed in
/// `probes/ajv-u-patterns-r3*.out`), so a pattern V8 rejects must be rejected
/// here before Rust's more permissive regex parser sees it.
struct JsPatternParser {
    source: Vec<char>,
    position: usize,
    captures: usize,
    names: Vec<String>,
    named_references: Vec<String>,
    max_backreference: u64,
    has_named_captures: bool,
    /// `true` for the schema `/u` grammar, `false` for the non-`u` grammar
    /// `ajv-formats` uses for `format: "regex"` (Annex B).
    unicode: bool,
    unsupported: bool,
}

struct ClassAtom {
    literal: Option<u32>,
    class_escape: bool,
    unsupported: bool,
}

fn parse_js_pattern(pattern: &str) -> Result<ParsedJsPattern, JsPatternSyntaxError> {
    parse_js_pattern_mode(pattern, true)
}

/// Syntax check for the non-`u` grammar used by `ajv-formats`' `regex`
/// format (`new RegExp(str)`): Annex B allows identity escapes, octal
/// escapes, a literal `]`/`}`, and `{` that is not a quantifier.
fn parse_js_pattern_non_unicode(pattern: &str) -> Result<(), JsPatternSyntaxError> {
    parse_js_pattern_mode(pattern, false).map(|_| ())
}

fn parse_js_pattern_mode(
    pattern: &str,
    unicode: bool,
) -> Result<ParsedJsPattern, JsPatternSyntaxError> {
    let source: Vec<char> = pattern.chars().collect();
    let mut parser = JsPatternParser {
        has_named_captures: has_named_captures(&source),
        source,
        position: 0,
        captures: 0,
        names: Vec::new(),
        named_references: Vec::new(),
        max_backreference: 0,
        unicode,
        unsupported: false,
    };
    parser.parse_disjunction()?;
    if parser.unicode && parser.max_backreference > parser.captures as u64 {
        return Err(JsPatternSyntaxError("Invalid escape"));
    }
    // Without `/u`, `\k<name>` is only meaningful when the pattern also has
    // named groups; standalone `\k<name>` is an identity escape.
    if parser.unicode || !parser.names.is_empty() {
        for reference in &parser.named_references {
            if !parser.names.contains(reference) {
                return Err(JsPatternSyntaxError("Invalid named capture referenced"));
            }
        }
    }
    Ok(ParsedJsPattern {
        unsupported: parser.unsupported,
    })
}

/// Annex B only treats `\k` as a backreference if a named group exists
/// anywhere in the pattern, including after the reference. This lexical
/// scan skips escapes and classes without recursing into group bodies.
fn has_named_captures(source: &[char]) -> bool {
    let mut in_class = false;
    let mut index = 0;
    while let Some(ch) = source.get(index) {
        match ch {
            '\\' => index += 1,
            '[' => in_class = true,
            ']' => in_class = false,
            '(' if !in_class
                && source.get(index + 1) == Some(&'?')
                && source.get(index + 2) == Some(&'<')
                && !matches!(source.get(index + 3), Some('=' | '!')) =>
            {
                return true;
            }
            _ => {}
        }
        index += 1;
    }
    false
}

impl JsPatternParser {
    fn peek(&self) -> Option<char> {
        self.source.get(self.position).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.source.get(self.position + offset).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let ch = self.peek()?;
        self.position += 1;
        Some(ch)
    }

    fn parse_disjunction(&mut self) -> Result<(), JsPatternSyntaxError> {
        // Each open group only needs to remember whether its closing atom
        // can be quantified. Keep this on the heap: schema compilation runs
        // on a 2 MiB worker stack, and regex format data is untrusted too.
        let mut groups = Vec::new();
        while let Some(ch) = self.peek() {
            match ch {
                '(' => groups.push(self.parse_group()?),
                ')' => {
                    let quantifiable = groups.pop().ok_or(JsPatternSyntaxError("Unmatched ')'"))?;
                    self.position += 1;
                    self.parse_quantifier(quantifiable)?;
                }
                '|' => self.position += 1,
                _ => self.parse_term()?,
            }
        }
        if !groups.is_empty() {
            return Err(JsPatternSyntaxError("Unterminated group"));
        }
        Ok(())
    }

    fn parse_term(&mut self) -> Result<(), JsPatternSyntaxError> {
        let Some(ch) = self.peek() else {
            return Ok(());
        };
        match ch {
            '^' | '$' => {
                self.position += 1;
                self.reject_quantifier("Nothing to repeat")?;
            }
            '\\' if matches!(self.peek_at(1), Some('b' | 'B')) => {
                self.position += 2;
                self.reject_quantifier("Nothing to repeat")?;
            }
            '\\' => {
                self.parse_escape_outside()?;
                self.parse_quantifier(true)?;
            }
            '[' => {
                self.parse_class()?;
                self.parse_quantifier(true)?;
            }
            '.' => {
                self.position += 1;
                self.parse_quantifier(true)?;
            }
            '*' | '+' | '?' => {
                return Err(JsPatternSyntaxError("Nothing to repeat"));
            }
            '{' => {
                if self.try_quantifier()?.is_some() {
                    return Err(JsPatternSyntaxError("Nothing to repeat"));
                }
                if self.unicode {
                    return Err(JsPatternSyntaxError("Lone quantifier brackets"));
                }
                self.position += 1;
                self.parse_quantifier(true)?;
            }
            ']' | '}' if self.unicode => {
                return Err(JsPatternSyntaxError("Lone quantifier brackets"));
            }
            ')' => return Err(JsPatternSyntaxError("Unmatched ')'")),
            _ => {
                self.position += 1;
                self.parse_quantifier(true)?;
            }
        }
        Ok(())
    }

    fn reject_quantifier(&mut self, detail: &'static str) -> Result<(), JsPatternSyntaxError> {
        match self.peek() {
            Some('*' | '+' | '?') => Err(JsPatternSyntaxError(detail)),
            Some('{') if self.unicode => Err(JsPatternSyntaxError(detail)),
            Some('{') => {
                // Without `/u`, `{` is literal unless it forms a quantifier.
                // `try_quantifier` restores the position on failure.
                if self.try_quantifier()?.is_some() {
                    Err(JsPatternSyntaxError(detail))
                } else {
                    Ok(())
                }
            }
            _ => Ok(()),
        }
    }

    fn parse_quantifier(&mut self, quantifiable: bool) -> Result<(), JsPatternSyntaxError> {
        match self.peek() {
            Some('*' | '+' | '?') => {
                if !quantifiable {
                    return Err(JsPatternSyntaxError("Invalid quantifier"));
                }
                self.position += 1;
                if self.peek() == Some('?') {
                    self.position += 1;
                }
                self.reject_quantifier("Nothing to repeat")?;
            }
            Some('{') => {
                if !quantifiable {
                    // Annex B only treats a complete `{n,m}` after a
                    // lookbehind as an invalid quantifier; `{,3}` and
                    // `{2x}` are literal braces there.
                    if self.unicode || self.try_quantifier()?.is_some() {
                        return Err(JsPatternSyntaxError("Invalid quantifier"));
                    }
                    return Ok(());
                }
                if self.try_quantifier()?.is_none() {
                    // Without `/u`, `{` that is not a quantifier is a literal
                    // and the term after it starts normally.
                    if self.unicode {
                        return Err(JsPatternSyntaxError("Incomplete quantifier"));
                    }
                    return Ok(());
                }
                if self.peek() == Some('?') {
                    self.position += 1;
                }
                self.reject_quantifier("Nothing to repeat")?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Parse `{n}`, `{n,}` or `{n,m}` at the current position. Returns `None`
    /// (after restoring the position) when the braces are not a quantifier.
    fn try_quantifier(&mut self) -> Result<Option<()>, JsPatternSyntaxError> {
        let start = self.position;
        debug_assert_eq!(self.bump(), Some('{'));
        let Some(minimum) = self.decimal_digits() else {
            self.position = start;
            return Ok(None);
        };
        match self.peek() {
            Some('}') => {
                self.position += 1;
            }
            Some(',') => {
                self.position += 1;
                if self.peek() == Some('}') {
                    self.position += 1;
                } else {
                    let Some(maximum) = self.decimal_digits() else {
                        self.position = start;
                        return Ok(None);
                    };
                    if self.peek() != Some('}') {
                        self.position = start;
                        return Ok(None);
                    }
                    self.position += 1;
                    if maximum < minimum {
                        return Err(JsPatternSyntaxError(
                            "numbers out of order in {} quantifier",
                        ));
                    }
                }
            }
            _ => {
                self.position = start;
                return Ok(None);
            }
        }
        Ok(Some(()))
    }

    fn decimal_digits(&mut self) -> Option<u64> {
        let start = self.position;
        let mut value = 0_u64;
        while let Some(ch) = self.peek() {
            if !ch.is_ascii_digit() {
                break;
            }
            value = value
                .saturating_mul(10)
                .saturating_add(u64::from(ch as u8 - b'0'));
            self.position += 1;
        }
        (self.position > start).then_some(value)
    }

    fn add_capture(&mut self) -> Result<(), JsPatternSyntaxError> {
        self.captures += 1;
        if self.captures > MAX_REGEXP_CAPTURES {
            return Err(JsPatternSyntaxError("Too many captures"));
        }
        Ok(())
    }

    /// Consume only the opening header; `parse_disjunction` owns the body
    /// and the closing parenthesis, without calling back into this method.
    fn parse_group(&mut self) -> Result<bool, JsPatternSyntaxError> {
        debug_assert_eq!(self.peek(), Some('('));
        self.position += 1;
        if self.peek() != Some('?') {
            self.add_capture()?;
            return Ok(true);
        }
        self.position += 1;
        match self.peek() {
            Some(':') => {
                self.position += 1;
                Ok(true)
            }
            Some('=' | '!') => {
                self.position += 1;
                self.unsupported = true;
                // Without `/u`, V8 allows quantifying a lookahead (Annex B).
                Ok(!self.unicode)
            }
            Some('<') => {
                self.position += 1;
                match self.peek() {
                    Some('=' | '!') => {
                        self.position += 1;
                        self.unsupported = true;
                        Ok(false)
                    }
                    _ => {
                        let name = self.parse_group_name()?;
                        if self.names.contains(&name) {
                            return Err(JsPatternSyntaxError("Duplicate capture group name"));
                        }
                        self.names.push(name);
                        self.add_capture()?;
                        Ok(true)
                    }
                }
            }
            _ => Err(JsPatternSyntaxError("Invalid group")),
        }
    }

    fn parse_group_name(&mut self) -> Result<String, JsPatternSyntaxError> {
        let start = self.position;
        while let Some(ch) = self.peek() {
            if ch == '>' {
                break;
            }
            self.position += 1;
        }
        if self.peek() != Some('>') {
            return Err(JsPatternSyntaxError("Invalid capture group name"));
        }
        let raw: String = self.source[start..self.position].iter().collect();
        self.position += 1;
        if raw.is_empty() {
            return Err(JsPatternSyntaxError("Invalid capture group name"));
        }
        let name = decode_group_name(&raw)?;
        if !valid_group_name(&name) {
            return Err(JsPatternSyntaxError("Invalid capture group name"));
        }
        Ok(name)
    }

    fn parse_escape_outside(&mut self) -> Result<(), JsPatternSyntaxError> {
        debug_assert_eq!(self.peek(), Some('\\'));
        self.position += 1;
        let Some(escaped) = self.bump() else {
            return Err(JsPatternSyntaxError("Invalid escape"));
        };
        match escaped {
            'd' | 'D' | 's' | 'S' | 'w' | 'W' => {}
            'p' | 'P' if self.unicode => self.parse_property_escape(false)?,
            'p' | 'P' => {}
            'f' | 'n' | 'r' | 't' | 'v' => {}
            'c' => {
                if self.unicode {
                    self.parse_control_escape()?;
                } else if self.peek().is_some_and(|ch| ch.is_ascii_alphabetic()) {
                    self.position += 1;
                }
            }
            '0' => {
                if self.unicode && self.peek().is_some_and(|ch| ch.is_ascii_digit()) {
                    return Err(JsPatternSyntaxError("Invalid decimal escape"));
                }
            }
            '1'..='9' => {
                if self.unicode {
                    let value = self.consume_decimal_number(escaped);
                    self.max_backreference = self.max_backreference.max(value);
                    self.unsupported = true;
                }
            }
            'x' if self.unicode => {
                self.parse_hex_escape()?;
            }
            'x' => {}
            'u' if self.unicode => {
                self.parse_unicode_escape()?;
            }
            'u' => {}
            'k' if self.unicode || self.has_named_captures => {
                self.parse_named_backreference()?;
            }
            // Only the escaped `k` is literal. The following `<...>` must
            // still be parsed, including any groups and quantifiers inside.
            'k' => {}
            other if is_js_syntax_character(other) || other == '/' => {}
            _ if self.unicode => return Err(JsPatternSyntaxError("Invalid escape")),
            // Annex B identity escape: any other escaped character is
            // accepted as a literal by `new RegExp(str)`.
            _ => {}
        }
        Ok(())
    }

    fn parse_class(&mut self) -> Result<(), JsPatternSyntaxError> {
        debug_assert_eq!(self.peek(), Some('['));
        self.position += 1;
        if self.peek() == Some('^') {
            self.position += 1;
        }
        if self.peek() == Some(']') {
            // `[]` and `[^]` are valid ECMAScript and match nothing/anything,
            // but Rust's regex parser has no equivalent empty class.
            self.position += 1;
            if self.unicode {
                self.unsupported = true;
            }
            return Ok(());
        }
        loop {
            match self.peek() {
                None => return Err(JsPatternSyntaxError("Unterminated character class")),
                Some(']') => {
                    self.position += 1;
                    return Ok(());
                }
                Some(_) => {}
            }
            let left = self.parse_class_atom()?;
            self.unsupported |= left.unsupported;
            if self.peek() == Some('-') && self.peek_at(1) != Some(']') {
                if self.peek_at(1).is_none() {
                    return Err(JsPatternSyntaxError("Unterminated character class"));
                }
                self.position += 1;
                let right = self.parse_class_atom()?;
                self.unsupported |= right.unsupported;
                if left.class_escape || right.class_escape {
                    // Annex B accepts class escapes as range endpoints
                    // (`[\w-a]`); `/u` rejects them.
                    if self.unicode {
                        return Err(JsPatternSyntaxError("Invalid character class"));
                    }
                } else {
                    let (Some(start), Some(end)) = (left.literal, right.literal) else {
                        return Err(JsPatternSyntaxError("Invalid character class"));
                    };
                    // Without `/u`, a surrogate pair is two class atoms. V8
                    // compares the low surrogate of the left endpoint with
                    // the high surrogate of the right endpoint, so
                    // `[😀-😁]` is out of order even though the code points
                    // are ordered.
                    let (start, end) = if self.unicode {
                        (start, end)
                    } else {
                        (
                            if start > 0xFFFF {
                                0xDC00 + (start - 0x10000) % 0x400
                            } else {
                                start
                            },
                            if end > 0xFFFF {
                                0xD800 + (end - 0x10000) / 0x400
                            } else {
                                end
                            },
                        )
                    };
                    if start > end {
                        return Err(JsPatternSyntaxError(
                            "Range out of order in character class",
                        ));
                    }
                }
            }
        }
    }

    fn parse_class_atom(&mut self) -> Result<ClassAtom, JsPatternSyntaxError> {
        let Some(ch) = self.bump() else {
            return Err(JsPatternSyntaxError("Unterminated character class"));
        };
        if ch != '\\' {
            return Ok(ClassAtom {
                literal: Some(ch as u32),
                class_escape: false,
                // A raw `[` inside a class is a literal in `/u`, but Rust
                // reads it as a nested class/set operation.
                unsupported: ch == '[',
            });
        }
        let Some(escaped) = self.bump() else {
            return Err(JsPatternSyntaxError("Invalid escape"));
        };
        let atom = match escaped {
            'b' => literal_class_atom(0x08),
            'd' | 'D' | 's' | 'S' | 'w' | 'W' => ClassAtom {
                literal: None,
                class_escape: true,
                unsupported: false,
            },
            'p' | 'P' if self.unicode => {
                self.parse_property_escape(true)?;
                ClassAtom {
                    literal: None,
                    class_escape: true,
                    unsupported: false,
                }
            }
            'p' | 'P' => literal_class_atom(u32::from(escaped)),
            'f' => literal_class_atom(0x0C),
            'n' => literal_class_atom(0x0A),
            'r' => literal_class_atom(0x0D),
            't' => literal_class_atom(0x09),
            'v' => literal_class_atom(0x0B),
            'c' => {
                if self.unicode {
                    literal_class_atom(self.parse_control_escape()?)
                } else {
                    if self.peek().is_some_and(|ch| ch.is_ascii_alphabetic()) {
                        self.position += 1;
                    }
                    literal_class_atom(u32::from('c'))
                }
            }
            '0' => {
                if self.unicode && self.peek().is_some_and(|ch| ch.is_ascii_digit()) {
                    return Err(JsPatternSyntaxError("Invalid decimal escape"));
                }
                literal_class_atom(0)
            }
            '1'..='9' => {
                if self.unicode {
                    return Err(JsPatternSyntaxError("Invalid class escape"));
                }
                literal_class_atom(u32::from(escaped))
            }
            'x' if self.unicode => literal_class_atom(self.parse_hex_escape()?),
            'x' => literal_class_atom(u32::from('x')),
            'u' if self.unicode => {
                let code_point = self.parse_unicode_escape()?;
                ClassAtom {
                    literal: Some(code_point),
                    class_escape: false,
                    unsupported: false,
                }
            }
            'u' => literal_class_atom(u32::from('u')),
            'k' => {
                if self.unicode || self.has_named_captures {
                    return Err(JsPatternSyntaxError("Invalid escape"));
                }
                literal_class_atom(u32::from('k'))
            }
            '-' => literal_class_atom(0x2D),
            other if is_js_syntax_character(other) || other == '/' || !self.unicode => {
                literal_class_atom(other as u32)
            }
            _ => return Err(JsPatternSyntaxError("Invalid escape")),
        };
        Ok(atom)
    }

    fn parse_property_escape(&mut self, in_class: bool) -> Result<(), JsPatternSyntaxError> {
        let detail = if in_class {
            "Invalid property name in character class"
        } else {
            "Invalid property name"
        };
        if self.peek() != Some('{') {
            return Err(JsPatternSyntaxError(detail));
        }
        self.position += 1;
        let start = self.position;
        while let Some(ch) = self.peek() {
            if ch == '}' {
                break;
            }
            // ECMAScript property names are exact: no whitespace and no
            // case folding, unlike Perl or Rust's property parser.
            if !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '=') {
                return Err(JsPatternSyntaxError(detail));
            }
            self.position += 1;
        }
        if self.position == start || self.peek() != Some('}') {
            return Err(JsPatternSyntaxError(detail));
        }
        let name: String = self.source[start..self.position].iter().collect();
        self.position += 1;
        let (key, value) = match name.split_once('=') {
            Some((key, value)) if !value.is_empty() => (Some(key), value),
            Some(_) => return Err(JsPatternSyntaxError(detail)),
            None => (None, name.as_str()),
        };
        if !unicode::valid_property(key, value) {
            return Err(JsPatternSyntaxError(detail));
        }
        // Syntax check only: Rust's Unicode tables may not cover a valid
        // property (e.g. `Emoji`). `js_regex` compiles the ones it can and
        // reports the rest as unsupported when the pattern is used.
        Ok(())
    }

    fn parse_control_escape(&mut self) -> Result<u32, JsPatternSyntaxError> {
        match self.bump() {
            Some(ch) if ch.is_ascii_alphabetic() => Ok(u32::from(ch) % 32),
            _ => Err(JsPatternSyntaxError("Invalid Unicode escape")),
        }
    }

    fn parse_hex_escape(&mut self) -> Result<u32, JsPatternSyntaxError> {
        let mut value = 0_u32;
        for _ in 0..2 {
            let Some(ch) = self.bump() else {
                return Err(JsPatternSyntaxError("Invalid escape"));
            };
            let Some(digit) = ch.to_digit(16) else {
                return Err(JsPatternSyntaxError("Invalid escape"));
            };
            value = value * 16 + digit;
        }
        Ok(value)
    }

    fn parse_unicode_escape(&mut self) -> Result<u32, JsPatternSyntaxError> {
        if self.peek() == Some('{') {
            self.position += 1;
            let start = self.position;
            let mut value = 0_u32;
            while let Some(ch) = self.peek() {
                if ch == '}' {
                    break;
                }
                let Some(digit) = ch.to_digit(16) else {
                    return Err(JsPatternSyntaxError("Invalid Unicode escape"));
                };
                value = value
                    .checked_mul(16)
                    .and_then(|value| value.checked_add(digit))
                    .ok_or(JsPatternSyntaxError("Invalid Unicode escape"))?;
                self.position += 1;
            }
            if self.position == start || self.peek() != Some('}') {
                return Err(JsPatternSyntaxError("Invalid Unicode escape"));
            }
            self.position += 1;
            if value > 0x10FFFF {
                return Err(JsPatternSyntaxError("Invalid Unicode escape"));
            }
            return Ok(value);
        }
        let mut value = 0_u32;
        for _ in 0..4 {
            let Some(ch) = self.bump() else {
                return Err(JsPatternSyntaxError("Invalid Unicode escape"));
            };
            let Some(digit) = ch.to_digit(16) else {
                return Err(JsPatternSyntaxError("Invalid Unicode escape"));
            };
            value = value * 16 + digit;
        }
        Ok(value)
    }

    fn parse_named_backreference(&mut self) -> Result<(), JsPatternSyntaxError> {
        if self.peek() != Some('<') {
            return Err(JsPatternSyntaxError("Invalid escape"));
        }
        self.position += 1;
        let start = self.position;
        while let Some(ch) = self.peek() {
            if ch == '>' {
                break;
            }
            self.position += 1;
        }
        if self.peek() != Some('>') {
            return Err(JsPatternSyntaxError("Invalid named capture referenced"));
        }
        let raw: String = self.source[start..self.position].iter().collect();
        self.position += 1;
        if raw.is_empty() {
            return Err(JsPatternSyntaxError("Invalid capture group name"));
        }
        let name = decode_group_name(&raw)?;
        if !valid_group_name(&name) {
            return Err(JsPatternSyntaxError("Invalid capture group name"));
        }
        self.named_references.push(name);
        self.unsupported = true;
        Ok(())
    }

    fn consume_decimal_number(&mut self, first: char) -> u64 {
        let mut value = u64::from(first as u8 - b'0');
        while let Some(ch) = self.peek() {
            if !ch.is_ascii_digit() {
                break;
            }
            value = value
                .saturating_mul(10)
                .saturating_add(u64::from(ch as u8 - b'0'));
            self.position += 1;
        }
        value
    }
}

/// `RegExpIdentifierName`: `ID_Start` (or `$`/`_`) followed by `ID_Continue`
/// (or `$`/`_`). Rust's Unicode tables are the same UCD generation enough for
/// this syntax gate; a name V8 rejects must not become a schema `JsOnly` pass.
fn valid_group_name(name: &str) -> bool {
    static GROUP_NAME: OnceLock<Regex> = OnceLock::new();
    GROUP_NAME
        .get_or_init(|| {
            Regex::new(r"^[\p{ID_Start}$_][\p{ID_Continue}$_]*$").expect("group name regex")
        })
        .is_match(name)
}

/// Decode the only escapes ECMAScript allows in a group name (`\uXXXX` and
/// `\u{...}`); any other backslash is an invalid capture group name.
fn decode_group_name(raw: &str) -> Result<String, JsPatternSyntaxError> {
    let chars: Vec<char> = raw.chars().collect();
    let mut decoded = String::with_capacity(raw.len());
    let mut index = 0;
    while index < chars.len() {
        if chars[index] != '\\' {
            decoded.push(chars[index]);
            index += 1;
            continue;
        }
        if chars.get(index + 1) != Some(&'u') {
            return Err(JsPatternSyntaxError("Invalid capture group name"));
        }
        let Some((code_point, next)) = read_js_unicode_escape(&chars, index + 2) else {
            return Err(JsPatternSyntaxError("Invalid capture group name"));
        };
        let Some(ch) = char::from_u32(code_point) else {
            return Err(JsPatternSyntaxError("Invalid capture group name"));
        };
        decoded.push(ch);
        index = next;
    }
    Ok(decoded)
}

fn literal_class_atom(code_point: u32) -> ClassAtom {
    ClassAtom {
        literal: Some(code_point),
        class_escape: false,
        unsupported: false,
    }
}

fn is_js_syntax_character(ch: char) -> bool {
    matches!(
        ch,
        '^' | '$' | '\\' | '.' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|'
    )
}

fn regex_matches(pattern: &JsString, text: &JsString) -> Result<bool, ValidationError> {
    let pattern_text = String::from_utf16_lossy(pattern.units());
    let text = text.to_utf8().map_err(|_| {
        ValidationError::Unsupported(
            "pattern cannot be applied to a string with an unpaired surrogate".into(),
        )
    })?;
    match js_regex(&pattern_text) {
        JsRegex::Compiled(regex) => Ok(regex.is_match(&text)),
        JsRegex::JsOnly => Err(ValidationError::Unsupported(format!(
            "pattern \"{pattern_text}\" uses JavaScript regular-expression syntax this port does not implement"
        ))),
        JsRegex::Invalid(detail) => Err(ValidationError::Unsupported(format!(
            "Invalid regular expression: /{pattern_text}/u: {detail}"
        ))),
    }
}

// ---------------------------------------------------------------------------
// ajv-formats (full mode, the SDK default)
// ---------------------------------------------------------------------------

struct Formats {
    time: Regex,
    uri: Regex,
    uri_reference: Regex,
    uri_template: Regex,
    email: Regex,
    ipv6: Regex,
    json_pointer: Regex,
    json_pointer_uri_fragment: Regex,
    relative_json_pointer: Regex,
    byte: Regex,
}

fn formats() -> &'static Formats {
    static FORMATS: OnceLock<Formats> = OnceLock::new();
    FORMATS.get_or_init(|| Formats {
        time: Regex::new(
            r"^([0-9]{2}):([0-9]{2}):([0-9]{2}(?:\.[0-9]+)?)(?:([zZ])|([+-])([0-9]{2})(?::?([0-9]{2}))?)?$",
        )
        .expect("time format"),
        uri: Regex::new(
            r#"^(?:[a-z][a-z0-9+\-.]*:)(?:\/?\/(?:(?:[a-z0-9\-._~!$&'()*+,;=:]|%[0-9a-f]{2})*@)?(?:\[(?:(?:(?:(?:[0-9a-f]{1,4}:){6}|::(?:[0-9a-f]{1,4}:){5}|(?:[0-9a-f]{1,4})?::(?:[0-9a-f]{1,4}:){4}|(?:(?:[0-9a-f]{1,4}:){0,1}[0-9a-f]{1,4})?::(?:[0-9a-f]{1,4}:){3}|(?:(?:[0-9a-f]{1,4}:){0,2}[0-9a-f]{1,4})?::(?:[0-9a-f]{1,4}:){2}|(?:(?:[0-9a-f]{1,4}:){0,3}[0-9a-f]{1,4})?::[0-9a-f]{1,4}:|(?:(?:[0-9a-f]{1,4}:){0,4}[0-9a-f]{1,4})?::)(?:[0-9a-f]{1,4}:[0-9a-f]{1,4}|(?:(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)\.){3}(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?))|(?:(?:[0-9a-f]{1,4}:){0,5}[0-9a-f]{1,4})?::[0-9a-f]{1,4}|(?:(?:[0-9a-f]{1,4}:){0,6}[0-9a-f]{1,4})?::)|[Vv][0-9a-f]+\.[a-z0-9\-._~!$&'()*+,;=:]+)\]|(?:(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)\.){3}(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)|(?:[a-z0-9\-._~!$&'()*+,;=]|%[0-9a-f]{2})*)(?::[0-9]*)?(?:\/(?:[a-z0-9\-._~!$&'()*+,;=:@]|%[0-9a-f]{2})*)*|\/(?:(?:[a-z0-9\-._~!$&'()*+,;=:@]|%[0-9a-f]{2})+(?:\/(?:[a-z0-9\-._~!$&'()*+,;=:@]|%[0-9a-f]{2})*)*)?|(?:[a-z0-9\-._~!$&'()*+,;=:@]|%[0-9a-f]{2})+(?:\/(?:[a-z0-9\-._~!$&'()*+,;=:@]|%[0-9a-f]{2})*)*)(?:\?(?:[a-z0-9\-._~!$&'()*+,;=:@/?]|%[0-9a-f]{2})*)?(?:#(?:[a-z0-9\-._~!$&'()*+,;=:@/?]|%[0-9a-f]{2})*)?$"#,
        )
        .expect("uri format"),
        uri_reference: Regex::new(
            r#"^(?:[a-z][a-z0-9+\-.]*:)?(?:\/?\/(?:(?:[a-z0-9\-._~!$&'()*+,;=:]|%[0-9a-f]{2})*@)?(?:\[(?:(?:(?:(?:[0-9a-f]{1,4}:){6}|::(?:[0-9a-f]{1,4}:){5}|(?:[0-9a-f]{1,4})?::(?:[0-9a-f]{1,4}:){4}|(?:(?:[0-9a-f]{1,4}:){0,1}[0-9a-f]{1,4})?::(?:[0-9a-f]{1,4}:){3}|(?:(?:[0-9a-f]{1,4}:){0,2}[0-9a-f]{1,4})?::(?:[0-9a-f]{1,4}:){2}|(?:(?:[0-9a-f]{1,4}:){0,3}[0-9a-f]{1,4})?::[0-9a-f]{1,4}:|(?:(?:[0-9a-f]{1,4}:){0,4}[0-9a-f]{1,4})?::)(?:[0-9a-f]{1,4}:[0-9a-f]{1,4}|(?:(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)\.){3}(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?))|(?:(?:[0-9a-f]{1,4}:){0,5}[0-9a-f]{1,4})?::[0-9a-f]{1,4}|(?:(?:[0-9a-f]{1,4}:){0,6}[0-9a-f]{1,4})?::)|[Vv][0-9a-f]+\.[a-z0-9\-._~!$&'()*+,;=:]+)\]|(?:(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)\.){3}(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)|(?:[a-z0-9\-._~!$&'"()*+,;=]|%[0-9a-f]{2})*)(?::[0-9]*)?(?:\/(?:[a-z0-9\-._~!$&'"()*+,;=:@]|%[0-9a-f]{2})*)*|\/(?:(?:[a-z0-9\-._~!$&'"()*+,;=:@]|%[0-9a-f]{2})+(?:\/(?:[a-z0-9\-._~!$&'"()*+,;=:@]|%[0-9a-f]{2})*)*)?|(?:[a-z0-9\-._~!$&'"()*+,;=:@]|%[0-9a-f]{2})+(?:\/(?:[a-z0-9\-._~!$&'"()*+,;=:@]|%[0-9a-f]{2})*)*)(?:\?(?:[a-z0-9\-._~!$&'"()*+,;=:@/?]|%[0-9a-f]{2})*)?(?:#(?:[a-z0-9\-._~!$&'"()*+,;=:@/?]|%[0-9a-f]{2})*)?$"#,
        )
        .expect("uri-reference format"),
        uri_template: Regex::new(
            r#"^(?:(?:[^\x00-\x20"'<>%\\^`{|}]|%[0-9a-f]{2})|\{[+#./;?&=,!@|]?(?:[a-z0-9_]|%[0-9a-f]{2})+(?::[1-9][0-9]{0,3}|\*)?(?:,(?:[a-z0-9_]|%[0-9a-f]{2})+(?::[1-9][0-9]{0,3}|\*)?)*\})*$"#,
        )
        .expect("uri-template format"),
        email: Regex::new(
            r#"^(?i)[a-z0-9!#$%&'*+/=?^_`{|}~-]+(?:\.[a-z0-9!#$%&'*+/=?^_`{|}~-]+)*@(?:[a-z0-9](?:[a-z0-9-]*[a-z0-9])?\.)+[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$"#,
        )
        .expect("email format"),
        ipv6: Regex::new(
            r#"(?i)^((([0-9a-f]{1,4}:){7}([0-9a-f]{1,4}|:))|(([0-9a-f]{1,4}:){6}(:[0-9a-f]{1,4}|((25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9]?[0-9])(\.(25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9]?[0-9])){3})|:))|(([0-9a-f]{1,4}:){5}(((:[0-9a-f]{1,4}){1,2})|:((25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9]?[0-9])(\.(25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9]?[0-9])){3})|:))|(([0-9a-f]{1,4}:){4}(((:[0-9a-f]{1,4}){1,3})|((:[0-9a-f]{1,4})?:((25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9]?[0-9])(\.(25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9]?[0-9])){3}))|:))|(([0-9a-f]{1,4}:){3}(((:[0-9a-f]{1,4}){1,4})|((:[0-9a-f]{1,4}){0,2}:((25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9]?[0-9])(\.(25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9]?[0-9])){3}))|:))|(([0-9a-f]{1,4}:){2}(((:[0-9a-f]{1,4}){1,5})|((:[0-9a-f]{1,4}){0,3}:((25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9]?[0-9])(\.(25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9]?[0-9])){3}))|:))|(([0-9a-f]{1,4}:){1}(((:[0-9a-f]{1,4}){1,6})|((:[0-9a-f]{1,4}){0,4}:((25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9]?[0-9])(\.(25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9]?[0-9])){3}))|:))|(:(((:[0-9a-f]{1,4}){1,7})|((:[0-9a-f]{1,4}){0,5}:((25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9]?[0-9])(\.(25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9]?[0-9])){3}))|:)))$"#,
        )
        .expect("ipv6 format"),
        json_pointer: Regex::new(r"^(?:/(?:[^~/]|~0|~1)*)*$").expect("json-pointer format"),
        json_pointer_uri_fragment: Regex::new(
            r"(?i)^#(?:/(?:[a-z0-9_\-.!$&'()*+,;:=@]|%[0-9a-f]{2}|~0|~1)*)*$",
        )
        .expect("json-pointer-uri-fragment format"),
        relative_json_pointer: Regex::new(
            r"^(?:0|[1-9][0-9]*)(?:#|(?:/(?:[^~/]|~0|~1)*)*)$",
        )
        .expect("relative-json-pointer format"),
        byte: Regex::new(
            r"(?m)^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$",
        )
        .expect("byte format"),
    })
}

fn string_format_valid(format: &str, text: &str) -> Option<bool> {
    if format == "regex" {
        return regex_format_valid(text);
    }
    let regexes = formats();
    Some(match format {
        "date" => date_valid(text),
        "time" => time_valid(text, true),
        "date-time" => date_time_valid(text, true),
        "iso-time" => time_valid(text, false),
        "iso-date-time" => date_time_valid(text, false),
        "duration" => duration_valid(text),
        "uri" => (text.contains('/') || text.contains(':')) && regexes.uri.is_match(text),
        "uri-reference" => regexes.uri_reference.is_match(text),
        "uri-template" => regexes.uri_template.is_match(text),
        "url" => url_valid(text),
        "email" => regexes.email.is_match(text),
        "hostname" => hostname_valid(text),
        "ipv4" => ipv4_valid(text),
        "ipv6" => regexes.ipv6.is_match(text),
        "uuid" => uuid_valid(text),
        "json-pointer" => regexes.json_pointer.is_match(text),
        "json-pointer-uri-fragment" => regexes.json_pointer_uri_fragment.is_match(text),
        "relative-json-pointer" => regexes.relative_json_pointer.is_match(text),
        "byte" => regexes.byte.is_match(text),
        "password" | "binary" => true,
        "int32" | "int64" | "float" | "double" => return None,
        _ => return None,
    })
}

fn number_format_valid(format: &str, value: f64) -> Option<bool> {
    Some(match format {
        "int32" => {
            value.is_finite()
                && value.fract() == 0.0
                && (-2_147_483_648.0..=2_147_483_647.0).contains(&value)
        }
        "int64" => value.is_finite() && value.fract() == 0.0,
        "float" | "double" => true,
        _ => return None,
    })
}

fn date_valid(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || ![0, 1, 2, 3, 5, 6, 8, 9]
            .iter()
            .all(|i| bytes[*i].is_ascii_digit())
    {
        return false;
    }
    let year: u32 = text[..4].parse().unwrap();
    let month: u32 = text[5..7].parse().unwrap();
    let day: u32 = text[8..].parse().unwrap();
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let days = [
        0,
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    (1..=12).contains(&month) && day >= 1 && day <= days[month as usize]
}

fn time_valid(text: &str, strict_time_zone: bool) -> bool {
    let Some(captures) = formats().time.captures(text) else {
        return false;
    };
    let hour: i32 = captures[1].parse().unwrap();
    let minute: i32 = captures[2].parse().unwrap();
    let second: f64 = captures[3].parse().unwrap();
    let time_zone = captures.get(4).is_some() || captures.get(5).is_some();
    let sign = if captures.get(5).is_some_and(|m| m.as_str() == "-") {
        -1
    } else {
        1
    };
    let zone_hour: i32 = captures.get(6).map_or(0, |m| m.as_str().parse().unwrap());
    let zone_minute: i32 = captures.get(7).map_or(0, |m| m.as_str().parse().unwrap());
    if zone_hour > 23 || zone_minute > 59 || (strict_time_zone && !time_zone) {
        return false;
    }
    if hour <= 23 && minute <= 59 && second < 60.0 {
        return true;
    }
    let utc_minute = minute - zone_minute * sign;
    let utc_hour = hour - zone_hour * sign - if utc_minute < 0 { 1 } else { 0 };
    (utc_hour == 23 || utc_hour == -1) && (utc_minute == 59 || utc_minute == -1) && second < 61.0
}

fn date_time_valid(text: &str, strict_time_zone: bool) -> bool {
    let parts: Vec<&str> = text
        .split(|c: char| c == 't' || c == 'T' || c.is_whitespace())
        .collect();
    parts.len() == 2 && date_valid(parts[0]) && time_valid(parts[1], strict_time_zone)
}

fn duration_valid(text: &str) -> bool {
    static MAIN: OnceLock<Regex> = OnceLock::new();
    static WEEK: OnceLock<Regex> = OnceLock::new();
    // `^P(?!$)` in ajv-formats: the bare "P" is invalid.
    if text.len() <= 1 {
        return false;
    }
    let main = MAIN.get_or_init(|| {
        Regex::new(
            r"^P(?:[0-9]+Y)?(?:[0-9]+M)?(?:[0-9]+D)?(?:T(?:[0-9]+H(?:[0-9]+M)?(?:[0-9]+S)?|[0-9]+M(?:[0-9]+S)?|[0-9]+S))?$",
        )
        .unwrap()
    });
    let week = WEEK.get_or_init(|| Regex::new(r"^P[0-9]+W$").unwrap());
    main.is_match(text) || week.is_match(text)
}

fn hostname_valid(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let body = lower.strip_suffix('.').unwrap_or(&lower);
    let label = |label: &str| {
        (1..=63).contains(&label.len())
            && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            && !label.starts_with('-')
            && !label.ends_with('-')
    };
    (1..=253).contains(&body.len()) && body.split('.').all(label)
}

fn ipv4_valid(text: &str) -> bool {
    let parts: Vec<&str> = text.split('.').collect();
    parts.len() == 4
        && parts.iter().all(|part| {
            !part.is_empty()
                && part.len() <= 3
                && part.bytes().all(|b| b.is_ascii_digit())
                && (part.len() == 1 || !part.starts_with('0'))
                && part.parse::<u16>().is_ok_and(|n| n <= 255)
        })
}

fn uuid_valid(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let body = lower.strip_prefix("urn:uuid:").unwrap_or(&lower);
    let groups: Vec<&str> = body.split('-').collect();
    groups.len() == 5
        && groups.iter().zip([8, 4, 4, 4, 12]).all(|(group, length)| {
            group.len() == length && group.bytes().all(|b| b.is_ascii_hexdigit())
        })
}

/// `ajv-formats`' `regex` format is:
/// `if (/[^\\]\\Z/.test(str)) return false; try { new RegExp(str); ... }`
/// (`formats.js:197-207`). It is the **non-`/u`** grammar, which is much more
/// permissive than the schema `/u` gate. The return value is `None` when this
/// port cannot decide a value exactly; callers must turn that into an explicit
/// unsupported error instead of a silent pass.
fn regex_format_valid(text: &str) -> Option<bool> {
    // The literal `/[^\\]\\Z/` guard: a `\Z` escape (not a literal `\\Z`)
    // with at least one preceding character. The regex only looks at the one
    // character immediately before the backslash, so reproduce that quirk.
    for (index, _) in text.match_indices("\\Z") {
        if index > 0 && text.as_bytes()[index - 1] != b'\\' {
            return Some(false);
        }
    }
    Some(parse_js_pattern_non_unicode(text).is_ok())
}

fn is_uri_host_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || ('\u{a1}'..='\u{ffff}').contains(&c)
}

fn is_tld_char(c: char) -> bool {
    c.is_ascii_alphabetic() || ('\u{a1}'..='\u{ffff}').contains(&c)
}

/// `url` from ajv-formats: scheme, optional userinfo/port/path, then a public
/// IPv4 or a domain with a 2+ character alphabetic TLD. The original regex
/// excludes 10/127/169.254/192.168/172.16-31 hosts.
fn url_valid(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let rest = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))
        .or_else(|| lower.strip_prefix("ftp://"));
    let Some(rest) = rest else {
        return false;
    };
    let (authority, path) = match rest.split_once('/') {
        Some((authority, path)) => (authority, Some(path)),
        None => (rest, None),
    };
    if authority.chars().any(char::is_whitespace) {
        return false;
    }
    if let Some(path) = path
        && path.chars().any(char::is_whitespace)
    {
        return false;
    }
    let host_port = match authority.rsplit_once('@') {
        Some((user, host)) => {
            if user.is_empty() {
                return false;
            }
            host
        }
        None => authority,
    };
    let (host, port) = match host_port.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() && !port.is_empty() => (host, Some(port)),
        _ => (host_port, None),
    };
    if host.contains(':') {
        return false;
    }
    if let Some(port) = port
        && (!(2..=5).contains(&port.len()) || !port.bytes().all(|b| b.is_ascii_digit()))
    {
        return false;
    }
    if public_ipv4(host) {
        return true;
    }
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2 {
        return false;
    }
    for label in &labels {
        if label.is_empty()
            || label.starts_with('-')
            || label.ends_with('-')
            || !label.chars().all(|c| is_uri_host_char(c) || c == '-')
        {
            return false;
        }
    }
    let tld = labels.last().unwrap();
    tld.chars().count() >= 2 && tld.chars().all(is_tld_char)
}

fn public_ipv4(host: &str) -> bool {
    let parts: Vec<&str> = host.split('.').collect();
    if parts.len() != 4 {
        return false;
    }
    let mut octets = [0u32; 4];
    for (index, part) in parts.iter().enumerate() {
        if part.is_empty() || part.len() > 3 || !part.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
        octets[index] = part.parse().unwrap();
    }
    // First octet: [1-9]digits? | 1dd | 2[01]d | 22[0-3] -> 1..=223, no leading zero.
    if (parts[0].len() > 1 && parts[0].starts_with('0')) || !(1..=223).contains(&octets[0]) {
        return false;
    }
    // Middle octets: 1?d{1,2} | 2[0-4]d | 25[0-5] -> 0..=255, 3 digits only from 1xx/2xx.
    for part in [parts[1], parts[2]] {
        if part.len() == 3 && !part.starts_with('1') && !part.starts_with('2') {
            return false;
        }
    }
    if octets[1] > 255 || octets[2] > 255 {
        return false;
    }
    // Last octet: 1..=254, no leading zero.
    if (parts[3].len() > 1 && parts[3].starts_with('0')) || !(1..=254).contains(&octets[3]) {
        return false;
    }
    if octets[0] == 10 || octets[0] == 127 {
        return false;
    }
    if octets[0] == 169 && octets[1] == 254 {
        return false;
    }
    if octets[0] == 192 && octets[1] == 168 {
        return false;
    }
    if octets[0] == 172 && (16..=31).contains(&octets[1]) {
        return false;
    }
    true
}

impl<'a> Validator<'a> {
    fn fail(&mut self, path: &str, message: impl Into<String>) {
        self.errors.push((path.to_owned(), message.into()));
    }

    /// Validates into a scratch error list; returns validity and the errors.
    fn trial(
        &mut self,
        schema: &JsValue,
        data: &JsValue,
        path: &str,
    ) -> Result<(bool, Vec<(String, String)>), ValidationError> {
        let saved = std::mem::take(&mut self.errors);
        let result = self.check(schema, data, path);
        let errors = std::mem::replace(&mut self.errors, saved);
        let valid = result?;
        Ok((valid, errors))
    }

    /// Returns whether `data` is valid; errors are appended.
    fn check(
        &mut self,
        schema: &JsValue,
        data: &JsValue,
        path: &str,
    ) -> Result<bool, ValidationError> {
        let start = self.errors.len();
        let schema = match schema {
            JsValue::Bool(true) => return Ok(true),
            JsValue::Bool(false) => {
                self.fail(path, "boolean schema is false");
                return Ok(false);
            }
            JsValue::Object(object) => object,
            _ => return Ok(true),
        };
        if self.depth >= MAX_DEPTH {
            return Err(ValidationError::Threw);
        }
        let pair = (
            schema as *const JsObject as usize,
            data as *const JsValue as usize,
        );
        if !self.seen.insert(pair) {
            return Err(ValidationError::Threw);
        }
        self.depth += 1;
        let result = self.keywords(schema, data, path);
        self.depth -= 1;
        self.seen.remove(&pair);
        result?;
        Ok(self.errors.len() == start)
    }

    fn keywords(
        &mut self,
        schema: &JsObject,
        data: &JsValue,
        path: &str,
    ) -> Result<(), ValidationError> {
        let declared: Vec<String> = match schema.get("type") {
            // Ajv treats a falsy `type` (notably `""`) as "no type".
            Some(JsValue::String(name)) if !name.units().is_empty() => {
                vec![String::from_utf16_lossy(name.units())]
            }
            Some(JsValue::Array(names)) => names
                .iter()
                .filter_map(|n| n.as_string().map(|n| String::from_utf16_lossy(n.units())))
                .collect(),
            _ => Vec::new(),
        };
        let mut types = declared.clone();
        if let Some(JsValue::Bool(true)) = schema.get("nullable")
            && !types.iter().any(|name| name == "null")
        {
            types.push("null".into());
        }
        let group_rules = |group: &str| -> &'static [&'static str] {
            match group {
                "number" => &NUMBER_RULES,
                "string" => &STRING_RULES,
                "array" => &ARRAY_RULES,
                "object" => &OBJECT_RULES,
                _ => &[],
            }
        };
        let has_rules = |group: &str| group_rules(group).iter().any(|k| schema.get(k).is_some());
        let check_types = !types.is_empty() && !(types.len() == 1 && has_rules(&types[0]));
        let type_message = format!("must be {}", declared.join(","));
        if check_types && !types.iter().any(|t| data_type_matches(t, data)) {
            self.fail(path, type_message.clone());
        }
        self.untyped(schema, data, path)?;
        for group in ["number", "string", "array", "object"] {
            if !has_rules(group) {
                continue;
            }
            if data_type_matches(group, data) {
                match group {
                    "number" => self.number_rules(schema, data, path),
                    "string" => self.string_rules(schema, data, path)?,
                    "array" => self.array_rules(schema, data, path)?,
                    _ => self.object_rules(schema, data, path)?,
                }
            } else if !check_types && types.len() == 1 && types[0] == group {
                self.fail(path, type_message.clone());
            }
        }
        Ok(())
    }

    fn untyped(
        &mut self,
        schema: &JsObject,
        data: &JsValue,
        path: &str,
    ) -> Result<(), ValidationError> {
        if let Some(JsValue::String(reference)) = schema.get("$ref") {
            let reference = reference.to_utf8().map_err(|_| {
                ValidationError::Unsupported("$ref contains an unpaired surrogate".into())
            })?;
            let target = self
                .refs
                .resolve(&reference)
                .map_err(|SchemaError(message)| ValidationError::Unsupported(message))?;
            self.check(target, data, path)?;
        }
        if let Some(expected) = schema.get("const")
            && !deep_equal(expected, data)
        {
            self.fail(path, "must be equal to constant");
        }
        if let Some(JsValue::Array(allowed)) = schema.get("enum")
            && !allowed.iter().any(|value| deep_equal(value, data))
        {
            self.fail(path, "must be equal to one of the allowed values");
        }
        if let Some(not) = schema.get("not") {
            let (valid, _) = self.trial(not, data, path)?;
            if valid {
                self.fail(path, "must NOT be valid");
            }
        }
        if let Some(JsValue::Array(options)) = schema.get("anyOf") {
            let mut collected = Vec::new();
            let mut any = false;
            for option in options {
                let (valid, errors) = self.trial(option, data, path)?;
                any |= valid;
                collected.extend(errors);
            }
            if !any {
                self.errors.extend(collected);
                self.fail(path, "must match a schema in anyOf");
            }
        }
        if let Some(JsValue::Array(options)) = schema.get("oneOf") {
            let mut collected = Vec::new();
            let mut passing = 0;
            for option in options {
                let (valid, errors) = self.trial(option, data, path)?;
                passing += usize::from(valid);
                collected.extend(errors);
            }
            if passing != 1 {
                if passing == 0 {
                    self.errors.extend(collected);
                }
                self.fail(path, "must match exactly one schema in oneOf");
            }
        }
        if let Some(JsValue::Array(parts)) = schema.get("allOf") {
            for part in parts {
                self.check(part, data, path)?;
            }
        }
        if let Some(condition) = schema.get("if") {
            let (valid, _) = self.trial(condition, data, path)?;
            let (branch, name) = if valid {
                ("then", "then")
            } else {
                ("else", "else")
            };
            if let Some(next) = schema.get(branch)
                && !self.check(next, data, path)?
            {
                self.fail(path, format!("must match \"{name}\" schema"));
            }
        }
        Ok(())
    }

    fn number_rules(&mut self, schema: &JsObject, data: &JsValue, path: &str) {
        let value = data.as_number().unwrap_or(f64::NAN);
        let text = js_value::number_string;
        if let Some(max) = number(schema.get("maximum"))
            && value.partial_cmp(&max).is_none_or(|o| o.is_gt())
        {
            self.fail(path, format!("must be <= {}", text(max)));
        }
        if let Some(min) = number(schema.get("minimum"))
            && value.partial_cmp(&min).is_none_or(|o| o.is_lt())
        {
            self.fail(path, format!("must be >= {}", text(min)));
        }
        if let Some(max) = number(schema.get("exclusiveMaximum"))
            && value.partial_cmp(&max).is_none_or(|o| o.is_ge())
        {
            self.fail(path, format!("must be < {}", text(max)));
        }
        if let Some(min) = number(schema.get("exclusiveMinimum"))
            && value.partial_cmp(&min).is_none_or(|o| o.is_le())
        {
            self.fail(path, format!("must be > {}", text(min)));
        }
        if let Some(divisor) = number(schema.get("multipleOf"))
            && (value / divisor) % 1.0 != 0.0
        {
            self.fail(path, format!("must be multiple of {}", text(divisor)));
        }
        if let Some(format) = schema.get("format").and_then(JsValue::as_string) {
            let name = String::from_utf16_lossy(format.units());
            if number_format_valid(&name, value) == Some(false) {
                self.fail(path, format!("must match format \"{name}\""));
            }
        }
    }

    fn string_rules(
        &mut self,
        schema: &JsObject,
        data: &JsValue,
        path: &str,
    ) -> Result<(), ValidationError> {
        let Some(text) = data.as_string() else {
            return Ok(());
        };
        let length = code_points(text) as f64;
        if let Some(max) = number(schema.get("maxLength"))
            && length > max
        {
            self.fail(
                path,
                format!(
                    "must NOT have more than {} characters",
                    js_value::number_string(max)
                ),
            );
        }
        if let Some(min) = number(schema.get("minLength"))
            && length < min
        {
            self.fail(
                path,
                format!(
                    "must NOT have fewer than {} characters",
                    js_value::number_string(min)
                ),
            );
        }
        if let Some(pattern) = schema.get("pattern").and_then(JsValue::as_string)
            && !regex_matches(pattern, text)?
        {
            self.fail(
                path,
                format!(
                    "must match pattern \"{}\"",
                    String::from_utf16_lossy(pattern.units())
                ),
            );
        }
        if let Some(format) = schema.get("format").and_then(JsValue::as_string) {
            let name = String::from_utf16_lossy(format.units());
            let value = String::from_utf16_lossy(text.units());
            match string_format_valid(&name, &value) {
                Some(false) => self.fail(path, format!("must match format \"{name}\"")),
                Some(true) => {}
                // Only `regex` can currently be undecidable; an unknown
                // format is ignored by ajv-formats, like Ajv with
                // `strict: false`.
                None if name == "regex" => {
                    return Err(ValidationError::Unsupported(format!(
                        "format \"regex\" value cannot be decided by this port: {value:?}"
                    )));
                }
                None => {}
            }
        }
        Ok(())
    }

    fn array_rules(
        &mut self,
        schema: &JsObject,
        data: &JsValue,
        path: &str,
    ) -> Result<(), ValidationError> {
        let JsValue::Array(items) = data else {
            return Ok(());
        };
        let length = items.len() as f64;
        if let Some(max) = number(schema.get("maxItems"))
            && length > max
        {
            self.fail(
                path,
                format!(
                    "must NOT have more than {} items",
                    js_value::number_string(max)
                ),
            );
        }
        if let Some(min) = number(schema.get("minItems"))
            && length < min
        {
            self.fail(
                path,
                format!(
                    "must NOT have fewer than {} items",
                    js_value::number_string(min)
                ),
            );
        }
        let child = |index: usize| format!("{path}/{index}");
        if let Some(JsValue::Array(tuple)) = schema.get("items") {
            match schema.get("additionalItems") {
                Some(JsValue::Bool(false)) if items.len() > tuple.len() => {
                    self.fail(
                        path,
                        format!("must NOT have more than {} items", tuple.len()),
                    );
                }
                Some(extra @ JsValue::Object(_)) => {
                    for (index, item) in items.iter().enumerate().skip(tuple.len()) {
                        self.check(extra, item, &child(index))?;
                    }
                }
                _ => {}
            }
            for (index, (item, item_schema)) in items.iter().zip(tuple).enumerate() {
                self.check(item_schema, item, &child(index))?;
            }
        } else if let Some(item_schema) = schema.get("items") {
            for (index, item) in items.iter().enumerate() {
                self.check(item_schema, item, &child(index))?;
            }
        }
        if let Some(contains) = schema.get("contains") {
            let mut collected = Vec::new();
            let mut any = false;
            for (index, item) in items.iter().enumerate() {
                let (valid, errors) = self.trial(contains, item, &child(index))?;
                any |= valid;
                collected.extend(errors);
            }
            if !any {
                self.errors.extend(collected);
                self.fail(path, "must contain at least 1 valid item(s)");
            }
        }
        if matches!(schema.get("uniqueItems"), Some(JsValue::Bool(true))) {
            self.unique_items(schema, items, path)?;
        }
        Ok(())
    }

    fn unique_items(
        &mut self,
        schema: &JsObject,
        items: &[JsValue],
        path: &str,
    ) -> Result<(), ValidationError> {
        // Ajv has two code paths (`uniqueItems.js`): with a typed `items`
        // schema of primitive type it walks from the end and reports
        // `(items ## later and earlier)`; otherwise it walks nested loops and
        // reports `(items ## earlier and later)`. Measured in
        // `probes/sdk-ajv*.out`.
        let item_schema = schema.get("items").and_then(JsValue::as_object);
        let mut item_types: Vec<String> = item_schema
            .and_then(|items| items.get("type"))
            .map(|value| match value {
                JsValue::String(name) => vec![String::from_utf16_lossy(name.units())],
                JsValue::Array(names) => names
                    .iter()
                    .filter_map(|n| n.as_string().map(|n| String::from_utf16_lossy(n.units())))
                    .collect(),
                _ => Vec::new(),
            })
            .unwrap_or_default();
        if item_schema.is_some_and(|items| {
            matches!(items.get("nullable"), Some(JsValue::Bool(true)))
                && !item_types.iter().any(|name| name == "null")
        }) {
            item_types.push("null".into());
        }
        let optimized = !item_types.is_empty()
            && !item_types
                .iter()
                .any(|name| name == "object" || name == "array");
        if optimized {
            let mut seen: HashMap<String, usize> = HashMap::new();
            for index in (0..items.len()).rev() {
                if !item_types
                    .iter()
                    .any(|name| data_type_matches(name, &items[index]))
                {
                    continue;
                }
                let mut key = js_text(&items[index]);
                if item_types.len() > 1 && matches!(items[index], JsValue::String(_)) {
                    key.push('_');
                }
                if let Some(other) = seen.get(&key) {
                    self.fail(
                        path,
                        format!(
                            "must NOT have duplicate items (items ## {other} and {index} are identical)"
                        ),
                    );
                    return Ok(());
                }
                seen.insert(key, index);
            }
        } else {
            'outer: for i in (0..items.len()).rev() {
                for j in (0..i).rev() {
                    if deep_equal(&items[i], &items[j]) {
                        self.fail(
                            path,
                            format!(
                                "must NOT have duplicate items (items ## {j} and {i} are identical)"
                            ),
                        );
                        break 'outer;
                    }
                }
            }
        }
        Ok(())
    }

    fn object_rules(
        &mut self,
        schema: &JsObject,
        data: &JsValue,
        path: &str,
    ) -> Result<(), ValidationError> {
        let JsValue::Object(object) = data else {
            return Ok(());
        };
        let entries = object.own_entries();
        let count = entries.len() as f64;
        let child = |key: &JsString| format!("{path}/{}", pointer_segment(key));
        if let Some(max) = number(schema.get("maxProperties"))
            && count > max
        {
            self.fail(
                path,
                format!(
                    "must NOT have more than {} properties",
                    js_value::number_string(max)
                ),
            );
        }
        if let Some(min) = number(schema.get("minProperties"))
            && count < min
        {
            self.fail(
                path,
                format!(
                    "must NOT have fewer than {} properties",
                    js_value::number_string(min)
                ),
            );
        }
        if let Some(JsValue::Array(required)) = schema.get("required") {
            for name in required {
                let text = js_text(name);
                let key = JsString::from(text.clone());
                if object.get_key(&key).is_none_or(JsValue::is_undefined) {
                    self.fail(path, format!("must have required property '{text}'"));
                }
            }
        }
        if let Some(names) = schema.get("propertyNames") {
            for (key, _) in &entries {
                if !self.check(names, &JsValue::String((*key).clone()), path)? {
                    self.fail(path, "property name must be valid");
                }
            }
        }
        let properties = schema.get("properties").and_then(JsValue::as_object);
        let patterns: Vec<(&JsString, &JsValue)> = schema
            .get("patternProperties")
            .and_then(JsValue::as_object)
            .map(|p| p.own_entries())
            .unwrap_or_default();
        let (pattern_properties, additional_properties) = pattern_properties_codegen(schema);
        if additional_properties && let Some(additional) = schema.get("additionalProperties") {
            for (key, value) in &entries {
                let declared = properties.is_some_and(|p| p.get_key(key).is_some());
                let mut patterned = false;
                for (pattern, _) in &patterns {
                    if regex_matches(pattern, key)? {
                        patterned = true;
                        break;
                    }
                }
                if declared || patterned {
                    continue;
                }
                match additional {
                    JsValue::Bool(false) => self.fail(path, "must NOT have additional properties"),
                    JsValue::Object(_) => {
                        self.check(additional, value, &child(key))?;
                    }
                    _ => {}
                }
            }
        }
        if let Some(JsValue::Object(dependencies)) = schema.get("dependencies") {
            for (name, dependency) in dependencies.own_entries() {
                if object.get_key(name).is_none_or(JsValue::is_undefined) {
                    continue;
                }
                match dependency {
                    JsValue::Array(needed) => {
                        let names: Vec<String> = needed.iter().map(js_text).collect();
                        let noun = if names.len() == 1 {
                            "property"
                        } else {
                            "properties"
                        };
                        for missing in needed {
                            let key = JsString::from(js_text(missing));
                            if object.get_key(&key).is_none_or(JsValue::is_undefined) {
                                self.fail(
                                    path,
                                    format!(
                                        "must have {noun} {} when property {} is present",
                                        names.join(", "),
                                        String::from_utf16_lossy(name.units())
                                    ),
                                );
                            }
                        }
                    }
                    other => {
                        self.check(other, data, path)?;
                    }
                }
            }
        }
        if let Some(properties) = properties {
            for (name, property) in properties.own_entries() {
                if let Some(value) = object.get_key(name).filter(|v| !v.is_undefined()) {
                    self.check(property, value, &child(name))?;
                }
            }
        }
        if pattern_properties {
            for (pattern, property) in patterns {
                for (key, value) in &entries {
                    if regex_matches(pattern, key)? {
                        self.check(property, value, &child(key))?;
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pilotdeck_protocol::parse_json;

    /// Expected strings are Ajv 8.20 output recorded in
    /// `_tmp/20261020-a65-pr10b/probes/ajv.out` and re-generated in
    /// `tests/fixtures/output-schema/ajv-oracle.json`.
    #[test]
    fn matches_ajv_errors_text() {
        let cases: &[(&str, &str, &str)] = &[
            (
                r#"{"type":"object","properties":{"a":{"type":"string"}},"required":["a"]}"#,
                r#"{"a":1}"#,
                "data/a must be string",
            ),
            (
                r#"{"type":"object","properties":{"a":{"type":"string"},"b":{"type":"number"}},"required":["a","b","c"],"additionalProperties":false}"#,
                r#"{"a":1,"z":2,"b":"x"}"#,
                "data must have required property 'c', data must NOT have additional properties, data/a must be string, data/b must be number",
            ),
            (
                r#"{"type":"object","required":["a"],"properties":{"a":{"type":["string","null"],"minLength":3,"maxLength":1,"pattern":"^z"}}}"#,
                r#"{"a":"xy"}"#,
                "data/a must NOT have more than 1 characters, data/a must NOT have fewer than 3 characters, data/a must match pattern \"^z\"",
            ),
            (
                r#"{"type":"object","properties":{"n":{"type":"integer","minimum":2,"maximum":0,"exclusiveMinimum":5,"exclusiveMaximum":0,"multipleOf":3}}}"#,
                r#"{"n":1.5}"#,
                "data/n must be integer, data/n must be <= 0, data/n must be >= 2, data/n must be < 0, data/n must be > 5, data/n must be multiple of 3",
            ),
            (
                r#"{"type":"object","properties":{"l":{"type":"array","items":{"type":"string"},"minItems":3,"maxItems":1,"uniqueItems":true}}}"#,
                r#"{"l":[1,"a","a"]}"#,
                "data/l must NOT have more than 1 items, data/l/0 must be string, data/l must NOT have duplicate items (items ## 2 and 1 are identical)",
            ),
            (
                r#"{"type":"object","properties":{"e":{"enum":["a","b"]},"c":{"const":5}}}"#,
                r#"{"e":"z","c":4}"#,
                "data/e must be equal to one of the allowed values, data/c must be equal to constant",
            ),
            (
                r#"{"type":"object","properties":{"u":{"anyOf":[{"type":"string"},{"type":"number"}]},"o":{"oneOf":[{"type":"number"},{"minimum":0}]},"x":{"not":{"type":"string"}},"al":{"allOf":[{"type":"number"},{"minimum":3}]}}}"#,
                r#"{"u":true,"o":5,"x":"s","al":1}"#,
                "data/u must be string, data/u must be number, data/u must match a schema in anyOf, data/o must match exactly one schema in oneOf, data/x must NOT be valid, data/al must be >= 3",
            ),
            (
                r#"{"type":"object","properties":{"a/b~c":{"type":"string"}},"minProperties":5,"maxProperties":1}"#,
                r#"{"a/b~c":1}"#,
                "data must NOT have fewer than 5 properties, data/a~1b~0c must be string",
            ),
            (
                r#"{"type":"object","additionalProperties":{"type":"number"},"properties":{"k":{}}}"#,
                r#"{"k":1,"q":"x","r":2}"#,
                "data/q must be number",
            ),
            (
                r#"{"type":"object","properties":{"s":{"type":"string","format":"email"}}}"#,
                r#"{"s":"nope"}"#,
                "data/s must match format \"email\"",
            ),
            (
                r#"{"type":"object","properties":{"d":{"type":"object","properties":{"e":{"type":"boolean"}},"required":["f"]}}}"#,
                r#"{"d":{"e":"x"}}"#,
                "data/d must have required property 'f', data/d/e must be boolean",
            ),
            (
                r#"{"type":"object","properties":{"t":{"type":"number"}}}"#,
                r#"{"t":"a"}"#,
                "data/t must be number",
            ),
            (
                r#"{"type":"object","properties":{"t":{"type":"string","enum":["a"]}}}"#,
                r#"{"t":5}"#,
                "data/t must be string, data/t must be equal to one of the allowed values",
            ),
            (
                r#"{"type":"object","properties":{"i":{"items":[{"type":"string"}],"additionalItems":false}}}"#,
                r#"{"i":[1,2]}"#,
                "data/i must NOT have more than 1 items, data/i/0 must be string",
            ),
            (
                r##"{"type":"object","properties":{"r":{"$ref":"#/definitions/x"}},"definitions":{"x":{"type":"string"}}}"##,
                r#"{"r":1}"#,
                "data/r must be string",
            ),
            (
                r#"{"type":"object","patternProperties":{"^p":{"type":"string"}}}"#,
                r#"{"p1":1}"#,
                "data/p1 must be string",
            ),
            (
                r#"{"type":"object","propertyNames":{"maxLength":2}}"#,
                r#"{"abc":1}"#,
                "data must NOT have more than 2 characters, data property name must be valid",
            ),
            (
                r#"{"type":"object","dependencies":{"a":["b"]}}"#,
                r#"{"a":1}"#,
                "data must have property b when property a is present",
            ),
            (
                r#"{"type":"object","if":{"required":["a"]},"then":{"required":["b"]}}"#,
                r#"{"a":1}"#,
                "data must have required property 'b', data must match \"then\" schema",
            ),
            (
                r#"{"type":"object","properties":{"c":{"contains":{"type":"string"}}}}"#,
                r#"{"c":[1]}"#,
                "data/c/0 must be string, data/c must contain at least 1 valid item(s)",
            ),
            (
                r#"{"type":"object","properties":{"n":{"type":"number","minimum":0.5}}}"#,
                r#"{"n":0.25}"#,
                "data/n must be >= 0.5",
            ),
        ];
        for (schema, data, expected) in cases {
            let schema = parse_json(schema).unwrap();
            let data = parse_json(data).unwrap();
            assert_eq!(
                validate(&schema, &data).unwrap().as_deref(),
                Some(*expected),
                "{schema:?}"
            );
        }
        let schema =
            parse_json(r#"{"type":"object","properties":{"a":{"type":"string"}}}"#).unwrap();
        assert_eq!(
            validate(&schema, &parse_json(r#"{"a":"x"}"#).unwrap()).unwrap(),
            None
        );
    }

    #[test]
    fn self_referencing_schemas_are_bounded() {
        let schema = parse_json(r##"{"$ref":"#"}"##).unwrap();
        assert_eq!(
            validate(&schema, &JsValue::Null),
            Err(ValidationError::Threw)
        );
    }

    #[test]
    fn deep_recursive_data_matches_ajv_at_4000() {
        // `probes/sdk-ajv3.out`: Ajv accepts a 4000-deep recursive object.
        // `validate` runs on a dedicated 64 MiB stack for exactly this case.
        let mut data = JsValue::Object(JsObject::new());
        for _ in 0..4000 {
            let mut object = JsObject::new();
            object.insert("a".into(), data);
            data = JsValue::Object(object);
        }
        let schema = parse_json(r##"{"type":"object","properties":{"a":{"$ref":"#"}}}"##).unwrap();
        assert_eq!(validate(&schema, &data).unwrap(), None);
    }

    #[test]
    fn nullable_and_anchor_refs_match_ajv() {
        let schema = parse_json(r#"{"type":"string","nullable":true}"#).unwrap();
        assert_eq!(validate(&schema, &JsValue::Null).unwrap(), None);
        assert_eq!(
            validate(&schema, &parse_json("5").unwrap())
                .unwrap()
                .as_deref(),
            Some("data must be string")
        );

        let schema = parse_json(
            r##"{"type":"object","properties":{"a":{"$ref":"#foo"}},"definitions":{"x":{"$id":"#foo","type":"string"}}}"##,
        )
        .unwrap();
        assert_eq!(
            validate(&schema, &parse_json(r#"{"a":1}"#).unwrap())
                .unwrap()
                .as_deref(),
            Some("data/a must be string")
        );
    }

    #[test]
    fn compile_errors_are_explicit() {
        for (schema, expected) in [
            (
                r#"{"type":"foo"}"#,
                "type must be JSONType or JSONType[]: foo",
            ),
            (r#"{"enum":[]}"#, "enum must have non-empty array"),
            (
                r#"{"$ref":"https://example.com/schema"}"#,
                "can't resolve reference https://example.com/schema from id #",
            ),
            (r#"{"maximum":"5"}"#, "maximum value must be [\"number\"]"),
            (r#"{"type":"string","nullable":false,"x":1}"#, ""),
        ] {
            if expected.is_empty() {
                assert!(check_schema(&parse_json(schema).unwrap()).is_ok());
            } else {
                assert_eq!(
                    check_schema(&parse_json(schema).unwrap()),
                    Err(SchemaError(expected.into())),
                    "{schema}"
                );
            }
        }
        let error =
            check_schema(&parse_json(r#"{"type":"string","pattern":"["}"#).unwrap()).unwrap_err();
        assert!(
            error.0.starts_with("Invalid regular expression"),
            "{error:?}"
        );
        let error = check_schema(&parse_json(r##"{"$ref":"#foo"}"##).unwrap()).unwrap_err();
        assert_eq!(error.0, "can't resolve reference #foo from id #");
    }

    #[test]
    fn deep_patterns_compile_on_a_worker_stack() {
        std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024)
            .spawn(|| {
                for depth in [10_000, 32_767, 32_768, 100_000] {
                    for open in ["(", "(?:", "(?="] {
                        let pattern = format!("{}{}", open.repeat(depth), ")".repeat(depth));
                        for (schema, data) in [
                            (serde_json::json!({"pattern": pattern}), r#""""#),
                            (
                                serde_json::json!({
                                    "patternProperties": {&pattern: {"type": "number"}}
                                }),
                                r#"{"x":1}"#,
                            ),
                        ] {
                            let schema = parse_json(&schema.to_string()).unwrap();
                            let data = parse_json(data).unwrap();
                            if open == "(" && depth > 32_767 {
                                let error = check_schema(&schema).unwrap_err();
                                assert!(error.0.ends_with(": Too many captures"));
                            } else {
                                assert!(check_schema(&schema).is_ok(), "{open} {depth}");
                            }
                            // Run the actual validation walk on THIS 2 MiB
                            // stack too; public validate has its own thread.
                            assert!(matches!(
                                validate_inner(&schema, &data),
                                Err(ValidationError::Unsupported(_))
                            ));
                            assert!(matches!(
                                validate(&schema, &data),
                                Err(ValidationError::Unsupported(_))
                            ));
                        }
                    }
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn deep_regex_formats_validate_on_a_worker_stack() {
        std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024)
            .spawn(|| {
                let schema = parse_json(r#"{"type":"string","format":"regex"}"#).unwrap();
                assert!(check_schema(&schema).is_ok());
                for depth in [10_000, 32_767, 32_768, 100_000] {
                    for open in ["(", "(?:", "(?="] {
                        let pattern = format!("{}{}", open.repeat(depth), ")".repeat(depth));
                        let data = JsValue::String(pattern.into());
                        let expected = if open == "(" && depth > 32_767 {
                            Some("data must match format \"regex\"".into())
                        } else {
                            None
                        };
                        assert_eq!(validate_inner(&schema, &data), Ok(expected.clone()));
                        assert_eq!(validate(&schema, &data), Ok(expected));
                    }
                }
                // The V8 bound counts all captures, not just nesting.
                for count in [32_767, 32_768] {
                    let flat = "()".repeat(count);
                    assert_eq!(regex_format_valid(&flat), Some(count == 32_767));
                    let identity = format!(r"\k<{}{}>", "(".repeat(count), ")".repeat(count));
                    assert_eq!(regex_format_valid(&identity), Some(count == 32_767));
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn group_stack_preserves_alternatives_assertions_and_quantifiers() {
        for (pattern, unicode_valid, non_unicode_valid) in [
            ("(|a|(?:b|))*", true, true),
            ("(?:(?=a)|(?<=b))+", true, true),
            ("(?=a)*", false, true),
            ("(?<=a)*", false, false),
            ("((a){2,3}?|b)+", true, true),
            ("(a)**", false, false),
            ("(?:(a)", false, false),
            ("a)", false, false),
            (r"[\(\)]\(\)", true, true),
            (r"\k<(>", false, false),
            (r"\k<[]>", false, true),
            (r"\k<x>(?<x>a)", true, true),
            (r"\k<y>(?<x>a)", false, false),
            (r"[\k](?<x>a)", false, false),
            (r"(?<x>a)[\k]", false, false),
            (r"\k<(?<x>a)>", false, false),
        ] {
            assert_eq!(
                parse_js_pattern(pattern).is_ok(),
                unicode_valid,
                "{pattern}"
            );
            assert_eq!(
                parse_js_pattern_non_unicode(pattern).is_ok(),
                non_unicode_valid,
                "{pattern}"
            );
        }
        for pattern in [r"^(?<$>x)$", r"^(?<\u0061>x)$", r"^(?<中>x)$"] {
            let schema = parse_json(&serde_json::json!({"pattern": pattern}).to_string()).unwrap();
            assert_eq!(
                validate_inner(&schema, &JsValue::String("x".into())),
                Ok(None)
            );
            assert!(
                validate_inner(&schema, &JsValue::String("y".into()))
                    .unwrap()
                    .is_some()
            );
        }
        assert!(matches!(js_regex(r"(?<$>x)\k<$>"), JsRegex::JsOnly));
    }

    #[test]
    fn deep_schema_anchor_collection_is_safe_on_a_worker_stack() {
        std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024)
            .spawn(|| {
                let source = format!("{}true{}", "{\"not\":".repeat(10_000), "}".repeat(10_000));
                let schema = parse_json(&source).unwrap();
                let error = check_schema(&schema).unwrap_err();
                assert!(error.0.contains("schema nesting is deeper"), "{error:?}");
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn grok_r3_invalid_u_patterns_are_schema_errors() {
        // The exact rows from Grok r3 P1-A. V8 compiles these with `/u`, so
        // `check_schema` must reproduce V8's message before Rust's parser can
        // accept `\-` as an escaped hyphen and `]`/`}` as literals.
        let cases: &[(&str, &str)] = &[
            (r"\-", "Invalid escape"),
            (r"\_", "Invalid escape"),
            (r"\#", "Invalid escape"),
            (r"\&", "Invalid escape"),
            (r"\~", "Invalid escape"),
            (r"\@", "Invalid escape"),
            (r"\%", "Invalid escape"),
            (r"\ ", "Invalid escape"),
            ("a]", "Lone quantifier brackets"),
            ("}", "Lone quantifier brackets"),
            ("a**", "Nothing to repeat"),
            (r"\<a", "Invalid escape"),
        ];
        for (pattern, detail) in cases {
            let mut object = JsObject::new();
            object.insert("type".into(), JsValue::String(JsString::from("string")));
            object.insert("pattern".into(), JsValue::String(JsString::from(*pattern)));
            assert_eq!(
                check_schema(&JsValue::Object(object)),
                Err(SchemaError(format!(
                    "Invalid regular expression: /{pattern}/u: {detail}"
                ))),
                "{pattern}"
            );
        }

        let mut child = JsObject::new();
        child.insert("type".into(), JsValue::String(JsString::from("number")));
        let mut properties = JsObject::new();
        properties.insert(r"\-".into(), JsValue::Object(child));
        let mut object = JsObject::new();
        object.insert("patternProperties".into(), JsValue::Object(properties));
        assert_eq!(
            check_schema(&JsValue::Object(object)),
            Err(SchemaError(
                r"Invalid regular expression: /\-/u: Invalid escape".into()
            ))
        );
    }

    #[test]
    fn unsupported_patterns_fail_at_validation_instead_of_passing() {
        // Schema compile follows Ajv: valid `/u` syntax is accepted even when
        // this port cannot evaluate it. The gap is explicit at validation.
        let schema = parse_json(r#"{"type":"string","pattern":"^(?=a)"}"#).unwrap();
        assert_eq!(check_schema(&schema), Ok(()));
        let error = validate(&schema, &parse_json(r#""a""#).unwrap()).unwrap_err();
        assert!(
            matches!(&error, ValidationError::Unsupported(message) if message.contains("JavaScript")),
            "{error:?}"
        );
    }

    #[test]
    fn common_ajv_patterns_are_not_false_rejected() {
        // r4 Grok P1 regression: these are all accepted by the real SDK.
        let patterns = [
            r"^\u00e9$",
            r"^[\u4e00-\u9fa5]+$",
            r"^[^\u0000-\u001f]*$",
            r"^\u{1F600}$",
            r"^\uD83D\uDE00$",
            r"^\x41$",
            r"^\cJ$",
            r"^\0$",
            r"^(?<year>\d{4})-(?<month>\d{2})$",
            r"^\p{L}+$",
            r"^[\p{L}]+$",
            r"^\p{Script=Han}+$",
            r"^\p{Lu}\p{Ll}+$",
            r"^[\w-]+$",
            r"^[\w.-]+$",
            r"^[a-z\s]+$",
            r"^[0-9\s]+$",
            r"^[-\d]+$",
            r"^[\d-]+$",
            r"^[\w\s-]+$",
            r"^[a-zA-Z0-9_\-]+$",
            r"^[\u4e00-\u9fa5\w]+$",
            r"^[^\u0000-\u007F]+$",
            r"^\u{1F600}-\u{1F64F}$",
            r"^[😀-😁]$",
            r"^\u{10FFFF}$",
            r"^(?:foo|bar)$",
            r"^\d{2,4}$",
            r"^[\-]?\d+$",
            r"^\/[a-z]+$",
            r"^\.$",
            r"^\$\^$",
            r"^(?<word>[A-Za-z]+)$",
            r"^\u{1F1E6}-\u{1F1FF}$",
            r"^[\p{Script=Han}\p{L}]+$",
            r"^\p{Emoji}$",
            // Valid but not evaluable; compile must still succeed.
            r"(?=a)",
            r"(?<=a)b",
            r"(?<x>a)\k<x>",
        ];
        for pattern in patterns {
            let mut object = JsObject::new();
            object.insert("type".into(), JsValue::String(JsString::from("string")));
            object.insert("pattern".into(), JsValue::String(JsString::from(pattern)));
            let schema = JsValue::Object(object);
            assert_eq!(check_schema(&schema), Ok(()), "{pattern}");
        }
    }

    #[test]
    fn pattern_properties_with_only_always_valid_children_skip_the_patterns() {
        // Ajv 8.20 `patternProperties.js:14-20` never compiles these keys.
        for schema in [
            r#"{"type":"object","patternProperties":{"\\-":{}}}"#,
            r#"{"type":"object","patternProperties":{"(?=a)":{}}}"#,
            r#"{"type":"object","patternProperties":{"\\-":true}}"#,
            r#"{"type":"object","patternProperties":{"\\-":{"title":"x"}}}"#,
            r#"{"type":"object","patternProperties":{"\\-":{}},"additionalProperties":true}"#,
            r#"{"type":"object","patternProperties":{"\\-":{}},"additionalProperties":{}}"#,
        ] {
            let schema = parse_json(schema).unwrap();
            assert_eq!(check_schema(&schema), Ok(()), "{schema:?}");
            assert_eq!(
                validate(&schema, &parse_json(r#"{"x":1}"#).unwrap()),
                Ok(None),
                "{schema:?}"
            );
        }
        // The sibling `additionalProperties: false` compiles the same keys in
        // `additionalProperties.js`, so this one must still fail.
        let schema = parse_json(
            r#"{"type":"object","patternProperties":{"\\-":{}},"additionalProperties":false}"#,
        )
        .unwrap();
        assert_eq!(
            check_schema(&schema),
            Err(SchemaError(
                r"Invalid regular expression: /\-/u: Invalid escape".into()
            ))
        );
    }

    #[test]
    fn formats_follow_ajv_formats() {
        for (format, value, expected) in [
            ("date", "2024-02-29", Some(true)),
            ("date", "2023-02-29", Some(false)),
            ("date", "2024-13-01", Some(false)),
            ("date-time", "2024-02-29T12:00:00Z", Some(true)),
            ("date-time", "2024-02-29 12:00:00Z", Some(true)),
            ("date-time", "nope", Some(false)),
            ("time", "12:00:00", Some(false)),
            ("time", "23:59:60Z", Some(true)),
            ("iso-time", "12:00:00", Some(true)),
            ("duration", "P1Y2M3DT4H5M6S", Some(true)),
            ("duration", "P", Some(false)),
            ("uri", "https://example.com/a", Some(true)),
            ("uri", "not a uri", Some(false)),
            ("uri-reference", "/a/b", Some(true)),
            ("uri-template", "{var}", Some(true)),
            ("url", "https://example.com/a?b=1", Some(true)),
            ("url", "http://192.168.1.1/x", Some(false)),
            ("email", "a.b@example.com", Some(true)),
            ("email", "User@Example.com", Some(true)),
            ("email", "a..b@example.com", Some(false)),
            ("email", "a@localhost", Some(false)),
            ("hostname", "a-b.example.", Some(true)),
            ("hostname", "-a.example", Some(false)),
            ("ipv4", "192.168.0.1", Some(true)),
            ("ipv4", "01.1.1.1", Some(false)),
            ("ipv4", "256.1.1.1", Some(false)),
            ("ipv6", "::1", Some(true)),
            ("ipv6", "1.2.3.4", Some(false)),
            (
                "uuid",
                "urn:uuid:123e4567-e89b-12d3-a456-426614174000",
                Some(true),
            ),
            ("uuid", "123e4567e89b12d3a456426614174000", Some(false)),
            ("json-pointer", "/a~1b", Some(true)),
            ("relative-json-pointer", "0/a", Some(true)),
            ("byte", "aGVsbG8=", Some(true)),
            ("byte", "aGVsbG8", Some(false)),
            ("regex", "^a+$", Some(true)),
            ("regex", "[", Some(false)),
            ("regex", "(?=a)", Some(true)),
            // ajv-formats compiles `regex` values without `/u`; these are
            // valid there even though the same text is not a valid
            // outputSchema `/u` pattern.
            ("regex", "\\-", Some(true)),
            ("regex", "a]", Some(true)),
            ("regex", "\\<a", Some(true)),
            ("regex", "(?i:a)", Some(false)),
            // r4 P1: JS-invalid patterns Rust's parser accepts, and
            // Annex B patterns Rust's parser rejects.
            ("regex", "a**", Some(false)),
            ("regex", "x{2}{3}", Some(false)),
            ("regex", "a{,5}", Some(true)),
            ("regex", "\\cJ", Some(true)),
            ("regex", "\\0", Some(true)),
            ("regex", "\\k", Some(true)),
            ("regex", "\\Z", Some(true)),
            ("regex", "a\\Z", Some(false)),
            ("regex", "a\\\\Z", Some(true)),
            ("regex", "\\p{L}", Some(true)),
            ("regex", "(?<x>a)", Some(true)),
            ("regex", "(?<x>a)\\k<x>", Some(true)),
            ("regex", "(?<x>a)\\k<y>", Some(false)),
            ("regex", "[]]", Some(true)),
            ("regex", "[a-\\w]", Some(true)),
            ("regex", "a*{x}", Some(true)),
            ("regex", "a*{2}", Some(false)),
            ("regex", "(?<=a)*", Some(false)),
            ("regex", "(?=a)*", Some(true)),
            ("password", "anything", Some(true)),
            ("binary", "anything", Some(true)),
            ("int32", "3000000000", None),
            ("uri", "x", Some(false)),
        ] {
            assert_eq!(
                string_format_valid(format, value),
                expected,
                "{format} {value}"
            );
        }
        assert_eq!(string_format_valid("whatever", "x"), None);
        assert_eq!(number_format_valid("int32", 42.0), Some(true));
        assert_eq!(number_format_valid("int32", 3_000_000_000.0), Some(false));
        assert_eq!(number_format_valid("int64", 1.5), Some(false));
        assert_eq!(number_format_valid("float", 1.5e300), Some(true));
    }

    /// Generated by `tests/fixtures/output-schema/generate.mjs` against the
    /// real SDK 1.29.0 `AjvJsonSchemaValidator`.
    #[test]
    fn replays_the_ajv_oracle() {
        let oracle = super::baseline::oracle(include_str!(
            "../../../tests/fixtures/output-schema/ajv-oracle.json"
        ));
        let cases = match oracle.get("cases").unwrap() {
            JsValue::Array(cases) => cases,
            other => panic!("cases must be an array: {other:?}"),
        };
        assert_eq!(cases.len(), 563);
        for (key, expected) in [
            ("sdk", "1.29.0"),
            ("ajv", "8.20.0"),
            ("ajvFormats", "3.0.1"),
        ] {
            assert!(
                oracle
                    .get(key)
                    .unwrap()
                    .as_string()
                    .unwrap()
                    .equals_str(expected),
                "{key}"
            );
        }
        for key in ["providerSha256", "ajvSha256", "formatsSha256"] {
            let hash = oracle
                .get(key)
                .unwrap()
                .as_string()
                .unwrap()
                .to_utf8()
                .unwrap();
            assert!(
                hash.len() == 64 && hash.bytes().all(|ch| ch.is_ascii_hexdigit()),
                "{key}"
            );
        }
        for case in cases {
            let id = case.get("id").unwrap().as_string().unwrap();
            let id = String::from_utf16_lossy(id.units());
            let schema = case.get("schema").unwrap();
            let data = case.get("data").unwrap();
            if case.get("portUnsupported").and_then(JsValue::as_bool) == Some(true) {
                // "Unsupported" is a port gap only for patterns V8 accepts.
                // Schema compile must accept it (Ajv does), and using it must
                // fail explicitly, never compile into a permissive validator
                // or silently pass data.
                assert!(
                    check_schema(schema).is_ok(),
                    "{id}: valid /u pattern was rejected by check_schema"
                );
                assert!(
                    matches!(validate(schema, data), Err(ValidationError::Unsupported(_))),
                    "{id}"
                );
                continue;
            }
            if let Some(message) = case.get("compileError").and_then(JsValue::as_string) {
                assert_eq!(
                    check_schema(schema),
                    Err(SchemaError(String::from_utf16_lossy(message.units()))),
                    "{id}"
                );
                continue;
            }
            if let Some(prefix) = case.get("compileErrorPrefix").and_then(JsValue::as_string) {
                let error = check_schema(schema).unwrap_err();
                let prefix = String::from_utf16_lossy(prefix.units());
                assert!(error.0.starts_with(&prefix), "{id}: {error:?}");
                if let Some(detail) = case.get("compileErrorDetail").and_then(JsValue::as_string) {
                    let detail = String::from_utf16_lossy(detail.units());
                    assert!(error.0.ends_with(&format!(": {detail}")), "{id}: {error:?}");
                    if case.get("exactCompileError").and_then(JsValue::as_bool) == Some(true) {
                        let actual = case
                            .get("actualCompileError")
                            .and_then(JsValue::as_string)
                            .expect("exact cases record the real Ajv message");
                        assert_eq!(
                            error.0,
                            String::from_utf16_lossy(actual.units()),
                            "{id}: whole V8 message"
                        );
                    }
                }
                continue;
            }
            let expected = case
                .get("errorText")
                .and_then(JsValue::as_string)
                .map(|s| String::from_utf16_lossy(s.units()));
            assert_eq!(validate(schema, data).unwrap(), expected, "{id}");
        }
    }
}
