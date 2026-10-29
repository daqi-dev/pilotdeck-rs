//! Multi-sample timing budgets shared by the web HTML conversion regressions:
//! the `html_markdown` integration tests and the `html_compat` unit tests.
//!
//! A single sample plus an absolute wall clock turns red whenever the host
//! preempts the conversion once. These helpers instead interleave two sizes of
//! the same shape and check the scaling ratio (the fastest run for the
//! two-size callers, the median of the paired ratios for the 8x reparented
//! pair); the callers keep a loose absolute cap as the constant-factor
//! backstop. Callers that must stay red-free on a loaded host (#978) sample
//! the CPU seconds of an isolated worker process instead of the wall clock.

use std::time::Duration;

/// Timing samples per size. Three is the default for the shared budgets; the
/// reparented wide-subtree regression asks for five (`timing_samples_or`)
/// because it compares an eight-times-smaller input and judges the median.
pub fn timing_samples() -> usize {
    timing_samples_or(3)
}

/// `timing_samples` with a caller-specific default. `PDRS_TIMING_SAMPLES`
/// still wins, so the quadratic negative controls in
/// `tests/fixtures/html-timing/negative_controls.py` can force one sample.
pub fn timing_samples_or(default: usize) -> usize {
    std::env::var("PDRS_TIMING_SAMPLES")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|samples| (1..=9).contains(samples))
        .unwrap_or(default)
}

/// One interleaved round per sample, alternating which size runs first. The
/// sampler receives `true` for the larger size and `false` for the smaller.
fn sample_pairs<S>(
    label: &str,
    small_label: &str,
    large_label: &str,
    samples: usize,
    mut sample: S,
) -> Vec<(Duration, Duration)>
where
    S: FnMut(bool) -> Duration,
{
    let samples = samples.max(1);
    let mut pairs = Vec::with_capacity(samples);
    for round in 0..samples {
        let (small_elapsed, large_elapsed) = if round % 2 == 0 {
            (sample(false), sample(true))
        } else {
            let large_elapsed = sample(true);
            (sample(false), large_elapsed)
        };
        let round = round + 1;
        eprintln!(
            "{label} sample {round}: {small_label} in {small_elapsed:?}, \
             {large_label} in {large_elapsed:?}"
        );
        pairs.push((small_elapsed, large_elapsed));
    }
    pairs
}

/// Interleave the two sizes and keep the fastest run of each. A single
/// preempted sample must not decide the ratio on a loaded host; alternating
/// the order also keeps the first allocation from always falling on the
/// same size.
pub fn fastest_scaling<S>(label: &str, samples: usize, sample: S) -> (Duration, Duration)
where
    S: FnMut(bool) -> Duration,
{
    let pairs = sample_pairs(label, "half", "full", samples, sample);
    let half_fastest = pairs
        .iter()
        .map(|(half, _)| *half)
        .min()
        .unwrap_or(Duration::MAX);
    let full_fastest = pairs
        .iter()
        .map(|(_, full)| *full)
        .min()
        .unwrap_or(Duration::MAX);
    (half_fastest, full_fastest)
}

/// Timing evidence for inputs `factor` times apart, judged by the median of
/// the per-round ratios. The fastest durations are kept for the absolute cap.
#[allow(dead_code)]
pub struct ScalingSamples {
    /// Fastest small-input run, used to convert the constant slack below.
    pub small: Duration,
    /// Fastest large-input run, used by the per-sample absolute cap.
    pub large: Duration,
    /// Median of the per-round `large / small` ratios.
    pub median_ratio: f64,
    /// Number of interleaved rounds behind `median_ratio`.
    pub samples: usize,
}

/// Like [`fastest_scaling`], but also returns the median of the per-round
/// ratios. The median discards a minority of preempted rounds instead of
/// letting the fastest independent runs come from different load regimes.
#[allow(dead_code)]
pub fn fastest_scaling_ratio<S>(
    label: &str,
    small_label: &str,
    large_label: &str,
    samples: usize,
    sample: S,
) -> ScalingSamples
where
    S: FnMut(bool) -> Duration,
{
    let pairs = sample_pairs(label, small_label, large_label, samples, sample);
    let small = pairs
        .iter()
        .map(|(small, _)| *small)
        .min()
        .unwrap_or(Duration::MAX);
    let large = pairs
        .iter()
        .map(|(_, large)| *large)
        .min()
        .unwrap_or(Duration::MAX);
    let mut ratios: Vec<f64> = pairs
        .iter()
        .map(|(small, large)| large.as_secs_f64() / small.as_secs_f64().max(f64::EPSILON))
        .collect();
    ratios.sort_by(f64::total_cmp);
    let median_ratio = if ratios.len() % 2 == 1 {
        ratios[ratios.len() / 2]
    } else {
        let middle = ratios.len() / 2;
        (ratios[middle - 1] + ratios[middle]) / 2.0
    };
    ScalingSamples {
        small,
        large,
        median_ratio,
        samples: pairs.len(),
    }
}

/// Doubling the input doubles linear work but quadruples a quadratic rescan.
/// The fastest interleaved samples keep one preempted run from turning a
/// linear implementation red, while a quadratic one sits far outside the
/// 3x + 150 ms band.
pub fn assert_within_linear_band(label: &str, half: Duration, full: Duration, samples: usize) {
    const GROWTH: f64 = 3.0;
    const NOISE: f64 = 0.15;
    let limit = half.as_secs_f64() * GROWTH + NOISE;
    assert!(
        full.as_secs_f64() <= limit,
        "{label} {full:?} did not scale from half {half:?} (fastest of {samples}): \
         allowed {limit:.3}s = {GROWTH}x + {NOISE}s; \
         a quadratic implementation needs about 4x"
    );
}

/// Band for a pair of inputs `factor` times apart. Linear work grows by
/// `factor`; quadratic work by `factor^2`. The geometric midpoint
/// `factor^1.5` leaves the same multiplicative margin (`sqrt(factor)`) on
/// both sides: at factor 8 the band is 22.6x, 2.83x above the linear 8x and
/// 2.83x below the quadratic 64x. The median of the paired ratios tolerates
/// a minority of preempted rounds; the 150 ms slack is converted to ratio
/// space through the fastest small run, so short inputs keep the same
/// absolute allowance.
#[allow(dead_code)]
pub fn assert_within_scaling_band(label: &str, sampled: &ScalingSamples, factor: f64) {
    const NOISE: f64 = 0.15;
    let allowed = factor.powf(1.5);
    let small = sampled.small.as_secs_f64().max(f64::EPSILON);
    let limit = allowed + NOISE / small;
    let quadratic = factor * factor;
    let small_duration = sampled.small;
    let large_duration = sampled.large;
    let samples = sampled.samples;
    let ratio = sampled.median_ratio;
    assert!(
        ratio <= limit,
        "{label} did not scale from {factor:.0}x-smaller {small_duration:?} \
         to {large_duration:?} (median of {samples} paired ratios: {ratio:.2}x): \
         allowed {limit:.2}x = {allowed:.2}x + {NOISE}s/{small:.3}s; \
         a linear implementation needs about {factor:.0}x, \
         a quadratic one about {quadratic:.0}x"
    );
}

/// Constant-factor backstop for what the ratio cannot see: a linear-but-slow
/// rewrite stays inside the 3x band (the pre-4fbb5f78b moved-subtree rewalk
/// was linear at ~36x the cost per byte). The budget is per full-size sample:
/// taking more timing samples multiplies a test's wall cost but never divides
/// this budget (`PDRS_TIMING_SAMPLES=1` gets the same cap), and the fastest
/// sample is judged so one preempted run cannot redden the backstop either.
/// Debug keeps ~3.75x headroom over the slowest full sample observed on the
/// CI runners (~32 s; 46033287a, 193e8a1e); release keeps its original cap,
/// where the 36x rewalk measured 50.4 s against 5 s.
// The html_compat unit tests judge the scaling ratio only (#798 removed
// their absolute cap), so this helper is dead code in that compilation unit.
#[allow(dead_code)]
pub fn assert_within_per_sample_cap(label: &str, full: Duration, samples: usize) {
    let budget = if cfg!(debug_assertions) { 120.0 } else { 5.0 };
    assert!(
        full.as_secs_f64() < budget,
        "{label}: took {full:?} (fastest of {samples}), per-sample budget {budget}s"
    );
}

/// CPU seconds of one completed isolated worker sample (#978), or `None`
/// when the worker did not finish exactly one green test. Preemption inflates
/// wall time, not CPU time, so a loaded host cannot redden a ratio judged on
/// these durations; the sampler still refuses incomplete work.
pub fn cpu_duration(sample: &serde_json::Value) -> Option<Duration> {
    if sample["exit"].as_i64()? != 0 {
        return None;
    }
    let results: Vec<_> = sample["stdout"]
        .as_str()?
        .lines()
        .filter(|line| line.starts_with("test result:"))
        .collect();
    if results.len() != 1 || !results[0].starts_with("test result: ok. 1 passed; 0 failed;") {
        return None;
    }
    // #978: the pre-fix sampler judged wall time, so a loaded host reddened a
    // linear implementation. This switch re-injects exactly that regression;
    // the bound unit test's 40 s wall / 0.25 s CPU sample must stay red.
    let seconds = if pilotdeck_gateway::mutants::on("html-cpu-wall-clock") {
        sample["wall_seconds"].as_f64()?
    } else {
        sample["cpu_seconds"].as_f64()?
    };
    if !seconds.is_finite() || seconds <= 0.0 {
        return None;
    }
    Duration::try_from_secs_f64(seconds).ok()
}

/// Measure one isolated Rust HTML test's CPU work, excluding host preemption.
const CPU_SAMPLER: &str = r#""""Measure one isolated Rust HTML test's CPU work, excluding host preemption."""
import json
import resource
import sys
import time
import subprocess


def main():
    before = resource.getrusage(resource.RUSAGE_CHILDREN)
    started = time.monotonic()
    # Keep the worker in the caller's process group, so an outer timeout also
    # reaps it. subprocess.run kills and waits for this child on its own timeout.
    result = subprocess.run(sys.argv[1:], capture_output=True, text=True,
                            timeout=600, check=False)
    after = resource.getrusage(resource.RUSAGE_CHILDREN)
    print(json.dumps({
        "cpu_seconds": (after.ru_utime - before.ru_utime)
                       + (after.ru_stime - before.ru_stime),
        "wall_seconds": time.monotonic() - started,
        "exit": result.returncode,
        "stdout": result.stdout,
        "stderr": result.stderr,
    }))


if __name__ == "__main__":
    main()
"#;

/// Run one `#[ignore]`d worker test of this same test binary as a separate
/// process and return the CPU seconds it consumed (#978). An algorithm test
/// measures computation, not scheduler wait, and the separate worker also
/// excludes CPU spent by parallel sibling tests. The caller's sample count
/// and growth thresholds stay unchanged.
pub fn cpu_time_isolated(worker: &str, env: &[(&str, String)]) -> Duration {
    let result = std::process::Command::new("python3")
        .arg("-c")
        .arg(CPU_SAMPLER)
        .arg(std::env::current_exe().expect("#978 CPU worker executable"))
        .args([
            "--exact",
            worker,
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .envs(env.iter().map(|(key, value)| (*key, value.as_str())))
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .output()
        .expect("#978 CPU sampler spawn");
    assert!(
        result.status.success(),
        "#978 CPU sampler exit: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let sample: serde_json::Value =
        serde_json::from_slice(&result.stdout).expect("#978 CPU sampler JSON");
    let elapsed = cpu_duration(&sample)
        .unwrap_or_else(|| panic!("#978 CPU worker did not complete one green test: {sample}"));
    eprintln!("#978 CPU sample {worker}: {sample}");
    elapsed
}

/// The 8x pair judges the median of five paired ratios. Two preempted large
/// rounds out of five must not redden a linear implementation; the real
/// quadratic mutant is covered by `materializing-join-reparent` in
/// `tests/fixtures/html-timing/negative_controls.py`.
#[test]
fn scaling_median_ignores_two_preempted_rounds() {
    let mut large_calls = 0;
    let sampled = fastest_scaling_ratio("unit", "small", "large", 5, |large| {
        if large {
            large_calls += 1;
            if large_calls == 3 || large_calls == 4 {
                Duration::from_secs_f64(64.0)
            } else {
                Duration::from_secs_f64(8.0)
            }
        } else {
            Duration::from_secs_f64(1.0)
        }
    });
    assert_eq!(
        sampled.median_ratio, 8.0,
        "#873 the median must ignore two preempted rounds"
    );
    assert_within_scaling_band("#873 unit linear pair", &sampled, 8.0);
}

/// #978: the 3x + 150 ms band is unchanged. Prove it still separates a linear
/// doubling from a quadratic one by injecting both synthetic samples, instead
/// of rewriting product code: a 1 s -> 2 s pair must pass, and the same 2x
/// input taking 4 s must breach the band.
#[test]
fn linear_band_rejects_quadratic_growth_without_loosening() {
    let half = Duration::from_secs(1);
    assert_within_linear_band("#978 unit linear", half, Duration::from_secs(2), 1);
    let panic = std::panic::catch_unwind(|| {
        assert_within_linear_band("#978 unit quadratic", half, Duration::from_secs(4), 1)
    })
    .expect_err("#978 quadratic sample must breach the 3x + 150 ms band");
    let message = panic
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| panic.downcast_ref::<&str>().copied())
        .unwrap_or_default();
    assert!(
        message.contains("#978 unit quadratic")
            && message.contains("a quadratic implementation needs about 4x"),
        "#978 band diagnosis must identify the quadratic breach: {message}"
    );
}
