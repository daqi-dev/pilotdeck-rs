// Port of yaml@2.9.0 dist/schema/common/string.js (ISC).
use crate::schema::{Resolve, Tag};
pub(crate) fn tag() -> Tag {
    Tag {
        name: "tag:yaml.org,2002:str",
        default: true,
        key_only: false,
        pattern: None,
        resolve: Resolve::String,
        format: None,
        collection: None,
    }
}
