// Port of yaml@2.9.0 dist/schema/core/schema.js (ISC).
use super::{bool, float, int};
use crate::schema::{Tag, common, tags};
pub(crate) fn tags() -> Vec<Tag> {
    vec![
        tags::map(),
        tags::seq(),
        common::string::tag(),
        common::null::tag(),
        bool::tag(),
        int::int_oct(),
        int::int(),
        int::int_hex(),
        float::float_na_n(),
        float::float_exp(),
        float::float(),
    ]
}
