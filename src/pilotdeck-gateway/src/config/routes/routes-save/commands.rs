//! gateway/dialog/commands.ts catalog, including filesystem precedence and query-bound cursors.
use super::{ConfigRoutes, Result, RouteError, field};
use crate::{
    metadata::{Metadata, NativeFiles, js_trim, object, text_field},
    runtime::{plugins::PluginCommandContribution, sdk_session::SdkRuntimeResolver},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use std::{collections::HashSet, fs, path::Path};
const PINNED: &[&str] = &["/skill_install", "/projects", "/switch-project"];
const WEB: &[(&str, &str)] = &[
    ("clear", "Clear the conversation history"),
    ("model", "View the current AI model and available options"),
    ("compact", "Compact older conversation history"),
    ("cost", "Display token usage and cost information"),
    ("memory", "Open PILOTDECK.md memory file for editing"),
    ("config", "Open settings and configuration"),
    ("rewind", "Rewind the conversation to a previous state"),
    ("ao", "List, run, or inspect Always-On jobs"),
    ("turnkey", "Run turnkey workflow subcommands"),
    ("skill_install", "Install a skill"),
];
const CHANNEL: &[(&str, &str, &[&str])] = &[
    ("plan", "先生成计划并请求确认", &[]),
    ("projects", "List available projects", &["项目列表"]),
    (
        "switch-project",
        "Switch active project for this chat",
        &["切换项目"],
    ),
    (
        "update",
        "Install the latest supported Release and request a Web service restart",
        &["升级", "更新"],
    ),
    ("status", "Show PilotDeck status and version", &["状态"]),
    (
        "search",
        "Search chat history across sessions",
        &["find", "grep", "搜索"],
    ),
    ("help", "Show available commands", &["帮助"]),
];
fn builtin(name: &str, description: &str, aliases: Option<&[&str]>) -> JsValue {
    let mut metadata = vec![("type", "builtin".into())];
    if let Some(aliases) = aliases {
        metadata.push(("source", "registry".into()));
        if !aliases.is_empty() {
            metadata.push(("aliases", super::strings(aliases.iter().copied())));
        }
    }
    object([
        ("name", format!("/{name}").as_str().into()),
        ("description", description.into()),
        ("namespace", "builtin".into()),
        ("type", "builtin".into()),
        ("metadata", object(metadata)),
    ])
}
fn set(item: &mut JsValue, key: &str, value: JsValue) {
    if let JsValue::Object(obj) = item {
        obj.insert(key.into(), value);
    }
}
fn frontmatter(source: &str) -> JsValue {
    static FM: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"(?s)^---\r?\n(.*?)\r?\n---").unwrap());
    let Some(capture) = FM.captures(source) else {
        return object([]);
    };
    let doc = pilotdeck_yaml::parse_document(&JsString::from(&capture[1]), Default::default());
    if !doc.errors.is_empty() {
        return object([]);
    }
    doc.to_json()
        .and_then(|g| g.to_js_value())
        .ok()
        .filter(|v| matches!(v, JsValue::Object(_) | JsValue::Array(_)))
        .unwrap_or_else(|| object([]))
}
fn first_line(source: &str) -> String {
    let source = if let Some(rest) = source.strip_prefix("---") {
        rest.find("---")
            .map(|at| crate::metadata::js_trim_start(&rest[at + 3..]))
            .unwrap_or(source)
    } else {
        source
    };
    source
        .split('\n')
        .find(|line| !js_trim(line).is_empty())
        .map(|line| js_trim(line.trim_start_matches('#')).to_owned())
        .unwrap_or_default()
}
fn disk_item(
    path: &Path,
    relative: &str,
    name: &str,
    namespace: &str,
    kind: &str,
    source: &str,
) -> JsValue {
    let meta = frontmatter(source);
    let description = text_field(&meta, "description").unwrap_or_else(|| first_line(source));
    let mut fields = vec![
        ("name", name.into()),
        ("namespace", namespace.into()),
        ("type", kind.into()),
        ("path", path.to_string_lossy().as_ref().into()),
        ("relativePath", relative.into()),
        ("description", description.as_str().into()),
        ("metadata", meta.clone()),
    ];
    if let Some(hint) = meta.get("argumentHint").filter(|v| v.as_string().is_some()) {
        fields.push(("argumentHint", hint.clone()));
    }
    object(fields)
}
fn entries(root: &Path, collator: &icu_collator::CollatorBorrowed<'_>) -> Vec<fs::DirEntry> {
    let mut entries: Vec<_> = fs::read_dir(root)
        .into_iter()
        .flatten()
        .filter_map(std::result::Result::ok)
        .collect();
    entries.sort_by(|a, b| {
        collator.compare(
            &a.file_name().to_string_lossy(),
            &b.file_name().to_string_lossy(),
        )
    });
    entries
}
fn scan(
    root: &Path,
    namespace: &str,
    skills: bool,
    collator: &icu_collator::CollatorBorrowed<'_>,
) -> Vec<JsValue> {
    let mut out = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let mut found = entries(&dir, collator);
        if !skills {
            found.reverse();
        }
        for entry in found {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if skills {
                if !kind.is_dir() && !kind.is_symlink() {
                    continue;
                }
                let path = path.join("SKILL.md");
                let Some(source) = fs::read(&path)
                    .ok()
                    .map(|s| String::from_utf8_lossy(&s).into_owned())
                    .filter(|s| !s.is_empty())
                else {
                    continue;
                };
                let name = entry.file_name().to_string_lossy().into_owned();
                out.push(disk_item(
                    &path,
                    &format!("{name}/SKILL.md"),
                    &format!("/{name}"),
                    namespace,
                    "skill",
                    &source,
                ));
            } else if kind.is_dir() {
                pending.push(path);
            } else if kind.is_file() && entry.file_name().to_string_lossy().ends_with(".md") {
                let source = fs::read(&path)
                    .map(|s| String::from_utf8_lossy(&s).into_owned())
                    .unwrap_or_default();
                let relative = path.strip_prefix(root).unwrap().to_string_lossy();
                let name = format!("/{}", relative.strip_suffix(".md").unwrap());
                out.push(disk_item(
                    &path, &relative, &name, namespace, "command", &source,
                ));
            }
        }
    }
    // Traversal order affects duplicate resolution, including directory/file interleaving.
    if !skills {
        out.sort_by(|a, b| {
            collator.compare(
                text_field(a, "relativePath").as_deref().unwrap_or(""),
                text_field(b, "relativePath").as_deref().unwrap_or(""),
            )
        });
    }
    out
}
fn extension(command: &PluginCommandContribution) -> Option<JsValue> {
    let raw = js_trim(&command.name);
    if raw.is_empty() {
        return None;
    }
    let name = if raw.starts_with('/') {
        raw.to_owned()
    } else {
        format!("/{raw}")
    };
    let namespace = if js_trim(&command.namespace).is_empty() {
        "extension"
    } else {
        js_trim(&command.namespace)
    };
    let mut fields = vec![
        ("name", name.as_str().into()),
        ("namespace", namespace.into()),
        ("type", "command".into()),
        (
            "metadata",
            object([
                ("type", "command".into()),
                ("source", "plugin".into()),
                ("namespace", namespace.into()),
            ]),
        ),
    ];
    if let Some(v) = command.description.as_deref().filter(|s| !s.is_empty()) {
        fields.push(("description", v.into()));
    }
    if let Some(v) = command.argument_hint.as_deref().filter(|s| !s.is_empty()) {
        fields.push(("argumentHint", v.into()));
    }
    Some(object(fields))
}
fn matches(item: &JsValue, query: &str) -> Vec<JsValue> {
    if query.is_empty() {
        return vec![];
    }
    let mut fields = vec![
        ("name", text_field(item, "name").unwrap_or_default()),
        (
            "description",
            text_field(item, "description").unwrap_or_default(),
        ),
    ];
    if let JsValue::Array(aliases) = field(field(item, "metadata"), "aliases") {
        fields.extend(
            aliases
                .iter()
                .filter_map(|v| v.as_string()?.to_utf8().ok())
                .map(|v| ("alias", v)),
        );
    }
    let query: Vec<_> = query.encode_utf16().collect();
    let mut out = Vec::new();
    for (field, value) in fields {
        let lower: Vec<_> = value.to_lowercase().encode_utf16().collect();
        let mut at = 0;
        while at + query.len() <= lower.len() {
            let Some(offset) = lower[at..].windows(query.len()).position(|s| s == query) else {
                break;
            };
            let start = at + offset;
            out.push(object([
                ("field", field.into()),
                ("start", JsValue::Number(start as f64)),
                ("end", JsValue::Number((start + query.len()) as f64)),
            ]));
            at = start + query.len();
        }
    }
    out
}
fn encoded(value: &JsValue) -> String {
    URL_SAFE_NO_PAD.encode(stringify_json(value).unwrap().unwrap())
}
fn cursor(value: &JsValue, signature: &str) -> Result<usize> {
    if matches!(value, JsValue::Undefined | JsValue::Null)
        || value.as_string().is_some_and(|s| s.is_empty())
    {
        return Ok(0);
    }
    let invalid = || {
        RouteError::new(
            "INVALID_CURSOR",
            "cursor is invalid or does not match the query.",
        )
    };
    let source = value.as_string().ok_or_else(invalid)?;
    let mut clean: String = source
        .units()
        .iter()
        .map(|unit| char::from(*unit as u8))
        .take_while(|c| *c != '=')
        .filter_map(|c| match c {
            '+' => Some('-'),
            '/' => Some('_'),
            c if c.is_ascii_alphanumeric() || c == '-' || c == '_' => Some(c),
            _ => None,
        })
        .collect();
    if clean.len() % 4 == 1 {
        clean.pop();
    }
    use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
    let bytes = GeneralPurpose::new(
        &base64::alphabet::URL_SAFE,
        GeneralPurposeConfig::new()
            .with_decode_allow_trailing_bits(true)
            .with_decode_padding_mode(DecodePaddingMode::Indifferent),
    )
    .decode(clean)
    .map_err(|_| invalid())?;
    let parsed = parse_json(&String::from_utf8_lossy(&bytes)).map_err(|_| invalid())?;
    let offset = field(&parsed, "offset")
        .as_number()
        .filter(|n| n.is_finite() && n.fract() == 0.0 && *n >= 0.0)
        .ok_or_else(invalid)?;
    if text_field(&parsed, "signature").as_deref() != Some(signature) {
        return Err(invalid());
    }
    Ok(offset as usize)
}
impl ConfigRoutes {
    pub(super) async fn commands_list(&mut self, params: &JsValue) -> Result {
        let requested = text_field(params, "projectKey")
            .filter(|s| !s.is_empty())
            .ok_or_else(|| RouteError::new("PROJECT_NOT_FOUND", "projectKey is required."))?;
        let project = crate::metadata::resolve(&self.cwd, &requested);
        let metadata = Metadata {
            home: self.home.clone(),
            cwd: self.cwd.clone(),
            files: NativeFiles,
        };
        let registered =
            if let JsValue::Array(projects) = field(&metadata.list_projects(), "projects") {
                projects.iter().any(|p| {
                    text_field(p, "projectKey")
                        .is_some_and(|p| crate::metadata::resolve(&self.cwd, &p) == project)
                })
            } else {
                false
            };
        if project != self.home && !registered {
            return Err(RouteError::new(
                "PROJECT_NOT_FOUND",
                format!("Unknown projectKey: {requested}"),
            ));
        }
        let key = JsString::from(project.to_string_lossy().into_owned());
        let runtime = self
            .resolver
            .resolve(Some(&key))
            .map_err(|e| RouteError::new(e.code, e.message))?;
        runtime
            .plugin_runtime
            .refresh()
            .await
            .map_err(|e| RouteError::new("gateway_request_failed", e.message))?;
        let mut lease = runtime
            .plugin_runtime
            .acquire_command_catalog()
            .await
            .map_err(|e| RouteError::new("gateway_request_failed", e.message))?;
        let result = (|| {
            let query = match field(params, "query") {
                JsValue::Undefined | JsValue::Null => String::new(),
                JsValue::String(s) => js_trim(&String::from_utf16_lossy(s.units())).to_lowercase(),
                _ => {
                    return Err(RouteError::new(
                        "gateway_request_failed",
                        "input.query?.trim is not a function",
                    ));
                }
            };
            if query.encode_utf16().count() > 256 {
                return Err(RouteError::new(
                    "INVALID_QUERY",
                    "query must not exceed 256 characters.",
                ));
            }
            let limit = match field(params, "limit") {
                JsValue::Undefined | JsValue::Null => 50.0,
                v => v.as_number().unwrap_or(f64::NAN),
            };
            if !limit.is_finite() || limit.fract() != 0.0 || !(1.0..=100.0).contains(&limit) {
                return Err(RouteError::new(
                    "INVALID_LIMIT",
                    "limit must be an integer between 1 and 100.",
                ));
            }
            self.catalog(
                params,
                &project,
                &query,
                limit as usize,
                &lease.contributions.commands,
            )
        })();
        if let Err(e) = lease.release().await {
            eprintln!("[pilotdeck] failed to release extension command catalog lease: {e}");
        }
        result
    }
    fn catalog(
        &self,
        params: &JsValue,
        project: &Path,
        query: &str,
        limit: usize,
        extensions: &[PluginCommandContribution],
    ) -> Result {
        let locale = crate::project_files::node_locale(|key| self.env.get(key).cloned());
        let collator = crate::project_files::node_collator(&locale)
            .map_err(|(code, message)| RouteError::new(code, message))?;
        let mut all: Vec<_> = WEB
            .iter()
            .map(|(n, d)| builtin(n, d, None))
            .chain(CHANNEL.iter().map(|(n, d, a)| builtin(n, d, Some(a))))
            .collect();
        for (root, ns, skills) in [
            (project.join(".pilotdeck/commands"), "project", false),
            (project.join(".pilotdeck/skills"), "project", true),
            (self.home.join("commands"), "user", false),
            (self.home.join("skills"), "user", true),
        ] {
            all.extend(scan(&root, ns, skills, &collator));
        }
        all.extend(extensions.iter().filter_map(extension));
        let mut seen = HashSet::new();
        all.retain(|item| seen.insert(text_field(item, "name").unwrap()));
        let mut matched = Vec::new();
        for mut item in all {
            let hits = matches(&item, query);
            if !query.is_empty() && hits.is_empty() {
                continue;
            }
            if !hits.is_empty() {
                set(&mut item, "matches", JsValue::Array(hits));
            }
            matched.push(item);
        }
        let pinned: Vec<_> = PINNED
            .iter()
            .filter_map(|name| {
                matched
                    .iter()
                    .find(|item| text_field(item, "name").as_deref() == Some(name))
                    .cloned()
            })
            .map(|mut item| {
                set(&mut item, "namespace", "pinned".into());
                item
            })
            .collect();
        matched.retain(|item| !PINNED.contains(&text_field(item, "name").unwrap().as_str()));
        let rank = |item: &JsValue| match text_field(item, "namespace").as_deref() {
            Some("builtin") => 0,
            Some("project") => 1,
            _ => 2,
        };
        matched.sort_by(|a, b| {
            rank(a).cmp(&rank(b)).then_with(|| {
                collator.compare(
                    &text_field(a, "name").unwrap(),
                    &text_field(b, "name").unwrap(),
                )
            })
        });
        let signature = encoded(&object([
            ("projectKey", project.to_string_lossy().as_ref().into()),
            ("query", query.into()),
        ]));
        let offset = cursor(field(params, "cursor"), &signature)?;
        let page: Vec<_> = matched.iter().skip(offset).take(limit).cloned().collect();
        let builtin = |v: &JsValue| text_field(v, "namespace").as_deref() == Some("builtin");
        let mut result = JsObject::new();
        result.insert("pinned".into(), JsValue::Array(pinned));
        result.insert(
            "builtIn".into(),
            JsValue::Array(page.iter().filter(|v| builtin(v)).cloned().collect()),
        );
        result.insert(
            "custom".into(),
            JsValue::Array(page.iter().filter(|v| !builtin(v)).cloned().collect()),
        );
        if offset.saturating_add(page.len()) < matched.len() {
            result.insert(
                "nextCursor".into(),
                encoded(&object([
                    ("offset", JsValue::Number((offset + page.len()) as f64)),
                    ("signature", signature.as_str().into()),
                ]))
                .as_str()
                .into(),
            );
        }
        Ok(JsValue::Object(result))
    }
}
