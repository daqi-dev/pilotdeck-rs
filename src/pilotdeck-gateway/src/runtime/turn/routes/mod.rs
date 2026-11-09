//! Native TURN method ownership. Every admitted method has a Rust owner.
pub mod control;
pub mod models;
pub mod reads;
pub mod usage;

pub const METHODS: &[&str] = &[
    "submit_turn",
    "steer_turn",
    "cancel_steer",
    "abort_turn",
    "run_get",
    "run_events",
    "run_reattach",
    "active_turn_snapshot",
    "session_model_set",
    "session_model_clear",
    "usage_snapshot",
    "model_usage_snapshot",
];
