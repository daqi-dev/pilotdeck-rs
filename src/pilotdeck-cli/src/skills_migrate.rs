//! Local `skills migrate`: pilotdeck.ts:426–514 and migrateSkills.ts.
use crate::{
    Failure,
    paths::{self, Env},
    skills::{collator, fs_failure},
    stderr_line, stdout_line,
};
use pilotdeck_gateway::skills::{SkillManager, SkillManagerOptions};
use pilotdeck_protocol::{JsObject, JsValue, stringify_json};
use std::os::unix::fs::{FileTypeExt, MetadataExt, symlink};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

const USAGE: &str = "Usage: pilotdeck skills migrate [--execute] [--from cc,openclaw,hermes] [--source <dir>] [--overwrite|--rename]";
const STATUSES: [&str; 5] = ["migrated", "would_migrate", "skipped", "conflict", "error"];

fn object<const N: usize>(fields: [(&str, JsValue); N]) -> JsValue {
    let mut o = JsObject::new();
    for (key, value) in fields {
        o.insert(key.into(), value);
    }
    JsValue::Object(o)
}
fn field<'a>(v: &'a JsValue, key: &str) -> &'a JsValue {
    v.as_object()
        .and_then(|o| o.get(key))
        .expect("internal report field")
}
fn string(v: &JsValue) -> String {
    match v {
        JsValue::String(s) => String::from_utf16_lossy(s.units()),
        _ => String::new(),
    }
}
fn path_string(p: &Path) -> JsValue {
    p.to_string_lossy().as_ref().into()
}
fn flag<'a>(argv: &'a [String], key: &str) -> Option<&'a str> {
    argv.iter()
        .position(|a| a == key)
        .and_then(|i| argv.get(i + 1))
        .filter(|a| !a.is_empty() && !a.starts_with("--"))
        .map(String::as_str)
}
fn has(argv: &[String], key: &str) -> bool {
    argv.iter().any(|a| a == key)
}

#[derive(Clone)]
struct Source {
    kind: &'static str,
    label: &'static str,
    path: PathBuf,
}
impl Source {
    fn json(&self) -> JsValue {
        object([
            ("kind", self.kind.into()),
            ("label", self.label.into()),
            ("path", path_string(&self.path)),
        ])
    }
    fn item(
        &self,
        path: &Path,
        destination: Option<&Path>,
        slug: &str,
        status: &str,
        reason: &str,
    ) -> JsValue {
        object([
            ("sourceKind", self.kind.into()),
            ("sourceLabel", self.label.into()),
            ("sourcePath", path_string(path)),
            (
                "destinationPath",
                destination.map_or(JsValue::Null, path_string),
            ),
            ("slug", slug.into()),
            ("status", status.into()),
            ("reason", reason.into()),
        ])
    }
}

fn includes(raw: Option<&str>) -> Result<Vec<&'static str>, Failure> {
    let mut result = Vec::new();
    for part in raw.unwrap_or("").split(',') {
        let normalized = paths::js_trim(part).to_lowercase();
        let kinds: &[&str] = match normalized.as_str() {
            "" => &[],
            "cc" | "claude" | "claude-code" => &["claude-code"],
            "openclaw" => &["openclaw"],
            "hermes" => &["hermes"],
            "all" => &["claude-code", "openclaw", "hermes"],
            _ => {
                return Err(Failure::Error(format!(
                    "Unknown skills source \"{part}\". Use cc, openclaw, hermes, or all."
                )));
            }
        };
        for kind in kinds {
            if !result.contains(kind) {
                result.push(*kind);
            }
        }
    }
    if result.is_empty() {
        result.extend(["claude-code", "openclaw", "hermes"]);
    }
    Ok(result)
}
fn sources(argv: &[String], env: &Env, cwd: &Path) -> Result<Vec<Source>, Failure> {
    let include = includes(flag(argv, "--from"))?;
    let home = PathBuf::from(
        env.get("HOME")
            .ok_or(Failure::Todo("home directory lookup without $HOME"))?,
    );
    let project = paths::resolve(cwd, Path::new(flag(argv, "--project").unwrap_or("")));
    let mut sources = Vec::new();
    for kind in include {
        let rows = match kind {
            "claude-code" => vec![
                ("Claude Code user skills", home.join(".claude/skills")),
                ("Claude Code project skills", project.join(".claude/skills")),
            ],
            "openclaw" => vec![
                (
                    "OpenClaw workspace skills",
                    home.join(".openclaw/workspace/skills"),
                ),
                (
                    "OpenClaw workspace-main skills",
                    home.join(".openclaw/workspace-main/skills"),
                ),
                (
                    "OpenClaw workspace-assistant skills",
                    home.join(".openclaw/workspace-assistant/skills"),
                ),
                ("OpenClaw managed skills", home.join(".openclaw/skills")),
                ("OpenClaw shared skills", home.join(".agents/skills")),
                (
                    "OpenClaw project shared skills",
                    home.join(".openclaw/workspace/.agents/skills"),
                ),
                (
                    "OpenClaw default shared skills",
                    home.join(".openclaw/workspace.default/.agents/skills"),
                ),
            ],
            _ => vec![
                ("Hermes user skills", home.join(".hermes/skills")),
                (
                    "Hermes Claude-style skills",
                    home.join(".hermes/.claude/skills"),
                ),
                ("Hermes agent skills", home.join(".hermes/.agents/skills")),
            ],
        };
        for (label, path) in rows {
            sources.push(Source { kind, label, path });
        }
    }
    for pair in argv.windows(2) {
        if pair[0] == "--source" && !pair[1].is_empty() && !pair[1].starts_with("--") {
            let path = expand_home(&pair[1], &home);
            sources.push(Source {
                kind: "custom",
                label: "Custom skills",
                path,
            });
        }
    }
    let mut seen = HashSet::new();
    for source in &mut sources {
        source.path = paths::resolve(cwd, &source.path);
    }
    sources.retain(|s| seen.insert(s.path.clone()));
    Ok(sources)
}
fn expand_home(raw: &str, home: &Path) -> PathBuf {
    if raw == "~" {
        home.to_path_buf()
    } else if let Some(rest) = raw.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(raw)
    }
}
fn valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 100
        && slug.as_bytes()[0].is_ascii_alphanumeric()
        && slug
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        && !slug.contains("..")
}
fn discover(root: &Path, env: &Env) -> Result<Option<Vec<PathBuf>>, Failure> {
    if root.join("SKILL.md").exists() {
        return Ok(Some(vec![root.to_path_buf()]));
    }
    let entries = match fs::read_dir(root) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(fs_failure(e, "scandir", root)),
        Ok(entries) => entries,
    };
    let mut dirs = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| fs_failure(e, "scandir", root))?;
        let kind = entry
            .file_type()
            .map_err(|e| fs_failure(e, "lstat", &entry.path()))?;
        if (kind.is_dir() || kind.is_symlink()) && entry.path().join("SKILL.md").exists() {
            dirs.push(entry.path());
        }
    }
    dirs.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
    let collator = collator(env)?;
    dirs.sort_by(|a, b| {
        collator.compare(
            &a.file_name().unwrap().to_string_lossy(),
            &b.file_name().unwrap().to_string_lossy(),
        )
    });
    Ok(Some(dirs))
}
fn destination_slug(root: &Path, slug: &str, mode: &str) -> Option<String> {
    if mode == "overwrite" || !root.join(slug).exists() {
        return Some(slug.into());
    }
    if mode == "skip" {
        return None;
    }
    let mut candidate = format!("{slug}-imported");
    let mut suffix = 2;
    while root.join(&candidate).exists() {
        candidate = format!("{slug}-imported-{suffix}");
        suffix += 1;
    }
    Some(candidate)
}
fn validation_reason(validation: &JsValue) -> String {
    let JsValue::Array(issues) = field(validation, "hardFails") else {
        unreachable!()
    };
    format!(
        "validation_failed: {}",
        issues
            .iter()
            .map(|i| string(field(i, "message")))
            .collect::<Vec<_>>()
            .join("; ")
    )
}
fn attach_validation(item: JsValue, validation: JsValue) -> JsValue {
    let mut fields = item.as_object().unwrap().clone();
    fields.insert("validation".into(), validation);
    JsValue::Object(fields)
}
fn without_source_path(validation: &JsValue) -> JsValue {
    let mut fields = JsObject::new();
    for (key, value) in validation.as_object().unwrap().entries() {
        if !key.equals_str("sourcePath") {
            fields.insert(key.clone(), value.clone());
        }
    }
    JsValue::Object(fields)
}

/// CLI imports always use user scope and copy mode. Keep this adapter local until
/// the shared SkillManager import API lands; validation already uses that manager.
fn import_skill(
    manager: &SkillManager,
    source: &Path,
    target: &Path,
    force: bool,
    home: &Path,
    cwd: &Path,
) -> Result<(PathBuf, JsValue), Failure> {
    let source = paths::resolve(
        cwd,
        &expand_home(paths::js_trim(&source.to_string_lossy()), home),
    );
    let metadata = fs::metadata(&source).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Failure::Error(format!(
                "source_missing: Source path does not exist: {}",
                source.display()
            ))
        } else {
            fs_failure(e, "stat", &source)
        }
    })?;
    if !metadata.is_dir() {
        return Err(Failure::Error(format!(
            "source_not_directory: Source path is not a directory: {}",
            source.display()
        )));
    }
    if !source.join("SKILL.md").exists() {
        return Err(Failure::Error(format!(
            "no_skill_md: Source folder does not contain a SKILL.md at the root: {}",
            source.display()
        )));
    }
    let slug = target.file_name().unwrap().to_string_lossy();
    if !valid_slug(&slug) {
        return Err(Failure::Error(format!(
            "invalid_slug: Invalid slug \"{slug}\". Allowed: [a-zA-Z0-9][a-zA-Z0-9._-]{{0,99}}, no \"..\"."
        )));
    }
    if source == target {
        return Err(Failure::Error("self_import: Source and target resolve to the same path; pick a different slug or scope.".into()));
    }
    let exists = target.exists();
    if exists && !force {
        return Err(Failure::Error(format!(
            "conflict: Skill already exists at {}. Re-run with force=true to overwrite.",
            target.display()
        )));
    }
    let validation = manager
        .validate(&object([("sourcePath", path_string(&source))]))
        .map_err(|e| Failure::Error(e.message))?;
    if field(&validation, "ok") != &JsValue::Bool(true) {
        return Err(Failure::Error(validation_reason(&validation)));
    }
    if exists {
        let metadata = fs::symlink_metadata(target).map_err(|e| fs_failure(e, "lstat", target))?;
        if metadata.is_dir() {
            fs::remove_dir_all(target).map_err(|e| fs_failure(e, "rmdir", target))?;
        } else {
            fs::remove_file(target).map_err(|e| fs_failure(e, "unlink", target))?;
        }
    }
    fs::create_dir_all(target.parent().unwrap())
        .map_err(|e| fs_failure(e, "mkdir", target.parent().unwrap()))?;
    // Node checks ancestor identities even when a parent is reached via a symlink.
    let source_stat = fs::symlink_metadata(&source).map_err(|e| fs_failure(e, "lstat", &source))?;
    let mut parent = target.parent();
    while let Some(path) = parent {
        if Some(path) == source.parent() || path == Path::new("/") {
            break;
        }
        match fs::metadata(path) {
            Ok(stat) if stat.dev() == source_stat.dev() && stat.ino() == source_stat.ino() => {
                return Err(copy_into_self(&source, target));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
            Err(e) => return Err(fs_failure(e, "stat", path)),
        }
        parent = path.parent();
    }
    copy_tree(&source, target)?;
    Ok((source, without_source_path(&validation)))
}

fn copy_into_self(source: &Path, target: &Path) -> Failure {
    Failure::Error(format!(
        "Invalid src or dest: cp returned EINVAL (cannot copy {} to a subdirectory of self {}) {}",
        source.display(),
        target.display(),
        target.display()
    ))
}

fn copy_tree(source: &Path, target: &Path) -> Result<(), Failure> {
    let metadata = fs::symlink_metadata(source).map_err(|e| fs_failure(e, "lstat", source))?;
    let destination = match fs::symlink_metadata(target) {
        Ok(stat) => Some(stat),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(fs_failure(e, "lstat", target)),
    };
    if let Some(dest) = &destination {
        if metadata.dev() == dest.dev() && metadata.ino() == dest.ino() {
            return Err(Failure::Error(format!(
                "Invalid src or dest: cp returned EINVAL (src and dest cannot be the same) {}",
                target.display()
            )));
        }
        if metadata.is_dir() && !dest.is_dir() {
            return Err(Failure::Error(format!(
                "Cannot overwrite non-directory with directory: cp returned EISDIR (cannot overwrite non-directory {} with directory {}) {}",
                target.display(),
                source.display(),
                target.display()
            )));
        }
        if !metadata.is_dir() && dest.is_dir() {
            return Err(Failure::Error(format!(
                "Cannot overwrite directory with non-directory: cp returned ENOTDIR (cannot overwrite directory {} with non-directory {}) {}",
                target.display(),
                source.display(),
                target.display()
            )));
        }
    }
    if metadata.is_dir() && target.starts_with(source) {
        return Err(copy_into_self(source, target));
    }
    if metadata.file_type().is_symlink() {
        let link = fs::read_link(source).map_err(|e| fs_failure(e, "readlink", source))?;
        // Node fs.cp defaults verbatimSymlinks=false: relative targets become absolute.
        let normalized_link = paths::resolve(source.parent().unwrap(), &link);
        let link = if link.is_absolute() {
            link
        } else {
            normalized_link.clone()
        };
        if destination.is_some() {
            match fs::read_link(target) {
                Ok(dest_link) => {
                    let dest_link = paths::resolve(target.parent().unwrap(), &dest_link);
                    if dest_link.starts_with(&normalized_link) {
                        return Err(Failure::Error(format!(
                            "Invalid src or dest: cp returned EINVAL (cannot copy {} to a subdirectory of self {}) {}",
                            link.display(),
                            dest_link.display(),
                            target.display()
                        )));
                    }
                    let stat = fs::metadata(source).map_err(|e| fs_failure(e, "stat", source))?;
                    if stat.is_dir() && normalized_link.starts_with(&dest_link) {
                        return Err(Failure::Error(format!(
                            "Cannot overwrite symlink in subdirectory of self: cp returned EINVAL (cannot overwrite {} with {}) {}",
                            dest_link.display(),
                            link.display(),
                            target.display()
                        )));
                    }
                    fs::remove_file(target).map_err(|e| fs_failure(e, "unlink", target))?;
                }
                Err(e) if e.raw_os_error() == Some(libc::EINVAL) => {}
                Err(e) => return Err(fs_failure(e, "readlink", target)),
            }
        }
        symlink(&link, target).map_err(|e| match fs_failure(e, "symlink", &link) {
            Failure::Error(message) => {
                Failure::Error(format!("{message} -> '{}'", target.display()))
            }
            other => other,
        })?;
    } else if metadata.is_dir() {
        if destination.is_none() {
            fs::create_dir(target).map_err(|e| fs_failure(e, "mkdir", target))?;
        }
        for entry in fs::read_dir(source).map_err(|e| fs_failure(e, "opendir", source))? {
            let entry = entry.map_err(|e| fs_failure(e, "readdir", source))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            copy_tree(&source.join(&name), &target.join(&name))?;
        }
        if destination.is_none() {
            // Node 22 fs.promises.cp (mkDirAndCopy) chmods after copying;
            // unlike cpSync's fast path, the final mode is not masked by umask.
            fs::set_permissions(target, metadata.permissions())
                .map_err(|e| fs_failure(e, "chmod", target))?;
        }
    } else if metadata.is_file()
        || metadata.file_type().is_char_device()
        || metadata.file_type().is_block_device()
    {
        if destination.is_some() {
            fs::remove_file(target).map_err(|e| fs_failure(e, "unlink", target))?;
        }
        fs::copy(source, target).map_err(|e| match fs_failure(e, "copyfile", source) {
            Failure::Error(message) => {
                Failure::Error(format!("{message} -> '{}'", target.display()))
            }
            other => other,
        })?;
    } else {
        let kind = if metadata.file_type().is_fifo() {
            "a FIFO pipe"
        } else if metadata.file_type().is_socket() {
            "a socket file"
        } else {
            "an unknown file type"
        };
        return Err(Failure::Error(format!(
            "Cannot copy {kind}: cp returned EINVAL (cannot copy {kind}: {}) {}",
            target.display(),
            target.display()
        )));
    }
    Ok(())
}

fn report(argv: &[String], env: &Env, cwd: &Path) -> Result<JsValue, Failure> {
    // Resolve --from before resolving any paths, as the CLI does.
    includes(flag(argv, "--from"))?;
    let pilot_home = match flag(argv, "--pilot-home") {
        Some(raw) => paths::resolve(cwd, Path::new(raw)),
        None => paths::pilot_home(env, cwd)?,
    };
    let root = pilot_home.join("skills");
    let execute = has(argv, "--execute");
    let mode = if has(argv, "--overwrite") {
        "overwrite"
    } else if has(argv, "--rename") {
        "rename"
    } else {
        "skip"
    };
    let sources = sources(argv, env, cwd)?;
    let home = PathBuf::from(env.get("HOME").expect("checked by sources"));
    let mut options = SkillManagerOptions::new(pilot_home.to_string_lossy());
    options.home_dir = Some(home.to_string_lossy().into_owned());
    options.env = Some(env.strings());
    let manager = SkillManager::new(options).map_err(|e| Failure::Error(e.message))?;
    let mut items = Vec::new();
    for source in &sources {
        let Some(dirs) = discover(&source.path, env)? else {
            items.push(source.item(
                &source.path,
                None,
                "",
                "skipped",
                "Source directory does not exist.",
            ));
            continue;
        };
        if dirs.is_empty() {
            items.push(source.item(
                &source.path,
                None,
                "",
                "skipped",
                "No immediate child skill directories with SKILL.md found.",
            ));
        }
        for path in dirs {
            let original_slug = path
                .file_name()
                .map(|s| s.to_string_lossy())
                .unwrap_or_default();
            if !valid_slug(&original_slug) {
                items.push(source.item(
                    &path,
                    None,
                    &original_slug,
                    "error",
                    &format!("Invalid slug \"{original_slug}\"."),
                ));
                continue;
            }
            let Some(slug) = destination_slug(&root, &original_slug, mode) else {
                items.push(source.item(
                    &path,
                    Some(&root.join(original_slug.as_ref())),
                    &original_slug,
                    "conflict",
                    "Destination skill already exists.",
                ));
                continue;
            };
            let destination = root.join(&slug);
            if execute {
                match import_skill(
                    &manager,
                    &path,
                    &destination,
                    mode == "overwrite",
                    &home,
                    cwd,
                ) {
                    Ok((resolved, validation)) => items.push(attach_validation(
                        source.item(
                            &resolved,
                            Some(&destination),
                            &slug,
                            "migrated",
                            "Copied skill.",
                        ),
                        validation,
                    )),
                    Err(Failure::Error(reason)) => {
                        items.push(source.item(&path, Some(&destination), &slug, "error", &reason))
                    }
                    Err(other) => return Err(other),
                }
            } else {
                match manager.validate(&object([("sourcePath", path_string(&path))])) {
                    Ok(validation) => {
                        let (status, reason) = if field(&validation, "ok") != &JsValue::Bool(true) {
                            ("error", validation_reason(&validation))
                        } else {
                            (
                                "would_migrate",
                                if slug == original_slug {
                                    "Would copy skill.".into()
                                } else {
                                    format!("Would copy skill as {slug}.")
                                },
                            )
                        };
                        items.push(attach_validation(
                            source.item(&path, Some(&destination), &slug, status, &reason),
                            validation,
                        ));
                    }
                    Err(e) => items.push(source.item(
                        &path,
                        Some(&destination),
                        &slug,
                        "error",
                        &e.message,
                    )),
                }
            }
        }
    }
    let mut summary = JsObject::new();
    for status in STATUSES {
        let n = items
            .iter()
            .filter(|i| field(i, "status") == &JsValue::from(status))
            .count();
        summary.insert(status.into(), JsValue::Number(n as f64));
    }
    Ok(object([
        ("mode", if execute { "execute" } else { "dry-run" }.into()),
        ("targetRoot", path_string(&root)),
        (
            "sources",
            JsValue::Array(sources.iter().map(Source::json).collect()),
        ),
        ("summary", JsValue::Object(summary)),
        ("items", JsValue::Array(items)),
    ]))
}

/// Reindent the JS codec's bytes, preserving JS property order, numbers and escaping.
fn pretty(value: &JsValue) -> Result<String, Failure> {
    let raw = stringify_json(value)
        .map_err(|e| Failure::Error(e.message))?
        .expect("report is an object");
    let mut out = String::new();
    let mut depth = 0;
    let mut quoted = false;
    let mut escaped = false;
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                quoted = false;
            }
            continue;
        }
        match c {
            '"' => {
                quoted = true;
                out.push(c);
            }
            '{' | '[' => {
                out.push(c);
                if chars.peek() == Some(&if c == '{' { '}' } else { ']' }) {
                    out.push(chars.next().unwrap());
                } else {
                    depth += 1;
                    out.push('\n');
                    out.push_str(&"  ".repeat(depth));
                }
            }
            '}' | ']' => {
                depth -= 1;
                out.push('\n');
                out.push_str(&"  ".repeat(depth));
                out.push(c);
            }
            ',' => {
                out.push_str(",\n");
                out.push_str(&"  ".repeat(depth));
            }
            ':' => out.push_str(": "),
            _ => out.push(c),
        }
    }
    Ok(out)
}
fn text_report(report: &JsValue) -> String {
    let dry = field(report, "mode") == &JsValue::from("dry-run");
    let summary = field(report, "summary");
    let count = |key| stringify_json(field(summary, key)).unwrap().unwrap();
    let mut out = format!(
        "PilotDeck skills migration ({})\nTarget: {}\nSummary: migrated={} would_migrate={} conflict={} skipped={} error={}\n",
        if dry { "DRY RUN" } else { "EXECUTED" },
        string(field(report, "targetRoot")),
        count("migrated"),
        count("would_migrate"),
        count("conflict"),
        count("skipped"),
        count("error")
    );
    let JsValue::Array(items) = field(report, "items") else {
        unreachable!()
    };
    let mut first = true;
    for item in items {
        let symbol = match string(field(item, "status")).as_str() {
            "skipped" => continue,
            "migrated" => '+',
            "would_migrate" => '?',
            "conflict" => '!',
            _ => 'x',
        };
        if first {
            out.push('\n');
            first = false;
        }
        let slug = string(field(item, "slug"));
        out.push_str(&format!(
            "{symbol} {}: {}\n  {}\n",
            string(field(item, "sourceLabel")),
            if slug.is_empty() { "(n/a)" } else { &slug },
            string(field(item, "sourcePath"))
        ));
        let destination = string(field(item, "destinationPath"));
        if !destination.is_empty() {
            out.push_str(&format!("  -> {destination}\n"));
        }
        let reason = string(field(item, "reason"));
        if !reason.is_empty() {
            out.push_str(&format!("  {reason}\n"));
        }
    }
    if dry {
        out.push_str("\nThis was a dry run. Add --execute to copy skills.\n");
    }
    out
}
pub fn run(argv: &[String], env: &Env, cwd: &Path) -> Result<u8, Failure> {
    if argv.first().map(String::as_str) != Some("migrate") {
        stderr_line(USAGE);
        return Ok(1);
    }
    let report = report(argv, env, cwd)?;
    if has(argv, "--json") {
        stdout_line(&pretty(&report)?);
        return Ok(0);
    }
    stdout_line(text_report(&report).strip_suffix('\n').unwrap());
    Ok(u8::from(
        field(field(&report, "summary"), "error") != &JsValue::Number(0.0),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    const VALID: &str =
        "---\nname: sample\ndescription: A sample skill for testing local migration.\n---\nBody.\n";
    struct Fixture {
        path: PathBuf,
        env: Env,
    }
    impl Fixture {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("pdrs-migrate-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(path.join("project")).unwrap();
            let env = Env::from_pairs(&[
                ("HOME", &path.join("home").to_string_lossy()),
                ("PILOT_HOME", &path.join("pilot").to_string_lossy()),
                ("LC_ALL", "C"),
            ]);
            Self { path, env }
        }
        fn skill(&self, name: &str, content: &str) {
            let dir = self.path.join(name);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("SKILL.md"), content).unwrap();
        }
        fn report(&self, args: &[&str]) -> JsValue {
            report(
                &args.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                &self.env,
                &self.path.join("project"),
            )
            .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
    fn custom_items(report: &JsValue) -> Vec<&JsValue> {
        let JsValue::Array(items) = field(report, "items") else {
            panic!()
        };
        items
            .iter()
            .filter(|i| field(i, "sourceKind") == &JsValue::from("custom"))
            .collect()
    }
    fn count(report: &JsValue, key: &str) -> f64 {
        let JsValue::Number(n) = field(field(report, "summary"), key) else {
            panic!()
        };
        *n
    }

    #[test]
    fn migration_reports_match_coordinator_recorded_ts_bytes() {
        let scenarios: Vec<serde_json::Value> =
            serde_json::from_str(include_str!("../../../scripts/cli-parity/scenarios.json"))
                .unwrap();
        let mut checked = 0;
        for scenario in scenarios {
            if scenario["argv"][0] != "skills" || scenario["argv"][1] != "migrate" {
                continue;
            }
            checked += 1;
            let id = scenario["id"].as_str().unwrap();
            let f = Fixture::new(id);
            if let Some(files) = scenario["fixtureFiles"].as_object() {
                for (name, data) in files {
                    let path = f.path.join(name);
                    fs::create_dir_all(path.parent().unwrap()).unwrap();
                    fs::write(path, data.as_str().unwrap()).unwrap();
                }
            }
            let args: Vec<_> = scenario["argv"]
                .as_array()
                .unwrap()
                .iter()
                .skip(1)
                .map(|v| v.as_str().unwrap().to_string())
                .collect();
            match report(&args, &f.env, &f.path.join("project")) {
                Ok(report) => {
                    let actual = if has(&args, "--json") {
                        format!("{}\n", pretty(&report).unwrap())
                    } else {
                        text_report(&report)
                    };
                    let runtime = f.path.to_string_lossy();
                    let runtime = if scenario["stdoutRuntime"] == "json" {
                        let quoted = serde_json::to_string(runtime.as_ref()).unwrap();
                        quoted[1..quoted.len() - 1].to_string()
                    } else {
                        runtime.into_owned()
                    };
                    let expected = scenario["stdout"]
                        .as_str()
                        .unwrap()
                        .replace("{runtime}", &runtime);
                    assert_eq!(actual, expected, "{id}");
                }
                Err(Failure::Error(message)) => {
                    assert_eq!(scenario["stdout"], "", "{id}");
                    assert_eq!(format!("{message}\n"), scenario["stderr"], "{id}");
                }
                Err(other) => panic!("{id}: {other:?}"),
            }
        }
        assert_eq!(checked, 12);
    }

    #[test]
    fn source_aliases_flags_and_deduplication_match_cli() {
        assert_eq!(
            includes(Some(" Claude, cc, all,hermes ")).unwrap(),
            ["claude-code", "openclaw", "hermes"]
        );
        assert_eq!(includes(Some(" , ")).unwrap().len(), 3);
        assert!(
            matches!(includes(Some("cc, bad ")), Err(Failure::Error(m)) if m == "Unknown skills source \" bad \". Use cc, openclaw, hermes, or all.")
        );
        let f = Fixture::new("sources");
        let r = f.report(&[
            "--from",
            "cc",
            "--source",
            "../absent",
            "--source",
            "../project/../absent",
            "--source",
            "--json",
        ]);
        let JsValue::Array(sources) = field(&r, "sources") else {
            panic!()
        };
        assert_eq!(sources.len(), 3);
        assert_eq!(count(&r, "skipped"), 3.0);
        let r = f.report(&["--from=hermes", "--pilot-home", "~/custom"]);
        assert_eq!(
            field(&r, "targetRoot"),
            &path_string(&f.path.join("project/~/custom/skills"))
        );
    }
    #[test]
    fn explicit_pilot_home_keeps_tilde_while_custom_source_expands_it() {
        let f = Fixture::new("tilde");
        f.skill("home/imports/sample", VALID);
        let r = f.report(&[
            "--from",
            "hermes",
            "--pilot-home",
            "~/custom",
            "--source",
            "~/imports",
        ]);
        assert_eq!(
            field(&r, "targetRoot"),
            &path_string(&f.path.join("project/~/custom/skills"))
        );
        let items = custom_items(&r);
        assert_eq!(items.len(), 1);
        assert_eq!(
            field(items[0], "sourcePath"),
            &path_string(&f.path.join("home/imports/sample"))
        );
        assert_eq!(
            field(items[0], "destinationPath"),
            &path_string(&f.path.join("project/~/custom/skills/sample"))
        );
        assert_eq!(count(&r, "would_migrate"), 1.0);
        assert!(!f.path.join("project/~").exists());
        assert!(!f.path.join("home/custom").exists());
    }
    #[test]
    fn copy_tree_preserves_modes_like_node_async_cp() {
        // umask is process-wide: set it only in an exec child running this test.
        if std::env::var_os("PDRS_TEST_COPY_UMASK").is_none() {
            let mut child = std::process::Command::new("/bin/sh");
            child
                .args(["-c", "umask 022; exec \"$@\"", "pdrs-copy-umask-test"])
                .arg(std::env::current_exe().expect("#273 umask test binary path"))
                .args([
                    "--exact",
                    "skills_migrate::tests::copy_tree_preserves_modes_like_node_async_cp",
                    "--nocapture",
                ])
                .env("PDRS_TEST_COPY_UMASK", "1");
            let output = child.output().expect("#273 umask child spawn");
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            // #273: a renamed test makes this --exact filter match nothing, and
            // libtest still exits 0 — only the summary line proves the rerun
            // executed this test rather than a vacuous zero-match pass.
            assert!(
                output.status.success()
                    && stdout.contains("1 passed")
                    && stdout.contains("0 failed"),
                "#273 umask rerun matched no test: {status}\n{stdout}\n{stderr}",
                status = output.status
            );
            return;
        }
        let f = Fixture::new("copy-modes");
        f.skill("source", VALID);
        fs::create_dir(f.path.join("source/nested")).unwrap();
        fs::create_dir(f.path.join("source/private")).unwrap();
        for (path, mode) in [
            ("source", 0o777),
            ("source/nested", 0o777),
            ("source/private", 0o700),
            ("source/SKILL.md", 0o777),
        ] {
            fs::set_permissions(f.path.join(path), fs::Permissions::from_mode(mode)).unwrap();
        }
        copy_tree(&f.path.join("source"), &f.path.join("target")).unwrap();
        for (path, mode) in [
            ("target", 0o777),
            ("target/nested", 0o777),
            ("target/private", 0o700),
            ("target/SKILL.md", 0o777),
        ] {
            assert_eq!(
                fs::metadata(f.path.join(path)).unwrap().mode() & 0o777,
                mode,
                "{path}"
            );
        }
        fs::create_dir(f.path.join("existing")).unwrap();
        fs::set_permissions(f.path.join("existing"), fs::Permissions::from_mode(0o711)).unwrap();
        copy_tree(&f.path.join("source"), &f.path.join("existing")).unwrap();
        assert_eq!(
            fs::metadata(f.path.join("existing")).unwrap().mode() & 0o777,
            0o711
        );
    }
    #[test]
    fn dry_run_validates_without_writes_and_execute_copies_full_tree() {
        let f = Fixture::new("copy");
        f.skill("imports/sample", VALID);
        fs::create_dir_all(f.path.join("imports/sample/empty")).unwrap();
        fs::write(f.path.join("imports/sample/data.bin"), [0, 255, 1]).unwrap();
        fs::set_permissions(
            f.path.join("imports/sample/data.bin"),
            fs::Permissions::from_mode(0o640),
        )
        .unwrap();
        let r = f.report(&["--source", "../imports"]);
        assert_eq!(count(&r, "would_migrate"), 1.0);
        assert!(!f.path.join("pilot").exists());
        let item = custom_items(&r)[0];
        assert!(
            field(item, "validation")
                .as_object()
                .unwrap()
                .contains_key("sourcePath")
        );
        let r = f.report(&["--source", "../imports", "--execute"]);
        assert_eq!(count(&r, "migrated"), 1.0);
        assert!(f.path.join("pilot/skills/sample/empty").is_dir());
        assert_eq!(
            fs::read(f.path.join("pilot/skills/sample/data.bin")).unwrap(),
            [0, 255, 1]
        );
        assert_eq!(
            fs::metadata(f.path.join("pilot/skills/sample/data.bin"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o640
        );
        assert!(
            !field(custom_items(&r)[0], "validation")
                .as_object()
                .unwrap()
                .contains_key("sourcePath")
        );
        assert!(f.path.join("imports/sample/SKILL.md").exists());
    }
    #[test]
    fn skip_rename_overwrite_and_self_import_obey_precedence() {
        let f = Fixture::new("conflicts");
        f.skill("imports/sample", VALID);
        f.skill("pilot/skills/sample", "invalid original");
        f.skill("pilot/skills/sample-imported", VALID);
        let r = f.report(&["--source", "../imports"]);
        assert_eq!(count(&r, "conflict"), 1.0);
        let r = f.report(&["--source", "../imports", "--rename", "--execute"]);
        assert_eq!(
            string(field(custom_items(&r)[0], "slug")),
            "sample-imported-2"
        );
        assert_eq!(count(&r, "migrated"), 1.0);
        fs::write(f.path.join("pilot/skills/sample/old"), "remove").unwrap();
        let r = f.report(&[
            "--source",
            "../imports",
            "--rename",
            "--overwrite",
            "--execute",
        ]);
        assert_eq!(string(field(custom_items(&r)[0], "slug")), "sample");
        assert_eq!(count(&r, "migrated"), 1.0);
        assert!(!f.path.join("pilot/skills/sample/old").exists());
        let r = f.report(&[
            "--source",
            "../pilot/skills/sample",
            "--overwrite",
            "--execute",
        ]);
        assert!(string(field(custom_items(&r)[0], "reason")).starts_with("self_import: "));
        assert!(f.path.join("pilot/skills/sample/SKILL.md").exists());
    }
    #[test]
    fn validation_errors_keep_destination_and_only_dry_run_attaches_validation() {
        let f = Fixture::new("invalid");
        f.skill("imports/bad", "no frontmatter");
        f.skill("pilot/skills/bad", VALID);
        let r = f.report(&["--source", "../imports", "--overwrite"]);
        assert_eq!(count(&r, "error"), 1.0);
        assert!(
            custom_items(&r)[0]
                .as_object()
                .unwrap()
                .contains_key("validation")
        );
        let r = f.report(&["--source", "../imports", "--overwrite", "--execute"]);
        let item = custom_items(&r)[0];
        assert!(string(field(item, "reason")).starts_with("validation_failed: "));
        assert!(!item.as_object().unwrap().contains_key("validation"));
        assert_eq!(
            fs::read_to_string(f.path.join("pilot/skills/bad/SKILL.md")).unwrap(),
            VALID
        );
    }
    #[test]
    fn invalid_slugs_rename_overflow_and_direct_source_discovery() {
        let f = Fixture::new("slug");
        f.skill("imports/bad..name", VALID);
        let r = f.report(&["--source", "../imports"]);
        assert_eq!(
            field(custom_items(&r)[0], "destinationPath"),
            &JsValue::Null
        );
        let slug = "a".repeat(100);
        f.skill(&format!("imports/{slug}"), VALID);
        f.skill(&format!("pilot/skills/{slug}"), VALID);
        let source = format!("../imports/{slug}");
        let r = f.report(&["--source", &source, "--rename"]);
        assert_eq!(count(&r, "would_migrate"), 1.0);
        let r = f.report(&["--source", &source, "--rename", "--execute"]);
        assert_eq!(count(&r, "error"), 1.0);
        assert!(string(field(custom_items(&r)[0], "reason")).starts_with("invalid_slug: "));
    }
    #[test]
    fn dry_run_does_not_reserve_names_across_sources() {
        let f = Fixture::new("multi");
        for prefix in ["first", "second"] {
            f.skill(&format!("{prefix}/sample"), VALID);
        }
        let args = ["--source", "../first", "--source", "../second", "--rename"];
        let r = f.report(&args);
        assert_eq!(count(&r, "would_migrate"), 2.0);
        assert!(
            custom_items(&r)
                .iter()
                .all(|i| string(field(i, "slug")) == "sample")
        );
        let mut args = args.to_vec();
        args.push("--execute");
        let r = f.report(&args);
        assert_eq!(count(&r, "migrated"), 2.0);
        assert_eq!(
            string(field(custom_items(&r)[1], "slug")),
            "sample-imported"
        );
    }
    #[test]
    fn symlink_children_are_copied_as_absolute_links_and_ancestor_copy_is_rejected() {
        let f = Fixture::new("links");
        f.skill("imports/sample", VALID);
        fs::write(f.path.join("imports/sample/data"), "data").unwrap();
        symlink("data", f.path.join("imports/sample/link")).unwrap();
        let absolute_link = f.path.join("imports/sample/../sample/data");
        symlink(&absolute_link, f.path.join("imports/sample/absolute")).unwrap();
        let r = f.report(&["--source", "../imports", "--execute"]);
        assert_eq!(count(&r, "migrated"), 1.0);
        assert_eq!(
            fs::read_link(f.path.join("pilot/skills/sample/absolute")).unwrap(),
            absolute_link
        );
        assert_eq!(
            fs::read_link(f.path.join("pilot/skills/sample/link")).unwrap(),
            f.path.join("imports/sample/data")
        );
        let r = f.report(&[
            "--source",
            "../imports/sample",
            "--pilot-home",
            "../imports/sample/nested",
            "--execute",
        ]);
        assert_eq!(count(&r, "error"), 1.0);
        assert!(string(field(custom_items(&r)[0], "reason")).contains("to a subdirectory of self"));
    }
    #[test]
    fn project_override_changes_source_not_destination_and_reports_preserve_order() {
        let f = Fixture::new("project");
        f.skill("other/.claude/skills/sample", VALID);
        let r = f.report(&["--from", "cc", "--project", "../other"]);
        assert_eq!(count(&r, "would_migrate"), 1.0);
        assert_eq!(
            field(&r, "targetRoot"),
            &path_string(&f.path.join("pilot/skills"))
        );
        let rendered = pretty(&r).unwrap();
        assert!(rendered.starts_with("{\n  \"mode\": \"dry-run\",\n  \"targetRoot\": "));
        assert_eq!(pilotdeck_protocol::parse_json(&rendered).unwrap(), r);
        let text = text_report(&r);
        assert!(text.contains("Summary: migrated=0 would_migrate=1 conflict=0 skipped=1 error=0\n\n? Claude Code project skills: sample\n"));
        assert!(text.ends_with("\nThis was a dry run. Add --execute to copy skills.\n"));
    }
    #[test]
    fn dangling_destination_link_is_not_treated_as_an_absent_dirent() {
        let f = Fixture::new("dangling-destination");
        f.skill("imports/sample", VALID);
        fs::create_dir_all(f.path.join("pilot/skills")).unwrap();
        symlink("missing", f.path.join("pilot/skills/sample")).unwrap();
        let r = f.report(&["--source", "../imports/sample", "--execute"]);
        assert_eq!(count(&r, "error"), 1.0);
        assert!(
            string(field(custom_items(&r)[0], "reason"))
                .starts_with("Cannot overwrite non-directory with directory: cp returned EISDIR")
        );
        assert_eq!(
            fs::read_link(f.path.join("pilot/skills/sample")).unwrap(),
            PathBuf::from("missing")
        );
        // A symlink source is copied as a link, and an unrelated dangling target
        // link can be replaced even though access(target) was false.
        symlink("imports/sample", f.path.join("alias")).unwrap();
        symlink("missing", f.path.join("pilot/skills/alias")).unwrap();
        let r = f.report(&["--source", "../alias", "--execute"]);
        assert_eq!(count(&r, "migrated"), 1.0);
        assert_eq!(
            fs::read_link(f.path.join("pilot/skills/alias")).unwrap(),
            f.path.join("imports/sample")
        );
    }

    #[test]
    fn source_enotdir_escapes_report_instead_of_becoming_skipped() {
        let f = Fixture::new("enotdir");
        let file = f.path.join("file");
        fs::write(&file, "x").unwrap();
        let result = discover(&file, &f.env);
        assert!(
            matches!(result, Err(Failure::Error(m)) if m == format!("ENOTDIR: not a directory, scandir '{}'", file.display()))
        );
    }
}
