//! Only bounded child workers change process cwd; the parent retains scratch ownership.
use super::*;
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;

const ORACLE: &str = include_str!("oracle.json");
const WORKER: &str = "storage::file_history::cwd_tests::environment_worker";

fn matrix(name: &str) {
    crate::test_baseline::oracle(ORACLE);
    let scratch = crate::test_scratch::Scratch::new("file-history-cwd");
    let root = fs::canonicalize(scratch.path()).expect("#879 setup canonical scratch");
    fs::create_dir(root.join("cwd")).expect("#879 setup cwd directory");
    fs::create_dir(root.join("files")).expect("#879 setup files directory");
    fs::write(root.join("files/tracked.txt"), "original\n").expect("#879 setup target contents");
    fs::set_permissions(
        root.join("files/tracked.txt"),
        fs::Permissions::from_mode(0o640),
    )
    .expect("#879 setup target mode");
    crate::test_process::run_cases(
        WORKER,
        vec![(
            name.into(),
            vec![
                ("PDRS_FILE_HISTORY_CWD_CASE".into(), name.into()),
                (
                    "PDRS_FILE_HISTORY_CWD_ROOT".into(),
                    root.to_str().expect("#879 setup UTF-8 root").into(),
                ),
            ],
        )],
    );
}

fn view(store: &FileHistoryStore, records: &Mutex<Vec<String>>, root: &str) -> Value {
    let state = store.state();
    let file = format!("{root}/files/tracked.txt");
    let snapshots: Vec<_> = state.snapshots.iter().map(|snapshot| {
        let backups: Vec<_> = snapshot.tracked_file_backups.iter().map(|(path, backup)| {
            let name = crate::test_negative_receipt::expect_some(backup.backup_file_name.as_name(), "#879 state named backup");
            json!({
                "path": path, "nameMatches": name == get_backup_file_name(&file, 1),
                "version": backup.version, "backupTime": backup.backup_time_ms,
                "mode": backup.mode.expect("#879 state backup mode") & 0o777,
                "content": fs::read_to_string(store.backup_dir().join(name)).expect("#879 state backup content"),
            })
        }).collect();
        let expected: Vec<_> = snapshot.expected_file_states.iter().map(|(path, state)| {
            let state = match state {
                ExpectedFileState::Missing => json!({"exists": false}),
                ExpectedFileState::Present { sha256, mode } => json!({"exists": true, "sha256": sha256, "mode": mode}),
            };
            json!({"path": path, "state": state})
        }).collect();
        json!({"messageId": snapshot.message_id, "timestamp": snapshot.timestamp_ms,
            "backups": backups, "expected": expected})
    }).collect();
    let actual = json!({"trackedFiles": state.tracked_files, "snapshots": snapshots,
        "records": *records.lock().expect("#879 state records lock")});
    serde_json::from_str(&actual.to_string().replace(root, "<root>"))
        .expect("#879 state normalized JSON")
}

#[test]
#[ignore = "worker only; parent supplies isolated cwd and checks one receipt"]
fn environment_worker() {
    let name = std::env::var("PDRS_FILE_HISTORY_CWD_CASE").expect("#879 setup parent case");
    let root = std::env::var("PDRS_FILE_HISTORY_CWD_ROOT").expect("#879 setup parent scratch");
    let oracle: Value = serde_json::from_str(ORACLE).expect("#879 setup oracle JSON");
    let rows = oracle["cases"].as_array().expect("#879 setup oracle cases");
    let row = rows
        .iter()
        .find(|row| row["name"] == name)
        .expect("#879 setup oracle row");
    let records = Arc::new(Mutex::new(Vec::new()));
    let sink = records.clone();
    let mut options = FileHistoryOptions::new(Path::new(&root).join("backups"));
    options.now = Some(Arc::new(|| 1_700_000_000_000));
    options.on_snapshot_recorded = Some(Arc::new(move |_, kind| {
        sink.lock()
            .expect("#879 state transcript lock")
            .push(kind.into());
        Ok(())
    }));
    let store = FileHistoryStore::new(options);
    let mark = name.starts_with("mark-");
    if mark && !name.contains("-missing-") {
        store
            .track_edit(&format!("{root}/files/tracked.txt"), "message")
            .expect("#879 setup preseed absolute track");
    }
    assert_eq!(
        view(&store, &records, &root),
        row["before"],
        "#879 setup initial state: {name}"
    );
    let input = if name.contains("-absolute-") {
        format!("{root}//files/nested/../tracked.txt")
    } else {
        "../files/./nested/../tracked.txt".into()
    };
    let cwd = Path::new(&root).join("cwd");
    let original_cwd = std::env::current_dir().expect("#879 setup original cwd");
    std::env::set_current_dir(&cwd).expect("#879 setup enter cwd");
    let warmed = name.contains("-warmed-");
    if warmed {
        std::env::current_dir().expect("#879 setup warm cwd");
    }
    if name.ends_with("-deleted") {
        fs::remove_dir(&cwd).expect("#879 setup remove cwd");
        assert!(
            std::env::current_dir().is_err(),
            "#879 setup deleted cwd must fail"
        );
    }
    let result = if mark {
        store.mark_edit_committed(&input, "message")
    } else {
        store.track_edit(&input, "message")
    };
    // Restore before assertions, even if the product returned an unexpected result.
    std::env::set_current_dir(original_cwd).expect("#879 setup restore cwd");
    let actual = match result {
        Ok(()) => json!({"ok": true}),
        Err(FileHistoryError::Fs(message)) => json!({"error": {"message": message}}),
        Err(error) => panic!("#879 outcome error must use Node diagnostic: {name}: {error:?}"),
    };
    let expected = if warmed {
        // Existing Node cwd cache divergence: A6.6-FILEHISTORY-CWD-001.
        let cold_name = name.replace("-warmed", "");
        let cold = rows
            .iter()
            .find(|row| row["name"] == cold_name)
            .expect("#879 setup cold oracle row");
        assert_eq!(
            row["outcome"],
            json!({"ok": true}),
            "#879 setup warm TS success"
        );
        json!({"error": {"message": cold["outcome"]["error"]["message"]}})
    } else if row["outcome"].get("error").is_some() {
        json!({"error": {"message": row["outcome"]["error"]["message"]}})
    } else {
        row["outcome"].clone()
    };
    crate::negative_assert_eq!(actual, expected, "#879 outcome matrix: {name}");
    let expected_state = if warmed {
        &row["before"]
    } else {
        &row["after"]
    };
    crate::negative_assert_eq!(
        view(&store, &records, &root),
        *expected_state,
        "#879 state matrix: {name}"
    );
    crate::test_process::receipt(&name);
}

macro_rules! case {
    ($test:ident, $name:literal) => {
        #[test]
        fn $test() {
            matrix($name);
        }
    };
}
case!(track_relative_normal, "track-relative-normal");
case!(track_relative_deleted, "track-relative-deleted");
case!(track_absolute_normal, "track-absolute-normal");
case!(track_absolute_deleted, "track-absolute-deleted");
case!(mark_relative_normal, "mark-relative-normal");
case!(mark_relative_deleted, "mark-relative-deleted");
case!(mark_absolute_normal, "mark-absolute-normal");
case!(mark_absolute_deleted, "mark-absolute-deleted");
case!(mark_missing_relative_normal, "mark-missing-relative-normal");
case!(
    mark_missing_relative_deleted,
    "mark-missing-relative-deleted"
);
case!(
    track_relative_warmed_deleted,
    "track-relative-warmed-deleted"
);
case!(mark_relative_warmed_deleted, "mark-relative-warmed-deleted");

#[test]
#[ignore = "requires Node v22.22.0 and PDRS_BASELINE"]
fn file_history_cwd_regenerates_identically() {
    crate::test_baseline::regenerates_identically("file-history-cwd");
}
