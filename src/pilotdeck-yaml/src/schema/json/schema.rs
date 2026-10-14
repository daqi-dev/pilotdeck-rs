// Port of yaml@2.9.0 dist/schema/json/schema.js (ISC); parsing only.
use crate::schema::{Resolve, Tag, common, tags};
use regex::Regex;
use std::sync::LazyLock;
static NULL: LazyLock<Regex> = LazyLock::new(|| Regex::new("^null$").unwrap());
static BOOL: LazyLock<Regex> = LazyLock::new(|| Regex::new("^true$|^false$").unwrap());
static INT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^-?(?:0|[1-9][0-9]*)$").unwrap());
static FLOAT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^-?(?:0|[1-9][0-9]*)(?:\.[0-9]*)?(?:[eE][-+]?[0-9]+)?$").unwrap()
});
static ERROR: LazyLock<Regex> = LazyLock::new(|| Regex::new("^").unwrap());
pub(crate) fn tags() -> Vec<Tag> {
    let mut tags = vec![tags::map(), tags::seq(), common::string::tag()];
    for (name, pattern, resolve) in [
        ("tag:yaml.org,2002:null", &NULL, Resolve::Null),
        ("tag:yaml.org,2002:bool", &BOOL, Resolve::Bool),
        ("tag:yaml.org,2002:int", &INT, Resolve::Int(0, 10)),
        ("tag:yaml.org,2002:float", &FLOAT, Resolve::Float),
        ("", &ERROR, Resolve::JsonError),
    ] {
        tags.push(Tag {
            name,
            default: true,
            key_only: false,
            pattern: Some(pattern),
            resolve,
            format: None,
            collection: None,
        });
    }
    tags
}
