//! Form answer validation: legacy PilotDeck subset plus local Draft 2020-12.
use super::*;
use std::collections::HashSet;

const DRAFT: &str = "https://json-schema.org/draft/2020-12/schema";
pub(super) fn accepts(schema: &JsValue, value: &JsValue) -> bool {
    if value.as_object().is_none() {
        return false;
    }
    if text(schema, "$schema").as_deref() == Some(DRAFT) {
        return standard_accepts(schema, value);
    }
    check(schema, schema, value, 0).is_some()
}
fn array<'a>(value: &'a JsValue, name: &str) -> &'a [JsValue] {
    match value.get(name) {
        Some(JsValue::Array(values)) => values,
        _ => &[],
    }
}
fn fields<'a>(value: &'a JsValue, name: &str) -> &'a [(JsString, JsValue)] {
    value
        .get(name)
        .and_then(JsValue::as_object)
        .map_or(&[], JsObject::entries)
}
fn text(value: &JsValue, name: &str) -> Option<String> {
    value.get(name)?.as_string()?.to_utf8().ok()
}
fn eq(a: &JsValue, b: &JsValue) -> bool {
    match (a, b) {
        (JsValue::Object(a), JsValue::Object(b)) => {
            a.entries().len() == b.entries().len()
                && a.entries()
                    .iter()
                    .all(|(key, value)| b.get_key(key).is_some_and(|other| eq(value, other)))
        }
        (JsValue::Array(a), JsValue::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| eq(a, b))
        }
        _ => a == b,
    }
}
fn pattern(pattern: &JsString, value: &JsString, unicode: bool) -> bool {
    // The house flagless evaluator preserves UTF-16 lengths and JS escapes.
    if !unicode && !pattern.units().contains(&124) {
        return crate::hooks::test_flagless_regexp(pattern.units(), value.units());
    }
    match (pattern.to_utf8(), value.to_utf8()) {
        (Ok(pattern), Ok(value)) => {
            regex::Regex::new(&pattern).is_ok_and(|regex| regex.is_match(&value))
        }
        _ => false,
    }
}
fn reference<'a>(root: &'a JsValue, reference: &str) -> Option<&'a JsValue> {
    if reference == "#" {
        return Some(root);
    }
    let mut node = root;
    for segment in reference.strip_prefix("#/")?.split('/') {
        let segment = segment.replace("~1", "/").replace("~0", "~");
        node = match node {
            JsValue::Array(values) => values.get(segment.parse::<usize>().ok()?)?,
            _ => node.get(&segment)?,
        };
    }
    Some(node)
}

// Successful schemas return the object names they evaluated for unevaluatedProperties.
fn check(
    root: &JsValue,
    schema: &JsValue,
    value: &JsValue,
    depth: usize,
) -> Option<HashSet<JsString>> {
    if depth > 256 {
        return None;
    }
    if let JsValue::Bool(accept) = schema {
        return accept.then(HashSet::new);
    }
    schema.as_object()?;
    let standard = false;
    let recurse = |schema, value| check(root, schema, value, depth + 1);
    let mut evaluated = HashSet::new();
    if let Some(reference_text) = text(schema, "$ref") {
        if !standard
            && !reference_text.strip_prefix("#/$defs/").is_some_and(|name| {
                let mut chars = name.chars();
                chars.next().is_some_and(|ch| ch.is_ascii_alphabetic())
                    && name.len() <= 64
                    && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
            })
        {
            return None;
        }
        evaluated.extend(recurse(reference(root, &reference_text)?, value)?);
    }
    if let Some(types) = schema.get("type") {
        let matches = |kind: &JsValue| {
            let Some(kind) = kind.as_string() else {
                return false;
            };
            match kind.to_utf8().as_deref() {
                Ok("object") => value.as_object().is_some(),
                Ok("array") => matches!(value, JsValue::Array(_)),
                Ok("string") => value.as_string().is_some(),
                Ok("boolean") => value.as_bool().is_some(),
                Ok("null") => matches!(value, JsValue::Null),
                Ok("number") => value.as_number().is_some_and(f64::is_finite),
                Ok("integer") => value
                    .as_number()
                    .is_some_and(|value| value.is_finite() && value.fract() == 0.0),
                _ => false,
            }
        };
        if !match types {
            JsValue::Array(types) => types.iter().any(matches),
            _ => matches(types),
        } {
            return None;
        }
    }
    if let Some(constant) = schema.get("const").filter(|value| !value.is_undefined())
        && !eq(constant, value)
    {
        return None;
    }
    if let Some(JsValue::Array(values)) = schema.get("enum")
        && !values.iter().any(|candidate| eq(candidate, value))
    {
        return None;
    }
    let bound = |key: &str| schema.get(key).and_then(JsValue::as_number);
    if let Some(value) = value.as_string() {
        let length = if standard {
            value.to_utf8().ok()?.chars().count()
        } else {
            value.units().len()
        } as f64;
        if bound("minLength").is_some_and(|min| length < min)
            || bound("maxLength").is_some_and(|max| length > max)
        {
            return None;
        }
        if let Some(pattern_text) = schema.get("pattern").and_then(JsValue::as_string)
            && !pattern(pattern_text, value, standard)
        {
            return None;
        }
        if let Some(format) = text(schema, "format")
            && !format_valid(&value.to_utf8().ok()?, &format, standard)
        {
            return None;
        }
    }
    if let Some(value) = value.as_number() {
        if bound("minimum").is_some_and(|min| value < min)
            || bound("maximum").is_some_and(|max| value > max)
            || bound("exclusiveMinimum").is_some_and(|min| value <= min)
            || bound("exclusiveMaximum").is_some_and(|max| value >= max)
        {
            return None;
        }
        if let Some(divisor) = bound("multipleOf") {
            let q = value / divisor;
            if !q.is_finite()
                || if standard {
                    q.fract() != 0.0
                } else {
                    (q - q.round()).abs() >= 1e-10
                }
            {
                return None;
            }
        }
    }
    if let JsValue::Array(values) = value {
        let len = values.len() as f64;
        if bound("minItems").is_some_and(|min| len < min)
            || bound("maxItems").is_some_and(|max| len > max)
        {
            return None;
        }
        if schema.get("uniqueItems").and_then(JsValue::as_bool) == Some(true)
            && values
                .iter()
                .enumerate()
                .any(|(index, value)| values[..index].iter().any(|old| eq(old, value)))
        {
            return None;
        }
        let prefix = array(schema, "prefixItems");
        for (index, value) in values.iter().enumerate() {
            if let Some(child) = prefix.get(index).or_else(|| schema.get("items")) {
                recurse(child, value)?;
            }
        }
        if let Some(contains) = schema.get("contains") {
            let count = values
                .iter()
                .filter(|value| recurse(contains, value).is_some())
                .count() as f64;
            if count < bound("minContains").unwrap_or(1.0)
                || bound("maxContains").is_some_and(|max| count > max)
            {
                return None;
            }
        }
    }
    if let Some(values) = value.as_object() {
        let len = values.entries().len() as f64;
        if bound("minProperties").is_some_and(|min| len < min)
            || bound("maxProperties").is_some_and(|max| len > max)
        {
            return None;
        }
        for required in array(schema, "required") {
            values.get_key(required.as_string()?)?;
        }
        for (key, item) in values.entries() {
            let property = fields(schema, "properties")
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, child)| child);
            let patterns: Vec<_> = fields(schema, "patternProperties")
                .iter()
                .filter(|(pattern_text, _)| pattern(pattern_text, key, standard))
                .collect();
            if let Some(child) = property {
                recurse(child, item)?;
                evaluated.insert(key.clone());
            }
            if property.is_none()
                && (!standard || patterns.is_empty())
                && let Some(child) = schema.get("additionalProperties")
            {
                recurse(child, item)?;
                evaluated.insert(key.clone());
            }
            for (_, child) in patterns {
                recurse(child, item)?;
                evaluated.insert(key.clone());
            }
            if let Some(names) = schema.get("propertyNames") {
                check(root, names, &JsValue::String(key.clone()), depth + 1)?;
            }
        }
        for (trigger, dependencies) in fields(schema, "dependentRequired") {
            if values.get_key(trigger).is_some()
                && let JsValue::Array(dependencies) = dependencies
            {
                for required in dependencies {
                    values.get_key(required.as_string()?)?;
                }
            }
        }
        for (trigger, child) in fields(schema, "dependentSchemas") {
            if values.get_key(trigger).is_some() {
                evaluated.extend(recurse(child, value)?);
            }
        }
    }
    for child in array(schema, "allOf") {
        evaluated.extend(recurse(child, value)?);
    }
    for (name, exactly_one) in [("anyOf", false), ("oneOf", true)] {
        if schema.get(name).is_some() {
            let successes: Vec<_> = array(schema, name)
                .iter()
                .filter_map(|child| recurse(child, value))
                .collect();
            if successes.is_empty() || (exactly_one && successes.len() != 1) {
                return None;
            }
            for result in successes {
                evaluated.extend(result);
            }
        }
    }
    if let Some(child) = schema.get("not")
        && recurse(child, value).is_some()
    {
        return None;
    }
    if let Some(condition) = schema.get("if") {
        let result = recurse(condition, value);
        let branch = if result.is_some() {
            if let Some(names) = result {
                evaluated.extend(names);
            }
            schema.get("then")
        } else {
            schema.get("else")
        };
        if let Some(branch) = branch {
            evaluated.extend(recurse(branch, value)?);
        }
    }
    Some(evaluated)
}

fn format_valid(value: &str, format: &str, standard: bool) -> bool {
    match format {
        "email" => {
            let Some((local, domain)) = value.split_once('@') else {
                return false;
            };
            !local.is_empty()
                && !domain.contains('@')
                && domain
                    .split_once('.')
                    .is_some_and(|(a, b)| !a.is_empty() && !b.is_empty())
                && !value.chars().any(crate::metadata::js_space)
        }
        "uri" => !value.chars().any(crate::metadata::js_space) && url::Url::parse(value).is_ok(),
        "uuid" => regex::Regex::new(if standard {
            r"(?i)^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$"
        } else {
            r"(?i)^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"
        })
        .unwrap()
        .is_match(value),
        "date" => date_valid(value),
        "time" => time_valid(value, standard),
        "date-time" => value
            .split_once(if standard && value.contains('t') {
                't'
            } else {
                'T'
            })
            .is_some_and(|(date, time)| date_valid(date) && time_valid(time, true)),
        _ => false,
    }
}
fn date_valid(value: &str) -> bool {
    let p: Vec<_> = value.split('-').collect();
    if p.len() != 3 || p[0].len() != 4 || p[1].len() != 2 || p[2].len() != 2 {
        return false;
    }
    if !p
        .iter()
        .all(|value| value.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return false;
    }
    let (Ok(year), Ok(month), Ok(day)) = (
        p[0].parse::<u32>(),
        p[1].parse::<u32>(),
        p[2].parse::<u32>(),
    ) else {
        return false;
    };
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let max = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if leap {
                29
            } else {
                28
            }
        }
        _ => 0,
    };
    day > 0 && day <= max
}
fn time_valid(value: &str, zone_required: bool) -> bool {
    let re = regex::Regex::new(
        r"^([0-9]{2}):([0-9]{2}):([0-9]{2})(?:\.[0-9]+)?(Z|[+-][0-9]{2}:[0-9]{2})?$",
    )
    .unwrap();
    let Some(c) = re.captures(value) else {
        return false;
    };
    if c[1].parse::<u32>().unwrap() > 23
        || c[2].parse::<u32>().unwrap() > 59
        || c[3].parse::<u32>().unwrap() > 59
    {
        return false;
    }
    let Some(zone) = c.get(4) else {
        return !zone_required;
    };
    let zone = zone.as_str();
    zone.len() == 1
        || (zone[1..3].parse::<u32>().unwrap() <= 23 && zone[4..].parse::<u32>().unwrap() <= 59)
}

fn standard_accepts(schema: &JsValue, value: &JsValue) -> bool {
    // The TS form validator refuses every external reference before compiling.
    let mut pending = vec![schema];
    while let Some(node) = pending.pop() {
        match node {
            JsValue::Array(nodes) => pending.extend(nodes),
            JsValue::Object(fields) => {
                for (key, child) in fields.entries() {
                    if ["$ref", "$dynamicRef", "$recursiveRef"]
                        .iter()
                        .any(|name| key.equals_str(name))
                        && let Some(reference) = child.as_string()
                        && reference.units().first() != Some(&35)
                    {
                        return false;
                    }
                    pending.push(child);
                }
            }
            _ => {}
        }
    }
    let json_value = |value: &JsValue| {
        stringify_json(value)
            .ok()
            .flatten()
            .and_then(|json| serde_json::from_str::<serde_json::Value>(&json).ok())
    };
    let (Some(mut schema), Some(value)) = (json_value(schema), json_value(value)) else {
        return false;
    };
    ajv_contains_annotations(&mut schema);
    jsonschema::draft202012::options()
        .should_validate_formats(true)
        .should_ignore_unknown_formats(false)
        .build(&schema)
        .is_ok_and(|validator| validator.is_valid(&value))
}

fn ajv_contains_annotations(schema: &mut serde_json::Value) {
    let Some(fields) = schema.as_object_mut() else {
        return;
    };
    // Ajv 8.20 marks the whole array evaluated when contains is present.
    if fields.contains_key("contains") {
        fields.remove("unevaluatedItems");
    }
    for key in [
        "properties",
        "patternProperties",
        "$defs",
        "dependentSchemas",
    ] {
        if let Some(children) = fields
            .get_mut(key)
            .and_then(serde_json::Value::as_object_mut)
        {
            for child in children.values_mut() {
                ajv_contains_annotations(child);
            }
        }
    }
    for key in ["allOf", "anyOf", "oneOf", "prefixItems"] {
        if let Some(children) = fields
            .get_mut(key)
            .and_then(serde_json::Value::as_array_mut)
        {
            for child in children {
                ajv_contains_annotations(child);
            }
        }
    }
    for key in [
        "items",
        "contains",
        "not",
        "if",
        "then",
        "else",
        "additionalProperties",
        "unevaluatedProperties",
        "unevaluatedItems",
        "propertyNames",
    ] {
        if let Some(child) = fields.get_mut(key) {
            ajv_contains_annotations(child);
        }
    }
}
