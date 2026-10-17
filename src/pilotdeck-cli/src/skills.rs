//! migrateLegacyBundledSkillCopies (src/extension/skills/migrateLegacyBundledSkills.ts),
//! up to the one-time completion marker.
use crate::Failure;
use crate::paths::{self, Env};
use std::fs;
use std::path::{Path, PathBuf};

const MARKER: &str = ".legacy-bundled-skills-migrated-v1";

pub enum BuiltinSkills {
    /// PILOTDECK_BUNDLED_SKILLS_DIR, checked on disk exactly like TS.
    Dir(PathBuf),
    /// Skills shipped with the release itself (TS: the package `skills/` next to dist).
    Packaged,
}

pub fn builtin_skills(env: &Env, cwd: &Path) -> BuiltinSkills {
    match env.get("PILOTDECK_BUNDLED_SKILLS_DIR") {
        Some(dir) if !dir.is_empty() => BuiltinSkills::Dir(paths::resolve(cwd, Path::new(dir))),
        _ => BuiltinSkills::Packaged,
    }
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
    builtin: &BuiltinSkills,
) -> Result<Vec<String>, Failure> {
    let marker = pilot_home.join(MARKER);
    if marker.exists() {
        return Ok(vec![]);
    }
    let has_skills = match builtin {
        BuiltinSkills::Dir(root) => has_bundled_skills(root),
        BuiltinSkills::Packaged => true,
    };
    if !has_skills {
        return Ok(vec![warning(
            "__migration__",
            "Bundled skills root is missing or contains no skills; legacy copies were left untouched.",
        )]);
    }
    let user_root = pilot_home.join("skills");
    if user_root.exists() {
        const COPIES: &str = "migrating legacy bundled skill copies out of PilotHome/skills";
        let entries = fs::read_dir(&user_root).map_err(|_| Failure::Todo(COPIES))?;
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

    #[test]
    fn writes_marker_once_with_default_file_mode() {
        let home = temp_home("marker");
        let warnings = migrate_legacy_bundled_skill_copies(&home, &BuiltinSkills::Packaged);
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
        let warnings = migrate_legacy_bundled_skill_copies(&home, &BuiltinSkills::Dir(root))
            .ok()
            .unwrap();
        assert_eq!(
            warnings,
            vec![
                "[pilotdeck] Could not migrate legacy skill '__migration__': Bundled skills root is missing or contains no skills; legacy copies were left untouched."
            ]
        );
        assert!(!home.join(MARKER).exists());
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn user_skill_copy_is_an_explicit_todo() {
        let home = temp_home("copies");
        fs::create_dir_all(home.join("skills/docx")).unwrap();
        fs::write(home.join("skills/docx/SKILL.md"), "x").unwrap();
        let result = migrate_legacy_bundled_skill_copies(&home, &BuiltinSkills::Packaged);
        assert!(matches!(result, Err(Failure::Todo(_))));
        assert!(!home.join(MARKER).exists());
        fs::remove_dir_all(home).unwrap();
    }
}
