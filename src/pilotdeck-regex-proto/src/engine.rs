//! The bounded rquickjs (QuickJS-NG 0.16.2) RegExp engine.
//!
//! Boundary (#1265, following the #685 report): the only JavaScript this
//! module ever evaluates is the fixed [`BOOTSTRAP`] source below. Patterns,
//! flags and subjects are passed as *values* — an array of UTF-16 code units
//! and a flag string — never spliced into evaluated source. At the build level
//! rquickjs-sys compiles exactly `quickjs.c`, `libregexp.c`, `libunicode.c`
//! and `dtoa.c`: `quickjs-libc.c` (stdio/os) is not part of the build, no
//! module resolver is registered, and this crate forbids `unsafe` via the
//! workspace lint, so there is no path from a pattern to host I/O.
//!
//! UTF-16 fidelity: both directions go through `String.fromCharCode` /
//! `charCodeAt`, which are exact at the code-unit level, so lone surrogates
//! and surrogate pairs survive round trips. `String::from_utf16_lossy` is
//! used only for human-facing diagnostics ([`CompiledPattern::pattern_debug`])
//! and never on a matching path.
//!
//! Budget: one interrupt callback ("poll") is charged per poll; QuickJS calls
//! the handler from inside regexp execution roughly every 10 000 steps and
//! from the bytecode interpreter loop, so a [`MatchBudget`] is the shared
//! *total* for one request. The JS half of the subject bridge (`fromCodes`)
//! runs inside JS and is charged like any other JS work; the Rust half —
//! the per-code-unit `Array::set` loop that builds the bridge array — runs
//! before any JS and is *not* charged, so it is bounded by the JS heap
//! alone (a subject length limit is left to the production-integration
//! change). Exhaustion is always the explicit [`MatchError::Interrupted`],
//! never a silent no-match.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

use rquickjs::{Array, CatchResultExt, CaughtError, Context, Ctx, Function, Runtime};

/// The fixed bootstrap evaluated once per engine. No user input reaches this
/// source; `units` arrays are constructed by Rust from the caller's u16 slices.
const BOOTSTRAP: &str = r#"
"use strict";
(function () {
  var cache = new Map();
  var next = 0;
  function fromCodes(units) {
    var out = "";
    for (var i = 0; i < units.length; i++) out += String.fromCharCode(units[i]);
    return out;
  }
  globalThis.__pdrsStore = function (units, flags) {
    var re = new RegExp(fromCodes(units), flags);
    // The `d` flag only adds `match.indices`; it never changes what matches.
    // Compiled when the engine supports it, so captures are available.
    var indexed = re;
    if (flags.indexOf("d") < 0) {
      try { indexed = new RegExp(fromCodes(units), flags + "d"); } catch (e) { indexed = re; }
    }
    var id = next++;
    cache.set(id, indexed);
    return id;
  };
  globalThis.__pdrsRelease = function (id) { cache.delete(id); };
  globalThis.__pdrsCacheSize = function () { return cache.size; };
  globalThis.__pdrsExec = function (id, units) {
    var re = cache.get(id);
    re.lastIndex = 0;
    var m = re.exec(fromCodes(units));
    if (m === null) return null;
    var out = [m.index, m[0].length];
    var indices = m.indices;
    if (indices === undefined) {
      out.push(-3);
    } else {
      out.push(0);
      for (var i = 1; i < indices.length; i++) {
        var g = indices[i];
        if (g === undefined) { out.push(-1, -1); } else { out.push(g[0], g[1]); }
      }
    }
    return out;
  };
})();
"#;

/// Marker the exec bridge returns in place of the group list when the engine
/// could not compile the `d`-flagged twin of a pattern (indices unavailable).
const NO_INDICES_MARKER: i32 = -3;
/// Marker for an unmatched group inside the flat group list.
const UNMATCHED_GROUP: i32 = -1;

/// Resource limits applied when an engine is created.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EngineLimits {
    /// Maximum bytes the QuickJS runtime may allocate (`set_memory_limit`).
    pub memory_bytes: usize,
    /// Maximum C stack the runtime may use (`set_max_stack_size`).
    pub stack_bytes: usize,
    /// Maximum UTF-16 code units accepted in one pattern.
    pub pattern_units_max: usize,
}

impl EngineLimits {
    /// The defaults used by [`RegexEngine::new`]: 16 MiB / 512 KiB, matching
    /// the #685 report's probe configuration, and a 256 Ki-unit pattern cap.
    pub const fn new() -> Self {
        Self {
            memory_bytes: 16 * 1024 * 1024,
            stack_bytes: 512 * 1024,
            pattern_units_max: 256 * 1024,
        }
    }
}

impl Default for EngineLimits {
    fn default() -> Self {
        Self::new()
    }
}

/// The per-request match budget.
///
/// One unit is charged per interrupt poll (QuickJS polls the interrupt
/// handler from inside regexp execution and the interpreter loop). The budget
/// is shared by every exec of one request; after each [`RegexEngine::exec`]
/// it reflects the polls consumed so far, so exhaustion carries over until a
/// new request constructs a fresh budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MatchBudget {
    remaining_polls: i64,
}

impl MatchBudget {
    /// A budget allowing at most `poll_limit` interrupt polls.
    pub const fn new(poll_limit: u64) -> Self {
        Self {
            remaining_polls: if poll_limit > i64::MAX as u64 {
                i64::MAX
            } else {
                poll_limit as i64
            },
        }
    }

    /// Polls still allowed by this budget.
    pub const fn remaining_polls(&self) -> u64 {
        if self.remaining_polls < 0 {
            0
        } else {
            self.remaining_polls as u64
        }
    }
}

/// One successful match, in UTF-16 code-unit offsets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MatchResult {
    /// Offset of the first code unit of the match.
    pub index: usize,
    /// Length of the match in code units.
    pub length: usize,
    /// Numbered capture groups, when the engine provided `match.indices`;
    /// `None` per group when the group did not participate in the match.
    /// `None` for the whole field when the `d` flag is unavailable.
    pub groups: Option<Vec<Option<(usize, usize)>>>,
}

/// The outcome of a match that ran to completion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecOutcome {
    Matched(MatchResult),
    NoMatch,
}

/// Why an exec did not produce a match verdict.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MatchError {
    /// The budget ran out; the engine was interrupted mid-match. Never
    /// collapses into [`ExecOutcome::NoMatch`].
    Interrupted,
    /// A runtime resource limit (memory or C stack) was hit while matching.
    Resource(String),
    /// Any other engine-level failure.
    Engine(String),
}

impl fmt::Display for MatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Interrupted => write!(f, "match budget exhausted (interrupted)"),
            Self::Resource(message) => write!(f, "engine resource limit: {message}"),
            Self::Engine(message) => write!(f, "engine failure: {message}"),
        }
    }
}

impl std::error::Error for MatchError {}

/// Why a pattern failed to compile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompileError {
    /// The engine rejected the pattern/flags (JS `SyntaxError`).
    Syntax(String),
    /// A resource limit was hit while compiling (length cap, memory, stack).
    Resource(String),
    /// Any other engine-level failure.
    Engine(String),
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Syntax(message) => write!(f, "invalid regular expression: {message}"),
            Self::Resource(message) => write!(f, "compile resource limit: {message}"),
            Self::Engine(message) => write!(f, "engine failure: {message}"),
        }
    }
}

impl std::error::Error for CompileError {}

/// The engine failed to start at all (runtime/context creation, bootstrap).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineInitError(String);

impl fmt::Display for EngineInitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "engine init failed: {}", self.0)
    }
}

impl std::error::Error for EngineInitError {}

impl From<rquickjs::Error> for EngineInitError {
    fn from(error: rquickjs::Error) -> Self {
        Self(error.to_string())
    }
}

struct EngineInner {
    runtime: Runtime,
    context: Context,
    remaining_polls: Arc<AtomicI64>,
    interrupted: Arc<AtomicBool>,
    limits: EngineLimits,
}

/// A bounded JS RegExp engine. Clone shares one QuickJS runtime; the runtime
/// is not used concurrently — QuickJS is single-threaded per runtime, so one
/// engine handle belongs to one thread at a time.
///
/// The handle is **neither `Send` nor `Sync`** (pinned by the `compile_fail`
/// doctests below): this crate does not enable rquickjs's `parallel`
/// feature, so `Runtime` and `Context` are `Rc`/`RefCell`-backed
/// (`rquickjs-core` gates its `unsafe impl Send/Sync` behind that feature),
/// `EngineInner` is neither `Send` nor `Sync`, and `Arc<EngineInner>`
/// transmits neither marker. That is the desired protection, not an
/// oversight: the compiler — not a comment — keeps one engine on one
/// thread. If a workspace member ever enables rquickjs's `parallel`
/// feature (Cargo feature unification would turn every `Arc<EngineInner>`
/// into a shareable handle without touching this crate), these doctests
/// stop failing to compile and surface the change.
///
/// ```compile_fail
/// use pilotdeck_regex_proto::RegexEngine;
/// fn is_send<T: Send>() {}
/// is_send::<RegexEngine>();
/// ```
///
/// ```compile_fail
/// use pilotdeck_regex_proto::RegexEngine;
/// fn is_sync<T: Sync>() {}
/// is_sync::<RegexEngine>();
/// ```
#[derive(Clone)]
pub struct RegexEngine {
    inner: Arc<EngineInner>,
}

impl RegexEngine {
    /// Create an engine with the given limits.
    pub fn new(limits: EngineLimits) -> Result<Self, EngineInitError> {
        let runtime = Runtime::new()?;
        runtime.set_memory_limit(limits.memory_bytes);
        runtime.set_max_stack_size(limits.stack_bytes);
        let remaining_polls = Arc::new(AtomicI64::new(i64::MAX));
        let interrupted = Arc::new(AtomicBool::new(false));
        {
            let rem = Arc::clone(&remaining_polls);
            let hit = Arc::clone(&interrupted);
            runtime.set_interrupt_handler(Some(Box::new(move || {
                // One unit per poll; interrupt once the allowance is spent.
                let previous = rem.fetch_sub(1, Ordering::SeqCst);
                if previous <= 0 {
                    hit.store(true, Ordering::SeqCst);
                    return true;
                }
                false
            })));
        }
        let context = Context::full(&runtime)?;
        // `Arc` here is for shared ownership by clones and compiled patterns,
        // not for threading: without rquickjs's `parallel` feature the inner
        // is neither `Send` nor `Sync`, so the handle stays single-threaded
        // (see the `RegexEngine` doctests).
        #[allow(clippy::arc_with_non_send_sync)]
        let engine = Self {
            inner: Arc::new(EngineInner {
                runtime,
                context,
                remaining_polls,
                interrupted,
                limits,
            }),
        };
        engine
            .inner
            .context
            .with(|ctx| ctx.eval::<(), _>(BOOTSTRAP))?;
        Ok(engine)
    }

    /// The limits this engine was created with.
    pub fn limits(&self) -> EngineLimits {
        self.inner.limits
    }

    /// Bytes of JS heap the runtime currently reports (`JS_ComputeMemoryUsage`).
    /// Probe for the #1265 acceptance checklist's resource measurements.
    pub fn memory_used_bytes(&self) -> i64 {
        self.inner.runtime.memory_usage().memory_used_size
    }

    /// Number of patterns currently held in the engine's JS-side cache.
    /// Lifecycle evidence that dropping patterns releases their entries.
    pub fn cached_pattern_count(&self) -> usize {
        self.inner
            .context
            .with(|ctx| -> Result<usize, rquickjs::Error> {
                let size: Function = ctx.globals().get("__pdrsCacheSize")?;
                size.call(())
            })
            .expect("#1265 cache-size probe must not fail on a live engine")
    }

    /// Compile a pattern (UTF-16 code units) with a flag string.
    ///
    /// Flags are validated by the engine itself, exactly like `new RegExp`:
    /// unknown or duplicated flags are a [`CompileError::Syntax`].
    pub fn compile(&self, pattern: &[u16], flags: &str) -> Result<CompiledPattern, CompileError> {
        if pattern.len() > self.inner.limits.pattern_units_max {
            return Err(CompileError::Resource(format!(
                "pattern length {} exceeds the {} code-unit limit",
                pattern.len(),
                self.inner.limits.pattern_units_max
            )));
        }
        // Compile runs outside any request budget: unarm the slot so a stale
        // exhausted budget from a previous exec cannot interrupt compilation.
        self.inner.interrupted.store(false, Ordering::SeqCst);
        self.inner.remaining_polls.store(i64::MAX, Ordering::SeqCst);
        let id = self
            .inner
            .context
            .with(|ctx| -> Result<i64, CompileError> {
                let store: Function = ctx
                    .globals()
                    .get("__pdrsStore")
                    .map_err(compile_engine_error)?;
                // `.catch` keeps the pending exception when building the
                // bridge array itself fails (e.g. the memory limit), so it
                // classifies by its real name instead of a generic engine error.
                let units = code_units(&ctx, pattern)
                    .catch(&ctx)
                    .map_err(classify_compile_error)?;
                match store.call::<_, i64>((units, flags)).catch(&ctx) {
                    Ok(id) => Ok(id),
                    Err(caught) => Err(classify_compile_error(caught)),
                }
            })?;
        Ok(CompiledPattern {
            engine: Arc::clone(&self.inner),
            id: id as u32,
            pattern: pattern.to_vec(),
            flags: flags.to_owned(),
        })
    }

    /// Execute a compiled pattern against a subject (UTF-16 code units),
    /// charging the shared per-request `budget`.
    pub fn exec(
        &self,
        pattern: &CompiledPattern,
        subject: &[u16],
        budget: &mut MatchBudget,
    ) -> Result<ExecOutcome, MatchError> {
        // Pattern ids are per-engine (the bootstrap's `next` counter restarts
        // in every context): a pattern compiled by another engine would hit
        // *this* engine's cache under the same id and silently run the wrong
        // pattern, so the mismatch is an error instead of a verdict.
        if !Arc::ptr_eq(&pattern.engine, &self.inner) {
            return Err(MatchError::Engine(
                "pattern belongs to another engine".to_owned(),
            ));
        }
        self.inner.interrupted.store(false, Ordering::SeqCst);
        self.inner
            .remaining_polls
            .store(budget.remaining_polls, Ordering::SeqCst);
        let outcome = self
            .inner
            .context
            .with(|ctx| -> Result<ExecOutcome, MatchError> {
                let exec: Function = ctx.globals().get("__pdrsExec").map_err(engine_error)?;
                let units = code_units(&ctx, subject)
                    .catch(&ctx)
                    .map_err(classify_match_error)?;
                match exec
                    .call::<_, Option<Array>>((pattern.id, units))
                    .catch(&ctx)
                {
                    Ok(None) => Ok(ExecOutcome::NoMatch),
                    Ok(Some(flat)) => parse_match(&flat),
                    Err(caught) => Err(classify_match_error(caught)),
                }
            });
        // Write the consumed polls back so the budget spans the whole request.
        let remaining = self.inner.remaining_polls.load(Ordering::SeqCst);
        budget.remaining_polls = remaining.max(0);
        // Fail closed: once the handler decided to interrupt, the verdict is
        // Interrupted even if a value also made it out of the engine.
        if self.inner.interrupted.load(Ordering::SeqCst) {
            return Err(MatchError::Interrupted);
        }
        // Disarm on the completed path: an allowance spent down to exactly
        // zero must not stay armed against later JS entries that run outside
        // any request budget (pattern drops, cache probes) — their next poll
        // would see `previous <= 0` and interrupt them. An interrupted return
        // keeps the next request's entry stores above as its reset, which
        // `fresh_budget_does_not_inherit_previous_exhaustion` pins.
        self.inner.remaining_polls.store(i64::MAX, Ordering::SeqCst);
        outcome
    }
}

impl fmt::Debug for RegexEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RegexEngine")
            .field("limits", &self.inner.limits)
            .finish_non_exhaustive()
    }
}

/// A compiled pattern bound to the engine that produced it.
///
/// Holding a pattern keeps its engine alive (the JS value lives in the
/// engine's context); dropping it releases the JS-side cache entry.
pub struct CompiledPattern {
    engine: Arc<EngineInner>,
    id: u32,
    pattern: Vec<u16>,
    flags: String,
}

impl CompiledPattern {
    /// The pattern's UTF-16 code units, as passed to [`RegexEngine::compile`].
    pub fn pattern(&self) -> &[u16] {
        &self.pattern
    }

    /// The flags this pattern was compiled with.
    pub fn flags(&self) -> &str {
        &self.flags
    }

    /// Human-facing diagnostics only: the pattern with unpaired surrogates
    /// replaced by U+FFFD. Never used on a matching path; the test
    /// `diagnostic_spelling_is_lossy_and_off_the_matching_path` pins that the
    /// lossy spelling of a lone surrogate is U+FFFD while matching still sees
    /// the exact code unit.
    pub fn pattern_debug(&self) -> String {
        String::from_utf16_lossy(&self.pattern)
    }
}

impl Drop for CompiledPattern {
    fn drop(&mut self) {
        self.engine.context.with(|ctx| {
            if let Ok(release) = ctx.globals().get::<_, Function>("__pdrsRelease") {
                let _ = release.call::<_, ()>((self.id,));
            }
        });
    }
}

impl fmt::Debug for CompiledPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CompiledPattern")
            .field("id", &self.id)
            .field("flags", &self.flags)
            .field("pattern_debug", &self.pattern_debug())
            .finish_non_exhaustive()
    }
}

fn engine_error(error: rquickjs::Error) -> MatchError {
    MatchError::Engine(error.to_string())
}

fn compile_engine_error(error: rquickjs::Error) -> CompileError {
    CompileError::Engine(error.to_string())
}

/// Build the JS array of code units for the bootstrap bridge.
fn code_units<'js>(ctx: &Ctx<'js>, units: &[u16]) -> rquickjs::Result<Array<'js>> {
    let array = Array::new(ctx.clone())?;
    for (index, &unit) in units.iter().enumerate() {
        array.set(index, unit)?;
    }
    Ok(array)
}

fn exception_parts(error: &CaughtError<'_>) -> (String, String) {
    match error {
        CaughtError::Exception(exception) => {
            let name = exception
                .as_object()
                .get::<_, String>("name")
                .unwrap_or_else(|_| "Error".to_owned());
            let message = exception.message().unwrap_or_default();
            (name, message)
        }
        _ => ("Error".to_owned(), error.to_string()),
    }
}

fn classify_compile_error(error: CaughtError<'_>) -> CompileError {
    let (name, message) = exception_parts(&error);
    match (name.as_str(), message.as_str()) {
        // quickjs.c throws SyntaxError for *every* lre_compile failure, but
        // "stack overflow" is what libregexp reports when its parser recursion
        // hits the C stack bound (guarded by `set_max_stack_size`): a resource
        // limit, not a malformed pattern. Reclassified on purpose.
        (_, "stack overflow") => CompileError::Resource(message),
        ("SyntaxError", _) => CompileError::Syntax(message),
        ("InternalError", _) => CompileError::Resource(message),
        _ => CompileError::Engine(format!("{name}: {message}")),
    }
}

fn classify_match_error(error: CaughtError<'_>) -> MatchError {
    let (name, message) = exception_parts(&error);
    match name.as_str() {
        "InternalError" => MatchError::Resource(message),
        _ => MatchError::Engine(format!("{name}: {message}")),
    }
}

/// Decode the bootstrap exec result: `[index, length, marker?, groups...]`.
fn parse_match(flat: &Array<'_>) -> Result<ExecOutcome, MatchError> {
    let index: i32 = flat.get(0).map_err(engine_error)?;
    let length: i32 = flat.get(1).map_err(engine_error)?;
    let mut groups = None;
    let marker: i32 = flat.get(2).map_err(engine_error)?;
    if marker != NO_INDICES_MARKER {
        let mut parsed = Vec::new();
        let mut slot = 3;
        loop {
            let start: Option<i32> = flat.get(slot).map_err(engine_error)?;
            let Some(start) = start else { break };
            let end: i32 = flat.get(slot + 1).map_err(engine_error)?;
            parsed.push(if start == UNMATCHED_GROUP || end == UNMATCHED_GROUP {
                None
            } else {
                Some((start as usize, end as usize))
            });
            slot += 2;
        }
        groups = Some(parsed);
    }
    Ok(ExecOutcome::Matched(MatchResult {
        index: index as usize,
        length: length as usize,
        groups,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> RegexEngine {
        RegexEngine::new(EngineLimits::new()).expect("#1265 engine init")
    }

    fn compile(engine: &RegexEngine, pattern: &str, flags: &str) -> CompiledPattern {
        engine
            .compile(&pattern.encode_utf16().collect::<Vec<_>>(), flags)
            .expect("#1265 compile")
    }

    fn exec(
        engine: &RegexEngine,
        pattern: &CompiledPattern,
        subject: &str,
    ) -> Result<ExecOutcome, MatchError> {
        let mut budget = MatchBudget::new(u64::MAX);
        engine.exec(
            pattern,
            &subject.encode_utf16().collect::<Vec<_>>(),
            &mut budget,
        )
    }

    #[test]
    fn smoke_matches_and_rejects() {
        let engine = engine();
        let pattern = compile(&engine, "^a+$", "u");
        assert_eq!(
            exec(&engine, &pattern, "aaa"),
            Ok(ExecOutcome::Matched(MatchResult {
                index: 0,
                length: 3,
                groups: Some(vec![]),
            }))
        );
        assert_eq!(exec(&engine, &pattern, "aab"), Ok(ExecOutcome::NoMatch));
    }

    #[test]
    fn syntax_error_is_reported_as_compile_error() {
        let engine = engine();
        let error = engine
            .compile(&"a**".encode_utf16().collect::<Vec<_>>(), "u")
            .expect_err("#1265 a** must not compile");
        assert!(
            matches!(error, CompileError::Syntax(ref message) if message.contains("nothing to repeat")),
            "#1265 unexpected compile error: {error:?}"
        );
    }

    #[test]
    fn budget_exhaustion_is_an_explicit_interrupt() {
        let engine = engine();
        // Catastrophic backtracking; 18 a's and a '!' like the #685 probe.
        let pattern = compile(&engine, "^(a+)+(?=!)$", "");
        let subject = format!("{}!", "a".repeat(18));
        let mut budget = MatchBudget::new(1);
        let error = engine
            .exec(
                &pattern,
                &subject.encode_utf16().collect::<Vec<_>>(),
                &mut budget,
            )
            .expect_err("#1265 low budget must interrupt");
        assert_eq!(error, MatchError::Interrupted, "#1265 expected interrupt");
        // The same match with a large budget completes with no match.
        let mut big = MatchBudget::new(u64::MAX);
        let granted = big.remaining_polls();
        let outcome = engine
            .exec(
                &pattern,
                &subject.encode_utf16().collect::<Vec<_>>(),
                &mut big,
            )
            .expect("#1265 big budget must complete");
        assert_eq!(outcome, ExecOutcome::NoMatch);
        // Compare against the granted allowance, not `u64::MAX`: `new`
        // saturates to `i64::MAX`, so `< u64::MAX` holds even when nothing
        // was charged and would never catch a handler that stops counting.
        assert!(
            big.remaining_polls() < granted,
            "#1265 polls must be charged"
        );
    }

    #[test]
    fn dropping_patterns_releases_the_js_cache() {
        let engine = engine();
        let before = engine.cached_pattern_count();
        let pattern = compile(&engine, "x", "");
        assert_eq!(engine.cached_pattern_count(), before + 1);
        drop(pattern);
        assert_eq!(
            engine.cached_pattern_count(),
            before,
            "#1265 drop must release the cache entry"
        );
    }

    #[test]
    fn diagnostic_spelling_is_lossy_and_off_the_matching_path() {
        let engine = engine();
        let units: Vec<u16> = [0xd800u16].to_vec();
        let pattern = engine
            .compile(&units, "")
            .expect("#1265 lone high compiles");
        assert_eq!(pattern.pattern_debug(), "\u{fffd}");
        let mut budget = MatchBudget::new(u64::MAX);
        // The exact code unit still matches: `.` sees the lone surrogate.
        assert_eq!(
            engine.exec(&pattern, &units, &mut budget),
            Ok(ExecOutcome::Matched(MatchResult {
                index: 0,
                length: 1,
                groups: Some(vec![]),
            }))
        );
    }
}
