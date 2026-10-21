//! Node timer normalization and fetch event ordering shared by web, MCP and providers.
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
    /// At equal deadlines, web exchanges dispatch their already registered
    /// response/timeout timers before this subsequently delivered event.
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
    continuation_before_abort: bool,
}

impl<T: Send + 'static> ExchangeFuture<T> {
    /// Socket I/O, without an application-level delayed completion.
    pub fn io(future: impl Future<Output = T> + Send + 'static) -> Self {
        Self {
            future: Box::pin(future),
            timer: None,
            microtask: false,
            continuation_before_abort: false,
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
            continuation_before_abort: false,
        }
    }

    /// A timer callback that settles the exchange. Keep the timer here so
    /// callers can order it against their own timers, including overdue ones.
    pub fn after(delay: Duration, result: T) -> Self {
        Self {
            future: Box::pin(std::future::ready(result)),
            timer: Some(Box::pin(tokio::time::sleep(delay))),
            microtask: false,
            continuation_before_abort: false,
        }
    }

    /// Continue an earlier winning event's queued work before a later abort.
    /// Unlike a fulfilled promise, this gets just one poll: if that work needs
    /// more I/O or a timer, it must observe cancellation normally.
    pub(crate) fn continuing(mut self, before_abort: bool) -> Self {
        self.continuation_before_abort = before_abort;
        self
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
    Timeout,
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
        let timer_first = !aborted || parent_event_at().is_some_and(|at| next <= at);
        if timer_first && timer.as_mut().poll(cx).is_ready() {
            return Poll::Ready(if outer_first {
                Exchange::Timeout
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
/// The timeout was registered before the response, and the parent event after
/// both, which also determines ties. A queued promise continuation runs before
/// timers. Socket readiness is polled after due timers in this dispatcher;
/// transports must distinguish it from a queued continuation.
/// `ExchangeFuture::after` makes a delayed response's deadline observable.
pub(crate) async fn exchange<T: Send + 'static>(
    mut send: ExchangeFuture<T>,
    abort: impl Future<Output = ()>,
    parent_event_at: impl Fn() -> Option<Instant>,
    deadline: Option<Instant>,
) -> Exchange<T> {
    let mut abort = Box::pin(abort);
    let mut timer = deadline.map(|at| Box::pin(tokio::time::sleep_until(at)));
    let mut initial = true;
    std::future::poll_fn(|cx| {
        // A fulfilled promise's queued continuation cannot be rejected by a
        // later abort, even a synchronous one. `microtask` must not hide an
        // undispatched socket/timer: those still lose to synchronous abort.
        if send.is_microtask() {
            let result = std::task::ready!(Pin::new(&mut send).poll(cx));
            return Poll::Ready(Exchange::Response(
                result,
                abort.as_mut().poll(cx).is_ready(),
            ));
        }
        if std::mem::take(&mut send.continuation_before_abort)
            && let Poll::Ready(result) = Pin::new(&mut send).poll(cx)
        {
            return Poll::Ready(Exchange::Response(
                result,
                abort.as_mut().poll(cx).is_ready(),
            ));
        }
        let aborted = abort.as_mut().poll(cx).is_ready();
        let parent_at = aborted.then(&parent_event_at).flatten();
        if aborted && parent_at.is_none() {
            return Poll::Ready(Exchange::Aborted);
        }
        let response_before_timeout = match (send.scheduled_at(), deadline) {
            (Some(response_at), Some(abort_at)) => response_at < abort_at,
            (None, Some(_)) => initial || send.is_microtask(),
            (_, None) => true,
        };
        let response_before_parent = parent_at.is_none_or(|at| {
            send.scheduled_at()
                .map_or_else(|| send.is_microtask(), |response_at| response_at <= at)
        });
        let response_first = response_before_timeout && response_before_parent;
        initial = false;
        if response_first && let Poll::Ready(result) = Pin::new(&mut send).poll(cx) {
            return Poll::Ready(Exchange::Response(result, aborted));
        }
        if parent_at.is_none_or(|at| deadline.is_some_and(|deadline| deadline <= at))
            && let Some(timer) = &mut timer
            && timer.as_mut().poll(cx).is_ready()
        {
            return Poll::Ready(Exchange::Timeout);
        }
        if aborted {
            return Poll::Ready(Exchange::Aborted);
        }
        if !response_first && let Poll::Ready(result) = Pin::new(&mut send).poll(cx) {
            return Poll::Ready(Exchange::Response(result, false));
        }
        Poll::Pending
    })
    .await
}
