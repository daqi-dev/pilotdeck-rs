//! Port of TS `src/always-on/runtime/ChannelLeaseRegistry.ts` — the
//! in-memory, process-bound channel lease registry. Adapters mutate it on
//! connect/disconnect/submit/complete; the discovery scheduler reads it via
//! `list_fresh`.
use std::sync::{Arc, Mutex};

use pilotdeck_transport::system_clock::parse_node22_date;

/// `AlwaysOnChannelLease` (`src/always-on/protocol/types.ts`) with the
/// `schemaVersion`/`writtenAt` the registry itself owns.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ChannelLease {
    pub schema_version: f64,
    pub written_at: String,
    pub channel_key: String,
    pub writer_id: String,
    pub project_key: String,
    pub session_key: String,
    pub agent_busy: bool,
    /// `null` in TS, carried as `None` here.
    pub last_user_msg_at: Option<String>,
}

/// `LeaseUpdateInput` (ChannelLeaseRegistry.ts:5-7).
#[derive(Clone, Debug)]
pub(crate) struct LeaseUpdate {
    pub channel_key: String,
    pub writer_id: String,
    pub project_key: String,
    pub session_key: String,
    pub agent_busy: bool,
    pub last_user_msg_at: Option<String>,
    pub written_at: Option<String>,
}

/// The registry (ChannelLeaseRegistry.ts:17-99): a `Map` keyed by
/// `projectKey \u0001 channelKey \u0001 writerId`, in insertion order. The
/// TS `Map` semantics the gate chain's lease pick depends on: `set` on a
/// live key updates in place (keeping its position), delete + re-insert
/// appends at the end, and `list`/`listFresh` iterate that order. A
/// `HashMap` would lose all three — `storage/task_budget.rs:118` keeps the
/// same `Vec` shape for the same reason.
pub(crate) struct ChannelLeaseRegistry {
    leases: Mutex<Vec<(String, ChannelLease)>>,
    now: Arc<dyn Fn() -> f64 + Send + Sync>,
}

impl ChannelLeaseRegistry {
    pub(crate) fn new(now: Arc<dyn Fn() -> f64 + Send + Sync>) -> Self {
        Self {
            leases: Mutex::new(Vec::new()),
            now,
        }
    }

    fn key(project_key: &str, channel_key: &str, writer_id: &str) -> String {
        format!("{project_key}\u{1}{channel_key}\u{1}{writer_id}")
    }

    fn iso(&self) -> String {
        crate::runtime::cron::iso_from_epoch_ms((self.now)())
    }

    /// `set` (ChannelLeaseRegistry.ts:27-40).
    pub(crate) fn set(&self, input: LeaseUpdate) -> ChannelLease {
        let lease = ChannelLease {
            schema_version: 1.0,
            written_at: input.written_at.unwrap_or_else(|| self.iso()),
            channel_key: input.channel_key,
            writer_id: input.writer_id,
            project_key: input.project_key,
            session_key: input.session_key,
            agent_busy: input.agent_busy,
            last_user_msg_at: input.last_user_msg_at,
        };
        let key = Self::key(&lease.project_key, &lease.channel_key, &lease.writer_id);
        let mut leases = self
            .leases
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        match leases.iter_mut().find(|(existing, _)| existing == &key) {
            Some(slot) => slot.1 = lease.clone(),
            None => leases.push((key, lease.clone())),
        }
        lease
    }

    /// `markBusy` (ChannelLeaseRegistry.ts:42-47): a missing lease is a no-op.
    pub(crate) fn mark_busy(&self, project_key: &str, channel_key: &str, writer_id: &str) {
        let key = Self::key(project_key, channel_key, writer_id);
        if let Some((_, lease)) = self
            .leases
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter_mut()
            .find(|(existing, _)| existing == &key)
        {
            lease.agent_busy = true;
            lease.written_at = self.iso();
        }
    }

    /// `markIdle` (ChannelLeaseRegistry.ts:49-56): idling records the user
    /// message time, which is what the `recent_user_msg` gate reads.
    pub(crate) fn mark_idle(&self, project_key: &str, channel_key: &str, writer_id: &str) {
        let key = Self::key(project_key, channel_key, writer_id);
        if let Some((_, lease)) = self
            .leases
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter_mut()
            .find(|(existing, _)| existing == &key)
        {
            lease.agent_busy = false;
            lease.last_user_msg_at = Some(self.iso());
            lease.written_at = self.iso();
        }
    }

    /// `remove` (ChannelLeaseRegistry.ts:58-60).
    pub(crate) fn remove(&self, project_key: &str, channel_key: &str, writer_id: &str) {
        let key = Self::key(project_key, channel_key, writer_id);
        let mut leases = self
            .leases
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(position) = leases.iter().position(|(existing, _)| existing == &key) {
            leases.remove(position);
        }
    }

    /// `removeByWriter` (ChannelLeaseRegistry.ts:62-67).
    pub(crate) fn remove_by_writer(&self, writer_id: &str) {
        self.leases
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .retain(|(_, lease)| lease.writer_id != writer_id);
    }

    /// `listFresh` (ChannelLeaseRegistry.ts:69-80): leases for one project
    /// whose `writtenAt` parses at or after `now - staleSeconds` — an
    /// unparseable timestamp reads as `NaN`, which never passes the `>=`.
    pub(crate) fn list_fresh(
        &self,
        project_key: &str,
        stale_seconds: f64,
        now: Option<f64>,
    ) -> Vec<ChannelLease> {
        let cutoff = now.unwrap_or_else(|| (self.now)()) - stale_seconds * 1000.0;
        self.leases
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .filter(|(_, lease)| {
                lease.project_key == project_key
                    && parse_node22_date(&pilotdeck_protocol::JsString::from(
                        lease.written_at.as_str(),
                    ))
                    .ok()
                    .flatten()
                    .is_some_and(|written| written >= cutoff)
            })
            .map(|(_, lease)| lease.clone())
            .collect()
    }

    /// `list` (ChannelLeaseRegistry.ts:82-84).
    pub(crate) fn list(&self) -> Vec<ChannelLease> {
        self.leases
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .map(|(_, lease)| lease.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW_MS: f64 = 1_767_225_900_000.0;

    fn registry() -> ChannelLeaseRegistry {
        ChannelLeaseRegistry::new(Arc::new(|| NOW_MS))
    }

    fn update(project: &str, channel: &str, writer: &str) -> LeaseUpdate {
        LeaseUpdate {
            channel_key: channel.into(),
            writer_id: writer.into(),
            project_key: project.into(),
            session_key: "s1".into(),
            agent_busy: false,
            last_user_msg_at: None,
            written_at: Some("2026-01-01T00:04:30.000Z".into()),
        }
    }

    // `set` writes the registry-owned fields (ChannelLeaseRegistry.ts:27-40):
    // schemaVersion 1, the injected `writtenAt`, `lastUserMsgAt` null.
    #[test]
    fn set_owns_the_registry_fields() {
        let registry = registry();
        let lease = registry.set(update("/proj/a", "web", "w1"));
        negative_assert_eq!(lease.schema_version, 1.0, "#2294 schemaVersion");
        negative_assert_eq!(
            lease.written_at,
            "2026-01-01T00:04:30.000Z",
            "#2294 writtenAt"
        );
        negative_assert_eq!(lease.last_user_msg_at, None, "#2294 lastUserMsgAt null");
        let defaulted = registry.set(LeaseUpdate {
            written_at: None,
            ..update("/proj/a", "web", "w2")
        });
        negative_assert_eq!(
            defaulted.written_at,
            "2026-01-01T00:05:00.000Z",
            "#2294 writtenAt defaults to the injected clock"
        );
    }

    // The same (project, channel, writer) triple overwrites one slot; a
    // different writer or channel is a different lease.
    #[test]
    fn keys_are_project_channel_writer_triples() {
        let registry = registry();
        registry.set(update("/proj/a", "web", "w1"));
        registry.set(LeaseUpdate {
            agent_busy: true,
            ..update("/proj/a", "web", "w1")
        });
        negative_assert_eq!(registry.list().len(), 1, "#2294 same triple overwrites");
        registry.set(update("/proj/a", "web", "w2"));
        registry.set(update("/proj/a", "cli", "w1"));
        registry.set(update("/proj/b", "web", "w1"));
        negative_assert_eq!(registry.list().len(), 4, "#2294 distinct triples");
        negative_assert_eq!(
            registry.list_fresh("/proj/a", 90.0, Some(NOW_MS)).len(),
            3,
            "#2294 listFresh scopes to the project"
        );
    }

    // markBusy/markIdle (ChannelLeaseRegistry.ts:42-56): a missing lease is a
    // no-op; markIdle records the user-message time.
    #[test]
    fn mark_busy_and_idle() {
        let registry = registry();
        registry.mark_busy("/proj/a", "web", "ghost");
        negative_assert_eq!(
            registry.list().len(),
            0,
            "#2294 markBusy on a missing lease"
        );
        registry.set(update("/proj/a", "web", "w1"));
        registry.mark_busy("/proj/a", "web", "w1");
        let lease = &registry.list()[0];
        negative_assert_eq!(lease.agent_busy, true, "#2294 markBusy sets agentBusy");
        negative_assert_eq!(
            lease.written_at,
            "2026-01-01T00:05:00.000Z",
            "#2294 markBusy refreshes writtenAt"
        );
        registry.mark_idle("/proj/a", "web", "w1");
        let lease = &registry.list()[0];
        negative_assert_eq!(lease.agent_busy, false, "#2294 markIdle clears agentBusy");
        negative_assert_eq!(
            lease.last_user_msg_at.as_deref(),
            Some("2026-01-01T00:05:00.000Z"),
            "#2294 markIdle records lastUserMsgAt"
        );
    }

    // `listFresh` (ChannelLeaseRegistry.ts:69-80): the cutoff is inclusive
    // (`>=`), staleness is `heartbeatStaleSeconds`, and the default clock is
    // the injected `now`.
    #[test]
    fn list_fresh_applies_the_stale_cutoff() {
        let registry = registry();
        // now - 90s exactly: fresh (>=).
        registry.set(LeaseUpdate {
            written_at: Some("2026-01-01T00:03:30.000Z".into()),
            ..update("/proj/a", "web", "boundary")
        });
        // One second past the window: stale.
        registry.set(LeaseUpdate {
            written_at: Some("2026-01-01T00:03:29.000Z".into()),
            ..update("/proj/a", "cli", "stale")
        });
        // An unparseable writtenAt reads as NaN and never passes the >=.
        registry.set(LeaseUpdate {
            written_at: Some("not a date".into()),
            ..update("/proj/a", "web", "invalid")
        });
        let fresh = registry.list_fresh("/proj/a", 90.0, Some(NOW_MS));
        negative_assert_eq!(
            fresh
                .iter()
                .map(|lease| lease.writer_id.as_str())
                .collect::<Vec<_>>(),
            vec!["boundary"],
            "#2294 listFresh keeps only the fresh, parseable leases"
        );
        // The registry clock is the default `now` (the injected fixed one).
        let defaulted = registry.list_fresh("/proj/a", 90.0, None);
        negative_assert_eq!(
            defaulted.len(),
            1,
            "#2294 listFresh defaults to the registry clock"
        );
    }

    // `list` keeps the TS `Map`'s insertion order (ChannelLeaseRegistry.ts:
    // 17-99): `set` on a live key updates in place, delete + re-insert
    // appends at the end. The gate chain's lease pick reads that order, so a
    // hash-ordering registry would pick a random lease.
    #[test]
    fn list_keeps_insertion_order() {
        let registry = registry();
        registry.set(update("/proj/a", "web", "w1"));
        registry.set(update("/proj/a", "cli", "w2"));
        registry.set(update("/proj/a", "api", "w3"));
        // A live key keeps its position on re-set.
        registry.set(LeaseUpdate {
            agent_busy: true,
            ..update("/proj/a", "cli", "w2")
        });
        // Delete + re-insert appends at the end.
        registry.remove("/proj/a", "web", "w1");
        registry.set(update("/proj/a", "web", "w1"));
        negative_assert_eq!(
            registry
                .list()
                .iter()
                .map(|lease| lease.writer_id.as_str())
                .collect::<Vec<_>>(),
            vec!["w2", "w3", "w1"],
            "#2294 list keeps JS Map insertion order"
        );
    }

    // `remove`/`removeByWriter` (ChannelLeaseRegistry.ts:58-67).
    #[test]
    fn remove_drops_leases() {
        let registry = registry();
        registry.set(update("/proj/a", "web", "w1"));
        registry.set(update("/proj/a", "cli", "w2"));
        registry.set(update("/proj/b", "web", "w2"));
        registry.remove("/proj/a", "web", "w1");
        negative_assert_eq!(registry.list().len(), 2, "#2294 remove drops one lease");
        registry.remove_by_writer("w2");
        negative_assert_eq!(
            registry.list().len(),
            0,
            "#2294 removeByWriter drops the rest"
        );
    }

    use crate::negative_assert_eq;
}
