//! Channel component: registration/config loading, lifecycle, the A7 inbound
//! port and concrete channel adapters (A8.3 `api-server` and `webhook`).
pub mod api_server;
pub mod im_cron;
pub mod loader;
pub mod protocol;
pub mod runtime_status;
pub mod server;
pub mod sms;
#[cfg(test)]
mod tests;
pub mod turn_submitter;
pub mod webhook;
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
}

impl<K: Eq + Hash> ActiveChat<K> {
    /// Only construct this after the id has been inserted into `active`;
    /// `Drop` would otherwise release a slot another turn of the same chat
    /// legitimately holds (non-dispatch paths never admit).
    pub(crate) fn new(active: Arc<Mutex<HashSet<K>>>, chat_id: K) -> Self {
        Self { active, chat_id }
    }
}

impl<K: Eq + Hash> Drop for ActiveChat<K> {
    fn drop(&mut self) {
        // A poisoned lock still owns a valid set; recover it instead of
        // panicking inside the unwind — a `Drop` panic aborts the process.
        self.active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.chat_id);
    }
}
