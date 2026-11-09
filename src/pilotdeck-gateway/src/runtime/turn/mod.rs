//! Gateway turn runtime: replay store, event coordination and AgentEvent → GatewayEvent projection.
pub mod admission;
pub(crate) mod capabilities;
pub mod coordinator;
pub mod failure_guard;
pub mod execute_payload;
pub mod gateway;
pub mod history;
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
pub mod routes;
pub mod services;
pub mod runner;
pub mod submit;
pub mod submit_host;
pub(crate) mod submit_input;
pub(crate) mod submit_uploads;
pub mod wiring;
pub mod wiring_submit;
