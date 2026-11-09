//! Bounded validation for the Gateway user-dialog form contract.
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::collections::HashSet;
const DRAFT: &str = "https://json-schema.org/draft/2020-12/schema";
const KEYS: &[&str] = &[
    "type",
    "title",
    "description",
    "format",
    "default",
    "enum",
    "const",
    "$ref",
    "$defs",
    "properties",
    "patternProperties",
    "required",
    "additionalProperties",
    "items",
    "prefixItems",
    "contains",
    "minContains",
    "maxContains",
    "propertyNames",
    "allOf",
    "anyOf",
    "oneOf",
    "not",
    "if",
    "then",
    "else",
    "dependentRequired",
    "dependentSchemas",
    "minLength",
    "maxLength",
    "pattern",
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "multipleOf",
    "minItems",
    "maxItems",
    "uniqueItems",
    "minProperties",
    "maxProperties",
];
fn object(v: &JsValue) -> Option<&JsObject> {
    v.as_object()
}
fn strval<'a>(v: &'a JsValue, key: &str) -> Option<&'a JsString> {
    v.get(key).and_then(JsValue::as_string)
}
fn numeric(v: &JsValue, key: &str) -> Option<f64> {
    v.get(key).and_then(JsValue::as_number)
}
fn present(v: &JsValue, key: &str) -> bool {
    v.get(key).is_some_and(|v| !v.is_undefined())
}
fn nonblank(v: &JsString) -> bool {
    v.units().iter().any(|c| !matches!(*c, 0x0009..=0x000d | 0x0020 | 0x00a0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff))
}
fn json(v: &JsValue, budget: &mut usize, depth: usize) -> bool {
    *budget += 1;
    if *budget > 100_000 || depth > 128 {
        return false;
    }
    match v {
        JsValue::Null | JsValue::Bool(_) | JsValue::String(_) => true,
        JsValue::Number(n) => n.is_finite(),
        JsValue::Array(a) => a.iter().all(|v| json(v, budget, depth + 1)),
        JsValue::Object(o) => o
            .own_entries()
            .iter()
            .all(|(_, v)| json(v, budget, depth + 1)),
        _ => false,
    }
}
fn name_ok(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && name.len() <= 64
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}
fn refname(value: &JsString) -> Option<String> {
    let value = value.to_utf8().ok()?;
    let name = value.strip_prefix("#/$defs/")?;
    name_ok(name).then(|| name.to_owned())
}
const REGEX_BACKTRACK_BUDGET: u64 = 10_000;
const REGEX_SUBJECT_MAX_UNITS: usize = 1_048_576;
const REGEX_CALL_COST: usize = 2_048;
pub(super) fn legacy_regex(value: &JsString) -> Result<regress::Regex, String> {
    if value.len_utf16() > 4096 {
        return Err("Form pattern exceeds its native unit limit".into());
    }
    // Bound parser recursion before invoking the third-party compiler.
    let mut depth = 0usize;
    let mut escaped = false;
    let mut in_class = false;
    for unit in value.units() {
        if escaped {
            escaped = false;
            continue;
        }
        match *unit {
            0x5c => escaped = true,
            0x5b if !in_class => in_class = true,
            0x5d if in_class => in_class = false,
            0x28 if !in_class => {
                depth += 1;
                if depth > 128 {
                    return Err("Form pattern nesting exceeds the native limit".into());
                }
            }
            0x29 if !in_class => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    regress::Regex::from_unicode(
        value.units().iter().copied().map(u32::from),
        regress::Flags::default(),
    )
    .map_err(|_| "Invalid JavaScript regular expression".into())
}
fn pattern(value: &JsString, path: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("{path} cannot contain an empty pattern."));
    }
    if value.len_utf16() > 4096 {
        return Err(format!("{path} patterns must not exceed 4096 characters."));
    }
    legacy_regex(value)
        .map(|_| ())
        .map_err(|_| format!("{path} must be a valid JavaScript regular expression."))
}
fn node(
    value: &JsValue,
    path: &str,
    depth: usize,
    nodes: &mut usize,
    refs: &mut Vec<(String, String)>,
) -> Result<(), String> {
    let Some(o) = object(value) else {
        return Err(format!("{path} must be an object."));
    };
    *nodes += 1;
    if *nodes > 256 {
        return Err("schema may contain at most 256 nodes.".into());
    }
    if depth > 8 {
        return Err("schema nesting may not exceed 8 levels.".into());
    }
    for (key, _) in o.own_entries() {
        let key = key
            .to_utf8()
            .map_err(|_| format!("{path} contains an unsupported key."))?;
        if !KEYS.contains(&key.as_str()) {
            return Err(format!("{path}.{key} is not supported by form dialogs."));
        }
    }
    if let Some(r) = value.get("$ref").filter(|v| !v.is_undefined()) {
        let name = r
            .as_string()
            .and_then(refname)
            .ok_or_else(|| format!("{path}.$ref must be a local #/$defs/<name> reference."))?;
        refs.push((path.into(), name));
    }
    if let Some(defs) = value.get("$defs").filter(|v| !v.is_undefined()) {
        if path != "$" {
            return Err(format!(
                "{path}.$defs is only supported on the root form schema."
            ));
        }
        let defs = object(defs).ok_or_else(|| format!("{path}.$defs must be an object."))?;
        if defs.own_entries().len() > 64 {
            return Err(format!("{path}.$defs may contain at most 64 definitions."));
        }
        for (name, child) in defs.own_entries() {
            let name = name
                .to_utf8()
                .map_err(|_| format!("{path}.$defs contains an invalid name."))?;
            if !name_ok(&name) {
                return Err(format!(
                    "{path}.$defs.{name} must use a simple local definition name."
                ));
            }
            node(
                child,
                &format!("{path}.$defs.{name}"),
                depth + 1,
                nodes,
                refs,
            )?;
        }
    }
    if let Some(t) = value.get("type").filter(|v| !v.is_undefined()) {
        let one = |v: &JsValue| {
            v.as_string().is_some_and(|s| {
                [
                    "object", "array", "string", "number", "integer", "boolean", "null",
                ]
                .iter()
                .any(|x| s.equals_str(x))
            })
        };
        if !(one(t) || matches!(t, JsValue::Array(a) if !a.is_empty() && a.iter().all(one))) {
            return Err(format!("{path}.type must name supported JSON value types."));
        }
    }
    for key in ["title", "description", "format", "pattern"] {
        if present(value, key) && strval(value, key).is_none() {
            return Err(format!("{path}.{key} must be a string."));
        }
    }
    if let Some(fmt) = strval(value, "format")
        && !["email", "uri", "uuid", "date", "time", "date-time"]
            .iter()
            .any(|x| fmt.equals_str(x))
    {
        return Err(format!(
            "{path}.format must be one of email, uri, uuid, date, time, date-time."
        ));
    }
    if let Some(p) = strval(value, "pattern") {
        if p.len_utf16() > 4096 {
            return Err(format!("{path}.pattern must not exceed 4096 characters."));
        }
        if !p.is_empty() {
            legacy_regex(p).map_err(|_| {
                format!("{path}.pattern must be a valid JavaScript regular expression.")
            })?;
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
        if present(value, key)
            && numeric(value, key).is_none_or(|n| !n.is_finite() || n < 0.0 || n.fract() != 0.0)
        {
            return Err(format!("{path}.{key} must be a non-negative integer."));
        }
    }
    for (a, b) in [
        ("minLength", "maxLength"),
        ("minItems", "maxItems"),
        ("minProperties", "maxProperties"),
        ("minContains", "maxContains"),
    ] {
        if numeric(value, a)
            .zip(numeric(value, b))
            .is_some_and(|(a, b)| a > b)
        {
            return Err(format!("{path}.{a} must not exceed {path}.{b}."));
        }
    }
    for key in [
        "minimum",
        "maximum",
        "exclusiveMinimum",
        "exclusiveMaximum",
        "multipleOf",
    ] {
        if present(value, key) && numeric(value, key).is_none_or(|n| !n.is_finite()) {
            return Err(format!("{path}.{key} must be a finite number."));
        }
    }
    if numeric(value, "multipleOf").is_some_and(|n| n <= 0.0) {
        return Err(format!("{path}.multipleOf must be greater than zero."));
    }
    if numeric(value, "minimum")
        .zip(numeric(value, "maximum"))
        .is_some_and(|(a, b)| a > b)
    {
        return Err(format!("{path}.minimum must not exceed {path}.maximum."));
    }
    if numeric(value, "exclusiveMinimum")
        .zip(numeric(value, "exclusiveMaximum"))
        .is_some_and(|(a, b)| a >= b)
    {
        return Err(format!(
            "{path}.exclusiveMinimum must be less than {path}.exclusiveMaximum."
        ));
    }
    if present(value, "uniqueItems")
        && value
            .get("uniqueItems")
            .and_then(JsValue::as_bool)
            .is_none()
    {
        return Err(format!("{path}.uniqueItems must be a boolean."));
    }
    if (present(value, "minContains") || present(value, "maxContains"))
        && !present(value, "contains")
    {
        return Err(format!(
            "{path}.minContains and {path}.maxContains require {path}.contains."
        ));
    }
    let properties = value.get("properties").and_then(object);
    if let Some(required) = value.get("required").filter(|v| !v.is_undefined()) {
        let JsValue::Array(required) = required else {
            return Err(format!(
                "{path}.required must be an array of unique non-empty property names."
            ));
        };
        let mut seen = HashSet::new();
        if required.iter().any(|item| {
            item.as_string()
                .is_none_or(|s| !nonblank(s) || !seen.insert(s.clone()))
        }) {
            return Err(format!(
                "{path}.required must be an array of unique non-empty property names."
            ));
        }
        if properties.is_none() && !required.is_empty() {
            return Err(format!("{path}.required requires properties."));
        }
        if properties.is_some_and(|props| {
            required
                .iter()
                .any(|key| props.get_key(key.as_string().unwrap()).is_none())
        }) {
            return Err(format!(
                "{path}.required must only reference declared properties."
            ));
        }
    }
    for key in ["properties", "patternProperties", "dependentSchemas"] {
        if let Some(raw) = value.get(key).filter(|v| !v.is_undefined()) {
            let map = object(raw).ok_or_else(|| format!("{path}.{key} must be an object."))?;
            if map.own_entries().len() > 64 {
                return Err(format!("{path}.{key} may contain at most 64 fields."));
            }
            for (name, child) in map.own_entries() {
                if !nonblank(name) {
                    return Err(format!("{path}.{key} cannot contain an empty field name."));
                }
                if key == "patternProperties" {
                    pattern(name, &format!("{path}.patternProperties"))?;
                }
                if key == "dependentSchemas" && properties.is_none_or(|p| p.get_key(name).is_none())
                {
                    return Err(format!(
                        "{path}.dependentSchemas.{} must reference a declared property.",
                        String::from_utf16_lossy(name.units())
                    ));
                }
                node(
                    child,
                    &format!("{path}.{key}.{}", String::from_utf16_lossy(name.units())),
                    depth + 1,
                    nodes,
                    refs,
                )?;
            }
        }
    }
    if let Some(raw) = value.get("dependentRequired").filter(|v| !v.is_undefined()) {
        let deps =
            object(raw).ok_or_else(|| format!("{path}.dependentRequired must be an object."))?;
        if deps.own_entries().len() > 64 {
            return Err(format!(
                "{path}.dependentRequired may contain at most 64 dependencies."
            ));
        }
        for (name, values) in deps.own_entries() {
            let n = String::from_utf16_lossy(name.units());
            if !nonblank(name) {
                return Err(format!(
                    "{path}.dependentRequired cannot contain an empty property name."
                ));
            }
            if properties.is_none_or(|p| p.get_key(name).is_none()) {
                return Err(format!(
                    "{path}.dependentRequired.{n} must reference a declared property."
                ));
            }
            let JsValue::Array(values) = values else {
                return Err(format!(
                    "{path}.dependentRequired.{n} must contain 1-64 unique non-empty declared property names."
                ));
            };
            let mut seen = HashSet::new();
            if values.is_empty()
                || values.len() > 64
                || values.iter().any(|v| {
                    v.as_string()
                        .is_none_or(|s| !nonblank(s) || !seen.insert(s.clone()))
                })
            {
                return Err(format!(
                    "{path}.dependentRequired.{n} must contain 1-64 unique non-empty declared property names."
                ));
            }
            if values.iter().any(|v| {
                properties
                    .unwrap()
                    .get_key(v.as_string().unwrap())
                    .is_none()
            }) {
                return Err(format!(
                    "{path}.dependentRequired.{n} must only reference declared properties."
                ));
            }
        }
    }
    for key in [
        "items",
        "contains",
        "propertyNames",
        "not",
        "if",
        "then",
        "else",
    ] {
        if let Some(child) = value.get(key).filter(|v| !v.is_undefined()) {
            node(child, &format!("{path}.{key}"), depth + 1, nodes, refs)?;
        }
    }
    if let Some(child) = value
        .get("additionalProperties")
        .filter(|v| !v.is_undefined() && v.as_bool().is_none())
    {
        node(
            child,
            &format!("{path}.additionalProperties"),
            depth + 1,
            nodes,
            refs,
        )?;
    }
    for key in ["prefixItems", "allOf", "anyOf", "oneOf"] {
        if let Some(raw) = value.get(key).filter(|v| !v.is_undefined()) {
            let JsValue::Array(items) = raw else {
                return Err(format!("{path}.{key} must contain 1-32 schema branches."));
            };
            if items.is_empty() || items.len() > 32 {
                return Err(format!("{path}.{key} must contain 1-32 schema branches."));
            }
            for (i, child) in items.iter().enumerate() {
                node(child, &format!("{path}.{key}[{i}]"), depth + 1, nodes, refs)?;
            }
        }
    }
    if (present(value, "then") || present(value, "else")) && !present(value, "if") {
        return Err(format!("{path}.then and {path}.else require {path}.if."));
    }
    if present(value, "if") && !present(value, "then") && !present(value, "else") {
        return Err(format!("{path}.if requires {path}.then or {path}.else."));
    }
    if let Some(e) = value.get("enum").filter(|v| !v.is_undefined())
        && !matches!(e, JsValue::Array(a) if !a.is_empty() && a.len()<=64)
    {
        return Err(format!("{path}.enum must contain 1-64 JSON values."));
    }
    Ok(())
}
pub(crate) fn validate_schema(value: &JsValue) -> Result<(), String> {
    if object(value).is_none() {
        return Err("schema must be a JSON object.".into());
    }
    if !strval(value, "type").is_some_and(|t| t.equals_str("object")) {
        return Err("schema.type must be object.".into());
    }
    if !json(value, &mut 0, 0) {
        return Err("schema must contain only finite JSON values.".into());
    }
    let bytes = pilotdeck_protocol::stringify_json(value)
        .map_err(|e| e.to_string())?
        .ok_or("schema must contain only finite JSON values.")?;
    if bytes.len() > 64_000 {
        return Err("schema must not exceed 64000 bytes.".into());
    }
    if let Some(draft) = value.get("$schema").filter(|v| !v.is_undefined()) {
        if draft.as_string().is_some_and(|v| v.equals_str(DRAFT)) {
            return super::form_draft2020::validate(value);
        }
        return Err(format!(
            "schema.$schema must be {DRAFT} to enable standards-based form validation."
        ));
    }
    let mut refs = Vec::new();
    node(value, "$", 0, &mut 0, &mut refs)?;
    let defs = value.get("$defs").and_then(object);
    for (path, name) in &refs {
        if defs.is_none_or(|d| d.get(name).is_none()) {
            return Err(format!(
                "{path}.$ref references unknown root definition {name}."
            ));
        }
    }
    fn visit(
        name: &str,
        refs: &[(String, String)],
        active: &mut Vec<String>,
        done: &mut HashSet<String>,
    ) -> Result<(), String> {
        if let Some(i) = active.iter().position(|n| n == name) {
            let mut chain = active[i..].to_vec();
            chain.push(name.into());
            return Err(format!(
                "$defs contains a circular reference: {}.",
                chain.join(" -> ")
            ));
        }
        if done.contains(name) {
            return Ok(());
        }
        active.push(name.into());
        let prefix = format!("$.$defs.{name}");
        for (path, target) in refs {
            if path == &prefix
                || path
                    .strip_prefix(&prefix)
                    .is_some_and(|tail| tail.starts_with('.') || tail.starts_with('['))
            {
                visit(target, refs, active, done)?;
            }
        }
        active.pop();
        done.insert(name.into());
        Ok(())
    }
    if let Some(defs) = defs {
        let mut done = HashSet::new();
        for (name, _) in defs.own_entries() {
            visit(
                &name.to_utf8().map_err(|_| "Invalid definition name")?,
                &refs,
                &mut Vec::new(),
                &mut done,
            )?;
        }
    }
    Ok(())
}
fn equal(a: &JsValue, b: &JsValue, depth: usize, budget: &mut usize) -> bool {
    *budget += 1;
    if depth > 128 || *budget > 100_000 {
        return false;
    }
    match (a, b) {
        (JsValue::Array(a), JsValue::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| equal(a, b, depth + 1, budget))
        }
        (JsValue::Object(a), JsValue::Object(b)) => {
            let a = a.own_entries();
            a.len() == b.own_entries().len()
                && a.iter().all(|(k, v)| {
                    b.get_key(k)
                        .is_some_and(|other| equal(v, other, depth + 1, budget))
                })
        }
        _ => a == b,
    }
}
fn type_match(value: &JsValue, t: &JsString) -> bool {
    if t.equals_str("null") {
        matches!(value, JsValue::Null)
    } else if t.equals_str("boolean") {
        matches!(value, JsValue::Bool(_))
    } else if t.equals_str("number") {
        value.as_number().is_some_and(f64::is_finite)
    } else if t.equals_str("integer") {
        value
            .as_number()
            .is_some_and(|n| n.is_finite() && n.fract() == 0.0)
    } else if t.equals_str("string") {
        value.as_string().is_some()
    } else if t.equals_str("array") {
        matches!(value, JsValue::Array(_))
    } else if t.equals_str("object") {
        value.as_object().is_some()
    } else {
        false
    }
}
fn match_pattern(pattern: &JsString, value: &JsString, budget: &mut usize) -> bool {
    *budget = budget.saturating_add(REGEX_CALL_COST);
    if *budget > 100_000 || value.len_utf16() > REGEX_SUBJECT_MAX_UNITS {
        *budget = 100_001;
        return false;
    }
    let Ok(regex) = legacy_regex(pattern) else {
        *budget = 100_001;
        return false;
    };
    match regex.find_from_ucs2_budgeted(value.units(), 0, REGEX_BACKTRACK_BUDGET) {
        regress::BudgetedMatch::Match(_) => true,
        regress::BudgetedMatch::NoMatch => false,
        regress::BudgetedMatch::BudgetExhausted => {
            *budget = 100_001;
            false
        }
    }
}
fn date(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 10
        || b[4] != b'-'
        || b[7] != b'-'
        || !b
            .iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
    {
        return false;
    }
    let year = s[0..4].parse::<u32>().unwrap();
    let month = s[5..7].parse::<u32>().unwrap();
    let day = s[8..10].parse::<u32>().unwrap();
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if leap {
                29
            } else {
                28
            }
        }
        _ => return false,
    };
    (1..=days).contains(&day)
}
fn time(s: &str, tz_required: bool) -> bool {
    let b = s.as_bytes();
    if b.len() < 8
        || b[2] != b':'
        || b[5] != b':'
        || ![0, 1, 3, 4, 6, 7].iter().all(|i| b[*i].is_ascii_digit())
    {
        return false;
    }
    let num = |a: usize| (b[a] - b'0') * 10 + (b[a + 1] - b'0');
    if num(0) > 23 || num(3) > 59 || num(6) > 59 {
        return false;
    }
    let mut i = 8;
    if b.get(i) == Some(&b'.') {
        i += 1;
        let start = i;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        if i == start {
            return false;
        }
    }
    if i == b.len() {
        return !tz_required;
    }
    if b[i] == b'Z' {
        return i + 1 == b.len();
    }
    if !matches!(b[i], b'+' | b'-')
        || i + 6 != b.len()
        || b[i + 3] != b':'
        || ![i + 1, i + 2, i + 4, i + 5]
            .iter()
            .all(|n| b[*n].is_ascii_digit())
    {
        return false;
    }
    num(i + 1) <= 23 && num(i + 4) <= 59
}
fn format_ok(name: &JsString, value: &JsString) -> bool {
    if name.equals_str("email") {
        let units = value.units();
        let whitespace = |c: u16| matches!(c,0x0009..=0x000d|0x0020|0x00a0|0x1680|0x2000..=0x200a|0x2028|0x2029|0x202f|0x205f|0x3000|0xfeff);
        if units.iter().any(|c| whitespace(*c)) {
            return false;
        }
        let Some(at) = units.iter().position(|c| *c == 64) else {
            return false;
        };
        if at == 0 || units[at + 1..].contains(&64) {
            return false;
        }
        let domain = &units[at + 1..];
        return domain
            .iter()
            .rposition(|c| *c == 46)
            .is_some_and(|dot| dot > 0 && dot + 1 < domain.len());
    }
    if name.equals_str("uri") {
        // WHATWG URL consumes a WebIDL USVString; isolated surrogates are
        // replaced during URL parsing, but the original dialog stays intact.
        if value.units().iter().any(|c|matches!(*c,0x0009..=0x000d|0x0020|0x00a0|0x1680|0x2000..=0x200a|0x2028|0x2029|0x202f|0x205f|0x3000|0xfeff)){return false;}
        return url::Url::parse(&String::from_utf16_lossy(value.units())).is_ok();
    }
    let Ok(v) = value.to_utf8() else {
        return false;
    };
    if name.equals_str("date") {
        date(&v)
    } else if name.equals_str("time") {
        time(&v, false)
    } else if name.equals_str("date-time") {
        v.split_once('T')
            .is_some_and(|(a, b)| date(a) && time(b, true))
    } else if name.equals_str("uuid") {
        let b = v.as_bytes();
        b.len() == 36
            && [8, 13, 18, 23].iter().all(|i| b[*i] == b'-')
            && b.iter()
                .enumerate()
                .all(|(i, c)| [8, 13, 18, 23].contains(&i) || c.is_ascii_hexdigit())
            && matches!(b[19], b'8' | b'9' | b'a' | b'b' | b'A' | b'B')
    } else {
        false
    }
}
fn check(
    value: &JsValue,
    schema: &JsValue,
    root: &JsValue,
    depth: usize,
    budget: &mut usize,
) -> bool {
    *budget += 1;
    if *budget > 100_000 || depth > 128 {
        return false;
    }
    if let Some(r) = strval(schema, "$ref") {
        let Some(name) = refname(r) else {
            return false;
        };
        let Some(target) = root
            .get("$defs")
            .and_then(object)
            .and_then(|d| d.get(&name))
        else {
            return false;
        };
        if !check(value, target, root, depth + 1, budget) {
            return false;
        }
    }
    if let Some(t) = schema.get("type") {
        let ok = match t {
            JsValue::String(s) => type_match(value, s),
            JsValue::Array(a) => a
                .iter()
                .any(|t| t.as_string().is_some_and(|s| type_match(value, s))),
            _ => false,
        };
        if !ok {
            return false;
        }
    }
    if schema
        .get("const")
        .is_some_and(|v| !equal(v, value, 0, budget))
    {
        return false;
    }
    if let Some(JsValue::Array(a)) = schema.get("enum")
        && !a.iter().any(|v| equal(v, value, 0, budget))
    {
        return false;
    }
    if let Some(s) = value.as_string() {
        let len = s.len_utf16() as f64;
        if numeric(schema, "minLength").is_some_and(|n| len < n)
            || numeric(schema, "maxLength").is_some_and(|n| len > n)
        {
            return false;
        }
        if strval(schema, "pattern").is_some_and(|p| !match_pattern(p, s, budget))
            || strval(schema, "format").is_some_and(|f| !format_ok(f, s))
        {
            return false;
        }
    }
    if let Some(n) = value.as_number()
        && (numeric(schema, "minimum").is_some_and(|x| n < x)
            || numeric(schema, "maximum").is_some_and(|x| n > x)
            || numeric(schema, "exclusiveMinimum").is_some_and(|x| n <= x)
            || numeric(schema, "exclusiveMaximum").is_some_and(|x| n >= x)
            || numeric(schema, "multipleOf")
                .is_some_and(|x| ((n / x) - (n / x).round()).abs() >= 1e-10))
    {
        return false;
    }
    if let JsValue::Array(items) = value {
        let n = items.len() as f64;
        if numeric(schema, "minItems").is_some_and(|x| n < x)
            || numeric(schema, "maxItems").is_some_and(|x| n > x)
        {
            return false;
        }
        if schema.get("uniqueItems").and_then(JsValue::as_bool) == Some(true)
            && items.iter().enumerate().any(|(i, v)| {
                items[..i]
                    .iter()
                    .any(|p| *budget > 100_000 || equal(v, p, 0, budget))
            })
        {
            return false;
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
            }) && !check(item, child, root, depth + 1, budget)
            {
                return false;
            }
        }
        if let Some(contains) = schema.get("contains") {
            let count = items
                .iter()
                .filter(|v| check(v, contains, root, depth + 1, budget))
                .count() as f64;
            if count < numeric(schema, "minContains").unwrap_or(1.0)
                || numeric(schema, "maxContains").is_some_and(|n| count > n)
            {
                return false;
            }
        }
    }
    if let Some(map) = object(value) {
        let entries = map.own_entries();
        let n = entries.len() as f64;
        if numeric(schema, "minProperties").is_some_and(|x| n < x)
            || numeric(schema, "maxProperties").is_some_and(|x| n > x)
        {
            return false;
        }
        if let Some(JsValue::Array(req)) = schema.get("required")
            && req
                .iter()
                .any(|r| r.as_string().is_some_and(|s| map.get_key(s).is_none()))
        {
            return false;
        }
        let props = schema.get("properties").and_then(object);
        let patterns = schema.get("patternProperties").and_then(object);
        for (key, item) in &entries {
            if let Some(child) = props.and_then(|p| p.get_key(key)) {
                if !check(item, child, root, depth + 1, budget) {
                    return false;
                }
            } else if let Some(additional) = schema.get("additionalProperties")
                && (additional.as_bool() == Some(false)
                    || (object(additional).is_some()
                        && !check(item, additional, root, depth + 1, budget)))
            {
                return false;
            }
            if let Some(patterns) = patterns {
                for (p, child) in patterns.own_entries() {
                    if match_pattern(p, key, budget) && !check(item, child, root, depth + 1, budget)
                    {
                        return false;
                    }
                }
            }
            if let Some(names) = schema.get("propertyNames")
                && !check(
                    &JsValue::String((*key).clone()),
                    names,
                    root,
                    depth + 1,
                    budget,
                )
            {
                return false;
            }
        }
        if let Some(deps) = schema.get("dependentRequired").and_then(object) {
            for (key, values) in deps.own_entries() {
                if map.get_key(key).is_some()
                    && let JsValue::Array(values) = values
                    && values
                        .iter()
                        .any(|v| v.as_string().is_some_and(|s| map.get_key(s).is_none()))
                {
                    return false;
                }
            }
        }
        if let Some(deps) = schema.get("dependentSchemas").and_then(object) {
            for (key, child) in deps.own_entries() {
                if map.get_key(key).is_some() && !check(value, child, root, depth + 1, budget) {
                    return false;
                }
            }
        }
    }
    for (key, min, max) in [
        ("allOf", usize::MAX, usize::MAX),
        ("anyOf", 1, usize::MAX),
        ("oneOf", 1, 1),
    ] {
        if let Some(JsValue::Array(branches)) = schema.get(key) {
            let count = branches
                .iter()
                .filter(|branch| check(value, branch, root, depth + 1, budget))
                .count();
            if (key == "allOf" && count != branches.len())
                || (key != "allOf" && (count < min || count > max))
            {
                return false;
            }
        }
    }
    if schema
        .get("not")
        .is_some_and(|s| check(value, s, root, depth + 1, budget))
    {
        return false;
    }
    if let Some(condition) = schema.get("if") {
        let key = if check(value, condition, root, depth + 1, budget) {
            "then"
        } else {
            "else"
        };
        if schema
            .get(key)
            .is_some_and(|s| !check(value, s, root, depth + 1, budget))
        {
            return false;
        }
    }
    *budget <= 100_000
}
pub(crate) fn accepts_form_dialog_answer(schema: &JsValue, value: &JsValue) -> bool {
    if object(value).is_none() || !json(value, &mut 0, 0) || validate_schema(schema).is_err() {
        return false;
    }
    if strval(schema, "$schema").is_some_and(|s| s.equals_str(DRAFT)) {
        return super::form_draft2020::accepts(schema, value);
    }
    check(value, schema, schema, 0, &mut 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixed_ts_legacy_schema_and_answer_oracle() {
        let fixture = pilotdeck_protocol::parse_json(include_str!(
            "../../../../tests/fixtures/user-dialog-cli/oracle.json"
        ))
        .unwrap();
        let Some(JsValue::Array(records)) = fixture.get("records") else {
            panic!("records")
        };
        let mut count = 0;
        for row in records {
            if row
                .get("id")
                .and_then(JsValue::as_string)
                .is_some_and(|id| {
                    id.equals_str("draft-unevaluated") || id.equals_str("draft-prefix")
                })
            {
                continue;
            }
            let schema = row.get("schema").unwrap();
            let answer = row.get("answer").unwrap();
            let expected = row.get("result").unwrap();
            let actual = validate_schema(schema);
            assert_eq!(
                actual.is_ok(),
                expected.get("ok").and_then(JsValue::as_bool).unwrap()
            );
            if let Err(error) = actual {
                assert_eq!(
                    Some(error.as_str()),
                    expected
                        .get("message")
                        .and_then(JsValue::as_string)
                        .and_then(|s| s.to_utf8().ok())
                        .as_deref()
                );
            } else {
                assert_eq!(
                    accepts_form_dialog_answer(schema, answer),
                    row.get("accepted").and_then(JsValue::as_bool).unwrap()
                );
            }
            count += 1;
        }
        assert_eq!(count, 15);
    }
    #[test]
    fn unique_items_comparisons_are_bounded_and_fail_closed() {
        let schema = pilotdeck_protocol::parse_json(
            r#"{"type":"object","properties":{"items":{"type":"array","uniqueItems":true}}}"#,
        )
        .unwrap();
        let make = |count| {
            let mut object = JsObject::new();
            object.insert(
                "items".into(),
                JsValue::Array((0..count).map(|n| JsValue::Number(f64::from(n))).collect()),
            );
            JsValue::Object(object)
        };
        assert!(accepts_form_dialog_answer(&schema, &make(100)));
        assert!(!accepts_form_dialog_answer(&schema, &make(1000)));
    }
    #[test]
    fn fixed_node_legacy_regex_oracle_preserves_utf16() {
        let fixture = pilotdeck_protocol::parse_json(include_str!(
            "../../../../tests/fixtures/user-dialog-cli/regex-oracle.json"
        ))
        .unwrap();
        let Some(JsValue::Array(rows)) = fixture.get("records") else {
            panic!("records")
        };
        let mut count = 0;
        for row in rows {
            let pat = row.get("pattern").and_then(JsValue::as_string).unwrap();
            let result = legacy_regex(pat);
            let expected = row.get("validation").unwrap();
            assert_eq!(
                result.is_ok(),
                expected.get("ok").and_then(JsValue::as_bool).unwrap()
            );
            if result.is_ok() {
                let mut budget = 0;
                assert_eq!(
                    match_pattern(
                        pat,
                        row.get("value").and_then(JsValue::as_string).unwrap(),
                        &mut budget
                    ),
                    row.get("accepted").and_then(JsValue::as_bool).unwrap()
                );
            }
            count += 1;
        }
        assert_eq!(count, 18);
    }
    #[test]
    fn pathological_backtracking_is_interrupted_not_guessed() {
        let pattern: JsString = "^(a+)+$".into();
        let subject = JsString::from(format!("{}!", "a".repeat(200)).as_str());
        let mut budget = 0;
        assert!(!match_pattern(&pattern, &subject, &mut budget));
        assert!(
            budget > 100_000,
            "engine must signal exhausted work, not a normal non-match"
        );
    }
    #[test]
    fn fixed_node_draft2020_metaschema_and_resource_oracle() {
        let fixture = pilotdeck_protocol::parse_json(include_str!(
            "../../../../tests/fixtures/user-dialog-cli/meta-oracle.json"
        ))
        .unwrap();
        let Some(JsValue::Array(rows)) = fixture.get("records") else {
            panic!("records")
        };
        assert_eq!(rows.len(), 17);
        for row in rows {
            let schema = row.get("schema").unwrap();
            let expected = row.get("result").unwrap();
            let actual = validate_schema(schema);
            assert_eq!(
                actual.is_ok(),
                expected.get("ok").and_then(JsValue::as_bool).unwrap(),
                "{:?}",
                row.get("id")
            );
            if let Err(error) = actual {
                assert_eq!(
                    Some(error.as_str()),
                    expected
                        .get("message")
                        .and_then(JsValue::as_string)
                        .and_then(|s| s.to_utf8().ok())
                        .as_deref(),
                    "{:?}",
                    row.get("id")
                );
            } else {
                assert_eq!(
                    accepts_form_dialog_answer(schema, row.get("answer").unwrap()),
                    row.get("accepted").and_then(JsValue::as_bool).unwrap(),
                    "{:?}",
                    row.get("id")
                );
            }
        }
    }
    #[test]
    fn fixed_node_format_oracle_preserves_surrogates() {
        let fixture = pilotdeck_protocol::parse_json(include_str!(
            "../../../../tests/fixtures/user-dialog-cli/formats-oracle.json"
        ))
        .unwrap();
        let Some(JsValue::Array(rows)) = fixture.get("records") else {
            panic!("records")
        };
        assert_eq!(rows.len(), 32);
        for row in rows {
            let schema = row.get("schema").unwrap();
            let expected = row.get("result").unwrap();
            let actual = validate_schema(schema);
            assert_eq!(
                actual.is_ok(),
                expected.get("ok").and_then(JsValue::as_bool).unwrap(),
                "{:?}",
                row.get("id")
            );
            if let Err(error) = actual {
                assert_eq!(
                    Some(error.as_str()),
                    expected
                        .get("message")
                        .and_then(JsValue::as_string)
                        .and_then(|s| s.to_utf8().ok())
                        .as_deref(),
                    "{:?}",
                    row.get("id")
                );
            } else {
                assert_eq!(
                    accepts_form_dialog_answer(schema, row.get("answer").unwrap()),
                    row.get("accepted").and_then(JsValue::as_bool).unwrap(),
                    "{:?}",
                    row.get("id")
                );
            }
        }
    }
    #[test]
    fn fixed_node_draft2020_meta_and_dynamic_scope_oracles() {
        let files = [
            (
                include_str!("../../../../tests/fixtures/user-dialog-cli/draft-extra-oracle.json"),
                24usize,
            ),
            (
                include_str!("../../../../tests/fixtures/user-dialog-cli/dynamic-oracle.json"),
                13usize,
            ),
            (
                include_str!(
                    "../../../../tests/fixtures/user-dialog-cli/dynamic-scope-oracle.json"
                ),
                4usize,
            ),
        ];
        for (fixture, want) in files {
            let fixture = pilotdeck_protocol::parse_json(fixture).unwrap();
            let Some(JsValue::Array(rows)) = fixture.get("records") else {
                panic!("records")
            };
            assert_eq!(rows.len(), want);
            for row in rows {
                let schema = row.get("schema").unwrap();
                let expected = row.get("result").unwrap();
                let actual = validate_schema(schema);
                assert_eq!(
                    actual.is_ok(),
                    expected.get("ok").and_then(JsValue::as_bool).unwrap(),
                    "{:?}",
                    row.get("id")
                );
                if let Err(error) = actual {
                    assert_eq!(
                        Some(error.as_str()),
                        expected
                            .get("message")
                            .and_then(JsValue::as_string)
                            .and_then(|s| s.to_utf8().ok())
                            .as_deref(),
                        "{:?}",
                        row.get("id")
                    );
                } else {
                    assert_eq!(
                        accepts_form_dialog_answer(schema, row.get("answer").unwrap()),
                        row.get("accepted").and_then(JsValue::as_bool).unwrap(),
                        "{:?}",
                        row.get("id")
                    );
                }
            }
        }
    }
    #[test]
    fn fixed_node_draft2020_schema_answer_oracle() {
        let fixture = pilotdeck_protocol::parse_json(include_str!(
            "../../../../tests/fixtures/user-dialog-cli/draft-oracle.json"
        ))
        .unwrap();
        let Some(JsValue::Array(rows)) = fixture.get("records") else {
            panic!("records")
        };
        let mut count = 0;
        for row in rows {
            let schema = row.get("schema").unwrap();
            let expected = row.get("result").unwrap();
            let actual = validate_schema(schema);
            assert_eq!(
                actual.is_ok(),
                expected.get("ok").and_then(JsValue::as_bool).unwrap(),
                "{:?}",
                row.get("id")
            );
            if let Err(error) = actual {
                assert_eq!(
                    Some(error.as_str()),
                    expected
                        .get("message")
                        .and_then(JsValue::as_string)
                        .and_then(|s| s.to_utf8().ok())
                        .as_deref(),
                    "{:?}",
                    row.get("id")
                );
            } else {
                assert_eq!(
                    accepts_form_dialog_answer(schema, row.get("answer").unwrap()),
                    row.get("accepted").and_then(JsValue::as_bool).unwrap(),
                    "{:?}",
                    row.get("id")
                );
            }
            count += 1;
        }
        assert_eq!(count, 25);
    }
}
