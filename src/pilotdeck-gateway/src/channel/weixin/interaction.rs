//! Shared IM helper adaptation; no second session/permission owner.
use super::*;
pub(super) struct AnswerGuard {
    pub channel: WeixinChannel,
    pub user: JsString,
    pub token: Arc<()>,
}
impl Drop for AnswerGuard {
    fn drop(&mut self) {
        let mut inner = self.channel.lock_inner();
        if inner
            .permission_answering
            .get(&self.user)
            .is_some_and(|value| Arc::ptr_eq(value, &self.token))
        {
            inner.permission_answering.remove(&self.user);
        }
    }
}
pub(super) struct LiveGuard {
    pub channel: WeixinChannel,
    pub user: JsString,
    pub token: Arc<()>,
}
impl Drop for LiveGuard {
    fn drop(&mut self) {
        let mut inner = self.channel.lock_inner();
        if inner
            .active_live
            .get(&self.user)
            .is_some_and(|(token, _)| Arc::ptr_eq(token, &self.token))
        {
            inner.active_live.remove(&self.user);
        }
    }
}
impl WeixinChannel {
    pub(super) async fn answer_interaction(&self, user: JsString, text: String, permission: bool) {
        let chat = String::from_utf16_lossy(user.units());
        let helper = self.0.interaction.clone();
        if !permission {
            match helper.elicitation_answer(&chat, &text).await {
                Ok(Some(reply)) => {
                    self.send_reply(&user, &reply.into(), false).await;
                }
                Ok(None) => {}
                Err(error) => self.log(
                    "error",
                    format!("weixin: elicitation answer error: {error}"),
                ),
            };
            return;
        }
        let answer = match helper.permission_answer(&chat, &text).await {
            Ok(Some(answer)) => answer,
            Ok(None) => return,
            Err(error) => {
                self.log("error", format!("weixin: permission answer error: {error}"));
                return;
            }
        };
        let token = answer.answer_token;
        if let Some(reply) = answer.text.as_ref().filter(|s| !s.is_empty()) {
            if !self.send_reply(&user, &reply.as_str().into(), false).await {
                helper.permission_release_answer(&chat, token);
                return;
            }
            if !answer.can_advance && !answer.retry_prompt {
                return;
            }
            if let Some(next) = helper.permission_take_next_prompt(&chat, token) {
                let request = helper.permission_prompt_request_id(&chat, token);
                if !self.send_reply(&user, &next.as_str().into(), false).await {
                    helper.permission_release_answer(&chat, token);
                    self.log(
                        "error",
                        "weixin: permission answer error: Error: permission prompt delivery failed"
                            .into(),
                    );
                    return;
                }
                helper.permission_confirm_next(&chat, true, request.as_deref(), token);
            }
            if matches!(crate::metadata::js_trim(&text), "1" | "2")
                && reply.starts_with("已允许")
                && !helper.permission_pending(&chat)
                && let Some((_, resume)) = self.lock_inner().active_live.get(&user)
            {
                let _ = resume.send(());
            }
        }
    }
    pub(super) fn register_live(&self, user: &JsString) -> (watch::Receiver<()>, LiveGuard) {
        let (tx, rx) = watch::channel(());
        let token = Arc::new(());
        self.lock_inner()
            .active_live
            .insert(user.clone(), (token.clone(), tx));
        (
            rx,
            LiveGuard {
                channel: self.clone(),
                user: user.clone(),
                token,
            },
        )
    }
}
