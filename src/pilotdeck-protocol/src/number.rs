//! JavaScript `Number` formatting shared by every crate that reproduces a
//! baseline string. One implementation per ECMA-262 algorithm.

/// `Number.prototype.toString()` (radix 10), via the same ryu-js formatter the
/// JSON codec uses. `-0` prints as `"0"`.
pub fn number_to_string(number: f64) -> String {
    if number.is_nan() {
        "NaN".into()
    } else if number.is_infinite() {
        (if number > 0.0 {
            "Infinity"
        } else {
            "-Infinity"
        })
        .into()
    } else if number == 0.0 {
        "0".into()
    } else {
        ryu_js::Buffer::new().format_finite(number).to_owned()
    }
}

/// `Number.prototype.toFixed(6)`: the exact binary rational is scaled by 1e6
/// and rounded to the nearest integer; an exact half rounds the magnitude up,
/// i.e. away from zero for negatives, as V8 does (ECMA-262 §21.1.3.3). Values
/// with `|x| >= 1e21` and non-finite values fall back to `toString`. A
/// negative value that rounds to zero keeps its sign (`"-0.000000"`), `-0`
/// does not. For `|x| < 1e21`, `x * 1e6 < 2^90` and the significand times
/// 1e6 is below `2^73`, so the `u128` arithmetic cannot overflow.
pub fn to_fixed_6(number: f64) -> String {
    if !number.is_finite() || number.abs() >= 1e21 {
        return number_to_string(number);
    }
    let bits = number.abs().to_bits();
    let biased = ((bits >> 52) & 0x7ff) as i32;
    let fraction = bits & ((1_u64 << 52) - 1);
    let (significand, exponent) = if biased == 0 {
        (fraction, -1074)
    } else {
        (fraction | (1_u64 << 52), biased - 1023 - 52)
    };
    let scaled = u128::from(significand) * 1_000_000;
    let integer = if exponent >= 0 {
        scaled << exponent as u32
    } else {
        let shift = (-exponent) as u32;
        if shift >= 128 {
            0
        } else {
            let divisor = 1_u128 << shift;
            scaled / divisor + u128::from(scaled % divisor >= divisor / 2)
        }
    };
    format!(
        "{}{}.{:06}",
        if number < 0.0 { "-" } else { "" },
        integer / 1_000_000,
        integer % 1_000_000
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Expected strings recorded from Node v22 `String(v)` / `v.toFixed(6)`
    /// (`_tmp/20261019-p2-sweep-6/probe-270.mjs`, real run).
    #[test]
    fn to_fixed_6_matches_node() {
        for (value, expected) in [
            (0.0, "0.000000"),
            (-0.0, "0.000000"),
            (5e-7, "0.000000"),
            (0.0000015, "0.000002"),
            (0.0000025, "0.000003"),
            (-5e-7, "-0.000000"),
            (1e-7, "0.000000"),
            (-1e-7, "-0.000000"),
            (0.1, "0.100000"),
            (0.123456789, "0.123457"),
            (1.0000005, "1.000001"),
            (2.5e-7, "0.000000"),
            (5e-324, "0.000000"),
            (-5e-324, "-0.000000"),
            (2.2250738585072014e-308, "0.000000"),
            (1.7976931348623157e308, "1.7976931348623157e+308"),
            (999999999999999999999.0, "1e+21"),
            (1e21, "1e+21"),
            (-1e21, "-1e+21"),
            (1e20, "100000000000000000000.000000"),
            (123456789.1234565, "123456789.123456"),
            (0.0000045, "0.000005"),
            (9.9999995, "9.999999"),
            (f64::NAN, "NaN"),
            (f64::INFINITY, "Infinity"),
            (f64::NEG_INFINITY, "-Infinity"),
            (4503599627370496.5, "4503599627370496.000000"),
            (9007199254740993.0, "9007199254740992.000000"),
            (0.3, "0.300000"),
            // Exact binary half ties round up, not to even.
            (0.0078125, "0.007813"),
            (-0.0078125, "-0.007813"),
            (0.0234375, "0.023438"),
            // Biased exponent 947: shift == 128, the first short-circuited
            // shift (1.3234889800848443e-23).
            (f64::from_bits(947_u64 << 52), "0.000000"),
        ] {
            assert_eq!(to_fixed_6(value), expected, "{value:e}");
        }
    }
}
