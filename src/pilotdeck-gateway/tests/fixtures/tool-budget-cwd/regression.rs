//! Only bounded child workers change process cwd; the parent retains scratch ownership.
use super::*;
use serde_json::{Value, json};
use std::path::Path;

const ORACLE: &str = include_str!("oracle.json");
const WORKER: &str = "runtime::context::tool_budget::cwd_tests::environment_worker";

fn matrix(name: &str) {
    crate::test_baseline::oracle(ORACLE);
    let scratch = crate::test_scratch::Scratch::new("tool-budget-cwd");
    let root = fs::canonicalize(scratch.path()).expect("#885 setup canonical scratch");
    fs::create_dir(root.join("cwd")).expect("#885 setup cwd directory");
    crate::test_process::run_cases(
        WORKER,
        vec![(
            name.into(),
            vec![
                ("PDRS_TOOL_BUDGET_CWD_CASE".into(), name.into()),
                (
                    "PDRS_TOOL_BUDGET_CWD_ROOT".into(),
                    root.to_str().expect("#885 setup UTF-8 root").into(),
                ),
            ],
        )],
    );
}

#[test]
#[ignore = "worker only; parent supplies isolated cwd and checks one receipt"]
fn environment_worker() {
    let name = std::env::var("PDRS_TOOL_BUDGET_CWD_CASE").expect("#885 setup parent case");
    let root = std::env::var("PDRS_TOOL_BUDGET_CWD_ROOT").expect("#885 setup parent scratch");
    let oracle: Value = serde_json::from_str(ORACLE).expect("#885 setup oracle JSON");
    let rows = oracle["cases"].as_array().expect("#885 setup oracle cases");
    let row = rows
        .iter()
        .find(|row| row["name"] == name)
        .expect("#885 setup oracle row");
    let input = if name.starts_with("absolute-") {
        format!("{root}//files/nested/../results")
    } else {
        "../files/./nested/../results".into()
    };
    let cwd = Path::new(&root).join("cwd");
    let original_cwd = std::env::current_dir().expect("#885 setup original cwd");
    std::env::set_current_dir(&cwd).expect("#885 setup enter cwd");
    if name.contains("-warmed-") {
        // Rust has no process cwd cache; the read just proves the path was alive.
        std::env::current_dir().expect("#885 setup warm cwd");
    }
    if name.ends_with("-deleted") {
        fs::remove_dir(&cwd).expect("#885 setup remove cwd");
        assert!(
            std::env::current_dir().is_err(),
            "#885 setup deleted cwd must fail"
        );
    }
    let result = ToolResultBudget::new(ToolResultBudgetOptions::new(input));
    // Restore before assertions, even if the product returned an unexpected result.
    std::env::set_current_dir(original_cwd).expect("#885 setup restore cwd");
    let actual = match result {
        Ok(budget) => json!({
            "ok": true,
            "toolResultsDir": String::from_utf16_lossy(budget.tool_results_dir().units()),
        }),
        Err(error) => json!({"error": {"message": error.message}}),
    };
    let actual: Value = serde_json::from_str(&actual.to_string().replace(&root, "<root>"))
        .expect("#885 setup normalized JSON");
    let expected = if name == "relative-warmed-deleted" {
        // Registered Node warm-cwd-cache divergence: A7.4-TOOLBUDGET-CWD-001.
        // TS resolves against the cached (deleted) cwd; Rust keeps the cold
        // read's explicit error instead of a silent fallback.
        let cold = rows
            .iter()
            .find(|row| row["name"] == "relative-deleted")
            .expect("#885 setup cold oracle row");
        assert_eq!(
            row["outcome"],
            json!({"ok": true, "toolResultsDir": "<root>/files/results"}),
            "#885 setup warm TS success"
        );
        json!({"error": {"message": cold["outcome"]["error"]["message"]}})
    } else if row["outcome"].get("error").is_some() {
        json!({"error": {"message": row["outcome"]["error"]["message"]}})
    } else {
        json!({
            "ok": true,
            "toolResultsDir": row["outcome"]["toolResultsDir"],
        })
    };
    crate::negative_assert_eq!(actual, expected, "#885 outcome matrix: {name}");
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
case!(relative_normal, "relative-normal");
case!(relative_deleted, "relative-deleted");
case!(absolute_normal, "absolute-normal");
case!(absolute_deleted, "absolute-deleted");
case!(relative_warmed_normal, "relative-warmed-normal");
case!(relative_warmed_deleted, "relative-warmed-deleted");

#[test]
#[ignore = "requires Node v22.22.0 and PDRS_BASELINE"]
fn tool_budget_cwd_regenerates_identically() {
    crate::test_baseline::regenerates_identically("tool-budget-cwd");
}
