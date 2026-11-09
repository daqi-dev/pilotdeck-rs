#![cfg_attr(not(test), allow(dead_code))]
pub(crate) mod client;

pub(crate) mod authority;
mod binding;
mod common;
pub(crate) mod controller;
mod definitions;
mod discovery;
#[cfg(test)]
mod execution_tests;
mod tool;
