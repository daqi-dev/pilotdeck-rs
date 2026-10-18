//! migrateLegacyBundledSkillCopies (src/extension/skills/migrateLegacyBundledSkills.ts),
//! up to the one-time completion marker.
use crate::Failure;
use crate::paths::{self, Env};
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

/// Returns the console.warn lines createLocalGateway prints for failures.
pub fn migrate_legacy_bundled_skill_copies(
    pilot_home: &Path,
    builtin_root: &Path,
) -> Result<Vec<String>, Failure> {
    let marker = pilot_home.join(MARKER);
    if marker.exists() {
        return Ok(vec![]);
    }
    if !has_bundled_skills(builtin_root) {
        return Ok(vec![warning(
            "__migration__",
            "Bundled skills root is missing or contains no skills; legacy copies were left untouched.",
        )]);
    }
    let user_root = pilot_home.join("skills");
    if user_root.exists() {
        const COPIES: &str = "migrating legacy bundled skill copies out of PilotHome/skills";
        // readdirSync here is outside any try (migrateLegacyBundledSkills.ts:82-83), so
        // ENOTDIR/EACCES escape createLocalGateway to main().catch.
        let entries = fs::read_dir(&user_root).map_err(|error| {
            paths::node_fs_message(&error, "scandir", &user_root)
                .map_or(Failure::Todo(COPIES), Failure::Error)
        })?;
        for entry in entries {
            let entry = entry.map_err(|_| Failure::Todo(COPIES))?;
            let is_dir = entry
                .file_type()
                .map_err(|_| Failure::Todo(COPIES))?
                .is_dir();
            if is_dir && entry.path().join("SKILL.md").exists() {
                return Err(Failure::Todo(COPIES));
            }
        }
    }
    let written = fs::create_dir_all(pilot_home)
        .map_err(|error| (error, "mkdir", pilot_home.to_path_buf()))
        .and_then(|()| {
            fs::write(&marker, "completed\n").map_err(|error| (error, "open", marker.clone()))
        });
    match written {
        Ok(()) => Ok(vec![]),
        Err((error, syscall, path)) => paths::node_fs_message(&error, syscall, &path)
            .map(|message| vec![warning("__migration__", &message)])
            .ok_or(Failure::Todo(
                "reporting this completion-marker write error",
            )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

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
    fn user_skill_copy_is_an_explicit_todo() {
        let home = temp_home("copies");
        let root = bundled_root(&home);
        fs::create_dir_all(home.join("skills/docx")).unwrap();
        fs::write(home.join("skills/docx/SKILL.md"), "x").unwrap();
        let result = migrate_legacy_bundled_skill_copies(&home, &root);
        assert!(matches!(result, Err(Failure::Todo(_))));
        assert!(!home.join(MARKER).exists());
        fs::remove_dir_all(home).unwrap();
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
