//! Session-local tool catalog, aliases and scoped views (ToolRegistry.ts).
use super::{ExecutableTool, named, object};
use crate::{
    host::{HostError, ModuleError},
    project_files::{node_collator, node_locale},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicBool, Ordering},
};

pub type ToolPolicy = Arc<dyn Fn(&dyn ExecutableTool) -> bool + Send + Sync>;

#[derive(Clone, Debug, PartialEq)]
pub struct UnavailableTool {
    pub name: JsString,
    pub code: JsString,
    pub reason: JsString,
}
#[derive(Clone, Debug, PartialEq)]
pub struct UnavailableEntry {
    pub diagnostic: UnavailableTool,
    pub aliases: Vec<JsString>,
}
struct Entry {
    name: JsString,
    aliases: Vec<JsString>,
    tool: Arc<dyn ExecutableTool>,
    active: Arc<AtomicBool>,
}
impl Entry {
    fn new(tool: Arc<dyn ExecutableTool>) -> Self {
        Self {
            name: name(tool.as_ref()),
            aliases: aliases(tool.as_ref()),
            tool,
            active: Arc::new(AtomicBool::new(true)),
        }
    }
}
#[derive(Default)]
struct State {
    disposed: bool,
    tools: Vec<Entry>,
    aliases: Vec<(JsString, JsString)>,
    unavailable: Vec<(JsString, UnavailableTool)>,
    hidden: Vec<JsString>,
}
struct Inner {
    state: Mutex<State>,
    parent: Option<ToolRegistry>,
    policy: Option<ToolPolicy>,
}
/// Cloning this handle shares the live registry. `snapshot` implements TS clone().
#[derive(Clone)]
pub struct ToolRegistry(Arc<Inner>);
impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new(None, None)
    }
}
impl ToolRegistry {
    pub fn new(parent: Option<Self>, policy: Option<ToolPolicy>) -> Self {
        Self(Arc::new(Inner {
            state: Mutex::default(),
            parent,
            policy,
        }))
    }
    pub fn is_disposed(&self) -> bool {
        self.0.state.lock().unwrap().disposed
    }
    pub fn register(&self, tool: Arc<dyn ExecutableTool>) -> Result<ToolRegistration, HostError> {
        let entry = Entry::new(tool);
        let mut state = self.0.state.lock().unwrap();
        active(&state, "register a tool")?;
        let name = entry.name.clone();
        if state.tools.iter().any(|e| name_of(e) == &name) {
            return Err(error(named("Tool ", &name, " is already registered.")));
        }
        if state.aliases.iter().any(|(a, _)| a == &name) {
            return Err(error(named(
                "Tool ",
                &name,
                " conflicts with an existing alias.",
            )));
        }
        for alias in &entry.aliases {
            if state.tools.iter().any(|e| name_of(e) == alias) {
                return Err(error(named(
                    "Alias ",
                    alias,
                    " conflicts with an existing tool name.",
                )));
            }
            if state.aliases.iter().any(|(a, _)| a == alias) {
                return Err(error(named("Alias ", alias, " is already registered.")));
            }
        }
        Ok(self.insert(&mut state, entry, true))
    }
    fn insert(
        &self,
        state: &mut State,
        entry: Entry,
        clear_alias_diagnostics: bool,
    ) -> ToolRegistration {
        let name = entry.name.clone();
        state.unavailable.retain(|(key, _)| key != &name);
        for alias in &entry.aliases {
            set(&mut state.aliases, alias.clone(), name.clone());
            if clear_alias_diagnostics {
                state.unavailable.retain(|(key, _)| key != alias);
            }
        }
        let active = entry.active.clone();
        state.tools.push(entry);
        ToolRegistration {
            name,
            active,
            registry: Arc::downgrade(&self.0),
        }
    }
    pub fn replace(&self, tool: Arc<dyn ExecutableTool>) -> Result<ToolRegistration, HostError> {
        let entry = Entry::new(tool);
        let mut state = self.0.state.lock().unwrap();
        active(&state, "replace a tool")?;
        let name = entry.name.clone();
        let Some(index) = state.tools.iter().position(|e| name_of(e) == &name) else {
            return Err(error(named(
                "Tool ",
                &name,
                " is not registered — cannot replace.",
            )));
        };
        let removed = remove(&mut state, index);
        // TS replace intentionally does not repeat register's alias checks.
        let registration = self.insert(&mut state, entry, false);
        drop(state);
        drop(removed);
        Ok(registration)
    }
    pub fn register_or_replace(
        &self,
        tool: Arc<dyn ExecutableTool>,
    ) -> Result<ToolRegistration, HostError> {
        // Serialize the compound operation. The local replacement and insertion
        // must not race another register_or_replace on a native worker thread.
        let entry = Entry::new(tool);
        let mut state = self.0.state.lock().unwrap();
        if let Some(index) = state.tools.iter().position(|e| name_of(e) == &entry.name) {
            active(&state, "replace a tool")?;
            let removed = remove(&mut state, index);
            let result = self.insert(&mut state, entry, false);
            drop(state);
            drop(removed);
            Ok(result)
        } else {
            // register's validation is performed under the same lock.
            active(&state, "register a tool")?;
            if state.aliases.iter().any(|(a, _)| a == &entry.name) {
                return Err(error(named(
                    "Tool ",
                    &entry.name,
                    " conflicts with an existing alias.",
                )));
            }
            for alias in &entry.aliases {
                if state.tools.iter().any(|e| name_of(e) == alias) {
                    return Err(error(named(
                        "Alias ",
                        alias,
                        " conflicts with an existing tool name.",
                    )));
                }
                if state.aliases.iter().any(|(a, _)| a == alias) {
                    return Err(error(named("Alias ", alias, " is already registered.")));
                }
            }
            Ok(self.insert(&mut state, entry, true))
        }
    }
    pub fn unregister(&self, name: &JsString) -> Result<bool, HostError> {
        let mut state = self.0.state.lock().unwrap();
        active(&state, "unregister a tool")?;
        if let Some(index) = state.tools.iter().position(|e| name_of(e) == name) {
            let removed = remove(&mut state, index);
            drop(state);
            drop(removed);
            Ok(true)
        } else {
            Ok(false)
        }
    }
    fn visible(&self, tool: &dyn ExecutableTool) -> bool {
        self.0.policy.as_ref().is_none_or(|p| p(tool))
    }
    pub fn get(&self, requested: &JsString) -> Option<Arc<dyn ExecutableTool>> {
        // User predicates run outside the lock; they may query this registry.
        let local = {
            let state = self.0.state.lock().unwrap();
            if state.disposed {
                return None;
            }
            let real = lookup(&state.aliases, requested).unwrap_or(requested);
            state
                .tools
                .iter()
                .find(|e| name_of(e) == real)
                .map(|e| e.tool.clone())
        };
        if let Some(tool) = local {
            return self.visible(tool.as_ref()).then_some(tool);
        }
        let inherited = self.0.parent.as_ref()?.get(requested)?;
        let inherited_name = name(inherited.as_ref());
        let shadow = self
            .0
            .state
            .lock()
            .unwrap()
            .tools
            .iter()
            .find(|e| name_of(e) == &inherited_name)
            .map(|e| e.tool.clone());
        let tool = shadow.unwrap_or(inherited);
        self.visible(tool.as_ref()).then_some(tool)
    }
    pub fn has(&self, name: &JsString) -> bool {
        self.get(name).is_some()
    }
    pub fn list_all(&self) -> Result<Vec<Arc<dyn ExecutableTool>>, HostError> {
        let locals = {
            let state = self.0.state.lock().unwrap();
            if state.disposed {
                return Ok(vec![]);
            }
            state
                .tools
                .iter()
                .map(|e| e.tool.clone())
                .collect::<Vec<_>>()
        };
        let mut tools = self
            .0
            .parent
            .as_ref()
            .map(Self::list_all)
            .transpose()?
            .unwrap_or_default();
        tools.retain(|t| self.visible(t.as_ref()));
        for tool in locals {
            let index = tools
                .iter()
                .position(|t| name(t.as_ref()) == name(tool.as_ref()));
            if self.visible(tool.as_ref()) {
                if let Some(index) = index {
                    tools[index] = tool;
                } else {
                    tools.push(tool);
                }
            } else if let Some(index) = index {
                tools.remove(index);
            }
        }
        let collator = node_collator(&node_locale(|key| std::env::var(key).ok()))
            .map_err(|(_, message)| error(message))?;
        tools.sort_by(|a, b| {
            collator.compare_utf16(name(a.as_ref()).units(), name(b.as_ref()).units())
        });
        Ok(tools)
    }
    pub fn list(&self) -> Result<Vec<Arc<dyn ExecutableTool>>, HostError> {
        let mut tools = self.list_all()?;
        let hidden = self.0.state.lock().unwrap().hidden.clone();
        tools.retain(|tool| !hidden.contains(&name(tool.as_ref())));
        Ok(tools)
    }
    pub fn hide(&self, requested: &JsString) -> bool {
        let Some(tool) = self.get(requested) else {
            return false;
        };
        let name = name(tool.as_ref());
        let mut state = self.0.state.lock().unwrap();
        if !state.hidden.contains(&name) {
            state.hidden.push(name);
        }
        true
    }
    pub fn reveal(&self, requested: &JsString) -> bool {
        let mut state = self.0.state.lock().unwrap();
        let real = lookup(&state.aliases, requested)
            .unwrap_or(requested)
            .clone();
        let before = state.hidden.len();
        state.hidden.retain(|name| name != &real);
        before != state.hidden.len()
    }
    pub fn is_hidden(&self, requested: &JsString) -> bool {
        let state = self.0.state.lock().unwrap();
        state
            .hidden
            .contains(lookup(&state.aliases, requested).unwrap_or(requested))
    }
    pub fn mark_unavailable(
        &self,
        diagnostic: UnavailableTool,
        aliases: &[JsString],
    ) -> Result<(), HostError> {
        let mut state = self.0.state.lock().unwrap();
        active(&state, "mark a tool unavailable")?;
        set(
            &mut state.unavailable,
            diagnostic.name.clone(),
            diagnostic.clone(),
        );
        for alias in aliases {
            set(&mut state.unavailable, alias.clone(), diagnostic.clone());
        }
        Ok(())
    }
    pub fn get_unavailable(&self, requested: &JsString) -> Option<UnavailableTool> {
        let state = self.0.state.lock().unwrap();
        if state.disposed {
            return None;
        }
        let real = lookup(&state.aliases, requested).unwrap_or(requested);
        lookup(&state.unavailable, real)
            .or_else(|| lookup(&state.unavailable, requested))
            .cloned()
            .or_else(|| self.0.parent.as_ref()?.get_unavailable(requested))
    }
    pub fn unavailable_entries(&self) -> Result<Vec<UnavailableEntry>, HostError> {
        let unavailable = {
            let state = self.0.state.lock().unwrap();
            if state.disposed {
                return Ok(vec![]);
            }
            state.unavailable.clone()
        };
        let mut entries = self
            .0
            .parent
            .as_ref()
            .map(Self::unavailable_entries)
            .transpose()?
            .unwrap_or_default();
        for (key, diagnostic) in unavailable {
            let index = entries
                .iter()
                .position(|e| e.diagnostic.name == diagnostic.name)
                .unwrap_or_else(|| {
                    entries.push(UnavailableEntry {
                        diagnostic: diagnostic.clone(),
                        aliases: vec![],
                    });
                    entries.len() - 1
                });
            if key != diagnostic.name && !entries[index].aliases.contains(&key) {
                entries[index].aliases.push(key);
            }
        }
        for entry in &mut entries {
            entry.aliases.sort_by(|a, b| a.units().cmp(b.units()));
        }
        let collator = node_collator(&node_locale(|key| std::env::var(key).ok()))
            .map_err(|(_, message)| error(message))?;
        entries.sort_by(|a, b| {
            collator.compare_utf16(a.diagnostic.name.units(), b.diagnostic.name.units())
        });
        Ok(entries)
    }
    pub fn canonical_schemas(&self) -> Result<Vec<JsObject>, HostError> {
        Ok(self
            .list()?
            .iter()
            .map(|tool| {
                object(["name", "description", "inputSchema"].map(|key| {
                    (
                        key,
                        tool.fields()
                            .get(key)
                            .cloned()
                            .unwrap_or(JsValue::Undefined),
                    )
                }))
            })
            .collect())
    }
    pub fn snapshot(&self) -> Result<Self, HostError> {
        active(&self.0.state.lock().unwrap(), "clone a tool registry")?;
        let copy = Self::default();
        for tool in self.list()? {
            copy.register(tool)?;
        }
        for entry in self.unavailable_entries()? {
            copy.mark_unavailable(entry.diagnostic, &entry.aliases)?;
        }
        copy.0.state.lock().unwrap().hidden = self.0.state.lock().unwrap().hidden.clone();
        Ok(copy)
    }
    pub fn scoped(&self, policy: ToolPolicy) -> Result<Self, HostError> {
        active(&self.0.state.lock().unwrap(), "create a scoped tool view")?;
        Ok(Self::new(Some(self.clone()), Some(policy)))
    }
    pub fn dispose(&self) {
        let mut state = self.0.state.lock().unwrap();
        state.disposed = true;
        for entry in &state.tools {
            entry.active.store(false, Ordering::Release);
        }
        let removed = std::mem::take(&mut state.tools);
        state.aliases.clear();
        state.unavailable.clear();
        drop(state);
        drop(removed);
    }
}
pub struct ToolRegistration {
    pub name: JsString,
    active: Arc<AtomicBool>,
    registry: Weak<Inner>,
}
impl ToolRegistration {
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }
    /// Explicit disposal matches TS. Dropping a handle alone keeps registration.
    pub fn dispose(&self) {
        if !self.active.swap(false, Ordering::AcqRel) {
            return;
        }
        if let Some(registry) = self.registry.upgrade() {
            let mut state = registry.state.lock().unwrap();
            let removed = state
                .tools
                .iter()
                .position(|e| Arc::ptr_eq(&e.active, &self.active))
                .map(|index| remove(&mut state, index));
            drop(state);
            drop(removed);
        }
    }
}
pub(super) fn name(tool: &dyn ExecutableTool) -> JsString {
    tool.fields()
        .get("name")
        .and_then(JsValue::as_string)
        .expect("registered tool name")
        .clone()
}
fn name_of(entry: &Entry) -> &JsString {
    &entry.name
}
pub(super) fn aliases(tool: &dyn ExecutableTool) -> Vec<JsString> {
    match tool.fields().get("aliases") {
        Some(JsValue::Array(values)) => values
            .iter()
            .filter_map(|v| v.as_string().cloned())
            .collect(),
        _ => vec![],
    }
}
fn remove(state: &mut State, index: usize) -> Entry {
    let entry = state.tools.remove(index);
    for alias in &entry.aliases {
        state.aliases.retain(|(a, _)| a != alias);
        state.unavailable.retain(|(a, _)| a != alias);
    }
    state.unavailable.retain(|(a, _)| a != &entry.name);
    entry.active.store(false, Ordering::Release);
    entry
}
fn active(state: &State, action: &str) -> Result<(), HostError> {
    if state.disposed {
        Err(error(format!(
            "Cannot {action}; tool registry is disposed."
        )))
    } else {
        Ok(())
    }
}
fn error(message: impl Into<JsString>) -> HostError {
    ModuleError::new(message).into()
}
fn lookup<'a, T>(items: &'a [(JsString, T)], key: &JsString) -> Option<&'a T> {
    items.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}
fn set<T>(items: &mut Vec<(JsString, T)>, key: JsString, value: T) {
    if let Some((_, v)) = items.iter_mut().find(|(k, _)| k == &key) {
        *v = value;
    } else {
        items.push((key, value));
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct NameRepair {
    pub name: JsString,
    pub reason: &'static str,
}
/// Resolve only unambiguous spellings; an exact registry lookup precedes this.
/// Errors are the TypeErrors TS throws for a non-string configured target and
/// for the one builtin-alias lookup that reaches Object.prototype.
pub fn repair_tool_name(
    raw: &JsString,
    targets: &[(JsString, Vec<JsString>)],
    configured: Option<&JsObject>,
) -> Result<Option<NameRepair>, HostError> {
    let variants = normalized_variants(raw);
    if variants.is_empty() {
        return Ok(None);
    }
    let spellings: Vec<_> = targets
        .iter()
        .flat_map(|(name, aliases)| {
            std::iter::once(name)
                .chain(aliases)
                .flat_map(|s| {
                    normalized_variants(s)
                        .into_iter()
                        .map(|v| (v, name.clone()))
                })
                .collect::<Vec<_>>()
        })
        .collect();
    let unique = |variant: &str| {
        let mut matches = spellings
            .iter()
            .filter(|(v, _)| v == variant)
            .map(|(_, n)| n);
        let first = matches.next()?;
        matches.all(|n| n == first).then(|| first.clone())
    };
    let resolve = |target: &JsString| normalized_variants(target).iter().find_map(|v| unique(v));
    if let Some(configured) = configured {
        let mut entries: Vec<_> = configured.entries().iter().collect();
        entries.sort_by_key(|(key, _)| array_index(key).map_or((1, 0), |n| (0, n)));
        for (source, target) in entries {
            let source_variants = normalized_variants(source);
            if raw == source || variants.iter().any(|v| source_variants.contains(v)) {
                let target = match target {
                    JsValue::String(target) => target,
                    JsValue::Null => {
                        return Err(error("Cannot read properties of null (reading 'trim')"));
                    }
                    JsValue::Undefined => {
                        return Err(error(
                            "Cannot read properties of undefined (reading 'trim')",
                        ));
                    }
                    _ => return Err(error("name.trim is not a function")),
                };
                if let Some(name) = resolve(target) {
                    return Ok(Some(NameRepair {
                        name,
                        reason: "configured_alias",
                    }));
                }
                // First matching configured source wins even if its target is absent.
                break;
            }
        }
    }
    if let Some(name) = variants.iter().find_map(|v| unique(v)) {
        return Ok(Some(NameRepair {
            name,
            reason: "normalized_match",
        }));
    }
    for variant in &variants {
        // BUILTIN_ALIASES is a plain object literal; after normalization only
        // `constructor` reaches an inherited (non-iterable) property.
        if variant == "constructor" {
            return Err(error("targets is not iterable"));
        }
        if let Some(name) = builtin_aliases(variant)
            .iter()
            .find_map(|s| resolve(&(*s).into()))
        {
            return Ok(Some(NameRepair {
                name,
                reason: "builtin_alias",
            }));
        }
    }
    let mut best: Option<(usize, JsString)> = None;
    let mut tied = false;
    for variant in variants {
        for (spelling, name) in &spellings {
            let bound = match variant.len().min(spelling.len()) {
                0..=2 => 0,
                3..=4 => 1,
                _ => 2,
            };
            let distance = edit_distance(variant.as_bytes(), spelling.as_bytes());
            if distance > bound {
                continue;
            }
            match &best {
                None => {
                    best = Some((distance, name.clone()));
                    tied = false;
                }
                Some((old, _)) if distance < *old => {
                    best = Some((distance, name.clone()));
                    tied = false;
                }
                Some((old, current)) if distance == *old && name != current => tied = true,
                _ => {}
            }
        }
    }
    Ok(best.filter(|_| !tied).map(|(_, name)| NameRepair {
        name,
        reason: "fuzzy_match",
    }))
}
pub(super) fn array_index(key: &JsString) -> Option<u32> {
    let text = key.to_utf8().ok()?;
    let n = text.parse::<u32>().ok()?;
    (n != u32::MAX && n.to_string() == text).then_some(n)
}
fn normalized_variants(name: &JsString) -> Vec<String> {
    let input = String::from_utf16_lossy(name.units());
    let input = input.trim_matches(super::js_space);
    let mut values = Vec::new();
    let mut add = |s: &str| {
        let chars: Vec<_> = s.chars().collect();
        let mut value = String::new();
        for (i, &c) in chars.iter().enumerate() {
            if c.is_ascii_alphanumeric() {
                if c.is_ascii_uppercase()
                    && i > 0
                    && (chars[i - 1].is_ascii_lowercase()
                        || chars[i - 1].is_ascii_digit()
                        || (chars[i - 1].is_ascii_uppercase()
                            && chars.get(i + 1).is_some_and(char::is_ascii_lowercase)))
                {
                    value.push('_');
                }
                value.push(c.to_ascii_lowercase());
            } else if !value.ends_with('_') {
                value.push('_');
            }
        }
        let value = value.trim_matches('_').to_owned();
        if !value.is_empty() && !values.contains(&value) {
            values.push(value);
        }
    };
    add(input);
    for part in input.split("::").flat_map(|s| s.split(['.', '/', '\\'])) {
        add(part);
    }
    for value in values.clone() {
        let mut current = value.as_str();
        loop {
            let before = current;
            for prefix in [
                "tool_",
                "tools_",
                "function_",
                "functions_",
                "pilotdeck_",
                "builtin_",
            ] {
                if current.len() > prefix.len() {
                    current = current.strip_prefix(prefix).unwrap_or(current);
                }
            }
            for suffix in ["_tool", "_tools", "_function", "_functions"] {
                if current.len() > suffix.len() {
                    current = current.strip_suffix(suffix).unwrap_or(current);
                }
            }
            if current == before {
                break;
            }
        }
        if !values.iter().any(|v| v == current) {
            values.push(current.to_owned());
        }
    }
    values
}
fn edit_distance(a: &[u8], b: &[u8]) -> usize {
    let mut row: Vec<_> = (0..=b.len()).collect();
    for (i, x) in a.iter().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, y) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (row[j] + 1)
                .min(above + 1)
                .min(diagonal + usize::from(x != y));
            diagonal = above;
        }
    }
    row[b.len()]
}
fn builtin_aliases(name: &str) -> &'static [&'static str] {
    match name {
        "run" | "shell" | "sh" | "terminal" | "command" | "exec" | "execute" => &["bash"],
        "write" | "write_file" | "writefile" | "create_file" | "createfile" | "save_file"
        | "savefile" => &["write_file", "write"],
        "read" | "read_file" | "readfile" | "open_file" | "openfile" | "cat_file" | "catfile" => {
            &["read_file", "read"]
        }
        "edit" | "edit_file" | "editfile" | "modify_file" | "modifyfile" | "patch_file"
        | "patchfile" | "replace_file" | "replacefile" => &["edit_file", "edit"],
        "web_fetch" | "webfetch" => &["web_fetch", "webfetch"],
        "fetch_url" | "fetchurl" | "url_fetch" | "urlfetch" => &["web_fetch"],
        "web_search" | "websearch" => &["web_search", "websearch"],
        "search_web" | "searchweb" => &["web_search"],
        "todo_write" | "todowrite" => &["todo_write", "todowrite"],
        "todo" | "update_todo" | "updatetodo" => &["todo_write"],
        "ask_user" | "ask_user_question" => &["ask_user_question", "ask_user"],
        "askuser" | "ask_question" | "askquestion" => &["ask_user_question"],
        "task" | "subagent" => &["agent"],
        "edit_notebook" | "notebook_edit" | "notebookedit" => &["edit_notebook", "notebook_edit"],
        "editnotebook" => &["edit_notebook"],
        "enter_plan_mode" | "enterplanmode" => &["enter_plan_mode", "enterplanmode"],
        "exit_plan_mode" | "exitplanmode" => &["exit_plan_mode", "exitplanmode"],
        "list_mcp_resources" | "listmcpresources" => &["list_mcp_resources", "listmcpresources"],
        "read_mcp_resource" | "readmcpresource" => &["read_mcp_resource", "readmcpresource"],
        _ => &[],
    }
}
