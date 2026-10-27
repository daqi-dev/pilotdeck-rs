//! #946: failure receipts are emitted only by explicitly bound assertions.
//! Normal test runs have no receipt environment and retain normal assertions.
use std::{fmt::Debug, fs::OpenOptions, io::Write};

pub(crate) const CONFIG_ENV: &str = "PDRS_NEGATIVE_RECEIPTS";
const PARENT_ENV: &str = "PDRS_NEGATIVE_PARENT_TEST";

/// Keep env_clear isolation, forwarding only the receipt protocol and actual
/// libtest parent identity. Never trust a caller-supplied parent identity.
pub(crate) fn forward_to_worker(env: &mut Vec<(String, String)>) {
    env.retain(|(key, _)| key != CONFIG_ENV && key != PARENT_ENV);
    if let Ok(config) = std::env::var(CONFIG_ENV) {
        let thread = std::thread::current();
        let parent = thread.name().expect("#946 named libtest parent");
        env.push((CONFIG_ENV.into(), config));
        env.push((PARENT_ENV.into(), parent.into()));
    }
}

pub(crate) fn record_failure(assertion: &str) {
    let Some(config) = std::env::var_os(CONFIG_ENV) else {
        return;
    };
    let config = config.to_str().expect("#946 receipt config UTF-8");
    let run: serde_json::Value = serde_json::from_str(config).expect("#946 receipt config JSON");
    let nonce = run["nonce"].as_str().expect("#946 string nonce");
    assert!(
        nonce.len() == 64 && nonce.bytes().all(|b| b.is_ascii_hexdigit()),
        "#946 invalid receipt nonce"
    );
    let thread = std::thread::current();
    let parent = std::env::var(PARENT_ENV).ok();
    let test = parent
        .as_deref()
        .or_else(|| thread.name())
        .expect("#946 named failure test");
    let targets: Vec<_> = run["targets"]
        .as_array()
        .expect("#946 targets array")
        .iter()
        .filter(|target| target["test"] == test && target["assertion"] == assertion)
        .collect();
    assert!(targets.len() <= 1, "#946 duplicate receipt target");
    if let Some(target) = targets.first() {
        let path = std::path::Path::new(target["path"].as_str().expect("#946 receipt path string"));
        assert!(path.is_absolute(), "#946 absolute receipt path required");
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .expect("#946 create fresh failure receipt");
        file.write_all(nonce.as_bytes())
            .expect("#946 write failure receipt");
        file.sync_all().expect("#946 flush failure receipt");
    }
}

pub(crate) fn expect_err<T: Debug, E>(result: Result<T, E>, assertion: &str) -> E {
    match result {
        Err(error) => error,
        Ok(value) => {
            record_failure(assertion);
            panic!("{assertion}: unexpected Ok({value:?})");
        }
    }
}

pub(crate) fn expect_ok<T, E: Debug>(result: Result<T, E>, assertion: &str) -> T {
    result.unwrap_or_else(|error| {
        record_failure(assertion);
        panic!("{assertion}: {error:?}");
    })
}

pub(crate) fn expect_some<T>(value: Option<T>, assertion: &str) -> T {
    value.unwrap_or_else(|| {
        record_failure(assertion);
        panic!("{assertion}: unexpected None");
    })
}

#[macro_export]
macro_rules! negative_assert_eq {
    ($left:expr, $right:expr, $($message:tt)+) => {
        match (&$left, &$right) {
            (left, right) => {
                if left != right {
                    let assertion = format!($($message)+);
                    $crate::test_negative_receipt::record_failure(&assertion);
                    assert_eq!(left, right, "{assertion}");
                }
            }
        }
    };
}

#[macro_export]
macro_rules! negative_assert {
    ($condition:expr, $($message:tt)+) => {
        if !$condition {
            let assertion = format!($($message)+);
            $crate::test_negative_receipt::record_failure(&assertion);
            panic!("{assertion}");
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    const PARENT: &str = "test_negative_receipt::tests::parent_worker";
    const WORKER: &str = "test_negative_receipt::tests::assertion_worker";
    const ASSERTION: &str = "#946 dedicated assertion";

    #[test]
    #[ignore = "bounded receipt protocol worker only"]
    fn parent_worker() {
        // Deliberate forged identity in fixture input must be overwritten.
        crate::test_process::run_cases(
            WORKER,
            vec![(
                "receipt".into(),
                vec![
                    (
                        "PDRS_RECEIPT_CONTROL".into(),
                        std::env::var("PDRS_RECEIPT_CONTROL").expect("#946 control"),
                    ),
                    (PARENT_ENV.into(), "forged".into()),
                ],
            )],
        );
    }

    #[test]
    #[ignore = "bounded receipt protocol worker only"]
    fn assertion_worker() {
        let mode = std::env::var("PDRS_RECEIPT_CONTROL").expect("#946 control");
        match mode.as_str() {
            "equal" => crate::negative_assert_eq!(1, 2, "{ASSERTION}"),
            "boolean" => crate::negative_assert!(false, "{ASSERTION}"),
            "expect-error" => {
                expect_err::<_, ()>(Ok(1), ASSERTION);
            }
            "expect-ok" => {
                expect_ok::<(), _>(Err("wrong"), ASSERTION);
            }
            "expect-some" => {
                expect_some::<()>(None, ASSERTION);
            }
            "pass" => {
                crate::negative_assert_eq!(1, 1, "{ASSERTION}");
                crate::negative_assert!(true, "{ASSERTION}");
                expect_err::<(), _>(Err(()), ASSERTION);
                expect_ok::<_, ()>(Ok(()), ASSERTION);
                expect_some(Some(()), ASSERTION);
            }
            "wrong-assertion" => crate::negative_assert!(false, "#946 unrelated assertion"),
            "setup" => {
                println!("test result: FAILED. 0 passed; 1 failed;\n{ASSERTION}");
                panic!("#946 unrelated setup failure");
            }
            other => panic!("#946 unknown control: {other}"),
        }
        crate::test_process::receipt("receipt");
    }

    #[test]
    fn receipts_bind_failure_branch_test_and_fresh_file_through_env_clear() {
        let scratch = crate::test_scratch::Scratch::new("negative-receipt");
        for (index, (mode, identity, configured, preexisting, expected)) in [
            ("equal", PARENT, true, false, true),
            ("boolean", PARENT, true, false, true),
            ("expect-error", PARENT, true, false, true),
            ("expect-ok", PARENT, true, false, true),
            ("expect-some", PARENT, true, false, true),
            ("pass", PARENT, true, false, false),
            ("wrong-assertion", PARENT, true, false, false),
            ("setup", PARENT, true, false, false),
            ("equal", "sibling", true, false, false),
            ("equal", PARENT, false, false, false),
            ("equal", PARENT, true, true, false),
        ]
        .into_iter()
        .enumerate()
        {
            let path = scratch.path().join(format!("{index}.receipt"));
            let nonce = format!("{index:064x}");
            let run = serde_json::json!({
                "nonce": nonce,
                "targets": [{
                    "test": identity,
                    "assertion": ASSERTION,
                    "path": path.to_str().expect("#946 receipt path UTF-8"),
                }],
            });
            if preexisting {
                std::fs::write(&path, "stale").expect("#946 stale fixture");
            }
            let mut command = Command::new(std::env::current_exe().expect("#946 test executable"));
            command
                .args(["--exact", PARENT, "--ignored", "--show-output"])
                .env_clear()
                .env("PDRS_RECEIPT_CONTROL", mode);
            if configured {
                command.env(
                    CONFIG_ENV,
                    serde_json::to_string(&run).expect("#946 config JSON"),
                );
            }
            let output = command.output().expect("#946 bounded child result");
            assert_eq!(output.status.success(), mode == "pass", "#946 {mode}");
            let content = std::fs::read_to_string(&path).ok();
            assert_eq!(
                content.as_deref(),
                if expected {
                    Some(nonce.as_str())
                } else if preexisting {
                    Some("stale")
                } else {
                    None
                },
                "#946 receipt branch {index}: {mode}/{identity}"
            );
            println!(
                "#946 protocol {index}: {mode}, receipt={expected}, exit={}",
                output.status
            );
            if expected {
                let stdout = String::from_utf8(output.stdout).expect("#946 output UTF-8");
                assert!(
                    stdout
                        .lines()
                        .any(|line| line == format!("test {PARENT} ... FAILED")),
                    "#946 --show-output preserves named FAILED line: {stdout}"
                );
            }
        }
    }
}
