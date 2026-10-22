// Manual RSS probe for #409/#454. Build/run through `rss-probe.sh`; it runs
// one mode per process so `/usr/bin/time -l` reports that mode's peak RSS.
use pilotdeck_gateway::runtime::tools::builtin::web::{
    HtmlToMarkdown, fetcher::MAX_MARKDOWN_LENGTH, html_markdown::Turndown,
};
use std::time::Instant;

fn deep_html(bytes: usize) -> String {
    let unit = "a<br>";
    assert_eq!(bytes % unit.len(), 0);
    let mut html = "<blockquote>".repeat(250);
    for _ in 0..(bytes / unit.len()) {
        html.push_str(unit);
    }
    html.push_str(&"</blockquote>".repeat(250));
    html
}

#[test]
#[ignore = "manual RSS probe; run through tests/fixtures/html-markdown/rss-probe.sh"]
fn probe() {
    let mode = std::env::var("PDRS_PROBE").expect("set PDRS_PROBE");
    let mib10 = 10 * 1024 * 1024;
    let html = match mode.as_str() {
        "deep-full-2.5" | "deep-bounded-2.5" => deep_html(2_621_440),
        "deep-bounded-10" => deep_html(mib10),
        // Maximum element-node count inside the 10 MiB body cap: every tag is
        // `<br>`, Turndown output is whitespace only.
        "wide-bounded-10" => "<br>".repeat(mib10 / 4),
        // Text node + <br> element per five input bytes, small flat output.
        "flat-bounded-10" => "a<br>".repeat(mib10 / 5),
        other => panic!("unknown PDRS_PROBE={other}"),
    };
    let started = Instant::now();
    if mode == "deep-full-2.5" {
        let full = Turndown.convert(&html).unwrap();
        println!(
            "mode={mode} html_bytes={} full_utf16={} output_bytes={}",
            html.len(),
            full.encode_utf16().count(),
            full.len()
        );
    } else {
        match Turndown.convert_bounded(&html, MAX_MARKDOWN_LENGTH + 1) {
            Ok(output) => println!(
                "mode={mode} html_bytes={} full_utf16={} prefix_utf16={} complete={}",
                html.len(),
                output.utf16_length,
                output.text.encode_utf16().count(),
                output.complete
            ),
            Err(error) => println!("mode={mode} html_bytes={} err={error}", html.len()),
        }
    }
    println!(
        "mode={mode} elapsed_ms={:.1}",
        started.elapsed().as_secs_f64() * 1000.0
    );
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "manual RSS probe: spawns /usr/bin/time twice (~2 GB peak); run via rss-probe.sh"]
fn bounded_peak_rss_is_lower_than_full() {
    use std::process::Command;
    if std::env::var_os("PDRS_PROBE").is_some() {
        return;
    }
    let peak = |mode: &str| {
        let output = Command::new("/usr/bin/time")
            .arg("-l")
            .arg(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "probe", "--nocapture"])
            .env("PDRS_PROBE", mode)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        let line = stderr
            .lines()
            .find(|line| line.contains("maximum resident set size"))
            .unwrap();
        line.split_whitespace()
            .next()
            .unwrap()
            .parse::<u64>()
            .unwrap()
    };
    let full = peak("deep-full-2.5");
    let bounded = peak("deep-bounded-2.5");
    assert!(
        bounded + 50 * 1024 * 1024 < full,
        "full={full} bytes, bounded={bounded} bytes"
    );
}
