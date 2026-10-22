//! Shared fixture setup and wire normalization for builtin tool oracles.
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use pilotdeck_gateway::runtime::tools::ToolOutput;
use pilotdeck_protocol::{JsObject, JsValue, stringify_json};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    time::{Duration, UNIX_EPOCH},
};

pub struct Scratch {
    pub root: PathBuf,
    pub outside: PathBuf,
}
impl Scratch {
    pub fn new() -> Self {
        let base = fs::canonicalize(std::env::temp_dir()).expect("canonical temp");
        let unique = format!("pdrs-oracle-replay-{}", uuid::Uuid::new_v4());
        let root = base.join(&unique);
        let outside = base.join(format!("{unique}-outside"));
        for path in [&root, &outside] {
            match fs::create_dir(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    panic!("scratch path already exists: {}", path.display())
                }
                Err(error) => panic!("create {}: {error}", path.display()),
            }
        }
        Self { root, outside }
    }
    pub fn text(&self, path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let base = fs::canonicalize(std::env::temp_dir()).expect("canonical temp");
        for path in [&self.outside, &self.root] {
            let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.clone());
            assert!(
                canonical.starts_with(&base),
                "refusing to remove {} outside {}",
                canonical.display(),
                base.display()
            );
            let _ = fs::remove_dir_all(canonical);
        }
    }
}

pub fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    static UNDEFINED: JsValue = JsValue::Undefined;
    value.get(key).unwrap_or(&UNDEFINED)
}

pub fn text(value: &JsValue) -> String {
    value
        .as_string()
        .expect("fixture string")
        .to_utf8()
        .expect("fixture UTF-8 text")
}

pub fn number(value: &JsValue) -> Option<f64> {
    value.as_number()
}

pub fn array(value: &JsValue) -> &[JsValue] {
    match value {
        JsValue::Array(values) => values,
        _ => panic!("fixture array"),
    }
}

pub fn optional_array(value: &JsValue) -> &[JsValue] {
    match value {
        JsValue::Array(values) => values,
        _ => &[],
    }
}

pub fn object(entries: impl IntoIterator<Item = (&'static str, JsValue)>) -> JsObject {
    let mut object = JsObject::new();
    for (key, value) in entries {
        object.insert(key.into(), value);
    }
    object
}

pub fn wire(value: &JsValue) -> String {
    stringify_json(value).unwrap().unwrap_or_default()
}

pub fn substitute(value: &JsValue, scratch: &Scratch) -> JsValue {
    match value {
        JsValue::String(value) => {
            let value = value
                .to_utf8()
                .unwrap()
                .replace("<outside>", &scratch.text(&scratch.outside))
                .replace("<root>", &scratch.text(&scratch.root));
            JsValue::from(value.as_str())
        }
        JsValue::Array(values) => JsValue::Array(
            values
                .iter()
                .map(|value| substitute(value, scratch))
                .collect(),
        ),
        JsValue::Object(object) => {
            let mut result = JsObject::new();
            for (key, value) in object.entries() {
                result.insert(key.clone(), substitute(value, scratch));
            }
            JsValue::Object(result)
        }
        other => other.clone(),
    }
}

pub fn apply_step(step: &JsValue, scratch: &Scratch) -> Result<(), String> {
    let op = text(field(step, "op"));
    let path = field(step, "path")
        .as_string()
        .map(|value| value.to_utf8().unwrap())
        .unwrap_or_default();
    let absolute = scratch.root.join(&path);
    match op.as_str() {
        "file" => {
            if let Some(parent) = absolute.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let content = if let Some(encoded) = field(step, "base64").as_string() {
                BASE64
                    .decode(encoded.to_utf8().unwrap())
                    .map_err(|e| e.to_string())?
            } else if let Some(repeat) = field(step, "repeat").as_object() {
                let unit = field(&JsValue::Object(repeat.clone()), "unit")
                    .as_string()
                    .ok_or("repeat.unit")?
                    .to_utf8()
                    .unwrap();
                let count = field(&JsValue::Object(repeat.clone()), "count")
                    .as_number()
                    .ok_or("repeat.count")? as usize;
                unit.repeat(count).into_bytes()
            } else {
                text(field(step, "content")).into_bytes()
            };
            fs::write(&absolute, content).map_err(|e| e.to_string())?;
            if let Some(mtime) = number(field(step, "mtime")) {
                let duration = Duration::try_from_secs_f64(mtime.abs() / 1_000.0)
                    .map_err(|e| e.to_string())?;
                let taken = if mtime < 0.0 {
                    UNIX_EPOCH - duration
                } else {
                    UNIX_EPOCH + duration
                };
                fs::OpenOptions::new()
                    .write(true)
                    .open(&absolute)
                    .and_then(|file| file.set_modified(taken))
                    .map_err(|e| e.to_string())?;
            }
        }
        "dir" => fs::create_dir_all(&absolute).map_err(|e| e.to_string())?,
        "symlink" => {
            let target = text(field(step, "target"));
            let target = target.replace("<outside>", &scratch.text(&scratch.outside));
            if let Some(parent) = absolute.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            symlink(target, &absolute).map_err(|e| e.to_string())?;
        }
        "chmod" => {
            let mode = number(field(step, "mode")).ok_or("chmod mode")? as u32;
            fs::set_permissions(&absolute, fs::Permissions::from_mode(mode))
                .map_err(|e| e.to_string())?;
        }
        "sleep" => std::thread::sleep(Duration::from_millis(
            number(field(step, "ms")).ok_or("sleep ms")? as u64,
        )),
        other => return Err(format!("unknown setup step {other}")),
    }
    Ok(())
}

pub fn normalize(value: &JsValue, scratch: &Scratch) -> String {
    wire(value)
        .replace(&scratch.text(&scratch.outside), "<outside>")
        .replace(&scratch.text(&scratch.root), "<root>")
}

pub fn futures_block<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

pub fn error_value(error: &pilotdeck_gateway::runtime::tools::ToolError) -> JsValue {
    JsValue::Object(object([(
        "error",
        JsValue::Object(object([
            ("code", JsValue::from(error.code.as_str())),
            ("message", JsValue::String(error.message.clone())),
        ])),
    )]))
}

pub fn output_value(output: ToolOutput) -> JsValue {
    let mut value = object([("content", JsValue::Array(output.content))]);
    if let Some(data) = output.data {
        value.insert("data".into(), data);
    }
    if let Some(metadata) = output.metadata {
        value.insert("metadata".into(), JsValue::Object(metadata));
    }
    JsValue::Object(value)
}

/// Null execution records (e.g. rejected validation) never count as compared.
pub fn expected_execute(call: &JsValue) -> Option<&JsValue> {
    let expected = field(call, "execute");
    if matches!(expected, JsValue::Null) {
        None
    } else {
        Some(expected)
    }
}
