// Port of yaml@2.9.0 dist/schema/tags.js and common/{map,seq}.js (ISC).
use super::{Resolve, Tag};
use regex::Regex;
use std::sync::LazyLock;
fn collection(name: &'static str, kind: &'static str, resolve: Resolve, default: bool) -> Tag {
    Tag {
        name,
        default,
        key_only: false,
        pattern: None,
        resolve,
        format: None,
        collection: Some(kind),
    }
}
pub(crate) fn map() -> Tag {
    collection("tag:yaml.org,2002:map", "map", Resolve::Map, true)
}
pub(crate) fn seq() -> Tag {
    collection("tag:yaml.org,2002:seq", "seq", Resolve::Seq, true)
}
pub(crate) fn omap() -> Tag {
    collection("tag:yaml.org,2002:omap", "seq", Resolve::OMap, false)
}
pub(crate) fn pairs() -> Tag {
    collection("tag:yaml.org,2002:pairs", "seq", Resolve::Pairs, false)
}
pub(crate) fn set() -> Tag {
    collection("tag:yaml.org,2002:set", "map", Resolve::Set, false)
}
pub(crate) fn binary() -> Tag {
    Tag {
        name: "tag:yaml.org,2002:binary",
        default: false,
        key_only: false,
        pattern: None,
        resolve: Resolve::Binary,
        format: None,
        collection: None,
    }
}
static MERGE: LazyLock<Regex> = LazyLock::new(|| Regex::new("^<<$").unwrap());
pub(crate) fn merge() -> Tag {
    Tag {
        name: "tag:yaml.org,2002:merge",
        default: false,
        key_only: true,
        pattern: Some(&MERGE),
        resolve: Resolve::Merge,
        format: None,
        collection: None,
    }
}
pub(crate) fn known(name: &pilotdeck_protocol::JsString) -> Option<Tag> {
    [
        binary(),
        merge(),
        omap(),
        pairs(),
        set(),
        super::yaml_1_1::timestamp::timestamp(),
    ]
    .into_iter()
    .find(|t| name.equals_str(t.name))
}
pub(crate) fn get_tags(name: &str) -> Vec<Tag> {
    match name {
        "yaml-1.1" | "yaml11" => super::yaml_1_1::schema::tags(),
        "json" => super::json::schema::tags(),
        "failsafe" => super::failsafe::tags(),
        _ => super::core::schema::tags(),
    }
}
