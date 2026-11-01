//! Isolated #1265 experiment; no product dependency or production call site.
//!
//! Only RegExp and base intrinsics are installed. All calls below are to native
//! builtins: no evaluated source, loader, host function, I/O, or exposed JS value.
//! Limits are prototype policy, **not** the outputSchema validation budget.
use rquickjs::{Array, Context, Ctx, Function, Object, Runtime, Value};
use rquickjs::{
    context::intrinsic,
    function::{Args, Constructor, This},
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub memory_bytes: usize,
    /// QuickJS's ceiling, not an allocation of host thread stack. The caller
    /// must provide more remaining host stack plus Rust/bridge headroom.
    pub stack_bytes: usize,
    pub pattern_units: usize,
    pub input_units: usize,
    pub output_units: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            memory_bytes: 64 * 1024 * 1024,
            stack_bytes: 512 * 1024,
            pattern_units: 4096,
            input_units: 1024 * 1024,
            output_units: 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Setup,
    Bridge,
    Compile,
    Match,
    Decode,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    InvalidLimits,
    Limit(&'static str),
    BudgetExhausted,
    Cancelled,
    /// Native errors, including syntax/stack/allocation failure, never mean no match.
    Engine {
        phase: Phase,
        name: String,
        detail: String,
    },
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}

type Captures = Vec<Option<Vec<u16>>>;
type Groups = BTreeMap<String, Option<Vec<u16>>>;
type Ranges = Vec<Option<[usize; 2]>>;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Indices {
    pub captures: Ranges,
    pub groups: Option<BTreeMap<String, Option<[usize; 2]>>>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    pub range: [usize; 2],
    pub captures: Captures,
    pub groups: Option<Groups>,
    pub indices: Option<Indices>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub matched: Option<Match>,
    pub last_index: usize,
}

/// A token belongs to exactly one request; retaining it cannot cancel its successor.
#[derive(Clone, Default)]
pub struct Cancellation(Arc<AtomicBool>);
impl Cancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
}
struct Budget {
    max_polls: u64,
    polls: AtomicU64,
    exhausted: AtomicBool,
    active: AtomicBool,
    cancellation: Cancellation,
    #[cfg(test)]
    cancel_at_poll: AtomicU64,
    #[cfg(test)]
    cancel_at_decode_unit: AtomicU64,
}
impl Budget {
    fn check(&self) -> Result<(), Error> {
        if self.cancellation.0.load(Ordering::SeqCst) {
            return Err(Error::Cancelled);
        }
        if self.max_polls == 0 || self.exhausted.load(Ordering::SeqCst) {
            return Err(Error::BudgetExhausted);
        }
        Ok(())
    }
    fn interrupt(&self) -> bool {
        if !self.active.load(Ordering::SeqCst) {
            return false;
        }
        let polls = self.polls.fetch_add(1, Ordering::SeqCst) + 1;
        #[cfg(test)]
        if self.cancel_at_poll.load(Ordering::SeqCst) == polls {
            self.cancellation.cancel();
        }
        if polls > self.max_polls {
            self.exhausted.store(true, Ordering::SeqCst);
        }
        self.check().is_err()
    }
}

/// Own and use on one thread. Create independent engines inside concurrent threads.
/// No cache, worker queue, async task or JS object escapes this type.
pub struct Engine {
    context: Context,
    runtime: Runtime,
    limits: Limits,
}
impl Engine {
    pub fn new(limits: Limits) -> Result<Self, Error> {
        // QuickJS treats memory=0 / stack=0 or stack>16MiB as unlimited.
        // Keep initialization above the tiny-memory partial-intrinsic boundary.
        if limits.memory_bytes < 1024 * 1024
            || !(16 * 1024..=16 * 1024 * 1024).contains(&limits.stack_bytes)
            || limits.pattern_units == 0
            || limits.input_units == 0
            || limits.output_units == 0
        {
            return Err(Error::InvalidLimits);
        }
        let runtime = Runtime::new().map_err(|e| plain_error(Phase::Setup, e))?;
        runtime.set_memory_limit(limits.memory_bytes);
        runtime.set_max_stack_size(limits.stack_bytes);
        let context = Context::custom::<(intrinsic::RegExpCompiler, intrinsic::RegExp)>(&runtime)
            .map_err(|e| plain_error(Phase::Setup, e))?;
        context.with(|ctx| {
            let _: Constructor = ctx
                .globals()
                .get("RegExp")
                .map_err(|e| engine_error(&ctx, Phase::Setup, e))?;
            let _: Constructor = ctx
                .globals()
                .get("String")
                .map_err(|e| engine_error(&ctx, Phase::Setup, e))?;
            Ok::<_, Error>(())
        })?;
        Ok(Self {
            context,
            runtime,
            limits,
        })
    }

    /// Polls count only native RegExp.exec execution, shared across calls in this
    /// request. Exactly max_polls polls are allowed; the next poll aborts.
    /// This is cooperative cancellation, not a time or backtrack count.
    pub fn request(&mut self, max_polls: u64) -> Request<'_> {
        let budget = Arc::new(Budget {
            max_polls,
            polls: AtomicU64::new(0),
            exhausted: AtomicBool::new(false),
            active: AtomicBool::new(false),
            cancellation: Cancellation::default(),
            #[cfg(test)]
            cancel_at_poll: AtomicU64::new(0),
            #[cfg(test)]
            cancel_at_decode_unit: AtomicU64::new(0),
        });
        let handler = budget.clone();
        self.runtime
            .set_interrupt_handler(Some(Box::new(move || handler.interrupt())));
        Request {
            engine: self,
            budget,
        }
    }
}

pub struct Request<'a> {
    engine: &'a mut Engine,
    budget: Arc<Budget>,
}
impl Request<'_> {
    pub fn cancellation(&self) -> Cancellation {
        self.budget.cancellation.clone()
    }
    pub fn polls(&self) -> u64 {
        self.budget.polls.load(Ordering::SeqCst)
    }

    /// `last_index` has JS lastIndex semantics: only g/y use it; u/v may
    /// back up from the middle of a surrogate pair. Never slice the input.
    pub fn exec(
        &mut self,
        pattern: &[u16],
        flags: &str,
        input: &[u16],
        last_index: u32,
    ) -> Result<Outcome, Error> {
        self.budget.check()?;
        let limits = self.engine.limits;
        if pattern.len() > limits.pattern_units {
            return Err(Error::Limit("pattern"));
        }
        if input.len() > limits.input_units {
            return Err(Error::Limit("input"));
        }
        if flags.len() > 8 {
            return Err(Error::Limit("flags"));
        }
        self.engine.context.with(|ctx| {
            let pattern =
                from_units(&ctx, pattern).map_err(|e| engine_error(&ctx, Phase::Bridge, e))?;
            let input =
                from_units(&ctx, input).map_err(|e| engine_error(&ctx, Phase::Bridge, e))?;
            let ctor: Constructor = ctx
                .globals()
                .get("RegExp")
                .map_err(|e| engine_error(&ctx, Phase::Compile, e))?;
            let regexp: Object = ctor
                .construct((pattern, flags))
                .map_err(|e| engine_error(&ctx, Phase::Compile, e))?;
            regexp
                .set("lastIndex", last_index)
                .map_err(|e| engine_error(&ctx, Phase::Compile, e))?;
            let exec: Function = regexp
                .get("exec")
                .map_err(|e| engine_error(&ctx, Phase::Match, e))?;
            self.budget.check()?;
            // Compilation and UTF-16 conversion have completed. An interrupt
            // here is evidence of the native regex matcher, not a JS bridge loop.
            self.budget.active.store(true, Ordering::SeqCst);
            let result: rquickjs::Result<Option<Array>> = exec.call((This(regexp.clone()), input));
            self.budget.active.store(false, Ordering::SeqCst);
            let result = result.map_err(|e| engine_error(&ctx, Phase::Match, e));
            self.budget.check()?;
            let result = result?;
            let last_index = regexp
                .get("lastIndex")
                .map_err(|e| engine_error(&ctx, Phase::Decode, e))?;
            let matched = result
                .map(|array| decode_match(&ctx, array, limits.output_units, &self.budget))
                .transpose()?;
            // Cancellation during UTF-16 decoding must win over a successful
            // outcome too. This is not a latency bound on native calls.
            self.budget.check()?;
            Ok(Outcome {
                matched,
                last_index,
            })
        })
    }
}
impl Drop for Request<'_> {
    fn drop(&mut self) {
        self.budget.active.store(false, Ordering::SeqCst);
        self.engine.runtime.set_interrupt_handler(None);
    }
}

fn plain_error(phase: Phase, error: rquickjs::Error) -> Error {
    Error::Engine {
        phase,
        name: "EngineError".into(),
        detail: error.to_string(),
    }
}
fn engine_error(ctx: &Ctx<'_>, phase: Phase, error: rquickjs::Error) -> Error {
    let exception = ctx.catch(); // Clear the pending exception, including interruption.
    if let Some(object) = exception.as_object() {
        Error::Engine {
            phase,
            name: object.get("name").unwrap_or_else(|_| "EngineError".into()),
            detail: object.get("message").unwrap_or_else(|_| error.to_string()),
        }
    } else {
        plain_error(phase, error)
    }
}

fn from_units<'js>(ctx: &Ctx<'js>, units: &[u16]) -> rquickjs::Result<rquickjs::String<'js>> {
    let string: Object = ctx.globals().get("String")?;
    let from_char_code: Function = string.get("fromCharCode")?;
    let chunks = Array::new(ctx.clone())?;
    for (index, chunk) in units.chunks(512).enumerate() {
        let mut args = Args::new(ctx.clone(), chunk.len());
        args.push_args(chunk.iter().copied())?;
        let part: rquickjs::String = from_char_code.call_arg(args)?;
        chunks.set(index, part)?;
    }
    // One join, not repeated concatenation (quadratic copying on large input).
    let join: Function = chunks.as_object().get("join")?;
    join.call((This(chunks), ""))
}
fn to_units<'js>(
    ctx: &Ctx<'js>,
    value: Value<'js>,
    remaining: &mut usize,
    _budget: &Budget,
) -> Result<Option<Vec<u16>>, Error> {
    if value.is_undefined() {
        return Ok(None);
    }
    let read = || -> rquickjs::Result<(Object<'_>, usize, Function<'_>)> {
        let ctor: Constructor = ctx.globals().get("String")?;
        let boxed: Object = ctor.construct((value,))?;
        let len: usize = boxed.get("length")?;
        let at: Function = boxed.get("charCodeAt")?;
        Ok((boxed, len, at))
    };
    let (boxed, len, at) = read().map_err(|e| engine_error(ctx, Phase::Decode, e))?;
    *remaining = remaining.checked_sub(len).ok_or(Error::Limit("output"))?;
    let mut units = Vec::with_capacity(len);
    for index in 0..len {
        units.push(
            at.call((This(boxed.clone()), index))
                .map_err(|e| engine_error(ctx, Phase::Decode, e))?,
        );
        #[cfg(test)]
        if _budget.cancel_at_decode_unit.load(Ordering::SeqCst) == index as u64 + 1 {
            _budget.cancellation.cancel();
        }
    }
    Ok(Some(units))
}
fn decode_match<'js>(
    ctx: &Ctx<'js>,
    array: Array<'js>,
    mut remaining: usize,
    budget: &Budget,
) -> Result<Match, Error> {
    let read = |error| engine_error(ctx, Phase::Decode, error);
    let start: usize = array.as_object().get("index").map_err(read)?;
    let mut captures = Vec::with_capacity(array.len());
    for index in 0..array.len() {
        captures.push(to_units(
            ctx,
            array.get(index).map_err(read)?,
            &mut remaining,
            budget,
        )?);
    }
    let end = start
        + captures
            .first()
            .and_then(Option::as_ref)
            .map_or(0, Vec::len);
    let groups = array
        .as_object()
        .get::<_, Option<Object>>("groups")
        .map_err(read)?
        .map(|groups| {
            groups
                .props::<String, Value>()
                .map(|entry| {
                    let (key, value) = entry.map_err(read)?;
                    Ok((key, to_units(ctx, value, &mut remaining, budget)?))
                })
                .collect::<Result<Groups, Error>>()
        })
        .transpose()?;
    let indices = array
        .as_object()
        .get::<_, Option<Array>>("indices")
        .map_err(read)?
        .map(|indices| {
            let captures = indices
                .iter::<Option<Array>>()
                .map(|a| index_range(a?))
                .collect::<rquickjs::Result<Ranges>>()
                .map_err(read)?;
            let groups = indices
                .as_object()
                .get::<_, Option<Object>>("groups")
                .map_err(read)?
                .map(|groups| {
                    groups
                        .props::<String, Option<Array>>()
                        .map(|entry| {
                            let (key, value) = entry?;
                            Ok((key, index_range(value)?))
                        })
                        .collect::<rquickjs::Result<BTreeMap<_, _>>>()
                })
                .transpose()
                .map_err(read)?;
            Ok::<_, Error>(Indices { captures, groups })
        })
        .transpose()?;
    Ok(Match {
        range: [start, end],
        captures,
        groups,
        indices,
    })
}

fn index_range(array: Option<Array<'_>>) -> rquickjs::Result<Option<[usize; 2]>> {
    array.map(|a| Ok([a.get(0)?, a.get(1)?])).transpose()
}

#[cfg(test)]
mod tests;
