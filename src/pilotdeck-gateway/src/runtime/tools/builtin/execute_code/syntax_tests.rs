//! Actual TS+Python oracle. Pure projection is portable; actual compiler replay
//! is explicit because Python's diagnostic wording is version-specific.
use super::*;
const SYNTAX_ORACLE: &str =
    include_str!("../../../../../tests/fixtures/execute-code/syntax-oracle.json");
fn rows() -> Vec<JsValue> {
    let oracle = pilotdeck_protocol::parse_json(SYNTAX_ORACLE).unwrap();
    match oracle.get("rows").unwrap() {
        JsValue::Array(rows) => rows.clone(),
        _ => panic!("syntax oracle rows"),
    }
}
fn assert_issues(case: &JsValue, actual: Option<Vec<JsObject>>) {
    let expected = case.get("output").unwrap();
    if expected.get("ok") == Some(&JsValue::Bool(true)) {
        assert!(actual.is_none(), "{case:?}");
    } else {
        assert_eq!(
            JsValue::Array(actual.unwrap().into_iter().map(JsValue::Object).collect()),
            expected.get("issues").unwrap().clone(),
            "case {:?}",
            case.get("id")
        );
    }
}
#[test]
fn recorded_real_python_syntax_diagnostics_match_ts_utf16_projection() {
    let cases = rows();
    assert_eq!(cases.len(), 19);
    for case in &cases {
        let input = case.get("input").unwrap();
        let mut issues = validation(input);
        if issues.is_empty()
            && let Some(JsValue::Array(requests)) = case.get("requests")
            && let Some(request) = requests.first()
            && request.get("exitCode") != Some(&JsValue::Number(0.))
            && let Some(stdout) = request.get("stdout").and_then(JsValue::as_string)
            && let Some(diagnostic) = syntax_issue(&text(stdout))
        {
            issues.push(diagnostic);
        }
        assert_issues(case, (!issues.is_empty()).then_some(issues));
    }
}
#[tokio::test]
#[ignore = "requires the fixed oracle runtime /usr/bin/python3 3.9.6 on mbp2; run explicitly"]
async fn actual_python_validator_replays_fixed_ts_nineteen_cases() {
    let root = Workspace::create().unwrap();
    let python = std::process::Command::new("/usr/bin/python3")
        .arg("--version")
        .env_clear()
        .env("HOME", &root.path)
        .env("PILOT_HOME", &root.path)
        .env("PATH", "/usr/bin:/bin")
        .output()
        .unwrap();
    assert!(python.status.success());
    assert_eq!(
        String::from_utf8(python.stdout).unwrap().trim(),
        "Python 3.9.6",
        "unsupported oracle runtime, do not count as pass"
    );
    let context =
        super::tests::make_context(&root.path, crate::runtime::network::Cancellation::new());
    let tool = ExecuteCodeTool::new();
    for case in rows() {
        let actual = tokio::time::timeout(
            Duration::from_secs(5),
            tool.validate_input(case.get("input").unwrap().clone(), context.clone()),
        )
        .await
        .unwrap()
        .unwrap();
        assert_issues(&case, actual);
    }
}
