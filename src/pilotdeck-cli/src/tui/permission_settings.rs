//! TUI permission settings, preserving JS UTF-16 and using atomic publication.
use crate::atomic_file;
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json};
use std::{
    collections::HashSet,
    fs::OpenOptions,
    io::{self, Read},
    os::unix::fs::OpenOptionsExt,
    path::Path,
};

const MAX_BYTES: u64 = 8 * 1024 * 1024;
#[derive(Debug)]
pub enum Error {
    Atomic(atomic_file::Error),
    Io(io::Error),
    InvalidJson,
    TooLarge,
    Encode,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Atomic(e) => write!(f, "{e}"),
            Self::Io(e) => write!(f, "{e}"),
            Self::InvalidJson => write!(
                f,
                "Cannot save permission settings: existing JSON is invalid."
            ),
            Self::TooLarge => write!(f, "Permission settings exceed the 8 MiB limit."),
            Self::Encode => write!(f, "Cannot encode permission settings."),
        }
    }
}
impl std::error::Error for Error {}
impl From<atomic_file::Error> for Error {
    fn from(value: atomic_file::Error) -> Self {
        Self::Atomic(value)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    pub allowed_tools: Vec<JsString>,
    pub ask_tools: Vec<JsString>,
    pub disallowed_tools: Vec<JsString>,
    pub skip_permissions: bool,
    pub last_updated: Option<JsString>,
}
#[derive(Default)]
pub struct Patch {
    pub allowed_tools: Option<Vec<JsString>>,
    pub ask_tools: Option<Vec<JsString>>,
    pub disallowed_tools: Option<Vec<JsString>>,
    pub skip_permissions: Option<bool>,
}
fn space(u: u16) -> bool {
    matches!(u, 0x09..=0x0d | 0x20 | 0xa0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff)
}
fn trim(units: &[u16]) -> &[u16] {
    let start = units.iter().position(|u| !space(*u)).unwrap_or(units.len());
    let end = units
        .iter()
        .rposition(|u| !space(*u))
        .map_or(start, |i| i + 1);
    &units[start..end]
}
/// Alias conversion follows TS and keeps nonmatching UTF-16 units intact.
pub fn normalize_entry(entry: &JsString) -> JsString {
    let units = trim(entry.units());
    let prefix: Vec<u16> = "Bash(".encode_utf16().collect();
    if units.starts_with(&prefix) && units.last() == Some(&u16::from(b')')) {
        let pattern = &units[prefix.len()..units.len() - 1];
        if !pattern
            .iter()
            .any(|u| matches!(*u, 0x0a | 0x0d | 0x2028 | 0x2029))
        {
            let pattern = trim(pattern);
            let mut result: Vec<u16> = "bash".encode_utf16().collect();
            if !pattern.is_empty() {
                result.push(u16::from(b':'));
                result.extend_from_slice(pattern);
            }
            return JsString::from_utf16(result);
        }
    }
    let s = JsString::from_utf16(units.to_vec());
    for (alias, canonical) in [
        ("Read", "read_file"),
        ("Write", "write_file"),
        ("Edit", "edit_file"),
        ("MultiEdit", "edit_file"),
        ("NotebookEdit", "edit_notebook"),
        ("Glob", "glob"),
        ("Grep", "grep"),
        ("Bash", "bash"),
        ("Task", "agent"),
        ("TodoWrite", "todo_write"),
        ("WebFetch", "web_fetch"),
        ("WebSearch", "web_search"),
    ] {
        if s.equals_str(alias) {
            return canonical.into();
        }
    }
    s
}
fn list(value: Option<&JsValue>) -> Vec<JsString> {
    let Some(JsValue::Array(items)) = value else {
        return vec![];
    };
    let mut seen = HashSet::new();
    items
        .iter()
        .filter_map(JsValue::as_string)
        .map(normalize_entry)
        .filter(|s| !s.is_empty() && seen.insert(s.clone()))
        .collect()
}
fn truthy(value: Option<&JsValue>) -> bool {
    match value {
        None | Some(JsValue::Undefined | JsValue::Null | JsValue::Bool(false)) => false,
        Some(JsValue::Number(n)) => *n != 0.0 && !n.is_nan(),
        Some(JsValue::String(s)) => !s.is_empty(),
        _ => true,
    }
}
pub fn normalize(value: &JsValue) -> Settings {
    Settings {
        allowed_tools: list(value.get("allowedTools")),
        ask_tools: list(value.get("askTools")),
        disallowed_tools: list(value.get("disallowedTools")),
        skip_permissions: truthy(value.get("skipPermissions")),
        last_updated: value
            .get("lastUpdated")
            .and_then(JsValue::as_string)
            .cloned(),
    }
}
impl Settings {
    pub fn to_js(&self) -> JsValue {
        let strings = |items: &[JsString]| {
            JsValue::Array(items.iter().cloned().map(JsValue::String).collect())
        };
        let mut object = JsObject::new();
        for (name, value) in [
            ("version", JsValue::Number(1.0)),
            ("allowedTools", strings(&self.allowed_tools)),
            ("askTools", strings(&self.ask_tools)),
            ("disallowedTools", strings(&self.disallowed_tools)),
            ("skipPermissions", JsValue::Bool(self.skip_permissions)),
        ] {
            object.insert(name.into(), value);
        }
        if let Some(updated) = &self.last_updated {
            object.insert("lastUpdated".into(), JsValue::String(updated.clone()));
        }
        JsValue::Object(object)
    }
}
fn bytes(path: &Path) -> Result<Vec<u8>, Error> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .map_err(Error::Io)?;
    if !file.metadata().map_err(Error::Io)?.is_file() {
        return Err(Error::Io(io::Error::other(
            "permission settings must be a regular file",
        )));
    }
    let mut bytes = Vec::new();
    file.by_ref()
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(Error::Io)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(Error::TooLarge);
    }
    Ok(bytes)
}
fn decode(bytes: &[u8]) -> Result<Settings, Error> {
    let text = String::from_utf8_lossy(bytes);
    parse_json(&text)
        .map(|v| normalize(&v))
        .map_err(|_| Error::InvalidJson)
}
/// TS defaults on missing, unreadable and invalid JSON; oversize is an explicit error.
pub fn read(home: &Path) -> Result<Settings, Error> {
    match bytes(&home.join("permissions.json")) {
        Ok(raw) => Ok(decode(&raw).unwrap_or_default()),
        Err(Error::TooLarge) => Err(Error::TooLarge),
        Err(_) => Ok(Settings::default()),
    }
}
/// Prepares and verifies the target before strict read; failed writes never replace old bytes.
pub fn write_at(home: &Path, patch: Patch, timestamp: &str) -> Result<Settings, Error> {
    let path = home.join("permissions.json");
    let target = atomic_file::Target::prepare(&path)?;
    let old = match bytes(&path) {
        Ok(raw) => decode(&raw)?,
        Err(Error::Io(error)) if error.kind() == io::ErrorKind::NotFound => Settings::default(),
        Err(error) => return Err(error),
    };
    let next = Settings {
        allowed_tools: patch.allowed_tools.unwrap_or(old.allowed_tools),
        ask_tools: patch.ask_tools.unwrap_or(old.ask_tools),
        disallowed_tools: patch.disallowed_tools.unwrap_or(old.disallowed_tools),
        skip_permissions: patch.skip_permissions.unwrap_or(old.skip_permissions),
        last_updated: Some(timestamp.into()),
    };
    let next = normalize(&next.to_js());
    let mut encoded = super::state::pretty(&next.to_js())
        .map_err(|_| Error::Encode)?
        .ok_or(Error::Encode)?;
    encoded.push('\n');
    if encoded.len() as u64 > MAX_BYTES {
        return Err(Error::TooLarge);
    }
    target.publish(encoded.as_bytes())?;
    Ok(next)
}
pub fn write(home: &Path, patch: Patch) -> Result<Settings, Error> {
    write_at(
        home,
        patch,
        &chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use pilotdeck_protocol::stringify_json;
    use std::fs;
    struct Scratch(std::path::PathBuf);
    impl Scratch {
        fn new() -> Self {
            let p =
                std::env::temp_dir().join(format!("pdrs-tui-permission-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn compact(value: &JsValue) -> String {
        stringify_json(value).unwrap().unwrap()
    }
    fn array(value: &JsValue) -> &[JsValue] {
        match value {
            JsValue::Array(a) => a,
            _ => panic!("expected array"),
        }
    }
    fn utf8(value: &JsValue) -> String {
        value.as_string().unwrap().to_utf8().unwrap()
    }
    #[test]
    fn recorded_typescript_normalization_and_roundtrip() {
        let fixture =
            parse_json(include_str!("../../tests/fixtures/tui/permissions.json")).unwrap();
        for row in array(fixture.get("normalized").unwrap()) {
            assert_eq!(
                compact(&normalize(row.get("input").unwrap()).to_js()),
                compact(row.get("output").unwrap())
            );
        }
        let cases = array(fixture.get("rawNormalized").unwrap());
        assert_eq!(cases.len(), 3);
        for row in cases {
            let input = parse_json(&utf8(row.get("raw").unwrap())).unwrap();
            assert_eq!(
                compact(&normalize(&input).to_js()),
                utf8(row.get("output").unwrap())
            );
        }
        let home = Scratch::new();
        assert_eq!(
            compact(&read(&home.0).unwrap().to_js()),
            compact(fixture.get("missing").unwrap())
        );
        fs::write(home.0.join("permissions.json"), r#"{"allowedTools":["Read"],"askTools":["Grep"],"disallowedTools":["Write"],"skipPermissions":false}"#).unwrap();
        let result = write_at(
            &home.0,
            Patch {
                allowed_tools: Some(vec!["Read".into(), "Bash(git status)".into()]),
                skip_permissions: Some(true),
                ..Patch::default()
            },
            "2026-11-08T00:00:00.000Z",
        )
        .unwrap();
        assert_eq!(
            compact(&result.to_js()),
            compact(fixture.get("written").unwrap())
        );
        assert_eq!(
            fs::read_to_string(home.0.join("permissions.json")).unwrap(),
            utf8(fixture.get("bytes").unwrap())
        );
        let raw = utf8(cases[0].get("raw").unwrap());
        fs::write(home.0.join("permissions.json"), raw).unwrap();
        write_at(
            &home.0,
            Patch {
                skip_permissions: Some(true),
                ..Patch::default()
            },
            "2026-11-08T00:00:00.000Z",
        )
        .unwrap();
        let result = read(&home.0).unwrap();
        assert_eq!(result.allowed_tools[0].units(), &[0xd800]);
        assert_eq!(result.disallowed_tools[0].units(), &[0xdfff]);
        assert!(result.skip_permissions);
    }
    #[test]
    fn malformed_write_refuses_without_destroying_original() {
        let home = Scratch::new();
        let path = home.0.join("permissions.json");
        fs::write(&path, b"{bad").unwrap();
        assert_eq!(read(&home.0).unwrap(), Settings::default());
        assert!(matches!(
            write_at(
                &home.0,
                Patch {
                    skip_permissions: Some(true),
                    ..Patch::default()
                },
                "2026-11-08T00:00:00.000Z"
            ),
            Err(Error::InvalidJson)
        ));
        assert_eq!(fs::read(&path).unwrap(), b"{bad");
    }
    #[test]
    fn unsafe_target_and_over_budget_fail_without_replacement() {
        let home = Scratch::new();
        let path = home.0.join("permissions.json");
        fs::create_dir(&path).unwrap();
        fs::write(path.join("sentinel"), b"keep").unwrap();
        assert!(write_at(&home.0, Patch::default(), "now").is_err());
        assert_eq!(fs::read(path.join("sentinel")).unwrap(), b"keep");
        fs::remove_dir_all(&path).unwrap();
        let original = vec![b' '; MAX_BYTES as usize + 1];
        fs::write(&path, &original).unwrap();
        assert!(matches!(read(&home.0), Err(Error::TooLarge)));
        assert!(matches!(
            write_at(&home.0, Patch::default(), "now"),
            Err(Error::TooLarge)
        ));
        assert_eq!(fs::read(&path).unwrap(), original);
        fs::write(&path, b"{}").unwrap();
        let giant: JsString = "a".repeat(MAX_BYTES as usize).into();
        assert!(matches!(
            write_at(
                &home.0,
                Patch {
                    allowed_tools: Some(vec![giant]),
                    ..Patch::default()
                },
                "now"
            ),
            Err(Error::TooLarge)
        ));
        assert_eq!(fs::read(&path).unwrap(), b"{}");
    }
    #[test]
    #[ignore = "spawned by real_write_failure_preserves_permission_file"]
    fn permission_write_child_over_size_limit() {
        let home = std::path::PathBuf::from(std::env::var_os("PDRS_TUI_PERMISSION_HOME").unwrap());
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async { pilotdeck_gateway::ensure_sigxfsz_ignored() });
        let error = write_at(
            &home,
            Patch {
                skip_permissions: Some(true),
                ..Patch::default()
            },
            "2026-11-08T00:00:00.000Z",
        )
        .unwrap_err();
        match error {
            Error::Atomic(atomic_file::Error::Io {
                error, operation, ..
            }) => {
                assert_eq!(operation, "write");
                assert_eq!(error.raw_os_error(), Some(libc::EFBIG));
                println!("PERMISSION_WRITE_EFBIG");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }
    #[test]
    fn real_write_failure_preserves_permission_file() {
        let home = Scratch::new();
        let path = home.0.join("permissions.json");
        let original = br#"{"disallowedTools":["Bash"],"skipPermissions":false}"#;
        fs::write(&path, original).unwrap();
        let child = concat!(module_path!(), "::permission_write_child_over_size_limit")
            .split_once("::")
            .unwrap()
            .1;
        let output = std::process::Command::new("/bin/sh")
            .arg("-c").arg("ulimit -f 0 || exit 1; exec \"$0\" --exact \"$1\" --ignored --nocapture --test-threads=2")
            .arg(std::env::current_exe().unwrap()).arg(child)
            .env("PDRS_TUI_PERMISSION_HOME", &home.0).output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
        assert!(
            stdout.lines().any(|line| line == "PERMISSION_WRITE_EFBIG"),
            "child receipt missing: {stdout}{stderr}"
        );
        assert_eq!(fs::read(&path).unwrap(), original);
        assert_eq!(
            fs::read_dir(&home.0).unwrap().count(),
            1,
            "temporary file must be removed"
        );
    }
}
