#[path = "common/baseline.rs"]
mod baseline;
#[path = "common/scratch.rs"]
mod scratch;
#[path = "common/timing.rs"]
mod timing;

use pilotdeck_gateway::runtime::tools::builtin::web::{
    HtmlToMarkdown, UrlFetcher,
    fetcher::{MAX_MARKDOWN_LENGTH, truncate_markdown},
    html_markdown::{DOM_LIMIT_ERROR, MAX_DOM_DEPTH, STRING_LIMIT_ERROR, Turndown},
};

const ORACLE: &str = include_str!("fixtures/html-markdown/oracle.json");

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn sha256_utf16(text: &str) -> String {
    sha256_hex(
        &text
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    )
}

fn sha256_units(units: &[u16]) -> String {
    sha256_hex(
        &units
            .iter()
            .flat_map(|unit| unit.to_le_bytes())
            .collect::<Vec<_>>(),
    )
}

#[test]
fn bounded_prefix_and_truncation_match_real_ts() {
    let raw = include_str!("fixtures/html-markdown/truncate-oracle.json");
    baseline::oracle(raw);
    let oracle: serde_json::Value = serde_json::from_str(raw).unwrap();
    let cases = oracle["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 18);
    let mut failures = Vec::new();
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let mut html = String::new();
        for part in case["parts"].as_array().unwrap() {
            let text = part["text"].as_str().unwrap();
            let repeat = part["repeat"].as_u64().unwrap() as usize;
            html.push_str(&text.repeat(repeat));
        }
        if sha256_hex(html.as_bytes()) != case["htmlSha256"].as_str().unwrap() {
            failures.push(format!("{name}: expanded HTML hash"));
            continue;
        }
        let output = Turndown
            .convert_bounded(&html, MAX_MARKDOWN_LENGTH + 1)
            .unwrap();
        let full_units = case["full"]["utf16Length"].as_u64().unwrap() as usize;
        if output.utf16_length != full_units {
            failures.push(format!(
                "{name}: full length {} != {full_units}",
                output.utf16_length
            ));
            continue;
        }
        if output.complete && sha256_utf16(&output.text) != case["full"]["sha256"].as_str().unwrap()
        {
            failures.push(format!("{name}: complete Markdown"));
        }
        if output.complete != (full_units <= MAX_MARKDOWN_LENGTH + 1) {
            failures.push(format!("{name}: completeness {full_units}"));
        }
        // The bounded prefix must match the real TS slice of the full text
        // (one extra unit is allowed when the cut splits a surrogate pair).
        let prefix_units = case["prefix"]["utf16Length"].as_u64().unwrap() as usize;
        let actual_units: Vec<u16> = output.text.encode_utf16().collect();
        let actual_len = actual_units.len();
        let expected_sha = case["prefix"]["sha256"].as_str().unwrap();
        let actual_sha = if actual_len >= prefix_units {
            sha256_units(&actual_units[..prefix_units])
        } else {
            String::from("<short>")
        };
        if actual_len < prefix_units
            || actual_len > prefix_units.saturating_add(1)
            || actual_sha != expected_sha
        {
            failures.push(format!(
                "{name}: bounded prefix mismatch: got {actual_len} units / sha {actual_sha}, \
                 want {prefix_units} units / sha {expected_sha}"
            ));
        }
        let truncated = truncate_markdown(&output.text);
        for (index, result) in case["tool"].as_array().unwrap().iter().enumerate() {
            if result["rawLength"].as_u64().unwrap() as usize != full_units {
                failures.push(format!("{name}: tool[{index}] rawLength"));
            }
            if result["truncated"].as_bool().unwrap() != (full_units > MAX_MARKDOWN_LENGTH) {
                failures.push(format!("{name}: tool[{index}] truncated"));
            }
            if result["returnedLength"].as_u64().unwrap() as usize != truncated.units().len() {
                failures.push(format!("{name}: tool[{index}] returnedLength"));
            }
            if result["textSha256"].as_str().unwrap() != sha256_units(truncated.units()) {
                failures.push(format!("{name}: tool[{index}] text"));
            }
            if index == 1 && result["fromCache"].as_bool() != Some(true) {
                failures.push(format!("{name}: tool[1] cache hit"));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Reproduce real TS content at cut points far below the production limit.
/// Each `k` asks the bounded path for exactly `k` units; the generated
/// expectations are `content.slice(0, k)` of the real web_fetch output.
#[test]
fn bounded_code_windows_match_real_ts() {
    let raw = include_str!("fixtures/html-markdown/truncate-oracle.json");
    baseline::oracle(raw);
    let oracle: serde_json::Value = serde_json::from_str(raw).unwrap();
    let windows = oracle["windows"].as_array().unwrap();
    assert_eq!(windows.len(), 9);
    let mut failures = Vec::new();
    let mut points = 0usize;
    for case in windows {
        let name = case["name"].as_str().unwrap();
        let mut html = String::new();
        for part in case["parts"].as_array().unwrap() {
            let text = part["text"].as_str().unwrap();
            let repeat = part["repeat"].as_u64().unwrap() as usize;
            html.push_str(&text.repeat(repeat));
        }
        if sha256_hex(html.as_bytes()) != case["htmlSha256"].as_str().unwrap() {
            failures.push(format!("{name}: expanded HTML hash"));
            continue;
        }
        let full_units = case["full"]["utf16Length"].as_u64().unwrap() as usize;
        for expected in case["expected"].as_array().unwrap() {
            points += 1;
            let k = expected["k"].as_u64().unwrap() as usize;
            let output = Turndown.convert_bounded(&html, k).unwrap();
            if output.utf16_length != full_units {
                failures.push(format!(
                    "{name}@k{k}: full length {} != {full_units}",
                    output.utf16_length
                ));
                continue;
            }
            if output.complete {
                failures.push(format!("{name}@k{k}: unexpectedly complete"));
            }
            let want_units = expected["utf16Length"].as_u64().unwrap() as usize;
            let expected_sha = expected["sha256"].as_str().unwrap();
            let got: Vec<u16> = output.text.encode_utf16().collect();
            let actual_sha = if got.len() >= want_units {
                sha256_units(&got[..want_units])
            } else {
                String::from("<short>")
            };
            if got.len() < want_units
                || got.len() > want_units.saturating_add(1)
                || actual_sha != expected_sha
            {
                failures.push(format!(
                    "{name}@k{k}: prefix mismatch: got {} units / sha {actual_sha}, \
                     want {want_units} units / sha {expected_sha}",
                    got.len()
                ));
            }
        }
    }
    assert_eq!(points, 22);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Grok r4 P1: a trimmed inline-code prefix ending on a CR must be grown so
/// its folded length covers the requested take; otherwise the bounded walk
/// runs past the materialized prefix and skips the unmaterialized tail. The
/// full Rust conversion matches the real TS baseline for this input (Grok
/// verified it), so matching the full output at every k is the same promise
/// the golden windows check.
#[test]
fn bounded_code_matches_full_past_a_short_trimmed_crlf_prefix() {
    let mut html = String::from("<pre>x<code>&#10;`&#13;\r```");
    for _ in 0..8 {
        html.push_str("&#13;&#10;");
    }
    html.push_str("&#13;\n```</code></pre>");
    let full = Turndown.convert(&html).unwrap();
    let full_units = full.encode_utf16().count();
    assert_eq!(full_units, 25);
    let want: Vec<u16> = full.encode_utf16().collect();
    let mut failures = Vec::new();
    for k in 0..=full_units {
        let out = Turndown.convert_bounded(&html, k).unwrap();
        if out.utf16_length != full_units {
            failures.push(format!("k={k}: full length {}", out.utf16_length));
            continue;
        }
        let got: Vec<u16> = out.text.encode_utf16().collect();
        let take = k.min(full_units);
        if got.len() < take || got.len() > take + 1 || !want.starts_with(&got) {
            failures.push(format!(
                "k={k}: {:?} is not a prefix of {:?}",
                out.text, full
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

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

/// Domino 2.2.0 compatibility cases. The first eleven rows are the original
/// `accepted-diffs.md:64` gaps (select/hr, search, select inner tags); the rest
/// pin the rewrite's allowed tokens, raw-text/template/table/foreign contexts,
/// and the ordinary block negative control. All expected bytes are recorded
/// from the real TS `getURLMarkdownContent` entry point. The historical test
/// name is kept because the frozen ledger points at it.
#[test]
fn parser_version_gaps_match_real_ts() {
    baseline::oracle(ORACLE);
    let oracle: serde_json::Value = serde_json::from_str(ORACLE).unwrap();
    let cases = oracle["parserGaps"].as_array().unwrap();
    assert_eq!(cases.len(), 105);
    assert_eq!(
        cases
            .iter()
            .map(|case| case["name"].as_str().unwrap())
            .collect::<std::collections::HashSet<_>>()
            .len(),
        105
    );
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
        (
            "generate-truncate.mts",
            include_str!("fixtures/html-markdown/truncate-oracle.json"),
            "truncate-oracle.json",
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
fn bounded_html_conversion_emits_only_the_truncation_prefix() {
    let depth = (MAX_DOM_DEPTH - 3) / 2;
    // A large leaf with many line breaks makes the old ancestor replace scan
    // recopy the expanded indentation at every level. Assert the output work
    // instead of wall-clock time: the logical length is tens of MiB while the
    // materialized text stops at the truncation prefix. That contract is
    // machine-speed independent (a slow CI runner cannot turn it red).
    let payload = format!("{}x", "<br>".repeat(90_000));
    for (open, close) in [
        ("<ul><li>", "</li></ul>"),
        ("<ol start=9007199254740991><li>", "</li></ol>"),
    ] {
        let html = format!("{}{payload}{}", open.repeat(depth), close.repeat(depth));
        let output = Turndown
            .convert_bounded(&html, MAX_MARKDOWN_LENGTH + 1)
            .unwrap();
        assert!(output.utf16_length > 40_000_000);
        assert!(!output.complete);
        assert!(
            output.text.encode_utf16().count() <= MAX_MARKDOWN_LENGTH + 1,
            "materialized {} units",
            output.text.encode_utf16().count()
        );
    }
    let flat = format!("<div>{}</div>", "x".repeat(10 * 1024 * 1024 - 11));
    let output = Turndown
        .convert_bounded(&flat, MAX_MARKDOWN_LENGTH + 1)
        .unwrap();
    assert_eq!(output.utf16_length, 10 * 1024 * 1024 - 11);
    assert!(!output.complete);
    assert_eq!(output.text.encode_utf16().count(), MAX_MARKDOWN_LENGTH + 1);
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

/// The same recipe with its dominant (largest) repeat halved: the same shape
/// at about half the bytes, for the linear-scaling ratio. The dominant repeat
/// carries the repeated payload of every large-oracle recipe, so the halved
/// page keeps the conversion work proportional to its size. There is no TS
/// oracle at the halved size; only its timing matters.
fn halved_recipe(rule: &serde_json::Value) -> serde_json::Value {
    // Recipe nodes can be plain strings (leaf text); indexing those with a key
    // would panic, so go through get().
    fn dominant(rule: &serde_json::Value) -> u64 {
        let own = rule
            .get("repeat")
            .and_then(|value| value.as_u64())
            .unwrap_or(1);
        rule.get("parts")
            .and_then(|value| value.as_array())
            .map_or(own, |parts| parts.iter().map(dominant).fold(own, u64::max))
    }
    fn halve_first(rule: &mut serde_json::Value, target: u64) -> bool {
        if target >= 2 && rule.get("repeat").and_then(|value| value.as_u64()) == Some(target) {
            rule["repeat"] = (target / 2).into();
            return true;
        }
        rule.get_mut("parts")
            .and_then(|value| value.as_array_mut())
            .is_some_and(|parts| parts.iter_mut().any(|part| halve_first(part, target)))
    }
    let mut halved = rule.clone();
    let dominant = dominant(&halved);
    assert!(
        halve_first(&mut halved, dominant),
        "recipe must contain a repeat"
    );
    halved
}

#[test]
fn large_shallow_pages_match_real_ts_bytes_and_stay_fast() {
    use sha2::{Digest, Sha256};
    let raw = include_str!("fixtures/html-markdown/large-oracle.json");
    baseline::oracle(raw);
    let oracle: serde_json::Value = serde_json::from_str(raw).unwrap();
    let cases = oracle["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 5);
    let samples = timing::timing_samples();
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
        let halved = expand_large_recipe(&halved_recipe(&case["html"]));
        let error = std::cell::RefCell::new(None);
        let (half_elapsed, full_elapsed) = timing::fastest_scaling(name, samples, |full| {
            let (input, want) = if full {
                (&html, Some(expected.as_str()))
            } else {
                (&halved, None)
            };
            let started = std::time::Instant::now();
            let actual = Turndown.convert(input);
            let elapsed = started.elapsed();
            match actual {
                Ok(actual) => match want {
                    Some(want) => assert!(
                        actual.as_bytes() == want.as_bytes(),
                        "{name}: TS bytes differ"
                    ),
                    None => assert!(!actual.is_empty(), "{name}: halved page empty"),
                },
                Err(why) => *error.borrow_mut() = Some(why),
            }
            elapsed
        });
        if let Some(why) = error.into_inner() {
            failures.push(format!("{name}: {why}"));
            continue;
        }
        eprintln!(
            "{name}: {} HTML bytes, fastest of {samples}: full {full_elapsed:?}, half {half_elapsed:?}",
            html.len()
        );
        // ~2.1 million DOM nodes at 10 MiB. Doubling the input doubles linear
        // conversion work; the ratio keeps a preempted single sample from
        // turning the test red on a loaded host.
        timing::assert_within_linear_band(
            &format!("{name}: full page"),
            half_elapsed,
            full_elapsed,
            samples,
        );
        // Constant-factor backstop at the same loose wall clock (the
        // pre-4fbb5f78b moved-subtree rewalk was ~36x slower per byte while
        // still scaling linearly).
        let budget = if cfg!(debug_assertions) { 60.0 } else { 5.0 };
        assert!(
            full_elapsed.as_secs_f64() < budget,
            "{name}: took {full_elapsed:?} (fastest of {samples}), budget {budget}s"
        );
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
    let half_html = "<em>x</em>".repeat(512 * 1024);
    let expected_full = period.repeat(1024 * 1024);
    let expected_half = period.repeat(512 * 1024);
    let samples = timing::timing_samples();
    let (half_elapsed, full_elapsed) =
        timing::fastest_scaling("dense-inline-10mib-flat", samples, |full| {
            let (input, want) = if full {
                (&html, expected_full.as_bytes())
            } else {
                (&half_html, expected_half.as_bytes())
            };
            let started = std::time::Instant::now();
            let actual = Turndown.convert(input).unwrap();
            let elapsed = started.elapsed();
            assert!(actual.as_bytes() == want);
            elapsed
        });
    eprintln!(
        "dense-inline-10mib-flat: {} HTML bytes, fastest of {samples}: \
         full {full_elapsed:?}, half {half_elapsed:?}",
        html.len()
    );
    timing::assert_within_linear_band("flat inline: 10 MiB", half_elapsed, full_elapsed, samples);
    let budget = if cfg!(debug_assertions) { 60.0 } else { 5.0 };
    assert!(
        full_elapsed.as_secs_f64() < budget,
        "flat inline: took {full_elapsed:?} (fastest of {samples}), budget {budget}s"
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

#[test]
fn bounded_prefix_covers_a_surrogate_pair_at_the_truncation_cut() {
    let suffix = "\n\n[Content truncated due to length...]";
    // Pair starts at unit 100000: the 100000-unit cut keeps only the a's, but
    // the prefix still has to carry one extra unit so the suffix is emitted.
    let html = format!("<p>{}😀tail</p>", "a".repeat(MAX_MARKDOWN_LENGTH));
    let output = Turndown
        .convert_bounded(&html, MAX_MARKDOWN_LENGTH + 1)
        .unwrap();
    assert_eq!(output.utf16_length, MAX_MARKDOWN_LENGTH + 6);
    let text = truncate_markdown(&output.text);
    assert_eq!(text.units().len(), MAX_MARKDOWN_LENGTH + suffix.len());
    assert_eq!(text.units()[MAX_MARKDOWN_LENGTH - 1], b'a' as u16);

    // Pair starts at unit 99999: the cut splits it, exactly like slice.
    let html = format!("<p>{}😀tail</p>", "a".repeat(MAX_MARKDOWN_LENGTH - 1));
    let output = Turndown
        .convert_bounded(&html, MAX_MARKDOWN_LENGTH + 1)
        .unwrap();
    assert_eq!(output.utf16_length, MAX_MARKDOWN_LENGTH + 5);
    let text = truncate_markdown(&output.text);
    assert_eq!(text.units().len(), MAX_MARKDOWN_LENGTH + suffix.len());
    assert_eq!(text.units()[MAX_MARKDOWN_LENGTH - 1], 0xD83D);
}

fn issue_html(bytes: usize, unit: &str) -> String {
    assert_eq!(bytes % unit.len(), 0, "unit must tile the payload");
    let lines = bytes / unit.len();
    let mut html = "<blockquote>".repeat(250);
    for _ in 0..lines {
        html.push_str(unit);
    }
    html.push_str(&"</blockquote>".repeat(250));
    html
}

#[test]
#[ignore = "heavy (2.5 MiB HTML, ~1 GB RSS in debug): run manually; see fixtures/html-markdown/README.md"]
fn deep_blockquote_250_2_5mib_bounded_is_prefix_only() {
    let html = issue_html(2_621_440, "a<br>");
    let started = std::time::Instant::now();
    let output = Turndown
        .convert_bounded(&html, MAX_MARKDOWN_LENGTH + 1)
        .unwrap();
    let elapsed = started.elapsed();
    assert!(!output.complete);
    // The old full conversion is ~220 MB for this shape (#409).
    assert!(
        output.utf16_length > 200_000_000,
        "full length {}",
        output.utf16_length
    );
    assert_eq!(output.text.encode_utf16().count(), MAX_MARKDOWN_LENGTH + 1);
    assert!(
        elapsed < std::time::Duration::from_secs(120),
        "bounded conversion took {elapsed:?}"
    );
}

#[test]
#[ignore = "heavy: full conversion is ~220 MB; run in release for #409 evidence"]
fn deep_blockquote_250_2_5mib_bounded_equals_full() {
    let html = issue_html(2_621_440, "a<br>");
    let full = Turndown.convert(&html).unwrap();
    let bounded = Turndown
        .convert_bounded(&html, MAX_MARKDOWN_LENGTH + 1)
        .unwrap();
    assert_eq!(bounded.utf16_length, full.encode_utf16().count());
    assert_eq!(
        truncate_markdown(&bounded.text).units(),
        truncate_markdown(&full).units()
    );
}

#[test]
#[ignore = "heavy: ~3.4 GB RSS before the V8 string limit; run in release for #409 evidence"]
fn deep_blockquote_250_10mib_stops_at_the_ts_string_limit() {
    let html = issue_html(10 * 1024 * 1024, "a<br>");
    let started = std::time::Instant::now();
    let error = Turndown
        .convert_bounded(&html, MAX_MARKDOWN_LENGTH + 1)
        .unwrap_err();
    assert_eq!(error, STRING_LIMIT_ERROR);
    assert!(started.elapsed() < std::time::Duration::from_secs(120));
}

#[test]
#[ignore = "heavy: ~4.0 GB RSS for 4.2 M DOM nodes; run in release for #409/#454 evidence"]
fn ten_mib_flat_a_br_bounded_stays_within_the_prefix() {
    let html = "a<br>".repeat(2_097_152);
    assert_eq!(html.len(), 10 * 1024 * 1024);
    let output = Turndown
        .convert_bounded(&html, MAX_MARKDOWN_LENGTH + 1)
        .unwrap();
    assert_eq!(output.utf16_length, 8_388_605);
    assert_eq!(output.text.encode_utf16().count(), MAX_MARKDOWN_LENGTH + 1);
    assert!(!output.complete);
}

fn reparented_wide_html(bytes: usize) -> (String, usize) {
    // Distinct attributes defeat the active-formatting list's three-identical-
    // entries rule. Closing each B moves the same wide P subtree via adoption
    // agency, while its element depth remains below MAX_DOM_DEPTH.
    let formatting = 240;
    let prefix = (0..formatting)
        .map(|i| format!("<b a={i}>"))
        .collect::<String>()
        + "<p>";
    let suffix = "</b>".repeat(formatting);
    let count = (bytes - prefix.len() - suffix.len()) / "<span>x</span>".len();
    let padding = bytes - prefix.len() - suffix.len() - count * "<span>x</span>".len();
    let html = prefix + &"<span>x</span>".repeat(count) + &"x".repeat(padding) + &suffix;
    assert_eq!(html.len(), bytes);
    (html, count + padding)
}

#[test]
fn reparented_wide_four_mib_succeeds() {
    // The corresponding real TS array-limit error remains an unaccepted gap.
    assert!(
        Turndown
            .convert(&reparented_wide_html(4 * 1024 * 1024).0)
            .is_ok()
    );
}

#[test]
fn reparented_wide_subtrees_stay_fast() {
    let (html, output_x_count) = reparented_wide_html(8 * 1024 * 1024);
    let (half_html, half_x_count) = reparented_wide_html(4 * 1024 * 1024);
    let formatting = 240;
    // Real domino/turndown byte-compared at 1/2 MiB; the 4/8 MiB baseline
    // throws Invalid array length. Rust's unchanged successful output follows
    // the same lossless period, verified against the pre-fix implementation.
    let bold = "**".repeat(formatting);
    let expected_full = format!("{bold}{}{bold}", "x".repeat(output_x_count));
    let expected_half = format!("{bold}{}{bold}", "x".repeat(half_x_count));
    let samples = timing::timing_samples();
    let (half_elapsed, full_elapsed) =
        timing::fastest_scaling("reparent-wide-8mib", samples, |full| {
            let (input, want) = if full {
                (&html, expected_full.as_str())
            } else {
                (&half_html, expected_half.as_str())
            };
            let started = std::time::Instant::now();
            let actual = Turndown.convert(input).unwrap();
            let elapsed = started.elapsed();
            assert_eq!(actual, want);
            elapsed
        });
    eprintln!(
        "reparent-wide-8mib: {} HTML bytes, fastest of {samples}: \
         full {full_elapsed:?}, half {half_elapsed:?}",
        html.len()
    );
    timing::assert_within_linear_band(
        "reparent wide subtree: 8 MiB",
        half_elapsed,
        full_elapsed,
        samples,
    );
    // The pre-4fbb5f78b moved-subtree rewalk scaled linearly too, just about
    // 36x slower per byte, so keep the loose absolute cap as the backstop.
    let budget = if cfg!(debug_assertions) { 60.0 } else { 5.0 };
    assert!(
        full_elapsed.as_secs_f64() < budget,
        "reparent wide subtree: took {full_elapsed:?} (fastest of {samples}), \
         budget {budget}s"
    );
}

/// The class-level bail-out in `html_compat` stops rewriting at an unmodelled
/// construct and hands the remainder to html5ever. These are the residual
/// differences against domino 2.2.0, kept deliberately and registered in
/// `specs/gateway-parity/diffs/A6.5-WEBFETCH-HTMLCOMPAT-BAILOUT-001.md`.
#[test]
fn unmodelled_bail_out_differences_match_registered_behavior() {
    // (input, real TS Markdown from fix-r2/ts-probe-r2.json, Rust Markdown)
    let cases = [
        ("<svg></svg><select><hr></select>", "", "* * *"),
        ("<svg></svg><p>a<search>b</search>c</p>", "abc", "a\n\nbc"),
        (
            "<script><!--<script></script></script><select><hr></select>",
            "<!--<script></script>",
            "<!--<script></script>\n\n* * *",
        ),
        (
            "<div>a<select><script><!--<script></script></script><h1>x</h1></select>d</div>",
            "a<!--<script></script>xd",
            "a<!--<script></script>\n\nx\n=\n\nd",
        ),
    ];
    for (html, ts, rust) in cases {
        let actual = Turndown.convert(html).unwrap();
        assert_eq!(actual, rust, "conservative output for {html:?}");
        assert_ne!(actual, ts, "registered difference for {html:?}");
    }
}
