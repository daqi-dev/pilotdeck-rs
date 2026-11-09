//! Destination-first migration; no source is pruned until all destinations exist.
use super::store::{atomic_write, global_lock, read_optional, read_tasks, safe_id, write_tasks};
use super::{CronResult, CronStore, absolute, text};
use crate::cron::schedule;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

struct Snapshot {
    dir: PathBuf,
    tasks: Vec<Value>,
    runs: Vec<Value>,
    invalid: Vec<String>,
}
#[derive(Default)]
struct RunBucket {
    records: Vec<Value>,
    invalid: Vec<String>,
}

pub(super) fn migrate(home: &Path, now_ms: i64) -> CronResult<()> {
    let _lock = global_lock(home, false)?;
    let dirs = project_dirs(home)?;
    let mut originals = Vec::new();
    for dir in dirs {
        // Validate every source before writing any destination. Corrupt data is never replaced by an empty store.
        read_tasks(&dir.join("tasks.json"))?;
        let tasks = match read_optional(&dir.join("tasks.json"))? {
            Some(raw) => serde_json::from_str::<Value>(&raw)?["tasks"]
                .as_array()
                .cloned()
                .unwrap_or_default(),
            None => Vec::new(),
        };
        let (runs, invalid) = read_migration_runs(&dir.join("run-history.jsonl"))?;
        originals.push(Snapshot {
            dir,
            tasks,
            runs,
            invalid,
        });
    }
    if originals.is_empty() {
        return Ok(());
    }
    let mut task_projects: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for source in &originals {
        for task in &source.tasks {
            if let Some(project) = project_key(task)? {
                task_projects
                    .entry(text(task, "taskId").to_owned())
                    .or_default()
                    .insert(project);
            }
        }
    }
    let mut tasks: BTreeMap<PathBuf, Vec<Value>> = BTreeMap::new();
    let mut runs: BTreeMap<PathBuf, RunBucket> = BTreeMap::new();
    let mut run_targets: BTreeMap<String, BTreeSet<PathBuf>> = BTreeMap::new();
    for source in &originals {
        tasks.entry(source.dir.clone()).or_default();
        runs.entry(source.dir.clone())
            .or_default()
            .invalid
            .extend(source.invalid.clone());
        for task in &source.tasks {
            let target = target(home, project_key(task)?.as_deref(), &source.dir)?;
            tasks.entry(target).or_default().push(task.clone());
        }
        for run in &source.runs {
            let project = project_key(run)?.or_else(|| {
                task_projects
                    .get(text(run, "taskId"))
                    .filter(|set| set.len() == 1)
                    .and_then(|set| set.first().cloned())
            });
            let target = target(home, project.as_deref(), &source.dir)?;
            if let Some(id) = run.get("runId").and_then(Value::as_str) {
                run_targets
                    .entry(id.into())
                    .or_default()
                    .insert(target.clone());
            }
            runs.entry(target).or_default().records.push(run.clone());
        }
    }
    for (dir, records) in &tasks {
        let owners: BTreeSet<_> = records
            .iter()
            .map(project_key)
            .collect::<CronResult<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect();
        if owners.len() > 1 {
            return Err(super::CronError::new(
                "cron_project_collision",
                format!(
                    "{}: distinct projects have the same Cron storage id; original data preserved",
                    dir.display()
                ),
            ));
        }
        if let Some(marker) = read_optional(&dir.join(".cwd"))?
            && !marker.trim().is_empty()
        {
            let marked = absolute(Path::new(marker.trim()))?
                .to_string_lossy()
                .into_owned();
            if owners.iter().any(|owner| owner != &marked) {
                return Err(super::CronError::new(
                    "cron_project_collision",
                    format!(
                        "{}: Cron marker belongs to another project; original data preserved",
                        dir.display()
                    ),
                ));
            }
        }
    }
    for records in tasks.values_mut() {
        *records = dedupe_tasks(records);
    }
    for bucket in runs.values_mut() {
        bucket.records = dedupe_runs(&bucket.records);
        bucket.invalid = unique_lines(&bucket.invalid);
    }
    for (dir, records) in &tasks {
        let original = originals.iter().find(|source| &source.dir == dir);
        let mut staged = original.map(|s| s.tasks.clone()).unwrap_or_default();
        staged.extend(records.clone());
        write_tasks(dir, &mut dedupe_tasks(&staged))?;
    }
    for (dir, bucket) in &runs {
        let original = originals.iter().find(|source| &source.dir == dir);
        let mut staged = original.map(|s| s.runs.clone()).unwrap_or_default();
        staged.extend(bucket.records.clone());
        let mut invalid = original.map(|s| s.invalid.clone()).unwrap_or_default();
        invalid.extend(bucket.invalid.clone());
        write_runs(dir, &dedupe_runs(&staged), &unique_lines(&invalid))?;
    }
    for source in &originals {
        for run in &source.runs {
            let id = text(run, "runId");
            let Some(targets) = run_targets.get(id).filter(|targets| targets.len() == 1) else {
                continue;
            };
            let dest = targets.first().expect("one target");
            if dest == &source.dir {
                continue;
            }
            let filename = format!("{}.events.jsonl", safe_id(id));
            let source_path = source.dir.join("runs").join(&filename);
            let Some(raw) = read_optional(&source_path)? else {
                continue;
            };
            let target_path = dest.join("runs").join(filename);
            let target_raw = read_optional(&target_path)?.unwrap_or_default();
            let lines: Vec<String> = target_raw
                .lines()
                .chain(raw.lines())
                .filter(|line| !line.trim().is_empty())
                .map(str::to_owned)
                .collect();
            let joined = unique_lines(&lines).join("\n") + "\n";
            atomic_write(&target_path, joined.as_bytes())?;
            match fs::remove_file(&source_path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    for (dir, mut records) in tasks {
        write_tasks(&dir, &mut records)?;
    }
    for (dir, bucket) in runs {
        write_runs(&dir, &bucket.records, &bucket.invalid)?;
    }
    atomic_write(
        &home.join("cron/store-migration-v1.json"),
        serde_json::to_vec_pretty(&json!({"version":1,"completedAt":schedule::iso(now_ms)}))?
            .as_slice(),
    )
}

pub(super) fn discover_projects(home: &Path) -> CronResult<Vec<String>> {
    let _lock = global_lock(home, true)?;
    let mut projects = BTreeSet::new();
    for dir in project_dirs(home)? {
        for task in read_tasks(&dir.join("tasks.json"))? {
            if let Some(project) = project_key(&task)? {
                projects.insert(project);
            }
        }
        let (runs, _) = read_migration_runs(&dir.join("run-history.jsonl"))?;
        for run in runs {
            if let Some(project) = project_key(&run)? {
                projects.insert(project);
            }
        }
        if let Some(marker) = read_optional(&dir.join(".cwd"))?
            && !marker.trim().is_empty()
        {
            projects.insert(
                absolute(Path::new(marker.trim()))?
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }
    Ok(projects.into_iter().collect())
}

fn project_dirs(home: &Path) -> CronResult<Vec<PathBuf>> {
    let entries = match fs::read_dir(home.join("cron/projects")) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            paths.push(entry.path());
        }
    }
    paths.sort();
    Ok(paths)
}
fn project_key(value: &Value) -> CronResult<Option<String>> {
    value
        .get("projectKey")
        .and_then(Value::as_str)
        .filter(|key| !key.trim().is_empty())
        .map(|key| absolute(Path::new(key)).map(|key| key.to_string_lossy().into_owned()))
        .transpose()
}
fn target(home: &Path, project: Option<&str>, fallback: &Path) -> CronResult<PathBuf> {
    match project {
        Some(project) => Ok(CronStore::new(home, project)?.project_dir),
        None => Ok(fallback.to_owned()),
    }
}
fn read_migration_runs(path: &Path) -> CronResult<(Vec<Value>, Vec<String>)> {
    let mut records = Vec::new();
    let mut invalid = Vec::new();
    if let Some(raw) = read_optional(path)? {
        for line in raw.lines().filter(|line| !line.trim().is_empty()) {
            match serde_json::from_str::<Value>(line) {
                Ok(record) if record.is_object() => records.push(record),
                _ => invalid.push(line.to_owned()),
            }
        }
    }
    Ok((records, invalid))
}
fn dedupe_tasks(records: &[Value]) -> Vec<Value> {
    let mut result: Vec<Value> = Vec::new();
    for record in records {
        if let Some(index) = result
            .iter()
            .position(|old| old["taskId"] == record["taskId"])
        {
            if text(record, "updatedAt") >= text(&result[index], "updatedAt") {
                result[index] = record.clone();
            }
        } else {
            result.push(record.clone());
        }
    }
    result
}
fn dedupe_runs(records: &[Value]) -> Vec<Value> {
    let mut result: Vec<Value> = Vec::new();
    for record in records {
        let id = record.get("runId").and_then(Value::as_str);
        let old = id.and_then(|id| result.iter().position(|old| text(old, "runId") == id));
        if let Some(index) = old {
            let rank = |run: &Value| {
                (
                    run["finishedAt"].is_string(),
                    run.get("finishedAt")
                        .or_else(|| run.get("startedAt"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                )
            };
            if rank(record) >= rank(&result[index]) {
                result[index] = record.clone();
            }
        } else {
            result.push(record.clone());
        }
    }
    result.sort_by(|a, b| text(a, "startedAt").cmp(text(b, "startedAt")));
    result
}
fn unique_lines(lines: &[String]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    lines
        .iter()
        .filter(|line| seen.insert((*line).clone()))
        .cloned()
        .collect()
}
fn write_runs(dir: &Path, records: &[Value], invalid: &[String]) -> CronResult<()> {
    let mut output = String::new();
    for record in records {
        output.push_str(&serde_json::to_string(record)?);
        output.push('\n');
    }
    for line in invalid {
        output.push_str(line);
        output.push('\n');
    }
    atomic_write(&dir.join("run-history.jsonl"), output.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn terminal_dedupe_beats_later_incomplete_record() {
        let records = dedupe_runs(&[
            json!({"runId":"r","startedAt":"z"}),
            json!({"runId":"r","startedAt":"a","finishedAt":"b","outcome":"failed"}),
        ]);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["outcome"], "failed");
    }
    #[test]
    fn corrupt_source_is_never_silently_emptied() {
        let home = std::env::temp_dir().join(format!("pdrs-cron-corrupt-{}", uuid::Uuid::new_v4()));
        let dir = home.join("cron/projects/legacy");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("tasks.json"), b"{broken").unwrap();
        let error = migrate(&home, 0).unwrap_err();
        assert_eq!(error.code, "cron_store_corrupt");
        assert_eq!(fs::read(dir.join("tasks.json")).unwrap(), b"{broken");
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn migration_moves_records_and_events_before_pruning_and_is_idempotent() {
        let home =
            std::env::temp_dir().join(format!("pdrs-cron-migration-{}", uuid::Uuid::new_v4()));
        let legacy = home.join("cron/projects/legacy");
        fs::create_dir_all(legacy.join("runs")).unwrap();
        let task = |id: &str, project: &str, updated: &str| json!({"schemaVersion":1,"taskId":id,"message":"work","schedule":{"type":"cron","expression":"0 9 * * *"},"status":"scheduled","sessionKey":format!("cron:{id}"),"channelKey":"cron","projectKey":project,"createdAt":"2026-01-01T00:00:00Z","updatedAt":updated,"revision":0});
        let project_a = "/tmp/pdrs-migration-a";
        let project_b = "/tmp/pdrs-migration-b";
        let mut originals = vec![
            task("a", project_a, "2026-01-01T00:00:00Z"),
            task("b", project_b, "2026-01-01T00:00:00Z"),
        ];
        write_tasks(&legacy, &mut originals).unwrap();
        let target_a = CronStore::new(&home, project_a).unwrap();
        let mut newer = task("a", project_a, "2026-02-01T00:00:00Z");
        newer["message"] = json!("newer");
        write_tasks(&target_a.project_dir, &mut [newer]).unwrap();
        let terminal = json!({"schemaVersion":1,"runId":"r","taskId":"a","sessionKey":"cron:a","startedAt":"2026-01-01T00:00:00Z","finishedAt":"2026-01-01T00:01:00Z","outcome":"completed"});
        fs::write(
            legacy.join("run-history.jsonl"),
            format!("{}\nbad-original-line\n", terminal),
        )
        .unwrap();
        fs::write(legacy.join("runs/r.events.jsonl"), "{\"event\":1}\n").unwrap();
        migrate(&home, 0).unwrap();
        assert!(read_tasks(&legacy.join("tasks.json")).unwrap().is_empty());
        assert_eq!(
            read_tasks(&target_a.project_dir.join("tasks.json")).unwrap()[0]["message"],
            "newer"
        );
        assert_eq!(
            read_optional(&legacy.join("run-history.jsonl"))
                .unwrap()
                .unwrap(),
            "bad-original-line\n"
        );
        assert_eq!(
            read_optional(&target_a.project_dir.join("runs/r.events.jsonl"))
                .unwrap()
                .unwrap(),
            "{\"event\":1}\n"
        );
        assert!(!legacy.join("runs/r.events.jsonl").exists());
        migrate(&home, 1).unwrap();
        assert_eq!(
            super::super::store::read_runs(&target_a.project_dir.join("run-history.jsonl"))
                .unwrap(),
            vec![terminal]
        );
        assert_eq!(
            discover_projects(&home).unwrap(),
            vec![project_a.to_owned(), project_b.to_owned()]
        );
        fs::remove_dir_all(home).unwrap();
    }
}
