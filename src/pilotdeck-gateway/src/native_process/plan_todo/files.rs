//! Bounded, project-rooted plan file access; never follows a raced symlink.
use crate::host::{HostError, HostFuture, ModuleError, modules::tool_context::PlanFiles};
use pilotdeck_protocol::{JsString, JsValue};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
const MAX_PLAN_BYTES: u64 = 8 * 1024 * 1024;
#[derive(Clone)]
pub(crate) struct NativePlanFiles {
    root: PathBuf,
    directory: PathBuf,
    physical_directory: PathBuf,
}
fn error(value: impl Into<String>) -> HostError {
    ModuleError::new(value.into()).into()
}
impl NativePlanFiles {
    pub(crate) async fn prepare(project: PathBuf) -> Result<Self, HostError> {
        #[cfg(not(unix))]
        {
            let _ = project;
            return Err(error("Native plan files require Unix."));
        }
        #[cfg(unix)]
        {
            let directory = project.join(".pilotdeck").join("plans");
            let permit = slots()
                .acquire_owned()
                .await
                .map_err(|_| error("Plan file executor unavailable."))?;
            let (root, physical_directory) = tokio::task::spawn_blocking(move || {
                let _permit = permit;
                let root = project.canonicalize()?;
                let target = root.join(".pilotdeck").join("plans");
                crate::project_files::create_directories_beneath(&root, &target)?;
                Ok::<_, std::io::Error>((root, target))
            })
            .await
            .map_err(|e| error(e.to_string()))?
            .map_err(|e| error(e.to_string()))?;
            Ok(Self {
                root,
                directory,
                physical_directory,
            })
        }
    }
    fn resolve_path(&self, file: &JsString, cwd: &JsString) -> Option<PathBuf> {
        let value = file.to_utf8().ok()?;
        if crate::metadata::js_trim(&value).is_empty() {
            return None;
        }
        let cwd = cwd.to_utf8().ok()?;
        let path =
            crate::metadata::resolve_process_path(Path::new(&cwd), Path::new(&value)).ok()?;
        let relative = path
            .strip_prefix(&self.directory)
            .or_else(|_| path.strip_prefix(&self.physical_directory))
            .ok()?;
        if relative.as_os_str().is_empty() {
            return None;
        }
        let first = relative.components().next()?.as_os_str().to_string_lossy();
        if first.starts_with("..") {
            return None;
        }
        if !path.to_string_lossy().to_ascii_lowercase().ends_with(".md") {
            return None;
        }
        Some(path)
    }
    pub(crate) fn directory_string(&self) -> JsString {
        self.directory.to_string_lossy().into_owned().into()
    }
}
fn slots() -> Arc<tokio::sync::Semaphore> {
    static SLOTS: std::sync::OnceLock<Arc<tokio::sync::Semaphore>> = std::sync::OnceLock::new();
    SLOTS
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(4)))
        .clone()
}
impl PlanFiles for NativePlanFiles {
    fn directory(&self) -> Option<JsString> {
        Some(self.directory_string())
    }
    fn resolve(&self, file: JsString, cwd: JsString) -> HostFuture<JsValue> {
        let path = self.resolve_path(&file, &cwd);
        Box::pin(async move {
            Ok(path
                .map(|v| JsValue::String(v.to_string_lossy().into_owned().into()))
                .unwrap_or(JsValue::Undefined))
        })
    }
    fn read(&self, file: JsString, cwd: JsString) -> HostFuture<JsValue> {
        let path = self.resolve_path(&file, &cwd).and_then(|path| {
            path.strip_prefix(&self.directory)
                .or_else(|_| path.strip_prefix(&self.physical_directory))
                .ok()
                .map(|relative| self.physical_directory.join(relative))
        });
        let root = self.root.clone();
        Box::pin(async move {
            let Some(path) = path else {
                return Ok(JsValue::Undefined);
            };
            let permit = slots()
                .acquire_owned()
                .await
                .map_err(|_| error("Plan file executor unavailable."))?;
            tokio::task::spawn_blocking(move || {
                use std::io::Read;
                let _permit = permit;
                let mut file = match crate::project_files::open_beneath(&root, &path) {
                    Ok(file) => file,
                    Err(_) => return Ok(JsValue::Undefined),
                };
                if !file.metadata().map_err(|e| error(e.to_string()))?.is_file() {
                    return Ok(JsValue::Undefined);
                }
                let mut bytes = Vec::new();
                (&mut file)
                    .take(MAX_PLAN_BYTES + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|e| error(e.to_string()))?;
                if bytes.len() as u64 > MAX_PLAN_BYTES {
                    return Err(error("Plan file exceeds the 8 MiB limit."));
                }
                let text = String::from_utf8_lossy(&bytes);
                let trimmed = crate::metadata::js_trim(&text);
                // Fixed TS planFile.ts:76 treats empty or whitespace-only text as absent.
                if trimmed.is_empty() {
                    return Ok(JsValue::Undefined);
                }
                let plan: JsString = trimmed.into();
                super::projection::validate_plan(&plan).map_err(|e| error(e.to_string()))?;
                Ok(JsValue::String(plan))
            })
            .await
            .map_err(|e| error(e.to_string()))?
        })
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{fs, time::Duration};
    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[tokio::test]
    async fn plan_file_empty_and_whitespace_match_real_ts_reader() {
        let scratch = Scratch(
            std::env::temp_dir().join(format!("pdrs-plan-reader-{}", uuid::Uuid::new_v4())),
        );
        fs::create_dir_all(&scratch.0).unwrap();
        let project = scratch.0.canonicalize().unwrap();
        let files = NativePlanFiles::prepare(project.clone()).await.unwrap();
        let oracle: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/plan-todo/read-file-oracle.json"
        ))
        .unwrap();
        assert_eq!(oracle["rows"].as_array().unwrap().len(), 4);
        for row in oracle["rows"].as_array().unwrap() {
            fs::write(
                project.join(".pilotdeck/plans/plan.md"),
                row["content"].as_str().unwrap(),
            )
            .unwrap();
            let actual = files
                .read(
                    ".pilotdeck/plans/plan.md".into(),
                    project.to_string_lossy().into_owned().into(),
                )
                .await
                .unwrap();
            if row["result"]["undefined"] == true {
                assert!(actual.is_undefined(), "{}", row["name"]);
            } else {
                assert_eq!(
                    actual,
                    JsValue::from(row["result"]["value"].as_str().unwrap()),
                    "{}",
                    row["name"]
                );
            }
        }
    }
    #[tokio::test]
    async fn plan_paths_are_project_confined_and_reads_are_bounded() {
        let scratch =
            Scratch(std::env::temp_dir().join(format!("pdrs-plan-{}", uuid::Uuid::new_v4())));
        fs::create_dir_all(scratch.0.join("project")).unwrap();
        let project = scratch.0.join("project").canonicalize().unwrap();
        let alias = scratch.0.join("project-alias");
        std::os::unix::fs::symlink(&project, &alias).unwrap();
        let files = NativePlanFiles::prepare(alias.clone()).await.unwrap();
        let path = project.join(".pilotdeck/plans/plan.md");
        fs::write(&path, "  # Plan\n\nDo the work\n ").unwrap();
        let cwd: JsString = alias.to_string_lossy().into_owned().into();
        assert_eq!(
            files
                .read(".pilotdeck/plans/plan.md".into(), cwd.clone())
                .await
                .unwrap(),
            JsValue::from("# Plan\n\nDo the work")
        );
        assert!(
            files
                .resolve("../escape.md".into(), cwd.clone())
                .await
                .unwrap()
                .is_undefined()
        );
        assert!(
            files
                .resolve(".pilotdeck/plans/not.txt".into(), cwd.clone())
                .await
                .unwrap()
                .is_undefined()
        );
        let secret = scratch.0.join("secret.md");
        fs::write(&secret, "private").unwrap();
        std::os::unix::fs::symlink(&secret, project.join(".pilotdeck/plans/link.md")).unwrap();
        assert!(
            files
                .read(".pilotdeck/plans/link.md".into(), cwd.clone())
                .await
                .unwrap()
                .is_undefined()
        );
        let fifo = project.join(".pilotdeck/plans/pipe.md");
        assert!(
            std::process::Command::new("/usr/bin/mkfifo")
                .arg(&fifo)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            tokio::time::timeout(
                Duration::from_secs(2),
                files.read(".pilotdeck/plans/pipe.md".into(), cwd.clone())
            )
            .await
            .unwrap()
            .unwrap()
            .is_undefined()
        );
        fs::write(&path, " ").unwrap();
        assert_eq!(
            files
                .read(".pilotdeck/plans/plan.md".into(), cwd.clone())
                .await
                .unwrap(),
            JsValue::Undefined
        );
        fs::write(&path, "x".repeat(3 * 1024 * 1024)).unwrap();
        let error = files
            .read(".pilotdeck/plans/plan.md".into(), cwd.clone())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("before requesting approval"));
        fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(MAX_PLAN_BYTES + 1)
            .unwrap();
        assert!(
            files
                .read(".pilotdeck/plans/plan.md".into(), cwd)
                .await
                .is_err()
        );
    }
}
