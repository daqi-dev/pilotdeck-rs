//! ImChatSessionState.ts — per-chat generation, bounded pending-turn queue, active run.
//!
//! TypeScript baseline: src/adapters/channel/protocol/ImChatSessionState.ts
//! (cc7c4e65), constructed by WeixinChannel.ts:135 with maxPendingTurns: 20 and
//! driven by dispatchMessage / queuePendingTurn / runQueuedTurns
//! (WeixinChannel.ts:403-544).
use super::*;
use std::collections::{HashMap, HashSet, VecDeque};

/// WeixinChannel.ts:45 — WEIXIN_MAX_PENDING_TURNS_PER_CHAT.
pub(crate) const WEIXIN_MAX_PENDING_TURNS_PER_CHAT: usize = 20;

/// ImChatSessionState.ts:1-6 — queued inbound turn.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct QueuedTurn {
    pub session_key: String,
    pub message: String,
    pub project_key: Option<String>,
    pub attachments: Vec<Value>,
    /// PilotDeck-inbox plumbing: the durable receipt this turn stages its reply
    /// into. ImChatSessionState has no analogue; the queue itself is the TS one.
    pub receipt_path: Option<std::path::PathBuf>,
}

/// ImChatSessionState.ts:8-12 — {sessionKey, runId?, generation}.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ActiveRun {
    pub session_key: String,
    pub run_id: Option<String>,
    pub generation: u64,
}

/// ImChatSessionState.ts:14-68.
#[derive(Debug)]
pub(crate) struct ChatSessionState {
    max_pending_turns: usize,
    pending: HashMap<String, VecDeque<QueuedTurn>>,
    generations: HashMap<String, u64>,
    active_runs: HashMap<String, ActiveRun>,
    /// WeixinChannel.ts:125 activeChats — one runQueuedTurns loop per chat.
    running: HashSet<String>,
}

impl Default for ChatSessionState {
    fn default() -> Self {
        Self::new(WEIXIN_MAX_PENDING_TURNS_PER_CHAT)
    }
}

impl ChatSessionState {
    /// ImChatSessionState.ts:20-22.
    pub(crate) fn new(max_pending_turns: usize) -> Self {
        Self {
            max_pending_turns,
            pending: HashMap::new(),
            generations: HashMap::new(),
            active_runs: HashMap::new(),
            running: HashSet::new(),
        }
    }

    /// ImChatSessionState.ts:24-26.
    pub(crate) fn generation(&self, chat: &str) -> u64 {
        self.generations.get(chat).copied().unwrap_or(0)
    }

    /// ImChatSessionState.ts:28-30.
    pub(crate) fn is_current(&self, chat: &str, generation: u64) -> bool {
        self.generation(chat) == generation
    }

    /// WeixinChannel.ts:524 — `resetChatInteractionState` drops pendingTurns for the chat.
    /// Returns the dropped turns so the owner can finish their receipts.
    pub(crate) fn clear_pending_turns(&mut self, chat: &str) -> Vec<QueuedTurn> {
        self.active_runs.remove(chat);
        self.pending
            .remove(chat)
            .map(|queue| queue.into_iter().collect())
            .unwrap_or_default()
    }

    /// ImChatSessionState.ts:32-36 — /new bumps the generation and drops the queued
    /// turns and the active run (WeixinChannel.ts:524-529).
    pub(crate) fn reset_for_new_session(&mut self, chat: &str) {
        let next = self.generation(chat) + 1;
        self.generations.insert(chat.into(), next);
        let _ = self.clear_pending_turns(chat);
    }

    /// ImChatSessionState.ts:38-45 — FIFO with the oldest turn dropped past the cap.
    /// Returns the dropped turns so the owner can finish their receipts.
    pub(crate) fn queue_turn(&mut self, chat: &str, turn: QueuedTurn) -> Vec<QueuedTurn> {
        if self.max_pending_turns == 0 {
            return vec![turn];
        }
        let pending = self.pending.entry(chat.into()).or_default();
        pending.push_back(turn);
        let mut dropped = Vec::new();
        while pending.len() > self.max_pending_turns {
            if let Some(turn) = pending.pop_front() {
                dropped.push(turn);
            }
        }
        dropped
    }

    pub(crate) fn pending_len(&self, chat: &str) -> usize {
        self.pending.get(chat).map(VecDeque::len).unwrap_or(0)
    }

    pub(crate) fn pending_turns(&self, chat: &str) -> Vec<QueuedTurn> {
        self.pending
            .get(chat)
            .map(|pending| pending.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// ImChatSessionState.ts:47-54.
    pub(crate) fn shift_turn(&mut self, chat: &str) -> Option<QueuedTurn> {
        let pending = self.pending.get_mut(chat)?;
        let next = pending.pop_front();
        if pending.is_empty() {
            self.pending.remove(chat);
        }
        next
    }

    /// ImChatSessionState.ts:56-58.
    pub(crate) fn set_active_run(&mut self, chat: &str, run: ActiveRun) {
        self.active_runs.insert(chat.into(), run);
    }

    /// ImChatSessionState.ts:60-62.
    pub(crate) fn active_run(&self, chat: &str) -> Option<ActiveRun> {
        self.active_runs.get(chat).cloned()
    }

    /// ImChatSessionState.ts:64-66 — clears only the active run.
    pub(crate) fn clear_active_run(&mut self, chat: &str) {
        self.active_runs.remove(chat);
    }

    /// WeixinChannel.ts:531-546 — claim the per-chat run loop.
    pub(crate) fn enter_run(&mut self, chat: &str) -> bool {
        self.running.insert(chat.into())
    }

    /// WeixinChannel.ts:544-546 — the finally block leaves the active-chat set.
    pub(crate) fn leave_run(&mut self, chat: &str) {
        self.running.remove(chat);
    }

    pub(crate) fn is_running(&self, chat: &str) -> bool {
        self.running.contains(chat)
    }
}

#[cfg(test)]
mod session_tests {
    use super::*;

    fn turn(message: &str) -> QueuedTurn {
        QueuedTurn {
            session_key: "session".into(),
            message: message.into(),
            project_key: None,
            attachments: vec![],
            receipt_path: None,
        }
    }

    // ImChatSessionState.ts:38-45 — capped FIFO keeps the newest 20 turns.
    #[test]
    fn queue_turn_drops_oldest_when_over_max_pending_turns() {
        let mut state = ChatSessionState::new(20);
        for index in 0..21 {
            state.queue_turn("chat", turn(&format!("m{index}")));
        }
        assert_eq!(state.pending_len("chat"), 20);
        assert_eq!(state.shift_turn("chat").unwrap().message, "m1");
        let kept: Vec<_> = state
            .pending_turns("chat")
            .into_iter()
            .map(|turn| turn.message)
            .collect();
        assert_eq!(kept.first().unwrap(), "m2");
        assert_eq!(kept.last().unwrap(), "m20");
    }

    // ImChatSessionState.ts:47-54 — shift returns FIFO order and empties the map.
    #[test]
    fn shift_turn_is_fifo_and_clears_an_empty_queue() {
        let mut state = ChatSessionState::new(20);
        state.queue_turn("chat", turn("first"));
        state.queue_turn("chat", turn("second"));
        assert_eq!(state.shift_turn("chat").unwrap().message, "first");
        assert_eq!(state.pending_len("chat"), 1);
        assert_eq!(state.shift_turn("chat").unwrap().message, "second");
        assert_eq!(state.pending_len("chat"), 0);
        assert!(state.shift_turn("chat").is_none());
    }

    // ImChatSessionState.ts:32-36 + WeixinChannel.ts:524-529 — /new drops the queue and run.
    #[test]
    fn reset_for_new_session_drops_pending_turns_active_run_and_bumps_generation() {
        let mut state = ChatSessionState::new(20);
        let generation = state.generation("chat");
        state.queue_turn("chat", turn("queued"));
        state.set_active_run(
            "chat",
            ActiveRun {
                session_key: "session".into(),
                run_id: Some("run-1".into()),
                generation,
            },
        );
        state.reset_for_new_session("chat");
        assert_eq!(state.generation("chat"), generation + 1);
        assert!(!state.is_current("chat", generation));
        assert_eq!(state.pending_len("chat"), 0);
        assert!(state.active_run("chat").is_none());
    }

    // ImChatSessionState.ts:56-66 — the run carries sessionKey, runId and generation.
    #[test]
    fn active_run_round_trips_the_run_id_and_generation() {
        let mut state = ChatSessionState::new(20);
        state.set_active_run(
            "chat",
            ActiveRun {
                session_key: "session-a".into(),
                run_id: Some("run-7".into()),
                generation: 3,
            },
        );
        assert_eq!(
            state.active_run("chat"),
            Some(ActiveRun {
                session_key: "session-a".into(),
                run_id: Some("run-7".into()),
                generation: 3,
            })
        );
        state.clear_active_run("chat");
        assert!(state.active_run("chat").is_none());
        // clearActiveRun does not touch the generation fence.
        assert_eq!(state.generation("chat"), 0);
    }

    // WeixinChannel.ts:531-546 — only one runQueuedTurns loop per chat.
    #[test]
    fn enter_run_claims_the_chat_until_leave_run() {
        let mut state = ChatSessionState::new(20);
        assert!(state.enter_run("chat"));
        assert!(!state.enter_run("chat"));
        assert!(state.is_running("chat"));
        state.leave_run("chat");
        assert!(!state.is_running("chat"));
        assert!(state.enter_run("chat"));
    }

    // ImChatSessionState.ts:15-22 — WeixinChannel.ts:135 passes 20.
    #[test]
    fn weixin_construction_uses_twenty_pending_turns() {
        assert_eq!(WEIXIN_MAX_PENDING_TURNS_PER_CHAT, 20);
        assert_eq!(
            ChatSessionState::default().max_pending_turns,
            WEIXIN_MAX_PENDING_TURNS_PER_CHAT
        );
    }
}




