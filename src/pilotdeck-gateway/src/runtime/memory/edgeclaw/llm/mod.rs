//! EdgeClaw LLM transport, prompt construction and model-output parsing.
//!
//! Each file owns one concern so the remaining A6.6 work packages can add
//! `config`, `transport`, `recall`, `index` and `dream` without touching the
//! others. Only [`envelope`] exists today (A6.6-edgeclaw-7).
pub mod envelope;
