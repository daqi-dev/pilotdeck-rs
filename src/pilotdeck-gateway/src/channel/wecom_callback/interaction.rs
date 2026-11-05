//! The slice of `ImElicitationHelper` / `ImPermissionHelper` that
//! `WeComCallbackChannel.ts:216-251` drives.
//!
//! Those helpers live outside `adapters/channel/wecom-callback/`; this unit
//! ports the channel-side orchestration against this narrow port. Assembly
//! supplies a shared implementation, and the TS oracle/Rust tests supply the
//! same scripted fake so the comparison covers the channel, not helper
//! internals.

use std::{future::Future, pin::Pin};

use pilotdeck_protocol::JsValue;

use super::super::protocol::ChannelError;

pub(crate) type InteractionFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PermissionAnswer {
    pub text: Option<String>,
    pub can_advance: bool,
    pub retry_prompt: bool,
    pub answer_token: Option<u64>,
}

pub trait InteractionPort: Send + Sync {
    fn elicitation_pending(&self, chat_id: &str) -> bool;
    fn elicitation_capture(&self, chat_id: &str, session_key: &str, event: &JsValue) -> String;
    fn elicitation_answer<'a>(
        &'a self,
        chat_id: &'a str,
        text: &'a str,
    ) -> InteractionFuture<'a, Result<Option<String>, ChannelError>>;
    fn elicitation_clear(&self, chat_id: &str);

    fn permission_pending(&self, chat_id: &str) -> bool;
    fn permission_capture(
        &self,
        chat_id: &str,
        session_key: &str,
        event: &JsValue,
    ) -> Option<String>;
    fn permission_confirm_initial(&self, chat_id: &str, delivered: bool, request_id: Option<&str>);
    fn permission_answer<'a>(
        &'a self,
        chat_id: &'a str,
        text: &'a str,
    ) -> InteractionFuture<'a, Result<Option<PermissionAnswer>, ChannelError>>;
    fn permission_release_answer(&self, chat_id: &str, answer_token: Option<u64>);
    fn permission_take_next_prompt(
        &self,
        chat_id: &str,
        answer_token: Option<u64>,
    ) -> Option<String>;
    fn permission_prompt_request_id(
        &self,
        chat_id: &str,
        answer_token: Option<u64>,
    ) -> Option<String>;
    fn permission_confirm_next(
        &self,
        chat_id: &str,
        delivered: bool,
        request_id: Option<&str>,
        answer_token: Option<u64>,
    );
    fn permission_clear_after_turn(&self, chat_id: &str);
}

/// The `hasPending`-only default: no interaction state, so inbound text flows
/// to the gateway. An assembly that has not installed the shared helpers yet
/// still gets the TS text path.
#[derive(Default)]
pub(crate) struct NoInteraction;

impl InteractionPort for NoInteraction {
    fn elicitation_pending(&self, _chat_id: &str) -> bool {
        false
    }
    fn elicitation_capture(&self, _: &str, _: &str, _: &JsValue) -> String {
        String::new()
    }
    fn elicitation_answer<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
    ) -> InteractionFuture<'a, Result<Option<String>, ChannelError>> {
        Box::pin(async { Ok(None) })
    }
    fn elicitation_clear(&self, _: &str) {}

    fn permission_pending(&self, _: &str) -> bool {
        false
    }
    fn permission_capture(&self, _: &str, _: &str, _: &JsValue) -> Option<String> {
        None
    }
    fn permission_confirm_initial(&self, _: &str, _: bool, _: Option<&str>) {}
    fn permission_answer<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
    ) -> InteractionFuture<'a, Result<Option<PermissionAnswer>, ChannelError>> {
        Box::pin(async { Ok(None) })
    }
    fn permission_release_answer(&self, _: &str, _: Option<u64>) {}
    fn permission_take_next_prompt(&self, _: &str, _: Option<u64>) -> Option<String> {
        None
    }
    fn permission_prompt_request_id(&self, _: &str, _: Option<u64>) -> Option<String> {
        None
    }
    fn permission_confirm_next(&self, _: &str, _: bool, _: Option<&str>, _: Option<u64>) {}
    fn permission_clear_after_turn(&self, _: &str) {}
}
