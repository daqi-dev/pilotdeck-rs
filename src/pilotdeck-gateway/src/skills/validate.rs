//! SkillManager.ts:313–326 validate and 741–994 its helpers.
use super::{
    Result, SkillError, SkillManager, field, frontmatter, into_object, invalid_input, js_to_number,
    nullish, number_string, object, resolve, trim_units,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::fs;

const MAX_TOTAL_BYTES: f64 = 50.0 * 1024.0 * 1024.0;
const MAX_FILE_BYTES: f64 = 10.0 * 1024.0 * 1024.0;
const MAX_FILE_COUNT: f64 = 500.0;
const LARGE_FILE_BYTES: f64 = 1024.0 * 1024.0;
const RISKY_EXTS: [&str; 10] = [
    ".sh", ".bash", ".zsh", ".fish", ".exe", ".bat", ".cmd", ".dll", ".so", ".dylib",
];

#[derive(Default)]
struct Report {
    hard_fails: Vec<JsValue>,
    warnings: Vec<JsValue>,
    file_count: f64,
    total_bytes: f64,
}
impl Report {
    fn fail(&mut self, code: &'static str, message: JsString) {
        self.hard_fails.push(issue(code, message));
    }
    fn warn(&mut self, code: &'static str, message: JsString) {
        self.warnings.push(issue(code, message));
    }
    fn finish(self, frontmatter: Option<frontmatter::Frontmatter>) -> Result<JsObject> {
        let frontmatter = match frontmatter {
            None => JsValue::Null,
            // TS returns the toJS object as-is; only a cycle (which TS would
            // fail to JSON.stringify later) has no JsValue form.
            Some(fm) => fm.graph.to_js_value().map_err(|e| {
                SkillError::type_error("", String::from_utf16_lossy(e.message.units()))
            })?,
        };
        Ok(into_object(object([
            ("ok", JsValue::Bool(self.hard_fails.is_empty())),
            ("hardFails", JsValue::Array(self.hard_fails)),
            ("warnings", JsValue::Array(self.warnings)),
            (
                "stats",
                object([
                    ("fileCount", JsValue::Number(self.file_count)),
                    ("totalBytes", JsValue::Number(self.total_bytes)),
                ]),
            ),
            ("frontmatter", frontmatter),
        ]))
        .expect("object"))
    }
    fn limits(&mut self) {
        if self.file_count > MAX_FILE_COUNT {
            self.fail("too_many_files", text("Bundle has more than 500 files."));
        }
        if self.total_bytes > MAX_TOTAL_BYTES {
            self.fail(
                "total_too_large",
                text(&format!(
                    "Bundle total size exceeds 52428800 bytes ({}).",
                    number_string(self.total_bytes)
                )),
            );
        }
    }
    /// Shared size/extension checks of walkDir (871–887) and the manifest (960–972).
    fn file(&mut self, rel: &JsString, size: Option<f64>, name: &JsString) {
        if let Some(size) = size {
            if size > MAX_FILE_BYTES {
                self.fail(
                    "file_too_large",
                    cat(&[
                        &text("File exceeds 10485760 bytes: "),
                        rel,
                        &text(&format!(" ({} bytes)", number_string(size))),
                    ]),
                );
            } else if size > LARGE_FILE_BYTES {
                self.warn(
                    "file_large",
                    cat(&[
                        &text("Large file: "),
                        rel,
                        &text(&format!(" ({} MB)", to_fixed_1(size / 1024.0 / 1024.0))),
                    ]),
                );
            }
        }
        // extOf(...).toLowerCase(): only ASCII can land in RISKY_EXTS (the one
        // non-ASCII letter lowering to ASCII, KELVIN SIGN → k, matches none).
        let units = name.units();
        let ext = match units.iter().rposition(|&u| u == u16::from(b'.')) {
            Some(i) if i > 0 => &units[i..],
            _ => &[][..],
        };
        if ext.iter().all(|&u| u < 128) {
            let ext = String::from_utf16_lossy(ext).to_ascii_lowercase();
            if RISKY_EXTS.contains(&ext.as_str()) {
                self.warn(
                    "risky_extension",
                    cat(&[&text(&format!("Executable-style file ({ext}): ")), rel]),
                );
            }
        }
    }
}

impl SkillManager {
    /// SkillManager.ts:313.
    pub fn validate(&self, input: &JsValue) -> Result<JsValue> {
        // `"key" in input` throws on primitives; arrays have neither key.
        let props = match input {
            JsValue::Object(o) => Some(o),
            JsValue::Array(_) => None,
            _ => {
                return Err(SkillError::type_error(
                    "",
                    "Cannot use 'in' operator to search for 'sourcePath'",
                ));
            }
        };
        if let Some(JsValue::String(path)) = props.and_then(|o| o.get("sourcePath")) {
            let trimmed = trim_units(path.units());
            if !trimmed.is_empty() {
                let resolved = resolve(&self.expand_home(&String::from_utf16_lossy(trimmed)));
                let mut result = validate_from_disk(&resolved)?;
                result.insert("sourcePath".into(), resolved.as_str().into());
                return Ok(JsValue::Object(result));
            }
        }
        if let Some(JsValue::Array(files)) = props.and_then(|o| o.get("files")) {
            let content = match field(input, "skillMdContent")? {
                v if nullish(v) => JsValue::String(JsString::default()),
                v => v.clone(),
            };
            return Ok(JsValue::Object(validate_from_manifest(&content, files)?));
        }
        Err(invalid_input(
            "Provide either { sourcePath } or { skillMdContent, files: [...] }.",
        ))
    }

    /// SkillManager.ts:516 expandHome (os.homedir() reads $HOME).
    fn expand_home(&self, path: &str) -> String {
        let home = || {
            self.home_dir
                .clone()
                .or_else(|| std::env::var("HOME").ok())
                .unwrap_or_default()
        };
        if path == "~" {
            home()
        } else if let Some(rest) = path.strip_prefix("~/") {
            format!("{}/{rest}", home())
        } else {
            path.into()
        }
    }
}

/// SkillManager.ts:745 validateRequiredFrontmatter.
fn required_frontmatter(
    content: &JsValue,
    report: &mut Report,
) -> Option<frontmatter::Frontmatter> {
    let content = match content {
        JsValue::String(s) if !trim_units(s.units()).is_empty() => s,
        _ => {
            report.fail("no_skill_md", text("SKILL.md is empty or missing."));
            return None;
        }
    };
    let fm = frontmatter::parse(content);
    if fm.is_empty() && !content.units().starts_with(&[u16::from(b'-'); 3]) {
        report.fail(
            "frontmatter_missing",
            text("SKILL.md does not start with a YAML frontmatter block."),
        );
        return Some(fm);
    }
    if fm.used_compat {
        report.warn(
            "frontmatter_compat_fallback",
            text("Frontmatter was parsed with compatibility fallback; consider standard YAML formatting."),
        );
    }
    let filled = |key: &str| {
        fm.string(key)
            .map(|s| trim_units(s.units()))
            .filter(|s| !s.is_empty())
    };
    if filled("name").is_none() {
        report.fail(
            "frontmatter_missing_name",
            text("Frontmatter is missing required field: name."),
        );
    }
    match filled("description") {
        None => report.fail(
            "frontmatter_missing_description",
            text("Frontmatter is missing required field: description (skill won't surface in the slash menu without it)."),
        ),
        Some(desc) => {
            let length = desc.len();
            if length < 20 {
                report.warn(
                    "description_short",
                    text(&format!("Description is short ({length} chars). Consider expanding for better discovery.")),
                );
            }
            if length > 1024 {
                report.warn(
                    "description_long",
                    text(&format!("Description is very long ({length} chars). Most slash-menu surfaces truncate this.")),
                );
            }
        }
    }
    Some(fm)
}

/// SkillManager.ts:796 validateFromDisk.
fn validate_from_disk(source: &str) -> Result<JsObject> {
    let mut report = Report::default();
    match fs::metadata(source) {
        Err(_) => {
            report.fail(
                "source_missing",
                text(&format!("Source path does not exist: {source}")),
            );
            return report.finish(None);
        }
        Ok(m) if !m.is_dir() => {
            report.fail(
                "source_not_directory",
                text(&format!("Source path is not a directory: {source}")),
            );
            return report.finish(None);
        }
        Ok(_) => {}
    }
    let Ok(bytes) = fs::read(super::join(source, "SKILL.md")) else {
        report.fail(
            "no_skill_md",
            text("Source folder does not contain a SKILL.md at the root."),
        );
        return report.finish(None);
    };
    let fm = required_frontmatter(&JsValue::String(super::decode_utf8(&bytes)), &mut report);
    walk_dir(source, "", &mut report);
    report.limits();
    report.finish(fm)
}

/// SkillManager.ts:839 walkDir: readdir failures are swallowed, symlinks are
/// reported and never followed, anything else that is not a directory counts.
fn walk_dir(dir: &str, prefix: &str, report: &mut Report) {
    let Ok(entries) = fs::read_dir(dir).and_then(|r| r.collect::<std::io::Result<Vec<_>>>()) else {
        return;
    };
    let mut entries: Vec<_> = entries.into_iter().map(|e| (e.file_name(), e)).collect();
    entries.sort_by(|a, b| a.0.as_encoded_bytes().cmp(b.0.as_encoded_bytes()));
    for (name, entry) in entries {
        if report.file_count > MAX_FILE_COUNT {
            return;
        }
        let name = name.to_string_lossy();
        let rel = if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}/{name}")
        };
        let abs = super::join(dir, &name);
        let kind = entry.file_type().ok();
        if kind.is_some_and(|k| k.is_symlink()) {
            report.warn(
                "contains_symlink",
                text(&format!("Bundle contains a symlink: {rel}")),
            );
            continue;
        }
        if kind.is_some_and(|k| k.is_dir()) {
            walk_dir(&abs, &rel, report);
            continue;
        }
        report.file_count += 1.0;
        let size = fs::metadata(&abs).ok().map(|m| m.len() as f64);
        if let Some(size) = size {
            report.total_bytes += size;
        }
        report.file(&text(&rel), size, &text(&name));
    }
}

/// SkillManager.ts:940 validateFromManifest.
fn validate_from_manifest(content: &JsValue, files: &[JsValue]) -> Result<JsObject> {
    let mut report = Report::default();
    let mut has_skill_md = false;
    for file in files {
        let rel = match field(file, "relativePath")? {
            JsValue::String(s) if !s.is_empty() => s,
            _ => continue,
        };
        if rel.equals_str("SKILL.md") {
            has_skill_md = true;
        }
        let units = rel.units();
        if units.windows(2).any(|w| w == [0x2e, 0x2e]) || units[0] == u16::from(b'/') {
            report.fail("unsafe_path", cat(&[&text("File path is unsafe: "), rel]));
            continue;
        }
        // `Number(f.size) || 0`
        let size = js_to_number(field(file, "size")?);
        let size = if size.is_nan() || size == 0.0 {
            0.0
        } else {
            size
        };
        report.file_count += 1.0;
        report.total_bytes += size;
        report.file(rel, Some(size), rel);
    }
    if !has_skill_md {
        report.fail(
            "no_skill_md",
            text("No SKILL.md at the root of the picked folder."),
        );
    }
    report.limits();
    let fm = if has_skill_md {
        required_frontmatter(content, &mut report)
    } else {
        None
    };
    report.finish(fm)
}

/// Number#toFixed(1) for 1 < x ≤ 10 (the file_large range): an exact tie
/// picks the larger n, where Rust's `{:.1}` rounds half to even.
fn to_fixed_1(x: f64) -> String {
    let scaled = x * 10.0;
    let n = if scaled.fract() == 0.5 {
        scaled.ceil()
    } else {
        scaled.round()
    } as u64;
    format!("{}.{}", n / 10, n % 10)
}
fn issue(code: &'static str, message: JsString) -> JsValue {
    object([("code", code.into()), ("message", JsValue::String(message))])
}
fn text(s: &str) -> JsString {
    s.into()
}
fn cat(parts: &[&JsString]) -> JsString {
    JsString::from_utf16(
        parts
            .iter()
            .flat_map(|p| p.units().iter().copied())
            .collect(),
    )
}
