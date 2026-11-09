//! Native localSettings writer. Validates a candidate before atomic publication.
use super::{LocalSettingsUpdate, LocalSettingsWriter, SettingsError, edit, normalize};
use crate::config::Env;
use pilotdeck_protocol::{JsString, JsValue};
use std::{fs, io::Write, path::PathBuf};

// Matches config::bounded FILE_LIMIT; validation remains bounded there too.
const LIMIT: usize = 8 * 1024 * 1024;
fn error(code: &str, prefix: &str, detail: impl std::fmt::Display) -> SettingsError {
    SettingsError::local_settings(code, format!("{prefix}: {detail}"))
}
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
pub struct NativeLocalSettingsWriter {
    pub home: PathBuf,
    pub project: PathBuf,
    pub env: Env,
}
impl NativeLocalSettingsWriter {
    fn validate(&self, text: &str) -> Result<(), SettingsError> {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        let path =
            std::env::temp_dir().join(format!("pdrs-local-settings-{}", uuid::Uuid::new_v4()));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(|e| {
                error(
                    "LOCAL_SETTINGS_INVALID",
                    "localSettings failed PilotDeck validation",
                    e,
                )
            })?;
        let _scratch = Scratch(path.clone());
        let candidate = path.join("pilotdeck.yaml");
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&candidate)
            .map_err(|e| {
                error(
                    "LOCAL_SETTINGS_INVALID",
                    "localSettings failed PilotDeck validation",
                    e,
                )
            })?;
        file.write_all(text.as_bytes()).map_err(|e| {
            error(
                "LOCAL_SETTINGS_INVALID",
                "localSettings failed PilotDeck validation",
                e,
            )
        })?;
        let mut env = self.env.clone();
        env.insert("PILOT_HOME".into(), path.to_string_lossy().into_owned());
        // Guarantee the candidate is validated even if the process supplied an
        // alternate config path. Never validate unrelated old bytes instead.
        env.insert(
            "PILOTDECK_CONFIG_PATH".into(),
            candidate.to_string_lossy().into_owned(),
        );
        crate::config::load_project_bounded(
            &path,
            &self.project,
            &env,
            Some(self.project.to_string_lossy().as_ref()),
        )
        .map_err(|e| {
            error(
                "LOCAL_SETTINGS_INVALID",
                "localSettings failed PilotDeck validation",
                String::from_utf16_lossy(e.message.units()),
            )
        })?;
        Ok(())
    }
}
impl LocalSettingsWriter for NativeLocalSettingsWriter {
    fn update(&self, settings: &JsValue) -> Result<LocalSettingsUpdate, SettingsError> {
        let actions = normalize::normalize(settings)?;
        let path = self.home.join("pilotdeck.yaml");
        // Unlike setup, the existing local settings file is required.
        fs::symlink_metadata(&path).map_err(|e| {
            error(
                "LOCAL_SETTINGS_READ_FAILED",
                "Unable to read local settings",
                e,
            )
        })?;
        let target = crate::atomic_file::Target::prepare(&path).map_err(|e| {
            error(
                "LOCAL_SETTINGS_WRITE_FAILED",
                "Unable to prepare local settings",
                e,
            )
        })?;
        let bytes = target.read_existing_bounded(LIMIT).map_err(|e| {
            error(
                "LOCAL_SETTINGS_READ_FAILED",
                "Unable to read local settings",
                e,
            )
        })?;
        let text = std::str::from_utf8(&bytes).map_err(|e| {
            error(
                "LOCAL_SETTINGS_YAML_INVALID",
                "Unable to update local settings",
                e,
            )
        })?;
        let updated = edit::edit(text, &actions)?;
        if actions.is_empty() {
            return Ok(LocalSettingsUpdate::default());
        }
        if updated.len() > LIMIT {
            return Err(error(
                "LOCAL_SETTINGS_WRITE_FAILED",
                "Unable to write local settings",
                "candidate exceeds file limit",
            ));
        }
        self.validate(&updated)?;
        target.publish(updated.as_bytes()).map_err(|e| {
            error(
                "LOCAL_SETTINGS_WRITE_FAILED",
                "Unable to write local settings",
                e,
            )
        })?;
        let mut applied = Vec::new();
        let mut cleared = Vec::new();
        for item in actions {
            let path = JsString::from(item.path.join("."));
            if item.value.is_some() {
                applied.push(path)
            } else {
                cleared.push(path)
            }
        }
        let mut changed_paths = applied.clone();
        changed_paths.extend(cleared.iter().cloned());
        Ok(LocalSettingsUpdate {
            applied,
            cleared,
            changed_paths,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pilotdeck_protocol::parse_json;
    #[test]
    fn real_settings_save_validates_and_preserves_user_text() {
        let oracle = parse_json(include_str!(
            "../../../tests/fixtures/native-settings/writer-oracle.json"
        ))
        .unwrap();
        let JsValue::Array(cases) = oracle.get("cases").unwrap() else {
            panic!("cases")
        };
        for case in cases {
            let id = case
                .get("id")
                .unwrap()
                .as_string()
                .unwrap()
                .to_utf8()
                .unwrap();
            let root = std::env::temp_dir().join(format!(
                "pdrs-native-settings-{id}-{}",
                uuid::Uuid::new_v4()
            ));
            let home = root.join("home");
            let project = root.join("project");
            fs::create_dir_all(&home).unwrap();
            fs::create_dir_all(&project).unwrap();
            let before = case
                .get("before")
                .unwrap()
                .as_string()
                .unwrap()
                .to_utf8()
                .unwrap();
            let path = home.join("pilotdeck.yaml");
            fs::write(&path, &before).unwrap();
            let mut env = Env::new();
            env.insert("PILOT_HOME".into(), home.to_string_lossy().into_owned());
            let writer = NativeLocalSettingsWriter { home, project, env };
            let result = writer.update(case.get("settings").unwrap());
            let actual = fs::read_to_string(&path).unwrap();
            if let Some(expected) = case.get("error") {
                let error = result.unwrap_err();
                assert_eq!(
                    error.code(),
                    expected.get("code").and_then(JsValue::as_string),
                    "{id}"
                );
                assert_eq!(actual, before, "{id}");
            } else {
                let result =
                    result.unwrap_or_else(|e| panic!("{id}: {}", e.message().to_utf8().unwrap()));
                let expected = case.get("result").unwrap();
                let expected_paths = |name| {
                    let JsValue::Array(values) = expected.get(name).unwrap() else {
                        panic!("paths")
                    };
                    values
                        .iter()
                        .map(|v| v.as_string().unwrap().clone())
                        .collect::<Vec<_>>()
                };
                assert_eq!(result.applied, expected_paths("applied"), "{id}");
                assert_eq!(result.cleared, expected_paths("cleared"), "{id}");
                assert!(actual.contains("# human config"), "{id}");
                let a = pilotdeck_yaml::parse_document(&JsString::from(actual), Default::default());
                let b = pilotdeck_yaml::parse_document(
                    case.get("after").unwrap().as_string().unwrap(),
                    Default::default(),
                );
                assert_eq!(a.to_json().unwrap(), b.to_json().unwrap(), "{id}");
            }
            fs::remove_dir_all(root).unwrap();
        }
    }
}
