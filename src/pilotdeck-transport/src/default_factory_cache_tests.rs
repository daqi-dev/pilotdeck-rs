//! Production native injection regression. Expected plans are fixed Node output.
use super::*;
use pilotdeck_agent::request::RequestAssemblyOptions;
use pilotdeck_agent::turn::TurnPermissionContext;
use pilotdeck_protocol::{parse_json, stringify_json};
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn array(value: &JsValue) -> &[JsValue] {
    match value {
        JsValue::Array(values) => values,
        _ => panic!("fixture array"),
    }
}
fn encoded(value: &JsValue) -> String {
    stringify_json(value).unwrap().unwrap()
}

#[test]
fn production_cache_assembly_preserves_lone_surrogate_tool_order() {
    // Rust 2024 process-environment mutation requires unsafe. Isolate the real
    // production environment lookup in an owned test subprocess instead.
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "default_factory::native_cache_collation_tests::production_assembly_child",
            "--ignored",
            "--nocapture",
        ])
        .env_remove("LC_ALL")
        .env_remove("LC_MESSAGES")
        .env("LANG", "en_US.UTF-8")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("production native cache child exceeded fifteen seconds");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut stdout = String::new();
    let mut stderr = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut stdout)
        .unwrap();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(child.wait().unwrap().success(), "{stdout}\n{stderr}");
    assert!(
        stdout.contains("NATIVE_CACHE_PRODUCTION_CASES=8"),
        "{stdout}"
    );
    assert!(stderr.is_empty(), "{stderr}");
}

struct ContextHost {
    prepared: JsValue,
    calls: usize,
}
impl HostPort for ContextHost {
    fn advertised(&self, module: &str, operation: &str) -> bool {
        module == "context" && operation == "prepare_for_model"
    }
    fn call(&mut self, control: &CallControl, call: HostCall) -> PortResult<HostReply> {
        control.check()?;
        assert!(matches!(call, HostCall::PrepareContext(_)));
        self.calls += 1;
        assert_eq!(self.calls, 1, "one real context preparation per assembly");
        Ok(HostReply::ContextBoundaryOnly(BoundarySnapshot::capture(
            &self.prepared,
        )))
    }
}
struct Observer;
impl RequestPreparationObserver for Observer {
    fn instructions_loaded(&mut self, has_system_prompt: bool) -> PortResult<()> {
        assert!(has_system_prompt);
        Ok(())
    }
}

#[test]
#[ignore = "LANG-isolated helper, actually executed by the production cache parent test"]
fn production_assembly_child() {
    assert_eq!(std::env::var("LANG").unwrap(), "en_US.UTF-8");
    assert!(std::env::var_os("LC_ALL").is_none());
    assert!(std::env::var_os("LC_MESSAGES").is_none());
    let oracle = parse_json(include_str!(
        "../tests/fixtures/cache-collation/production-oracle.json"
    ))
    .unwrap();
    assert_eq!(oracle.get("LANG"), Some(&JsValue::from("en_US.UTF-8")));
    let inputs = array(oracle.get("cases").unwrap());
    assert_eq!(inputs.len(), 4);
    assert_eq!(
        inputs[1].get("id"),
        Some(&JsValue::from("reverse-opposed-description-pair"))
    );
    let reverse_tools = array(inputs[1].get("input").unwrap().get("tools").unwrap());
    assert!(reverse_tools.len() > 1);
    // These names collapse to equal U+FFFD under plain compare_utf16. The
    // descriptions deliberately order the two names in the opposite direction.
    // Plain collation then takes the stableSerialize tiebreak and changes the
    // TS fingerprint; the production preserving-surrogates line participates.
    for unit in [0xd800, 0xd801] {
        assert!(reverse_tools.iter().any(|tool| {
            tool.get("name")
                .and_then(JsValue::as_string)
                .is_some_and(|name| name.units() == [unit])
        }));
    }
    assert_eq!(
        reverse_tools[0].get("description"),
        Some(&JsValue::from("z"))
    );
    assert_eq!(
        reverse_tools[1].get("description"),
        Some(&JsValue::from("a"))
    );
    let mut cases = 0;
    for (index, row) in inputs.iter().take(4).enumerate() {
        let raw = row.get("input").unwrap();
        let route = Route {
            provider: raw.get("provider").unwrap().as_string().unwrap().clone(),
            model: raw.get("model").unwrap().as_string().unwrap().clone(),
        };
        let config = TurnRuntimeConfig {
            route,
            cwd: "/controlled/production-cache".into(),
            system_prompt: None,
            append_system_prompt: None,
            plan_mode_instructions: None,
            runtime_context_surface: RuntimeContextSurface::SystemPrompt,
            max_output_tokens: None,
            max_context_tokens: None,
            thinking: None,
            tool_choice: None,
            max_context_messages: None,
            stop_on_structured_output: None,
            json_self_correct: None,
            run_mode: None,
            is_subagent: None,
            subagent_model: None,
            permission_mode_before_plan: None,
            permission: TurnPermissionContext {
                mode: PermissionMode::Default,
                can_prompt: false,
                bypass_available: false,
                additional_working_directories: vec![],
                rules: None,
            },
            metadata: None,
        };
        let input = TurnInput {
            session_id: "session".into(),
            turn_id: "turn".into(),
            messages: vec![],
            max_turns: None,
            max_budget_usd: None,
            task_budget_usd: None,
            initial_task_budget_spent_usd: None,
            base_permission_mode: None,
            allow_plan_mode_tools: false,
            can_elicit: false,
            model_override: None,
        };
        for options in [
            RequestAssemblyOptions::default(),
            RequestAssemblyOptions {
                preview_only: Some(true),
                emit_instruction_events: false,
            },
        ] {
            let mut prepared = raw.as_object().unwrap().clone();
            let mut cache = JsObject::new();
            cache.insert("generation".into(), row.get("generation").unwrap().clone());
            prepared.insert("cachePlan".into(), JsValue::Object(cache));
            let original_tools = raw.get("tools").unwrap().clone();
            let mut host = ContextHost {
                prepared: JsValue::Object(prepared),
                calls: 0,
            };
            let mut state = RequestPreparationState {
                tools: vec![],
                metadata: vec![],
                plan_todo_addendum: None,
                elicitation_available: false,
                token_caps: vec![],
            };
            let control = CallControl {
                cancellation: Cancellation::default(),
                deadline: None,
            };
            // Calls the production environment lookup and production comparator;
            // no independent test comparator or pure cache helper is involved.
            let actual = assemble_with_native_collation(
                &mut state,
                &mut host,
                RequestAssemblyInput {
                    control: &control,
                    config: &config,
                    input: &input,
                    messages: &[],
                },
                options,
                &mut Observer,
            )
            .unwrap();
            assert_eq!(host.calls, 1);
            assert_eq!(
                encoded(actual.request.value().get("cachePlan").unwrap()),
                encoded(row.get("result").unwrap()),
                "case {index}, {options:?}"
            );
            assert_eq!(
                actual.request.value().get("tools"),
                Some(&original_tools),
                "native cache identity must preserve request catalog order"
            );
            cases += 1;
        }
    }
    assert_eq!(cases, 8);
    println!("NATIVE_CACHE_PRODUCTION_CASES={cases}");
}
