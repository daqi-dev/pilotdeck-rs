//! Gateway turn runtime: replay store, event coordination and AgentEvent → GatewayEvent projection.
pub mod coordinator;
pub mod failure_guard;
pub mod gateway;
pub mod interaction;
pub mod live;
pub mod projector;
pub mod publish;
pub mod read;
pub mod replay;
pub mod submit;
pub mod submit_host;
