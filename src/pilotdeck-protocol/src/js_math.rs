//! JavaScript `Math.max` / `Math.min` for the values the product ports.
//!
//! Rust's `f64::max` / `f64::min` differ from JavaScript in two ways that the
//! baseline can observe:
//! - a NaN argument wins in JS, while the Rust methods return the other side;
//! - a `+0` / `-0` tie is ordered: `Math.max` returns `+0` and `Math.min`
//!   returns `-0`, while the Rust methods may return either input.
//!
//! Node v22.22.0 probe: `_tmp/20261026-issue-947/probe/js-math-zero.json`.

/// JavaScript `Math.max` over two numbers.
pub fn max(left: f64, right: f64) -> f64 {
    if left.is_nan() || right.is_nan() {
        return f64::NAN;
    }
    if left == 0.0 && right == 0.0 {
        // Math.max(+0, -0) and Math.max(-0, +0) are +0; only two -0 inputs
        // produce -0.
        return if left.is_sign_negative() && right.is_sign_negative() {
            -0.0
        } else {
            0.0
        };
    }
    if left > right { left } else { right }
}

/// JavaScript `Math.min` over two numbers.
pub fn min(left: f64, right: f64) -> f64 {
    if left.is_nan() || right.is_nan() {
        return f64::NAN;
    }
    if left == 0.0 && right == 0.0 {
        // Math.min(-0, +0) and Math.min(+0, -0) are -0; only two +0 inputs
        // produce +0.
        return if left.is_sign_negative() || right.is_sign_negative() {
            -0.0
        } else {
            0.0
        };
    }
    if left < right { left } else { right }
}

#[cfg(test)]
mod tests {
    use super::{max, min};

    const PLUS_ZERO: u64 = 0.0f64.to_bits();
    const MINUS_ZERO: u64 = (-0.0f64).to_bits();

    #[test]
    fn max_projects_positive_zero_for_every_signed_zero_tie() {
        for (index, (left, right, expected)) in [
            (0.0, -0.0, PLUS_ZERO),
            (-0.0, 0.0, PLUS_ZERO),
            (0.0, 0.0, PLUS_ZERO),
            (-0.0, -0.0, MINUS_ZERO),
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(
                max(left, right).to_bits(),
                expected,
                "#947 js_math::max tie case {index} ({left}, {right}) must project the JS sign"
            );
        }
    }

    #[test]
    fn min_projects_negative_zero_for_every_signed_zero_tie() {
        for (index, (left, right, expected)) in [
            (0.0, -0.0, MINUS_ZERO),
            (-0.0, 0.0, MINUS_ZERO),
            (0.0, 0.0, PLUS_ZERO),
            (-0.0, -0.0, MINUS_ZERO),
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(
                min(left, right).to_bits(),
                expected,
                "#947 js_math::min tie case {index} ({left}, {right}) must project the JS sign"
            );
        }
    }

    #[test]
    fn nan_wins_for_both_operations() {
        for value in [f64::NAN, -f64::NAN] {
            assert!(max(value, 1.0).is_nan(), "#947 max(NaN, 1)");
            assert!(max(1.0, value).is_nan(), "#947 max(1, NaN)");
            assert!(min(value, 1.0).is_nan(), "#947 min(NaN, 1)");
            assert!(min(1.0, value).is_nan(), "#947 min(1, NaN)");
        }
    }

    #[test]
    fn ordinary_numbers_and_infinities_keep_the_js_order() {
        assert_eq!(max(-2.0, -3.0), -2.0);
        assert_eq!(min(-2.0, -3.0), -3.0);
        assert_eq!(max(f64::NEG_INFINITY, -1.0), -1.0);
        assert_eq!(min(f64::INFINITY, 1.0), 1.0);
        assert_eq!(max(f64::INFINITY, f64::INFINITY), f64::INFINITY);
        assert_eq!(min(f64::NEG_INFINITY, f64::NEG_INFINITY), f64::NEG_INFINITY);
    }
}
