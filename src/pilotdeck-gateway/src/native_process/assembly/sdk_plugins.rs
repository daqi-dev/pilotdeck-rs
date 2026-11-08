//! Native SDK-local plugin batch: one budget and one cancellation-owned IO slot.
use crate::runtime::{
    plugins::loader::{self, LoadedPlugin, StyleBudget},
    sdk_session::{PluginBatchFuture, PluginLoadFuture, SdkPluginLoader, SdkSessionError},
};
use pilotdeck_protocol::JsValue;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};
use tokio::sync::Semaphore;
fn slots() -> Arc<Semaphore> {
    static SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    SLOTS.get_or_init(|| Arc::new(Semaphore::new(4))).clone()
}
pub(super) struct NativeSdkPluginLoader;
impl SdkPluginLoader for NativeSdkPluginLoader {
    fn load<'a>(&'a self, _path: &'a Path) -> PluginLoadFuture<'a> {
        Box::pin(async { Err("Native SDK plugins require the bounded batch path.".to_owned()) })
    }
    fn load_session<'a>(
        &'a self,
        requested: &'a [JsValue],
        names: &'a [String],
    ) -> Option<PluginBatchFuture<'a>> {
        let requested = requested.to_vec();
        let names = names.to_vec();
        Some(Box::pin(async move {
            let permit = slots().acquire_owned().await.map_err(|_| {
                SdkSessionError::new("SDK_PLUGIN_LOAD_FAILED", "Native plugin worker is closed.")
            })?;
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                load_batch(&requested, &names)
            })
            .await
            .map_err(|_| {
                SdkSessionError::new("SDK_PLUGIN_LOAD_FAILED", "Native plugin worker failed.")
            })?
        }))
    }
}
fn load_batch(
    requested: &[JsValue],
    existing: &[String],
) -> Result<Vec<Arc<LoadedPlugin>>, SdkSessionError> {
    if requested.len() > 10_000 {
        return Err(SdkSessionError::new(
            "SDK_PLUGIN_LOAD_FAILED",
            "Native SDK plugin entry budget exceeded.",
        ));
    }
    let mut budget = StyleBudget::new();
    let mut names: HashSet<String> = existing.iter().cloned().collect();
    let mut paths: HashSet<PathBuf> = HashSet::new();
    let mut plugins = Vec::new();
    for descriptor in requested {
        let value = descriptor
            .get("path")
            .and_then(JsValue::as_string)
            .ok_or_else(|| {
                SdkSessionError::new(
                    "SDK_PLUGIN_NOT_FOUND",
                    "Gateway-local SDK plugin path does not exist.",
                )
            })?;
        let text = String::from_utf16_lossy(value.units());
        let path = std::fs::canonicalize(&text).map_err(|_| {
            SdkSessionError::new(
                "SDK_PLUGIN_NOT_FOUND",
                format!("Gateway-local SDK plugin path does not exist: {text}"),
            )
        })?;
        if !paths.insert(path.clone()) {
            return Err(SdkSessionError::new(
                "SDK_PLUGIN_DUPLICATE_PATH",
                format!("Gateway-local SDK plugin path is listed more than once: {text}"),
            ));
        }
        let metadata = std::fs::metadata(&path).map_err(|_| {
            SdkSessionError::new(
                "SDK_PLUGIN_NOT_FOUND",
                format!("Gateway-local SDK plugin path does not exist: {text}"),
            )
        })?;
        if !metadata.is_dir() {
            return Err(SdkSessionError::new(
                "SDK_PLUGIN_NOT_DIRECTORY",
                format!("Gateway-local SDK plugin path is not a directory: {text}"),
            ));
        }
        let plugin = loader::load_native_session_plugin(&path, &mut budget).map_err(|error| {
            SdkSessionError::new(
                "SDK_PLUGIN_LOAD_FAILED",
                format!("Could not load SDK plugin {text}: {}", error.message),
            )
        })?;
        if !names.insert(plugin.name.clone()) {
            return Err(SdkSessionError::new(
                "SDK_PLUGIN_NAME_CONFLICT",
                format!(
                    "SDK plugin {} conflicts with an already discovered project plugin.",
                    plugin.name
                ),
            ));
        }
        plugins.push(Arc::new(plugin));
    }
    Ok(plugins)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::metadata::object;
    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn descriptor(path: &Path) -> JsValue {
        object([
            ("type", "local".into()),
            ("path", path.to_string_lossy().as_ref().into()),
        ])
    }
    #[test]
    fn bounded_sdk_loader_reads_real_manifest_and_rejects_fifo_and_duplicate() {
        let root =
            Scratch(std::env::temp_dir().join(format!("pdrs-sdk-plugin-{}", uuid::Uuid::new_v4())));
        let plugin = root.0.join("plugin");
        std::fs::create_dir_all(&plugin).unwrap();
        std::fs::write(
            plugin.join("plugin.json"),
            r#"{"name":"example","version":"1.0.0"}"#,
        )
        .unwrap();
        let item = descriptor(&plugin);
        let loaded = load_batch(std::slice::from_ref(&item), &[]).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "example");
        assert_eq!(
            load_batch(&[item.clone(), item], &[]).unwrap_err().code,
            "SDK_PLUGIN_DUPLICATE_PATH"
        );
        let special = root.0.join("special");
        std::fs::create_dir(&special).unwrap();
        assert!(
            std::process::Command::new("/usr/bin/mkfifo")
                .arg(special.join("plugin.json"))
                .status()
                .unwrap()
                .success()
        );
        let item = descriptor(&special);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(load_batch(&[item], &[]).err().map(|e| e.code));
        });
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(3))
                .unwrap()
                .as_deref(),
            Some("SDK_PLUGIN_LOAD_FAILED")
        );
    }
}
