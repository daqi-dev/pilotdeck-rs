//! migrateLegacyBundledSkillCopies (src/extension/skills/migrateLegacyBundledSkills.ts),
//! One-time cleanup of unmodified bootstrap copies, without starting a gateway.
use crate::Failure;
use crate::paths::{self, Env};
use icu_collator::{Collator, CollatorBorrowed, CollatorPreferences};
use icu_locale_core::Locale;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

const MARKER: &str = ".legacy-bundled-skills-migrated-v1";

/// resolveBuiltinSkillsRoot (src/cli/LocalGatewayBootConfig.ts:70-85). A non-empty
/// PILOTDECK_BUNDLED_SKILLS_DIR wins; otherwise the first existing candidate, falling
/// back to `<cwd>/skills`. TS candidates are relative to dist/src/cli; the Rust release
/// layout puts the binary in `<root>/bin`, so they are relative to the executable's
/// directory (current_exe after realpath, as Node resolves import.meta.url).
pub fn builtin_skills_root(
    env: &Env,
    cwd: &Path,
    exe_dir: Option<&Path>,
) -> Result<PathBuf, Failure> {
    if let Some(dir) = env
        .get("PILOTDECK_BUNDLED_SKILLS_DIR")
        .filter(|d| !d.is_empty())
    {
        return Ok(paths::resolve(cwd, Path::new(dir)));
    }
    let exe_dir = exe_dir.ok_or(Failure::Todo(
        "locating bundled skills without a known executable path",
    ))?;
    let fallback = paths::resolve(cwd, Path::new("skills"));
    let candidates = [
        paths::resolve(cwd, &exe_dir.join("../skills")),
        paths::resolve(cwd, &exe_dir.join("../../skills")),
    ];
    Ok(candidates
        .into_iter()
        .find(|candidate| candidate.exists())
        .unwrap_or(fallback))
}

fn has_bundled_skills(root: &Path) -> bool {
    fs::read_dir(root).is_ok_and(|entries| {
        entries.flatten().any(|entry| {
            entry.file_type().is_ok_and(|kind| kind.is_dir())
                && entry.path().join("SKILL.md").exists()
        })
    })
}

fn warning(slug: &str, message: &str) -> String {
    format!("[pilotdeck] Could not migrate legacy skill '{slug}': {message}")
}

#[derive(Default, Debug)]
pub struct LegacyMigrationReport {
    pub notice: Option<String>,
    pub warnings: Vec<String>,
}

/// Output of createLocalGateway.ts:672–683, kept on its original stdout/stderr.
pub fn migrate_legacy_bundled_skill_copies(
    pilot_home: &Path,
    builtin_root: &Path,
) -> Result<LegacyMigrationReport, Failure> {
    let mut report = LegacyMigrationReport::default();
    let marker = pilot_home.join(MARKER);
    if marker.exists() {
        return Ok(report);
    }
    if !has_bundled_skills(builtin_root) {
        report.warnings.push(warning("__migration__",
            "Bundled skills root is missing or contains no skills; legacy copies were left untouched."));
        return Ok(report);
    }
    let user_root = pilot_home.join("skills");
    let backup_root = pilot_home.join("skill-backups/legacy-bundled-v1");
    let mut moved = 0;
    if user_root.exists() {
        // This readdir is outside the per-skill catch in TS.
        for entry in read_entries(&user_root)? {
            if !entry
                .file_type()
                .map_err(|e| fs_failure(e, "lstat", &entry.path()))?
                .is_dir()
                || !entry.path().join("SKILL.md").exists()
            {
                continue;
            }
            let slug = entry.file_name().to_string_lossy().into_owned();
            let result = (|| {
                let source_hash = hash_directory_tree(&entry.path())?;
                let current = builtin_root.join(&slug);
                let matches_current = current.join("SKILL.md").exists()
                    && source_hash == hash_directory_tree(&current)?;
                if !matches_current && !known_legacy(&slug, &source_hash) {
                    return Ok(false);
                }
                fs::create_dir_all(&backup_root)
                    .map_err(|e| fs_failure(e, "mkdir", &backup_root))?;
                let backup = available_backup_path(&backup_root, &slug, &source_hash);
                fs::rename(entry.path(), &backup)
                    .map_err(|e| rename_failure(e, &entry.path(), &backup))?;
                Ok(true)
            })();
            match result {
                Ok(true) => moved += 1,
                Ok(false) => {}
                Err(Failure::Error(message)) => report.warnings.push(warning(&slug, &message)),
                Err(other) => return Err(other),
            }
        }
    }
    if moved > 0 {
        report.notice = Some(format!(
            "[pilotdeck] Activated bundled skills directly; moved {moved} unchanged legacy {} to {}.",
            if moved == 1 { "copy" } else { "copies" },
            backup_root.display()
        ));
    }
    if report.warnings.is_empty() {
        // The marker is `writeFileSync(completionMarker, "completed\n", "utf8")`
        // (migrateLegacyBundledSkills.ts:126): an open error names the marker,
        // a descriptor write error is `write` without it.
        let written = fs::create_dir_all(pilot_home)
            .map_err(|error| (error, "mkdir", pilot_home.to_path_buf()))
            .and_then(|()| {
                fs::File::create(&marker)
                    .map_err(|error| (error, "open", marker.clone()))
                    .and_then(|mut file| {
                        std::io::Write::write_all(&mut file, b"completed\n")
                            .map_err(|error| (error, "write", marker.clone()))
                    })
            });
        if let Err((error, syscall, path)) = written {
            match fs_failure(error, syscall, &path) {
                Failure::Error(message) => report.warnings.push(warning("__migration__", &message)),
                other => return Err(other),
            }
        }
    }
    Ok(report)
}

// Fixed historical fingerprints from migrateLegacyBundledSkills.ts:20–25.
const KNOWN_LEGACY: [(&str, &str); 4] = [
    (
        "browser-use",
        "e955f4f0813e4735c8130c2ccab5725a35a9f5dcb9098159c35d5ecf895ab2f8",
    ),
    (
        "docx",
        "6ea15a5334a23bc920b1722b2e43974fc0a253292e745f28b5f69f4c7203efb7",
    ),
    (
        "frontend-slides",
        "c9f3463532727bbeeb2bb641ca1da5849b88acf493b21bbedc8bbbbcf88b44d5",
    ),
    (
        "minimax-pdf",
        "3ef2748b6ae31af92f6aa76c6b00a547658a6ea01f8a8467f0e09475a306a7de",
    ),
];
fn known_legacy(slug: &str, hash: &str) -> bool {
    KNOWN_LEGACY.contains(&(slug, hash))
}

pub(super) fn fs_failure(error: std::io::Error, syscall: &str, path: &Path) -> Failure {
    Failure::Error(paths::node_fs_message_or_unknown(&error, syscall, path))
}
fn rename_failure(error: std::io::Error, from: &Path, to: &Path) -> Failure {
    match fs_failure(error, "rename", from) {
        Failure::Error(message) => Failure::Error(format!("{message} -> '{}'", to.display())),
        other => other,
    }
}

/// Node readdir uses byte order before the stable localeCompare sort.
pub(super) fn read_entries(path: &Path) -> Result<Vec<fs::DirEntry>, Failure> {
    let mut entries = fs::read_dir(path)
        .and_then(|e| e.collect::<std::io::Result<Vec<_>>>())
        .map_err(|e| fs_failure(e, "scandir", path))?;
    entries.sort_by_key(|entry| entry.file_name());
    Ok(entries)
}

pub(super) fn collator(env: &Env) -> Result<CollatorBorrowed<'static>, Failure> {
    // Same Node/ICU locale selection already measured for gateway project_files.
    // D2's independent C-locale probe is scripts/cli-parity/skills_probe.mjs.
    let locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|key| env.string(key))
        .unwrap_or_else(|| "en-US".into());
    let language = locale
        .split(['.', '@'])
        .next()
        .unwrap_or("")
        .replace('_', "-");
    let locale: Locale = match language.as_str() {
        "C" | "POSIX" => "en-US".parse().expect("locale literal"),
        "" => Locale::UNKNOWN,
        _ => language
            .parse()
            .unwrap_or_else(|_| "en-US".parse().expect("locale literal")),
    };
    let mut preferences: CollatorPreferences = locale.into();
    Collator::try_new(preferences, Default::default())
        .or_else(|_| {
            preferences.collation_type = None;
            Collator::try_new(preferences, Default::default())
        })
        .or_else(|_| Collator::try_new(Default::default(), Default::default()))
        .map_err(|_| Failure::Todo("loading skill filename collation data"))
}

/// Hash only files and symlinks; empty dirs and entry type are not serialized.
/// Despite TS's comment, its actual stream is path NUL content NUL (no type tag).
fn hash_directory_tree(root: &Path) -> Result<String, Failure> {
    hash_directory_tree_with_collator(root, &collator(&Env::from_process())?)
}

fn hash_directory_tree_with_collator(
    root: &Path,
    collator: &CollatorBorrowed<'_>,
) -> Result<String, Failure> {
    fn collect(root: &Path, folder: &Path, out: &mut Vec<(String, bool)>) -> Result<(), Failure> {
        for entry in read_entries(folder)? {
            // Node decodes readdir names to UTF-8, then uses those decoded names.
            let path = folder.join(entry.file_name().to_string_lossy().as_ref());
            let metadata =
                fs::symlink_metadata(&path).map_err(|e| fs_failure(e, "lstat", &path))?;
            if metadata.is_dir() {
                collect(root, &path, out)?;
            } else if metadata.is_file() || metadata.file_type().is_symlink() {
                out.push((
                    path.strip_prefix(root)
                        .expect("descendant")
                        .to_string_lossy()
                        .into_owned(),
                    metadata.file_type().is_symlink(),
                ));
            }
        }
        Ok(())
    }
    let mut entries = Vec::new();
    collect(root, root, &mut entries)?;
    entries.sort_by(|a, b| collator.compare(&a.0, &b.0));
    let mut hash = Sha256::new();
    for (relative, link) in entries {
        hash.update(relative.as_bytes());
        hash.update([0]);
        let path = root.join(relative);
        if link {
            let target = fs::read_link(&path).map_err(|e| fs_failure(e, "readlink", &path))?;
            hash.update(target.to_string_lossy().as_bytes());
        } else {
            // `readFileSync(join(resolvedRoot, entry.path))`
            // (migrateLegacyBundledSkills.ts:163): an open error names the
            // path, a descriptor read error is `read` without it.
            let mut file = fs::File::open(&path).map_err(|e| fs_failure(e, "open", &path))?;
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut file, &mut bytes)
                .map_err(|e| fs_failure(e, "read", &path))?;
            hash.update(bytes);
        }
        hash.update([0]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn available_backup_path(root: &Path, slug: &str, hash: &str) -> PathBuf {
    let preferred = root.join(slug);
    if !preferred.exists() {
        return preferred;
    }
    let name = format!("{slug}-{}", &hash[..12]);
    let hashed = root.join(&name);
    if !hashed.exists() {
        return hashed;
    }
    let mut suffix = 2;
    loop {
        let candidate = root.join(format!("{name}-{suffix}"));
        if !candidate.exists() {
            return candidate;
        }
        suffix += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn migrate_legacy_bundled_skill_copies(
        home: &Path,
        root: &Path,
    ) -> Result<Vec<String>, Failure> {
        super::migrate_legacy_bundled_skill_copies(home, root).map(|report| report.warnings)
    }

    fn temp_home(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pdrs-cli-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn bundled_root(home: &Path) -> PathBuf {
        let root = home.join("bundled");
        fs::create_dir_all(root.join("docx")).unwrap();
        fs::write(root.join("docx/SKILL.md"), "x").unwrap();
        root
    }

    const MISSING_ROOT_WARNING: &str = "[pilotdeck] Could not migrate legacy skill '__migration__': Bundled skills root is missing or contains no skills; legacy copies were left untouched.";

    #[test]
    fn writes_marker_once_with_default_file_mode() {
        let home = temp_home("marker");
        let root = bundled_root(&home);
        let warnings = migrate_legacy_bundled_skill_copies(&home, &root);
        assert!(matches!(warnings, Ok(w) if w.is_empty()));
        let marker = home.join(MARKER);
        assert_eq!(fs::read(&marker).unwrap(), b"completed\n");
        let mode = fs::metadata(&marker).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode & 0o600, 0o600);
        assert_eq!(mode & 0o111, 0);
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn empty_explicit_root_warns_and_leaves_no_marker() {
        let home = temp_home("empty-root");
        let root = home.join("bundled");
        fs::create_dir_all(root.join("not-a-skill")).unwrap();
        let warnings = migrate_legacy_bundled_skill_copies(&home, &root)
            .ok()
            .unwrap();
        assert_eq!(warnings, vec![MISSING_ROOT_WARNING]);
        assert!(!home.join(MARKER).exists());
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn missing_root_warns_and_leaves_no_marker() {
        let home = temp_home("missing-root");
        let warnings = migrate_legacy_bundled_skill_copies(&home, &home.join("absent"))
            .ok()
            .unwrap();
        assert_eq!(warnings, vec![MISSING_ROOT_WARNING]);
        assert!(!home.join(MARKER).exists());
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn matching_copy_is_renamed_and_marker_prevents_later_override_migration() {
        let home = temp_home("copies");
        let root = bundled_root(&home);
        let source = home.join("skills/docx");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("SKILL.md"), "x").unwrap();
        assert!(
            migrate_legacy_bundled_skill_copies(&home, &root)
                .unwrap()
                .is_empty()
        );
        assert!(!source.exists());
        assert_eq!(
            fs::read(home.join("skill-backups/legacy-bundled-v1/docx/SKILL.md")).unwrap(),
            b"x"
        );
        assert!(home.join(MARKER).exists());
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("SKILL.md"), "x").unwrap();
        assert!(
            migrate_legacy_bundled_skill_copies(&home, &root)
                .unwrap()
                .is_empty()
        );
        assert!(source.exists());
        fs::remove_dir_all(home).unwrap();
    }

    /// Child half of `marker_write_reports_the_write_stage_like_node`: runs
    /// under `ulimit -f 0`, so even the fixed 10-byte marker write fails at
    /// the descriptor write stage. The parent pre-creates the home and the
    /// bundled root, because under that limit the child cannot set them up
    /// itself.
    #[cfg(unix)]
    #[test]
    #[ignore = "spawned by marker_write_reports_the_write_stage_like_node"]
    fn marker_child_over_file_size_limit() {
        let Ok(home) = std::env::var("PDRS_MARKER_HOME") else {
            panic!(
                "PDRS_MARKER_HOME not set; spawn via marker_write_reports_the_write_stage_like_node"
            );
        };
        // The gateway binary's startup disposition, through the same safe
        // signal API: SIGXFSZ ignored, so the write past RLIMIT_FSIZE
        // surfaces as EFBIG instead of killing us.
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("child test runtime")
            .block_on(async { pilotdeck_gateway::ensure_sigxfsz_ignored() });
        let home = PathBuf::from(home);
        let report = super::migrate_legacy_bundled_skill_copies(&home, &home.join("bundled"))
            .expect("the migration itself must not fail");
        let warning = report
            .warnings
            .first()
            .expect("the failed marker write must warn");
        println!("WRITE_ERROR={warning}");
    }

    /// The marker write is `writeFileSync(completionMarker, "completed\n")`
    /// (migrateLegacyBundledSkills.ts:126): a descriptor write error must
    /// read like Node's `EFBIG: file too large, write` with no path (node
    /// v22.22.0 under RLIMIT_FSIZE=0,
    /// `_tmp/20261025-issue-656/node-probe/write-stage-zero-limit.out`), not
    /// the open label with the marker path.
    #[cfg(unix)]
    #[test]
    fn marker_write_reports_the_write_stage_like_node() {
        let home = temp_home("marker-write-stage");
        bundled_root(&home);
        let output = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(
                "ulimit -f 0; exec \"$0\" --exact skills::tests::marker_child_over_file_size_limit --ignored --nocapture",
            )
            .arg(std::env::current_exe().unwrap())
            .env("PDRS_MARKER_HOME", &home)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let _ = fs::remove_dir_all(&home);
        assert_eq!(
            output.status.code(),
            Some(0),
            "child killed or failed: {stdout}"
        );
        let line = stdout
            .lines()
            .find_map(|line| line.strip_prefix("WRITE_ERROR="))
            .unwrap_or_else(|| panic!("no WRITE_ERROR line: {stdout}"));
        assert_eq!(
            line,
            "[pilotdeck] Could not migrate legacy skill '__migration__': EFBIG: file too large, write"
        );
    }

    /// The renderer the marker write feeds must drop the path for the write
    /// stage, including in the `UV_UNKNOWN` fallback for errnos outside the
    /// shared libuv table (node v22.22.0 formats a descriptor write error
    /// without a path).
    #[test]
    fn fs_failure_drops_the_path_for_write_stage_errors() {
        let marker = Path::new("/home/u/.pilotdeck/.legacy-bundled-skills-migrated-v1");
        for (error, expected) in [
            (
                std::io::Error::from_raw_os_error(libc::EFBIG),
                "EFBIG: file too large, write",
            ),
            (
                std::io::Error::from_raw_os_error(libc::ENOSPC),
                "ENOSPC: no space left on device, write",
            ),
            (
                std::io::Error::from_raw_os_error(123456),
                "UNKNOWN: unknown error, write",
            ),
        ] {
            match fs_failure(error, "write", marker) {
                Failure::Error(message) => assert_eq!(message, expected),
                other => panic!("expected Failure::Error, got {other:?}"),
            }
        }
    }

    #[test]
    fn unreadable_bundled_root_is_the_missing_warning() {
        let home = temp_home("root-errors");
        let file_root = home.join("file-root");
        fs::write(&file_root, "x").unwrap();
        let warnings = migrate_legacy_bundled_skill_copies(&home, &file_root.join("sub"));
        assert!(matches!(warnings, Ok(w) if w == vec![MISSING_ROOT_WARNING]));
        let warnings = migrate_legacy_bundled_skill_copies(&home, &file_root);
        assert!(matches!(warnings, Ok(w) if w == vec![MISSING_ROOT_WARNING]));
        assert!(!home.join(MARKER).exists());
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn unreadable_user_skills_root_is_thrown_like_readdir_sync() {
        let home = temp_home("user-root-file");
        let root = bundled_root(&home);
        fs::write(home.join("skills"), "x").unwrap();
        let message = match migrate_legacy_bundled_skill_copies(&home, &root) {
            Err(Failure::Error(message)) => message,
            _ => String::from("not an error"),
        };
        assert_eq!(
            message,
            format!(
                "ENOTDIR: not a directory, scandir '{}'",
                home.join("skills").display()
            )
        );
        assert!(!home.join(MARKER).exists());

        fs::remove_file(home.join("skills")).unwrap();
        fs::create_dir_all(home.join("skills/docx")).unwrap();
        fs::set_permissions(home.join("skills"), fs::Permissions::from_mode(0o000)).unwrap();
        let result = migrate_legacy_bundled_skill_copies(&home, &root);
        fs::set_permissions(home.join("skills"), fs::Permissions::from_mode(0o700)).unwrap();
        let expected = format!(
            "EACCES: permission denied, scandir '{}'",
            home.join("skills").display()
        );
        assert!(matches!(result, Err(Failure::Error(m)) if m == expected));
        assert!(!home.join(MARKER).exists());
        fs::remove_dir_all(home).unwrap();
    }

    /// Cases recorded by probes/p4-skills-errors (Node v22.22.0, uid 502).
    #[test]
    fn unsearchable_skill_dirs_and_file_pilot_home_match_ts_probe() {
        let home = temp_home("unsearchable");
        let root = bundled_root(&home);
        fs::set_permissions(root.join("docx"), fs::Permissions::from_mode(0o600)).unwrap();
        let warnings = migrate_legacy_bundled_skill_copies(&home, &root);
        fs::set_permissions(root.join("docx"), fs::Permissions::from_mode(0o700)).unwrap();
        assert!(matches!(warnings, Ok(w) if w == vec![MISSING_ROOT_WARNING]));
        assert!(!home.join(MARKER).exists());

        // existsSync(<skill>/SKILL.md) is false under EACCES: not a legacy copy.
        let skill = home.join("skills/docx");
        fs::create_dir_all(&skill).unwrap();
        fs::write(skill.join("SKILL.md"), "user\n").unwrap();
        fs::set_permissions(&skill, fs::Permissions::from_mode(0o600)).unwrap();
        let warnings = migrate_legacy_bundled_skill_copies(&home, &root);
        fs::set_permissions(&skill, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(matches!(warnings, Ok(w) if w.is_empty()));
        assert!(home.join(MARKER).exists());

        let file_home = home.join("pilot-file");
        fs::write(&file_home, "a file, not a directory\n").unwrap();
        let warnings = migrate_legacy_bundled_skill_copies(&file_home, &root);
        let expected = format!(
            "[pilotdeck] Could not migrate legacy skill '__migration__': EEXIST: file already exists, mkdir '{}'",
            file_home.display()
        );
        assert!(matches!(warnings, Ok(w) if w == vec![expected]));
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn hash_ignores_empty_dirs_but_includes_relative_paths_and_link_text() {
        use std::os::unix::fs::symlink;
        let home = temp_home("tree-hash");
        fs::write(home.join("SKILL.md"), "x").unwrap();
        let expected = format!("{:x}", Sha256::digest(b"SKILL.md\0x\0"));
        assert_eq!(hash_directory_tree(&home).unwrap(), expected);
        fs::create_dir(home.join("empty")).unwrap();
        assert_eq!(hash_directory_tree(&home).unwrap(), expected);
        symlink("missing-target", home.join("link")).unwrap();
        let with_link = hash_directory_tree(&home).unwrap();
        fs::remove_file(home.join("link")).unwrap();
        fs::write(home.join("link"), "missing-target").unwrap();
        // TS does not serialize a type tag: file bytes and link text are equal.
        assert_eq!(hash_directory_tree(&home).unwrap(), with_link);
        fs::rename(home.join("link"), home.join("renamed")).unwrap();
        assert_ne!(hash_directory_tree(&home).unwrap(), with_link);
        fs::remove_dir_all(home).unwrap();
    }

    fn recorded_probe() -> serde_json::Value {
        // Coordinator's Node 22.22.0 / ICU 78.2 probe under LANG=C, LC_ALL=C.
        // See RESULT.md, second round, for the raw recording and its SHA-256.
        serde_json::from_str(include_str!("../tests/fixtures/skills-probe.json")).unwrap()
    }

    #[test]
    fn c_locale_matches_all_recorded_node_comparisons_and_stable_sort() {
        let probe = recorded_probe();
        assert_eq!(probe["locale"]["locale"], "en-US");
        let collator = collator(&Env::from_pairs(&[("LANG", "C"), ("LC_ALL", "C")])).unwrap();
        let names: Vec<_> = probe["names"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        for (i, a) in names.iter().enumerate() {
            for (j, b) in names.iter().enumerate() {
                let sign = match collator.compare(a, b) {
                    std::cmp::Ordering::Less => -1,
                    std::cmp::Ordering::Equal => 0,
                    std::cmp::Ordering::Greater => 1,
                };
                assert_eq!(probe["comparisons"][i][j], sign, "{a:?} vs {b:?}");
            }
        }
        let mut sorted = names;
        sorted.sort_by(|a, b| collator.compare(a, b));
        assert_eq!(serde_json::json!(sorted), probe["sorted"]);
    }

    #[test]
    fn directory_hash_matches_recorded_node_unicode_tree() {
        let probe = recorded_probe();
        let tree = temp_home("recorded-hash");
        fs::create_dir(tree.join("empty")).unwrap();
        for name in probe["hashTree"]["paths"].as_array().unwrap() {
            let name = name.as_str().unwrap();
            let path = tree.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, format!("bytes:{name}\n")).unwrap();
        }
        std::os::unix::fs::symlink(
            probe["hashTree"]["link"].as_str().unwrap(),
            tree.join("link"),
        )
        .unwrap();
        let collator = collator(&Env::from_pairs(&[("LANG", "C"), ("LC_ALL", "C")])).unwrap();
        assert_eq!(
            hash_directory_tree_with_collator(&tree, &collator).unwrap(),
            probe["hashTree"]["sha256"].as_str().unwrap()
        );
        fs::remove_dir_all(tree).unwrap();
    }

    #[test]
    fn modified_copy_stays_and_backup_collisions_get_incremental_suffixes() {
        let home = temp_home("backup-collisions");
        let root = bundled_root(&home);
        fs::create_dir_all(home.join("skills/docx")).unwrap();
        fs::write(home.join("skills/docx/SKILL.md"), "x").unwrap();
        fs::create_dir_all(home.join("skills/custom")).unwrap();
        fs::write(home.join("skills/custom/SKILL.md"), "user edit").unwrap();
        let hash = hash_directory_tree(&home.join("skills/docx")).unwrap();
        let backups = home.join("skill-backups/legacy-bundled-v1");
        for name in [
            "docx".to_string(),
            format!("docx-{}", &hash[..12]),
            format!("docx-{}-2", &hash[..12]),
        ] {
            fs::create_dir_all(backups.join(name)).unwrap();
        }
        assert!(
            migrate_legacy_bundled_skill_copies(&home, &root)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            fs::read(backups.join(format!("docx-{}-3/SKILL.md", &hash[..12]))).unwrap(),
            b"x"
        );
        assert!(home.join("skills/custom/SKILL.md").exists());
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn per_skill_failure_warns_and_blocks_marker_until_retry() {
        let home = temp_home("retry");
        let root = bundled_root(&home);
        fs::create_dir_all(home.join("skills/docx")).unwrap();
        fs::write(home.join("skills/docx/SKILL.md"), "x").unwrap();
        fs::write(home.join("skill-backups"), "not a directory").unwrap();
        let warnings = migrate_legacy_bundled_skill_copies(&home, &root).unwrap();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].starts_with(
            "[pilotdeck] Could not migrate legacy skill 'docx': ENOTDIR: not a directory, mkdir '"
        ));
        assert!(home.join("skills/docx/SKILL.md").exists());
        assert!(!home.join(MARKER).exists());
        fs::remove_file(home.join("skill-backups")).unwrap();
        assert!(
            migrate_legacy_bundled_skill_copies(&home, &root)
                .unwrap()
                .is_empty()
        );
        assert!(home.join(MARKER).exists());
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    #[ignore = "requires read-only PDRS_BASELINE with the pinned historical git blob"]
    fn real_known_legacy_copy_moves_even_when_current_builtin_differs() {
        let baseline = std::env::var("PDRS_BASELINE").expect("baseline path");
        let blob = std::process::Command::new("/usr/bin/git")
            .env_clear()
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .args([
                "-C",
                &baseline,
                "show",
                "db77b86d09441c5fab6340a669598f8fbabe82c7:skills/browser-use/SKILL.md",
            ])
            .output()
            .unwrap();
        assert!(blob.status.success());
        let probe = recorded_probe();
        assert_eq!(
            blob.stdout.len(),
            probe["historicalBlob"]["size"].as_u64().unwrap() as usize
        );
        assert_eq!(
            format!("{:x}", Sha256::digest(&blob.stdout)),
            probe["historicalBlob"]["sha256"]
        );
        let home = temp_home("real-legacy");
        let builtin = bundled_root(&home);
        let copy = home.join("skills/browser-use");
        fs::create_dir_all(&copy).unwrap();
        fs::write(copy.join("SKILL.md"), blob.stdout).unwrap();
        assert_eq!(hash_directory_tree(&copy).unwrap(), KNOWN_LEGACY[0].1);
        assert_eq!(KNOWN_LEGACY[0].1, probe["historicalBlob"]["treeHash"]);
        fs::create_dir_all(builtin.join("browser-use")).unwrap();
        fs::write(
            builtin.join("browser-use/SKILL.md"),
            "different current bundle",
        )
        .unwrap();
        fs::create_dir_all(home.join("skills/docx")).unwrap();
        fs::write(home.join("skills/docx/SKILL.md"), "modified by user").unwrap();
        assert!(
            migrate_legacy_bundled_skill_copies(&home, &builtin)
                .unwrap()
                .is_empty()
        );
        assert!(!copy.exists());
        assert!(
            home.join("skill-backups/legacy-bundled-v1/browser-use/SKILL.md")
                .exists()
        );
        assert!(home.join("skills/docx/SKILL.md").exists());
        assert_eq!(fs::read(home.join(MARKER)).unwrap(), b"completed\n");
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn successful_moves_emit_the_exact_stdout_notice() {
        let home = temp_home("notice");
        let root = bundled_root(&home);
        fs::create_dir_all(home.join("skills/docx")).unwrap();
        fs::write(home.join("skills/docx/SKILL.md"), "x").unwrap();
        let report = super::migrate_legacy_bundled_skill_copies(&home, &root).unwrap();
        assert!(report.warnings.is_empty());
        assert_eq!(
            report.notice,
            Some(format!(
                "[pilotdeck] Activated bundled skills directly; moved 1 unchanged legacy copy to {}.",
                home.join("skill-backups/legacy-bundled-v1").display()
            ))
        );
        assert!(
            super::migrate_legacy_bundled_skill_copies(&home, &root)
                .unwrap()
                .notice
                .is_none()
        );
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn long_backup_name_warns_and_later_skill_still_moves() {
        let home = temp_home("long-warning");
        let root = bundled_root(&home);
        let long = "a".repeat(250);
        for name in [long.as_str(), "docx"] {
            fs::create_dir_all(home.join("skills").join(name)).unwrap();
            fs::write(home.join("skills").join(name).join("SKILL.md"), "x").unwrap();
            fs::create_dir_all(root.join(name)).unwrap();
            fs::write(root.join(name).join("SKILL.md"), "x").unwrap();
        }
        let backup = home.join("skill-backups/legacy-bundled-v1");
        fs::create_dir_all(backup.join(&long)).unwrap();
        let report = super::migrate_legacy_bundled_skill_copies(&home, &root).unwrap();
        assert_eq!(report.warnings.len(), 1);
        assert!(report.warnings[0].contains("ENAMETOOLONG: name too long, rename"));
        assert!(
            report
                .notice
                .unwrap()
                .contains("moved 1 unchanged legacy copy")
        );
        assert!(home.join("skills").join(&long).exists());
        assert!(backup.join("docx/SKILL.md").exists());
        assert!(!home.join(MARKER).exists());
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn looping_backup_root_warns_without_todo() {
        use std::os::unix::fs::symlink;
        let home = temp_home("loop-warning");
        let root = bundled_root(&home);
        fs::create_dir_all(home.join("skills/docx")).unwrap();
        fs::write(home.join("skills/docx/SKILL.md"), "x").unwrap();
        symlink("skill-backups", home.join("skill-backups")).unwrap();
        let report = super::migrate_legacy_bundled_skill_copies(&home, &root).unwrap();
        assert_eq!(report.warnings.len(), 1);
        assert!(report.warnings[0].contains("ELOOP: too many symbolic links encountered, mkdir"));
        assert!(report.notice.is_none());
        assert!(!home.join(MARKER).exists());
        fs::remove_dir_all(home).unwrap();
    }

    fn root_of(env: &Env, cwd: &Path, exe_dir: &Path) -> PathBuf {
        builtin_skills_root(env, cwd, Some(exe_dir)).ok().unwrap()
    }

    #[test]
    fn explicit_root_wins_and_empty_value_probes() {
        let base = temp_home("explicit-root");
        let exe_dir = base.join("stage/bin");
        fs::create_dir_all(base.join("stage/skills")).unwrap();
        let cwd = base.join("work");
        let env = Env::from_pairs(&[("PILOTDECK_BUNDLED_SKILLS_DIR", "rel")]);
        assert_eq!(root_of(&env, &cwd, &exe_dir), cwd.join("rel"));
        let env = Env::from_pairs(&[("PILOTDECK_BUNDLED_SKILLS_DIR", "")]);
        assert_eq!(root_of(&env, &cwd, &exe_dir), base.join("stage/skills"));
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn probes_candidates_in_order_then_falls_back_to_cwd() {
        let base = temp_home("candidates");
        let exe_dir = base.join("a/stage/bin");
        let cwd = base.join("work");
        let env = Env::from_pairs(&[]);
        let first = base.join("a/stage/skills");
        let second = base.join("a/skills");
        for dir in [&first, &second, &cwd.join("skills")] {
            fs::create_dir_all(dir).unwrap();
        }
        assert_eq!(root_of(&env, &cwd, &exe_dir), first);
        fs::remove_dir_all(&first).unwrap();
        assert_eq!(root_of(&env, &cwd, &exe_dir), second);
        fs::remove_dir_all(&second).unwrap();
        assert_eq!(root_of(&env, &cwd, &exe_dir), cwd.join("skills"));
        fs::remove_dir_all(cwd.join("skills")).unwrap();
        assert_eq!(root_of(&env, &cwd, &exe_dir), cwd.join("skills"));
        fs::remove_dir_all(base).unwrap();
    }
}
