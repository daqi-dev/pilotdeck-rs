//! Draft 2020-12 form evaluation over exact JavaScript UTF-16 values.
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::collections::HashSet;
const MAX_WORK: usize = 100_000;
const MAX_DEPTH: usize = 128;
const KEYS: &[&str] = &[
    "$schema",
    "$id",
    "$ref",
    "$dynamicRef",
    "$dynamicAnchor",
    "$recursiveRef",
    "$recursiveAnchor",
    "$defs",
    "$comment",
    "type",
    "nullable",
    "title",
    "description",
    "default",
    "examples",
    "readOnly",
    "writeOnly",
    "deprecated",
    "enum",
    "const",
    "multipleOf",
    "maximum",
    "exclusiveMaximum",
    "minimum",
    "exclusiveMinimum",
    "maxLength",
    "minLength",
    "pattern",
    "format",
    "maxItems",
    "minItems",
    "uniqueItems",
    "maxContains",
    "minContains",
    "prefixItems",
    "items",
    "contains",
    "maxProperties",
    "minProperties",
    "required",
    "properties",
    "patternProperties",
    "additionalProperties",
    "dependentRequired",
    "dependentSchemas",
    "propertyNames",
    "if",
    "then",
    "else",
    "allOf",
    "anyOf",
    "oneOf",
    "not",
    "unevaluatedItems",
    "unevaluatedProperties",
    "contentEncoding",
    "contentMediaType",
    "contentSchema",
];
fn obj(v: &JsValue) -> Option<&JsObject> {
    v.as_object()
}
fn text<'a>(v: &'a JsValue, key: &str) -> Option<&'a JsString> {
    v.get(key).and_then(JsValue::as_string)
}
fn num(v: &JsValue, key: &str) -> Option<f64> {
    v.get(key).and_then(JsValue::as_number)
}
fn exists(v: &JsValue, key: &str) -> bool {
    v.get(key).is_some_and(|x| !x.is_undefined())
}
fn compile_error(msg: impl AsRef<str>) -> String {
    format!(
        "schema must be a valid local Draft 2020-12 JSON Schema: {}",
        msg.as_ref()
    )
}
fn source(v: &JsString) -> Vec<u32> {
    char::decode_utf16(v.units().iter().copied())
        .map(|r| match r {
            Ok(c) => u32::from(c),
            Err(e) => u32::from(e.unpaired_surrogate()),
        })
        .collect()
}
fn compile_regex(pattern: &JsString) -> Result<regress::Regex, String> {
    if pattern.len_utf16() > 4096 {
        return Err("form pattern exceeds the native limit".into());
    }
    let mut depth = 0usize;
    let mut escape = false;
    let mut class = false;
    for c in pattern.units() {
        if escape {
            escape = false;
            continue;
        }
        match *c {
            0x5c => escape = true,
            0x5b if !class => class = true,
            0x5d if class => class = false,
            0x28 if !class => {
                depth += 1;
                if depth > 128 {
                    return Err("form pattern nesting exceeds the native limit".into());
                }
            }
            0x29 if !class => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    regress::Regex::from_unicode(source(pattern).into_iter(), "u")
        .map_err(|_| "invalid regular expression".into())
}
fn find_external(value: &JsValue, path: &str, depth: usize) -> Option<String> {
    if depth > MAX_DEPTH {
        return Some(format!("{path} exceeds the native nesting limit"));
    }
    match value {
        JsValue::Array(items) => {
            for (i, v) in items.iter().enumerate() {
                if let Some(e) = find_external(v, &format!("{path}[{i}]"), depth + 1) {
                    return Some(e);
                }
            }
        }
        JsValue::Object(o) => {
            for (k, v) in o.own_entries() {
                let key = String::from_utf16_lossy(k.units());
                let child = format!("{path}.{key}");
                if ["$ref", "$dynamicRef", "$recursiveRef"].contains(&key.as_str())
                    && v.as_string()
                        .is_some_and(|s| !s.units().starts_with(&[u16::from(b'#')]))
                {
                    return Some(format!(
                        "{child} must use a local # reference; Gateway form dialogs never fetch remote schemas."
                    ));
                }
                if let Some(e) = find_external(v, &child, depth + 1) {
                    return Some(e);
                }
            }
        }
        _ => {}
    };
    None
}
fn local_ref<'a>(root: &'a JsValue, r: &JsString) -> Option<&'a JsValue> {
    let raw = r.to_utf8().ok()?;
    if raw == "#" {
        return Some(root);
    }
    let path = raw.strip_prefix("#/")?;
    let mut current = root;
    for segment in path.split('/') {
        let bytes = segment.as_bytes();
        let mut decoded = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' {
                if i + 2 >= bytes.len() {
                    return None;
                }
                let hex = |b: u8| match b {
                    b'0'..=b'9' => Some(b - b'0'),
                    b'a'..=b'f' => Some(b - b'a' + 10),
                    b'A'..=b'F' => Some(b - b'A' + 10),
                    _ => None,
                };
                decoded.push(hex(bytes[i + 1])? * 16 + hex(bytes[i + 2])?);
                i += 3;
            } else {
                decoded.push(bytes[i]);
                i += 1;
            }
        }
        let segment = String::from_utf8(decoded).ok()?;
        let mut plain = String::new();
        let mut chars = segment.chars();
        while let Some(c) = chars.next() {
            if c == '~' {
                match chars.next()? {
                    '0' => plain.push('~'),
                    '1' => plain.push('/'),
                    _ => return None,
                }
            } else {
                plain.push(c);
            }
        }
        current = match current {
            JsValue::Object(o) => o.get(&plain)?,
            JsValue::Array(a) => a.get(plain.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(current)
}
fn compile_node<'a>(
    value: &'a JsValue,
    resource: &'a JsValue,
    path: &str,
    depth: usize,
    nodes: &mut usize,
    anchors: &mut Vec<(usize, JsString, String)>,
    inherited: Option<&str>,
) -> Result<(), String> {
    *nodes += 1;
    if *nodes > 100_000 || depth > MAX_DEPTH {
        return Err(compile_error("schema exceeds the native validation budget"));
    }
    if value.as_bool().is_some() {
        return Ok(());
    }
    let Some(o) = obj(value) else {
        return Err(compile_error("schema must be object or boolean"));
    };
    for (k, _) in o.own_entries() {
        let k = k.to_utf8().map_err(|_| compile_error("invalid keyword"))?;
        if !KEYS.contains(&k.as_str()) {
            return Err(compile_error(format!(
                "strict mode: unknown keyword: \"{k}\""
            )));
        }
    }
    let resource = if exists(value, "$id") {
        value
    } else {
        resource
    };
    let own_type = text(value, "type")
        .and_then(|s| s.to_utf8().ok())
        .or_else(|| match value.get("type") {
            Some(JsValue::Array(a)) => a
                .iter()
                .filter_map(JsValue::as_string)
                .find(|s| !s.equals_str("null"))
                .and_then(|s| s.to_utf8().ok()),
            _ => None,
        });
    let context = own_type.as_deref().or(inherited);
    if let Some(kind) = value.get("type") {
        let valid = |v: &JsValue| {
            v.as_string().is_some_and(|s| {
                [
                    "object", "array", "string", "number", "integer", "boolean", "null",
                ]
                .iter()
                .any(|k| s.equals_str(k))
            })
        };
        let good = match kind {
            JsValue::Array(a) => !a.is_empty() && a.iter().all(valid),
            _ => valid(kind),
        };
        if !good {
            return Err(compile_error(format!(
                "schema is invalid: data{}/type must be equal to one of the allowed values, data{}/type must be array, data{}/type must match a schema in anyOf",
                &path[1..],
                &path[1..],
                &path[1..]
            )));
        }
        if let JsValue::Array(a) = kind {
            let mut seen = std::collections::HashMap::new();
            for (i, v) in a.iter().enumerate() {
                let k = v.as_string().unwrap();
                if let Some(j) = seen.insert(k.clone(), i) {
                    return Err(compile_error(format!(
                        "schema is invalid: data{}/type must be equal to one of the allowed values, data{}/type must NOT have duplicate items (items ## {j} and {i} are identical), data{}/type must match a schema in anyOf",
                        &path[1..],
                        &path[1..],
                        &path[1..]
                    )));
                }
            }
            if a.iter()
                .filter(|v| v.as_string().is_some_and(|s| !s.equals_str("null")))
                .count()
                > 1
            {
                return Err(compile_error(format!(
                    "strict mode: use allowUnionTypes to allow union type keyword at \"{path}\" (strictTypes)"
                )));
            }
        }
    }
    for key in [
        "minLength",
        "maxLength",
        "minItems",
        "maxItems",
        "minProperties",
        "maxProperties",
        "minContains",
        "maxContains",
    ] {
        if exists(value, key) && num(value, key).is_none_or(|n| !n.is_finite() || n.fract() != 0.0)
        {
            return Err(compile_error(format!(
                "schema is invalid: data{}/{key} must be integer",
                &path[1..]
            )));
        }
        if num(value, key).is_some_and(|n| n < 0.0) {
            return Err(compile_error(format!(
                "schema is invalid: data{}/{key} must be >= 0",
                &path[1..]
            )));
        }
    }
    for key in [
        "minimum",
        "maximum",
        "exclusiveMinimum",
        "exclusiveMaximum",
        "multipleOf",
    ] {
        if exists(value, key) && num(value, key).is_none_or(|n| !n.is_finite()) {
            return Err(compile_error(format!(
                "schema is invalid: data{}/{key} must be number",
                &path[1..]
            )));
        }
    }
    if num(value, "multipleOf").is_some_and(|n| n <= 0.0) {
        return Err(compile_error("multipleOf must be greater than zero"));
    }
    for key in [
        "nullable",
        "uniqueItems",
        "readOnly",
        "writeOnly",
        "deprecated",
    ] {
        if exists(value, key) && value.get(key).and_then(JsValue::as_bool).is_none() {
            return Err(compile_error(format!("{key} must be boolean")));
        }
    }
    for key in [
        "$schema",
        "$id",
        "$dynamicRef",
        "$dynamicAnchor",
        "$recursiveRef",
        "$comment",
        "title",
        "description",
        "pattern",
        "format",
        "contentEncoding",
        "contentMediaType",
    ] {
        if exists(value, key) && text(value, key).is_none() {
            return Err(compile_error(format!("{key} must be string")));
        }
    }
    if let Some(enumeration) = value.get("enum")
        && !matches!(enumeration,JsValue::Array(a) if !a.is_empty()) {
            return Err(compile_error("enum must have non-empty array"));
        }
    if let Some(deps) = value.get("dependentRequired") {
        let deps = obj(deps).ok_or_else(|| compile_error("dependentRequired must be object"))?;
        for (name, required) in deps.own_entries() {
            if !matches!(required,JsValue::Array(a) if a.iter().all(|v|v.as_string().is_some())) {
                return Err(compile_error(
                    "dependentRequired values must be arrays of strings",
                ));
            }
            if let JsValue::Array(a) = required {
                let mut seen = std::collections::HashMap::new();
                for (i, v) in a.iter().enumerate() {
                    let k = v.as_string().unwrap();
                    if let Some(j) = seen.insert(k.clone(), i) {
                        return Err(compile_error(format!(
                            "schema is invalid: data{}/dependentRequired/{} must NOT have duplicate items (items ## {i} and {j} are identical)",
                            &path[1..],
                            String::from_utf16_lossy(name.units())
                        )));
                    }
                }
            }
        }
    }
    if let Some(anchor) = text(value, "$dynamicAnchor") {
        let Ok(anchor) = anchor.to_utf8() else {
            return Err(compile_error(
                "schema is invalid: data/$dynamicAnchor must match pattern \"^[A-Za-z_][-A-Za-z0-9._]*$\"",
            ));
        };
        let mut chars = anchor.chars();
        if !chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            || !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        {
            return Err(compile_error(
                "schema is invalid: data/$dynamicAnchor must match pattern \"^[A-Za-z_][-A-Za-z0-9._]*$\"",
            ));
        }
    }
    if let Some(anchor) = text(value, "$dynamicAnchor") {
        let id = resource as *const JsValue as usize;
        if anchors.iter().any(|(r, a, previous)| {
            *r == id
                && a == anchor
                && !(path.len() > previous.len()
                    && path.starts_with(previous)
                    && path.as_bytes().get(previous.len()) == Some(&b'/'))
        }) {
            return Err(compile_error(format!(
                "reference \"#{}\" resolves to more than one schema",
                String::from_utf16_lossy(anchor.units())
            )));
        }
        anchors.push((id, anchor.clone(), path.to_owned()));
    }
    if exists(value, "$recursiveAnchor") && text(value, "$recursiveAnchor").is_none() {
        return Err(compile_error(
            "schema is invalid: data/$recursiveAnchor must be string",
        ));
    }
    if let Some(r) = value.get("$ref") {
        let Some(r) = r.as_string() else {
            return Err(compile_error("$ref must be string"));
        };
        if local_ref(resource, r).is_none() {
            let reference = String::from_utf16_lossy(r.units());
            let id = text(resource, "$id")
                .map(|v| String::from_utf16_lossy(v.units()))
                .unwrap_or_else(|| "#".into());
            return Err(compile_error(format!(
                "can't resolve reference {reference} from id {id}"
            )));
        }
    }
    if let Some(JsValue::Array(required)) = value.get("required") {
        let props = value.get("properties").and_then(obj);
        let mut seen: std::collections::HashMap<JsString, usize> = std::collections::HashMap::new();
        for (index, key) in required.iter().enumerate() {
            let Some(key) = key.as_string() else {
                return Err(compile_error("required must be array of strings"));
            };
            if let Some(previous) = seen.insert(key.clone(), index) {
                return Err(compile_error(format!(
                    "schema is invalid: data{}/required must NOT have duplicate items (items ## {index} and {previous} are identical)",
                    &path[1..]
                )));
            }
            if props.is_none_or(|p| p.get_key(key).is_none()) {
                return Err(compile_error(format!(
                    "strict mode: required property \"{}\" is not defined at \"{path}\" (strictRequired)",
                    String::from_utf16_lossy(key.units())
                )));
            }
        }
    } else if exists(value, "required") {
        return Err(compile_error("required must be array"));
    }
    if !exists(value, "contains") && (exists(value, "maxContains") || exists(value, "minContains"))
    {
        let name = if exists(value, "maxContains") {
            "maxContains"
        } else {
            "minContains"
        };
        return Err(compile_error(format!(
            "strict mode: \"{name}\" without \"contains\" is ignored"
        )));
    }
    if num(value, "minContains") == Some(0.0)
        && !exists(value, "maxContains")
        && exists(value, "contains")
    {
        return Err(compile_error(
            "strict mode: \"minContains\" == 0 without \"maxContains\": \"contains\" keyword ignored",
        ));
    }
    if value.get("nullable").and_then(JsValue::as_bool) == Some(false)
        && matches!(value.get("type"),Some(JsValue::Array(a)) if a.iter().any(|v|v.as_string().is_some_and(|s|s.equals_str("null"))))
    {
        return Err(compile_error("type: null contradicts nullable: false"));
    }
    if value.get("nullable").and_then(JsValue::as_bool) == Some(true) && !exists(value, "type") {
        return Err(compile_error(
            "\"nullable\" cannot be used without \"type\"",
        ));
    }
    if let Some(pattern) = text(value, "pattern") {
        compile_regex(pattern).map_err(compile_error)?;
    }
    if let Some(fmt) = text(value, "format") {
        let f = fmt.to_utf8().map_err(|_| compile_error("invalid format"))?;
        if ![
            "date",
            "time",
            "date-time",
            "iso-time",
            "iso-date-time",
            "duration",
            "uri",
            "uri-reference",
            "uri-template",
            "url",
            "email",
            "hostname",
            "ipv4",
            "ipv6",
            "uuid",
            "json-pointer",
            "json-pointer-uri-fragment",
            "relative-json-pointer",
            "regex",
            "byte",
            "password",
            "binary",
            "int32",
            "int64",
            "float",
            "double",
        ]
        .contains(&f.as_str())
        {
            return Err(compile_error(format!(
                "unknown format \"{f}\" ignored in schema at path \"{path}\""
            )));
        }
    }
    for (key, need) in [
        ("properties", "object"),
        ("patternProperties", "object"),
        ("additionalProperties", "object"),
        ("dependentRequired", "object"),
        ("dependentSchemas", "object"),
        ("required", "object"),
        ("propertyNames", "object"),
        ("minProperties", "object"),
        ("maxProperties", "object"),
        ("pattern", "string"),
        ("minLength", "string"),
        ("maxLength", "string"),
        ("format", "string"),
        ("items", "array"),
        ("prefixItems", "array"),
        ("contains", "array"),
        ("minItems", "array"),
        ("maxItems", "array"),
        ("uniqueItems", "array"),
        ("minimum", "number"),
        ("maximum", "number"),
        ("exclusiveMinimum", "number"),
        ("exclusiveMaximum", "number"),
        ("multipleOf", "number"),
    ] {
        if exists(value, key)
            && !context.is_some_and(|t| {
                t == need
                    || (need == "number" && t == "integer")
                    || (key == "format"
                        && ["int32", "int64", "float", "double"]
                            .iter()
                            .any(|n| text(value, "format").is_some_and(|f| f.equals_str(n)))
                        && (t == "number" || t == "integer"))
            })
        {
            return Err(compile_error(format!(
                "strict mode: missing type \"{need}\" for keyword \"{key}\" at \"{path}\" (strictTypes)"
            )));
        }
    }
    if (exists(value, "then") || exists(value, "else")) && !exists(value, "if") {
        let keyword = if exists(value, "then") {
            "then"
        } else {
            "else"
        };
        return Err(compile_error(format!(
            "strict mode: \"{keyword}\" without \"if\" is ignored"
        )));
    }
    if exists(value, "if") && !exists(value, "then") && !exists(value, "else") {
        return Err(compile_error(
            "strict mode: \"if\" without \"then\" and \"else\" is ignored",
        ));
    }
    for key in [
        "$defs",
        "properties",
        "patternProperties",
        "dependentSchemas",
    ] {
        if let Some(v) = value.get(key) {
            let m = obj(v).ok_or_else(|| compile_error(format!("{key} must be object")))?;
            for (k, child) in m.own_entries() {
                if key == "patternProperties" {
                    compile_regex(k).map_err(compile_error)?;
                }
                compile_node(
                    child,
                    resource,
                    &format!("{path}/{key}/{}", String::from_utf16_lossy(k.units())),
                    depth + 1,
                    nodes,
                    anchors,
                    if key == "dependentSchemas" {
                        context
                    } else {
                        None
                    },
                )?;
            }
        }
    }
    for key in [
        "items",
        "contains",
        "propertyNames",
        "additionalProperties",
        "unevaluatedProperties",
        "unevaluatedItems",
        "not",
        "if",
        "then",
        "else",
        "contentSchema",
    ] {
        if let Some(child) = value.get(key) {
            compile_node(
                child,
                resource,
                &format!("{path}/{key}"),
                depth + 1,
                nodes,
                anchors,
                if ["if", "then", "else", "not"].contains(&key) {
                    context
                } else {
                    None
                },
            )?;
        }
    }
    for key in ["allOf", "anyOf", "oneOf", "prefixItems"] {
        if let Some(v) = value.get(key) {
            let JsValue::Array(a) = v else {
                return Err(compile_error(format!("{key} must be array")));
            };
            if a.is_empty() {
                return Err(compile_error(format!(
                    "{key} must NOT have fewer than 1 items"
                )));
            }
            for (i, child) in a.iter().enumerate() {
                compile_node(
                    child,
                    resource,
                    &format!("{path}/{key}/{i}"),
                    depth + 1,
                    nodes,
                    anchors,
                    if key == "prefixItems" { None } else { context },
                )?;
            }
        }
    }
    Ok(())
}
pub(super) fn validate(schema: &JsValue) -> Result<(), String> {
    if let Some(error) = find_external(schema, "$", 0) {
        return Err(error);
    }
    compile_node(
        schema,
        schema,
        "#",
        0,
        &mut 0,
        &mut Vec::new(),
        None,
    )
}
#[derive(Default)]
struct Mark {
    properties: HashSet<JsString>,
    items: HashSet<usize>,
}
impl Mark {
    fn merge(&mut self, other: Self) {
        self.properties.extend(other.properties);
        self.items.extend(other.items);
    }
}
struct Eval<'a> {
    root: &'a JsValue,
    work: usize,
    active: Vec<(usize, usize)>,
    resources: Vec<&'a JsValue>,
    anchors: std::collections::HashMap<JsString, &'a JsValue>,
    exhausted: bool,
}
impl<'a> Eval<'a> {
    fn charge(&mut self, amount: usize) -> bool {
        self.work = self.work.saturating_add(amount);
        if self.work > MAX_WORK {
            self.exhausted = true;
            false
        } else {
            true
        }
    }
    fn regex(&mut self, pattern: &JsString, subject: &JsString) -> Option<bool> {
        if !self.charge(2048) || subject.len_utf16() > 1_048_576 {
            self.exhausted = true;
            return None;
        }
        let regex = match compile_regex(pattern) {
            Ok(r) => r,
            Err(_) => {
                self.exhausted = true;
                return None;
            }
        };
        match regex.find_from_utf16_budgeted(subject.units(), 0, 10_000) {
            regress::BudgetedMatch::Match(_) => Some(true),
            regress::BudgetedMatch::NoMatch => Some(false),
            regress::BudgetedMatch::BudgetExhausted => {
                self.exhausted = true;
                None
            }
        }
    }
    fn equal(&mut self, a: &JsValue, b: &JsValue, depth: usize) -> bool {
        if depth > MAX_DEPTH || !self.charge(1) {
            self.exhausted = true;
            return false;
        }
        match (a, b) {
            (JsValue::Array(a), JsValue::Array(b)) => {
                a.len() == b.len() && a.iter().zip(b).all(|(a, b)| self.equal(a, b, depth + 1))
            }
            (JsValue::Object(a), JsValue::Object(b)) => {
                let entries = a.own_entries();
                entries.len() == b.own_entries().len()
                    && entries.iter().all(|(k, v)| {
                        b.get_key(k)
                            .is_some_and(|other| self.equal(v, other, depth + 1))
                    })
            }
            _ => a == b,
        }
    }
    fn check(&mut self, schema: &'a JsValue, value: &JsValue, depth: usize) -> Option<Mark> {
        if depth > MAX_DEPTH || !self.charge(1) {
            self.exhausted = true;
            return None;
        }
        if let Some(b) = schema.as_bool() {
            return b.then(Mark::default);
        }
        obj(schema)?;
        let pair = (
            schema as *const JsValue as usize,
            value as *const JsValue as usize,
        );
        if self.active.contains(&pair) {
            self.exhausted = true;
            return None;
        }
        self.active.push(pair);
        let new_resource = exists(schema, "$id") && !std::ptr::eq(schema, self.root);
        if new_resource {
            self.resources.push(schema);
        }
        if let Some(anchor) = text(schema, "$dynamicAnchor") {
            self.anchors.entry(anchor.clone()).or_insert(schema);
        }
        let result = self.body(schema, value, depth);
        if new_resource {
            self.resources.pop();
        }
        self.active.pop();
        result
    }
    fn body(&mut self, schema: &'a JsValue, value: &JsValue, depth: usize) -> Option<Mark> {
        let mut mark = Mark::default();
        if let Some(r) = text(schema, "$ref") {
            let resource = self.resources.last().copied().unwrap_or(self.root);
            let target = local_ref(resource, r)?;
            let nested = self.check(target, value, depth + 1)?;
            mark.merge(nested);
        }
        if let Some(reference) = text(schema, "$dynamicRef") {
            let target = reference
                .to_utf8()
                .ok()
                .and_then(|r| r.strip_prefix('#').map(str::to_owned))
                .and_then(|name| self.anchors.get(&JsString::from(name.as_str())).copied())
                .unwrap_or(self.root);
            let nested = self.check(target, value, depth + 1)?;
            mark.merge(nested);
        }
        if let Some(types) = schema.get("type") {
            let fits = match types {
                JsValue::String(s) => type_match(value, s),
                JsValue::Array(a) => a
                    .iter()
                    .any(|t| t.as_string().is_some_and(|s| type_match(value, s))),
                _ => false,
            };
            if !fits
                && !(matches!(value, JsValue::Null)
                    && schema.get("nullable").and_then(JsValue::as_bool) == Some(true))
            {
                return None;
            }
        }
        if schema
            .get("const")
            .is_some_and(|c| !self.equal(c, value, 0))
        {
            return None;
        }
        if let Some(JsValue::Array(items)) = schema.get("enum")
            && !items.iter().any(|c| self.equal(c, value, 0)) {
                return None;
            }
        if let Some(v) = value.as_string() {
            let len = char::decode_utf16(v.units().iter().copied()).count() as f64;
            if num(schema, "minLength").is_some_and(|n| len < n)
                || num(schema, "maxLength").is_some_and(|n| len > n)
            {
                return None;
            }
            if let Some(p) = text(schema, "pattern")
                && !self.regex(p, v)? {
                    return None;
                }
            if let Some(f) = text(schema, "format") {
                let f = f.to_utf8().ok()?;
                if f == "regex" {
                    // ajv-formats rejects a non-escaped \Z after any preceding unit.
                    let u = v.units();
                    if (1..u.len().saturating_sub(1))
                        .any(|i| u[i] == 92 && u[i + 1] == 90 && u[i - 1] != 92)
                    {
                        return None;
                    }
                    if super::form_schema::legacy_regex(v).is_err() {
                        return None;
                    }
                } else if !["int32", "int64", "float", "double", "password", "binary"]
                    .contains(&f.as_str())
                {
                    let v = v.to_utf8().ok()?;
                    if !crate::runtime::mcp::output_schema::string_format_valid(&f, &v)? {
                        return None;
                    }
                }
            }
        }
        if let Some(n) = value.as_number() {
            if let Some(f) = text(schema, "format").and_then(|f| f.to_utf8().ok())
                && crate::runtime::mcp::output_schema::number_format_valid(&f, n) == Some(false) {
                    return None;
                }
            if num(schema, "minimum").is_some_and(|x| n < x)
                || num(schema, "maximum").is_some_and(|x| n > x)
                || num(schema, "exclusiveMinimum").is_some_and(|x| n <= x)
                || num(schema, "exclusiveMaximum").is_some_and(|x| n >= x)
                || num(schema, "multipleOf").is_some_and(|x| x <= 0.0 || (n / x).fract() != 0.0)
            {
                return None;
            }
        }
        if let Some(map) = obj(value) {
            let entries = map.own_entries();
            let len = entries.len() as f64;
            if num(schema, "minProperties").is_some_and(|n| len < n)
                || num(schema, "maxProperties").is_some_and(|n| len > n)
            {
                return None;
            }
            if let Some(JsValue::Array(required)) = schema.get("required")
                && required
                    .iter()
                    .any(|k| k.as_string().is_some_and(|k| map.get_key(k).is_none()))
                {
                    return None;
                }
            let props = schema.get("properties").and_then(obj);
            let patterns = schema.get("patternProperties").and_then(obj);
            for (key, item) in &entries {
                let mut matched = false;
                if let Some(child) = props.and_then(|p| p.get_key(key)) {
                    self.check(child, item, depth + 1)?;
                    mark.properties.insert((*key).clone());
                    matched = true;
                }
                if let Some(patterns) = patterns {
                    for (pattern, child) in patterns.own_entries() {
                        if self.regex(pattern, key)? {
                            self.check(child, item, depth + 1)?;
                            mark.properties.insert((*key).clone());
                            matched = true;
                        }
                    }
                }
                if !matched
                    && let Some(additional) = schema.get("additionalProperties") {
                        self.check(additional, item, depth + 1)?;
                        mark.properties.insert((*key).clone());
                    }
                if let Some(names) = schema.get("propertyNames") {
                    self.check(names, &JsValue::String((*key).clone()), depth + 1)?;
                }
            }
            if let Some(deps) = schema.get("dependentRequired").and_then(obj) {
                for (key, required) in deps.own_entries() {
                    if map.get_key(key).is_some() {
                        let JsValue::Array(a) = required else {
                            return None;
                        };
                        if a.iter()
                            .any(|k| k.as_string().is_some_and(|k| map.get_key(k).is_none()))
                        {
                            return None;
                        }
                    }
                }
            }
            if let Some(deps) = schema.get("dependentSchemas").and_then(obj) {
                for (key, child) in deps.own_entries() {
                    if map.get_key(key).is_some() {
                        let nested = self.check(child, value, depth + 1)?;
                        mark.merge(nested);
                    }
                }
            }
        }
        if let JsValue::Array(items) = value {
            let len = items.len() as f64;
            if num(schema, "minItems").is_some_and(|n| len < n)
                || num(schema, "maxItems").is_some_and(|n| len > n)
            {
                return None;
            }
            if schema.get("uniqueItems").and_then(JsValue::as_bool) == Some(true) {
                for (i, v) in items.iter().enumerate() {
                    for prev in &items[..i] {
                        if self.equal(v, prev, 0) || self.exhausted {
                            return None;
                        }
                    }
                }
            }
            let prefix = match schema.get("prefixItems") {
                Some(JsValue::Array(a)) => a.as_slice(),
                _ => &[],
            };
            for (i, item) in items.iter().enumerate() {
                if let Some(child) = prefix.get(i).or_else(|| {
                    if i >= prefix.len() {
                        schema.get("items")
                    } else {
                        None
                    }
                }) {
                    self.check(child, item, depth + 1)?;
                    mark.items.insert(i);
                }
            }
            if let Some(contains) = schema.get("contains") {
                let mut count = 0usize;
                for item in items {
                    if self.check(contains, item, depth + 1).is_some() {
                        count += 1;
                    }
                    if self.exhausted {
                        return None;
                    }
                }
                if (count as f64) < num(schema, "minContains").unwrap_or(1.0)
                    || num(schema, "maxContains").is_some_and(|n| count as f64 > n)
                {
                    return None;
                }
                // AJV 8 in the fixed source annotates all items for contains.
                mark.items.extend(0..items.len());
            }
        }
        if let Some(JsValue::Array(branches)) = schema.get("allOf") {
            for child in branches {
                let nested = self.check(child, value, depth + 1)?;
                mark.merge(nested);
            }
        }
        if let Some(JsValue::Array(branches)) = schema.get("anyOf") {
            let mut found = false;
            for child in branches {
                if let Some(nested) = self.check(child, value, depth + 1) {
                    found = true;
                    mark.merge(nested);
                }
                if self.exhausted {
                    return None;
                }
            }
            if !found {
                return None;
            }
        }
        if let Some(JsValue::Array(branches)) = schema.get("oneOf") {
            let mut found = None;
            for child in branches {
                if let Some(nested) = self.check(child, value, depth + 1) {
                    if found.is_some() {
                        return None;
                    }
                    found = Some(nested);
                }
                if self.exhausted {
                    return None;
                }
            }
            mark.merge(found?);
        }
        if let Some(not) = schema.get("not")
            && (self.check(not, value, depth + 1).is_some() || self.exhausted) {
                return None;
            }
        if let Some(condition) = schema.get("if") {
            let passed = self.check(condition, value, depth + 1);
            if self.exhausted {
                return None;
            }
            let key = if passed.is_some() { "then" } else { "else" };
            if let Some(passed) = passed {
                mark.merge(passed);
            }
            if let Some(child) = schema.get(key) {
                let nested = self.check(child, value, depth + 1)?;
                mark.merge(nested);
            }
        }
        if let (Some(map), Some(unevaluated)) = (obj(value), schema.get("unevaluatedProperties")) {
            for (key, item) in map.own_entries() {
                if !mark.properties.contains(key) {
                    self.check(unevaluated, item, depth + 1)?;
                    mark.properties.insert(key.clone());
                }
            }
        }
        if let (JsValue::Array(items), Some(unevaluated)) = (value, schema.get("unevaluatedItems"))
        {
            for (i, item) in items.iter().enumerate() {
                if !mark.items.contains(&i) {
                    self.check(unevaluated, item, depth + 1)?;
                    mark.items.insert(i);
                }
            }
        }
        if self.exhausted { None } else { Some(mark) }
    }
}
fn type_match(value: &JsValue, t: &JsString) -> bool {
    if t.equals_str("object") {
        obj(value).is_some()
    } else if t.equals_str("array") {
        matches!(value, JsValue::Array(_))
    } else if t.equals_str("string") {
        value.as_string().is_some()
    } else if t.equals_str("number") {
        value.as_number().is_some_and(f64::is_finite)
    } else if t.equals_str("integer") {
        value
            .as_number()
            .is_some_and(|n| n.is_finite() && n.fract() == 0.0)
    } else if t.equals_str("boolean") {
        value.as_bool().is_some()
    } else if t.equals_str("null") {
        matches!(value, JsValue::Null)
    } else {
        false
    }
}
pub(super) fn accepts(schema: &JsValue, value: &JsValue) -> bool {
    if obj(value).is_none() {
        return false;
    }
    let mut eval = Eval {
        root: schema,
        work: 0,
        active: Vec::new(),
        resources: Vec::new(),
        anchors: Default::default(),
        exhausted: false,
    };
    eval.check(schema, value, 0).is_some() && !eval.exhausted
}
