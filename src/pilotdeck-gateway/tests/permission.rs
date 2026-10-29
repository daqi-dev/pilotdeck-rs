//! Replay of the real-TS permission oracle. Every comparison below is the
//! serialized JSON bytes of the decision, so key order and UTF-16 details are
//! part of the fixture.
#[path = "common/baseline.rs"]
mod baseline;
#[path = "common/exact_receipt.rs"]
mod exact_receipt;

use pilotdeck_gateway::{
    config::permission::{
        ConfiguredRuleSet, GatewaySessionPermissionRuleSetRegistry, PermissionMode, PermissionRule,
        PermissionRuleSet,
    },
    host::HostError,
    runtime::permission::{
        DefaultPermissionContextOptions, PermissionContext, PermissionFuture, PermissionTool,
        create_default_permission_context, decide, is_permission_mode, match_permission_rule,
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

const ORACLE: &str = include_str!("fixtures/permission/oracle.json");

fn text(value: &JsValue) -> String {
    stringify_json(value)
        .expect("serializable value")
        .expect("value is not undefined")
}

fn string(value: &JsValue, what: &str) -> JsString {
    value
        .as_string()
        .cloned()
        .unwrap_or_else(|| panic!("{what} must be a string"))
}

fn boolean(value: &JsValue, what: &str) -> bool {
    value
        .as_bool()
        .unwrap_or_else(|| panic!("{what} must be a boolean"))
}

fn array<'a>(value: &'a JsValue, what: &str) -> &'a [JsValue] {
    match value {
        JsValue::Array(items) => items,
        _ => panic!("{what} must be an array"),
    }
}

fn object<'a>(value: &'a JsValue, what: &str) -> &'a JsObject {
    value
        .as_object()
        .unwrap_or_else(|| panic!("{what} must be an object"))
}

fn parse_rules(value: Option<&JsValue>) -> Vec<PermissionRule> {
    value
        .filter(|value| !value.is_undefined())
        .map(|value| {
            array(value, "rule list")
                .iter()
                .map(|rule| PermissionRule::from_value(rule).expect("rule object"))
                .collect()
        })
        .unwrap_or_default()
}

fn parse_rule_set(value: &JsValue) -> PermissionRuleSet {
    let rules = object(value, "rules");
    PermissionRuleSet {
        allow: parse_rules(rules.get("allow")),
        deny: parse_rules(rules.get("deny")),
        ask: parse_rules(rules.get("ask")),
    }
}

fn parse_context(value: &JsValue) -> PermissionContext {
    let context = object(value, "permission context");
    PermissionContext {
        mode: PermissionMode::new(string(context.get("mode").expect("mode"), "context.mode")),
        rules: parse_rule_set(context.get("rules").expect("rules")),
        cwd: string(context.get("cwd").expect("cwd"), "context.cwd"),
        additional_working_directories: array(
            context
                .get("additionalWorkingDirectories")
                .expect("additionalWorkingDirectories"),
            "additionalWorkingDirectories",
        )
        .iter()
        .map(|value| string(value, "additional working directory"))
        .collect(),
        can_prompt: boolean(context.get("canPrompt").expect("canPrompt"), "canPrompt"),
        policy_can_prompt: context.get("policyCanPrompt").and_then(JsValue::as_bool),
        bypass_available: boolean(
            context.get("bypassAvailable").expect("bypassAvailable"),
            "bypassAvailable",
        ),
        accept_edits: context.get("acceptEdits").and_then(JsValue::as_bool),
        plan_directory_path: context
            .get("planDirectoryPath")
            .and_then(JsValue::as_string)
            .cloned(),
    }
}

struct FakeTool {
    name: JsString,
    kind: Option<JsString>,
    read_only: JsValue,
    checks: Mutex<Vec<JsValue>>,
    calls: AtomicUsize,
}

impl FakeTool {
    fn from_scenario(value: &JsValue) -> Self {
        let tool = object(value, "tool");
        Self {
            name: string(tool.get("name").expect("tool.name"), "tool.name"),
            kind: tool.get("kind").and_then(JsValue::as_string).cloned(),
            read_only: tool
                .get("readOnly")
                .cloned()
                .unwrap_or(JsValue::Bool(false)),
            checks: Mutex::new(
                tool.get("checks")
                    .map(|checks| array(checks, "tool.checks").to_vec())
                    .unwrap_or_default(),
            ),
            calls: AtomicUsize::new(0),
        }
    }
}

impl PermissionTool for FakeTool {
    fn name(&self) -> &JsString {
        &self.name
    }
    fn kind(&self) -> Option<&JsString> {
        self.kind.as_ref()
    }
    fn is_read_only(&self, _input: &JsValue) -> Result<JsValue, HostError> {
        Ok(self.read_only.clone())
    }
    fn check_permissions<'a>(
        &'a self,
        _input: &'a JsValue,
        _context: &'a PermissionContext,
    ) -> PermissionFuture<'a, Option<JsValue>> {
        let index = self.calls.fetch_add(1, Ordering::SeqCst);
        let value = self.checks.lock().expect("checks lock").get(index).cloned();
        Box::pin(async move { Ok(value) })
    }
}

fn input_of(value: &JsObject) -> JsValue {
    value.get("input").cloned().unwrap_or(JsValue::Undefined)
}

#[tokio::test]
async fn permission_oracle_replays_in_order() {
    let oracle = baseline::oracle(ORACLE);
    let counts = object(oracle.get("counts").expect("counts"), "counts");
    let match_scenarios = array(oracle.get("matches").expect("matches"), "matches");
    let decision_scenarios = array(oracle.get("decisions").expect("decisions"), "decisions");
    let mode_scenarios = array(oracle.get("modes").expect("modes"), "modes");
    let default_scenarios = array(oracle.get("defaults").expect("defaults"), "defaults");

    // Exact cardinality: a dropped or duplicated scenario fails here first.
    assert_eq!(match_scenarios.len(), 47);
    assert_eq!(decision_scenarios.len(), 67);
    assert_eq!(mode_scenarios.len(), 9);
    assert_eq!(default_scenarios.len(), 4);
    for (name, expected) in [
        ("matches", 47.0),
        ("decisions", 67.0),
        ("modes", 9.0),
        ("defaults", 4.0),
    ] {
        assert_eq!(
            counts.get(name).and_then(JsValue::as_number),
            Some(expected),
            "oracle counts.{name}"
        );
    }
    assert!(
        object(
            oracle.get("sourceSha256").expect("sourceSha256"),
            "sourceSha256"
        )
        .len()
            > 3,
        "import closure must cover more than the entry files"
    );

    for scenario in mode_scenarios {
        let scenario = object(scenario, "mode");
        let expected = scenario.get("expected").expect("mode.expected");
        assert_eq!(
            JsValue::Bool(is_permission_mode(
                scenario.get("value").expect("mode.value")
            )),
            *expected,
            "mode {}",
            text(scenario.get("name").expect("mode.name"))
        );
    }

    for scenario in default_scenarios {
        let scenario = object(scenario, "default");
        let options = object(scenario.get("options").expect("options"), "options");
        let rules = options.get("rules").map(parse_rule_set).unwrap_or_default();
        let actual = create_default_permission_context(DefaultPermissionContextOptions {
            cwd: string(options.get("cwd").expect("cwd"), "cwd"),
            mode: options
                .get("mode")
                .and_then(JsValue::as_string)
                .cloned()
                .map(PermissionMode::new),
            can_prompt: options.get("canPrompt").and_then(JsValue::as_bool),
            policy_can_prompt: options.get("policyCanPrompt").and_then(JsValue::as_bool),
            bypass_available: options.get("bypassAvailable").and_then(JsValue::as_bool),
            accept_edits: options.get("acceptEdits").and_then(JsValue::as_bool),
            additional_working_directories: options.get("additionalWorkingDirectories").map(
                |directories| {
                    array(directories, "additionalWorkingDirectories")
                        .iter()
                        .map(|value| string(value, "directory"))
                        .collect()
                },
            ),
            plan_directory_path: options
                .get("planDirectoryPath")
                .and_then(JsValue::as_string)
                .cloned(),
            rules,
        })
        .to_value();
        assert_eq!(
            text(&actual),
            text(scenario.get("expected").expect("default.expected")),
            "default {}",
            text(scenario.get("name").expect("default.name"))
        );
    }

    for scenario in match_scenarios {
        let scenario = object(scenario, "match");
        let rule =
            PermissionRule::from_value(scenario.get("rule").expect("rule")).expect("rule object");
        let tool_name = string(scenario.get("toolName").expect("toolName"), "toolName");
        let input = input_of(scenario);
        let context = scenario.get("context").map(parse_context);
        let process_cwd = JsString::from(
            std::env::current_dir()
                .expect("process cwd")
                .to_string_lossy()
                .as_ref(),
        );
        let actual = JsValue::Bool(match_permission_rule(
            &rule,
            &tool_name,
            &input,
            context.as_ref(),
            &process_cwd,
        ));
        assert_eq!(
            text(&actual),
            text(scenario.get("expected").expect("match.expected")),
            "match {}",
            text(scenario.get("name").expect("match.name"))
        );
    }

    for scenario in decision_scenarios {
        let scenario = object(scenario, "decision");
        let context = parse_context(scenario.get("context").expect("context"));
        let tool = FakeTool::from_scenario(scenario.get("tool").expect("tool"));
        let input = input_of(scenario);
        let tool_call_id = string(
            scenario.get("toolCallId").expect("toolCallId"),
            "toolCallId",
        );
        let actual = decide(&context, &tool, &input, &tool_call_id)
            .await
            .expect("decision does not throw");
        let expected = object(scenario.get("expected").expect("expected"), "expected");
        assert_eq!(
            text(&actual),
            text(expected.get("decision").expect("decision")),
            "decision {}",
            text(scenario.get("name").expect("name"))
        );
        let calls = tool.calls.load(Ordering::SeqCst) as f64;
        assert_eq!(
            Some(calls),
            expected.get("calls").and_then(JsValue::as_number),
            "checkPermissions calls for {}",
            text(scenario.get("name").expect("name"))
        );
    }
}

#[tokio::test]
async fn session_registry_rules_feed_the_engine() {
    let registry = GatewaySessionPermissionRuleSetRegistry::new();
    let mut configured_rules = PermissionRuleSet::default();
    configured_rules.allow.push(
        PermissionRule::new("session", "allow", "read_file").with_pattern("/ws/project/src/*"),
    );
    let configured = ConfiguredRuleSet::new(configured_rules);
    let key = JsString::from("session");
    let lease = registry.acquire(&key, Some(&configured));
    let snapshot = lease.rules().snapshot();
    assert_eq!(snapshot.allow.len(), 1);

    let context = PermissionContext {
        mode: PermissionMode::new("default"),
        rules: snapshot,
        cwd: JsString::from("/ws/project"),
        additional_working_directories: vec![],
        can_prompt: false,
        policy_can_prompt: None,
        bypass_available: true,
        accept_edits: None,
        plan_directory_path: None,
    };
    let tool = FakeTool {
        name: JsString::from("read_file"),
        kind: None,
        read_only: JsValue::Bool(true),
        checks: Mutex::new(vec![]),
        calls: AtomicUsize::new(0),
    };
    let decision = decide(
        &context,
        &tool,
        &JsValue::Object({
            let mut input = JsObject::new();
            input.insert("file_path".into(), JsValue::from("src/a.rs"));
            input
        }),
        &JsString::from("call-1"),
    )
    .await
    .expect("decision");
    assert_eq!(
        decision.get("type").and_then(JsValue::as_string),
        Some(&JsString::from("allow"))
    );
    let reason = object(decision.get("reason").expect("reason"), "reason");
    assert_eq!(
        reason.get("message").and_then(JsValue::as_string),
        Some(&JsString::from("Session allow rule permits read_file."))
    );
    lease.release();
}

#[tokio::test]
async fn negative_controls_reject_case_folding_and_swapped_rule_priority() {
    let oracle = baseline::oracle(ORACLE);
    let match_scenarios = array(oracle.get("matches").expect("matches"), "matches");

    // Negative control 1: a case-insensitive wildcard implementation would
    // disagree with the baseline on these two scenarios.
    let case_sensitive = ["wildcard-case-mismatch", "file-pattern-case-sensitive"];
    for name in case_sensitive {
        let scenario = match_scenarios
            .iter()
            .map(|scenario| object(scenario, "match"))
            .find(|scenario| {
                scenario
                    .get("name")
                    .and_then(JsValue::as_string)
                    .is_some_and(|value| value.equals_str(name))
            })
            .unwrap_or_else(|| panic!("missing scenario {name}"));
        assert_eq!(
            scenario.get("expected"),
            Some(&JsValue::Bool(false)),
            "{name} must stay a rejection"
        );
    }

    // Negative control 2: both a user allow and a user deny rule match the
    // same call; deny must win because the baseline checks deny first.
    let decision_scenarios = array(oracle.get("decisions").expect("decisions"), "decisions");
    let scenario = decision_scenarios
        .iter()
        .map(|scenario| object(scenario, "decision"))
        .find(|scenario| {
            scenario
                .get("name")
                .and_then(JsValue::as_string)
                .is_some_and(|value| value.equals_str("user-deny-beats-user-allow"))
        })
        .expect("priority scenario");
    let decision = object(
        scenario
            .get("expected")
            .expect("expected")
            .get("decision")
            .expect("decision"),
        "decision",
    );
    assert_eq!(
        decision.get("type").and_then(JsValue::as_string),
        Some(&JsString::from("deny"))
    );
    let context = parse_context(scenario.get("context").expect("context"));
    let input = input_of(scenario);
    let tool_name = string(
        object(scenario.get("tool").expect("tool"), "tool")
            .get("name")
            .expect("name"),
        "name",
    );
    let process_cwd = JsString::from(
        std::env::current_dir()
            .expect("process cwd")
            .to_string_lossy()
            .as_ref(),
    );
    for rules in [&context.rules.allow, &context.rules.deny] {
        assert!(
            rules.iter().any(|rule| match_permission_rule(
                rule,
                &tool_name,
                &input,
                Some(&context),
                &process_cwd
            )),
            "the negative control needs both an allow and a deny match"
        );
    }
}

#[tokio::test]
async fn empty_cwd_is_skipped_instead_of_becoming_root() {
    // TS: path.resolve(path.join("", "secret.env")) is
    // `<process cwd>/secret.env`; it is not `/secret.env`. A port that
    // concatenates `cwd + "/" + value` unconditionally flips both match
    // assertions below, so this is the negative control for the P0 fix.
    let process_cwd = std::env::current_dir().expect("process cwd");
    assert_ne!(
        process_cwd,
        std::path::Path::new("/"),
        "cwd-empty expectations assume a non-root process cwd"
    );
    let process_cwd = JsString::from(process_cwd.to_string_lossy().as_ref());
    let context = create_default_permission_context(DefaultPermissionContextOptions {
        cwd: JsString::from(""),
        ..Default::default()
    });
    let input = JsValue::Object({
        let mut input = JsObject::new();
        input.insert("file_path".into(), JsValue::from("secret.env"));
        input
    });
    let read_file = JsString::from("read_file");
    let relative_rule =
        PermissionRule::new("user", "allow", "read_file").with_pattern("/*/secret.env");
    let root_rule = PermissionRule::new("user", "allow", "read_file").with_pattern("/secret.env");
    assert!(match_permission_rule(
        &relative_rule,
        &read_file,
        &input,
        Some(&context),
        &process_cwd
    ));
    assert!(!match_permission_rule(
        &root_rule,
        &read_file,
        &input,
        Some(&context),
        &process_cwd
    ));

    // The same pair at the decision layer: `/*/secret.env` denies, while the
    // root-only pattern must not match and read_file falls through to allow.
    for (pattern, expected) in [("/*/secret.env", "deny"), ("/secret.env", "allow")] {
        let mut rules = PermissionRuleSet::default();
        rules
            .deny
            .push(PermissionRule::new("user", "deny", "read_file").with_pattern(pattern));
        let context = create_default_permission_context(DefaultPermissionContextOptions {
            cwd: JsString::from(""),
            rules,
            can_prompt: Some(true),
            ..Default::default()
        });
        let tool = FakeTool {
            name: read_file.clone(),
            kind: None,
            read_only: JsValue::Bool(true),
            checks: Mutex::new(vec![]),
            calls: AtomicUsize::new(0),
        };
        let decision = decide(&context, &tool, &input, &JsString::from("call-1"))
            .await
            .expect("decision");
        assert_eq!(
            decision.get("type").and_then(JsValue::as_string),
            Some(&JsString::from(expected)),
            "pattern {pattern}"
        );
    }
}

#[tokio::test]
async fn malformed_tool_ask_is_rejected_like_the_typescript_throw() {
    fn ask(reason: Option<JsValue>) -> JsValue {
        let mut value = JsObject::new();
        value.insert("type".into(), JsValue::from("ask"));
        if let Some(reason) = reason {
            value.insert("reason".into(), reason);
        }
        JsValue::Object(value)
    }
    fn reason(entries: &[(&str, JsValue)]) -> JsValue {
        let mut value = JsObject::new();
        for (key, item) in entries {
            value.insert((*key).into(), item.clone());
        }
        JsValue::Object(value)
    }

    let context = create_default_permission_context(DefaultPermissionContextOptions {
        cwd: JsString::from("/ws/project"),
        can_prompt: Some(true),
        ..Default::default()
    });
    let input = JsValue::Object({
        let mut input = JsObject::new();
        input.insert("file_path".into(), JsValue::from("src/a.rs"));
        input
    });
    let malformed = [
        // `decision.reason.type` throws (TypeError).
        ask(None),
        // `decision.reason.rule.source` throws.
        ask(Some(reason(&[("type", JsValue::from("rule"))]))),
        // `decision.reason.rule.toolName.startsWith` throws.
        ask(Some(reason(&[
            ("type", JsValue::from("rule")),
            ("force", JsValue::Bool(true)),
            ("source", JsValue::from("session")),
        ]))),
    ];
    for check in malformed {
        let tool = FakeTool {
            name: JsString::from("write_file"),
            kind: None,
            read_only: JsValue::Bool(false),
            checks: Mutex::new(vec![check]),
            calls: AtomicUsize::new(0),
        };
        let result = decide(&context, &tool, &input, &JsString::from("call-1")).await;
        assert!(result.is_err(), "malformed ask must fail closed");
    }

    // The baseline returns before finalizeAsk in bypass mode, so a malformed
    // tool ask is still allowed there (no dereference happens).
    let bypass = create_default_permission_context(DefaultPermissionContextOptions {
        cwd: JsString::from("/ws/project"),
        mode: Some(PermissionMode::new("bypassPermissions")),
        can_prompt: Some(true),
        ..Default::default()
    });
    let tool = FakeTool {
        name: JsString::from("write_file"),
        kind: None,
        read_only: JsValue::Bool(false),
        checks: Mutex::new(vec![ask(None)]),
        calls: AtomicUsize::new(0),
    };
    let decision = decide(&bypass, &tool, &input, &JsString::from("call-1"))
        .await
        .expect("bypass short-circuits before finalizeAsk");
    assert_eq!(
        decision.get("type").and_then(JsValue::as_string),
        Some(&JsString::from("allow"))
    );
}

#[test]
#[ignore = "requires the fixed TS checkout and its installed tsx loader (PDRS_BASELINE)"]
fn regenerate_twice_identically() {
    let baseline = std::env::var_os("PDRS_BASELINE").expect("set PDRS_BASELINE");
    let baseline = std::path::Path::new(&baseline);
    let temp = std::env::temp_dir()
        .canonicalize()
        .expect("canonical temp root");
    let directory = temp.join(format!("pdrs-permission-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).expect("create scratch");
    let mut generated = Vec::new();
    for _ in 0..2 {
        let result = std::process::Command::new("node")
            .arg("--import")
            .arg(baseline.join("node_modules/tsx/dist/loader.mjs"))
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/permission/generate.mts"
            ))
            .arg(&directory)
            .env("PDRS_BASELINE", baseline)
            .output()
            .expect("run generator");
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let stdout = String::from_utf8(result.stdout).expect("utf-8 stdout");
        let output = std::path::PathBuf::from(stdout.trim());
        assert!(
            output.starts_with(&directory) && output.ends_with("oracle.json"),
            "generator wrote outside scratch"
        );
        generated.push(std::fs::read(&output).expect("read oracle"));
    }
    assert_eq!(
        generated[0], generated[1],
        "two runs must be byte-identical"
    );
    assert_eq!(
        generated[0],
        include_bytes!("fixtures/permission/oracle.json"),
        "fixture must equal a fresh run"
    );
    // Ownership check before the only recursive delete: the scratch is a
    // direct child of the canonical temp root with our prefix.
    assert!(
        directory.starts_with(&temp)
            && directory
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("pdrs-permission-")),
        "refusing to delete an unowned scratch directory"
    );
    std::fs::remove_dir_all(&directory).expect("remove scratch");
}

/// #335 ②: a cold Node process whose cwd was deleted throws
/// `ENOENT: no such file or directory, uv_cwd` from `process.cwd()` and the
/// whole `decide` fails. Delete the cwd in an owned child process because the
/// test harness shares one process and other tests resolve relative paths.
#[tokio::test]
async fn deleted_process_cwd_fails_the_decision() {
    if std::env::var_os("PDRS_DELETED_CWD_CHILD").is_none() {
        let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([
                "--exact",
                "deleted_process_cwd_fails_the_decision",
                "--nocapture",
                "--test-threads=2",
            ])
            .env("PDRS_DELETED_CWD_CHILD", "1")
            .output()
            .expect("run deleted-cwd child");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        // #942: exit 0 alone is not a receipt — a renamed test makes this
        // `--exact` filter match nothing and libtest still exits 0.
        assert!(
            exact_receipt::single_pass(
                &output.status,
                "deleted_process_cwd_fails_the_decision",
                &stdout
            ),
            "#942 deleted-cwd decision rerun matched no test:\n{stdout}\n{stderr}"
        );
        assert!(
            stdout.contains("PD_CWD_ERROR=ENOENT: no such file or directory, uv_cwd"),
            "missing Node-shaped error in: {stdout}\n{stderr}"
        );
        return;
    }

    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let directory =
        std::env::temp_dir().join(format!("pdrs-deleted-cwd-{}-{unique}", std::process::id()));
    std::fs::create_dir_all(&directory).expect("create cwd");
    std::env::set_current_dir(&directory).expect("enter cwd");
    std::fs::remove_dir_all(&directory).expect("delete cwd");

    let context = create_default_permission_context(DefaultPermissionContextOptions {
        cwd: JsString::from("relative/project"),
        ..Default::default()
    });
    let tool = FakeTool {
        name: JsString::from("Read"),
        kind: None,
        read_only: JsValue::Bool(true),
        checks: Mutex::new(Vec::new()),
        calls: AtomicUsize::new(0),
    };
    let error = decide(&context, &tool, &JsValue::Null, &JsString::from("call"))
        .await
        .expect_err("a deleted process cwd must fail decide");
    println!("PD_CWD_ERROR={}", error.message);
}
