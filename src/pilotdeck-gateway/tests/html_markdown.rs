#[path = "common/baseline.rs"]
mod baseline;
#[path = "common/scratch.rs"]
mod scratch;

use pilotdeck_gateway::runtime::tools::builtin::web::{
    HtmlToMarkdown, UrlFetcher, html_markdown::Turndown,
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
        "<span>".repeat(10_000),
        "</span>".repeat(10_000)
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
    for _ in 0..2 {
        let scratch = scratch::Scratch::new("html-markdown");
        let output = std::process::Command::new("node")
            .arg("--import")
            .arg(baseline.join("node_modules/tsx/dist/loader.mjs"))
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/html-markdown/generate.mts"
            ))
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
            std::fs::read(scratch.path().join("oracle.json")).unwrap(),
            ORACLE.as_bytes()
        );
    }
}
