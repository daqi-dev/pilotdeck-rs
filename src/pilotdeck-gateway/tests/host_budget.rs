use pilotdeck_gateway::host::modules::{ModuleRouter, budget::*};
use pilotdeck_gateway::host::{HostError, HostFuture, HostHandler, ModuleError};
use pilotdeck_gateway::model_cost::{ModelPricing, PricingMap};
use pilotdeck_gateway::runtime::network::Cancellation;
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use std::sync::{Arc, Mutex};

const ORACLE: &str = include_str!("fixtures/host-budget/oracle.json");
static UNDEFINED: JsValue = JsValue::Undefined;
fn field<'a>(v: &'a JsValue, key: &str) -> &'a JsValue {
    v.get(key).unwrap_or(&UNDEFINED)
}
fn array(v: &JsValue) -> &[JsValue] {
    match v {
        JsValue::Array(a) => a,
        _ => panic!("expected array {v:?}"),
    }
}
fn text(v: &JsValue) -> String {
    v.as_string().unwrap().to_utf8().unwrap()
}
fn object(fields: impl IntoIterator<Item = (&'static str, JsValue)>) -> JsObject {
    let mut result = JsObject::new();
    for (k, v) in fields {
        result.insert(k.into(), v);
    }
    result
}
fn normalize(v: JsValue) -> JsValue {
    parse_json(&stringify_json(&v).unwrap().unwrap()).unwrap()
}
fn special(v: &JsValue, default: f64) -> Option<f64> {
    match v {
        JsValue::Number(n) => Some(*n),
        JsValue::String(s) if s.equals_str("undefined") => None,
        JsValue::String(s) if s.equals_str("NaN") => Some(f64::NAN),
        JsValue::String(s) if s.equals_str("Infinity") => Some(f64::INFINITY),
        JsValue::String(s) if s.equals_str("-Infinity") => Some(f64::NEG_INFINITY),
        _ => Some(default),
    }
}
struct Fake {
    spec: JsValue,
    calls: Arc<Mutex<Vec<JsValue>>>,
    local: Ports,
    signal: Option<Cancellation>,
}
impl Fake {
    fn record(&self, call: JsObject) -> Result<(), HostError> {
        self.calls.lock().unwrap().push(JsValue::Object(call));
        if let Some(message) = field(&self.spec, "error").as_string() {
            return Err(ModuleError::new(message.clone()).into());
        }
        Ok(())
    }
}
impl RequestInputEstimator for Fake {
    fn estimate_request_input(&self, request: JsObject) -> HostFuture<f64> {
        let recorded = self.record(object([
            ("operation", "estimate_request_input".into()),
            ("request", JsValue::Object(request.clone())),
        ]));
        let local = self
            .local
            .estimate_request_input
            .clone()
            .filter(|_| field(&self.spec, "local").as_bool() == Some(true));
        let tokens = special(field(&self.spec, "tokens"), 17.5).unwrap();
        Box::pin(async move {
            recorded?;
            match local {
                Some(p) => p.estimate_request_input(request).await,
                None => Ok(tokens),
            }
        })
    }
}
impl RequestBudgetEvaluator for Fake {
    fn evaluate_request_budget(
        &self,
        request: JsObject,
        options: EvaluationOptions,
    ) -> HostFuture<JsValue> {
        let mut raw = object([(
            "maxContextTokens",
            JsValue::Number(options.max_context_tokens),
        )]);
        if let Some(n) = options.reserved_output_tokens {
            raw.insert("reservedOutputTokens".into(), JsValue::Number(n));
        }
        if let Some(b) = options.use_provider_count {
            raw.insert("useProviderCount".into(), JsValue::Bool(b));
        }
        if let Some(calibration) = options.calibration {
            raw.insert("calibration".into(), JsValue::Object(calibration));
        }
        let signal = match (&options.signal, &self.signal) {
            (None, None) => "absent",
            (Some(actual), Some(expected)) => {
                assert!(actual.same_channel(expected));
                if actual.is_cancelled() {
                    "aborted"
                } else {
                    "active"
                }
            }
            _ => panic!("trusted signal presence changed"),
        };
        let recorded = self.record(object([
            ("operation", "evaluate_request_budget".into()),
            ("request", JsValue::Object(request)),
            ("options", JsValue::Object(raw)),
            ("signal", signal.into()),
        ]));
        let snapshot = self.spec.get("snapshot").cloned().unwrap_or_else(|| {
            parse_json(r#"{"tokens":17.5,"state":"warning","extra":{"preserved":true}}"#).unwrap()
        });
        Box::pin(async move {
            recorded?;
            Ok(snapshot)
        })
    }
}
impl UsageCostEstimator for Fake {
    fn estimate_usage_cost(
        &self,
        usage: Option<JsObject>,
        provider: JsString,
        model: JsString,
    ) -> HostFuture<Option<f64>> {
        let mut call = object([("operation", "estimate_usage_cost".into())]);
        if let Some(usage) = &usage {
            call.insert("usage".into(), JsValue::Object(usage.clone()));
        }
        call.insert("provider".into(), JsValue::String(provider.clone()));
        call.insert("model".into(), JsValue::String(model.clone()));
        let recorded = self.record(call);
        let local = self
            .local
            .estimate_usage_cost
            .clone()
            .filter(|_| field(&self.spec, "local").as_bool() == Some(true));
        let cost = special(field(&self.spec, "cost"), 0.25);
        Box::pin(async move {
            recorded?;
            match local {
                Some(p) => p.estimate_usage_cost(usage, provider, model).await,
                None => Ok(cost),
            }
        })
    }
}
fn pricing(spec: &JsValue) -> Option<PricingMap> {
    field(spec, "pricing").as_object().map(|raw| {
        let mut rates = PricingMap::new();
        for (key, value) in raw.entries().iter() {
            rates.insert(
                key.to_utf8().unwrap(),
                ModelPricing {
                    input: field(value, "input").as_number(),
                    output: field(value, "output").as_number(),
                    cache_read: field(value, "cacheRead").as_number(),
                },
            );
        }
        rates
    })
}

#[tokio::test]
async fn budget_dispatch_matches_real_ts_calls_responses_and_capabilities() {
    let oracle = parse_json(ORACLE).unwrap();
    let cases = array(field(&oracle, "cases"));
    assert_eq!(cases.len(), 338);
    for case in cases {
        let name = text(field(case, "name"));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let signal = match field(case, "signal").as_string() {
            Some(s) if !s.equals_str("absent") => Some({
                let signal = Cancellation::new();
                if s.equals_str("aborted") {
                    signal.abort();
                }
                signal
            }),
            _ => None,
        };
        let fake = Arc::new(Fake {
            spec: case.clone(),
            calls: calls.clone(),
            local: Ports::local(pricing(case)),
            signal: signal.clone(),
        });
        let capabilities = array(field(case, "capabilities"));
        let enabled = |i: usize| capabilities[i].as_bool().unwrap();
        let handler = Handler {
            ports: (field(case, "available").as_bool() != Some(false)).then(|| Ports {
                estimate_request_input: enabled(0)
                    .then(|| fake.clone() as Arc<dyn RequestInputEstimator>),
                evaluate_request_budget: enabled(1)
                    .then(|| fake.clone() as Arc<dyn RequestBudgetEvaluator>),
                estimate_usage_cost: enabled(2)
                    .then(|| fake.clone() as Arc<dyn UsageCostEstimator>),
            }),
            signal,
        };
        let manifest = handler.methods().map_or(JsValue::Null, |methods| {
            JsValue::Object(object([(
                "methods",
                JsValue::Array(methods.into_iter().map(JsValue::from).collect()),
            )]))
        });
        assert_eq!(manifest, *field(case, "manifest"), "manifest {name}");
        let mut call = field(case, "call").clone();
        if let Some(nonfinite) = case.get("nonfinite") {
            let mut payload = field(&call, "payload").as_object().unwrap().clone();
            let mut options = payload.get("options").unwrap().as_object().unwrap().clone();
            options.insert(
                text(field(nonfinite, "field")).into(),
                JsValue::Number(special(field(nonfinite, "value"), 0.0).unwrap()),
            );
            payload.insert("options".into(), JsValue::Object(options));
            let mut raw = call.as_object().unwrap().clone();
            raw.insert("payload".into(), JsValue::Object(payload));
            call = JsValue::Object(raw);
        }
        let router = ModuleRouter {
            budget: handler,
            ..ModuleRouter::default()
        };
        let response = match router.module_call(call).await {
            Ok(result) => object([
                ("ok", JsValue::Bool(true)),
                ("result", JsValue::Object(result)),
            ]),
            Err(error) => object([
                ("ok", JsValue::Bool(false)),
                (
                    "message",
                    JsValue::String(error.module_error.expect("module error").message),
                ),
            ]),
        };
        assert_eq!(
            normalize(JsValue::Object(response)),
            *field(case, "response"),
            "response {name}"
        );
        assert_eq!(
            normalize(JsValue::Array(calls.lock().unwrap().clone())),
            *field(case, "calls"),
            "port calls {name}"
        );
        router.dispose().await.unwrap();
    }
}

#[test]
fn budget_oracle_is_pinned_to_clean_baseline_blobs() {
    use sha2::{Digest, Sha256};
    use std::process::Command;
    let oracle = parse_json(ORACLE).unwrap();
    let commit = "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85";
    assert_eq!(text(field(&oracle, "sourceCommit")), commit);
    let sources = field(&oracle, "sourceSha256").as_object().unwrap();
    assert_eq!(sources.len(), 12);
    assert!(
        sources
            .get("src/agent/loop/nativeAgentTurnCapabilitiesAdapter.ts")
            .is_some()
    );
    assert!(sources.get("src/router/RouterRuntime.ts").is_some());
    for (_, hash) in sources.entries().iter() {
        assert_eq!(text(hash).len(), 64);
    }
    let Ok(baseline) = std::env::var("PDRS_BASELINE") else {
        return;
    };
    let git = |args: &[&str]| {
        let result = Command::new("git")
            .arg("-C")
            .arg(&baseline)
            .args(args)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        result.stdout
    };
    assert_eq!(
        String::from_utf8(git(&["rev-parse", "HEAD"]))
            .unwrap()
            .trim(),
        commit
    );
    assert!(git(&["status", "--porcelain", "--untracked-files=no"]).is_empty());
    for (path, hash) in sources.entries().iter() {
        let path = path.to_utf8().unwrap();
        let blob = git(&["show", &format!("{commit}:{path}")]);
        assert_eq!(format!("{:x}", Sha256::digest(&blob)), text(hash));
        assert_eq!(
            std::fs::read(std::path::Path::new(&baseline).join(path)).unwrap(),
            blob
        );
    }
}

#[test]
#[ignore = "requires PDRS_BASELINE and the baseline Node/tsx installation"]
fn budget_fixtures_regenerate_twice_identically() {
    use std::{path::PathBuf, process::Command};
    let baseline = PathBuf::from(std::env::var("PDRS_BASELINE").expect("set PDRS_BASELINE"));
    let root = std::env::temp_dir().join(format!("pdrs-host-budget-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    // Both the parent and generator output directories belong to this run.
    // Retain them on failure for diagnosis; never remove a fixed path.
    let generator =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/host-budget/generate.mts");
    let run = || {
        let output = Command::new("node")
            .arg("--import")
            .arg(baseline.join("node_modules/tsx/dist/loader.mjs"))
            .arg(&generator)
            .arg(&root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        let result = parse_json(stdout.lines().last().unwrap()).unwrap();
        let directory = PathBuf::from(text(field(&result, "output")));
        assert!(directory.starts_with(&root));
        let bytes = std::fs::read(directory.join("oracle.json")).unwrap();
        (directory, bytes)
    };
    let (first_dir, first) = run();
    let (second_dir, second) = run();
    assert_ne!(first_dir, second_dir);
    assert_eq!(first, second, "consecutive TS oracle generations differ");
    assert_eq!(
        first,
        ORACLE.as_bytes(),
        "committed oracle differs from real TS"
    );
    println!(
        "HOST_BUDGET_REGEN_PASS cases={} runs=2 oracle_bytes={}",
        array(field(&parse_json(ORACLE).unwrap(), "cases")).len(),
        first.len()
    );
}
