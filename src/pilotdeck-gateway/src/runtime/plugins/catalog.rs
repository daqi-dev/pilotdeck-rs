//! Read-only commands.ts projection. Explicitly injected; gateway dispatch is unchanged.
use super::{
    BoxCommandsListFuture, CommandCatalogListCommands, CommandsListInput, CommandsListResult,
    PluginCommandContribution, PluginRuntimeError,
};
use crate::{
    metadata::{js_trim, js_trim_start, object},
    model_catalog::lower,
    project_files::{node_collator, node_compare_utf16, node_locale},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use icu_collator::CollatorBorrowed;
use pilotdeck_protocol::{JsString, JsValue, parse_json, stringify_json};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::Path,
};

/// Disk-backed implementation for a caller-owned, frozen command contribution lease.
#[derive(Default)]
pub struct NativeCommandCatalogProvider;
impl CommandCatalogListCommands for NativeCommandCatalogProvider {
    fn list(
        &self,
        input: CommandsListInput,
        pilot_home: String,
        commands: Vec<PluginCommandContribution>,
    ) -> BoxCommandsListFuture {
        let query = input.query.clone().unwrap_or_default().into();
        self.list_with_query(input, pilot_home, commands, query)
    }
}
impl NativeCommandCatalogProvider {
    /// Preserve UTF-16 query units at the wire boundary without narrowing the
    /// existing string-based command bundle API used by typed callers.
    pub(crate) fn list_with_query(
        &self,
        input: CommandsListInput,
        pilot_home: String,
        commands: Vec<PluginCommandContribution>,
        query: JsString,
    ) -> BoxCommandsListFuture {
        Box::pin(async move {
            tokio::task::spawn_blocking(move || project(input, &pilot_home, commands, query))
                .await
                .map_err(|e| PluginRuntimeError::new(e.to_string()))?
        })
    }
}
static UNDEFINED: JsValue = JsValue::Undefined;
const PINNED: [&str; 3] = ["/skill_install", "/projects", "/switch-project"];
fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    value.get(key).unwrap_or(&UNDEFINED)
}
fn text(value: &JsValue, key: &str) -> JsString {
    field(value, key).as_string().cloned().unwrap_or_default()
}
fn set(value: &mut JsValue, key: &str, item: JsValue) {
    if let JsValue::Object(o) = value {
        o.insert(key.into(), item);
    }
}
fn rank(item: &JsValue) -> u8 {
    match text(item, "namespace")
        .to_utf8()
        .unwrap_or_default()
        .as_str()
    {
        "builtin" => 0,
        "project" => 1,
        _ => 2,
    }
}
fn project(
    input: CommandsListInput,
    home: &str,
    commands: Vec<PluginCommandContribution>,
    raw: JsString,
) -> Result<CommandsListResult, PluginRuntimeError> {
    let error = |s: &str| PluginRuntimeError::new(s);
    if input.project_key.is_empty() {
        return Err(PluginRuntimeError::gateway(
            "PROJECT_NOT_FOUND",
            "projectKey is required.",
        ));
    }
    let locale = node_locale(|key| std::env::var(key).ok());
    let trimmed = crate::model_catalog::trim(&raw);
    let query = lower(&trimmed, &locale.id, trimmed != raw);
    if query.len_utf16() > 256 {
        return Err(PluginRuntimeError::gateway(
            "INVALID_QUERY",
            "query must not exceed 256 characters.",
        ));
    }
    let limit = input.limit.unwrap_or(50) as usize;
    if !(1..=100).contains(&limit) {
        return Err(PluginRuntimeError::gateway(
            "INVALID_LIMIT",
            "limit must be an integer between 1 and 100.",
        ));
    }
    let collator = node_collator(&locale).map_err(|(_, s)| error(&s))?;
    let mut all = builtins();
    let mut metadata = HashMap::new();
    for (base, namespace) in [
        (
            super::loader::join_directory(Path::new(&input.project_key), ".pilotdeck"),
            "project",
        ),
        (super::loader::join_directory(Path::new(home), ""), "user"),
    ] {
        all.extend(scan(
            &super::loader::join_directory(&base, "commands"),
            namespace,
            false,
            &collator,
            &mut metadata,
        ));
        all.extend(scan(
            &super::loader::join_directory(&base, "skills"),
            namespace,
            true,
            &collator,
            &mut metadata,
        ));
    }
    for c in commands {
        let name = js_trim(&c.name);
        if name.is_empty() {
            continue;
        }
        let namespace = js_trim(&c.namespace);
        let namespace = if namespace.is_empty() {
            "extension"
        } else {
            namespace
        };
        let mut item = object([
            (
                "name",
                JsValue::String(
                    if name.starts_with('/') {
                        name.to_owned()
                    } else {
                        format!("/{name}")
                    }
                    .into(),
                ),
            ),
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
        ]);
        for (key, value) in [
            ("description", c.description),
            ("argumentHint", c.argument_hint),
        ] {
            if let Some(value) = value.filter(|s| !s.is_empty()) {
                set(&mut item, key, JsValue::String(value.into()));
            }
        }
        all.push(item);
    }
    let mut seen = HashSet::new();
    all.retain(|item| seen.insert(text(item, "name")));
    all.retain_mut(|item| {
        if query.is_empty() {
            return true;
        }
        let mut values = vec![
            ("name", text(item, "name")),
            ("description", text(item, "description")),
        ];
        if let JsValue::Array(aliases) = field(field(item, "metadata"), "aliases") {
            values.extend(
                aliases
                    .iter()
                    .filter_map(|a| a.as_string().cloned().map(|a| ("alias", a))),
            );
        }
        let mut matches = vec![];
        for (key, value) in values {
            let value = lower(&value, &locale.id, false);
            let mut from = 0;
            while from + query.len_utf16() <= value.len_utf16() {
                let Some(start) = value.units()[from..]
                    .windows(query.len_utf16())
                    .position(|s| s == query.units())
                    .map(|i| from + i)
                else {
                    break;
                };
                from = start + query.len_utf16();
                matches.push(object([
                    ("field", key.into()),
                    ("start", JsValue::Number(start as f64)),
                    ("end", JsValue::Number(from as f64)),
                ]));
            }
        }
        if matches.is_empty() {
            false
        } else {
            set(item, "matches", JsValue::Array(matches));
            true
        }
    });
    let mut pinned: Vec<_> = PINNED
        .iter()
        .filter_map(|name| {
            all.iter()
                .find(|i| text(i, "name").equals_str(name))
                .cloned()
        })
        .map(|mut i| {
            set(&mut i, "namespace", "pinned".into());
            i
        })
        .collect();
    all.retain(|item| !PINNED.iter().any(|p| text(item, "name").equals_str(p)));
    all.sort_by(|a, b| {
        rank(a)
            .cmp(&rank(b))
            .then_with(|| node_compare_utf16(&collator, &text(a, "name"), &text(b, "name")))
    });
    let signature = encode(&object([
        ("projectKey", JsValue::String(input.project_key.into())),
        ("query", JsValue::String(query)),
    ]))?;
    let offset = cursor(input.cursor.as_deref(), &signature)?;
    let mut page: Vec<_> = all.iter().skip(offset).take(limit).cloned().collect();
    // JsValue is a wire value. Delay full metadata projection until after
    // dedupe/filter/page: an excluded cyclic YAML object must not fail the list.
    for item in pinned.iter_mut().chain(page.iter_mut()) {
        if let Some(graph) = metadata.get(&text(item, "path")) {
            let value = graph
                .to_js_value()
                .map_err(|e| PluginRuntimeError::new(e.message.to_utf8().unwrap_or_default()))?;
            set(item, "metadata", value);
        }
    }
    let next = offset.saturating_add(page.len());
    let (builtin, custom) = page
        .into_iter()
        .partition(|i| text(i, "namespace").equals_str("builtin"));
    let mut value = object([
        ("pinned", JsValue::Array(pinned)),
        ("builtIn", JsValue::Array(builtin)),
        ("custom", JsValue::Array(custom)),
    ]);
    if next < all.len() {
        set(
            &mut value,
            "nextCursor",
            JsValue::String(
                encode(&object([
                    ("offset", JsValue::Number(next as f64)),
                    ("signature", JsValue::String(signature.into())),
                ]))?
                .into(),
            ),
        );
    }
    Ok(CommandsListResult { value })
}
fn encode(value: &JsValue) -> Result<String, PluginRuntimeError> {
    stringify_json(value)
        .map(|s| URL_SAFE_NO_PAD.encode(s.unwrap_or_default()))
        .map_err(|e| PluginRuntimeError::new(e.to_string()))
}
fn cursor(cursor: Option<&str>, signature: &str) -> Result<usize, PluginRuntimeError> {
    let Some(cursor) = cursor.filter(|s| !s.is_empty()) else {
        return Ok(0);
    };
    let invalid = || {
        PluginRuntimeError::gateway(
            "INVALID_CURSOR",
            "cursor is invalid or does not match the query.",
        )
    };
    let decoded = crate::skills::base64_lenient(&cursor.encode_utf16().collect::<Vec<_>>());
    let value = parse_json(&String::from_utf8_lossy(&decoded)).map_err(|_| invalid())?;
    let n = field(&value, "offset").as_number().ok_or_else(invalid)?;
    if n < 0.0
        || n.fract() != 0.0
        || !n.is_finite()
        || !text(&value, "signature").equals_str(signature)
    {
        return Err(invalid());
    }
    Ok(n as usize)
}
fn scan(
    root: &Path,
    namespace: &str,
    skills: bool,
    collator: &CollatorBorrowed<'static>,
    metadata_graphs: &mut HashMap<JsString, pilotdeck_yaml::value::ValueGraph>,
) -> Vec<JsValue> {
    let mut pending = vec![(root.to_path_buf(), true)];
    let mut output = vec![];
    // Explicit DFS preserves directory/file interleaving without recursive stack growth.
    while let Some((path, directory)) = pending.pop() {
        if directory {
            let Ok(entries) = fs::read_dir(&path).and_then(|e| e.collect::<Result<Vec<_>, _>>())
            else {
                continue;
            };
            let mut entries: Vec<_> = entries
                .into_iter()
                .filter_map(|e| {
                    e.file_type()
                        .ok()
                        .map(|kind| (e.file_name().to_string_lossy().into_owned(), kind))
                })
                .collect();
            entries.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
            entries.sort_by(|a, b| {
                node_compare_utf16(collator, &a.0.as_str().into(), &b.0.as_str().into())
            });
            for (name, kind) in entries.into_iter().rev() {
                let child = path.join(&name);
                if skills {
                    if kind.is_dir() || kind.is_symlink() {
                        pending.push((child.join("SKILL.md"), false));
                    }
                } else if kind.is_dir() || (kind.is_file() && name.ends_with(".md")) {
                    pending.push((child, kind.is_dir()));
                }
            }
            continue;
        }
        let source = fs::read(&path)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_default();
        if skills && source.is_empty() {
            continue;
        }
        let metadata = frontmatter(&source)
            .map(|graph| {
                let preview = metadata_preview(&graph);
                metadata_graphs.insert(path.to_string_lossy().as_ref().into(), graph);
                preview
            })
            .unwrap_or_else(|| object([]));
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/");
        let name = if skills {
            rel.strip_suffix("/SKILL.md").unwrap_or(&rel)
        } else {
            rel.strip_suffix(".md").unwrap_or(&rel)
        };
        let description = field(&metadata, "description")
            .as_string()
            .cloned()
            .unwrap_or_else(|| first_line(&source).into());
        let mut item = object([
            ("name", JsValue::String(format!("/{name}").into())),
            ("namespace", namespace.into()),
            ("type", if skills { "skill" } else { "command" }.into()),
            (
                "path",
                JsValue::String(path.to_string_lossy().into_owned().into()),
            ),
            ("relativePath", JsValue::String(rel.into())),
            ("description", JsValue::String(description)),
        ]);
        if !skills && let Some(hint) = field(&metadata, "argumentHint").as_string() {
            set(&mut item, "argumentHint", JsValue::String(hint.clone()));
        }
        set(&mut item, "metadata", metadata);
        output.push(item);
    }
    output
}
fn frontmatter(source: &str) -> Option<pilotdeck_yaml::value::ValueGraph> {
    let raw = source
        .strip_prefix("---\r\n")
        .or_else(|| source.strip_prefix("---\n"))?;
    let end = raw.find("\n---")?;
    let raw = raw[..end].strip_suffix('\r').unwrap_or(&raw[..end]);
    let doc = pilotdeck_yaml::parse_document(&raw.into(), Default::default());
    if !doc.errors.is_empty() {
        return None;
    }
    let mut graph = doc.to_json().ok()?;
    if !matches!(graph.root, pilotdeck_yaml::value::Value::Ref(_)) {
        return None;
    }
    // Map/Set have no enumerable own keys in the JSON catalog projection.
    for node in &mut graph.nodes {
        if matches!(
            node,
            pilotdeck_yaml::value::Container::Map(_) | pilotdeck_yaml::value::Container::Set(_)
        ) {
            *node = pilotdeck_yaml::value::Container::Object(vec![]);
        }
    }
    Some(graph)
}
fn metadata_preview(graph: &pilotdeck_yaml::value::ValueGraph) -> JsValue {
    use pilotdeck_yaml::value::{Container, Value};
    let mut preview = object([]);
    if let Value::Ref(id) = graph.root
        && let Container::Object(entries) = &graph.nodes[id]
    {
        for (key, value) in entries {
            if let Value::String(s) = value
                && (key.equals_str("description") || key.equals_str("argumentHint"))
            {
                set(
                    &mut preview,
                    &key.to_utf8().unwrap_or_default(),
                    JsValue::String(s.as_ref().clone()),
                );
            } else if key.equals_str("aliases")
                && let Value::Ref(id) = value
                && let Container::Array(items) = &graph.nodes[*id]
            {
                set(
                    &mut preview,
                    "aliases",
                    JsValue::Array(
                        items
                            .iter()
                            .filter_map(|v| match v {
                                Value::String(s) => Some(JsValue::String(s.as_ref().clone())),
                                _ => None,
                            })
                            .collect(),
                    ),
                );
            }
        }
    }
    preview
}
fn first_line(source: &str) -> &str {
    let source = source
        .strip_prefix("---")
        .and_then(|s| s.find("---").map(|i| js_trim_start(&s[i + 3..])))
        .unwrap_or(source);
    let line = source
        .split('\n')
        .find(|line| !js_trim(line).is_empty())
        .unwrap_or("");
    js_trim(if line.starts_with('#') {
        js_trim_start(line.trim_start_matches('#'))
    } else {
        line
    })
}
fn builtins() -> Vec<JsValue> {
    let web = [
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
    let registry = [
        ("plan", "先生成计划并请求确认", &[][..]),
        ("projects", "List available projects", &["项目列表"][..]),
        (
            "switch-project",
            "Switch active project for this chat",
            &["切换项目"][..],
        ),
        (
            "update",
            "Install the latest supported Release and request a Web service restart",
            &["升级", "更新"][..],
        ),
        ("status", "Show PilotDeck status and version", &["状态"][..]),
        (
            "search",
            "Search chat history across sessions",
            &["find", "grep", "搜索"][..],
        ),
        ("help", "Show available commands", &["帮助"][..]),
    ];
    web.into_iter()
        .map(|(n, d)| (n, d, None))
        .chain(registry.into_iter().map(|(n, d, a)| (n, d, Some(a))))
        .map(|(name, description, aliases)| {
            let mut metadata = object([("type", "builtin".into())]);
            if let Some(aliases) = aliases {
                set(&mut metadata, "source", "registry".into());
                if !aliases.is_empty() {
                    set(
                        &mut metadata,
                        "aliases",
                        JsValue::Array(aliases.iter().map(|s| (*s).into()).collect()),
                    );
                }
            }
            object([
                ("name", JsValue::String(format!("/{name}").into())),
                ("description", description.into()),
                ("namespace", "builtin".into()),
                ("type", "builtin".into()),
                ("metadata", metadata),
            ])
        })
        .collect()
}
#[cfg(test)]
#[path = "catalog_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "catalog_lease_tests.rs"]
mod lease_tests;
