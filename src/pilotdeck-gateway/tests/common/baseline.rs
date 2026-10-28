//! Provenance checks shared by unit and integration fixture replays.
use pilotdeck_protocol::{JsValue, parse_json};
use sha2::{Digest, Sha256};
use std::{fs, path::Path, process::Command};

#[allow(dead_code)]
#[path = "regeneration_masks.rs"]
pub(crate) mod regeneration_masks;

const BASELINE_COMMIT: &str = "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85";

fn text(value: &JsValue) -> String {
    value
        .as_string()
        .expect("provenance string")
        .to_utf8()
        .expect("#988 provenance string must be valid UTF-8")
}

pub(crate) fn oracle(json: &str) -> JsValue {
    let oracle = parse_json(json).expect("oracle.json must be valid JSON");
    let baseline = std::env::var_os("PDRS_BASELINE");
    check(&oracle, baseline.as_deref().map(Path::new));
    oracle
}

pub(crate) fn check(oracle: &JsValue, baseline: Option<&Path>) {
    // Older corpora use commit/baselineCommit and sources. Keep their bytes and
    // schema intact while applying the same checks to every replay.
    let commit = ["sourceCommit", "baselineCommit", "commit"]
        .into_iter()
        .find_map(|key| oracle.get(key))
        .expect("oracle is missing its baseline commit");
    assert_eq!(text(commit), BASELINE_COMMIT, "oracle baseline commit");
    let hashes = oracle
        .get("sourceSha256")
        .or_else(|| oracle.get("sources"))
        .expect("oracle is missing sourceSha256/sources")
        .as_object()
        .expect("oracle source hashes must be an object");
    assert!(!hashes.is_empty(), "oracle source hashes must not be empty");
    if let Some(baseline) = baseline {
        let head = git(baseline, &["rev-parse", "HEAD"]);
        assert_eq!(
            String::from_utf8_lossy(&head).trim(),
            BASELINE_COMMIT,
            "PDRS_BASELINE must be checked out at the fixed commit"
        );
        let tracked = git(baseline, &["status", "--porcelain", "--untracked-files=no"]);
        assert!(
            tracked.is_empty(),
            "PDRS_BASELINE has uncommitted changes to tracked files:\n{}",
            String::from_utf8_lossy(&tracked)
        );
    }
    for (source, recorded) in hashes.entries() {
        let source = source.to_utf8().expect("UTF-8 source path");
        assert!(
            source.starts_with("src/") && source.split('/').all(|p| p != ".." && !p.is_empty()),
            "expected a baseline-relative source path: {source}"
        );
        let recorded = text(recorded);
        assert!(
            recorded.len() == 64 && recorded.bytes().all(|b| b.is_ascii_hexdigit()),
            "missing or malformed hash for {source}"
        );
        if let Some(baseline) = baseline {
            let committed = git(baseline, &["show", &format!("{BASELINE_COMMIT}:{source}")]);
            let expected = format!("{:x}", Sha256::digest(committed));
            assert_eq!(
                recorded, expected,
                "oracle hash for {source} does not match {BASELINE_COMMIT}"
            );
            let working =
                fs::read(baseline.join(&source)).unwrap_or_else(|e| panic!("{source}: {e}"));
            assert_eq!(
                format!("{:x}", Sha256::digest(working)),
                expected,
                "baseline source {source} differs from {BASELINE_COMMIT}"
            );
        }
    }
}

fn git(baseline: &Path, args: &[&str]) -> Vec<u8> {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let output = command
        .args(["--no-replace-objects", "-C"])
        .arg(baseline)
        .args(args)
        .output()
        .expect("run git against PDRS_BASELINE");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

/// Node generators share one environment contract, even on LC_* hosts.
#[allow(dead_code)]
pub(crate) fn node_command() -> Command {
    let mut command = Command::new("node");
    command
        .env("LANG", "en_US.UTF-8")
        .env_remove("LC_ALL")
        .env_remove("LC_MESSAGES");
    command
}

/// Re-runs `tests/fixtures/<fixture>/generate.mts` against the real TS
/// baseline (`PDRS_BASELINE`) into a fresh directory and requires a
/// byte-identical `oracle.json`, except the exact clock/PID paths documented
/// in `_shared/regeneration-masks.tsv`. Callers are `#[ignore]` tests.
#[allow(dead_code)]
pub(crate) fn regenerates_identically(fixture: &str) {
    let baseline = std::path::PathBuf::from(
        std::env::var("PDRS_BASELINE").expect("#280 set PDRS_BASELINE to the PilotDeck checkout"),
    );
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(fixture);
    // Every invocation gets its own directory; nothing outside it is removed.
    let scratch = std::env::temp_dir().join(format!("pdrs-{fixture}-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&scratch).expect("#280 create regeneration scratch");
    let generator_output = if fixture == "interaction-permission" {
        scratch.join("output")
    } else {
        scratch.clone()
    };
    let node_flags: &[&str] = match fixture {
        "model-stream-google" => &["--stack-size=8192"],
        "session-overrides" => &["--expose-gc"],
        _ => &[],
    };
    let extra_args = if fixture == "private-ca" {
        vec![scratch.join("work")]
    } else {
        vec![]
    };
    let output = node_command()
        .args(node_flags)
        .arg("--disable-warning=ExperimentalWarning")
        .arg("--import")
        .arg(baseline.join("node_modules/tsx/dist/loader.mjs"))
        .arg(fixtures.join("generate.mts"))
        .arg(&generator_output)
        .args(extra_args)
        .env("PDRS_BASELINE", &baseline)
        .env(
            "PDRS_GIT",
            std::env::var_os("PDRS_GIT").unwrap_or_else(|| "/usr/bin/git".into()),
        )
        .output()
        .expect("#280 run TS generator");
    assert!(
        output.status.success(),
        "#280 {fixture}: generator failed\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    println!(
        "#280 {fixture} stdout:\n{}stderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let generated = generated_oracle_bytes(fixture, &scratch, &output.stdout)
        .unwrap_or_else(|error| panic!("#280 {fixture}: {error}"));
    let committed = fs::read(fixtures.join("oracle.json")).expect("#280 read committed oracle");
    // Keep the scratch on mismatch so the differing bytes can be inspected.
    regeneration_masks::compare(fixture, "oracle.json", &generated, &committed, (None, None))
        .unwrap_or_else(|error| {
            panic!(
                "#280 {fixture}/oracle.json differs from the regenerated {}: {error}",
                scratch.join("oracle.json").display()
            )
        });
    compare_companion_files(fixture, &generator_output, &fixtures)
        .unwrap_or_else(|error| panic!("#280 {fixture}: {error}"));
    assert!(
        !scratch.join("oracle.json.tmp").exists(),
        "#280 generator left a temporary file behind"
    );
    fs::remove_dir_all(&scratch).expect("#280 remove owned regeneration scratch");
}

/// Additional generated artifacts consumed by replay tests must not silently
/// escape the byte comparison when the primary oracle happens to match.
#[allow(dead_code)]
pub(crate) fn compare_companion_files(
    fixture: &str,
    generated: &Path,
    committed: &Path,
) -> Result<(), String> {
    let names = companion_names(fixture, committed)?;
    let pids = if fixture == "run-registry" {
        let read_pid = |root: &Path| -> Result<Option<u32>, String> {
            let bytes = fs::read(root.join("oracle.json"))
                .map_err(|error| format!("#280 read PID manifest: {error}"))?;
            regeneration_masks::pid(&bytes).map(Some)
        };
        (read_pid(generated)?, read_pid(committed)?)
    } else {
        (None, None)
    };
    for name in names {
        let path = generated.join(&name);
        let kind = fs::symlink_metadata(&path)
            .map_err(|error| format!("missing companion {name}: {error}"))?
            .file_type();
        if !kind.is_file() {
            return Err(format!("companion {name} must be a regular file"));
        }
        let actual = fs::read(path).map_err(|error| format!("read companion {name}: {error}"))?;
        let expected = fs::read(committed.join(&name))
            .map_err(|error| format!("read committed companion {name}: {error}"))?;
        regeneration_masks::compare(fixture, &name, &actual, &expected, pids)
            .map_err(|error| format!("companion {name} differs from committed bytes: {error}"))?;
    }
    Ok(())
}

#[allow(dead_code)]
pub(crate) fn companion_names(fixture: &str, committed: &Path) -> Result<Vec<String>, String> {
    let fixed: &[&str] = match fixture {
        "interaction-events" => &["source.json"],
        "interaction-permission" => &["source.json", "profiles.json", "hooks.json"],
        "stats-dateparse" => &["local-profiles.json"],
        // The generator emits this kernel-dependent oracle only on darwin;
        // other hosts compare the portable oracle.json without this companion.
        "skills-tree-files" if cfg!(target_os = "macos") => &["oracle-macos.json"],
        "user-dialog" => &["hosted.json"],
        "upload" => &[
            "created.json",
            "uploading.json",
            "completed.json",
            "lease.json",
            "expired.json",
        ],
        _ => &[],
    };
    let mut names: Vec<_> = fixed.iter().map(|name| (*name).to_owned()).collect();
    if matches!(fixture, "operation-ledger" | "run-registry") {
        let json = fs::read_to_string(committed.join("oracle.json")).map_err(|e| e.to_string())?;
        let oracle = parse_json(&json).map_err(|e| format!("companion manifest JSON: {e:?}"))?;
        let mut add = |value: Option<&JsValue>| -> Result<(), String> {
            names.push(
                value
                    .and_then(JsValue::as_string)
                    .ok_or("companion manifest filename must be a string")?
                    .to_utf8()
                    .map_err(|e| format!("companion filename UTF-8: {e:?}"))?,
            );
            Ok(())
        };
        let groups: &[&str] = if fixture == "operation-ledger" {
            &["cases", "rejectedCases", "seedCases"]
        } else {
            &["scenarios"]
        };
        for group in groups {
            let Some(JsValue::Array(rows)) = oracle.get(group) else {
                return Err(format!("companion manifest missing {group} array"));
            };
            for row in rows {
                if fixture == "operation-ledger" {
                    add(row.get("file"))?;
                } else {
                    for key in ["initial", "after"] {
                        if let Some(value) = row.get(key).filter(|v| !matches!(v, JsValue::Null)) {
                            add(Some(value))?;
                        }
                    }
                }
            }
        }
        if fixture == "operation-ledger" {
            add(oracle.get("utf8Read").and_then(|row| row.get("file")))?;
            names.push("partial-continued.jsonl".into());
        }
        let expected = if fixture == "operation-ledger" {
            34
        } else {
            49
        };
        if names.len() != expected {
            return Err(format!(
                "companion manifest count: expected {expected}, got {}",
                names.len()
            ));
        }
    }
    let mut unique = std::collections::HashSet::new();
    for name in &names {
        if name.is_empty()
            || name == "."
            || name == ".."
            || name.contains(['/', '\\'])
            || !unique.insert(name)
        {
            return Err(format!("invalid or duplicate companion filename {name:?}"));
        }
    }
    Ok(names)
}

/// Historical output layouts; collect all candidates without following links.
#[allow(dead_code)]
pub(crate) fn generated_oracle_bytes(
    fixture: &str,
    directory: &Path,
    stdout: &[u8],
) -> Result<Vec<u8>, String> {
    if matches!(fixture, "interaction-events" | "turn-lifecycle-matrix") {
        return Ok(stdout.to_vec());
    }
    fn collect(
        directory: &Path,
        rewind: bool,
        found: &mut Vec<std::path::PathBuf>,
    ) -> std::io::Result<()> {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                collect(&entry.path(), rewind, found)?;
            } else if kind.is_file() {
                if entry.file_name() == "oracle.json.tmp" {
                    return Err(std::io::Error::other(
                        "generator left oracle.json.tmp behind",
                    ));
                }
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name == "oracle.json"
                    || (rewind && name.starts_with("rewind-oracle-") && name.ends_with(".json"))
                {
                    found.push(entry.path());
                }
            }
        }
        Ok(())
    }
    let mut found = Vec::new();
    collect(directory, fixture == "rewind", &mut found).map_err(|error| error.to_string())?;
    if found.len() != 1 {
        return Err(format!(
            "expected exactly one generated oracle in {}, found {}",
            directory.display(),
            found.len()
        ));
    }
    fs::read(&found[0]).map_err(|error| error.to_string())
}
