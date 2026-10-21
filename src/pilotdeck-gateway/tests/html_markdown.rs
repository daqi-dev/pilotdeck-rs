#[path = "common/baseline.rs"]
mod baseline;
#[path = "common/scratch.rs"]
mod scratch;

use pilotdeck_gateway::runtime::tools::builtin::web::{
    HtmlToMarkdown, UrlFetcher,
    html_markdown::{DOM_LIMIT_ERROR, MAX_DOM_DEPTH, MAX_DOM_NODES, Turndown},
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
fn node_budget_counts_parser_nodes_and_rejects_deep_non_list_shapes() {
    // Document, HTML, HEAD, BODY, wrapper = five implicit nodes.
    assert!(Turndown.convert(&"<br>".repeat(MAX_DOM_NODES - 5)).is_ok());
    assert_eq!(
        Turndown
            .convert(&"<br>".repeat(MAX_DOM_NODES - 4))
            .unwrap_err(),
        DOM_LIMIT_ERROR
    );
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
