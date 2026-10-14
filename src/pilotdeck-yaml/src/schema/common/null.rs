// Port of yaml@2.9.0 dist/schema/common/null.js (ISC); parsing tags only.
use crate::schema::{Resolve, Tag};
use regex::Regex;
use std::sync::LazyLock;
static TAG_TEST: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:~|[Nn]ull|NULL)?$").unwrap());
pub(crate) fn tag() -> Tag {
    Tag {
        name: "tag:yaml.org,2002:null",
        default: true,
        key_only: false,
        pattern: Some(&TAG_TEST),
        resolve: Resolve::Null,
        format: None,
        collection: None,
    }
}
