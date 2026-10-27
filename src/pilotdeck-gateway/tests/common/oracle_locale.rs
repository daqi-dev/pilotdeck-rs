//! Shared entry guard for en-US snapshots. Each crate root supplies the same
//! production parser, without exposing a public test API.
use crate::node_locale;

pub(crate) const MESSAGE: &str =
    "#887 oracle requires en-US; set LANG=en_US.UTF-8 and unset LC_ALL/LC_MESSAGES";

pub(crate) fn require_en_us() {
    assert_eq!(
        node_locale::node_locale(|key| std::env::var(key).ok()).to_string(),
        "en-US",
        "{MESSAGE}"
    );
}

pub(crate) async fn replay_locale_contract(child_test: &str, suite: &str) {
    const MSG: &str = "#887 replay locale contract";
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/node-locale/oracle.json")).expect(MSG);
    // These empty-directory replays have no locale-sensitive names. A missing
    // guard must fail the contract even when all their data still match.
    for case in fixture["localeProbe"].as_array().expect(MSG) {
        let id = case["id"].as_str().expect(MSG);
        let success = case["locale"] == "en-US";
        let mut child = tokio::process::Command::new(std::env::current_exe().expect(MSG));
        child
            .args([child_test, "--exact", "--nocapture", "--format", "pretty"])
            .env_remove("LANG")
            .env_remove("LC_ALL")
            .env_remove("LC_MESSAGES")
            .kill_on_drop(true);
        for (key, value) in case["env"].as_object().expect(MSG) {
            child.env(key, value.as_str().expect(MSG));
        }
        // Completion is child exit; the existing 30s deadline kills a stuck child.
        let output = tokio::time::timeout(std::time::Duration::from_secs(30), child.output())
            .await
            .unwrap_or_else(|e| panic!("{MSG}: {suite}/{id}: {e}"))
            .unwrap_or_else(|e| panic!("{MSG}: {suite}/{id}: {e}"));
        let log = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(log.contains("running 1 test"), "{MSG}: {suite}/{id}: {log}");
        if success {
            assert!(output.status.success(), "{MSG}: {suite}/{id}: {log}");
            assert!(
                log.contains("1 passed; 0 failed") && log.contains(&format!("{child_test} ... ok")),
                "{MSG}: {suite}/{id}: {log}"
            );
        } else {
            if output.status.success() {
                assert!(
                    log.contains("1 passed; 0 failed")
                        && log.contains(&format!("{child_test} ... ok")),
                    "{MSG}: malformed child success: {suite}/{id}: {log}"
                );
                panic!("#887 {suite} locale guard bypassed: {id}");
            }
            assert_eq!(
                output.status.code(),
                Some(101),
                "{MSG}: {suite}/{id}: {log}"
            );
            assert!(
                log.contains(MESSAGE) && log.contains(&format!("{child_test} ... FAILED")),
                "{MSG}: {suite}/{id}: {log}"
            );
        }
        println!(
            "#887 {suite}/{id}: locale={} success={success}",
            case["locale"]
        );
    }
}
