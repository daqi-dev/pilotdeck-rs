//! Node timer normalization and fetch event ordering shared by web, MCP and providers.
use http_body_util::BodyExt;
use std::{future::Future, pin::Pin, task::Poll, time::Duration};
use tokio::{sync::watch, time::Instant};

/// Long transport timeout installed by TS `cli/proxy.ts` globally.
pub const UNDICI_BODY_TIMEOUT: Duration = Duration::from_millis(600_000);
pub const UNDICI_BODY_TIMEOUT_MS: f64 = UNDICI_BODY_TIMEOUT.as_millis() as f64;

/// Store this host-owned signal in ToolRuntimeContext.abort_signal. No timers
/// or polling tasks are needed; watch retains cancellation before subscription.
#[derive(Clone, Debug)]
pub struct Cancellation(watch::Sender<Option<AbortEvent>>);

#[derive(Clone, Copy, Debug)]
struct AbortEvent {
    at: Instant,
    synchronous: bool,
}

impl Default for Cancellation {
    fn default() -> Self {
        Self(watch::channel(None).0)
    }
}
impl Cancellation {
    pub fn new() -> Self {
        Self::default()
    }
    /// JS `AbortController.abort()` in the current call stack.
    pub fn abort(&self) {
        self.cancel_synchronously();
    }
    pub fn is_aborted(&self) -> bool {
        self.is_cancelled()
    }

    /// Record the first cancellation at the runtime clock's current instant.
    /// Consumers can retain earlier events even if they are polled late.
    pub fn cancel(&self) {
        self.cancel_at(Instant::now());
    }
    /// Deliver a timer/event cancellation with its original occurrence time.
    /// The event must already have occurred; this does not schedule a task.
    /// Delivery time does not imply registration order: a caller can arm its
    /// parent timer before starting the exchange, then deliver both overdue.
    pub fn cancel_at(&self, at: Instant) {
        assert!(
            at <= Instant::now(),
            "cancellation event is still in the future"
        );
        self.deliver(AbortEvent {
            at,
            synchronous: false,
        });
    }
    /// JS `controller.abort()` in the current call stack, before control is
    /// returned to the event loop. Unlike a later timer callback, this has
    /// already fired even when earlier timers are overdue but undispatched.
    pub fn cancel_synchronously(&self) {
        self.deliver(AbortEvent {
            at: Instant::now(),
            synchronous: true,
        });
    }
    fn deliver(&self, event: AbortEvent) {
        self.0.send_if_modified(|current| {
            if current.is_some() {
                return false;
            }
            *current = Some(event);
            true
        });
    }
    pub fn cancelled_at(&self) -> Option<Instant> {
        self.0.borrow().map(|event| event.at)
    }
    pub(crate) fn abort_event_at(&self) -> Option<Instant> {
        self.0
            .borrow()
            .filter(|event| !event.synchronous)
            .map(|event| event.at)
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.borrow().is_some()
    }
    pub fn same_channel(&self, other: &Self) -> bool {
        self.0.same_channel(&other.0)
    }
    pub async fn cancelled(&self) {
        let mut receiver = self.0.subscribe();
        while receiver.borrow_and_update().is_none() {
            if receiver.changed().await.is_err() {
                return;
            }
        }
    }
}

/// The delay Node's `setTimeout` actually arms for a requested millisecond
/// value: values inside `[1, 2^31-1]` are truncated to integer milliseconds;
/// everything else — `NaN`, `±Infinity`, values below 1 and values
/// above `2^31-1` — becomes 1 ms (the `TimeoutOverflowWarning` path).
/// Node validates the original value before `insert()` truncates it, so
/// `2147483647.5` becomes 1, not `2147483647`. `_idleTimeout` alone still
/// contains the fraction and does not describe the scheduled timer list.
///
/// Both `networkFetch`'s per-attempt timer and the providers' outer abort
/// timer go through this, so an overflowing `timeoutMs` never reaches
/// `Duration::from_secs_f64`, which panics on non-finite or overflowing
/// input.
pub fn node_timer_delay_ms(ms: f64) -> f64 {
    if ms.is_finite() && (1.0..=i32::MAX as f64).contains(&ms) {
        ms.trunc()
    } else {
        1.0
    }
}

/// [`node_timer_delay_ms`] as a `Duration` for `tokio::time`.
pub fn node_timer_duration(ms: f64) -> Duration {
    Duration::from_millis(node_timer_delay_ms(ms) as u64)
}

/// An exchange with an observable completion source. A timer-backed transport
/// must use `after`, rather than hide a sleep in an opaque future: consumers
/// racing an abort timer need both deadlines when the executor resumes late.
/// Ordinary I/O is dispatched when polled; an already fulfilled exchange is a
/// microtask (`ready`). Dropping any variant also drops its body/connection.
pub struct ExchangeFuture<T> {
    future: Pin<Box<dyn Future<Output = T> + Send>>,
    timer: Option<Pin<Box<tokio::time::Sleep>>>,
    microtask: bool,
}

impl<T: Send + 'static> ExchangeFuture<T> {
    /// Socket I/O, without an application-level delayed completion.
    pub fn io(future: impl Future<Output = T> + Send + 'static) -> Self {
        Self {
            future: Box::pin(future),
            timer: None,
            microtask: false,
        }
    }

    pub fn ready(result: T) -> Self {
        Self::microtask(std::future::ready(result))
    }

    /// A promise continuation dispatched before the next timer callback.
    /// This must not hide a timer or socket wait; those have separate event
    /// sources. It may yield while its queued continuation is being delivered.
    pub fn microtask(future: impl Future<Output = T> + Send + 'static) -> Self {
        Self {
            future: Box::pin(future),
            timer: None,
            microtask: true,
        }
    }

    /// A timer callback that settles the exchange. Keep the timer here so
    /// callers can order it against their own timers, including overdue ones.
    pub fn after(delay: Duration, result: T) -> Self {
        Self {
            future: Box::pin(std::future::ready(result)),
            timer: Some(Box::pin(tokio::time::sleep(delay))),
            microtask: false,
        }
    }

    pub fn scheduled_at(&self) -> Option<tokio::time::Instant> {
        self.timer.as_ref().map(|timer| timer.deadline())
    }

    pub fn is_microtask(&self) -> bool {
        self.microtask
    }
}

impl<T> Future for ExchangeFuture<T> {
    type Output = T;

    fn poll(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        if let Some(timer) = &mut self.timer {
            std::task::ready!(timer.as_mut().poll(cx));
        }
        self.future.as_mut().poll(cx)
    }
}

pub(crate) enum Exchange<T> {
    Response(T, bool),
    Aborted,
    Timeout { before_abort: bool },
}

/// `delay(ms, parentSignal)`, optionally bounded by a surrounding timer.
/// An already registered retry timer runs its continuation before a later
/// parent event (including ties), even if both are overdue when polled.
/// Synchronous aborts have already fired and take precedence over that timer.
/// As with `exchange`, the response flag preserves the winning continuation
/// through the next attempt's parent-cancellation precheck.
pub(crate) async fn delay(
    duration: Duration,
    abort: impl Future<Output = ()>,
    parent_event_at: impl Fn() -> Option<Instant>,
    outer_deadline: Option<Instant>,
    continuation_before_abort: bool,
) -> Exchange<()> {
    let retry_at = Instant::now() + duration;
    let outer_first = outer_deadline.is_some_and(|at| at <= retry_at);
    let next = outer_deadline.map_or(retry_at, |at| at.min(retry_at));
    let mut abort = Box::pin(abort);
    let mut timer = Box::pin(tokio::time::sleep_until(next));
    let mut initial = true;
    std::future::poll_fn(|cx| {
        let aborted = abort.as_mut().poll(cx).is_ready();
        if initial && !continuation_before_abort && aborted {
            return Poll::Ready(Exchange::Aborted);
        }
        initial = false;
        let timer_first =
            !aborted || parent_event_at().is_some_and(|at| timer_precedes_parent(next, at, false));
        if timer_first && timer.as_mut().poll(cx).is_ready() {
            return Poll::Ready(if outer_first {
                Exchange::Timeout {
                    before_abort: aborted,
                }
            } else {
                Exchange::Response((), aborted)
            });
        }
        if aborted {
            return Poll::Ready(Exchange::Aborted);
        }
        Poll::Pending
    })
    .await
}

/// Dispatch pending events as Node does: synchronous parent aborts are already
/// delivered; timestamped parent events follow earlier response/timeout timers.
/// The timeout is registered before the response, and the parent event after
/// both, which determines ties for this entry. A queued promise continuation runs before
/// timers. Socket readiness is polled after due timers in this dispatcher;
/// transports must distinguish it from a queued continuation.
/// `ExchangeFuture::after` makes a delayed response's deadline observable.
pub(crate) async fn exchange<T: Send + 'static>(
    send: ExchangeFuture<T>,
    abort: impl Future<Output = ()>,
    parent_event_at: impl Fn() -> Option<Instant>,
    deadline: Option<Instant>,
) -> Exchange<T> {
    exchange_continuing(send, abort, parent_event_at, deadline, false).await
}

/// Provider/count callers can arm their parent cancellation timer before
/// entering the exchange. Node runs that timer first on an exact deadline tie.
/// Keep this registration order explicit; web/MCP use the existing entry above.
pub(crate) async fn exchange_with_prearmed_parent<T: Send + 'static>(
    send: ExchangeFuture<T>,
    abort: impl Future<Output = ()>,
    parent_event_at: impl Fn() -> Option<Instant>,
    deadline: Option<Instant>,
) -> Exchange<T> {
    exchange_prearmed_continuing(send, abort, parent_event_at, deadline, false).await
}

pub(crate) async fn exchange_prearmed_continuing<T: Send + 'static>(
    send: ExchangeFuture<T>,
    abort: impl Future<Output = ()>,
    parent_event_at: impl Fn() -> Option<Instant>,
    deadline: Option<Instant>,
    continuation_before_abort: bool,
) -> Exchange<T> {
    let order = DispatchOrder {
        scheduled_at: send.scheduled_at(),
        microtask: send.is_microtask(),
        continuation_before_abort,
        parent_first: true,
    };
    dispatch(send, abort, parent_event_at, deadline, order).await
}

/// Carry an earlier timer's queued continuation into the next exchange. A
/// pending first poll yields to cancellation, exactly as for a body frame.
pub(crate) async fn exchange_continuing<T: Send + 'static>(
    send: ExchangeFuture<T>,
    abort: impl Future<Output = ()>,
    parent_event_at: impl Fn() -> Option<Instant>,
    deadline: Option<Instant>,
    continuation_before_abort: bool,
) -> Exchange<T> {
    let scheduled_at = send.scheduled_at();
    let microtask = send.is_microtask();
    dispatch(
        send,
        abort,
        parent_event_at,
        deadline,
        DispatchOrder {
            scheduled_at,
            microtask,
            continuation_before_abort,
            parent_first: false,
        },
    )
    .await
}

/// A borrowed frame uses the same event dispatcher as headers. The continuation
/// gets one poll before a later abort; pending I/O relinquishes that privilege.
/// Every call arms a fresh idle deadline, rather than a total body deadline.
pub(crate) async fn body_frame<B: hyper::body::Body + Unpin>(
    body: &mut B,
    idle: Option<Duration>,
    signal: Option<&Cancellation>,
    continuation: &mut bool,
) -> Exchange<Option<Result<hyper::body::Frame<B::Data>, B::Error>>> {
    body_frame_with_abort(
        body,
        idle,
        async {
            match signal {
                Some(signal) => signal.cancelled().await,
                None => std::future::pending().await,
            }
        },
        || signal.and_then(Cancellation::abort_event_at),
        continuation,
    )
    .await
}

pub(crate) async fn body_frame_with_abort<B: hyper::body::Body + Unpin>(
    body: &mut B,
    idle: Option<Duration>,
    abort: impl Future<Output = ()>,
    parent_event_at: impl Fn() -> Option<Instant>,
    continuation: &mut bool,
) -> Exchange<Option<Result<hyper::body::Frame<B::Data>, B::Error>>> {
    body_frame_ordered(body, idle, abort, parent_event_at, continuation, false).await
}

pub(crate) async fn body_frame_with_prearmed_parent<B: hyper::body::Body + Unpin>(
    body: &mut B,
    idle: Option<Duration>,
    signal: Option<&Cancellation>,
    continuation: &mut bool,
) -> Exchange<Option<Result<hyper::body::Frame<B::Data>, B::Error>>> {
    body_frame_ordered(
        body,
        idle,
        async {
            match signal {
                Some(signal) => signal.cancelled().await,
                None => std::future::pending().await,
            }
        },
        || signal.and_then(Cancellation::abort_event_at),
        continuation,
        true,
    )
    .await
}

async fn body_frame_ordered<B: hyper::body::Body + Unpin>(
    body: &mut B,
    idle: Option<Duration>,
    abort: impl Future<Output = ()>,
    parent_event_at: impl Fn() -> Option<Instant>,
    continuation: &mut bool,
    parent_first: bool,
) -> Exchange<Option<Result<hyper::body::Frame<B::Data>, B::Error>>> {
    let deadline = idle.map(|idle| Instant::now() + idle);
    let next = dispatch(
        body.frame(),
        abort,
        &parent_event_at,
        deadline,
        DispatchOrder {
            scheduled_at: None,
            microtask: false,
            continuation_before_abort: std::mem::take(continuation),
            parent_first,
        },
    )
    .await;
    *continuation = match &next {
        Exchange::Response(_, before_abort) | Exchange::Timeout { before_abort } => *before_abort,
        Exchange::Aborted => false,
    };
    next
}

/// Node drains a timer callback's promise continuations before the next
/// callback. Equal deadlines follow registration order, never poll order.
fn timer_precedes_parent(event_at: Instant, parent_at: Instant, parent_first: bool) -> bool {
    event_at < parent_at || (!parent_first && event_at == parent_at)
}

struct DispatchOrder {
    scheduled_at: Option<Instant>,
    microtask: bool,
    continuation_before_abort: bool,
    parent_first: bool,
}

async fn dispatch<T>(
    send: impl Future<Output = T>,
    abort: impl Future<Output = ()>,
    parent_event_at: impl Fn() -> Option<Instant>,
    deadline: Option<Instant>,
    order: DispatchOrder,
) -> Exchange<T> {
    let DispatchOrder {
        scheduled_at,
        microtask,
        mut continuation_before_abort,
        parent_first,
    } = order;
    let mut send = std::pin::pin!(send);
    let mut abort = Box::pin(abort);
    let mut timer = deadline.map(|at| Box::pin(tokio::time::sleep_until(at)));
    let mut initial = true;
    std::future::poll_fn(|cx| {
        // A fulfilled promise's queued continuation cannot be rejected by a
        // later abort, even a synchronous one. `microtask` must not hide an
        // undispatched socket/timer: those still lose to synchronous abort.
        if microtask {
            let result = std::task::ready!(send.as_mut().poll(cx));
            return Poll::Ready(Exchange::Response(
                result,
                abort.as_mut().poll(cx).is_ready() && parent_event_at().is_some(),
            ));
        }
        if std::mem::take(&mut continuation_before_abort)
            && let Poll::Ready(result) = send.as_mut().poll(cx)
        {
            return Poll::Ready(Exchange::Response(
                result,
                abort.as_mut().poll(cx).is_ready() && parent_event_at().is_some(),
            ));
        }
        let aborted = abort.as_mut().poll(cx).is_ready();
        let parent_at = aborted.then(&parent_event_at).flatten();
        if aborted && parent_at.is_none() {
            return Poll::Ready(Exchange::Aborted);
        }
        let response_before_timeout = match (scheduled_at, deadline) {
            (Some(response_at), Some(abort_at)) => response_at < abort_at,
            (None, Some(_)) => initial || microtask,
            (_, None) => true,
        };
        let response_before_parent = parent_at.is_none_or(|at| {
            scheduled_at.map_or_else(
                || microtask,
                |response_at| timer_precedes_parent(response_at, at, parent_first),
            )
        });
        let response_first = response_before_timeout && response_before_parent;
        initial = false;
        if response_first && let Poll::Ready(result) = send.as_mut().poll(cx) {
            return Poll::Ready(Exchange::Response(result, aborted));
        }
        let timeout_before_parent = parent_at.is_none_or(|at| {
            deadline.is_some_and(|deadline| timer_precedes_parent(deadline, at, parent_first))
        });
        if timeout_before_parent
            && let Some(timer) = &mut timer
            && timer.as_mut().poll(cx).is_ready()
        {
            return Poll::Ready(Exchange::Timeout {
                before_abort: aborted && timeout_before_parent,
            });
        }
        if aborted {
            return Poll::Ready(Exchange::Aborted);
        }
        if !response_first && let Poll::Ready(result) = send.as_mut().poll(cx) {
            return Poll::Ready(Exchange::Response(result, false));
        }
        Poll::Pending
    })
    .await
}
