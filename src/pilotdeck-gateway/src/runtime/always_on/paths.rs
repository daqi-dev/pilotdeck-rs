//! Port of TS `src/always-on/storage/AlwaysOnPaths.ts` (resolveAlwaysOnPaths
//! plus the plan/report/run path helpers). `projectId` comes from the shared
//! `Metadata::project_storage_id` port of `resolveProjectStorageId`
//! (`pilot/paths.ts:85-87`): the `.cwd` marker scan first, the legacy path
//! slug as the fallback — the same identity the session and memory families
//! key their per-project directories by (unlike cron's `project_id`, which
//! is the legacy slug only).
use std::{
    io,
    path::{Path, PathBuf},
};

use pilotdeck_protocol::JsString;

use crate::metadata::{self, Metadata};

/// The ported `AlwaysOnPaths` shape (`resolveAlwaysOnPaths`,
/// AlwaysOnPaths.ts:36-77). The fields are the ones the ported halves read;
/// the TS type's `worktreesBaseDir`/`snapshotsBaseDir` overrides only feed
/// the workspace strategies, which stay behind the fire seam.
#[derive(Clone, Debug)]
pub(crate) struct AlwaysOnPaths {
    pub project_key: String,
    pub state_file: PathBuf,
    pub plans_dir: PathBuf,
    pub plan_index_file: PathBuf,
    pub cycle_index_file: PathBuf,
    pub reports_dir: PathBuf,
    pub discovery_lock_file: PathBuf,
}

/// `resolveAlwaysOnPaths({pilotHome, projectKey})` (AlwaysOnPaths.ts:36-77):
/// `path.resolve` on both inputs (an absolute argument ignores the process
/// cwd; a relative one resolves against it), then the storage id decides the
/// project directory. `metadata` is the same face the other per-project
/// families build (`home` = pilot home, lazy process cwd).
pub(crate) fn resolve_always_on_paths(
    pilot_home: &Path,
    project_key: &str,
    metadata: &Metadata,
) -> io::Result<AlwaysOnPaths> {
    let home = metadata::resolve_process_path(Path::new(""), pilot_home)?;
    let key = metadata::resolve_process_path(Path::new(""), Path::new(project_key))?;
    let key = key.to_string_lossy().into_owned();
    let project_id = metadata.project_storage_id(&key);
    let root = metadata::resolve(&home, "always-on");
    let project_dir = metadata::resolve(&metadata::resolve(&root, "projects"), &project_id);
    Ok(AlwaysOnPaths {
        project_key: key,
        state_file: metadata::resolve(&project_dir, "state.json"),
        plans_dir: metadata::resolve(&project_dir, "plans"),
        plan_index_file: metadata::resolve(&metadata::resolve(&project_dir, "plans"), "index.json"),
        cycle_index_file: metadata::resolve(
            &metadata::resolve(&project_dir, "cycles"),
            "index.json",
        ),
        reports_dir: metadata::resolve(&project_dir, "reports"),
        discovery_lock_file: metadata::resolve(
            &metadata::resolve(&project_dir, "locks"),
            "discovery.lock",
        ),
    })
}

/// `planMarkdownPath` (AlwaysOnPaths.ts:79-81).
pub(crate) fn plan_markdown_path(paths: &AlwaysOnPaths, plan_id: &JsString) -> PathBuf {
    metadata::resolve(&paths.plans_dir, &format!("{}.md", sanitize_id(plan_id)))
}

/// `reportMarkdownPath` (AlwaysOnPaths.ts:83-85).
pub(crate) fn report_markdown_path(paths: &AlwaysOnPaths, run_id: &JsString) -> PathBuf {
    metadata::resolve(&paths.reports_dir, &format!("{}.md", sanitize_id(run_id)))
}

/// AlwaysOnPaths.ts:95-97: `[^A-Za-z0-9._-]+` collapses to one `-` per run (the
/// regex has no `u` flag, so it walks UTF-16 code units), edge dash runs trim,
/// and an empty result becomes `unnamed`. The output is always non-empty
/// ASCII, so it is one safe path segment.
fn sanitize_id(value: &JsString) -> String {
    const LOWER: std::ops::RangeInclusive<u16> = b'a' as u16..=b'z' as u16;
    const UPPER: std::ops::RangeInclusive<u16> = b'A' as u16..=b'Z' as u16;
    const DIGIT: std::ops::RangeInclusive<u16> = b'0' as u16..=b'9' as u16;
    let allowed = |unit: u16| {
        LOWER.contains(&unit)
            || UPPER.contains(&unit)
            || DIGIT.contains(&unit)
            || unit == u16::from(b'.')
            || unit == u16::from(b'_')
            || unit == u16::from(b'-')
    };
    let mut out = String::new();
    let mut replacing = false;
    for &unit in value.units() {
        if allowed(unit) {
            out.push(char::from_u32(u32::from(unit)).unwrap_or('-'));
            replacing = false;
        } else if !replacing {
            out.push('-');
            replacing = true;
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "unnamed".into()
    } else {
        trimmed.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::negative_assert_eq;
    use pilotdeck_protocol::JsString;

    // AlwaysOnPaths.ts:44-77: the directory layout is
    // `<home>/always-on/projects/<projectId>/…` with the storage id — the
    // marker-scan identity, not cron's legacy slug.
    #[test]
    fn resolves_the_project_directory_layout() {
        let home = std::env::temp_dir().join("always-on-2294-paths-layout");
        let project = home.join("proj");
        let metadata = Metadata {
            home: home.clone(),
            cwd: PathBuf::new(),
            files: crate::metadata::NativeFiles,
        };
        let paths = resolve_always_on_paths(&home, project.to_string_lossy().as_ref(), &metadata)
            .expect("#2294 paths resolve");
        let id = metadata.project_storage_id(project.to_string_lossy().as_ref());
        let prefix = home.join("always-on").join("projects").join(&id);
        negative_assert_eq!(
            paths.state_file.strip_prefix(&prefix).is_ok()
                && paths.cycle_index_file.strip_prefix(&prefix).is_ok()
                && paths.plan_index_file.strip_prefix(&prefix).is_ok(),
            true,
            "#2294 state/cycle/plan files sit in the storage-id project directory"
        );
        negative_assert_eq!(
            paths.discovery_lock_file.to_string_lossy(),
            format!("{}/locks/discovery.lock", prefix.display()),
            "#2294 discovery.lock path"
        );
        negative_assert_eq!(
            paths.project_key,
            project.to_string_lossy().into_owned(),
            "#2294 project key resolves absolute"
        );
    }

    // The storage id is the marker scan, not the slug: two distinct projects
    // whose legacy slugs collide share one directory only when their `.cwd`
    // markers say so. The marker half is `metadata`'s own test surface; here
    // the claim is only that always-on keys by it (paths.ts:85-87).
    #[test]
    fn project_directory_uses_the_marker_identity() {
        let home = std::env::temp_dir().join("always-on-2294-paths-marker");
        let project = home.join("proj-marked");
        std::fs::create_dir_all(&project).expect("#2294 fixture project");
        std::fs::write(project.join(".cwd"), project.to_string_lossy().as_bytes())
            .expect("#2294 marker");
        std::fs::create_dir_all(home.join("projects").join("marked-id")).expect("#2294 registry");
        std::fs::write(
            home.join("projects").join("marked-id").join(".cwd"),
            project.to_string_lossy().as_bytes(),
        )
        .expect("#2294 registry marker");
        let metadata = Metadata {
            home: home.clone(),
            cwd: PathBuf::new(),
            files: crate::metadata::NativeFiles,
        };
        let paths = resolve_always_on_paths(&home, project.to_string_lossy().as_ref(), &metadata)
            .expect("#2294 paths resolve");
        negative_assert_eq!(
            paths
                .state_file
                .starts_with(home.join("always-on").join("projects").join("marked-id")),
            true,
            "#2294 the marker id wins over the legacy slug"
        );
    }

    // AlwaysOnPaths.ts:95-97: non-identifier runs collapse to one dash per run
    // and empty results become `unnamed`.
    #[test]
    fn sanitize_id_collapses_and_trims() {
        for (input, expected) in [
            ("plain-id_1.md", "plain-id_1.md"),
            ("a///b", "a-b"),
            ("  spaced  ", "spaced"),
            ("///", "unnamed"),
            ("中文", "unnamed"),
        ] {
            negative_assert_eq!(
                sanitize_id(&JsString::from(input)),
                expected,
                "#2294 sanitize_id({input})"
            );
        }
    }

    // planMarkdownPath (AlwaysOnPaths.ts:79-81).
    #[test]
    fn plan_markdown_path_uses_the_sanitized_id() {
        let paths = AlwaysOnPaths {
            project_key: "/p".into(),
            state_file: PathBuf::new(),
            plans_dir: PathBuf::from("/home/always-on/projects/x/plans"),
            plan_index_file: PathBuf::new(),
            cycle_index_file: PathBuf::new(),
            reports_dir: PathBuf::from("/home/always-on/projects/x/reports"),
            discovery_lock_file: PathBuf::new(),
        };
        negative_assert_eq!(
            plan_markdown_path(&paths, &JsString::from("p 1")).to_string_lossy(),
            "/home/always-on/projects/x/plans/p-1.md",
            "#2294 plan markdown path"
        );
        negative_assert_eq!(
            report_markdown_path(&paths, &JsString::from("r 1")).to_string_lossy(),
            "/home/always-on/projects/x/reports/r-1.md",
            "#2294 report markdown path"
        );
    }
}
