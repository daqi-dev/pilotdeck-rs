//! Pure hook configuration and protocol helpers; no hook execution or host IO.
pub mod config;
mod identifier_ranges;
pub mod protocol;
mod regexp;
pub(crate) use regexp::test as test_flagless_regexp;
