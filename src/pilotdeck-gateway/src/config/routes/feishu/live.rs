//! Native live post transport. Timing follows ImLiveReplyController defaults;
//! post failure falls back to text and editing keeps the same message handle.
use super::*;
use std::time::Instant;
#[derive(Default)]
pub(super) struct Live {
    pub text: String,
    handle: Option<(String, bool)>,
    visible: String,
    first: Option<Instant>,
    last: Option<Instant>,
    activity: Option<(Instant, &'static str)>,
    paused: bool,
    closed: bool,
    edit_disabled: bool,
}
impl Live {
    pub fn activity(&mut self, kind: &'static str) {
        if self.text.is_empty() {
            if self.activity.is_none_or(|(_, current)| current != kind) {
                self.activity = Some((Instant::now(), kind));
            }
            self.paused = false;
        }
    }
    pub fn pause(&mut self) {
        self.paused = true;
    }
    pub async fn append(&mut self, owner: &FeishuChannel, chat: &str, text: &str) {
        let mut rest = text.encode_utf16().collect::<Vec<_>>();
        while !rest.is_empty() {
            let available = 3998usize.saturating_sub(self.text.encode_utf16().count());
            if available == 0 {
                self.finish_segment(owner, chat).await;
                continue;
            }
            let length = available.min(rest.len());
            self.text
                .push_str(&String::from_utf16_lossy(&rest[..length]));
            rest.drain(..length);
            self.first.get_or_insert_with(Instant::now);
            self.paused = false;
            let pending = self
                .text
                .encode_utf16()
                .count()
                .saturating_sub(self.visible.encode_utf16().count());
            if (self.handle.is_none() && self.text.encode_utf16().count() >= 24)
                || (self.handle.is_some() && pending >= 96)
            {
                self.flush(owner, chat, false, false).await;
            }
            if !rest.is_empty() {
                self.finish_segment(owner, chat).await;
            }
        }
    }
    pub async fn tick(&mut self, owner: &FeishuChannel, chat: &str) {
        if self.closed || self.paused {
            return;
        }
        if !self.text.is_empty() {
            let due = if self.handle.is_none() {
                self.first
                    .is_some_and(|first| first.elapsed() >= Duration::from_millis(800))
            } else {
                self.last
                    .is_some_and(|last| last.elapsed() >= Duration::from_secs(2))
            };
            if due {
                self.flush(owner, chat, false, false).await;
            }
        } else if self
            .activity
            .is_some_and(|(started, _)| started.elapsed() >= Duration::from_millis(2500))
            && self.handle.is_none()
        {
            self.flush(owner, chat, false, true).await;
        }
    }
    pub async fn boundary(&mut self, owner: &FeishuChannel, chat: &str) {
        if !self.text.trim().is_empty() {
            self.finish_segment(owner, chat).await;
        }
    }
    async fn finish_segment(&mut self, owner: &FeishuChannel, chat: &str) {
        self.flush(owner, chat, true, false).await;
        *self = Self::default();
    }
    pub async fn finish(&mut self, owner: &FeishuChannel, chat: &str) {
        if self.closed {
            return;
        }
        self.flush(owner, chat, true, false).await;
        self.closed = true;
    }
    async fn flush(&mut self, owner: &FeishuChannel, chat: &str, final_: bool, activity: bool) {
        let text = if self.text.trim().is_empty() {
            if !final_ && !activity {
                return;
            }
            if final_ {
                if self.handle.is_none() {
                    return;
                }
                "处理完成，但没有可见回复。".to_owned()
            } else {
                match self.activity.map(|(_, kind)| kind).unwrap_or("thinking") {
                    "tool" => "正在执行工具…",
                    "subagent" => "正在处理子任务…",
                    _ => "正在思考…",
                }
                .into()
            }
        } else {
            crate::metadata::js_trim(&self.text).into()
        };
        if text == self.visible && (!final_ || self.handle.is_none()) {
            return;
        }
        let post = json!({"zh_cn":{"content":[[{"tag":"text","text":text}]]}});
        if let Some((id, live_post)) = &self.handle {
            if !self.edit_disabled {
                let content = if *live_post {
                    post.clone()
                } else {
                    json!({"text":text})
                };
                let kind = if *live_post { "post" } else { "text" };
                if owner
                    .rest(
                        "PUT",
                        &format!("/im/v1/messages/{}", http::encode(id)),
                        Some(json!({"msg_type":kind,"content":content.to_string()})),
                    )
                    .await
                    .is_ok()
                {
                    self.visible = text;
                    self.last = Some(Instant::now());
                    return;
                }
                if *live_post&&owner.rest("PUT",&format!("/im/v1/messages/{}",http::encode(id)),Some(json!({"msg_type":"text","content":json!({"text":text}).to_string()}))).await.is_ok(){self.handle=Some((id.clone(),false));self.visible=text;self.last=Some(Instant::now());return;}
                self.edit_disabled = true;
            }
        }
        // If a previous visible post cannot be edited, only send the unseen
        // suffix, preserving the controller's continuation behavior.
        let text = if self.edit_disabled && !self.visible.is_empty() {
            text.strip_prefix(&self.visible).unwrap_or(&text).to_owned()
        } else {
            text
        };
        if text.trim().is_empty() {
            return;
        }
        let post = json!({"zh_cn":{"content":[[{"tag":"text","text":text}]]}});
        let sent = owner
            .rest(
                "POST",
                "/im/v1/messages?receive_id_type=chat_id",
                Some(json!({"receive_id":chat,"msg_type":"post","content":post.to_string()})),
            )
            .await;
        let (sent,post)=match sent{Ok(sent)if sent["data"]["message_id"].as_str().is_some()=>(Ok(sent),true),_ =>(owner.rest("POST","/im/v1/messages?receive_id_type=chat_id",Some(json!({"receive_id":chat,"msg_type":"text","content":json!({"text":text}).to_string()}))).await,false)};
        if let Ok(sent) = sent {
            self.handle = sent["data"]["message_id"]
                .as_str()
                .map(|id| (id.into(), post));
            self.visible = crate::metadata::js_trim(&self.text).into();
            self.last = Some(Instant::now());
        }
    }
}
