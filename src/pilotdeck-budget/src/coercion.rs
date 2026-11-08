//! Shared original ECMAScript coercions; no IO or budget authority.
pub use pilotdeck_protocol::coercion::{
    MAX_ARRAY_DEPTH, StackOverflow, json_stringify, string_to_number, to_number, to_string_units,
};
