// Port of yaml@2.9.0 dist/schema/core/int.js (ISC); parsing tags only.
use crate::schema::{Resolve, Tag};
use regex::Regex;
use std::sync::LazyLock;
static INT_OCT_TEST: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^0o[0-7]+$").unwrap());
pub(crate) fn int_oct() -> Tag {
    Tag {
        name: "tag:yaml.org,2002:int",
        default: true,
        key_only: false,
        pattern: Some(&INT_OCT_TEST),
        resolve: Resolve::Int(2, 8),
        format: Some("OCT"),
        collection: None,
    }
}
static INT_TEST: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[-+]?[0-9]+$").unwrap());
pub(crate) fn int() -> Tag {
    Tag {
        name: "tag:yaml.org,2002:int",
        default: true,
        key_only: false,
        pattern: Some(&INT_TEST),
        resolve: Resolve::Int(0, 10),
        format: None,
        collection: None,
    }
}
static INT_HEX_TEST: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^0x[0-9a-fA-F]+$").unwrap());
pub(crate) fn int_hex() -> Tag {
    Tag {
        name: "tag:yaml.org,2002:int",
        default: true,
        key_only: false,
        pattern: Some(&INT_HEX_TEST),
        resolve: Resolve::Int(2, 16),
        format: Some("HEX"),
        collection: None,
    }
}
