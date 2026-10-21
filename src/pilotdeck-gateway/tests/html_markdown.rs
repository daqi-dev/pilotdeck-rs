#[path = "common/baseline.rs"]
mod baseline;
#[path = "common/scratch.rs"]
mod scratch;

use pilotdeck_gateway::runtime::tools::builtin::web::{
    HtmlToMarkdown, UrlFetcher,
    html_markdown::{DOM_LIMIT_ERROR, MAX_DOM_DEPTH, Turndown},
};

const ORACLE: &str = include_str!("fixtures/html-markdown/oracle.json");

#[test]
fn default_turndown_matches_real_ts_in_order() {
    baseline::oracle(ORACLE);
    let oracle: serde_json::Value = serde_json::from_str(ORACLE).unwrap();
    let cases = oracle["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 108);
    let mut failures = Vec::new();
    for (index, case) in cases.iter().enumerate() {
        let expected = case["markdown"].as_str().unwrap();
        let actual = Turndown.convert(case["html"].as_str().unwrap()).unwrap();
        if actual.as_bytes() != expected.as_bytes() {
            failures.push(format!(
                "{index}: {}: expected {expected:?}, actual {actual:?}",
                case["name"]
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn url_fetcher_installs_the_real_default_converter() {
    let html = "<h1>😀</h1>";
    let transport = pilotdeck_gateway::runtime::model::http::HyperModelTransport::default();
    assert_eq!(
        UrlFetcher::new(std::sync::Arc::new(transport))
            .html
            .convert(html)
            .unwrap(),
        "😀\n=="
    );
}

#[test]
fn deep_dom_conversion_and_drop_do_not_recurse() {
    let payload = "x".repeat(1024 * 1024);
    let html = format!(
        "{}{payload}{}",
        "<span>".repeat(MAX_DOM_DEPTH - 3),
        "</span>".repeat(MAX_DOM_DEPTH - 3)
    );
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(move || assert_eq!(Turndown.convert(&html).unwrap(), payload))
        .unwrap()
        .join()
        .unwrap();
}

#[test]
#[ignore = "pr9c: newer HTML5 parser rules differ from domino; not accepted differences"]
fn parser_version_gaps_match_real_ts() {
    baseline::oracle(ORACLE);
    let oracle: serde_json::Value = serde_json::from_str(ORACLE).unwrap();
    let cases = oracle["parserGaps"].as_array().unwrap();
    assert_eq!(cases.len(), 2);
    let mut failures = Vec::new();
    for case in cases {
        let actual = Turndown.convert(case["html"].as_str().unwrap()).unwrap();
        let expected = case["markdown"].as_str().unwrap();
        if actual != expected {
            failures.push(format!(
                "{}: expected {expected:?}, actual {actual:?}",
                case["name"]
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
#[ignore = "requires PDRS_BASELINE and permission to execute the shared git source guard"]
fn regenerates_twice_identically() {
    let baseline = std::path::PathBuf::from(std::env::var("PDRS_BASELINE").unwrap());
    for (script, expected, file) in [
        ("generate.mts", ORACLE, "oracle.json"),
        (
            "generate-depth.mts",
            include_str!("fixtures/html-markdown/depth-oracle.json"),
            "depth-oracle.json",
        ),
        (
            "generate-string-limit.mts",
            include_str!("fixtures/html-markdown/string-limit-oracle.json"),
            "string-limit-oracle.json",
        ),
        (
            "generate-large.mts",
            include_str!("fixtures/html-markdown/large-oracle.json"),
            "large-oracle.json",
        ),
    ] {
        for _ in 0..2 {
            let scratch = scratch::Scratch::new("html-markdown");
            let output = std::process::Command::new("node")
                .arg("--import")
                .arg(baseline.join("node_modules/tsx/dist/loader.mjs"))
                .arg(
                    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("tests/fixtures/html-markdown")
                        .join(script),
                )
                .arg(scratch.path())
                .env("LANG", "en_US.UTF-8")
                .env_remove("LC_ALL")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                std::fs::read(scratch.path().join(file)).unwrap(),
                expected.as_bytes()
            );
        }
    }
}

#[test]
fn depth_budget_boundaries_and_review_shapes_match_real_ts() {
    let raw = include_str!("fixtures/html-markdown/depth-oracle.json");
    baseline::oracle(raw);
    let oracle: serde_json::Value = serde_json::from_str(raw).unwrap();
    for case in oracle["cases"].as_array().unwrap() {
        let result = Turndown.convert(case["html"].as_str().unwrap());
        if case["depth"].as_u64().unwrap() <= MAX_DOM_DEPTH as u64 {
            assert_eq!(
                result.unwrap().as_bytes(),
                case["markdown"].as_str().unwrap().as_bytes(),
                "{}",
                case["name"]
            );
        } else {
            // Conservative difference: TS still succeeds at 257; Rust uses
            // the exact web_fetch overflow error recorded at 1500 groups.
            assert_eq!(result.unwrap_err(), DOM_LIMIT_ERROR, "{}", case["name"]);
        }
    }
    for case in oracle["extra"].as_array().unwrap() {
        assert_eq!(
            Turndown.convert(case["html"].as_str().unwrap()).unwrap(),
            case["markdown"].as_str().unwrap(),
            "{}",
            case["name"]
        );
    }
}

#[test]
fn depth_budget_rejects_deep_non_list_shapes() {
    for (open, close) in [
        ("<div>", "</div>"),
        ("<template>", "</template>"),
        ("<svg><g>", "</g></svg>"),
        ("<ul><li>", "</ul></li>"),
        ("<ol><li>", "</ul></li>"),
    ] {
        let html = format!("{}x{}", open.repeat(100_000), close.repeat(100_000));
        assert_eq!(Turndown.convert(&html).unwrap_err(), DOM_LIMIT_ERROR);
    }
    // Token-looking raw text and quoted attributes are not DOM nesting.
    let raw = "<div>".repeat(1000);
    assert!(
        Turndown
            .convert(&format!("<script>{raw}</script><img title='{raw}'>"))
            .is_ok()
    );
}

#[test]
fn bounded_html_conversion_performance() {
    let started = std::time::Instant::now();
    let depth = (MAX_DOM_DEPTH - 3) / 2;
    // A large leaf with many line breaks makes the old ancestor replace scan
    // and recopy the expanded indentation at every level (not just small x).
    let payload = format!("{}x", "<br>".repeat(90_000));
    for (open, close) in [
        ("<ul><li>", "</li></ul>"),
        ("<ol start=9007199254740991><li>", "</li></ol>"),
    ] {
        let html = format!("{}{payload}{}", open.repeat(depth), close.repeat(depth));
        let output = Turndown.convert(&html).unwrap();
        assert!(output.len() > 40_000_000);
    }
    let flat = format!("<div>{}</div>", "x".repeat(10 * 1024 * 1024 - 11));
    assert_eq!(
        Turndown.convert(&flat).unwrap().len(),
        10 * 1024 * 1024 - 11
    );
    let budget = if cfg!(debug_assertions) { 20.0 } else { 1.0 };
    assert!(
        started.elapsed().as_secs_f64() < budget,
        "conversion took {:?}, budget {budget}s",
        started.elapsed()
    );
    eprintln!("bounded list + 10 MiB flat HTML: {:?}", started.elapsed());
}

// Recipes keep multi-MiB HTML and Markdown out of the repository. The generator
// losslessly expands and byte-compares each Markdown recipe with real TS output.
fn expand_large_recipe(rule: &serde_json::Value) -> String {
    if let Some(text) = rule.as_str() {
        return text.to_owned();
    }
    rule["parts"]
        .as_array()
        .unwrap()
        .iter()
        .map(expand_large_recipe)
        .collect::<String>()
        .repeat(rule["repeat"].as_u64().unwrap() as usize)
}

#[test]
fn large_shallow_pages_match_real_ts_bytes_and_stay_fast() {
    use sha2::{Digest, Sha256};
    let raw = include_str!("fixtures/html-markdown/large-oracle.json");
    baseline::oracle(raw);
    let oracle: serde_json::Value = serde_json::from_str(raw).unwrap();
    let cases = oracle["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 4);
    let mut failures = Vec::new();
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let html = expand_large_recipe(&case["html"]);
        let expected = expand_large_recipe(&case["markdown"]);
        assert_eq!(html.len() as u64, case["htmlBytes"].as_u64().unwrap());
        assert_eq!(
            expected.len() as u64,
            case["markdownBytes"].as_u64().unwrap()
        );
        assert_eq!(
            format!("{:x}", Sha256::digest(html.as_bytes())),
            case["htmlSha256"].as_str().unwrap()
        );
        assert_eq!(
            format!("{:x}", Sha256::digest(expected.as_bytes())),
            case["markdownSha256"].as_str().unwrap()
        );
        let started = std::time::Instant::now();
        let actual = Turndown.convert(&html);
        let elapsed = started.elapsed();
        eprintln!("{name}: {} HTML bytes, {:?}", html.len(), elapsed);
        match actual {
            Ok(actual) => {
                assert!(
                    actual.as_bytes() == expected.as_bytes(),
                    "{name}: TS bytes differ"
                );
                // ~2.1 million DOM nodes at 10 MiB: allow scheduler/allocator
                // variance while keeping conversion in single-digit seconds.
                let budget = if cfg!(debug_assertions) { 30.0 } else { 5.0 };
                assert!(
                    elapsed.as_secs_f64() < budget,
                    "{name}: took {elapsed:?}, budget {budget}s"
                );
            }
            Err(error) => failures.push(format!("{name}: {error}")),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn ten_mib_of_flat_inline_siblings_stays_fast() {
    // TS's single wide sibling run has expensive string joins. Record 50,000
    // tags in TS, then scale that byte-verified period to a full 10 MiB in Rust.
    let raw = include_str!("fixtures/html-markdown/large-oracle.json");
    baseline::oracle(raw);
    let oracle: serde_json::Value = serde_json::from_str(raw).unwrap();
    let case = &oracle["cases"][3];
    assert_eq!(case["name"], "dense-inline-50000");
    let sampled = expand_large_recipe(&case["markdown"]);
    let period = &sampled[..3];
    assert_eq!(sampled, period.repeat(50_000));
    let html = "<em>x</em>".repeat(1024 * 1024);
    assert_eq!(html.len(), 10 * 1024 * 1024);
    let started = std::time::Instant::now();
    let actual = Turndown.convert(&html).unwrap();
    let elapsed = started.elapsed();
    assert!(actual.as_bytes() == period.repeat(1024 * 1024).as_bytes());
    let budget = if cfg!(debug_assertions) { 30.0 } else { 5.0 };
    eprintln!(
        "dense-inline-10mib-flat: {} HTML bytes, {elapsed:?}",
        html.len()
    );
    assert!(
        elapsed.as_secs_f64() < budget,
        "flat inline: took {elapsed:?}, budget {budget}s"
    );
}

#[test]
fn nested_code_and_flanks_match_real_ts_bytes() {
    let raw = include_str!("fixtures/html-markdown/depth-oracle.json");
    baseline::oracle(raw);
    let oracle: serde_json::Value = serde_json::from_str(raw).unwrap();
    for tag in ["code", "span"] {
        let name = format!("nested-{tag}-flanks-250");
        let case = oracle["extra"]
            .as_array()
            .unwrap()
            .iter()
            .find(|case| case["name"] == name)
            .expect("250-level TS fixture");
        assert_eq!(
            case["html"]
                .as_str()
                .unwrap()
                .matches(&format!("<{tag}> "))
                .count(),
            250
        );
        assert_eq!(
            Turndown
                .convert(case["html"].as_str().unwrap())
                .unwrap()
                .as_bytes(),
            case["markdown"].as_str().unwrap().as_bytes(),
            "{name}"
        );
    }
}

#[test]
fn nested_headings_match_real_ts_string_limit() {
    use sha2::{Digest, Sha256};
    let raw = include_str!("fixtures/html-markdown/string-limit-oracle.json");
    baseline::oracle(raw);
    let oracle: serde_json::Value = serde_json::from_str(raw).unwrap();
    let first_failure = oracle["firstFailure"].as_u64().unwrap();
    let cases = oracle["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 4);
    assert_eq!(cases[1]["n"].as_u64().unwrap(), first_failure - 1);
    assert_eq!(cases[2]["n"].as_u64().unwrap(), first_failure);
    for case in cases {
        let actual = Turndown.convert(case["html"].as_str().unwrap());
        if let Some(expected) = case["direct"]["markdown"].as_str() {
            assert!(expected.len() < 1024 * 1024);
            assert_eq!(actual.unwrap(), expected);
        } else if let Some(length) = case["direct"]["markdownUtf16Length"].as_u64() {
            let actual = actual.unwrap();
            assert_eq!(actual.encode_utf16().count() as u64, length);
            assert_eq!(
                format!("{:x}", Sha256::digest(actual.as_bytes())),
                case["direct"]["markdownSha256"].as_str().unwrap()
            );
        } else {
            assert_eq!(
                actual.unwrap_err(),
                case["direct"]["error"]["message"].as_str().unwrap()
            );
        }
    }
}
