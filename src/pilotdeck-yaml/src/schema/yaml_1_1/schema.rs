// Port of yaml@2.9.0 dist/schema/yaml-1.1/schema.js (ISC).
use super::{bool, float, int, timestamp};
use crate::schema::{Tag, common, tags};
pub(crate) fn tags() -> Vec<Tag> {
    vec![
        tags::map(),
        tags::seq(),
        common::string::tag(),
        common::null::tag(),
        bool::true_tag(),
        bool::false_tag(),
        int::int_bin(),
        int::int_oct(),
        int::int(),
        int::int_hex(),
        float::float_na_n(),
        float::float_exp(),
        float::float(),
        tags::binary(),
        tags::merge(),
        tags::omap(),
        tags::pairs(),
        tags::set(),
        timestamp::int_time(),
        timestamp::float_time(),
        timestamp::timestamp(),
    ]
}
