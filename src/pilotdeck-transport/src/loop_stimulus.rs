//! #1371 controlled AgentLoop / turn-completion-fence stimulus (collection only).
//!
//! The #1242 step-1 design (`_tmp/20261102-issue-1242-step1/RESULT.md`
//! "完成顺序矩阵") names five lifecycle cells that the native collector cannot
//! drive today: the loop has no controllable tool phase, no result-unknown
//! terminal, no held `return`, and the gateway's completion fence publishes no
//! boundary witness. This module is the shared, env-armed entry for both
//! processes of the real assembly (gateway and sidecar):
//!
//! - `PILOTDECK_NATIVE_LOOP_STIMULUS` names an existing control directory.
//!   Unset keeps the production default: no control file is read and no
//!   receipt is published. A set value that is not a directory fails closed at
//!   startup (both `native_process::assembly` and the sidecar `main`).
//! - `<dir>/loop.json` is the per-process plan written by the collector before
//!   launch: `{"id","phase":"model"|"tool","order":<order>,"boundMs"?}`. A
//!   missing file leaves the entry *inert*: turns keep their real behaviour.
//!   The gateway resolves it at its own startup and the sidecar again when it
//!   starts, so "before launch" means before the *gateway* is launched:
//!   writing it later would let the gateway keep the automatic title request
//!   while the sidecar runs a controlled turn.
//!   Optional `requireProvider: true` arms an actual admitted model invocation
//!   before the controlled outcome. Absent/false retains the original stimulus.
//! - `<dir>/release.json` is the collector's release, written atomically:
//!   `{"id","stage":"outcome"|"return","outcome":"unknown"|"known"}`.
//! - `<dir>/witness/<pid>-<seq>.json` are the product's receipts, published
//!   atomically (tmp + rename) so a half-written file is never readable as a
//!   finished receipt. Both processes share one directory; the pid prefix
//!   keeps their monotonic sequences from colliding.
//!
//! Every wait is bounded (`boundMs`, default 30 s). A present `boundMs` must
//! be a whole number of milliseconds inside `1..=120000`: anything else is a
//! claim the loop cannot honour and fails closed. A lost collector yields a
//! `loop.bound` receipt and a failed turn, never a hung process: "never
//! completes" must be red, not stuck.
use pilotdeck_protocol::{JsObject, JsValue, parse_json, stringify_json};
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{
        Arc, LazyLock, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

pub const STIMULUS_ENV: &str = "PILOTDECK_NATIVE_LOOP_STIMULUS";
const DEFAULT_BOUND: Duration = Duration::from_secs(30);
const MAX_BOUND: Duration = Duration::from_secs(120);
const RELEASE_POLL: Duration = Duration::from_millis(10);

/// The two phases of the #1242 stimulus plan (`TOOL` row of the stimulus
/// difference table): `model` emits only `model_request_started` before the
/// wait; `tool` also emits `tool_calls_detected` with the `controlled_wait`
/// call so the tool prefix and the resulting sequence are reconstructible.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Model,
    Tool,
}
impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Tool => "tool",
        }
    }
    fn parse(value: &str) -> Option<Self> {
        match value {
            "model" => Some(Self::Model),
            "tool" => Some(Self::Tool),
            _ => None,
        }
    }
}

/// The five completion orderings of the design matrix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Order {
    /// U-T: gateway timeout first, unknown released afterwards.
    TimeoutFirst,
    /// U-F: unknown released before the timer fires.
    UnknownFirst,
    /// K-F: known success released before the timer fires.
    KnownFinalFirst,
    /// A-C: timeout, client done, `fence.complete`, then a late unknown.
    AfterComplete,
    /// K-P: the loop's completed result is buffered behind a held `return`.
    KnownFinalBeforeTimeoutPending,
}
impl Order {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TimeoutFirst => "timeout-first",
            Self::UnknownFirst => "unknown-first",
            Self::KnownFinalFirst => "known-final-first",
            Self::AfterComplete => "after-complete",
            Self::KnownFinalBeforeTimeoutPending => "known-final-before-timeout-pending",
        }
    }
    fn parse(value: &str) -> Option<Self> {
        match value {
            "timeout-first" => Some(Self::TimeoutFirst),
            "unknown-first" => Some(Self::UnknownFirst),
            "known-final-first" => Some(Self::KnownFinalFirst),
            "after-complete" => Some(Self::AfterComplete),
            "known-final-before-timeout-pending" => Some(Self::KnownFinalBeforeTimeoutPending),
            _ => None,
        }
    }
}

/// One parsed `loop.json` plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub id: String,
    pub phase: Phase,
    pub order: Order,
    pub bound: Duration,
    pub require_provider: bool,
}

impl Plan {
    /// Read the plan. `Ok(None)` means "no plan": the entry stays armed but
    /// inert and turns keep their real behaviour. A malformed plan is an
    /// error so the entry fails closed instead of silently turning normally.
    fn read(dir: &Path) -> Result<Option<Self>, String> {
        let bytes = match fs::read(dir.join("loop.json")) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(format!(
                    "native loop stimulus: loop.json unreadable: {error}"
                ));
            }
        };
        let value = parse_json(&String::from_utf8_lossy(&bytes))
            .map_err(|error| format!("native loop stimulus: loop.json is not JSON: {error}"))?;
        let text = |name: &str| {
            value
                .get(name)
                .and_then(JsValue::as_string)
                .and_then(|value| value.to_utf8().ok())
        };
        let id = text("id")
            .ok_or_else(|| "native loop stimulus: loop.json needs a string id".to_string())?;
        let phase = text("phase")
            .as_deref()
            .and_then(Phase::parse)
            .ok_or_else(|| {
                "native loop stimulus: loop.json phase must be model or tool".to_string()
            })?;
        let order = text("order")
            .as_deref()
            .and_then(Order::parse)
            .ok_or_else(|| {
                "native loop stimulus: loop.json order is not a known completion ordering"
                    .to_string()
            })?;
        let bound = match value.get("boundMs") {
            None => DEFAULT_BOUND,
            Some(raw) => {
                // A present bound is a claim: half of one is not "the
                // default", not a zero-millisecond bound, and one above the
                // cap is not the cap either, so anything but a whole number
                // of milliseconds inside `1..=MAX_BOUND` fails closed.
                let cap = MAX_BOUND.as_millis() as f64;
                let ms = raw
                    .as_number()
                    .filter(|ms| ms.is_finite() && *ms >= 1.0 && *ms <= cap && ms.fract() == 0.0)
                    .ok_or_else(|| {
                        format!(
                            "native loop stimulus: loop.json boundMs must be a whole number of \
                             milliseconds between 1 and {cap}"
                        )
                    })?;
                Duration::from_millis(ms as u64)
            }
        };
        let require_provider = value
            .get("requireProvider")
            .map(|raw| {
                raw.as_bool().ok_or_else(|| {
                    "native loop stimulus: loop.json requireProvider must be a boolean".to_string()
                })
            })
            .transpose()?
            .unwrap_or(false);
        Ok(Some(Self {
            id,
            phase,
            order,
            bound,
            require_provider,
        }))
    }
}

/// The collector's release for one stage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub stage: String,
    pub outcome: Option<String>,
}

/// Armed-but-inert / planned / invalid, resolved once per process.
enum State {
    Inert,
    Planned(Plan),
    Invalid(String),
}

pub struct LoopStimulus {
    dir: PathBuf,
    state: State,
    seq: AtomicU64,
    /// Fence witnesses are deduplicated: `is_current` is polled per event, so
    /// without this the same boundary would be stamped thousands of times.
    fenced: Mutex<Vec<String>>,
}

static GLOBAL: LazyLock<Option<Arc<LoopStimulus>>> = LazyLock::new(|| {
    let value = std::env::var(STIMULUS_ENV).ok()?;
    if value.trim().is_empty() {
        return None;
    }
    Some(Arc::new(LoopStimulus::control(PathBuf::from(value))))
});

/// Validate an env value for startup. Both the gateway assembly and the
/// sidecar `main` call this before advertising readiness, so an invalid value
/// fails closed instead of producing a half-armed process.
pub fn validate_value(value: &str) -> Result<PathBuf, String> {
    if value.trim().is_empty() {
        return Err(format!(
            "native loop stimulus ({STIMULUS_ENV}) must be a control directory."
        ));
    }
    let path = PathBuf::from(value);
    if !path.is_dir() {
        return Err(format!(
            "native loop stimulus ({STIMULUS_ENV}) must name an existing directory."
        ));
    }
    Plan::read(&path)?;
    Ok(path)
}

impl LoopStimulus {
    /// The process-wide armed entry. Resolved once from the environment and
    /// immutable for the process lifetime.
    pub fn global() -> Option<Arc<Self>> {
        GLOBAL.clone()
    }

    /// Construct an armed entry directly (tests and the startup validation
    /// path). The plan is read once; a malformed plan is retained so the
    /// controlled loop fails closed instead of falling back to a real turn.
    pub fn control(dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        let state = match Plan::read(&dir) {
            Ok(Some(plan)) => State::Planned(plan),
            Ok(None) => State::Inert,
            Err(message) => State::Invalid(message),
        };
        Self {
            dir,
            state,
            seq: AtomicU64::new(0),
            fenced: Mutex::new(Vec::new()),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The plan, `None` when inert, `Err` when the plan is malformed.
    pub fn plan(&self) -> Result<Option<&Plan>, &str> {
        match &self.state {
            State::Inert => Ok(None),
            State::Planned(plan) => Ok(Some(plan)),
            State::Invalid(message) => Err(message),
        }
    }

    /// A resolved plan is the one decision that replaces the turn, suppresses
    /// the automatic title request and publishes fence receipts. Callers must
    /// ask this question rather than re-deriving it from `plan()`, so the
    /// three sites cannot drift apart.
    pub fn is_planned(&self) -> bool {
        matches!(self.plan(), Ok(Some(_)))
    }

    /// Publish one receipt atomically. Evidence only: a failed receipt must
    /// not change the turn's classification, so the sequence is still
    /// returned and the write error is swallowed.
    pub fn publish(&self, site: &str, phase: &str, key: &str, extra: Option<&JsObject>) -> u64 {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst);
        let mut out = JsObject::new();
        out.insert("seq".into(), JsValue::Number(seq as f64));
        out.insert("pid".into(), JsValue::Number(f64::from(std::process::id())));
        out.insert("site".into(), JsValue::from(site));
        out.insert("phase".into(), JsValue::from(phase));
        out.insert("key".into(), JsValue::from(key));
        if let Some(extra) = extra {
            for (name, value) in extra.entries() {
                out.insert(name.clone(), value.clone());
            }
        }
        let _ = self.write(seq, JsValue::Object(out));
        seq
    }

    /// A fence receipt, deduplicated per `phase`/`key` pair.
    pub fn fence(&self, phase: &str, key: &str) {
        {
            let mut fenced = self.fenced.lock().unwrap_or_else(|p| p.into_inner());
            let tag = format!("{phase}\0{key}");
            if fenced.iter().any(|seen| seen == &tag) {
                return;
            }
            fenced.push(tag);
        }
        self.publish("fence", phase, key, None);
    }

    fn write(&self, seq: u64, value: JsValue) -> io::Result<()> {
        let dir = self.dir.join("witness");
        fs::create_dir_all(&dir)?;
        let Some(text) =
            stringify_json(&value).map_err(|error| io::Error::other(error.to_string()))?
        else {
            return Err(io::Error::other("witness receipt is not encodable"));
        };
        let path = dir.join(format!("{:08}-{:06}.json", std::process::id(), seq));
        let temporary = path.with_extension("json.tmp");
        fs::write(&temporary, text + "\n")?;
        fs::rename(&temporary, &path)
    }

    /// Wait, bounded, for the collector to release `stage`. Returns `None`
    /// when the bound expired (the caller fails closed and publishes
    /// `loop.bound`). Cancellation is deliberately not observed here: the
    /// whole point of the timeout-first cell is that the loop keeps waiting
    /// after the gateway has already timed the turn out, exactly like the TS
    /// controlled loop awaiting its outcome promise.
    pub fn await_release(&self, stage: &str) -> Option<Release> {
        let plan = match self.plan() {
            Ok(Some(plan)) => plan,
            _ => return None,
        };
        let deadline = Instant::now() + plan.bound;
        loop {
            match fs::read(self.dir.join("release.json")) {
                Ok(bytes) => {
                    if let Ok(value) = parse_json(&String::from_utf8_lossy(&bytes)) {
                        // A release names its plan: a file without an id
                        // belongs to no plan and must never release one.
                        let matches_id = value
                            .get("id")
                            .and_then(JsValue::as_string)
                            .is_some_and(|id| id.equals_str(&plan.id));
                        if matches_id
                            && value
                                .get("stage")
                                .and_then(JsValue::as_string)
                                .is_some_and(|value| value.equals_str(stage))
                        {
                            return Some(Release {
                                stage: stage.to_string(),
                                outcome: value
                                    .get("outcome")
                                    .and_then(JsValue::as_string)
                                    .and_then(|value| value.to_utf8().ok()),
                            });
                        }
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(_) => {}
            }
            if Instant::now() >= deadline {
                return None;
            }
            thread::sleep(RELEASE_POLL);
        }
    }

    /// The plan's bound, for the caller's own bookkeeping.
    pub fn bound(&self) -> Duration {
        match self.plan() {
            Ok(Some(plan)) => plan.bound,
            _ => DEFAULT_BOUND,
        }
    }
}

/// Gateway-side fence witness used by `TurnCompletionFence`. A no-op unless a
/// planned stimulus is armed.
pub fn witness_fence(phase: &str, key: &str) {
    if let Some(stimulus) = LoopStimulus::global()
        && stimulus.is_planned()
    {
        stimulus.fence(phase, key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "pdrs-loop-stimulus-{name}-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&dir).expect("#1371 scratch directory");
        dir
    }

    fn write(dir: &Path, name: &str, text: &str) {
        fs::write(dir.join(name), text).expect("#1371 control file");
    }

    #[test]
    fn missing_plan_is_inert() {
        let dir = scratch("inert");
        let stimulus = LoopStimulus::control(&dir);
        assert_eq!(stimulus.plan(), Ok(None), "#1371 no plan is inert");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn plan_reads_every_ordering_and_bounds() {
        let dir = scratch("plan");
        for (order, expected) in [
            ("timeout-first", Order::TimeoutFirst),
            ("unknown-first", Order::UnknownFirst),
            ("known-final-first", Order::KnownFinalFirst),
            ("after-complete", Order::AfterComplete),
            (
                "known-final-before-timeout-pending",
                Order::KnownFinalBeforeTimeoutPending,
            ),
        ] {
            write(
                &dir,
                "loop.json",
                &format!(r#"{{"id":"p","phase":"tool","order":"{order}","boundMs":1500}}"#),
            );
            let stimulus = LoopStimulus::control(&dir);
            let plan = stimulus
                .plan()
                .expect("#1371 plan parses")
                .expect("#1371 planned");
            assert_eq!(plan.order, expected, "#1371 order {order}");
            assert_eq!(plan.phase, Phase::Tool, "#1371 tool phase");
            assert_eq!(plan.bound, Duration::from_millis(1500), "#1371 bound");
        }
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn provider_arm_is_strict_and_opt_in() {
        let dir = scratch("model-arm");
        for (field, expected) in [
            ("", Some(false)),
            (",\"requireProvider\":false", Some(false)),
            (",\"requireProvider\":true", Some(true)),
            (",\"requireProvider\":\"true\"", None),
        ] {
            write(
                &dir,
                "loop.json",
                &format!(r#"{{"id":"p","phase":"model","order":"known-final-first"{field}}}"#),
            );
            let stimulus = LoopStimulus::control(&dir);
            match expected {
                Some(value) => assert_eq!(
                    stimulus
                        .plan()
                        .expect("valid provider flag")
                        .expect("armed plan")
                        .require_provider,
                    value,
                ),
                None => assert!(
                    stimulus.plan().is_err(),
                    "invalid provider flag must fail closed"
                ),
            }
        }
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn malformed_plan_fails_closed() {
        let dir = scratch("malformed");
        write(
            &dir,
            "loop.json",
            r#"{"id":"p","phase":"nope","order":"x"}"#,
        );
        let stimulus = LoopStimulus::control(&dir);
        assert!(stimulus.plan().is_err(), "#1371 unknown phase fails closed");
        assert!(
            validate_value(dir.to_str().expect("#1371 utf8 path")).is_err(),
            "#1371 startup validation rejects a malformed plan"
        );
        // A bound that is present but not a whole number of milliseconds
        // inside the documented range is a claim the loop cannot honour:
        // silently using the default, truncating 0.4 ms to zero or capping
        // 200 s to the 120 s limit would all record a cell under a bound
        // nobody asked for. `null` is a present-but-unusable claim too.
        // A fractional value inside the range isolates the whole-millisecond
        // rule from the range rule: 1500.5 is neither below 1 nor above the
        // cap, so only the fraction check can reject it.
        for bound in ["0", "-5", "0.4", "1500.5", "\"400\"", "200000", "null"] {
            write(
                &dir,
                "loop.json",
                &format!(
                    r#"{{"id":"p","phase":"model","order":"unknown-first","boundMs":{bound}}}"#
                ),
            );
            assert!(
                LoopStimulus::control(&dir).plan().is_err(),
                "#1371 unsupported bound fails closed"
            );
        }
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn witness_receipts_are_finished_json_only() {
        let dir = scratch("witness");
        let stimulus = LoopStimulus::control(&dir);
        let first = stimulus.publish("loop", "wait", "id-1", None);
        let second = stimulus.publish("loop", "final", "id-1", None);
        assert_eq!((first, second), (0, 1), "#1371 monotonic sequences");
        let names = fs::read_dir(dir.join("witness"))
            .expect("#1371 witness directory")
            .map(|entry| entry.expect("#1371 entry").file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(names.len(), 2, "#1371 one file per receipt");
        assert!(
            names.iter().all(|name| name.ends_with(".json")),
            "#1371 tmp names never survive a finished receipt"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn release_matches_id_and_stage() {
        let dir = scratch("release");
        write(
            &dir,
            "loop.json",
            r#"{"id":"p1","phase":"model","order":"unknown-first","boundMs":400}"#,
        );
        let stimulus = LoopStimulus::control(&dir);
        // A foreign id and a foreign stage are both ignored. Only the foreign
        // id is on disk for the first wait, so a mutant that drops the id check
        // accepts it instead of expiring at the bound.
        write(&dir, "release.json", r#"{"id":"other","stage":"outcome"}"#);
        assert!(
            stimulus.await_release("outcome").is_none(),
            "#1371 foreign releases are ignored"
        );
        // No id at all is not a wildcard: it belongs to no plan.
        write(&dir, "release.json", r#"{"stage":"outcome"}"#);
        assert!(
            stimulus.await_release("outcome").is_none(),
            "#1371 releases without an id are ignored"
        );
        write(&dir, "release.json", r#"{"id":"p1","stage":"return"}"#);
        assert!(
            stimulus.await_release("outcome").is_none(),
            "#1371 foreign stages are ignored"
        );
        write(
            &dir,
            "release.json",
            r#"{"id":"p1","stage":"outcome","outcome":"unknown"}"#,
        );
        let released = stimulus.await_release("outcome").expect("#1371 release");
        assert_eq!(
            released.outcome.as_deref(),
            Some("unknown"),
            "#1371 outcome"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn bound_expires_instead_of_hanging() {
        let dir = scratch("bound");
        write(
            &dir,
            "loop.json",
            r#"{"id":"p1","phase":"model","order":"timeout-first","boundMs":150}"#,
        );
        let stimulus = Arc::new(LoopStimulus::control(&dir));
        // A watchdog keeps a mutant that drops the bound from hanging libtest:
        // the wait runs on its own thread and the test fails if it never ends.
        let (sender, receiver) = std::sync::mpsc::channel();
        let waiting = Arc::clone(&stimulus);
        std::thread::spawn(move || {
            let _ = sender.send(waiting.await_release("outcome"));
        });
        let ended = receiver.recv_timeout(Duration::from_secs(5));
        assert!(
            matches!(ended, Ok(None)),
            "#1371 bound expires instead of hanging"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn fence_receipts_are_deduplicated() {
        let dir = scratch("fence");
        write(
            &dir,
            "loop.json",
            r#"{"id":"p1","phase":"model","order":"after-complete"}"#,
        );
        let stimulus = LoopStimulus::control(&dir);
        stimulus.fence("current", "true");
        stimulus.fence("current", "true");
        stimulus.fence("complete", "run-1");
        let files = fs::read_dir(dir.join("witness"))
            .expect("#1371 witness directory")
            .count();
        assert_eq!(files, 2, "#1371 repeated fence polls publish once");
        let _ = fs::remove_dir_all(dir);
    }
}
