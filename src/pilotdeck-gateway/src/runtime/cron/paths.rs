//! Port of TS `src/cron/storage/CronPaths.ts` (42 lines; line refs below) and
//! `createLegacyProjectId` (`src/pilot/paths.ts:110-116`, via the shared
//! `metadata::project_id` port).
use std::{
    io,
    path::{Path, PathBuf},
};

use pilotdeck_protocol::JsString;

use crate::metadata;

/// CronPaths.ts:6-15.
pub struct CronPaths {
    pub pilot_home: PathBuf,
    pub project_key: String,
    pub project_id: String,
    pub root_dir: PathBuf,
    pub project_dir: PathBuf,
    pub tasks_file: PathBuf,
    pub runs_dir: PathBuf,
    pub run_history_file: PathBuf,
}

/// CronPaths.ts:17-34. `path.resolve` against the process cwd, like the TS
/// one-argument resolve on both inputs: an absolute argument returns
/// normalized without consulting the process cwd (so a deleted cwd cannot
/// fail an absolute home), a relative one resolves against it.
pub fn resolve_cron_paths(pilot_home: &str, project_key: &str) -> io::Result<CronPaths> {
    let home = metadata::resolve_process_path(Path::new(""), Path::new(pilot_home))?;
    let key = metadata::resolve_process_path(Path::new(""), Path::new(project_key))?;
    resolve_cron_paths_from(&home, &key.to_string_lossy())
}

/// The same resolution over an already-resolved pilot home. The migration
/// resolves its home once at the entry and every derived key (routing
/// targets, blocked directories) must come from those exact bytes: a lossy
/// re-parse would key the same physical directory twice and the later empty
/// bucket would overwrite the migrated tasks.
pub(super) fn resolve_cron_paths_from(
    pilot_home: &Path,
    project_key: &str,
) -> io::Result<CronPaths> {
    let key = metadata::resolve_process_path(Path::new(""), Path::new(project_key))?;
    let project_id = metadata::project_id(&key);
    let root_dir = metadata::resolve(pilot_home, "cron");
    let project_dir = metadata::resolve(&metadata::resolve(&root_dir, "projects"), &project_id);
    let tasks_file = metadata::resolve(&project_dir, "tasks.json");
    let runs_dir = metadata::resolve(&project_dir, "runs");
    let run_history_file = metadata::resolve(&project_dir, "run-history.jsonl");
    Ok(CronPaths {
        pilot_home: pilot_home.to_path_buf(),
        project_key: key.to_string_lossy().into_owned(),
        project_id,
        root_dir,
        project_dir,
        tasks_file,
        runs_dir,
        run_history_file,
    })
}

/// CronPaths.ts:36-38.
pub(crate) fn cron_run_events_path(paths: &CronPaths, run_id: &JsString) -> PathBuf {
    metadata::resolve(
        &paths.runs_dir,
        &format!("{}.events.jsonl", sanitize_id(run_id)),
    )
}

/// CronPaths.ts:40-42: `[^A-Za-z0-9._-]+` collapses to one `-` per run (the
/// regex has no `u` flag, so it walks UTF-16 code units), edge dash runs trim,
/// and an empty result becomes `unnamed`. The output is always non-empty ASCII,
/// so it is one safe path segment.
fn sanitize_id(value: &JsString) -> String {
    let mut out = String::new();
    let mut replacing = false;
    for &unit in value.units() {
        // `[^A-Za-z0-9._-]` on UTF-16 code units.
        let allowed = matches!(
            unit,
            0x30..=0x39 | 0x41..=0x5A | 0x61..=0x7A | 0x2E | 0x5F | 0x2D
        );
        if allowed {
            out.push(unit as u8 as char);
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
