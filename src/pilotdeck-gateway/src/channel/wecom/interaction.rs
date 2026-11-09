//! Answer existing gateway-owned prompts before a new message can start a turn.
use super::{delivery, transport::ActiveTransport};
use crate::channel::{im_interaction::InteractionPort, protocol::ChannelError};

pub(crate) async fn answer_pending(
    interaction: &dyn InteractionPort,
    transport: &ActiveTransport,
    key: &str,
    text: &str,
    request_id: &delivery::ReplyTarget,
) -> Result<bool, ChannelError> {
    if interaction.elicitation_pending(key) {
        if let Some(confirmation) = interaction.elicitation_answer(key, text).await? {
            delivery::reply(transport, request_id.clone(), confirmation.into()).await;
        }
        return Ok(true);
    }
    if !interaction.permission_pending(key) {
        return Ok(false);
    }
    let Some(answer) = interaction.permission_answer(key, text).await? else {
        return Ok(true);
    };
    if let Some(text) = answer.text {
        if !delivery::reply(transport, request_id.clone(), text.into()).await {
            interaction.permission_release_answer(key, answer.answer_token);
            return Ok(true);
        }
        if !answer.can_advance && !answer.retry_prompt {
            return Ok(true);
        }
        if let Some(prompt) = interaction.permission_take_next_prompt(key, answer.answer_token) {
            let next_id = interaction.permission_prompt_request_id(key, answer.answer_token);
            let delivered = delivery::reply(transport, request_id.clone(), prompt.into()).await;
            interaction.permission_confirm_next(
                key,
                delivered,
                next_id.as_deref(),
                answer.answer_token,
            );
        }
    }
    Ok(true)
}
