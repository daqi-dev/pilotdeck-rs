//! Gateway turn runtime: replay store, event coordination and AgentEvent → GatewayEvent projection.
pub mod coordinator;
pub mod projector;
pub mod publish;
pub mod replay;
pub mod submit;
