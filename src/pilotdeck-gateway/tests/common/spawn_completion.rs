//! Shared deterministic lifecycle matrix for #808's two completion owners.
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::{
    runtime::{Builder, Runtime},
    sync::oneshot,
};
pub type Work = Pin<Box<dyn Future<Output = Result<(), ()>> + Send>>;
pub type Wait = Pin<Box<dyn Future<Output = Result<String, String>> + Send>>;
pub trait Port {
    const SCOPE: &'static str;
    const PANIC: &'static str;
    fn new() -> Self;
    fn begin(&self, work: Work) -> Wait;
    fn snapshot(&mut self);
    fn subscribe(&self, repeated: bool) -> Wait;
    fn cached(&self) -> bool;
    fn published(&self) -> bool;
}
pub fn runtime() -> Runtime {
    Builder::new_current_thread().enable_all().build().unwrap()
}
pub fn pending<F: Future + ?Sized>(future: Pin<&mut F>) {
    assert!(
        future
            .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
            .is_pending()
    );
}
pub async fn bounded<F: Future>(future: F) -> F::Output {
    tokio::time::timeout(std::time::Duration::from_secs(2), future)
        .await
        .expect("completion deadline")
}
pub fn matrix<P: Port>(dropped: bool) {
    let raw = include_str!("../fixtures/mcp-hooks-spawn/oracle.json");
    crate::test_baseline::oracle(raw);
    let oracle: serde_json::Value = serde_json::from_str(raw).unwrap();
    let cases: Vec<_> = oracle["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|case| case["scope"] == P::SCOPE)
        .collect();
    assert_eq!(cases.len(), 4, "complete TS matrix");
    let modes: &[&str] = if dropped {
        &["drop-first", "drop-gated"]
    } else {
        &["panic-first", "panic-gated", "error", "success"]
    };
    for (index, &mode) in modes.iter().enumerate() {
        let owner = runtime();
        let mut port = P::new();
        let calls = Arc::new(AtomicU64::new(0));
        let count = calls.clone();
        let (release, gate) = oneshot::channel();
        let (entered, started) = oneshot::channel();
        let mut first = {
            let _enter = owner.enter();
            port.begin(Box::pin(async move {
                count.fetch_add(1, Ordering::SeqCst);
                entered.send(()).unwrap();
                if mode != "panic-first" {
                    gate.await.unwrap();
                }
                match mode {
                    "panic-first" | "panic-gated" => panic!("injected spawn panic"),
                    "error" => Err(()),
                    _ => Ok(()),
                }
            }))
        };
        let mut second;
        {
            let _enter = owner.enter();
            pending(first.as_mut());
            port.snapshot();
            second = port.subscribe(true);
            pending(second.as_mut());
            let mut cancelled = port.subscribe(true);
            pending(cancelled.as_mut());
            drop(cancelled);
        }
        if mode != "drop-first" {
            owner.block_on(bounded(started)).unwrap();
        }
        let executor = if dropped {
            drop(owner);
            runtime()
        } else {
            release.send(()).ok();
            owner
        };
        executor.block_on(async {
            for (i, waiter) in [first, second].into_iter().enumerate() {
                let result = bounded(waiter).await;
                if mode != "success" {
                    assert_eq!(
                        result.as_ref().unwrap_err(),
                        if mode == "error" {
                            "ordinary failure"
                        } else {
                            P::PANIC
                        }
                    );
                }
                if !dropped {
                    assert_eq!(cases[index]["mode"], mode);
                    assert_eq!(
                        cases[index]["values"][i],
                        result.as_deref().unwrap_or("ordinary failure"),
                        "TS outcome"
                    );
                }
            }
            assert!(port.published(), "guard publishes an outcome");
            assert_eq!(
                port.cached(),
                !dropped && cases[index]["cached"].as_bool().unwrap(),
                "failure clears cached attempt"
            );
            let count = calls.clone();
            let retry = bounded(port.begin(Box::pin(async move {
                count.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })))
            .await
            .unwrap();
            if !dropped {
                assert_eq!(cases[index]["retry"], retry);
                assert_eq!(cases[index]["calls"], calls.load(Ordering::SeqCst));
            }
        });
    }
}
