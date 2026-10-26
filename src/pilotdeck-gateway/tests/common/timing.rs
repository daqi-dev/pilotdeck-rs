//! Multi-sample timing budgets shared by the web HTML conversion regressions:
//! the `html_markdown` integration tests and the `html_compat` unit tests.
//!
//! A single sample plus an absolute wall clock turns red whenever the host
//! preempts the conversion once. These helpers instead interleave two sizes of
//! the same shape, keep the fastest run of each, and check the scaling ratio;
//! the callers keep a loose absolute cap as the constant-factor backstop.

use std::time::Duration;

/// Timing samples per size. Three is the default; the quadratic negative
/// controls in `tests/fixtures/html-timing/negative_controls.py` lower it to
/// one because every mutant sample costs minutes.
pub fn timing_samples() -> usize {
    std::env::var("PDRS_TIMING_SAMPLES")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|samples| (1..=9).contains(samples))
        .unwrap_or(3)
}

/// Interleave the two sizes and keep the fastest run of each. A single
/// preempted sample must not decide the ratio on a loaded host; alternating
/// the order also keeps the first allocation from always falling on the
/// same size. The sampler receives `true` for the full size and `false` for
/// the half size.
pub fn fastest_scaling<S>(label: &str, samples: usize, mut sample: S) -> (Duration, Duration)
where
    S: FnMut(bool) -> Duration,
{
    let mut half_fastest = Duration::MAX;
    let mut full_fastest = Duration::MAX;
    for round in 0..samples {
        let (half_elapsed, full_elapsed) = if round % 2 == 0 {
            (sample(false), sample(true))
        } else {
            let full_elapsed = sample(true);
            (sample(false), full_elapsed)
        };
        eprintln!(
            "{label} sample {}: half in {half_elapsed:?}, full in {full_elapsed:?}",
            round + 1
        );
        half_fastest = half_fastest.min(half_elapsed);
        full_fastest = full_fastest.min(full_elapsed);
    }
    (half_fastest, full_fastest)
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
