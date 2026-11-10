//! Host-owned persisted settings, re-read at turn admission. No tool payload is authority.
use crate::config::permission::{PermissionRule, PermissionRuleSet};
use pilotdeck_protocol::{JsString, JsValue, parse_json};
use std::{collections::HashSet, io::Read, path::Path};
#[derive(Default)]
pub(super) struct Settings {
    pub rules: PermissionRuleSet,
    pub skip_permissions: bool,
    pub error: Option<JsString>,
}
fn normalize_entry(entry: &JsString) -> JsString {
    let units = crate::runtime::memory::js_trim_units(entry.units());
    if units.starts_with(&[66, 97, 115, 104, 40])
        && units.last() == Some(&41)
        && !units.iter().any(|u| matches!(u, 10 | 13 | 0x2028 | 0x2029))
    {
        let pattern = crate::runtime::memory::js_trim_units(&units[5..units.len() - 1]);
        let mut out: Vec<u16> = "bash".encode_utf16().collect();
        if !pattern.is_empty() {
            out.push(58);
            out.extend_from_slice(pattern);
        }
        return JsString::from_utf16(out);
    }
    for (alias, name) in [
        ("Read", "read_file"),
        ("Write", "write_file"),
        ("Edit", "edit_file"),
        ("NotebookEdit", "edit_notebook"),
        ("MultiEdit", "edit_file"),
        ("Glob", "glob"),
        ("Grep", "grep"),
        ("Bash", "bash"),
        ("Task", "agent"),
        ("TodoWrite", "todo_write"),
        ("WebFetch", "web_fetch"),
        ("WebSearch", "web_search"),
    ] {
        if units.iter().copied().eq(alias.encode_utf16()) {
            return name.into();
        }
    }
    JsString::from_utf16(units.to_vec())
}
fn entries(value: Option<&JsValue>) -> Vec<JsString> {
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
fn rule(entry: JsString, behavior: &str) -> PermissionRule {
    // Reuse the independently implemented session/policy entry parser; source
    // is reset to user so persisted allow can never become a policy bypass.
    let mut rule = crate::runtime::interaction::policy_rule(&entry, false);
    rule.source = "user".into();
    rule.behavior = behavior.into();
    rule
}
pub(super) fn normalize(value: &JsValue) -> Settings {
    let list = |name, behavior| {
        entries(value.get(name))
            .into_iter()
            .map(|e| rule(e, behavior))
            .collect()
    };
    let skip_permissions = match value.get("skipPermissions") {
        None | Some(JsValue::Undefined | JsValue::Null | JsValue::Bool(false)) => false,
        Some(JsValue::Number(n)) => *n != 0.0 && !n.is_nan(),
        Some(JsValue::String(s)) => !s.is_empty(),
        _ => true,
    };
    Settings {
        rules: PermissionRuleSet {
            allow: list("allowedTools", "allow"),
            deny: list("disallowedTools", "deny"),
            ask: list("askTools", "ask"),
        },
        skip_permissions,
        error: None,
    }
}
pub(super) fn read(home: &Path) -> Settings {
    fn load(path: &Path) -> Option<Settings> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NONBLOCK);
        }
        let file = options.open(path).ok()?;
        if !file.metadata().ok()?.is_file() {
            return None;
        }
        let mut bytes = Vec::new();
        const MAX_BYTES: u64 = 8 * 1024 * 1024; // Same permissions limit as TUI settings.
        file.take(MAX_BYTES + 1).read_to_end(&mut bytes).ok()?;
        if bytes.len() as u64 > MAX_BYTES {
            return Some(Settings {
                error: Some("Permission settings exceed the 8 MiB limit.".into()),
                ..Settings::default()
            });
        }
        parse_json(&String::from_utf8_lossy(&bytes))
            .ok()
            .map(|v| normalize(&v))
    }
    load(&home.join("permissions.json")).unwrap_or_default()
}
/// The production async admission reads only settings on the blocking pool.
/// Tool/SessionPorts construction stays on the original async executor.
pub(super) async fn read_async(
    home: std::path::PathBuf,
) -> Result<Settings, crate::host::HostError> {
    tokio::task::spawn_blocking(move || read(&home))
        .await
        .map_err(|error| {
            crate::host::HostError::new(
                crate::host::HostErrorKind::Session,
                format!("Permission settings read task failed: {error}"),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixed_ts_normalization_oracle_preserves_aliases_utf16_and_truthiness() {
        let oracle = parse_json(include_str!(
            "../../../pilotdeck-cli/tests/fixtures/tui/permissions.json"
        ))
        .unwrap();
        let JsValue::Array(rows) = oracle.get("normalized").unwrap() else {
            panic!("rows")
        };
        for row in rows {
            let actual = normalize(row.get("input").unwrap());
            let expected = normalize(row.get("output").unwrap());
            assert_eq!(actual.rules, expected.rules);
            assert_eq!(actual.skip_permissions, expected.skip_permissions);
        }
        let JsValue::Array(rows) = oracle.get("rawNormalized").unwrap() else {
            panic!("rows")
        };
        for row in rows {
            let input = parse_json(
                &row.get("raw")
                    .unwrap()
                    .as_string()
                    .unwrap()
                    .to_utf8()
                    .unwrap(),
            )
            .unwrap();
            let output = parse_json(
                &row.get("output")
                    .unwrap()
                    .as_string()
                    .unwrap()
                    .to_utf8()
                    .unwrap(),
            )
            .unwrap();
            let actual = normalize(&input);
            let expected = normalize(&output);
            assert_eq!(actual.rules, expected.rules);
            assert_eq!(actual.skip_permissions, expected.skip_permissions);
        }
    }
    #[test]
    fn allow_deny_ask_are_user_rules_with_exact_bash_pattern() {
        let s=normalize(&parse_json(r#"{"allowedTools":["Read","read_file","Bash( echo:a )"],"disallowedTools":["mcp__funasr__transcribe_audio"],"askTools":["Write"],"skipPermissions":"false"}"#).unwrap());
        assert_eq!(s.rules.allow.len(), 2);
        assert_eq!(
            s.rules.allow[0],
            PermissionRule::new("user", "allow", "read_file")
        );
        assert_eq!(
            s.rules.allow[1],
            PermissionRule::new("user", "allow", "bash").with_pattern("echo:a")
        );
        assert_eq!(s.rules.deny[0].source, JsString::from("user"));
        assert_eq!(s.rules.ask[0].behavior, JsString::from("ask"));
        assert!(s.skip_permissions);
    }
    #[test]
    fn bounded_read_reports_oversize_without_discarding_deny_and_continuing() {
        let root =
            std::env::temp_dir().join(format!("pdrs-permission-read-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let file = root.join("permissions.json");
        std::fs::write(&file, b"invalid json").unwrap();
        let malformed = read(&root);
        assert!(malformed.error.is_none());
        assert_eq!(malformed.rules, PermissionRuleSet::default());
        assert!(!malformed.skip_permissions);
        std::fs::write(&file, vec![b' '; 8 * 1024 * 1024 + 1]).unwrap();
        assert!(
            read(&root).error.is_some(),
            "oversize must fail the catalog rather than run with empty deny"
        );
        std::fs::remove_file(file).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
}
