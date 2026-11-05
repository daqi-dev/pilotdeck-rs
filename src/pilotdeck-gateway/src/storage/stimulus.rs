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
//!   `queued` (a write arrived at the site while a barrier was held — the FIFO
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
    barrier_consumed: Option<String>,
    fault_fired: Option<String>,
}
struct Inner {
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
        let seq = {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if site_state_ref(&inner, site).held.is_none() {
                return Ok(());
            }
            next_sequence(&mut inner)
        };
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
            drop(inner);
            let receipt = object([("boundMs", JsValue::Number(bound.as_millis() as f64))]);
            self.publish(seq, site, key, "block", Some(&receipt))?;
            let released = self.wait_for_release(seq, bound)?;
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            site_state(&mut inner, site).held = None;
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

/// Entry hook for the registry site (`runRegistry.append` invoked): `queued`
/// receipt while a barrier is held. `stimulus` is the caller's registry
/// handle (`None` keeps the entry inert).
pub(crate) fn registry_stimulus_queued(
    stimulus: Option<&Arc<StorageStimulus>>,
    event: &JsValue,
) -> Result<(), super::RunRegistryError> {
    let Some(stimulus) = stimulus else {
        return Ok(());
    };
    stimulus
        .queued(Site::Registry, &registry_event_key(event))
        .map_err(super::RunRegistryError::Io)
}
