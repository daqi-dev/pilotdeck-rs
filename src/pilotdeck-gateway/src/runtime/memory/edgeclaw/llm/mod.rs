//! EdgeClaw LLM transport, prompt construction and model-output parsing.
//!
//! Each file owns one concern so the remaining A6.6 work packages can add
//! `transport`, `recall`, `index` and `dream` without touching the others.
//! [`config`] (A6.6-edgeclaw-5) and [`envelope`] (A6.6-edgeclaw-7) exist today.
pub mod config;
pub mod envelope;
pub mod index;

pub mod transport;
