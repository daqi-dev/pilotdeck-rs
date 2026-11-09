//! #1370 controlled storage stimulus apparatus (collection only).
//!
//! The #1368 stimulus plan declares two durable-write boundaries that a
//! collector must be able to hold, fault and witness in order:
//!
//! - the real writer's underlying append (`eventStore.append` inside
//!   `writer.writeTail`, JsonlTranscriptWriter.ts:298-308 — the Rust commit
//!   lock held around `JsonlTranscriptStore::append`), and
//! - the run registry persist that runs inside the mutation chain and the
//!   cross-process file lock, after the in-memory mutation and before the
//!   snapshot is serialized (RunRegistry.ts:184-209).
//!
//! Both hooks live in the production paths but stay inert unless the
//! process-wide controlled entry is armed: `PILOTDECK_NATIVE_STORAGE_STIMULUS`
//! names an existing control directory (the native assembly validates it at
//! startup and publishes a `native storage stimulus on` receipt, #1369
//! pattern). Unset keeps the production default — no file reads, no receipts.
//!
//! The entry is captured per store instance at construction and never
//! re-resolved or swapped at write time: only writes through a store wired to
//! an armed stimulus can ever observe it, so a stimulus armed for one
//! component cannot fail another's writes in the same process.
//!
//! Control protocol (collector writes atomically via tmp + rename; the product
//! only reads control files and only writes receipts):
//!
//! - `<dir>/stimulus.json`: `{"writer":{"barrier":{"id","key","boundMs"?},
//!   "fault":{"id","key"}},"registry":{...}}`. Every arm is single-shot per
//!   `id`: the same id never fires twice, a rewritten config with a new id
//!   re-arms. A missing file arms nothing; a malformed one fails the operation
//!   closed.
//! - `<dir>/release.json`: `{"seq": <block receipt seq>}` — releases exactly
//!   the barrier whose block receipt carries that sequence, so a writer and a
//!   registry barrier held at the same time never release each other.
//! - `<dir>/witness/<seq>.json`: product receipts, published atomically
//!   (tmp + rename). Phases: `block` (a matched write entered the barrier),
//!   `queued` (an admitted write is pending behind a held barrier — the FIFO
//!   witness), `release` (with `bounded: true` when the bound expired instead
//!   of a collector release), `fault` (with `code: "EIO"`) and `complete`
//!   (the blocked write's real storage finished).
//!
//! The barrier is bounded: without a release it self-releases after `boundMs`
//! (default 30 s, cap 120 s), so a lost collector can never wedge the
//! gateway forever. The fault is a single EIO: a real storage error kind
//! (`Errno::IO`, raw os error 5) raised only at these two storage sites —
//! provider latency or HTTP 500 never enters this apparatus, and a faulted
//! registry append surfaces through the existing `RunRegistryError::Io` →
//! `result_unknown` publication path, not a provider error family.
use super::{ledger::field, transcript::timestamp};
use crate::metadata::object;
use pilotdeck_protocol::{JsObject, JsValue, parse_json, stringify_json};
use std::{
    fs, io,
    path::PathBuf,
    sync::{Arc, LazyLock, Mutex},
    thread,
    time::{Duration, Instant},
};

pub const STIMULUS_ENV: &str = "PILOTDECK_NATIVE_STORAGE_STIMULUS";
const DEFAULT_BOUND: Duration = Duration::from_secs(30);
const MAX_BOUND: Duration = Duration::from_secs(120);
const RELEASE_POLL: Duration = Duration::from_millis(10);

/// The two durable-write sites of #1370.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Site {
    Writer,
    Registry,
}
impl Site {
    fn as_str(self) -> &'static str {
        match self {
            Self::Writer => "writer",
            Self::Registry => "registry",
        }
    }
}

/// Whether a stimulated storage operation passed or was held at the barrier.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Applied {
    Proceed,
    Blocked,
}

#[derive(Debug, Default)]
struct SiteState {
    /// Block receipt sequence of the currently held barrier.
    held: Option<u64>,
    held_published: bool,
    held_scope: Option<RegistryAppendScope>,
    barrier_consumed: Option<String>,
    fault_fired: Option<String>,
}
struct Inner {
    next_ticket: u64,
    registry_pending: std::collections::BTreeMap<u64, std::sync::Weak<AppendTicket>>,
    next_seq: u64,
    writer: SiteState,
    registry: SiteState,
}

/// One controlled stimulus arm as parsed from `stimulus.json`.
#[derive(Debug, PartialEq, Eq)]
struct Arm {
    id: String,
    key: String,
    bound: Duration,
}
#[derive(Debug, Default, PartialEq, Eq)]
struct SiteConfig {
    barrier: Option<Arm>,
    fault: Option<Arm>,
}
#[derive(Debug, Default, PartialEq, Eq)]
struct Config {
    writer: SiteConfig,
    registry: SiteConfig,
}

pub struct StorageStimulus {
    dir: PathBuf,
    inner: Mutex<Inner>,
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::other(format!("native storage stimulus: {}", message.into()))
}

/// The single injected storage error: EIO and only EIO, so a faulted write can
/// never be confused with a provider failure (HTTP 500, timeouts).
fn storage_eio() -> io::Error {
    io::Error::from_raw_os_error(rustix::io::Errno::IO.raw_os_error())
}

impl StorageStimulus {
    pub fn control(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            inner: Mutex::new(Inner {
                next_seq: 0,
                next_ticket: 0,
                registry_pending: std::collections::BTreeMap::new(),
                writer: SiteState::default(),
                registry: SiteState::default(),
            }),
        }
    }

    /// The process-wide controlled entry. Unset (or not unicode) keeps the
    /// production default. The entry resolves once and is immutable for the
    /// process lifetime, so a set value stays armed for every store that
    /// captured it, failing closed when the directory disappears.
    pub fn global() -> Option<Arc<Self>> {
        GLOBAL.clone()
    }
    /// Same entry as a shared reference, for store implementations that do
    /// not capture a handle of their own (the writer-site entry witness).
    pub(crate) fn global_ref() -> Option<&'static Arc<Self>> {
        let resolved: &'static Option<Arc<Self>> = &GLOBAL;
        resolved.as_ref()
    }

    fn read_config(&self) -> io::Result<Config> {
        let bytes = match fs::read(self.dir.join("stimulus.json")) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Config::default()),
            Err(e) => return Err(invalid(format!("control directory unreadable: {e}"))),
        };
        let text = String::from_utf8_lossy(&bytes);
        let value = parse_json(&text)
            .map_err(|error| invalid(format!("stimulus.json is not valid JSON: {error}")))?;
        parse_config(&value)
    }

    /// Entry observation (`writer.append` / `runRegistry.append` invoked):
    /// publish a `queued` receipt when a barrier is currently held at this
    /// site, so the collector can witness what FIFO-queued behind the block.
    pub(crate) fn queued(&self, site: Site, key: &str) -> io::Result<()> {
        match self.reserve_queued(site) {
            None => Ok(()),
            Some(seq) => self.write_queued(seq, site, key),
        }
    }

    /// Decide, on the caller's tick, whether an entry is owed a `queued`
    /// receipt, and reserve that receipt's sequence. The decision and the
    /// sequence have to be taken together with the barrier's own `held`
    /// transition: a later re-check would already see the released barrier and
    /// silently drop a receipt the collector is still waiting for.
    fn reserve_queued(&self, site: Site) -> Option<u64> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let held = site_state_ref(&inner, site).held.is_some();
        if !held {
            return None;
        }
        Some(next_sequence(&mut inner))
    }

    /// Write a `queued` receipt whose sequence was reserved at entry.
    fn write_queued(&self, seq: u64, site: Site, key: &str) -> io::Result<()> {
        self.publish(seq, site, key, "queued", None)
    }

    /// The underlying durable write: single EIO first, then the bounded
    /// barrier. `Err` fails the storage operation; `Ok(Applied::Blocked)`
    /// means the caller owes a `completed` receipt after the real write.
    pub(crate) fn before_write(&self, site: Site, key: &str) -> io::Result<Applied> {
        if !self.dir.is_dir() {
            return Err(invalid(format!(
                "control directory is missing: {}",
                self.dir.display()
            )));
        }
        let config = self.read_config()?;
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let site_config = match site {
            Site::Writer => &config.writer,
            Site::Registry => &config.registry,
        };
        if let Some(fault) = &site_config.fault
            && fault.key == key
            && site_state_ref(&inner, site).fault_fired.as_deref() != Some(fault.id.as_str())
        {
            site_state(&mut inner, site).fault_fired = Some(fault.id.clone());
            let seq = next_sequence(&mut inner);
            drop(inner);
            self.publish(
                seq,
                site,
                key,
                "fault",
                Some(&object([("code", "EIO".into())])),
            )?;
            return Err(storage_eio());
        }
        if let Some(barrier) = &site_config.barrier
            && barrier.key == key
            && site_state_ref(&inner, site).barrier_consumed.as_deref() != Some(barrier.id.as_str())
        {
            site_state(&mut inner, site).barrier_consumed = Some(barrier.id.clone());
            let bound = barrier.bound;
            let seq = next_sequence(&mut inner);
            site_state(&mut inner, site).held = Some(seq);
            site_state(&mut inner, site).held_scope = if site == Site::Registry {
                REGISTRY_APPEND_SCOPE.with(|scope| scope.borrow().clone())
            } else {
                None
            };
            site_state(&mut inner, site).held_published = false;
            drop(inner);
            let mut receipt = object([("boundMs", JsValue::Number(bound.as_millis() as f64))]);
            if let Some(scope) = REGISTRY_APPEND_SCOPE.with(|scope| scope.borrow().clone()) {
                scope.add_fields(&mut receipt);
            }
            let outcome = self
                .publish(seq, site, key, "block", Some(&receipt))
                .and_then(|()| {
                    {
                        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                        site_state(&mut inner, site).held_published = true;
                    }
                    if site == Site::Registry {
                        self.publish_pending_registry();
                    }
                    self.wait_for_release(seq, bound)
                });
            let released = match outcome {
                Ok(released) => released,
                Err(error) => {
                    // Fail closed without leaving the site wedged: a hold left
                    // behind would stamp every later entry with a receipt.
                    self.clear_hold(site);
                    return Err(error);
                }
            };
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            site_state(&mut inner, site).held = None;
            site_state(&mut inner, site).held_scope = None;
            site_state(&mut inner, site).held_published = false;
            let release_seq = next_sequence(&mut inner);
            drop(inner);
            let bounded = (!released).then(|| object([("bounded", JsValue::Bool(true))]));
            self.publish(release_seq, site, key, "release", bounded.as_ref())?;
            return Ok(Applied::Blocked);
        }
        Ok(Applied::Proceed)
    }

    /// Post-commit receipt for the write that was held at the barrier.
    /// Evidence only: the durable write already succeeded, so a receipt
    /// failure must not turn a successful append into a reported failure.
    pub(crate) fn completed(&self, site: Site, key: &str) {
        let seq = {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if site_state_ref(&inner, site).barrier_consumed.is_none() {
                return;
            }
            next_sequence(&mut inner)
        };
        let _ = self.publish(seq, site, key, "complete", None);
    }

    fn clear_hold(&self, site: Site) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        site_state(&mut inner, site).held = None;
        site_state(&mut inner, site).held_scope = None;
        site_state(&mut inner, site).held_published = false;
    }

    fn wait_for_release(&self, block_seq: u64, bound: Duration) -> io::Result<bool> {
        let deadline = Instant::now() + bound;
        loop {
            match fs::read(self.dir.join("release.json")) {
                Ok(bytes) => {
                    if let Ok(value) = parse_json(&String::from_utf8_lossy(&bytes))
                        && field(&value, "seq").as_number() == Some(block_seq as f64)
                    {
                        return Ok(true);
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(invalid(format!("release.json unreadable: {e}"))),
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            thread::sleep(RELEASE_POLL);
        }
    }

    fn publish(
        &self,
        seq: u64,
        site: Site,
        key: &str,
        phase: &str,
        extra: Option<&JsValue>,
    ) -> io::Result<()> {
        let mut receipt = JsObject::new();
        receipt.insert("seq".into(), JsValue::Number(seq as f64));
        receipt.insert("site".into(), site.as_str().into());
        receipt.insert("key".into(), key.into());
        receipt.insert("phase".into(), phase.into());
        receipt.insert(
            "at".into(),
            timestamp().as_deref().unwrap_or_default().into(),
        );
        if let Some(JsValue::Object(extra)) = extra {
            for (name, value) in extra.entries() {
                receipt.insert(name.clone(), value.clone());
            }
        }
        let text = stringify_json(&JsValue::Object(receipt))
            .map_err(|error| invalid(format!("receipt is not JSON: {error}")))?
            .ok_or_else(|| invalid("receipt is not JSON."))?;
        let witness = self.dir.join("witness");
        fs::create_dir_all(&witness)?;
        let receipt_path = witness.join(format!("{seq:06}.json"));
        let mut temporary = receipt_path.clone().into_os_string();
        temporary.push(".tmp");
        let temporary = PathBuf::from(temporary);
        fs::write(&temporary, text.as_bytes())?;
        fs::rename(&temporary, &receipt_path).inspect_err(|_| {
            let _ = fs::remove_file(&temporary);
        })?;
        Ok(())
    }
}

fn next_sequence(inner: &mut Inner) -> u64 {
    inner.next_seq += 1;
    inner.next_seq
}
fn site_state_ref(inner: &Inner, site: Site) -> &SiteState {
    match site {
        Site::Writer => &inner.writer,
        Site::Registry => &inner.registry,
    }
}
fn site_state(inner: &mut Inner, site: Site) -> &mut SiteState {
    match site {
        Site::Writer => &mut inner.writer,
        Site::Registry => &mut inner.registry,
    }
}

fn non_empty_string(value: &JsValue) -> Option<String> {
    value
        .as_string()
        .filter(|text| !text.is_empty())
        .map(|text| String::from_utf16_lossy(text.units()))
}

fn parse_arm(value: &JsValue, with_bound: bool) -> Result<Option<Arm>, io::Error> {
    if matches!(value, JsValue::Undefined | JsValue::Null) {
        return Ok(None);
    }
    let JsValue::Object(_) = value else {
        return Err(invalid("stimulus arms must be objects."));
    };
    let Some(id) = non_empty_string(field(value, "id")) else {
        return Err(invalid("stimulus arms require a non-empty string id."));
    };
    let Some(key) = non_empty_string(field(value, "key")) else {
        return Err(invalid("stimulus arms require a non-empty string key."));
    };
    let mut bound = DEFAULT_BOUND;
    if with_bound {
        let configured = field(value, "boundMs");
        if !matches!(configured, JsValue::Undefined | JsValue::Null) {
            let Some(value) = configured.as_number().filter(|value| {
                value.is_finite() && *value > 0.0 && *value <= MAX_BOUND.as_millis() as f64
            }) else {
                return Err(invalid(
                    "stimulus barrier boundMs must be a positive number of at most 120000.",
                ));
            };
            bound = Duration::from_millis(value as u64);
        }
    }
    Ok(Some(Arm { id, key, bound }))
}
fn parse_site(value: &JsValue) -> Result<SiteConfig, io::Error> {
    if matches!(value, JsValue::Undefined | JsValue::Null) {
        return Ok(SiteConfig::default());
    }
    let JsValue::Object(_) = value else {
        return Err(invalid("stimulus sites must be objects."));
    };
    Ok(SiteConfig {
        barrier: parse_arm(field(value, "barrier"), true)?,
        fault: parse_arm(field(value, "fault"), false)?,
    })
}
fn parse_config(value: &JsValue) -> Result<Config, io::Error> {
    let JsValue::Object(_) = value else {
        return Err(invalid("stimulus.json must contain an object."));
    };
    Ok(Config {
        writer: parse_site(field(value, "writer"))?,
        registry: parse_site(field(value, "registry"))?,
    })
}

static GLOBAL: LazyLock<Option<Arc<StorageStimulus>>> =
    LazyLock::new(|| match std::env::var(STIMULUS_ENV) {
        Ok(dir) => Some(Arc::new(StorageStimulus::control(dir))),
        Err(_) => None,
    });

fn object_field<'a>(draft: &'a JsObject, key: &str) -> &'a JsValue {
    static UNDEFINED: JsValue = JsValue::Undefined;
    draft.get(key).unwrap_or(&UNDEFINED)
}

/// Writer-site key: a status entry is addressed by its `event` name
/// (`turn_timeout`, `gateway_submit_failed`); every other entry by its type.
fn writer_key(kind: &JsValue, event: &JsValue) -> String {
    if kind
        .as_string()
        .is_some_and(|t| t.equals_str("agent_status_message"))
        && let Some(event) = non_empty_string(event)
    {
        return event;
    }
    non_empty_string(kind).unwrap_or_default()
}
pub(crate) fn writer_entry_key(entry: &JsValue) -> String {
    writer_key(field(entry, "type"), field(entry, "event"))
}
pub(crate) fn writer_draft_key(draft: &JsObject) -> String {
    writer_key(object_field(draft, "type"), object_field(draft, "event"))
}

/// Registry-site key: `agent_status:<event>`, `error:<code>`, else the type —
/// the stimulus plan's `registry.persist.block` keys.
pub(crate) fn registry_event_key(event: &JsValue) -> String {
    let kind = field(event, "type");
    if kind
        .as_string()
        .is_some_and(|t| t.equals_str("agent_status"))
        && let Some(name) = non_empty_string(field(event, "event"))
    {
        return format!("agent_status:{name}");
    }
    if kind.as_string().is_some_and(|t| t.equals_str("error"))
        && let Some(code) = non_empty_string(field(event, "code"))
    {
        return format!("error:{code}");
    }
    non_empty_string(kind).unwrap_or_default()
}

/// Entry hook for the writer site (`writer.append` invoked): `queued` receipt
/// while a barrier is held, before the caller queues on the commit lock.
/// `stimulus` is the caller's store handle (`None` keeps the entry inert).
pub(crate) fn writer_stimulus_queued_draft(
    stimulus: Option<&Arc<StorageStimulus>>,
    draft: &JsObject,
) -> Result<(), super::LedgerError> {
    let Some(stimulus) = stimulus else {
        return Ok(());
    };
    stimulus
        .queued(Site::Writer, &writer_draft_key(draft))
        .map_err(super::LedgerError::Io)
}
/// Same witness for the `recordEntry` path, which arrives as a full entry.
pub(crate) fn writer_stimulus_queued_entry(
    stimulus: Option<&Arc<StorageStimulus>>,
    entry: &JsValue,
) -> Result<(), super::LedgerError> {
    let Some(stimulus) = stimulus else {
        return Ok(());
    };
    stimulus
        .queued(Site::Writer, &writer_entry_key(entry))
        .map_err(super::LedgerError::Io)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RegistryAppendScope {
    pub(crate) registry: uuid::Uuid,
    pub(crate) sequence: u64,
    pub(crate) run: super::RunRef,
}
impl RegistryAppendScope {
    fn follows(&self, held: &Self) -> bool {
        self.registry == held.registry && self.run == held.run && self.sequence > held.sequence
    }
    fn add_fields(&self, value: &mut JsValue) {
        let identity = object([
            (
                "registryInstance",
                JsValue::String(self.registry.to_string().into()),
            ),
            ("appendSequence", JsValue::Number(self.sequence as f64)),
            (
                "appendProjectKey",
                self.run
                    .project_key
                    .clone()
                    .map(JsValue::String)
                    .unwrap_or(JsValue::Null),
            ),
            (
                "appendSessionKey",
                JsValue::String(self.run.session_key.clone()),
            ),
            ("appendRunId", JsValue::String(self.run.run_id.clone())),
        ]);
        if let (JsValue::Object(target), JsValue::Object(fields)) = (value, &identity) {
            for (key, value) in fields.entries() {
                target.insert(key.clone(), value.clone());
            }
        }
    }
}
thread_local! { static REGISTRY_APPEND_SCOPE: std::cell::RefCell<Option<RegistryAppendScope>> = const { std::cell::RefCell::new(None) }; }
pub(crate) struct RegistryAppendScopeGuard {
    previous: Option<RegistryAppendScope>,
    _thread: std::marker::PhantomData<std::rc::Rc<()>>,
}
impl RegistryAppendScopeGuard {
    fn enter(scope: RegistryAppendScope) -> Self {
        let previous = REGISTRY_APPEND_SCOPE.with(|active| active.replace(Some(scope)));
        Self {
            previous,
            _thread: std::marker::PhantomData,
        }
    }
}
impl Drop for RegistryAppendScopeGuard {
    fn drop(&mut self) {
        REGISTRY_APPEND_SCOPE.with(|active| {
            active.replace(self.previous.take());
        });
    }
}

/// A real registry job admitted behind the currently armed barrier. The
/// admission handle is separate from the job guard: dropping an awaiter does
/// not cancel already accepted blocking I/O or fabricate its completion.
#[derive(Clone)]
pub(crate) struct RegistryAppendAdmission {
    stimulus: Arc<StorageStimulus>,
    ticket: Arc<AppendTicket>,
}
pub(crate) struct RegistryAppendJob {
    admission: RegistryAppendAdmission,
}
#[derive(Default)]
struct AppendTicketState {
    enqueued: bool,
    initialized: bool,
    initializing: bool,
    #[cfg(test)]
    initialization_flushed: bool,
    arm_id: Option<String>,
    initialization_error: Option<String>,
    started: bool,
    settled: bool,
    claimed: bool,
    receipt: Option<Result<(), String>>,
}
struct AppendTicket {
    id: u64,
    key: String,
    scope: RegistryAppendScope,
    state: Mutex<AppendTicketState>,
    changed: std::sync::Condvar,
    failure: tokio::sync::watch::Sender<bool>,
}

/// Registration alone proves nothing. Only `enqueued()` after a successful
/// worker handoff can owe a queued receipt; `start()` removes it before I/O.
pub(crate) fn registry_append_ticket(
    stimulus: Option<&Arc<StorageStimulus>>,
    event: &JsValue,
    scope: RegistryAppendScope,
) -> io::Result<Option<(RegistryAppendAdmission, RegistryAppendJob)>> {
    let Some(stimulus) = stimulus else {
        return Ok(None);
    };
    let mut inner = stimulus.inner.lock().unwrap_or_else(|e| e.into_inner());
    let id = inner.next_ticket;
    inner.next_ticket = inner
        .next_ticket
        .checked_add(1)
        .ok_or_else(|| invalid("registry ticket exhausted"))?;
    let ticket = Arc::new(AppendTicket {
        id,
        key: registry_event_key(event),
        scope,
        state: Mutex::new(AppendTicketState::default()),
        changed: std::sync::Condvar::new(),
        failure: tokio::sync::watch::channel(false).0,
    });
    inner.registry_pending.insert(id, Arc::downgrade(&ticket));
    drop(inner);
    let admission = RegistryAppendAdmission {
        stimulus: Arc::clone(stimulus),
        ticket,
    };
    Ok(Some((admission.clone(), RegistryAppendJob { admission })))
}

impl RegistryAppendAdmission {
    pub(crate) async fn failure(&self) -> io::Error {
        let mut changed = self.ticket.failure.subscribe();
        loop {
            if let Some(message) = self.failure_message() {
                return invalid(message);
            }
            if changed.changed().await.is_err() {
                return invalid("Registry witness failure channel stopped.");
            }
        }
    }
    fn failure_message(&self) -> Option<String> {
        let state = self.ticket.state.lock().unwrap_or_else(|e| e.into_inner());
        state.initialization_error.clone().or_else(|| {
            state
                .receipt
                .as_ref()
                .and_then(|value| value.as_ref().err().cloned())
        })
    }
    pub(crate) fn enqueued(&self) {
        {
            let _inner = self
                .stimulus
                .inner
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let mut state = self.ticket.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.settled || state.initialized || state.initializing {
                return;
            }
            state.initializing = true;
            state.enqueued = !state.started;
        }
        let admission = self.clone();
        let initialize = move || {
            let config = admission.stimulus.read_config();
            {
                let _inner = admission
                    .stimulus
                    .inner
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let mut state = admission
                    .ticket
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if state.settled {
                    return;
                }
                match config {
                    Ok(config) => state.arm_id = config.registry.barrier.map(|arm| arm.id),
                    Err(error) => state.initialization_error = Some(error.to_string()),
                }
                state.initialized = true;
                if state.initialization_error.is_some() {
                    admission.ticket.failure.send_replace(true);
                }
                admission.ticket.changed.notify_all();
            }
            admission.stimulus.publish_pending_registry();
            #[cfg(test)]
            {
                let mut state = admission
                    .ticket
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                state.initialization_flushed = true;
                admission.ticket.changed.notify_all();
            }
        };
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn_blocking(initialize);
        } else {
            initialize();
        }
    }
}
impl RegistryAppendJob {
    pub(crate) fn enter(&self) -> RegistryAppendScopeGuard {
        RegistryAppendScopeGuard::enter(self.admission.ticket.scope.clone())
    }
    pub(crate) fn start(&self, bound: Duration) -> io::Result<()> {
        {
            let mut inner = self
                .admission
                .stimulus
                .inner
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            inner.registry_pending.remove(&self.admission.ticket.id);
            let mut state = self
                .admission
                .ticket
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            state.started = true;
        }
        let ticket = &self.admission.ticket;
        let state = ticket.state.lock().unwrap_or_else(|e| e.into_inner());
        let (state, timed) = ticket
            .changed
            .wait_timeout_while(state, bound, |state| {
                !state.initialized || (state.claimed && state.receipt.is_none())
            })
            .unwrap_or_else(|e| e.into_inner());
        if timed.timed_out() && (!state.initialized || (state.claimed && state.receipt.is_none())) {
            return Err(invalid("Storage stimulus queued witness timed out."));
        }
        if let Some(error) = &state.initialization_error {
            return Err(invalid(error.clone()));
        }
        match &state.receipt {
            Some(Err(message)) => Err(invalid(message.clone())),
            _ => Ok(()),
        }
    }
}
impl Drop for RegistryAppendJob {
    fn drop(&mut self) {
        let mut inner = self
            .admission
            .stimulus
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        inner.registry_pending.remove(&self.admission.ticket.id);
        let mut state = self
            .admission
            .ticket
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        state.settled = true;
        self.admission.ticket.changed.notify_all();
    }
}

impl StorageStimulus {
    #[cfg(test)]
    pub(crate) fn pending_ready_for_test(&self) -> usize {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner
            .registry_pending
            .values()
            .filter_map(std::sync::Weak::upgrade)
            .filter(|ticket| {
                let state = ticket.state.lock().unwrap_or_else(|e| e.into_inner());
                state.enqueued && state.initialization_flushed && !state.started && !state.settled
            })
            .count()
    }
    fn publish_pending_registry(&self) {
        let pending = {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let Some(block_seq) = inner
                .registry
                .held
                .filter(|_| inner.registry.held_published)
            else {
                return;
            };
            let Some(held_scope) = inner.registry.held_scope.clone() else {
                return;
            };
            let Some(arm_id) = inner.registry.barrier_consumed.clone() else {
                return;
            };
            let tickets: Vec<_> = inner
                .registry_pending
                .values()
                .filter_map(std::sync::Weak::upgrade)
                .collect();
            let mut owed = Vec::new();
            for ticket in tickets {
                let mut state = ticket.state.lock().unwrap_or_else(|e| e.into_inner());
                if !ticket.scope.follows(&held_scope)
                    || state.arm_id.as_deref() != Some(arm_id.as_str())
                    || state.initialization_error.is_some()
                    || !state.initialized
                    || !state.enqueued
                    || state.started
                    || state.settled
                    || state.claimed
                {
                    continue;
                }
                state.claimed = true;
                let seq = next_sequence(&mut inner);
                owed.push((seq, block_seq, arm_id.clone(), Arc::clone(&ticket)));
            }
            owed
        };
        for (seq, block_seq, arm_id, ticket) in pending {
            let mut binding = object([
                ("barrierId", arm_id.as_str().into()),
                ("barrierSeq", JsValue::Number(block_seq as f64)),
                ("appendTicket", JsValue::Number(ticket.id as f64)),
            ]);
            ticket.scope.add_fields(&mut binding);
            let mut state = ticket.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.settled {
                state.receipt = Some(Err("Registry append settled before queued witness.".into()));
                ticket.changed.notify_all();
                ticket.failure.send_replace(true);
                continue;
            }
            let outcome = self
                .publish(seq, Site::Registry, &ticket.key, "queued", Some(&binding))
                .map_err(|error| error.to_string());
            state.receipt = Some(outcome);
            if state.receipt.as_ref().is_some_and(Result::is_err) {
                ticket.failure.send_replace(true);
            }
            ticket.changed.notify_all();
        }
    }
}

#[cfg(test)]
#[path = "stimulus_pending_tests.rs"]
mod pending_tests;
