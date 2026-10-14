// Port of yaml@2.9.0 dist/schema/yaml-1.1/bool.js (ISC); parsing tags only.
use crate::schema::{Resolve, Tag};
use regex::Regex;
use std::sync::LazyLock;
static TRUE_TAG_TEST: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:Y|y|[Yy]es|YES|[Tt]rue|TRUE|[Oo]n|ON)$").unwrap());
pub(crate) fn true_tag() -> Tag {
    Tag {
        name: "tag:yaml.org,2002:bool",
        default: true,
        key_only: false,
        pattern: Some(&TRUE_TAG_TEST),
        resolve: Resolve::True,
        format: None,
        collection: None,
    }
}
static FALSE_TAG_TEST: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:N|n|[Nn]o|NO|[Ff]alse|FALSE|[Oo]ff|OFF)$").unwrap());
pub(crate) fn false_tag() -> Tag {
    Tag {
        name: "tag:yaml.org,2002:bool",
        default: true,
        key_only: false,
        pattern: Some(&FALSE_TAG_TEST),
        resolve: Resolve::False,
        format: None,
        collection: None,
    }
}
