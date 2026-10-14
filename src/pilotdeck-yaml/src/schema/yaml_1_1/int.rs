// Port of yaml@2.9.0 dist/schema/yaml-1.1/int.js (ISC); parsing tags only.
use crate::schema::{Resolve, Tag};
use regex::Regex;
use std::sync::LazyLock;
static INT_BIN_TEST: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[-+]?0b[0-1_]+$").unwrap());
pub(crate) fn int_bin() -> Tag {
    Tag {
        name: "tag:yaml.org,2002:int",
        default: true,
        key_only: false,
        pattern: Some(&INT_BIN_TEST),
        resolve: Resolve::Int11(2, 2),
        format: Some("BIN"),
        collection: None,
    }
}
static INT_OCT_TEST: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[-+]?0[0-7_]+$").unwrap());
pub(crate) fn int_oct() -> Tag {
    Tag {
        name: "tag:yaml.org,2002:int",
        default: true,
        key_only: false,
        pattern: Some(&INT_OCT_TEST),
        resolve: Resolve::Int11(1, 8),
        format: Some("OCT"),
        collection: None,
    }
}
static INT_TEST: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[-+]?[0-9][0-9_]*$").unwrap());
pub(crate) fn int() -> Tag {
    Tag {
        name: "tag:yaml.org,2002:int",
        default: true,
        key_only: false,
        pattern: Some(&INT_TEST),
        resolve: Resolve::Int11(0, 10),
        format: None,
        collection: None,
    }
}
static INT_HEX_TEST: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[-+]?0x[0-9a-fA-F_]+$").unwrap());
pub(crate) fn int_hex() -> Tag {
    Tag {
        name: "tag:yaml.org,2002:int",
        default: true,
        key_only: false,
        pattern: Some(&INT_HEX_TEST),
        resolve: Resolve::Int11(2, 16),
        format: Some("HEX"),
        collection: None,
    }
}
