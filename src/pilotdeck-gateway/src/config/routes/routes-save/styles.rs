//! Output-style wire projection uses the existing plugin loader and SDK session state.
use super::{ConfigRoutes, Result, RouteError, field};
use crate::{
    metadata::object,
    runtime::{plugins::PluginOutputStyleContribution, sdk_session::SdkRuntimeResolver},
};
use pilotdeck_protocol::{JsString, JsValue};

pub(super) fn project_key(params: &JsValue) -> Result<Option<JsString>> {
    match field(params, "projectKey") {
        JsValue::Undefined | JsValue::Null => Ok(None),
        JsValue::String(key) => Ok(Some(key.clone())),
        _ => Err(RouteError::new(
            "ERR_INVALID_ARG_TYPE",
            "projectKey must be a string.",
        )),
    }
}
pub(super) fn style_wire(style: &PluginOutputStyleContribution) -> JsValue {
    let mut fields = vec![("name", style.name.as_str().into())];
    if let Some(description) = style.description.as_deref().filter(|s| !s.is_empty()) {
        fields.push(("description", description.into()));
    }
    if !style.plugin.is_empty() {
        fields.push(("plugin", style.plugin.as_str().into()));
    }
    fields.push(("source", style.source.as_str().into()));
    object(fields)
}
impl ConfigRoutes {
    pub(super) async fn styles(
        &mut self,
        params: &JsValue,
    ) -> Result<(std::path::PathBuf, Vec<PluginOutputStyleContribution>)> {
        let key = project_key(params)?;
        let runtime = self
            .resolver
            .resolve(key.as_ref())
            .map_err(|e| RouteError::new(e.code, e.message))?;
        runtime
            .plugin_runtime
            .refresh()
            .await
            .map_err(|e| RouteError::new("gateway_request_failed", e.message))?;
        let additional = field(params, "sessionKey")
            .as_string()
            .map(|key| self.sdk.session_plugins(key).to_vec())
            .unwrap_or_default();
        let base = runtime.plugin_runtime.snapshot().await;
        let mut registry = Vec::<PluginOutputStyleContribution>::new();
        for style in flatten_styles(&base) {
            if let Some(previous) = registry.iter_mut().find(|s| s.name == style.name) {
                *previous = style;
            } else {
                registry.push(style);
            }
        }
        self.output_styles
            .insert(runtime.project_root.clone(), registry);
        let view = runtime.plugin_runtime.create_view(additional).await;
        let mut styles = flatten_styles(&view.snapshot());
        let locale = crate::project_files::node_locale(|key| self.env.get(key).cloned());
        let collator = crate::project_files::node_collator(&locale)
            .map_err(|(code, message)| RouteError::new(code, message))?;
        styles.sort_by(|a, b| collator.compare(&a.name, &b.name));
        Ok((runtime.project_root, styles))
    }
    pub(super) async fn output_styles_list(&mut self, params: &JsValue) -> Result {
        let (_, styles) = self.styles(params).await?;
        let mut fields = vec![(
            "styles",
            JsValue::Array(styles.iter().map(style_wire).collect()),
        )];
        if let Some(key) = field(params, "sessionKey").as_string()
            && let Some(selected) = self
                .sdk
                .session_config(key)
                .and_then(|v| v.get("outputStyle"))
            && selected.as_string().is_some_and(|s| !s.is_empty())
        {
            fields.push(("selected", selected.clone()));
        }
        Ok(object(fields))
    }
}

impl ConfigRoutes {
    pub(super) async fn set_output_style(&mut self, params: &JsValue) -> Result {
        let key = crate::runtime::sdk_session::routes::session_key(params)?;
        self.check_busy(&key, "Cannot change output style during an active turn.")?;
        let name = match field(params, "name") {
            JsValue::Null => None,
            JsValue::String(name)
                if !crate::metadata::js_trim(&String::from_utf16_lossy(name.units()))
                    .is_empty() =>
            {
                Some(name.clone())
            }
            JsValue::String(_) => {
                return Err(RouteError::new(
                    "INVALID_OUTPUT_STYLE",
                    "name must be non-empty or null.",
                ));
            }
            JsValue::Undefined => {
                return Err(RouteError::new(
                    "gateway_request_failed",
                    "Cannot read properties of undefined (reading 'trim')",
                ));
            }
            _ => {
                return Err(RouteError::new(
                    "gateway_request_failed",
                    "input.name.trim is not a function",
                ));
            }
        };
        let (project, styles) = self.styles(params).await?;
        if let Some(name) = &name
            && !styles.iter().any(|s| name.equals_str(&s.name))
        {
            return Err(RouteError::new(
                "OUTPUT_STYLE_NOT_FOUND",
                format!(
                    "Unknown output style: {}",
                    String::from_utf16_lossy(name.units())
                ),
            ));
        }
        self.sdk.store_output_style(&key, name.as_ref(), project);
        self.gateway_close_session(&key, "sdk_output_style_changed")
            .await?;
        let mut result = vec![("applied", JsValue::Bool(true))];
        if let Some(name) = name {
            result.push(("selected", JsValue::String(name)));
        }
        Ok(object(result))
    }
}

impl ConfigRoutes {
    pub(super) async fn reload_output_styles(&mut self, params: &JsValue) -> Result {
        use crate::runtime::plugins::{
            PilotExtensionPaths, PluginSource,
            loader::{discover_plugin_paths, load_plugin_output_styles_from_path},
        };
        let key = project_key(params)?;
        let runtime = self
            .resolver
            .resolve(key.as_ref())
            .map_err(|e| RouteError::new(e.code, e.message))?;
        let paths = PilotExtensionPaths::resolve(&runtime.project_root, &self.home);
        let mut next = flatten_styles(&self.resolver.builtin_plugins);
        for plugin in discover_plugin_paths(&[
            (paths.global_plugins_dir, PluginSource::Global),
            (paths.project_plugins_dir, PluginSource::Project),
        ]) {
            // TS omits malformed style plugins here, preserving all other plugin contributions.
            let Ok(loaded) = load_plugin_output_styles_from_path(&plugin.path, plugin.source)
            else {
                continue;
            };
            for style in loaded.output_styles {
                let contribution = PluginOutputStyleContribution {
                    name: style.name,
                    description: style
                        .frontmatter
                        .get("description")
                        .and_then(|v| v.as_str())
                        .map(str::to_owned),
                    content: style.content,
                    path: style.path,
                    plugin: loaded.name.clone(),
                    source: loaded.source,
                };
                if let Some(previous) = next.iter_mut().find(|s| s.name == contribution.name) {
                    *previous = contribution;
                } else {
                    next.push(contribution);
                }
            }
        }
        let previous = self
            .output_styles
            .get(&runtime.project_root)
            .cloned()
            .unwrap_or_default();
        let mut names = Vec::new();
        for style in previous.iter().chain(&next) {
            if !names.contains(&style.name) {
                names.push(style.name.clone());
            }
        }
        let changed: Vec<_> = names
            .into_iter()
            .filter(|name| {
                match (
                    previous.iter().find(|s| &s.name == name),
                    next.iter().find(|s| &s.name == name),
                ) {
                    (Some(a), Some(b)) => {
                        a.description != b.description
                            || a.content != b.content
                            || a.path != b.path
                            || a.plugin != b.plugin
                            || a.source != b.source
                    }
                    _ => true,
                }
            })
            .collect();
        self.output_styles
            .insert(runtime.project_root.clone(), next);
        let sessions: Vec<_> = self
            .sdk
            .project_entries()
            .iter()
            .filter(|(session, project)| {
                (key.as_ref().is_none_or(|k| k.is_empty()) || project == &runtime.project_root)
                    && self.sdk.session_config(session).is_some()
            })
            .map(|(session, _)| session.clone())
            .collect();
        for session in sessions {
            self.gateway_close_session(&session, "sdk_output_styles_reloaded")
                .await?;
        }
        Ok(object([
            ("reloaded", JsValue::Bool(true)),
            ("changed", super::strings(changed)),
        ]))
    }
}

fn flatten_styles(
    plugins: &[std::sync::Arc<crate::runtime::plugins::LoadedPlugin>],
) -> Vec<PluginOutputStyleContribution> {
    plugins
        .iter()
        .flat_map(|plugin| {
            plugin
                .output_styles
                .iter()
                .map(|style| PluginOutputStyleContribution {
                    name: style.name.clone(),
                    description: style
                        .frontmatter
                        .get("description")
                        .and_then(|v| v.as_str())
                        .map(str::to_owned),
                    content: style.content.clone(),
                    path: style.path.clone(),
                    plugin: plugin.name.clone(),
                    source: plugin.source,
                })
        })
        .collect()
}
impl ConfigRoutes {
    /// Turn construction consumes the style-only registry, or the session plugin view.
    pub async fn session_output_style_content(
        &mut self,
        session: &JsString,
        project: Option<&JsString>,
    ) -> Result<Option<String>> {
        let name = self
            .sdk
            .session_config(session)
            .and_then(|v| field(v, "outputStyle").as_string())
            .cloned();
        let Some(name) = name.filter(|s| !s.is_empty()) else {
            return Ok(None);
        };
        let runtime = self
            .resolver
            .resolve(project)
            .map_err(|e| RouteError::new(e.code, e.message))?;
        let additional = self.sdk.session_plugins(session).to_vec();
        if additional.is_empty() {
            if !self.output_styles.contains_key(&runtime.project_root) {
                let params = object([(
                    "projectKey",
                    project
                        .cloned()
                        .map(JsValue::String)
                        .unwrap_or(JsValue::Undefined),
                )]);
                self.styles(&params).await?;
            }
            Ok(self
                .output_styles
                .get(&runtime.project_root)
                .and_then(|styles| styles.iter().find(|s| name.equals_str(&s.name)))
                .map(|s| s.content.clone()))
        } else {
            let view = runtime.plugin_runtime.create_view(additional).await;
            Ok(flatten_styles(&view.snapshot())
                .iter()
                .find(|s| name.equals_str(&s.name))
                .map(|s| s.content.clone()))
        }
    }
}
