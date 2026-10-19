//! Fixed b243 file/model projection plus constructor restoration. No filesystem IO.
//! Native Map/prototype/alias input profiles cannot be represented by JsValue.
use crate::ports::{PortError, PortResult, Route, TokenCalibration};
use crate::session_state::*;
use pilotdeck_protocol::model::BoundarySnapshot;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::collections::{HashMap, HashSet};

fn error(message: impl Into<JsString>) -> PortError {
    PortError::InvalidResponse {
        message: message.into(),
    }
}
fn absent(v: Option<&JsValue>) -> bool {
    v.is_none_or(JsValue::is_undefined)
}
fn nullish(v: &JsValue) -> bool {
    matches!(v, JsValue::Null | JsValue::Undefined)
}
fn record<'a>(v: &'a JsValue, message: &str) -> PortResult<&'a JsObject> {
    v.as_object().ok_or_else(|| error(message))
}
fn field_error(prefix: &str, key: &JsString) -> PortError {
    let mut units: Vec<u16> = prefix.encode_utf16().collect();
    units.extend_from_slice(key.units());
    units.push(u16::from(b'.'));
    error(JsString::from_utf16(units))
}
fn entries(obj: &JsObject) -> Vec<(&JsString, &JsValue)> {
    let mut indexed = Vec::new();
    let mut other = Vec::new();
    for (key, value) in obj.entries() {
        if let Some(index) = key.array_index() {
            indexed.push((index, key, value));
        } else {
            other.push((key, value));
        }
    }
    indexed.sort_by_key(|(index, _, _)| *index);
    indexed
        .into_iter()
        .map(|(_, key, value)| (key, value))
        .chain(other)
        .collect()
}
fn optional_number(obj: &JsObject, key: &str) -> Option<f64> {
    obj.get(key).and_then(JsValue::as_number)
}
fn kind(v: Option<&JsValue>) -> Option<ReadKind> {
    let s = v?.as_string()?;
    if s.equals_str("text") {
        Some(ReadKind::Text)
    } else if s.equals_str("image") {
        Some(ReadKind::Image)
    } else if s.equals_str("pdf") {
        Some(ReadKind::Pdf)
    } else if s.equals_str("notebook") {
        Some(ReadKind::Notebook)
    } else {
        None
    }
}
pub(crate) fn parse_file(value: &JsValue) -> PortResult<Option<AgentFileState>> {
    if nullish(value) {
        return Ok(None);
    }
    let source = record(value, "Invalid sidecar seedState: expected an object.")?;
    let mut state = AgentFileState::default();
    if !absent(source.get("allowedReadFiles")) {
        let Some(JsValue::Array(paths)) = source.get("allowedReadFiles") else {
            return Err(error("Invalid sidecar seedState.allowedReadFiles."));
        };
        // The parser validates the entire array before the constructor Set deduplicates.
        if paths.iter().any(|v| v.as_string().is_none()) {
            return Err(error("Invalid sidecar seedState.allowedReadFiles."));
        }
        let mut seen = HashSet::new();
        for path in paths.iter().filter_map(JsValue::as_string) {
            if seen.insert(path.clone()) {
                state.allowed_read_files.push(path.clone());
            }
        }
    }
    if !absent(source.get("readFileState")) {
        let source = record(
            source.get("readFileState").expect("present"),
            "Invalid sidecar seedState.readFileState.",
        )?;
        for (path, raw) in entries(source) {
            let entry = raw.as_object();
            let mtime = entry
                .and_then(|v| v.get("mtimeMs"))
                .and_then(JsValue::as_number);
            let read_kind = entry.and_then(|v| kind(v.get("kind")));
            let (Some(entry), Some(mtime_ms), Some(kind)) = (entry, mtime, read_kind) else {
                return Err(field_error("Invalid readFileState entry: ", path));
            };
            state.read_file_state.push((
                path.clone(),
                ReadFileEntry {
                    mtime_ms,
                    kind,
                    offset: optional_number(entry, "offset"),
                    limit: optional_number(entry, "limit"),
                    pages: entry.get("pages").and_then(JsValue::as_string).cloned(),
                },
            ));
        }
    }
    if !absent(source.get("writeSnapshots")) {
        let source = record(
            source.get("writeSnapshots").expect("present"),
            "Invalid sidecar seedState.writeSnapshots.",
        )?;
        for (path, raw) in entries(source) {
            let entry = raw.as_object();
            let absolute = entry
                .and_then(|v| v.get("absolutePath"))
                .and_then(JsValue::as_string);
            let mtime = entry
                .and_then(|v| v.get("mtimeMs"))
                .and_then(JsValue::as_number);
            let hash = entry
                .and_then(|v| v.get("contentHash"))
                .and_then(JsValue::as_string);
            let (Some(entry), Some(absolute), Some(mtime_ms), Some(hash)) =
                (entry, absolute, mtime, hash)
            else {
                return Err(field_error("Invalid writeSnapshots entry: ", path));
            };
            state.write_snapshots.push((
                path.clone(),
                WriteSnapshotEntry {
                    absolute_path: absolute.clone(),
                    mtime_ms,
                    content_hash: hash.clone(),
                    offset: optional_number(entry, "offset"),
                    limit: optional_number(entry, "limit"),
                },
            ));
        }
    }
    Ok(Some(state))
}
fn array<'a>(obj: &'a JsObject, key: &str) -> PortResult<&'a [JsValue]> {
    match obj.get(key) {
        None | Some(JsValue::Undefined) => Ok(&[]),
        Some(JsValue::Array(values)) => Ok(values),
        _ => Err(error(format!(
            "Invalid sidecar modelState.{key}: expected an array."
        ))),
    }
}
fn nonempty(obj: &JsObject, key: &str, field: &str) -> PortResult<JsString> {
    obj.get(key)
        .and_then(JsValue::as_string)
        .filter(|s| !s.is_empty())
        .cloned()
        .ok_or_else(|| error(format!("Invalid sidecar {field}.")))
}
fn positive(obj: &JsObject, key: &str, field: &str) -> PortResult<f64> {
    obj.get(key)
        .and_then(JsValue::as_number)
        .filter(|v| v.is_finite() && *v > 0.0)
        .ok_or_else(|| error(format!("Invalid sidecar {field}.")))
}
fn cap(obj: &JsObject, key: &str) -> PortResult<Option<f64>> {
    if absent(obj.get(key)) {
        Ok(None)
    } else {
        positive(obj, key, &format!("modelState cap {key}")).map(Some)
    }
}
fn route_key(route: &Route) -> JsString {
    let mut key = route.provider.units().to_vec();
    key.push(0);
    key.extend_from_slice(route.model.units());
    JsString::from_utf16(key)
}
fn map_set<T>(
    values: &mut Vec<T>,
    indices: &mut HashMap<JsString, usize>,
    key: JsString,
    value: T,
) {
    if let Some(&slot) = indices.get(&key) {
        values[slot] = value;
    } else {
        indices.insert(key, values.len());
        values.push(value);
    }
}
pub(crate) fn parse_model(value: &JsValue) -> PortResult<Option<AgentModelSessionState>> {
    if nullish(value) {
        return Ok(None);
    }
    let source = record(value, "Invalid sidecar modelState: expected an object.")?;
    let mut state = AgentModelSessionState::default();
    let mut calibration_indices = HashMap::new();
    for raw in array(source, "tokenCalibration")? {
        let entry = record(raw, "Invalid sidecar modelState calibration entry.")?;
        let provider = nonempty(entry, "provider", "modelState calibration provider")?;
        let model = nonempty(entry, "model", "modelState calibration model")?;
        let actual_input_tokens = positive(
            entry,
            "actualInputTokens",
            "modelState calibration actualInputTokens",
        )?;
        let estimated_input_tokens = positive(
            entry,
            "estimatedInputTokens",
            "modelState calibration estimatedInputTokens",
        )?;
        let route = Route { provider, model };
        let key = route_key(&route);
        map_set(
            &mut state.token_calibration,
            &mut calibration_indices,
            key,
            TokenCalibration {
                route,
                actual_input_tokens,
                estimated_input_tokens,
            },
        );
    }
    let mut cap_indices = HashMap::new();
    for raw in array(source, "tokenCaps")? {
        let entry = record(raw, "Invalid sidecar modelState cap entry.")?;
        let max_context_tokens = cap(entry, "maxContextTokens")?;
        let session_max_output_tokens = cap(entry, "sessionMaxOutputTokens")?;
        let hard_max_output_tokens = cap(entry, "hardMaxOutputTokens")?;
        if max_context_tokens.is_none()
            && session_max_output_tokens.is_none()
            && hard_max_output_tokens.is_none()
        {
            return Err(error(
                "Invalid sidecar modelState cap entry without a persistent cap.",
            ));
        }
        let provider = nonempty(entry, "provider", "modelState cap provider")?;
        let model = nonempty(entry, "model", "modelState cap model")?;
        let route = Route { provider, model };
        let key = route_key(&route);
        map_set(
            &mut state.token_caps,
            &mut cap_indices,
            key,
            PersistentTokenCap {
                route,
                max_context_tokens,
                session_max_output_tokens,
                hard_max_output_tokens,
            },
        );
    }
    Ok(Some(state))
}
fn obj(fields: impl IntoIterator<Item = (&'static str, JsValue)>) -> JsValue {
    let mut obj = JsObject::new();
    for (key, value) in fields {
        obj.insert(key.into(), value);
    }
    JsValue::Object(obj)
}
fn put_number(obj: &mut JsObject, key: &str, value: Option<f64>) {
    if let Some(value) = value {
        obj.insert(key.into(), JsValue::Number(value));
    }
}
pub(crate) fn snapshot_file(state: &AgentFileState) -> BoundarySnapshot {
    let mut reads = JsObject::new();
    for (path, entry) in &state.read_file_state {
        let kind = match entry.kind {
            ReadKind::Text => "text",
            ReadKind::Image => "image",
            ReadKind::Pdf => "pdf",
            ReadKind::Notebook => "notebook",
        };
        let mut raw = JsObject::new();
        raw.insert("mtimeMs".into(), JsValue::Number(entry.mtime_ms));
        raw.insert("kind".into(), kind.into());
        put_number(&mut raw, "offset", entry.offset);
        put_number(&mut raw, "limit", entry.limit);
        if let Some(pages) = &entry.pages {
            raw.insert("pages".into(), JsValue::String(pages.clone()));
        }
        reads.insert(path.clone(), JsValue::Object(raw));
    }
    let mut writes = JsObject::new();
    for (path, entry) in &state.write_snapshots {
        let mut raw = JsObject::new();
        raw.insert(
            "absolutePath".into(),
            JsValue::String(entry.absolute_path.clone()),
        );
        raw.insert("mtimeMs".into(), JsValue::Number(entry.mtime_ms));
        raw.insert(
            "contentHash".into(),
            JsValue::String(entry.content_hash.clone()),
        );
        put_number(&mut raw, "offset", entry.offset);
        put_number(&mut raw, "limit", entry.limit);
        writes.insert(path.clone(), JsValue::Object(raw));
    }
    BoundarySnapshot::capture(&obj([
        (
            "allowedReadFiles",
            JsValue::Array(
                state
                    .allowed_read_files
                    .iter()
                    .cloned()
                    .map(JsValue::String)
                    .collect(),
            ),
        ),
        ("readFileState", JsValue::Object(reads)),
        ("writeSnapshots", JsValue::Object(writes)),
    ]))
}
pub(crate) fn snapshot_model(state: &AgentModelSessionState) -> BoundarySnapshot {
    let calibration = state
        .token_calibration
        .iter()
        .map(|entry| {
            obj([
                ("provider", JsValue::String(entry.route.provider.clone())),
                ("model", JsValue::String(entry.route.model.clone())),
                (
                    "actualInputTokens",
                    JsValue::Number(entry.actual_input_tokens),
                ),
                (
                    "estimatedInputTokens",
                    JsValue::Number(entry.estimated_input_tokens),
                ),
            ])
        })
        .collect::<Vec<_>>();
    let caps = state
        .token_caps
        .iter()
        .filter_map(|entry| {
            if entry.max_context_tokens.is_none()
                && entry.session_max_output_tokens.is_none()
                && entry.hard_max_output_tokens.is_none()
            {
                return None;
            }
            let key = route_key(&entry.route);
            let mut parts = key.units().split(|u| *u == 0);
            let provider = parts.next()?;
            let model = parts.next()?;
            if provider.is_empty() || model.is_empty() {
                return None;
            }
            let mut raw = JsObject::new();
            raw.insert(
                "provider".into(),
                JsValue::String(JsString::from_utf16(provider.to_vec())),
            );
            raw.insert(
                "model".into(),
                JsValue::String(JsString::from_utf16(model.to_vec())),
            );
            put_number(&mut raw, "maxContextTokens", entry.max_context_tokens);
            put_number(
                &mut raw,
                "sessionMaxOutputTokens",
                entry.session_max_output_tokens,
            );
            put_number(
                &mut raw,
                "hardMaxOutputTokens",
                entry.hard_max_output_tokens,
            );
            Some(JsValue::Object(raw))
        })
        .collect::<Vec<_>>();
    let mut raw = JsObject::new();
    if !calibration.is_empty() {
        raw.insert("tokenCalibration".into(), JsValue::Array(calibration));
    }
    if !caps.is_empty() {
        raw.insert("tokenCaps".into(), JsValue::Array(caps));
    }
    BoundarySnapshot::capture(&JsValue::Object(raw))
}
