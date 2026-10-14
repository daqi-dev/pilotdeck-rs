// Port of yaml@2.9.0 dist/schema/yaml-1.1/float.js (ISC); parsing tags only.
use crate::schema::{Resolve, Tag};
use regex::Regex;
use std::sync::LazyLock;
static FLOAT_NA_N_TEST: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:[-+]?\.(?:inf|Inf|INF)|\.nan|\.NaN|\.NAN)$").unwrap());
pub(crate) fn float_na_n() -> Tag {
    Tag {
        name: "tag:yaml.org,2002:float",
        default: true,
        key_only: false,
        pattern: Some(&FLOAT_NA_N_TEST),
        resolve: Resolve::FloatNaN,
        format: None,
        collection: None,
    }
}
static FLOAT_EXP_TEST: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[-+]?(?:[0-9][0-9_]*)?(?:\.[0-9_]*)?[eE][-+]?[0-9]+$").unwrap());
pub(crate) fn float_exp() -> Tag {
    Tag {
        name: "tag:yaml.org,2002:float",
        default: true,
        key_only: false,
        pattern: Some(&FLOAT_EXP_TEST),
        resolve: Resolve::Float11,
        format: Some("EXP"),
        collection: None,
    }
}
static FLOAT_TEST: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[-+]?(?:[0-9][0-9_]*)?\.[0-9_]*$").unwrap());
pub(crate) fn float() -> Tag {
    Tag {
        name: "tag:yaml.org,2002:float",
        default: true,
        key_only: false,
        pattern: Some(&FLOAT_TEST),
        resolve: Resolve::Float11,
        format: None,
        collection: None,
    }
}
