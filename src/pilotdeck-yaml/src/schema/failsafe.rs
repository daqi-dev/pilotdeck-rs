// Port of yaml@2.9.0 dist/schema/tags.js failsafe table (ISC).
use super::{Tag, common, tags};
pub(crate) fn tags() -> Vec<Tag> {
    vec![tags::map(), tags::seq(), common::string::tag()]
}
