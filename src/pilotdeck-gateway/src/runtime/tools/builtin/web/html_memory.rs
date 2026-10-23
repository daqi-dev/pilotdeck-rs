//! Weighted admission for web_fetch HTML conversion only.
use crate::{
    host::{HostError, ModuleError},
    runtime::model::cpu,
};
use std::sync::{Arc, LazyLock};
use tokio::sync::Semaphore;

const MIB: usize = 1024 * 1024;
// #454, human-approved option C (2026-10-23): pr9c's worst measured ratio
// (including full conversion) was 1,234,714,624 B / 2,627,690 input bytes
// = 469.89 B/byte. Round up to 512 (~9% headroom, ~33% over the worst
// bounded conversion), admitting at most 4 GiB of estimates at once. This is
// an admission estimate, not a hard RSS cap; one oversized page runs alone.
const BUDGET_MIB: u32 = 4096;
const RSS_PER_INPUT_BYTE: usize = 512;

static MEMORY: LazyLock<HtmlMemory> =
    LazyLock::new(|| HtmlMemory::new(BUDGET_MIB, RSS_PER_INPUT_BYTE));

pub(super) async fn run<T, F>(html_bytes: usize, work: F) -> Result<T, HostError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, HostError> + Send + 'static,
{
    MEMORY.run(html_bytes, work).await
}

struct HtmlMemory {
    slots: Arc<Semaphore>,
    budget_mib: u32,
    rss_per_input_byte: usize,
}

impl HtmlMemory {
    fn new(budget_mib: u32, rss_per_input_byte: usize) -> Self {
        assert!(budget_mib > 0 && rss_per_input_byte > 0);
        Self {
            slots: Arc::new(Semaphore::new(budget_mib as usize)),
            budget_mib,
            rss_per_input_byte,
        }
    }

    fn weight(&self, html_bytes: usize) -> u32 {
        html_bytes
            .saturating_mul(self.rss_per_input_byte)
            .div_ceil(MIB)
            .clamp(1, self.budget_mib as usize) as u32
    }

    async fn run<T, F>(&self, html_bytes: usize, work: F) -> Result<T, HostError>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T, HostError> + Send + 'static,
    {
        // Always memory -> CPU. No CPU caller acquires this private budget,
        // so HTML waiting for memory cannot occupy the tokenizer/JSON slots.
        let permit = self
            .slots
            .clone()
            .acquire_many_owned(self.weight(html_bytes))
            .await
            .map_err(|error| ModuleError::new(error.to_string()))?;
        cpu::run(move || {
            // Cancellation while queued drops the acquisition/closure. Once
            // blocking work starts it cannot be aborted: keep its reservation
            // until conversion (including DOM destruction) actually finishes.
            let _permit = permit;
            work()
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        future::Future,
        pin::Pin,
        task::{Context, Poll, Waker},
    };
    use tokio::sync::oneshot;

    // Tests that reserve the process-wide budget must not inspect each other's
    // permits. This lock is test-only; injected-budget tests remain parallel.
    static GLOBAL_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    // Polling once deterministically registers a weighted waiter. No timing
    // assumptions, real large pages, or multi-GB allocations in these tests.
    fn poll_once<T>(future: Pin<&mut impl Future<Output = T>>) -> Poll<T> {
        future.poll(&mut Context::from_waker(Waker::noop()))
    }

    // Keep polling the CPU admission future while waiting for the blocking
    // closure to start: another test/runtime may currently own all CPU slots.
    async fn wait_until_started<T>(
        future: Pin<&mut impl Future<Output = T>>,
        running: oneshot::Receiver<()>,
    ) {
        tokio::select! {
            signal = running => signal.unwrap(),
            _ = future => panic!("gated work finished before release"),
        }
    }

    #[test]
    fn estimates_round_up_saturate_and_use_measured_headroom() {
        let memory = HtmlMemory::new(BUDGET_MIB, RSS_PER_INPUT_BYTE);
        assert_eq!(memory.weight(0), 1);
        assert_eq!(memory.weight(1), 1);
        assert_eq!(memory.weight(MIB / 512 + 1), 2);
        assert_eq!(memory.weight(1024 * 1024), 512);
        assert_eq!(memory.weight(10 * MIB), BUDGET_MIB);
        assert_eq!(memory.weight(usize::MAX), BUDGET_MIB);
    }

    #[tokio::test]
    async fn concurrent_estimated_memory_never_exceeds_budget() {
        let memory = HtmlMemory::new(10, 2);
        let (release, blocked) = std::sync::mpsc::channel();
        let (started, running) = oneshot::channel();
        let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let first_active = active.clone();
        let mut first = Box::pin(memory.run(3 * MIB, move || {
            first_active.fetch_add(6, std::sync::atomic::Ordering::SeqCst);
            started.send(()).unwrap();
            blocked.recv().unwrap();
            first_active.fetch_sub(6, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }));
        assert!(poll_once(first.as_mut()).is_pending());
        wait_until_started(first.as_mut(), running).await;
        assert_eq!(memory.slots.available_permits(), 4);
        // Reserve another four units without awaiting a second CPU job while
        // the first is held. Other tests may themselves occupy the CPU pool.
        let (fit_release, fit_blocked) = std::sync::mpsc::channel();
        let fitting_active = active.clone();
        let mut fitting = Box::pin(memory.run(2 * MIB, move || {
            assert!(fitting_active.fetch_add(4, std::sync::atomic::Ordering::SeqCst) + 4 <= 10);
            fit_blocked.recv().unwrap();
            fitting_active.fetch_sub(4, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }));
        assert!(poll_once(fitting.as_mut()).is_pending());
        assert_eq!(memory.slots.available_permits(), 0);
        let starts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut waiters = (0..8)
            .map(|_| {
                let starts = starts.clone();
                let active = active.clone();
                Box::pin(memory.run(3 * MIB, move || {
                    assert!(active.fetch_add(6, std::sync::atomic::Ordering::SeqCst) + 6 <= 10);
                    starts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    active.fetch_sub(6, std::sync::atomic::Ordering::SeqCst);
                    Ok(())
                }))
            })
            .collect::<Vec<_>>();
        for waiter in &mut waiters {
            assert!(poll_once(waiter.as_mut()).is_pending());
        }
        // Each needs six units; none may start while the first owns six.
        assert_eq!(starts.load(std::sync::atomic::Ordering::SeqCst), 0);
        release.send(()).unwrap();
        fit_release.send(()).unwrap();
        first.await.unwrap();
        fitting.await.unwrap();
        for waiter in waiters {
            waiter.await.unwrap();
        }
        assert_eq!(memory.slots.available_permits(), 10);
        assert_eq!(starts.load(std::sync::atomic::Ordering::SeqCst), 8);
        assert_eq!(active.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn oversized_conversion_runs_exclusively_and_completes() {
        let memory = HtmlMemory::new(10, 2);
        let held = memory.slots.clone().acquire_owned().await.unwrap();
        let (release, blocked) = std::sync::mpsc::channel();
        let (started, running) = oneshot::channel();
        let mut large = Box::pin(memory.run(usize::MAX, move || {
            started.send(()).unwrap();
            blocked.recv().unwrap();
            Ok("converted")
        }));
        assert!(poll_once(large.as_mut()).is_pending());
        drop(held);
        assert!(poll_once(large.as_mut()).is_pending());
        wait_until_started(large.as_mut(), running).await;
        assert_eq!(memory.slots.available_permits(), 0);
        let mut small = Box::pin(memory.run(1, || Ok("next")));
        assert!(poll_once(small.as_mut()).is_pending());
        release.send(()).unwrap();
        assert_eq!(large.await.unwrap(), "converted");
        assert_eq!(small.await.unwrap(), "next");
        assert_eq!(memory.slots.available_permits(), 10);
    }

    #[tokio::test]
    async fn cancelling_partial_weighted_waiter_returns_reserved_units() {
        let memory = HtmlMemory::new(10, 1);
        let held = memory.slots.clone().acquire_many_owned(6).await.unwrap();
        let mut queued = Box::pin(memory.run(8 * MIB, || -> Result<(), HostError> {
            panic!("cancelled work ran")
        }));
        assert!(poll_once(queued.as_mut()).is_pending());
        // Tokio reserves the four free units for this eight-unit waiter.
        let mut next = Box::pin(memory.run(4 * MIB, || Ok("unblocked")));
        assert!(poll_once(next.as_mut()).is_pending());
        drop(queued);
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(5), next)
                .await
                .expect("cancelled waiter retained memory")
                .unwrap(),
            "unblocked"
        );
        assert_eq!(memory.slots.available_permits(), 4);
        drop(held);
        assert_eq!(memory.slots.available_permits(), 10);
    }

    #[tokio::test]
    async fn cancellation_keeps_running_conversion_reserved() {
        let memory = HtmlMemory::new(10, 1);
        let (release, blocked) = std::sync::mpsc::channel();
        let (started, running) = oneshot::channel();
        let (finished, done) = oneshot::channel();
        let mut call = Box::pin(memory.run(10 * MIB, move || {
            started.send(()).unwrap();
            blocked.recv().unwrap();
            finished.send(()).unwrap();
            Ok(())
        }));
        assert!(poll_once(call.as_mut()).is_pending());
        wait_until_started(call.as_mut(), running).await;
        drop(call);
        assert_eq!(memory.slots.available_permits(), 0);
        let mut next = Box::pin(memory.run(1, || Ok(())));
        assert!(poll_once(next.as_mut()).is_pending());
        release.send(()).unwrap();
        done.await.unwrap();
        next.await.unwrap();
        assert_eq!(memory.slots.available_permits(), 10);
    }

    #[tokio::test]
    async fn conversion_error_returns_budget_and_preserves_error() {
        let memory = HtmlMemory::new(10, 1);
        let result: Result<(), HostError> = memory
            .run(10 * MIB, || {
                Err(ModuleError::new("conversion failed").into())
            })
            .await;
        assert_eq!(result.unwrap_err().message, "conversion failed");
        assert_eq!(memory.slots.available_permits(), 10);
    }

    #[tokio::test]
    async fn production_entry_shares_budget_without_blocking_other_cpu_callers() {
        let _guard = GLOBAL_TEST_LOCK.lock().await;
        let (release, blocked) = std::sync::mpsc::channel();
        let (started, running) = oneshot::channel();
        let mut first = Box::pin(run(10 * MIB, move || {
            started.send(()).unwrap();
            blocked.recv().unwrap();
            Ok(())
        }));
        assert!(poll_once(first.as_mut()).is_pending());
        wait_until_started(first.as_mut(), running).await;
        // Independent calls through the production entry share the singleton.
        // An unrelated CPU job must still run while HTML waits for memory.
        let mut next = Box::pin(run(10 * MIB, || Ok(())));
        assert!(poll_once(next.as_mut()).is_pending());
        assert_eq!(MEMORY.slots.available_permits(), 0);
        release.send(()).unwrap();
        first.await.unwrap();
        next.await.unwrap();
        assert_eq!(MEMORY.slots.available_permits(), BUDGET_MIB as usize);
        // Hold only memory here, so unrelated tests can still obtain all four
        // CPU permits without deadlocking against a blocked test closure.
        let held = MEMORY
            .slots
            .clone()
            .acquire_many_owned(BUDGET_MIB)
            .await
            .unwrap();
        let mut waiting = Box::pin(run(10 * MIB, || Ok(())));
        assert!(poll_once(waiting.as_mut()).is_pending());
        assert_eq!(
            cpu::run(|| Ok("tokenizer/JSON")).await.unwrap(),
            "tokenizer/JSON"
        );
        drop(held);
        waiting.await.unwrap();
    }

    #[tokio::test]
    async fn fetcher_html_conversion_waits_for_global_memory_and_preserves_ts_output() {
        use super::super::{
            cache::UrlCache,
            fetcher::{FetchResult, HtmlToMarkdown, MarkdownOutput, UrlFetcher},
            html_markdown::Turndown,
        };
        use crate::runtime::model::http::{ModelHttpTransport, TransportFuture};
        use bytes::Bytes;
        use http_body_util::{BodyExt, Full};
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct ConversionTransport(String);
        impl ModelHttpTransport for ConversionTransport {
            fn send(&self, _: hyper::Request<Full<Bytes>>) -> TransportFuture {
                TransportFuture::ready(Ok(hyper::Response::builder()
                    .status(200)
                    .header("content-type", "text/html")
                    .body(
                        Full::new(Bytes::from(self.0.clone()))
                            .map_err(|never| match never {})
                            .boxed(),
                    )
                    .unwrap()))
            }
        }

        struct ObservedTurndown(AtomicUsize);
        impl HtmlToMarkdown for ObservedTurndown {
            fn convert(&self, html: &str) -> Result<String, String> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Turndown.convert(html)
            }

            fn convert_bounded(&self, html: &str, units: usize) -> Result<MarkdownOutput, String> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Turndown.convert_bounded(html, units)
            }
        }

        let _guard = GLOBAL_TEST_LOCK.lock().await;
        let raw = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/html-markdown/depth-oracle.json"
        ));
        crate::test_baseline::oracle(raw);
        let oracle: serde_json::Value = serde_json::from_str(raw).unwrap();
        let case = oracle["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|case| case["name"] == "ul-255")
            .unwrap();
        let html = case["html"].as_str().unwrap();
        let expected = case["markdown"].as_str().unwrap();
        assert_eq!(MEMORY.weight(html.len()), 2);
        let converter = Arc::new(ObservedTurndown(AtomicUsize::new(0)));
        let mut fetcher = UrlFetcher::new(Arc::new(ConversionTransport(html.into())));
        fetcher.cache = Arc::new(UrlCache::default());
        fetcher.html = converter.clone();
        let mut held = MEMORY
            .slots
            .clone()
            .acquire_many_owned(BUDGET_MIB)
            .await
            .unwrap();
        let url = "https://example.com/memory-wiring";
        let mut pending = Box::pin(fetcher.get_url_markdown_content(url, None));
        // The ready transport and Full body reach memory admission in this
        // poll. No timer, socket, spawned fetch task or scheduler delay.
        assert!(poll_once(pending.as_mut()).is_pending());
        assert_eq!(converter.0.load(Ordering::SeqCst), 0);
        assert!(fetcher.cache.get(url).is_none());

        // Explicit handoff: the HTML waiter must reserve this one unit even
        // though it still needs a second. A bypass to cpu::run leaves it free,
        // making this assertion fail regardless of blocking-thread scheduling.
        drop(held.split(1).unwrap());
        assert_eq!(
            MEMORY.slots.available_permits(),
            0,
            "fetcher did not queue HTML conversion on the global memory budget"
        );
        assert!(poll_once(pending.as_mut()).is_pending());
        assert_eq!(converter.0.load(Ordering::SeqCst), 0);
        drop(held);

        let FetchResult::Fetched { entry, from_cache } = pending.await.unwrap() else {
            panic!("expected converted HTML");
        };
        assert!(!from_cache);
        assert_eq!(entry.content.as_bytes(), expected.as_bytes());
        assert_eq!(entry.bytes, html.len());
        assert_eq!(
            entry.raw_markdown_length,
            Some(expected.encode_utf16().count())
        );
        assert_eq!(converter.0.load(Ordering::SeqCst), 1);
        assert_eq!(fetcher.cache.get(url), Some(entry));
        assert_eq!(MEMORY.slots.available_permits(), BUDGET_MIB as usize);
    }
}
