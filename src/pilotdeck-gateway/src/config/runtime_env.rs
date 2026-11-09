//! CLI-local runtime environment projection from `LocalGatewayBootConfig`.

use super::Env;

pub const DEFAULT_PERMISSION_TIMEOUT_MS: u64 = 120_000;
pub const DEFAULT_ELICITATION_TIMEOUT_MS: u64 = 120_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LocalRuntimeEnv {
    pub permission_timeout_ms: u64,
    pub elicitation_timeout_ms: u64,
    pub max_output_tokens: Option<u64>,
}

/// JavaScript `Number.parseInt(value.trim(), 10)` followed by the baseline's
/// finite/positive check and `Math.floor`. Rust consumers receive only the
/// positive integer domain used by the CLI. Finite JavaScript numbers beyond
/// Rust's duration/token range saturate at `u64::MAX`; JavaScript `Infinity`
/// remains invalid and falls back to the caller's default.
pub fn positive_integer(value: Option<&str>) -> Option<u64> {
    let value = value?.trim_start();
    let mut chars = value.chars().peekable();
    let negative = matches!(chars.peek(), Some('-'));
    if matches!(chars.peek(), Some('-' | '+')) {
        chars.next();
    }
    let digits: String = chars.take_while(char::is_ascii_digit).collect();
    if digits.is_empty() || negative {
        return None;
    }
    let parsed = digits.parse::<f64>().ok()?;
    (parsed.is_finite() && parsed > 0.0).then(|| parsed.floor() as u64)
}

pub fn resolve(env: &Env) -> LocalRuntimeEnv {
    LocalRuntimeEnv {
        permission_timeout_ms: positive_integer(
            env.get("PILOTDECK_PERMISSION_TIMEOUT_MS")
                .map(String::as_str),
        )
        .unwrap_or(DEFAULT_PERMISSION_TIMEOUT_MS),
        elicitation_timeout_ms: positive_integer(
            env.get("PILOTDECK_ELICITATION_TIMEOUT_MS")
                .map(String::as_str),
        )
        .unwrap_or(DEFAULT_ELICITATION_TIMEOUT_MS),
        max_output_tokens: positive_integer(
            env.get("PILOTDECK_MAX_OUTPUT_TOKENS").map(String::as_str),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_int_matches_the_cli_positive_integer_domain() {
        for (input, expected) in [
            (None, None),
            (Some(""), None),
            (Some("  +42ms"), Some(42)),
            (Some("120.9"), Some(120)),
            (Some("0"), None),
            (Some("-1"), None),
            (Some("Infinity"), None),
            (Some("9007199254740993"), Some(9_007_199_254_740_992)),
            (Some("18446744073709551616"), Some(u64::MAX)),
            (
                Some(
                    "999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999",
                ),
                Some(u64::MAX),
            ),
        ] {
            assert_eq!(positive_integer(input), expected, "{input:?}");
        }
        let infinite = "9".repeat(400);
        assert_eq!(positive_integer(Some(&infinite)), None);
    }

    #[test]
    fn resolve_applies_the_two_minute_defaults_and_output_override() {
        assert_eq!(
            resolve(&Env::new()),
            LocalRuntimeEnv {
                permission_timeout_ms: 120_000,
                elicitation_timeout_ms: 120_000,
                max_output_tokens: None,
            }
        );
        let env = Env::from([
            ("PILOTDECK_PERMISSION_TIMEOUT_MS".into(), "17tail".into()),
            ("PILOTDECK_ELICITATION_TIMEOUT_MS".into(), "0".into()),
            ("PILOTDECK_MAX_OUTPUT_TOKENS".into(), " 8192 ".into()),
        ]);
        assert_eq!(
            resolve(&env),
            LocalRuntimeEnv {
                permission_timeout_ms: 17,
                elicitation_timeout_ms: 120_000,
                max_output_tokens: Some(8192),
            }
        );
    }
}
