//! Gateway turn runtime: replay store, event coordination and AgentEvent → GatewayEvent projection.
pub mod admission;
pub mod coordinator;
pub mod failure_guard;
pub mod gateway;
pub mod host_factory;
pub mod interaction;
pub mod live;
mod live_session;
pub mod projector;
#[cfg(all(test, unix))]
mod projector_cwd_tests;
pub mod publish;
pub mod read;
pub mod replay;
pub mod runner;
pub mod submit;
pub mod submit_host;
