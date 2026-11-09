use super::AlwaysOnError;
use serde_json::{Value, json};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct Store {
    pub home: PathBuf,
    pub project: PathBuf,
    pub root: PathBuf,
    pub worktrees: PathBuf,
    pub snapshots: PathBuf,
    gate: Arc<Mutex<()>>,
}

pub fn safe_id(value: &str) -> String {
    let mut out = String::new();
    let mut replacing = false;
    for c in value.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            out.push(c);
            replacing = false;
        } else if !replacing {
            out.push('-');
            replacing = true;
        }
    }
    let out = out.trim_matches('-');
    if out.is_empty() {
        "unnamed".into()
    } else {
        out.into()
    }
}

pub fn absolute(path: &Path) -> Result<PathBuf, AlwaysOnError> {
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            part => out.push(part.as_os_str()),
        }
    }
    Ok(out)
}

impl Store {
    pub fn new(home: &Path, project: &Path) -> Result<Self, AlwaysOnError> {
        let project = absolute(project)?;
        let home = absolute(home)?;
        let chats = crate::session_chat_dir(&home, &project, &project.to_string_lossy());
        let project_id = chats
            .parent()
            .and_then(Path::file_name)
            .ok_or_else(|| AlwaysOnError::new("storage_failed", "Invalid project storage path"))?;
        let root = home.join("always-on");
        Ok(Self {
            home,
            project,
            root: root.join("projects").join(project_id),
            worktrees: root.join("worktrees").join(project_id),
            snapshots: root.join("snapshots").join(project_id),
            gate: Arc::new(Mutex::new(())),
        })
    }

    pub fn read(&self, relative: &str) -> Result<Option<Value>, AlwaysOnError> {
        read_json(&self.root.join(relative))
    }

    pub fn write(&self, relative: &str, value: &Value) -> Result<(), AlwaysOnError> {
        write_atomic(
            &self.root.join(relative),
            serde_json::to_vec_pretty(value)?.as_slice(),
        )
    }

    pub fn index(&self, kind: &str) -> Result<Value, AlwaysOnError> {
        let value = self
            .read(&format!("{kind}/index.json"))?
            .unwrap_or_else(|| json!({"schemaVersion":1, kind: []}));
        if value["schemaVersion"] != 1 || !value[kind].is_array() {
            return Err(AlwaysOnError::new(
                "storage_corrupt",
                format!("Invalid {kind} index"),
            ));
        }
        Ok(value)
    }

    pub fn record(&self, kind: &str, id: &str) -> Result<Option<Value>, AlwaysOnError> {
        Ok(self.index(kind)?[kind]
            .as_array()
            .and_then(|rows| rows.iter().find(|r| r["id"] == id))
            .cloned())
    }

    pub fn update_record(
        &self,
        kind: &str,
        id: &str,
        update: Value,
    ) -> Result<Value, AlwaysOnError> {
        let _lock = self.gate.lock().unwrap_or_else(|e| e.into_inner());
        let mut index = self.index(kind)?;
        let rows = index[kind]
            .as_array_mut()
            .ok_or_else(|| AlwaysOnError::new("storage_corrupt", "Invalid index"))?;
        let row = rows.iter_mut().find(|row| row["id"] == id).ok_or_else(|| {
            AlwaysOnError::new("record_not_found", format!("{kind} record {id} not found"))
        })?;
        let object = row
            .as_object_mut()
            .ok_or_else(|| AlwaysOnError::new("storage_corrupt", "Invalid record"))?;
        for (key, value) in update.as_object().into_iter().flatten() {
            let mut value = value.clone();
            if key == "reportFilePath"
                && let Some(path) = value.as_str()
                && let Ok(relative) = Path::new(path).strip_prefix(&self.root)
            {
                value = Value::String(relative.to_string_lossy().into_owned());
            }
            object.insert(key.clone(), value.clone());
        }
        if object.contains_key("workCycleId") {
            object.remove("workspace");
        }
        if object.contains_key("executionStatus")
            && let Some(status) = object.get("status").cloned().filter(|v| {
                matches!(
                    v.as_str(),
                    Some("completed" | "completed_no_report" | "failed")
                )
            })
        {
            object.insert("executionStatus".into(), status);
        }
        let row = row.clone();
        self.write(&format!("{kind}/index.json"), &index)?;
        Ok(row)
    }

    pub fn insert_record(&self, kind: &str, record: Value) -> Result<(), AlwaysOnError> {
        let _lock = self.gate.lock().unwrap_or_else(|e| e.into_inner());
        let mut index = self.index(kind)?;
        let rows = index[kind]
            .as_array_mut()
            .ok_or_else(|| AlwaysOnError::new("storage_corrupt", "Invalid index"))?;
        if let Some(old) = rows.iter_mut().find(|row| row["id"] == record["id"]) {
            *old = record;
        } else {
            rows.push(record);
        }
        self.write(&format!("{kind}/index.json"), &index)
    }

    pub fn add_plan(&self, cycle_id: &str, plan_id: &str) -> Result<(), AlwaysOnError> {
        let _lock = self.gate.lock().unwrap_or_else(|e| e.into_inner());
        let mut index = self.index("cycles")?;
        let cycle = index["cycles"]
            .as_array_mut()
            .and_then(|rows| rows.iter_mut().find(|r| r["id"] == cycle_id))
            .ok_or_else(|| AlwaysOnError::new("cycle_not_found", cycle_id))?;
        let ids = cycle["planIds"]
            .as_array_mut()
            .ok_or_else(|| AlwaysOnError::new("storage_corrupt", "Invalid cycle planIds"))?;
        if !ids.iter().any(|id| id == plan_id) {
            ids.push(json!(plan_id));
        }
        self.write("cycles/index.json", &index)
    }

    pub fn state(&self, now: &str) -> Result<Value, AlwaysOnError> {
        let day = now.get(..10).filter(|day| day.is_ascii()).ok_or_else(|| {
            AlwaysOnError::new("invalid_timestamp", "Expected an ISO date timestamp")
        })?;
        let mut state = self.read("state.json")?.unwrap_or_else(
            || json!({"schemaVersion":1,"todayKey":day,"todayRunCount":0,"consecutiveFailures":0}),
        );
        if state["schemaVersion"] != 1 || !state.is_object() {
            return Err(AlwaysOnError::new(
                "storage_corrupt",
                "Invalid discovery state",
            ));
        }
        let candidate = state;
        state = json!({"schemaVersion":1,"todayKey":candidate["todayKey"].as_str().unwrap_or(day),"todayRunCount":candidate["todayRunCount"].as_f64().filter(|n|*n>=0.0).unwrap_or(0.0),"consecutiveFailures":candidate["consecutiveFailures"].as_f64().filter(|n|*n>=0.0).unwrap_or(0.0)});
        // Keep integral counters integral on disk; legacy fractional counters remain valid.
        for key in ["todayRunCount", "consecutiveFailures"] {
            if candidate[key].as_u64().is_some() {
                state[key] = candidate[key].clone();
            }
        }
        for key in [
            "lastFireStartedAt",
            "lastFireCompletedAt",
            "lastPlanId",
            "lastRunId",
            "activeWorkCycleId",
        ] {
            if candidate[key].is_string() {
                state[key] = candidate[key].clone();
            }
        }
        if matches!(
            candidate["lastFireOutcome"].as_str(),
            Some("executed" | "no_plan" | "failed" | "aborted")
        ) {
            state["lastFireOutcome"] = candidate["lastFireOutcome"].clone();
        }
        let dormant = &candidate["dormant"];
        if dormant["since"].is_string() && dormant["lastBaselineAt"].is_string() {
            state["dormant"] =
                json!({"since":dormant["since"],"lastBaselineAt":dormant["lastBaselineAt"]});
            if dormant["lastChangeAt"].is_string() {
                state["dormant"]["lastChangeAt"] = dormant["lastChangeAt"].clone();
            }
        }
        let workspace = &candidate["currentWorkspace"];
        if ["runId", "cwd"]
            .iter()
            .all(|k| workspace[*k].as_str().is_some_and(|v| !v.is_empty()))
            && matches!(
                workspace["strategy"].as_str(),
                Some("git-worktree" | "snapshot-copy")
            )
        {
            let metadata: serde_json::Map<String, Value> = workspace["metadata"]
                .as_object()
                .into_iter()
                .flatten()
                .filter(|(_, v)| v.is_string())
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            state["currentWorkspace"] = json!({"runId":workspace["runId"],"cwd":workspace["cwd"],"strategy":workspace["strategy"],"metadata":metadata});
        }
        if state["todayKey"] != day {
            state["todayKey"] = json!(day);
            state["todayRunCount"] = json!(0);
        }
        Ok(state)
    }

    pub fn update_state(&self, now: &str, update: Value) -> Result<Value, AlwaysOnError> {
        let _lock = self.gate.lock().unwrap_or_else(|e| e.into_inner());
        let mut state = self.state(now)?;
        for (key, value) in update.as_object().into_iter().flatten() {
            if value.is_null() {
                state.as_object_mut().expect("state checked").remove(key);
            } else {
                state[key] = value.clone();
            }
        }
        self.write("state.json", &state)?;
        Ok(state)
    }

    pub fn mark_started(&self, now: &str, run: &str) -> Result<(), AlwaysOnError> {
        let _lock = self.gate.lock().unwrap_or_else(|e| e.into_inner());
        let mut state = self.state(now)?;
        state["todayRunCount"] = json!(state["todayRunCount"].as_f64().unwrap_or(0.0) + 1.0);
        state["lastFireStartedAt"] = json!(now);
        state["lastRunId"] = json!(run);
        state.as_object_mut().expect("checked").remove("dormant");
        self.write("state.json", &state)
    }

    pub fn complete(
        &self,
        now: &str,
        run: &str,
        plan: &str,
        outcome: &str,
    ) -> Result<(), AlwaysOnError> {
        let _lock = self.gate.lock().unwrap_or_else(|e| e.into_inner());
        let mut state = self.state(now)?;
        let failures = if outcome == "failed" {
            state["consecutiveFailures"]
                .as_u64()
                .unwrap_or(0)
                .saturating_add(1)
        } else {
            0
        };
        state["lastFireCompletedAt"] = json!(now);
        state["lastRunId"] = json!(run);
        if plan.is_empty() {
            state
                .as_object_mut()
                .expect("state checked")
                .remove("lastPlanId");
        } else {
            state["lastPlanId"] = json!(plan);
        }
        state["lastFireOutcome"] = json!(outcome);
        state["consecutiveFailures"] = json!(failures);
        self.write("state.json", &state)
    }

    pub fn append(&self, relative: &str, event: &Value) -> Result<(), AlwaysOnError> {
        let _lock = self.gate.lock().unwrap_or_else(|e| e.into_inner());
        let path = self.root.join(relative);
        fs::create_dir_all(path.parent().expect("file has parent"))?;
        let mut line = serde_json::to_vec(event)?;
        line.push(b'\n');
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?
            .write_all(&line)?;
        Ok(())
    }

    pub fn plan_body(&self, id: &str) -> Result<Option<String>, AlwaysOnError> {
        match fs::read_to_string(self.root.join("plans").join(format!("{}.md", safe_id(id)))) {
            Ok(body) => Ok(Some(body)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn report(&self, id: &str, body: &str) -> Result<PathBuf, AlwaysOnError> {
        let path = self
            .root
            .join("reports")
            .join(format!("{}.md", safe_id(id)));
        write_atomic(&path, body.as_bytes())?;
        Ok(path)
    }
}

fn read_json(path: &Path) -> Result<Option<Value>, AlwaysOnError> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| AlwaysOnError::new("storage_corrupt", format!("{}: {e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), AlwaysOnError> {
    fs::create_dir_all(
        path.parent()
            .ok_or_else(|| AlwaysOnError::new("storage_failed", "File has no parent"))?,
    )?;
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(Into::into)
}
