use super::super::{StatsConfig, TokenStatsCollector, stringify_json};
use super::*;
use pilotdeck_protocol::JsValue;
use std::sync::{Arc, mpsc};

use crate::test_baseline as baseline;

const ORACLE: &str = include_str!("../../../tests/fixtures/token-stats-sidecar/oracle.json");

fn oracle() -> JsValue {
    static FIXTURE: std::sync::OnceLock<JsValue> = std::sync::OnceLock::new();
    FIXTURE.get_or_init(|| baseline::oracle(ORACLE)).clone()
}
fn get<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    value.get(key).unwrap()
}
fn items(value: &JsValue) -> &[JsValue] {
    let JsValue::Array(items) = value else {
        panic!("expected array");
    };
    items
}
fn json(value: &JsValue) -> String {
    stringify_json(value).unwrap().unwrap()
}
fn root() -> PathBuf {
    let root = std::env::temp_dir().join(format!("pdrs-sidecar-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    root
}
fn collector(path: &Path) -> TokenStatsCollector {
    TokenStatsCollector::with_clock(
        Some(&StatsConfig {
            enabled: true,
            file_path: Some(path.to_path_buf()),
            retention_ms: Some(60_000.0),
            model_pricing: None,
            baseline_model: None,
        }),
        None,
        Arc::new(|| 1_772_366_400_500.0),
    )
}
fn rows() -> Vec<JsValue> {
    items(get(get(&oracle(), "expected"), "observed")).to_vec()
}
fn bytes(rows: &[JsValue]) -> Vec<u8> {
    rows.iter()
        .map(|row| format!("{}\n", json(row)))
        .collect::<String>()
        .into_bytes()
}
fn assert_rows(path: &Path, expected: &[JsValue]) {
    let (actual, _) = snapshot(path).expect("readable journal");
    let mut actual: Vec<_> = actual.iter().map(json).collect();
    actual.sort();
    let mut expected: Vec<_> = expected.iter().map(json).collect();
    expected.sort();
    assert_eq!(
        actual, expected,
        "every observation must occur exactly once"
    );
}

#[test]
fn sidecar_snapshots_merge_and_clear_match_real_ts() {
    let root = root();
    let path = root.join("stats.jsonl");
    let fixture = oracle();
    let expected = get(&fixture, "expected");
    let mut writer = collector(&path);
    let lock_path = sibling(&path, ".retention.lock");
    fs::write(&lock_path, b"").unwrap();
    for row in items(get(&fixture, "input")) {
        writer.observe(&mut row.clone());
    }
    assert!(!path.exists(), "fallback must never append main");
    // Reader has no lock and must see the sidecars before any compaction.
    let mut reader = collector(&path);
    assert_eq!(json(&reader.snapshot()), json(get(expected, "snapshot")));
    assert_eq!(
        json(&reader.hourly_snapshots()),
        json(get(expected, "hourly"))
    );
    assert_eq!(
        json(&reader.model_usage_snapshot(None)),
        json(get(expected, "models"))
    );
    assert_eq!(json(&reader.recent(0)), json(get(expected, "recent")));
    let session = reader.session_snapshot(&"row-000".into()).unwrap();
    assert_eq!(
        json(&session.aggregate),
        json(get(get(expected, "session"), "aggregate"))
    );
    fs::remove_file(lock_path).unwrap();
    reader.compact_retained_records();
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        get(expected, "journal")
            .as_string()
            .unwrap()
            .to_utf8()
            .unwrap()
    );
    assert!(pending_names(&path).unwrap().is_empty());
    assert_eq!(
        json(&collector(&path).snapshot()),
        json(get(expected, "snapshot"))
    );
    // Exercise clear both with and without the existing lock-timeout fallback.
    for locked in [false, true] {
        append_pending(&path, &bytes(&rows()[..1])).unwrap();
        if locked {
            fs::write(sibling(&path, ".retention.lock"), b"").unwrap();
        }
        reader.clear();
        assert!(pending_names(&path).unwrap().is_empty());
        assert_eq!(
            json(&collector(&path).snapshot()),
            json(get(&fixture, "cleared"))
        );
        if locked {
            fs::remove_file(sibling(&path, ".retention.lock")).unwrap();
        }
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn sidecar_crash_boundaries_recover_exactly_once() {
    // Identical observations are separate accounting events, never deduped by
    // record contents, timestamps, session/turn ids, or pid.
    for stage in [
        "unpublished",
        "prepared",
        "committed",
        "one-deleted",
        "all-deleted",
        "retired",
    ] {
        let root = root();
        let path = root.join("stats.jsonl");
        let row = rows().remove(0);
        fs::write(&path, bytes(std::slice::from_ref(&row))).unwrap();
        append_pending(&path, &bytes(std::slice::from_ref(&row))).unwrap();
        append_pending(&path, &bytes(std::slice::from_ref(&row))).unwrap();
        let (records, names) = snapshot(&path).unwrap();
        // Incomplete publication/staging has no effect on a reader.
        fs::write(
            sibling(&path, ".pending").join("interrupted.tmp"),
            json(&row).as_bytes(),
        )
        .unwrap();
        if stage != "unpublished" {
            prepare_merge(&path, &bytes(&records), &names).unwrap();
            let merge = receipt(&path).unwrap().unwrap();
            if stage != "prepared" {
                commit_merge(&path, &merge).unwrap();
            }
            if ["one-deleted", "all-deleted", "retired"].contains(&stage) {
                fs::remove_file(sibling(&path, ".pending").join(&names[0])).unwrap();
            }
            if ["all-deleted", "retired"].contains(&stage) {
                fs::remove_file(sibling(&path, ".pending").join(&names[1])).unwrap();
            }
            if stage == "retired" {
                clean_merge(&path, &merge).unwrap();
            }
        }
        let mut expected = vec![row.clone(); 3];
        // A late writer is never included in the old transaction's cleanup.
        append_pending(&path, &bytes(std::slice::from_ref(&row))).unwrap();
        expected.push(row);
        assert_rows(&path, &expected);
        assert_eq!(
            collector(&path)
                .snapshot()
                .get("totalRequests")
                .and_then(JsValue::as_number),
            Some(4.0),
            "{stage}"
        );
        recover(&path).unwrap();
        recover(&path).unwrap();
        assert_rows(&path, &expected);
        collector(&path).compact_retained_records();
        assert_rows(&path, &expected);
        assert!(pending_names(&path).unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn sidecar_recovery_precedes_a_locked_append() {
    for committed in [false, true] {
        let root = root();
        let path = root.join("stats.jsonl");
        let rows = rows();
        append_pending(&path, &bytes(&rows[..1])).unwrap();
        let (records, names) = snapshot(&path).unwrap();
        prepare_merge(&path, &bytes(&records), &names).unwrap();
        if committed {
            commit_merge(&path, &receipt(&path).unwrap().unwrap()).unwrap();
        }
        let mut writer = collector(&path);
        writer.observe(&mut items(get(&oracle(), "input"))[1].clone());
        assert_rows(&path, &rows[..2]);
        recover(&path).unwrap();
        assert_rows(&path, &rows[..2]);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn sidecar_interleaved_compaction_and_independent_writers_never_lose_or_duplicate() {
    for stage in ["snapshot", "prepared", "committed"] {
        for _ in 0..3 {
            let root = root();
            let path = root.join("stats.jsonl");
            let rows = rows();
            fs::write(&path, bytes(&rows[..1])).unwrap();
            let (ready_tx, ready_rx) = mpsc::channel();
            let (resume_tx, resume_rx) = mpsc::channel();
            let compact_path = path.clone();
            let worker = std::thread::spawn(move || {
                let mut compact = collector(&compact_path);
                CHECKPOINT.with_borrow_mut(|hook| {
                    *hook = Some(Box::new(move |at| {
                        if at == stage {
                            ready_tx.send(()).unwrap();
                            resume_rx.recv().unwrap();
                        }
                    }))
                });
                compact.compact_retained_records();
                CHECKPOINT.with_borrow_mut(|hook| *hook = None);
            });
            ready_rx
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap();
            // Four independent instances run while the compactor holds its
            // lock after reading main, deterministically reaching fallback.
            std::thread::scope(|scope| {
                for chunk in items(get(&oracle(), "input"))[1..].chunks(8) {
                    let path = &path;
                    let chunk = chunk.to_vec();
                    scope.spawn(move || {
                        let mut writer = collector(path);
                        for mut row in chunk {
                            writer.observe(&mut row);
                        }
                    });
                }
            });
            assert_rows(&path, &rows);
            resume_tx.send(()).unwrap();
            worker.join().unwrap();
            assert_rows(&path, &rows);
            collector(&path).compact_retained_records();
            assert_rows(&path, &rows);
            assert!(pending_names(&path).unwrap().is_empty());
            fs::remove_dir_all(root).unwrap();
        }
    }
}

#[test]
fn sidecar_reader_retries_publication_commit_and_receipt_retirement() {
    for stage in ["publication", "commit", "cleanup", "whole-merge"] {
        let root = root();
        let path = root.join("stats.jsonl");
        let expected = rows()[..2].to_vec();
        fs::write(&path, bytes(&expected[..1])).unwrap();
        append_pending(&path, &bytes(&expected[1..])).unwrap();
        let (records, names) = snapshot(&path).unwrap();
        if ["commit", "cleanup"].contains(&stage) {
            prepare_merge(&path, &bytes(&records), &names).unwrap();
        }
        if stage == "cleanup" {
            commit_merge(&path, &receipt(&path).unwrap().unwrap()).unwrap();
        }
        let checkpoint_path = path.clone();
        let mut fired = false;
        CHECKPOINT.with_borrow_mut(|hook| {
            *hook = Some(Box::new(move |at| {
                if at
                    != (if stage == "commit" {
                        "read-receipt"
                    } else {
                        "read-main"
                    })
                    || fired
                {
                    return;
                }
                fired = true;
                match stage {
                    "publication" => {
                        prepare_merge(&checkpoint_path, &bytes(&records), &names).unwrap()
                    }
                    "commit" => commit_merge(
                        &checkpoint_path,
                        &receipt(&checkpoint_path).unwrap().unwrap(),
                    )
                    .unwrap(),
                    "cleanup" => clean_merge(
                        &checkpoint_path,
                        &receipt(&checkpoint_path).unwrap().unwrap(),
                    )
                    .unwrap(),
                    "whole-merge" => {
                        prepare_merge(&checkpoint_path, &bytes(&records), &names).unwrap();
                        recover(&checkpoint_path).unwrap();
                    }
                    _ => unreachable!(),
                }
            }))
        });
        assert_rows(&path, &expected);
        CHECKPOINT.with_borrow_mut(|hook| *hook = None);
        recover(&path).unwrap();
        assert_rows(&path, &expected);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn sidecar_expiration_and_journal_format_stay_equal_to_real_ts() {
    let root = root();
    let path = root.join("stats.jsonl");
    let fixture = oracle();
    let before = get(get(&fixture, "tsRace"), "beforeRename")
        .as_string()
        .unwrap()
        .to_utf8()
        .unwrap();
    let expired = parse_stats_records(&before).remove(0);
    append_pending(&path, &bytes(&[expired])).unwrap();
    append_pending(&path, &bytes(&rows()[..1])).unwrap();
    let mut reader = collector(&path); // expired row triggers real compaction
    assert_eq!(
        reader
            .snapshot()
            .get("totalRequests")
            .and_then(JsValue::as_number),
        Some(1.0)
    );
    assert_rows(&path, &rows()[..1]);
    assert_eq!(fs::read(&path).unwrap(), bytes(&rows()[..1]));
    assert!(pending_names(&path).unwrap().is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[test]
#[ignore = "runs the real-TS generator; requires PDRS_BASELINE"]
fn sidecar_fixture_regenerates_identically() {
    baseline::regenerates_identically("token-stats-sidecar");
}

#[test]
fn sidecar_failed_recovery_still_persists_the_new_observation() {
    let root = root();
    let path = root.join("stats.jsonl");
    let mut writer = collector(&path);
    let rows = rows();
    append_pending(&path, &bytes(&rows[..1])).unwrap();
    let (records, names) = snapshot(&path).unwrap();
    prepare_merge(&path, &bytes(&records), &names).unwrap();
    // A filesystem failure at rename must not turn a successful lock into a
    // discarded new observation. Repair only our injected filesystem error.
    fs::create_dir(&path).unwrap();
    writer.observe(&mut items(get(&oracle(), "input"))[1].clone());
    fs::remove_dir(&path).unwrap();
    recover(&path).unwrap();
    assert_rows(&path, &rows[..2]);
    fs::remove_dir_all(root).unwrap();
}
