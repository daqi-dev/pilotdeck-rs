//! Channel component: registration/config loading, lifecycle, the A7 inbound
//! port and concrete channel adapters (A8.3 `api-server` and `webhook`).
pub mod api_server;
pub mod bluebubbles;
pub mod feishu;
pub(crate) mod http_client;
pub(crate) mod im_attachment;
pub mod im_cron;
pub(crate) mod im_interaction;
pub mod loader;
pub mod protocol;
pub mod runtime_status;
pub mod server;
pub mod slack;
pub mod sms;
#[cfg(test)]
mod tests;
pub mod turn_submitter;
pub mod webhook;
pub mod wecom_callback;
pub mod weixin;

use std::{
    collections::HashSet,
    hash::Hash,
    sync::{Arc, Mutex},
};

/// The active-chat admission guard every inbound channel shares (#1738): it
/// owns the chat id's slot in the shared set and releases it in `Drop`, so a
/// panic anywhere between admission and the end of the turn still unparks the
/// chat — the TS channels' `finally { this.activeChats.delete(chatId) }`.
///
/// The set lock is a `std::sync::Mutex` held only across sections that never
/// await, so the synchronous `Drop` can always take it (a tokio `Mutex` would
/// force `.await`, which `Drop` cannot do).
pub(crate) struct ActiveChat<K: Eq + Hash> {
    active: Arc<Mutex<HashSet<K>>>,
    chat_id: K,
    /// Cleared by [`Self::release`]: a slot already given back must not be
    /// taken back again by `Drop`, which would remove an admission a *later*
    /// turn of the same chat legitimately holds.
    armed: bool,
}

impl<K: Eq + Hash> ActiveChat<K> {
    /// Only construct this after the id has been inserted into `active`;
    /// `Drop` would otherwise release a slot another turn of the same chat
    /// legitimately holds (non-dispatch paths never admit).
    pub(crate) fn new(active: Arc<Mutex<HashSet<K>>>, chat_id: K) -> Self {
        Self {
            active,
            chat_id,
            armed: true,
        }
    }

    /// Take the slot and hand back its guard in one step: `Some` only when
    /// *this* call inserted the id. `None` means another turn of the same chat
    /// already holds it — the caller must skip the turn and must not build a
    /// guard, or its drop would release the other turn's admission. This is
    /// the shape callers should use when the admit decision has to survive a
    /// check made in an earlier lock section (`webhook::handle_incoming`);
    /// channels that insert inside a larger lock section (sms) keep using
    /// [`Self::new`] there.
    pub(crate) fn admit(active: Arc<Mutex<HashSet<K>>>, chat_id: K) -> Option<Self>
    where
        K: Clone,
    {
        let fresh = {
            let mut set = active.lock().expect("#1738 active chats mutex");
            set.insert(chat_id.clone())
        };
        fresh.then(|| Self::new(active, chat_id))
    }

    /// Give the slot back now, so a later `Drop` cannot release an admission
    /// another turn took after this one. Callers that release while holding
    /// their own state lock get an atomic "queue empty and no longer active"
    /// hand-off: a turn queued by a losing racer under that same lock is
    /// either seen before the release or wakes a fresh admission.
    ///
    /// The contract every user of this shape must keep: the release shares
    /// the critical section with the "queue is empty" decision that ends the
    /// drainer (see `feishu::Runtime::take_next_or_release` and weixin's
    /// `ActiveChatSlot::release` — the same form over different lock homes:
    /// a standalone set mutex here, the single `Inner` mutex there).
    pub(crate) fn release(&mut self) {
        if self.armed {
            self.active
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&self.chat_id);
            self.armed = false;
        }
    }
}

impl<K: Eq + Hash> Drop for ActiveChat<K> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        // A poisoned lock still owns a valid set; recover it instead of
        // panicking inside the unwind — a `Drop` panic aborts the process.
        self.active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.chat_id);
    }
}
