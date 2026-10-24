//! Explicit TS baseline execution, including the 72 #19 cases (five repeats).
//! This reuses the pre-registered independent observers and negative controls;
//! it does not replace measured TS outcomes with a desired Rust timeout policy.
use serde_json::Value;
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

fn read(path: impl AsRef<Path>) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}
fn complete(manifest: &Value, cases: usize, measurements: usize) {
    let expected = manifest["expectedCases"].as_array().unwrap();
    let executed = manifest["executedCases"].as_array().unwrap();
    assert_eq!(expected.len(), cases);
    assert_eq!(
        expected, executed,
        "no missing, extra, duplicate or implicit skip cases"
    );
    assert_eq!(
        expected
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect::<HashSet<_>>()
            .len(),
        cases
    );
    assert_eq!(manifest["repeats"], 5);
    assert_eq!(
        manifest
            .get("totalMeasurements")
            .or_else(|| manifest.get("measurements"))
            .unwrap(),
        measurements
    );
    if let Some(unstable) = manifest.get("unstableGroups") {
        assert_eq!(unstable, &serde_json::json!([]));
    }
}

fn controls(controls: &Value, count: usize) {
    let controls = controls.as_array().unwrap();
    assert_eq!(controls.len(), count);
    assert_eq!(
        controls
            .iter()
            .map(|c| c["name"].as_str().unwrap())
            .collect::<HashSet<_>>()
            .len(),
        count
    );
    for control in controls {
        assert_eq!(control["detected"], true);
        assert_eq!(
            &control["hitRule"],
            control
                .get("expectedRule")
                .or_else(|| control.get("rule"))
                .unwrap()
        );
    }
}

#[test]
#[ignore = "requires the pinned TS checkout, Node 22 and explicit A52_EVIDENCE_DIR; runs real processes and loopback WS"]
fn ts_six_cells_concurrency_restart_and_sidecar() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let baseline = PathBuf::from(std::env::var_os("PDRS_BASELINE").expect("set PDRS_BASELINE"));
    let output =
        PathBuf::from(std::env::var_os("A52_EVIDENCE_DIR").expect("set a fresh A52_EVIDENCE_DIR"));
    // Refuse reuse: evidence from a previous run must never satisfy this test.
    fs::create_dir(&output).expect("evidence directory must not already exist");
    for (name, script, args) in [
        ("six-cells", "run.py", vec![]),
        ("concurrency", "run-b.py", vec![]),
        ("production-close", "run-production-b.py", vec![]),
        ("sidecar", "run-b.py", vec!["--sidecar"]),
        (
            "cold",
            "run-external-b.py",
            vec!["--coordinator", "--suite", "cold"],
        ),
        (
            "ws",
            "run-external-b.py",
            vec!["--coordinator", "--suite", "ws", "--port", "48789"],
        ),
    ] {
        let directory = output.join(name);
        let result = Command::new("python3")
            .arg(root.join("scripts/a5-19").join(script))
            .args(args)
            .arg("--baseline")
            .arg(&baseline)
            .arg("--output")
            .arg(&directory)
            .output()
            .unwrap();
        fs::write(output.join(format!("{name}.stdout")), &result.stdout).unwrap();
        fs::write(output.join(format!("{name}.stderr")), &result.stderr).unwrap();
        assert!(
            result.status.success(),
            "{name} failed; raw evidence: {}\n{}",
            directory.display(),
            String::from_utf8_lossy(&result.stderr)
        );
        let launch = read(directory.join("launch.json"));
        if name == "cold" {
            let manifest = read(directory.join("manifest.json"));
            assert_eq!(manifest["summaries"].as_array().unwrap().len(), 10);
            let children = launch["ownedChildren"].as_array().unwrap();
            assert_eq!(children.len(), 20);
            assert!(children.iter().all(|v| v["reaped"] == true));
            for pair in children.chunks_exact(2) {
                assert_ne!(pair[0]["pid"], pair[1]["pid"]);
                assert_eq!(pair[0]["pilotHome"], pair[1]["pilotHome"]);
            }
        } else {
            assert_eq!(launch["exitCode"], 0);
            assert_eq!(
                launch[if name == "ws" {
                    "reaped"
                } else {
                    "childReaped"
                }],
                true
            );
            let manifest = read(directory.join("raw/manifest.json"));
            match name {
                "six-cells" => {
                    complete(&manifest, 72, 360);
                    controls(&manifest["checks"]["observerControls"], 9);
                    assert_eq!(
                        manifest["baselineCommit"],
                        "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85"
                    );
                    for cell in [
                        "no-registry-fast",
                        "no-registry-slow",
                        "no-registry-failure",
                        "registry-fast",
                        "registry-slow",
                        "registry-failure",
                    ] {
                        let raw = read(directory.join("raw").join(format!("{cell}.json")));
                        assert!(
                            !raw["observations"].as_array().unwrap().is_empty(),
                            "{cell}"
                        );
                    }
                    assert_eq!(
                        manifest["checks"]["diskRereadAgreesWithFreshRegistry"],
                        true
                    );
                    assert_eq!(manifest["checks"]["baselineHashesUnchanged"], true);
                }
                "concurrency" => {
                    complete(&manifest, 34, 170);
                    controls(&manifest["controls"], 10);
                }
                "production-close" => {
                    complete(&manifest, 8, 40);
                    assert!(
                        manifest["summaries"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .all(|s| s["runs"] == 5 && s["distinctOutcomes"] == 1)
                    );
                }
                "ws" => {
                    complete(&manifest, 15, 75);
                    assert_eq!(manifest["host"], "127.0.0.1");
                    assert_eq!(
                        manifest["bridgeReconnect"],
                        "NOT_RUN: requires original bridge, separate D5 gate"
                    );
                }
                "sidecar" => {
                    assert_eq!(manifest["expectedCases"].as_array().unwrap().len(), 4);
                    assert_eq!(manifest["measurements"], 20);
                    assert_eq!(manifest["stable"], true);
                    assert!(
                        manifest["groups"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .all(|g| g["measurements"] == 5 && g["stable"] == true)
                    );
                }
                _ => unreachable!(),
            }
        }
    }
}
