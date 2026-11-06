//! Multi-sample timing budgets shared by the web HTML conversion regressions:
//! the `html_markdown` integration tests and the `html_compat` unit tests.
//!
//! A single sample plus an absolute wall clock turns red whenever the host
//! preempts the conversion once. These helpers instead interleave two sizes of
//! the same shape and check the median of the per-round `large / small` ratios;
//! the fastest runs are kept only for the loose absolute cap that backstops a
//! constant-factor regression. A host-wide stall contaminates one round, not
//! the fastest independent runs from different load regimes, so the median
//! drops a minority of preempted rounds (#978 r3). Callers that must stay
//! red-free on a loaded host (#978) sample the CPU seconds of an isolated
//! worker process instead of the wall clock.

use std::time::Duration;

/// Timing samples per size. Three is the default for the shared budgets; the
/// reparented wide-subtree regression asks for five (`timing_samples_or`)
/// because it compares an eight-times-smaller input and judges the median.
pub fn timing_samples() -> usize {
    timing_samples_or(3)
}

/// `timing_samples` with a caller-specific default. `PDRS_TIMING_SAMPLES`
/// still wins, mostly for local debugging; the negative controls in
/// `tests/fixtures/html-timing/negative_controls.py` run the default sample
/// count so their red runs exercise the same median path as CI (#978).
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

/// Interleave the two sizes and keep the fastest run of each. No production
/// caller judges on this shape any more (#978 r3 moved every two-size caller
/// to [`fastest_scaling_ratio`]); it is kept so
/// `paired_median_ignores_whole_round_preemption` can reproduce the CI
/// fast/slow-regime split that the paired median fixes.
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
///
/// This is a different estimator, not a scaled copy of the retired
/// "fastest half vs fastest full" line, and the difference is deliberate
/// (#978 second review). On the CI samples `paired_median_ignores_whole_round_preemption`
/// pins, the retired shape reddened (4.480 s full against 3x * 1.360 s +
/// 0.15 s = 4.23 s) although two of the three paired rounds were inside the
/// 2.83x band (2.19x, 1.95x; the third read 3.32x): whole-round preemption, a
/// false red. The median drops exactly that minority. What it no longer catches
/// is an implementation that is super-linear in a minority of rounds and linear
/// in the rest.
///
/// What either shape catches is bounded by the band, not by "any regression".
/// A doubling-size run of `n^e` reads `2^e`. Without the slack the band is
/// `2^1.5 = 2.83x`, i.e. it rejects `e > 1.5`; the 150 ms slack is converted
/// through the fastest small run, so the boundary at a given call site is
/// `e > log2(2^1.5 + 0.15 / small)` — 1.552 at the smallest measured small
/// run (1.45 s), where the retired `3x + 0.15 s` line sits at 1.634, so the
/// replacement still catches strictly milder growth. A mild `n^1.1` reads
/// 2.14x and stays inside, as it did under the 3x line. Constant-factor
/// regressions are invisible to any ratio; the callers that also apply
/// [`assert_within_per_sample_cap`] catch those above the budget
/// (`large_shallow`'s four cases, `ten_mib`, and `reparented`'s 8 MiB run —
/// the 2 MiB probe before it judges the ratio only). The `html_compat`
/// deep-frame unit judges the ratio only, by design (#798). Anything narrower
/// than the median reintroduces the false red this issue exists to remove.
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

/// Band for a pair of inputs `factor` times apart. Linear work grows by
/// `factor`; quadratic work by `factor^2`. The geometric midpoint
/// `factor^1.5` leaves the same multiplicative margin (`sqrt(factor)`) on
/// both sides: at factor 8 the band is 22.6x, 2.83x above the linear 8x and
/// 2.83x below the quadratic 64x. Callers pass the factor measured from the
/// actual input bytes, not a hardcoded nominal one (#978: a "half" that is
/// 0.6x makes the real factor 1.667x, whose quadratic ratio 2.78x would slip
/// under a hardcoded 2.0 factor's 2.83x band). The median of the paired ratios
/// tolerates a minority of preempted rounds; the 150 ms slack is converted to
/// ratio space through the fastest small run, so short inputs keep the same
/// absolute allowance.
///
/// The slack costs guarantee width: a quadratic implementation at a 0.6x
/// "half" reads 2.78x against the 2.15x band, but only while
/// `0.15 s / small < factor^2 - factor^1.5`. That formula is the exact
/// boundary, which `scaling_band_tracks_the_measured_input_factor` asserts;
/// at a 0.6x half it evaluates to about 0.23957 s, and 0.24 s quoted
/// elsewhere is a conservative sufficient condition, not the boundary
/// itself.
/// Every measured call site clears it by a wide margin: the smallest observed
/// small sample is 1.45 s (deep-frame 4 MiB) and `large_shallow` runs 5.8 s,
/// so the slack takes at most 0.10x of the 0.63x gap on this hardware. That is
/// a measurement, not a portable guarantee: a caller with sub-0.24 s samples
/// cannot claim the same from the ratio alone.
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
/// rewrite stays inside the ratio band (the pre-4fbb5f78b moved-subtree rewalk
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

/// #978: reproduce the CI samples that reddened the retired "fastest half vs
/// fastest full" shape. Whole-round preemption left half 1.360 / 2.106 /
/// 2.297 s against full 4.518 / 4.605 / 4.480 s: the fastest runs read 3.29x
/// (over the retired 3x line) while the per-round ratios are 3.32 / 2.19 /
/// 1.95, median 2.19x (inside the 2^1.5 = 2.83x band). Pairing cancels a stall
/// that hits one size inside a round.
#[test]
fn paired_median_ignores_whole_round_preemption() {
    fn ci_samples() -> impl FnMut(bool) -> Duration {
        // `sample_pairs` alternates the order: round 0 samples half then full,
        // round 1 full then half, round 2 half then full.
        let order = [
            (false, 1.360),
            (true, 4.518),
            (true, 4.605),
            (false, 2.106),
            (false, 2.297),
            (true, 4.480),
        ];
        let mut next = 0;
        move |full| {
            let (expected, secs) = order[next];
            next += 1;
            assert_eq!(expected, full, "#978 sample order");
            Duration::from_secs_f64(secs)
        }
    }
    let (fastest_half, fastest_full) = fastest_scaling("unit", 3, ci_samples());
    assert!(
        (fastest_full.as_secs_f64() / fastest_half.as_secs_f64() - 3.294).abs() < 0.01,
        "#978 the retired fastest-of shape reads 3.29x on these samples"
    );
    let sampled = fastest_scaling_ratio("unit", "half", "full", 3, ci_samples());
    assert!(
        (sampled.median_ratio - 2.187).abs() < 0.01,
        "#978 the paired median reads 2.19x on the same samples"
    );
    assert_within_scaling_band("#978 unit paired", &sampled, 2.0);
}

/// #978: the 2^1.5 = 2.83x band replaces the retired 3x line. Inject both
/// shapes for a doubling: a linear 2.0x passes, the same input taking 4.0x
/// breaches the band. The 2.83x band is stricter than the old 3x + 150 ms
/// line, so this is a tightening, not a relaxation.
#[test]
fn scaling_band_rejects_quadratic_growth_without_loosening() {
    let linear = ScalingSamples {
        small: Duration::from_secs(1),
        large: Duration::from_secs(2),
        median_ratio: 2.0,
        samples: 3,
    };
    assert_within_scaling_band("#978 unit linear", &linear, 2.0);
    let quadratic = ScalingSamples {
        small: Duration::from_secs(1),
        large: Duration::from_secs(4),
        median_ratio: 4.0,
        samples: 3,
    };
    let panic = std::panic::catch_unwind(|| {
        assert_within_scaling_band("#978 unit quadratic", &quadratic, 2.0)
    })
    .expect_err("#978 quadratic sample must breach the 2.83x band");
    let message = panic
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| panic.downcast_ref::<&str>().copied())
        .unwrap_or_default();
    assert!(
        message.contains("#978 unit quadratic") && message.contains("a quadratic one about 4x"),
        "#978 band diagnosis must identify the quadratic breach: {message}"
    );
}

/// #978: the band follows the factor measured from the actual input bytes.
/// A "half" that is 0.6x the full bytes makes the real factor 1/0.6 = 1.667x;
/// a quadratic implementation then reads 1/0.6^2 = 2.78x. The retired
/// hardcoded factor 2.0 band (2.83x) admitted 2.78x; the derived band
/// (1.667^1.5 = 2.15x) rejects it. This is the narrow false-PASS window the
/// first review found, closed without widening any band.
#[test]
fn scaling_band_tracks_the_measured_input_factor() {
    let factor: f64 = 1.0 / 0.6;
    let quadratic_ratio = factor * factor;
    // The 0.15 s slack is converted through the small run, so the rejection
    // only holds while the small sample is long enough that the slack cannot
    // bridge the linear-to-quadratic gap: the exact condition is
    // 0.15 / (factor^2 - factor^1.5) < small, about 0.23957 s here (0.24 s is
    // a conservative sufficient condition). The measured call sites run
    // 1.45-5.8 s (deep-frame 4 MiB measured 1.45 s, `large_shallow` 5.8 s), so
    // the slack takes at most 0.10x of that gap there.
    let floor = 0.15 / (quadratic_ratio - factor.powf(1.5));
    assert!(
        (floor - 0.2396).abs() < 0.001,
        "#978 the 0.6x quadratic is rejected only for small > {floor:.4}s"
    );
    let small = Duration::from_secs_f64(1.45); // deep-frame's measured 4 MiB run
    let sampled = ScalingSamples {
        small,
        large: small.mul_f64(quadratic_ratio),
        median_ratio: quadratic_ratio,
        samples: 3,
    };
    assert!(
        std::panic::catch_unwind(|| assert_within_scaling_band("#978 unit nominal", &sampled, 2.0))
            .is_ok(),
        "#978 the retired hardcoded 2.0 factor band admitted the 0.6x quadratic"
    );
    assert!(
        std::panic::catch_unwind(|| {
            assert_within_scaling_band("#978 unit derived", &sampled, factor)
        })
        .is_err(),
        "#978 the derived factor band must reject the 0.6x quadratic"
    );
}
