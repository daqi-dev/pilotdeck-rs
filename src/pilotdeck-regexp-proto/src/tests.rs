use super::*;
use serde_json::{Value as Json, json};

fn units(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}
fn engine() -> Engine {
    Engine::new(Limits::default()).expect("#1265 engine setup")
}
fn run(request: &mut Request<'_>, pattern: &str, input: &str) -> Result<Outcome, Error> {
    request.exec(&units(pattern), "u", &units(input), 0)
}
fn numbers(value: &Json) -> Vec<u16> {
    value
        .as_array()
        .expect("#1265 fixture array")
        .iter()
        .map(|x| {
            u16::try_from(x.as_u64().expect("#1265 fixture integer")).expect("#1265 fixture u16")
        })
        .collect()
}
fn observed(result: Result<Outcome, Error>) -> Json {
    match result {
        Ok(outcome) => {
            json!({"lastIndex":outcome.last_index,"matched":outcome.matched.map(|m|json!({
                "range":m.range,"captures":m.captures,"groups":m.groups,"indices":m.indices.map(|i|json!({"captures":i.captures,"groups":i.groups}))
            }))})
        }
        Err(Error::Engine {
            phase: Phase::Compile,
            name,
            ..
        }) => json!({"error":name}),
        other => panic!("#1265 unexpected oracle error: {other:?}"),
    }
}
fn oracle() -> Json {
    serde_json::from_str(include_str!(
        "../../pilotdeck-gateway/tests/fixtures/rquickjs-regexp/oracle.json"
    ))
    .expect("#1265 parse oracle")
}
#[test]
fn node_semantics() {
    let oracle = oracle();
    assert_eq!(oracle["node"], "v22.22.0", "#1265 Node provenance");
    assert_eq!(oracle["icu"], "78.2", "#1265 ICU provenance");
    let mut engine = engine();
    let mut request = engine.request(1_000_000);
    let cases = oracle["cases"].as_array().expect("#1265 semantic corpus");
    assert_eq!(cases.len(), 100, "#1265 full semantic corpus");
    for case in cases {
        let result = request.exec(
            &numbers(&case["pattern"]),
            case["flags"].as_str().expect("#1265 flags"),
            &numbers(&case["input"]),
            case["start"].as_u64().expect("#1265 start") as u32,
        );
        assert_eq!(
            observed(result),
            case["expected"],
            "#1265 semantics {}",
            case["id"]
        );
    }
}
#[test]
fn sdk_108() {
    let oracle = oracle();
    let mut engine = engine();
    let mut request = engine.request(1_000_000);
    let cases = oracle["sdk"].as_array().expect("#1265 SDK corpus");
    assert_eq!(cases.len(), 108, "#1265 full SDK corpus");
    let mut schema_uri_errors = 0;
    for case in cases {
        let result = request.exec(&numbers(&case["pattern"]), "u", &numbers(&case["input"]), 0);
        assert_eq!(
            observed(result.clone()),
            case["regexp"],
            "#1265 SDK regex semantics {}",
            case["id"]
        );
        if case["expected"]["error"] == "URIError" {
            assert!(
                matches!(
                    case["id"].as_str(),
                    Some(
                        "utf16/raw-lone-pattern/key/positive"
                            | "utf16/raw-lone-pattern/key/negative"
                    )
                ),
                "#1265 known SDK URI boundary only"
            );
            schema_uri_errors += 1;
            continue;
        }
        let got = match result {
            Ok(outcome) => json!({"matched":outcome.matched.is_some()}),
            Err(Error::Engine {
                phase: Phase::Compile,
                name,
                ..
            }) => json!({"error":name}),
            other => panic!("#1265 SDK unexpected result {}: {other:?}", case["id"]),
        };
        assert_eq!(got, case["expected"], "#1265 SDK {}", case["id"]);
    }
    assert_eq!(
        schema_uri_errors, 2,
        "#1265 two SDK-layer URI errors remain outside RegExp"
    );
}
#[test]
fn arbitrary_u16_roundtrip() {
    let mut engine = engine();
    let mut request = engine.request(1_000_000);
    let input: Vec<u16> = (0..=u16::MAX).collect();
    let result = request
        .exec(&units("[\\s\\S]*"), "", &input, 0)
        .expect("#1265 all u16 exec");
    let matched = result.matched.expect("#1265 all u16 match");
    assert_eq!(
        matched.captures,
        vec![Some(input)],
        "#1265 all u16 preserved"
    );
    // All isolated surrogates also pass through the pattern bridge unchanged.
    for unit in 0xd800..=0xdfff {
        let result = request
            .exec(&[unit], "u", &[unit], 0)
            .expect("#1265 surrogate pattern exec");
        assert_eq!(
            result
                .matched
                .expect("#1265 surrogate pattern match")
                .captures,
            vec![Some(vec![unit])],
            "#1265 surrogate pattern preserved"
        );
    }
}
#[test]
fn budget_tristate_and_reuse() {
    let mut engine = engine();
    let bad = format!("{}!", "a".repeat(18));
    {
        let mut request = engine.request(1);
        assert!(
            run(&mut request, "a", "a")
                .expect("#1265 short match")
                .matched
                .is_some(),
            "#1265 short matched"
        );
        assert_eq!(request.polls(), 0, "#1265 short needs no poll");
        assert_eq!(
            run(&mut request, "^(a+)+$", &bad),
            Err(Error::BudgetExhausted),
            "#1265 native budget exhausted"
        );
        assert_eq!(
            request.polls(),
            2,
            "#1265 native matcher polled past allowance"
        );
        assert_eq!(
            run(&mut request, "a", "a"),
            Err(Error::BudgetExhausted),
            "#1265 exhaustion stays sticky"
        );
    }
    let mut request = engine.request(1_000_000);
    assert!(
        run(&mut request, "^(a+)+$", &bad)
            .expect("#1265 high budget completes")
            .matched
            .is_none(),
        "#1265 completed no match"
    );
    assert!(request.polls() > 1, "#1265 matcher spends polls");
    assert!(
        run(&mut request, "^(a+)+$", "aaaa")
            .expect("#1265 after interrupt succeeds")
            .matched
            .is_some(),
        "#1265 after interrupt matched"
    );
}
#[test]
fn shared_budget_and_new_request() {
    let mut engine = engine();
    let bad = format!("{}!", "a".repeat(18));
    let used = {
        let mut request = engine.request(1_000_000);
        run(&mut request, "^(a+)+$", &bad).expect("#1265 calibrate finite budget sample");
        request.polls()
    };
    assert!(used > 1, "#1265 calibration observes actual matcher polls");
    {
        let mut request = engine.request(used);
        run(&mut request, "^(a+)+$", &bad).expect("#1265 exact poll allowance finishes");
        assert_eq!(request.polls(), used, "#1265 exact poll allowance spent");
        assert_eq!(
            run(&mut request, "^(a+)+$", &bad),
            Err(Error::BudgetExhausted),
            "#1265 shared second call exhausts"
        );
    }
    let mut request = engine.request(used);
    assert_eq!(request.polls(), 0, "#1265 new request starts fresh");
    assert!(
        run(&mut request, "^(a+)+$", &bad)
            .expect("#1265 new request restored budget")
            .matched
            .is_none(),
        "#1265 new request completes"
    );
}
#[test]
fn cancellation_and_stale_token() {
    let mut engine = engine();
    let stale = {
        let mut request = engine.request(1000);
        let token = request.cancellation();
        token.cancel();
        assert_eq!(
            run(&mut request, "a", "a"),
            Err(Error::Cancelled),
            "#1265 cancelled before call"
        );
        token
    };
    stale.cancel();
    {
        let mut request = engine.request(1000);
        assert!(
            run(&mut request, "a", "a")
                .expect("#1265 stale token isolation")
                .matched
                .is_some(),
            "#1265 stale token cannot cancel successor"
        );
        request.budget.cancel_at_poll.store(2, Ordering::SeqCst);
        let bad = format!("{}!", "a".repeat(18));
        assert_eq!(
            run(&mut request, "^(a+)+$", &bad),
            Err(Error::Cancelled),
            "#1265 cancellation during native exec"
        );
        assert_eq!(request.polls(), 2, "#1265 deterministic cancel poll");
    }
    assert!(
        run(&mut engine.request(1000), "a", "a")
            .expect("#1265 after cancel reuse")
            .matched
            .is_some(),
        "#1265 after cancel match"
    );
}
#[test]
fn cancellation_during_decode() {
    let mut engine = engine();
    let input = vec![0x4e00; 1024];
    for unit in [1, 512, 1024] {
        let mut request = engine.request(1000);
        request
            .budget
            .cancel_at_decode_unit
            .store(unit, Ordering::SeqCst);
        assert_eq!(
            request.exec(&units("[\\s\\S]*"), "", &input, 0),
            Err(Error::Cancelled),
            "#1265 decode cancellation wins at unit {unit}"
        );
        assert_eq!(
            request.exec(&units("a"), "", &[97], 0),
            Err(Error::Cancelled),
            "#1265 decode cancellation remains sticky"
        );
    }
    let outcome = engine
        .request(1000)
        .exec(&units("[\\s\\S]*"), "", &input, 0)
        .expect("#1265 decode successor succeeds");
    assert_eq!(
        outcome
            .matched
            .expect("#1265 decode successor matches")
            .captures,
        vec![Some(input)],
        "#1265 decode successor preserves all units"
    );
}
#[test]
fn concurrent_runtimes() {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let mut starts = Vec::new();
    let handles: Vec<_> = [1, 1_000_000]
        .into_iter()
        .map(|limit| {
            let (start_tx, start_rx) = std::sync::mpsc::channel();
            starts.push(start_tx);
            let ready_tx = ready_tx.clone();
            std::thread::spawn(move || {
                let mut engine = engine();
                ready_tx.send(()).expect("#1265 runtime ready signal");
                start_rx
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .expect("#1265 bounded concurrent start");
                run(
                    &mut engine.request(limit),
                    "^(a+)+$",
                    &format!("{}!", "a".repeat(18)),
                )
            })
        })
        .collect();
    for _ in 0..2 {
        ready_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("#1265 bounded runtime ready");
    }
    for start in starts {
        start.send(()).expect("#1265 publish concurrent start");
    }
    let mut results = handles
        .into_iter()
        .map(|h| h.join().expect("#1265 concurrent runtime joins"));
    assert_eq!(
        results.next().expect("#1265 low runtime"),
        Err(Error::BudgetExhausted),
        "#1265 low runtime interrupted"
    );
    assert!(
        results
            .next()
            .expect("#1265 high runtime")
            .expect("#1265 high runtime result")
            .matched
            .is_none(),
        "#1265 high runtime isolated"
    );
}
#[test]
fn input_and_output_limits() {
    let mut engine = Engine::new(Limits {
        pattern_units: 3,
        input_units: 3,
        output_units: 3,
        ..Limits::default()
    })
    .expect("#1265 small limits engine");
    let mut request = engine.request(1000);
    assert_eq!(
        run(&mut request, "aaaa", "a"),
        Err(Error::Limit("pattern")),
        "#1265 pattern limit"
    );
    assert_eq!(
        run(&mut request, "a", "aaaa"),
        Err(Error::Limit("input")),
        "#1265 input limit"
    );
    assert_eq!(
        run(&mut request, "(a)", "aa"),
        Ok(Outcome {
            matched: Some(Match {
                range: [0, 1],
                captures: vec![Some(vec![97]), Some(vec![97])],
                groups: None,
                indices: None
            }),
            last_index: 0
        }),
        "#1265 capture within output limit"
    );
    assert_eq!(
        run(&mut request, "(.)", "aa"),
        run(&mut request, "(a)", "aa"),
        "#1265 matched capture shape"
    );
    let mut engine = Engine::new(Limits {
        output_units: 1,
        ..Limits::default()
    })
    .expect("#1265 output limit engine");
    assert_eq!(
        run(&mut engine.request(1000), "(a)", "a"),
        Err(Error::Limit("output")),
        "#1265 output multiplicity bounded"
    );
}
#[test]
fn invalid_limits_and_zero_budget() {
    for limits in [
        Limits {
            memory_bytes: 0,
            ..Limits::default()
        },
        Limits {
            stack_bytes: 0,
            ..Limits::default()
        },
        Limits {
            stack_bytes: 16 * 1024 * 1024 + 1,
            ..Limits::default()
        },
        Limits {
            pattern_units: 0,
            ..Limits::default()
        },
        Limits {
            input_units: 0,
            ..Limits::default()
        },
        Limits {
            output_units: 0,
            ..Limits::default()
        },
    ] {
        assert!(
            matches!(Engine::new(limits), Err(Error::InvalidLimits)),
            "#1265 reject unlimited configuration"
        );
    }
    assert_eq!(
        run(&mut engine().request(0), "a", "a"),
        Err(Error::BudgetExhausted),
        "#1265 zero budget refuses before native call"
    );
}
#[test]
fn closed_context() {
    let engine = engine();
    engine.context.with(|ctx| {
        for name in [
            "std",
            "os",
            "require",
            "process",
            "fetch",
            "WebAssembly",
            "Promise",
        ] {
            assert!(
                ctx.globals()
                    .get::<_, Value>(name)
                    .expect("#1265 inspect closed context")
                    .is_undefined(),
                "#1265 unavailable global {name}"
            );
        }
        let eval: Function = ctx.globals().get("eval").expect("#1265 native eval entry");
        let evaluated: rquickjs::Result<i32> = eval.call(("40+2",));
        assert!(evaluated.is_err(), "#1265 eval disabled");
        let _ = ctx.catch();
        let ctor: Constructor = ctx
            .globals()
            .get("Function")
            .expect("#1265 native Function constructor");
        let result: rquickjs::Result<Object> = ctor.construct(("return 42",));
        assert!(
            result.is_err(),
            "#1265 arbitrary source evaluation disabled"
        );
        let _ = ctx.catch();
    });
}

// Isolate compilation from the u16 bridge. Limits are changed outside with()
// (rquickjs holds the runtime lock during with()). No input can execute JS here.
fn compile_probe(engine: &Engine, pattern: &str) -> Result<(), Error> {
    engine.context.with(|ctx| {
        let pattern =
            from_units(&ctx, &units(pattern)).map_err(|e| engine_error(&ctx, Phase::Bridge, e))?;
        ctx.globals()
            .set("probePattern", pattern)
            .map_err(|e| engine_error(&ctx, Phase::Bridge, e))
    })
}
fn compile_prepared(engine: &Engine) -> Result<(), Error> {
    engine.context.with(|ctx| {
        let ctor: Constructor = ctx
            .globals()
            .get("RegExp")
            .map_err(|e| engine_error(&ctx, Phase::Compile, e))?;
        let pattern: Value = ctx
            .globals()
            .get("probePattern")
            .map_err(|e| engine_error(&ctx, Phase::Compile, e))?;
        ctor.construct::<_, Object>((pattern, "u"))
            .map(|_| ())
            .map_err(|e| engine_error(&ctx, Phase::Compile, e))
    })
}
#[test]
fn compilation_resources() {
    let engine = engine();
    assert_eq!(
        engine.runtime.memory_usage().malloc_limit,
        64 * 1024 * 1024,
        "#1265 configured native memory limit"
    );
    let nested = format!("{}a{}", "(?:".repeat(300), ")".repeat(300));
    compile_probe(&engine, &nested).expect("#1265 prepare nested pattern");
    compile_prepared(&engine).expect("#1265 nested high stack control");
    engine.runtime.set_max_stack_size(16 * 1024);
    let low_stack = compile_prepared(&engine);
    println!("low_stack={low_stack:?}");
    assert!(
        matches!(
            low_stack,
            Err(Error::Engine { phase: Phase::Compile, name, detail })
                if name == "SyntaxError" && detail.contains("stack overflow")
        ),
        "#1265 low stack rejects compilation"
    );
    compile_probe(&engine, "a").expect("#1265 low stack flat pattern bridge");
    compile_prepared(&engine).expect("#1265 low stack flat pattern compiles");
    engine
        .runtime
        .set_max_stack_size(Limits::default().stack_bytes);
    compile_probe(&engine, &nested).expect("#1265 restore nested pattern");
    compile_prepared(&engine).expect("#1265 restore stack compilation");
    let wide = "a".repeat(32_768);
    compile_probe(&engine, &wide).expect("#1265 prepare allocation pattern");
    compile_prepared(&engine).expect("#1265 high memory compilation");
    let used = engine.runtime.memory_usage().malloc_size as usize;
    engine.runtime.set_memory_limit(used + 4096);
    let low_memory = compile_prepared(&engine);
    println!("low_memory={low_memory:?}");
    assert!(
        matches!(
            low_memory,
            Err(Error::Engine { phase: Phase::Compile, name, detail })
                if name == "SyntaxError" && detail.contains("out of memory")
        ),
        "#1265 low memory rejects compilation"
    );
    engine
        .runtime
        .set_memory_limit(Limits::default().memory_bytes);
    compile_prepared(&engine).expect("#1265 restore memory compilation");
    for pattern in ["a{2147483647}", "(a){1000000}", "(?:a?){1000000}"] {
        compile_probe(&engine, pattern).expect("#1265 prepare finite quantifier");
        let result = compile_prepared(&engine);
        println!("quantifier={pattern} result={result:?}");
        assert!(
            result.is_ok(),
            "#1265 finite quantifier compilation remains bounded"
        );
    }
}

fn regenerates_identically(fixture: &str) {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("#1265 src directory")
        .parent()
        .expect("#1265 repository directory");
    let result = std::process::Command::new("python3")
        .arg(root.join("src/pilotdeck-gateway/tests/fixtures/rquickjs-regexp/regenerate.py"))
        .arg(fixture)
        .output()
        .expect("#1265 launch bounded fixture regeneration");
    assert!(
        result.status.success(),
        "#1265 regeneration identical: stdout={} stderr={}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    println!("{}", String::from_utf8_lossy(&result.stdout));
}
#[test]
#[ignore = "requires fixed Node v22.22.0 / ICU 78.2 and PDRS_BASELINE"]
fn fixture_regenerates_identically() {
    regenerates_identically("rquickjs-regexp");
}

#[test]
fn configured_stack_limit() {
    let pattern = format!("{}a{}", "(?:".repeat(300), ")".repeat(300));
    let small = Engine::new(Limits {
        stack_bytes: 16 * 1024,
        ..Limits::default()
    })
    .expect("#1265 low configured stack engine");
    // Bypass JS string-bridge calls: at 16 KiB those can fail before RegExp
    // compilation. This test must observe the parser's own stack check.
    small
        .context
        .with(|ctx| ctx.globals().set("probePattern", pattern.as_str()))
        .expect("#1265 prepare configured stack pattern");
    let result = compile_prepared(&small);
    println!("configured_stack={result:?}");
    assert!(
        matches!(
            result,
            Err(Error::Engine { phase: Phase::Compile, name, detail })
                if name == "SyntaxError" && detail.contains("stack overflow")
        ),
        "#1265 configured stack rejects before exhausting host"
    );
    let mut normal = engine();
    normal
        .context
        .with(|ctx| ctx.globals().set("probePattern", pattern.as_str()))
        .expect("#1265 prepare normal stack pattern");
    compile_prepared(&normal).expect("#1265 normal stack compiles identical prepared pattern");
    assert!(
        run(&mut normal.request(1000), &pattern, "a")
            .expect("#1265 normal stack same pattern")
            .matched
            .is_some(),
        "#1265 normal stack matches"
    );
}

#[test]
fn flags_limit() {
    assert!(
        matches!(
            engine().request(1000).exec(&units("a"), "dgimsuvy", &units("a"), 0),
            Err(Error::Engine { phase: Phase::Compile, name, .. }) if name == "SyntaxError"
        ),
        "#1265 eight flags reach compiler (u and v conflict)"
    );
    assert_eq!(
        engine()
            .request(1000)
            .exec(&units("a"), "ggggggggg", &units("a"), 0),
        Err(Error::Limit("flags")),
        "#1265 flags length limit"
    );
}
#[test]
fn large_quantifier_budget() {
    let mut engine = engine();
    assert_eq!(
        run(&mut engine.request(1), "^(?:a?){1000000}$", ""),
        Err(Error::BudgetExhausted),
        "#1265 large finite quantifier interrupts"
    );
}

#[test]
fn capture_count_boundary() {
    let mut engine = engine();
    let oracle = oracle();
    let boundaries = oracle["boundaries"]
        .as_array()
        .expect("#1265 capture boundary corpus");
    assert_eq!(boundaries.len(), 2, "#1265 both sides of capture boundary");
    for (index, case) in boundaries.iter().enumerate() {
        assert!(
            case["expected"]["matched"].is_object(),
            "#1265 Node accepts capture count"
        );
        let result = engine
            .request(1000)
            .exec(&numbers(&case["pattern"]), "u", &[], 0);
        if index == 0 {
            assert_eq!(
                observed(result),
                case["expected"],
                "#1265 254 captures align"
            );
        } else {
            println!("Node accepts 255 captures; QuickJS result={result:?}");
            assert!(
                matches!(result,Err(Error::Engine{phase:Phase::Compile,name,detail}) if name=="SyntaxError" && detail.contains("too many captures")),
                "#1265 255 captures remain explicit unsupported boundary"
            );
        }
    }
}
#[test]
fn unanchored_budget() {
    let mut engine = engine();
    let input = format!("{}!", "a".repeat(18));
    assert_eq!(
        run(&mut engine.request(1), "(a+)+b", &input),
        Err(Error::BudgetExhausted),
        "#1265 budget covers candidate starts"
    );
    assert!(
        run(&mut engine.request(1_000_000), "(a+)+b", &input)
            .expect("#1265 high budget unanchored search")
            .matched
            .is_none(),
        "#1265 unanchored no match is distinct"
    );
}

#[test]
fn length_limit_boundaries() {
    let mut engine = Engine::new(Limits {
        pattern_units: 3,
        input_units: 3,
        output_units: 2,
        ..Limits::default()
    })
    .expect("#1265 boundary engine setup");
    let mut request = engine.request(1000);
    assert!(
        run(&mut request, "a", "aaa")
            .expect("#1265 input exact limit accepted")
            .matched
            .is_some(),
        "#1265 input exact limit matches"
    );
    let result =
        run(&mut request, "(a)", "a").expect("#1265 pattern and output exact limits accepted");
    assert_eq!(
        result.matched.expect("#1265 exact output matched").captures,
        vec![Some(vec![97]), Some(vec![97])],
        "#1265 output exact limit preserved"
    );
}

#[test]
fn memory_limit_boundary() {
    assert!(
        matches!(
            Engine::new(Limits {
                memory_bytes: 1024 * 1024 - 1,
                ..Limits::default()
            }),
            Err(Error::InvalidLimits)
        ),
        "#1265 memory below minimum rejected"
    );
    let mut engine = Engine::new(Limits {
        memory_bytes: 1024 * 1024,
        ..Limits::default()
    })
    .expect("#1265 memory minimum initializes");
    assert!(
        run(&mut engine.request(1000), "a", "a")
            .expect("#1265 memory minimum executes")
            .matched
            .is_some(),
        "#1265 memory minimum matches"
    );
}

#[test]
fn stack_limit_boundaries() {
    assert!(
        matches!(
            Engine::new(Limits {
                stack_bytes: 16 * 1024 - 1,
                ..Limits::default()
            }),
            Err(Error::InvalidLimits)
        ),
        "#1265 stack below minimum rejected"
    );
    assert!(
        matches!(
            Engine::new(Limits {
                stack_bytes: 16 * 1024 * 1024 + 1,
                ..Limits::default()
            }),
            Err(Error::InvalidLimits)
        ),
        "#1265 stack above maximum rejected"
    );
    // The native ceiling does not grow the host stack. Leave headroom for Rust
    // and the bridge even when testing the largest permitted configuration.
    let results = std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .spawn(|| {
            [16 * 1024, 16 * 1024 * 1024].map(|stack_bytes| {
                Engine::new(Limits {
                    stack_bytes,
                    ..Limits::default()
                })
                .map(|_| ())
            })
        })
        .expect("#1265 stack endpoint thread starts")
        .join()
        .expect("#1265 stack endpoint thread joins");
    for result in results {
        result.expect("#1265 stack endpoints initialize");
    }
}

#[test]
fn default_input_output_limits() {
    let input = vec![0x4e00; 1_048_576];
    let mut engine = engine();
    let outcome = engine
        .request(1_000_000)
        .exec(&units("[\\s\\S]*"), "", &input, 0)
        .expect("#1265 default full input and output execute");
    let matched = outcome.matched.expect("#1265 default full input matches");
    assert_eq!(
        matched.range,
        [0, input.len()],
        "#1265 default full match range"
    );
    assert_eq!(
        matched.captures,
        vec![Some(input)],
        "#1265 default full output preserved"
    );
}

#[test]
fn fixture_content_hashes() {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../pilotdeck-gateway/tests/fixtures/rquickjs-regexp");
    // Byte guards run without Node or a baseline checkout. Regeneration still
    // needs the ignored test; updating these pins requires that evidence.
    for (name, digest) in [
        (
            "oracle.json",
            "11e65ce0f786f3c8ba2ca7d818d0cccbea705d6990600107f4a21ee702cb4529",
        ),
        (
            "regress-oracle.json",
            "46506384dd371a8059ac486527896e4f47afc01c652a1c72ffaa116ab7745690",
        ),
        (
            "generate.mjs",
            "a6e50cd672eec1d954d87a807bf2be18cab88764727a1ae66fbb4af6b6341ac5",
        ),
        (
            "generate-sdk.mjs",
            "f9f23f11946ef567f2b53987dc5765d2ab6f700cb99348d5236cd5c3b2325592",
        ),
        (
            "report-cases.json",
            "253b1c1bc4c08eae2eb3469afd8b9fa3fad5b1ad56e48000ac7128341bef6742",
        ),
        (
            "regenerate.py",
            "8713eb61bca98d485b5c821691b7139c2c8a19d8674d5c20309bc1761303f92c",
        ),
    ] {
        let result = std::process::Command::new("python3")
            .args(["-I", "-c", "import hashlib, pathlib, sys; print(hashlib.sha256(pathlib.Path(sys.argv[1]).read_bytes()).hexdigest())"])
            .arg(fixture.join(name)).output().expect("#1265 launch fixture digest check");
        assert!(
            result.status.success(),
            "#1265 read fixture digest {name}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&result.stdout).trim(),
            digest,
            "#1265 fixture digest {name}"
        );
    }
}
