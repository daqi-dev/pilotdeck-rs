//! Native skill manager, ported against PilotDeck cc7c4e6
//! src/extension/skills/SkillManager.ts: list / read / write / create / delete /
//! validate. `import`, `scan` and the disabled/external skills binding
//! selection (createLocalGateway.ts:851–860) belong to A6.8-pr4. This module is
//! not wired into Gateway dispatch.
//!
//! Inputs and results are wire `JsValue`s: the gateway hands frame params to
//! the manager untyped (GatewayWsConnection.ts:542–557), so every TS runtime
//! check (`typeof`, truthiness, `??`) is mirrored, and results keep the TS
//! object key order. Live TS cross-tests are `#[ignore]` (need PDRS_BASELINE);
//! the non-ignored golden test replays tests/fixtures/skills/oracle.json.
mod error;
mod frontmatter;
#[cfg(test)]
mod tests;
mod validate;

pub use error::SkillError;

use crate::{
    metadata::{argument_received, object},
    model_catalog::lower,
    project_files::{node_collator, node_locale},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use icu_collator::CollatorBorrowed;
use icu_locale_core::Locale;
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use pilotdeck_yaml::value::Value;
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{self, Write},
    os::unix::fs::MetadataExt,
};

type Result<T> = std::result::Result<T, SkillError>;
static UNDEFINED: JsValue = JsValue::Undefined;
const NL: u16 = b'\n' as u16;
const LIST_SCOPES: [&str; 5] = ["builtin", "user", "project", "plugin", "all"];

pub struct SkillManagerOptions {
    /// `~/.pilotdeck` root (resolved against the cwd). Required.
    pub pilot_home: String,
    /// isVirtualProjectRoot's env; `None` reads the process environment, as TS.
    pub env: Option<HashMap<String, String>>,
    /// Read-only skills shipped with the build; empty counts as unset (TS truthiness).
    pub builtin_skills_root: Option<String>,
    /// "General chat" cwds; defaults to `[pilot_home]`.
    pub general_cwd_paths: Option<Vec<String>>,
    /// Rust-only: Node takes the ICU default locale (localeCompare,
    /// toLocaleLowerCase) and `os.homedir()` from the process. `None` does too.
    pub locale: Option<Locale>,
    pub home_dir: Option<String>,
}
impl SkillManagerOptions {
    pub fn new(pilot_home: impl Into<String>) -> Self {
        Self {
            pilot_home: pilot_home.into(),
            env: None,
            builtin_skills_root: None,
            general_cwd_paths: None,
            locale: None,
            home_dir: None,
        }
    }
}

pub struct SkillManager {
    pilot_home: String,
    builtin_root: Option<String>,
    general_cwd_paths: Vec<String>,
    env: Option<HashMap<String, String>>,
    locale: Locale,
    home_dir: Option<String>,
}

impl SkillManager {
    /// SkillManager.ts:84–92.
    pub fn new(options: SkillManagerOptions) -> Self {
        let pilot_home = resolve(&options.pilot_home);
        let general_cwd_paths = match options.general_cwd_paths {
            Some(paths) => paths.iter().map(|p| resolve(p)).collect(),
            None => vec![pilot_home.clone()],
        };
        Self {
            builtin_root: options
                .builtin_skills_root
                .filter(|r| !r.is_empty())
                .map(|r| resolve(&r)),
            general_cwd_paths,
            pilot_home,
            env: options.env,
            locale: options
                .locale
                .unwrap_or_else(|| node_locale(|key| std::env::var(key).ok())),
            home_dir: options.home_dir,
        }
    }

    // ---------------------------------------------------------------- paths

    fn user_root(&self) -> String {
        join(&self.pilot_home, "skills")
    }

    fn env_var(&self, key: &str) -> Option<String> {
        match &self.env {
            Some(env) => env.get(key).cloned(),
            None => std::env::var(key).ok(),
        }
    }

    /// SkillManager.ts:113; a truthy non-string key throws from path.resolve.
    fn is_general_cwd(&self, key: &JsValue) -> Result<bool> {
        if !truthy(key) {
            return Ok(false);
        }
        let key = path_arg(key)?;
        Ok(self.general_cwd_paths.contains(&resolve(&key)) || self.is_virtual_root(&key))
    }

    /// writablePaths.ts:36–64 isVirtualProjectRoot / isDesktopRuntimeProjectRoot.
    fn is_virtual_root(&self, root: &str) -> bool {
        let root = resolve(root);
        if root == resolve(&self.pilot_home) {
            return true;
        }
        if self.env_var("PILOTDECK_DESKTOP").as_deref() != Some("1") {
            return false;
        }
        if let Some(runtime) = self
            .env_var("PILOTDECK_DESKTOP_RUNTIME_ROOT")
            .filter(|v| !v.is_empty())
            && root == resolve(&runtime)
        {
            return true;
        }
        // /[\\/]resources[\\/]runtime$/i (non-unicode /i folds ASCII only here).
        let b = root.as_bytes();
        b.len() >= 18 && {
            let t = &b[b.len() - 18..];
            b"/\\".contains(&t[0])
                && t[1..10].eq_ignore_ascii_case(b"resources")
                && b"/\\".contains(&t[10])
                && t[11..].eq_ignore_ascii_case(b"runtime")
        }
    }

    /// SkillManager.ts:124.
    fn resolve_scope_root(&self, scope: &JsValue, project_key: &JsValue) -> Result<String> {
        if is_str(scope, "builtin") {
            return self.builtin_root.clone().ok_or_else(|| {
                SkillError::manager("not_configured", "Built-in skills root is not configured.")
            });
        }
        if is_str(scope, "project") {
            if !truthy(project_key) || self.is_general_cwd(project_key)? {
                return Err(SkillError::manager(
                    "project_required",
                    "Project scope requires a real project (general chat doesn't qualify).",
                ));
            }
            return Ok(project_root(&path_arg(project_key)?));
        }
        Ok(self.user_root())
    }

    /// SkillManager.ts:149; returns the directory and the (validated) slug value.
    fn resolve_skill_dir(&self, input: &JsValue) -> Result<(String, JsValue)> {
        let slug = field(input, "slug")?;
        let Some(name) = valid_slug(slug) else {
            return Err(SkillError::manager(
                "invalid_slug",
                format!(
                    "Invalid slug \"{}\". Allowed: [a-zA-Z0-9][a-zA-Z0-9._-]{{0,99}}, no \"..\".",
                    display(slug)
                ),
            ));
        };
        let root = self.resolve_scope_root(field(input, "scope")?, field(input, "projectKey")?)?;
        Ok((join(&root, &name), slug.clone()))
    }

    // ----------------------------------------------------------- public API

    /// SkillManager.ts:164–234.
    pub fn list(&self, input: &JsValue) -> Result<JsValue> {
        let project_key = or(field(input, "projectKey")?, JsValue::Null);
        let effective = if self.is_general_cwd(&project_key)? {
            JsValue::Null
        } else {
            project_key
        };
        let collator = node_collator(&self.locale)
            .map_err(|(code, message)| SkillError::host(code, message))?;
        let builtin = match &self.builtin_root {
            Some(root) => list_skills_in(root, "builtin", &collator)?,
            None => vec![],
        };
        let user = list_skills_in(&self.user_root(), "user", &collator)?;
        let project = if truthy(&effective) {
            list_skills_in(&project_root(&path_arg(&effective)?), "project", &collator)?
        } else {
            vec![]
        };
        let slugs = |list: &[JsObject]| list.iter().map(slug_of).collect::<HashSet<_>>();
        let (builtin_slugs, user_slugs, project_slugs) =
            (slugs(&builtin), slugs(&user), slugs(&project));
        let set = |o: &mut JsObject, key: &str, value: JsValue| {
            o.insert(key.into(), value);
        };
        let builtin: Vec<JsObject> = builtin
            .into_iter()
            .map(|mut s| {
                let slug = slug_of(&s);
                if project_slugs.contains(&slug) {
                    set(&mut s, "overriddenBy", "project".into());
                } else if user_slugs.contains(&slug) {
                    set(&mut s, "overriddenBy", "user".into());
                }
                s
            })
            .collect();
        let user: Vec<JsObject> = user
            .into_iter()
            .map(|mut s| {
                let slug = slug_of(&s);
                if builtin_slugs.contains(&slug) {
                    set(&mut s, "overridesBuiltin", JsValue::Bool(true));
                }
                if project_slugs.contains(&slug) {
                    set(&mut s, "overriddenBy", "project".into());
                }
                s
            })
            .collect();
        let project: Vec<JsObject> = project
            .into_iter()
            .map(|mut s| {
                if builtin_slugs.contains(&slug_of(&s)) {
                    set(&mut s, "overridesBuiltin", JsValue::Bool(true));
                }
                s
            })
            .collect();

        let scope = or(field(input, "scope")?, "all".into());
        let Some(scope_name) = LIST_SCOPES.iter().find(|s| is_str(&scope, s)) else {
            return Err(invalid_input(format!(
                "Invalid skill scope: {}.",
                display(&scope)
            )));
        };
        let query = match field(input, "query")? {
            v if nullish(v) => JsString::default(),
            JsValue::String(s) => {
                let trimmed = trim_units(s.units());
                // V8 trim returns a slice only when it removed something.
                lower(
                    &JsString::from_utf16(trimmed.to_vec()),
                    &self.locale.id,
                    trimmed.len() != s.len_utf16(),
                )
            }
            _ => {
                return Err(SkillError::type_error(
                    "",
                    "(input.query ?? \"\").trim is not a function",
                ));
            }
        };
        if query.len_utf16() > 256 {
            return Err(invalid_input("query must not exceed 256 characters."));
        }
        let limit = match or(field(input, "limit")?, JsValue::Number(10.0)) {
            JsValue::Number(n) if is_integer(n) && (1.0..=50.0).contains(&n) => n as usize,
            _ => return Err(invalid_input("limit must be an integer between 1 and 50.")),
        };
        let signature = base64url(&object([
            ("projectKey", effective.clone()),
            ("query", JsValue::String(query.clone())),
            ("scope", scope.clone()),
        ]));
        let offset = decode_list_cursor(field(input, "cursor")?, &signature)?;
        let selected: Vec<&JsObject> = match *scope_name {
            "builtin" => builtin.iter().collect(),
            "user" => user.iter().collect(),
            "project" => project.iter().collect(),
            "plugin" => vec![],
            _ => builtin.iter().chain(&user).chain(&project).collect(),
        };
        let mut searchable: Vec<(JsString, JsObject)> = vec![];
        for skill in selected {
            let mut command = vec![u16::from(b'/')];
            command.extend(slug_of(skill).units());
            let command = JsString::from_utf16(command);
            let matches = self.skill_matches(skill, &query);
            if !query.is_empty() && matches.is_empty() {
                continue;
            }
            // `{ ...skill, command, matches }`; the later conditional spread
            // re-sets `matches`, so an empty array stays on every item.
            let mut item = skill.clone();
            set(&mut item, "command", JsValue::String(command.clone()));
            set(&mut item, "matches", JsValue::Array(matches));
            searchable.push((command, item));
        }
        searchable.sort_by(|a, b| collator.compare_utf16(a.0.units(), b.0.units()));
        let total = searchable.len();
        let start = if offset >= total as f64 {
            total
        } else {
            offset as usize
        };
        let items: Vec<JsValue> = searchable
            .into_iter()
            .skip(start)
            .take(limit)
            .map(|(_, item)| JsValue::Object(item))
            .collect();
        let next = offset + items.len() as f64;
        let objects =
            |list: Vec<JsObject>| JsValue::Array(list.into_iter().map(JsValue::Object).collect());
        let mut result = JsObject::new();
        set(&mut result, "builtin", objects(builtin));
        set(&mut result, "user", objects(user));
        set(&mut result, "project", objects(project));
        set(&mut result, "projectPath", effective);
        set(&mut result, "items", JsValue::Array(items));
        if next < total as f64 {
            let cursor = base64url(&object([
                ("offset", JsValue::Number(next)),
                ("signature", signature.as_str().into()),
            ]));
            set(&mut result, "nextCursor", cursor.as_str().into());
        }
        Ok(JsValue::Object(result))
    }

    /// SkillManager.ts:236–250.
    pub fn read(&self, input: &JsValue) -> Result<JsValue> {
        let (dir, slug) = self.resolve_skill_dir(input)?;
        let file = join(&dir, "SKILL.md");
        let bytes = fs::read(&file).map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                SkillError::manager("not_found", format!("SKILL.md not found at {file}."))
            } else {
                SkillError::read_file(&e, &file)
            }
        })?;
        let scope = field(input, "scope")?.clone();
        let skill = read_skill_meta(&dir, &scope);
        Ok(object([
            ("content", JsValue::String(decode_utf8(&bytes))),
            ("scope", scope),
            ("slug", slug),
            ("skill", skill),
        ]))
    }

    /// SkillManager.ts:252–263.
    pub fn write(&self, input: &JsValue) -> Result<JsValue> {
        let scope = field(input, "scope")?.clone();
        assert_mutable(&scope)?;
        let JsValue::String(content) = field(input, "content")? else {
            return Err(invalid_input("content (string) is required."));
        };
        let (dir, slug) = self.resolve_skill_dir(input)?;
        mkdir_p(&dir)?;
        write_file(&join(&dir, "SKILL.md"), content)?;
        let skill = read_skill_meta(&dir, &scope);
        Ok(object([
            ("ok", JsValue::Bool(true)),
            ("scope", scope),
            ("slug", slug),
            ("skill", skill),
        ]))
    }

    /// SkillManager.ts:265–300. Like TS, a malformed name/description/body
    /// throws after the directory was created.
    pub fn create(&self, input: &JsValue) -> Result<JsValue> {
        let scope = field(input, "scope")?.clone();
        assert_mutable(&scope)?;
        let (dir, slug) = self.resolve_skill_dir(input)?;
        // fs.access(F_OK) follows symlinks, as stat does.
        match fs::metadata(&dir) {
            Ok(_) => {
                return Err(SkillError::manager(
                    "conflict",
                    format!("Skill already exists at {dir}."),
                ));
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(SkillError::io(&e, "access", Some(&dir))),
        }
        mkdir_p(&dir)?;
        let content = match field(input, "content")? {
            JsValue::String(s) if !trim_units(s.units()).is_empty() => s.clone(),
            _ => initial_content(input, slug.as_string().expect("validated slug"))?,
        };
        write_file(&join(&dir, "SKILL.md"), &content)?;
        let skill = read_skill_meta(&dir, &scope);
        Ok(object([
            ("ok", JsValue::Bool(true)),
            ("scope", scope),
            ("slug", slug),
            ("skillPath", dir.as_str().into()),
            ("skill", skill),
        ]))
    }

    /// SkillManager.ts:302–311.
    pub fn delete(&self, input: &JsValue) -> Result<JsValue> {
        let scope = field(input, "scope")?.clone();
        assert_mutable(&scope)?;
        let (dir, slug) = self.resolve_skill_dir(input)?;
        remove_tree(&dir)?;
        Ok(object([
            ("ok", JsValue::Bool(true)),
            ("scope", scope),
            ("slug", slug),
        ]))
    }

    fn skill_matches(&self, skill: &JsObject, query: &JsString) -> Vec<JsValue> {
        let mut out = vec![];
        if query.is_empty() {
            return out;
        }
        for name in ["name", "description"] {
            let value = skill
                .get(name)
                .and_then(JsValue::as_string)
                .cloned()
                .unwrap_or_default();
            let hay = lower(&value, &self.locale.id, false);
            let (h, q) = (hay.units(), query.units());
            let mut offset = 0;
            while offset + q.len() <= h.len() {
                let Some(pos) = h[offset..].windows(q.len()).position(|w| w == q) else {
                    break;
                };
                let start = offset + pos;
                out.push(object([
                    ("field", name.into()),
                    ("start", JsValue::Number(start as f64)),
                    ("end", JsValue::Number((start + q.len()) as f64)),
                ]));
                offset = start + q.len().max(1);
            }
        }
        out
    }
}

// ------------------------------------------------------------------ helpers

fn invalid_input(message: impl Into<String>) -> SkillError {
    SkillError::manager("invalid_input", message)
}

/// SkillManager.ts:140.
fn assert_mutable(scope: &JsValue) -> Result<()> {
    if is_str(scope, "builtin") {
        return Err(SkillError::manager(
            "read_only",
            "Built-in skills are read-only. Create a user or project override to edit this skill.",
        ));
    }
    Ok(())
}

/// SkillManager.ts:36,512: `/^[a-zA-Z0-9][a-zA-Z0-9._-]{0,99}$/` and no "..".
fn valid_slug(value: &JsValue) -> Option<String> {
    let units = value.as_string()?.units();
    let ok = (1..=100).contains(&units.len())
        && units.iter().enumerate().all(|(i, &u)| {
            u < 128 && {
                let c = u as u8;
                c.is_ascii_alphanumeric() || (i > 0 && b"._-".contains(&c))
            }
        })
        && !units.windows(2).any(|w| w == [0x2e, 0x2e]);
    ok.then(|| String::from_utf16_lossy(units))
}

fn slug_of(skill: &JsObject) -> JsString {
    skill
        .get("slug")
        .and_then(JsValue::as_string)
        .cloned()
        .unwrap_or_default()
}

/// SkillManager.ts:629 readSkillMeta: `null` when SKILL.md is unreadable.
fn read_skill_meta(dir: &str, scope: &JsValue) -> JsValue {
    let file = join(dir, "SKILL.md");
    let Ok(bytes) = fs::read(&file) else {
        return JsValue::Null;
    };
    let fm = frontmatter::parse(&decode_utf8(&bytes));
    // Node Stats#mtimeMs: sec * 1e3 + nsec / 1e6.
    let mtime = fs::metadata(&file).map_or(JsValue::Null, |m| {
        JsValue::Number(m.mtime() as f64 * 1e3 + m.mtime_nsec() as f64 / 1e6)
    });
    let base = dir.rsplit('/').next().unwrap_or(dir);
    let version = match fm.get("version") {
        Some(Value::String(s)) => JsValue::String(s.as_ref().clone()),
        Some(Value::Number(n)) => number_string(*n).as_str().into(),
        _ => JsValue::Null,
    };
    object([
        ("slug", base.into()),
        (
            "name",
            JsValue::String(fm.string("name").cloned().unwrap_or_else(|| base.into())),
        ),
        (
            "description",
            JsValue::String(fm.string("description").cloned().unwrap_or_default()),
        ),
        ("version", version),
        ("skillFile", file.as_str().into()),
        ("skillDir", dir.into()),
        ("scope", scope.clone()),
        ("readonly", JsValue::Bool(is_str(scope, "builtin"))),
        ("mtime", mtime),
    ])
}

/// SkillManager.ts:706 listSkillsIn. readdir order is libuv's strcmp sort; the
/// result is then sorted by localeCompare (stable, like Array#sort).
fn list_skills_in(
    root: &str,
    scope: &str,
    collator: &CollatorBorrowed<'static>,
) -> Result<Vec<JsObject>> {
    let scandir = |e: io::Error| SkillError::io(&e, "scandir", Some(root));
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries.collect::<io::Result<Vec<_>>>().map_err(scandir)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(scandir(e)),
    };
    let mut entries: Vec<_> = entries.into_iter().map(|e| (e.file_name(), e)).collect();
    entries.sort_by(|a, b| a.0.as_encoded_bytes().cmp(b.0.as_encoded_bytes()));
    let scope = JsValue::from(scope);
    let mut skills = vec![];
    for (name, entry) in entries {
        let Some(name) = valid_slug(&JsValue::String(name.to_string_lossy().as_ref().into()))
        else {
            continue;
        };
        let dir = join(root, &name);
        let kind = entry.file_type().ok();
        let mut is_dir = kind.is_some_and(|k| k.is_dir());
        if !is_dir && kind.is_some_and(|k| k.is_symlink()) {
            is_dir = fs::metadata(&dir).is_ok_and(|m| m.is_dir());
        }
        if !is_dir {
            continue;
        }
        if let Some(meta) = into_object(read_skill_meta(&dir, &scope)) {
            skills.push(meta);
        }
    }
    skills.sort_by(|a, b| collator.compare_utf16(slug_of(a).units(), slug_of(b).units()));
    Ok(skills)
}

/// SkillManager.ts:528 buildInitialSkillContent.
fn initial_content(input: &JsValue, slug: &JsString) -> Result<JsString> {
    let text = |key: &str, default: JsString| -> Result<JsString> {
        match field(input, key)? {
            v if nullish(v) => Ok(default),
            JsValue::String(s) => Ok(s.clone()),
            _ => Err(SkillError::type_error(
                "",
                format!("input.{key}.replace is not a function"),
            )),
        }
    };
    // `.replace(/\n/g, " ").trim()`
    let one_line = |s: JsString| {
        let replaced: Vec<u16> = s
            .units()
            .iter()
            .map(|&u| if u == NL { u16::from(b' ') } else { u })
            .collect();
        trim_units(&replaced).to_vec()
    };
    let name = one_line(text("name", slug.clone())?);
    let description = one_line(text("description", JsString::default())?);
    let body = match field(input, "body")? {
        v if !truthy(v) => vec![],
        JsValue::String(s) => trim_units(s.units()).to_vec(),
        _ => {
            return Err(SkillError::type_error(
                "",
                "input.body.trim is not a function",
            ));
        }
    };
    let mut out: Vec<u16> = "---\nname: ".encode_utf16().collect();
    out.extend(&name);
    if !description.is_empty() {
        out.extend("\ndescription: ".encode_utf16());
        out.extend(&description);
    }
    out.extend("\n---\n\n# ".encode_utf16());
    out.extend(&name);
    out.extend("\n\n".encode_utf16());
    if body.is_empty() {
        out.extend(
            "Describe what this skill does, when to invoke it, and any prerequisites."
                .encode_utf16(),
        );
    } else {
        out.extend(body);
    }
    out.push(NL);
    Ok(JsString::from_utf16(out))
}

/// SkillManager.ts:924 decodeListCursor; every failure inside the TS `try`
/// (Buffer.from, JSON.parse, property reads) becomes `invalid_input`.
fn decode_list_cursor(value: &JsValue, signature: &str) -> Result<f64> {
    if !truthy(value) {
        return Ok(0.0);
    }
    let invalid = || invalid_input("cursor is invalid or does not match the query.");
    let bytes = match value {
        JsValue::String(s) => base64_lenient(s.units()),
        // Buffer.from(array) copies ToUint8 of each element and ignores the encoding.
        JsValue::Array(items) => items.iter().map(|v| to_uint8(js_to_number(v))).collect(),
        _ => return Err(invalid()),
    };
    let parsed = parse_json(&String::from_utf8_lossy(&bytes)).map_err(|_| invalid())?;
    let offset = parsed.get("offset").and_then(JsValue::as_number);
    let signed = parsed
        .get("signature")
        .and_then(JsValue::as_string)
        .is_some_and(|s| s.equals_str(signature));
    match offset {
        Some(n) if is_integer(n) && n >= 0.0 && signed => Ok(n),
        _ => Err(invalid()),
    }
}

/// Node's legacy base64/base64url decoder: each UTF-16 unit is truncated to a
/// byte, illegal characters are skipped and `=` ends decoding.
fn base64_lenient(units: &[u16]) -> Vec<u8> {
    let (mut out, mut acc, mut bits) = (vec![], 0u32, 0);
    for &unit in units {
        let c = unit as u8;
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            _ => continue,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    out
}

fn base64url(value: &JsValue) -> String {
    URL_SAFE_NO_PAD.encode(
        stringify_json(value)
            .expect("plain JSON value")
            .expect("defined"),
    )
}

/// fs.promises.mkdir(path, { recursive: true }) (Node MKDirpAsync): errors
/// always name the requested path; a non-directory in the way is EEXIST.
fn mkdir_p(path: &str) -> Result<()> {
    let fail = |e: &io::Error| SkillError::io(e, "mkdir", Some(path));
    let mut pending = vec![path.to_owned()];
    while let Some(next) = pending.last().cloned() {
        let Err(e) = fs::create_dir(&next) else {
            pending.pop();
            continue;
        };
        match e.raw_os_error() {
            Some(libc::EACCES | libc::ENOSPC | libc::ENOTDIR | libc::EPERM) => {
                return Err(fail(&e));
            }
            Some(libc::ENOENT) => {
                let parent = dirname(&next);
                if parent == next {
                    return Err(fail(&e));
                }
                pending.push(parent);
            }
            _ => match fs::metadata(&next) {
                Ok(m) if m.is_dir() => {
                    pending.pop();
                }
                Ok(_) => return Err(fail(&io::Error::from_raw_os_error(libc::EEXIST))),
                Err(stat) => return Err(fail(&stat)),
            },
        }
    }
    Ok(())
}

/// fs.writeFile(path, string, "utf8"): lone surrogates encode as U+FFFD.
fn write_file(path: &str, content: &JsString) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .map_err(|e| SkillError::io(&e, "open", Some(path)))?;
    file.write_all(String::from_utf16_lossy(content.units()).as_bytes())
        .map_err(|e| SkillError::io(&e, "write", None))
}

/// fs.promises.rm(path, { recursive: true, force: true }): the option check
/// lstats first, so only ENOENT is forgiven.
fn remove_tree(path: &str) -> Result<()> {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(SkillError::io(&e, "lstat", Some(path))),
    };
    let removed = if meta.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };
    match removed {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(SkillError::io(&e, "rm", Some(path))),
        _ => Ok(()),
    }
}

// ------------------------------------------------------- JS value semantics

fn field<'a>(input: &'a JsValue, key: &str) -> Result<&'a JsValue> {
    match input {
        JsValue::Undefined | JsValue::Null => Err(SkillError::type_error(
            "",
            format!(
                "Cannot read properties of {} (reading '{key}')",
                display(input)
            ),
        )),
        _ => Ok(input.get(key).unwrap_or(&UNDEFINED)),
    }
}
/// `value ?? fallback`
fn or(value: &JsValue, fallback: JsValue) -> JsValue {
    if nullish(value) {
        fallback
    } else {
        value.clone()
    }
}
/// JsValue implements Drop, so its object is taken rather than moved out.
fn into_object(mut value: JsValue) -> Option<JsObject> {
    match &mut value {
        JsValue::Object(o) => Some(std::mem::take(o)),
        _ => None,
    }
}
fn nullish(value: &JsValue) -> bool {
    matches!(value, JsValue::Undefined | JsValue::Null)
}
fn truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(b) => *b,
        JsValue::Number(n) => *n != 0.0 && !n.is_nan(),
        JsValue::String(s) => !s.is_empty(),
        _ => true,
    }
}
fn is_str(value: &JsValue, text: &str) -> bool {
    value.as_string().is_some_and(|s| s.equals_str(text))
}
/// Number.isInteger
fn is_integer(n: f64) -> bool {
    n.is_finite() && n.trunc() == n
}
/// JS Number#toString (radix 10).
fn number_string(n: f64) -> String {
    if n.is_nan() {
        "NaN".into()
    } else if n == f64::INFINITY {
        "Infinity".into()
    } else if n == f64::NEG_INFINITY {
        "-Infinity".into()
    } else if n == 0.0 {
        "0".into()
    } else {
        stringify_json(&JsValue::Number(n))
            .ok()
            .flatten()
            .unwrap_or_default()
    }
}
/// `String(value)` for wire values (lone surrogates become U+FFFD).
fn display(value: &JsValue) -> String {
    match value {
        JsValue::Undefined => "undefined".into(),
        JsValue::Null => "null".into(),
        JsValue::Bool(b) => b.to_string(),
        JsValue::Number(n) => number_string(*n),
        JsValue::String(s) => String::from_utf16_lossy(s.units()),
        JsValue::Array(items) => items
            .iter()
            .map(|v| {
                if nullish(v) {
                    String::new()
                } else {
                    display(v)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        JsValue::Object(_) => "[object Object]".into(),
    }
}
/// `Number(value)` for wire values.
fn js_to_number(value: &JsValue) -> f64 {
    match value {
        JsValue::Undefined => f64::NAN,
        JsValue::Null => 0.0,
        JsValue::Bool(b) => f64::from(u8::from(*b)),
        JsValue::Number(n) => *n,
        JsValue::String(s) => string_to_number(&String::from_utf16_lossy(s.units())),
        JsValue::Array(_) => string_to_number(&display(value)),
        JsValue::Object(_) => f64::NAN,
    }
}
/// ECMAScript StringToNumber.
fn string_to_number(text: &str) -> f64 {
    let t = crate::metadata::js_trim(text);
    if t.is_empty() {
        return 0.0;
    }
    let radix = match t.get(..2) {
        Some("0x" | "0X") => 16,
        Some("0o" | "0O") => 8,
        Some("0b" | "0B") => 2,
        _ => 0,
    };
    if radix != 0 {
        let digits = &t[2..];
        if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
            return f64::NAN;
        }
        return match u128::from_str_radix(digits, radix) {
            Ok(v) => v as f64,
            Err(_) => digits.chars().fold(0.0, |acc, c| {
                acc * f64::from(radix) + f64::from(c.to_digit(radix).unwrap())
            }),
        };
    }
    match t {
        "Infinity" | "+Infinity" => return f64::INFINITY,
        "-Infinity" => return f64::NEG_INFINITY,
        _ => {}
    }
    let body = t.strip_prefix(['+', '-']).unwrap_or(t);
    let (mantissa, exponent) = match body.find(['e', 'E']) {
        Some(i) => (&body[..i], Some(&body[i + 1..])),
        None => (body, None),
    };
    let (int, frac) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    let mantissa_ok = digits(int) && digits(frac) && !(int.is_empty() && frac.is_empty());
    let exponent_ok = exponent.is_none_or(|e| {
        let e = e.strip_prefix(['+', '-']).unwrap_or(e);
        !e.is_empty() && digits(e)
    });
    if mantissa_ok && exponent_ok {
        t.parse().unwrap_or(f64::NAN)
    } else {
        f64::NAN
    }
}
/// ToUint8 (Uint8Array element store).
fn to_uint8(n: f64) -> u8 {
    if !n.is_finite() {
        return 0;
    }
    let m = n.trunc() % 256.0;
    (if m < 0.0 { m + 256.0 } else { m }) as u8
}
/// String#trim over UTF-16 units.
fn trim_units(units: &[u16]) -> &[u16] {
    fn space(c: u16) -> bool {
        matches!(c, 0x0009..=0x000d | 0x0020 | 0x00a0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff)
    }
    let start = units.iter().position(|&c| !space(c)).unwrap_or(units.len());
    let end = units
        .iter()
        .rposition(|&c| !space(c))
        .map_or(start, |i| i + 1);
    &units[start..end]
}
/// fs.readFile(path, "utf8"): invalid bytes become U+FFFD, a BOM is kept.
fn decode_utf8(bytes: &[u8]) -> JsString {
    JsString::from(String::from_utf8_lossy(bytes).as_ref())
}

// ------------------------------------------------------ node:path (posix)

/// path.resolve(value) against the process cwd.
fn resolve(value: &str) -> String {
    let cwd = std::env::current_dir().unwrap_or_else(|_| "/".into());
    crate::metadata::resolve(&cwd, value)
        .to_string_lossy()
        .into_owned()
}
/// path.join(dir, name) for a resolved `dir` and a single, dot-free segment.
fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}
fn dirname(path: &str) -> String {
    match path.rfind('/') {
        Some(0) | None => "/".into(),
        Some(i) => path[..i].into(),
    }
}
/// getPilotExtensionPaths(projectRoot).projectSkillsDir
fn project_root(project: &str) -> String {
    join(&join(&resolve(project), ".pilotdeck"), "skills")
}
/// path.resolve rejects non-strings with ERR_INVALID_ARG_TYPE.
fn path_arg(value: &JsValue) -> Result<String> {
    match value {
        JsValue::String(s) => Ok(String::from_utf16_lossy(s.units())),
        _ => Err(SkillError::type_error(
            "ERR_INVALID_ARG_TYPE",
            format!(
                "The \"paths[0]\" argument must be of type string. Received {}",
                argument_received(value).unwrap_or_default()
            ),
        )),
    }
}
