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
mod timeline;

pub(crate) mod loop_control;
pub(crate) mod sop_executor;

const TIMEOUT_REASON_PREFIX: &str = "timeout:";

pub(crate) fn timeout_reason(
    run_id: &pilotdeck_protocol::JsString,
) -> pilotdeck_protocol::JsString {
    pilotdeck_protocol::JsString::from_utf16(
        TIMEOUT_REASON_PREFIX
            .encode_utf16()
            .chain(run_id.units().iter().copied())
            .collect(),
    )
}

pub(crate) fn is_timeout_reason(
    reason: &pilotdeck_protocol::JsString,
    run_id: &pilotdeck_protocol::JsString,
) -> bool {
    reason.units().iter().copied().eq(TIMEOUT_REASON_PREFIX
        .encode_utf16()
        .chain(run_id.units().iter().copied()))
}
