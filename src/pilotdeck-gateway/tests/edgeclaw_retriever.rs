//! #767 real TS oracle: `ReasoningRetriever` over a TS-written workspace.
//!
//! `generate.mts` drives the fixed cc7c4e6 `ReasoningRetriever` over a real
//! `MemoryRepository` (file-backed workspace + global root) and a loopback
//! LLM stub, then records the file tree, every result/trace/debug/stats and
//! the request bytes. This replay materializes the same files, opens the same
//! Rust stores and compares each step byte-for-byte.
#[path = "common/baseline.rs"]
mod baseline;
#[path = "support/llm_stub.rs"]
#[allow(dead_code)]
mod llm_stub;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response};
use llm_stub::LlmStub;
use pilotdeck_gateway::runtime::{
    memory::edgeclaw::{
        EdgeClawFileStores,
        file_memory::FileMemoryClock,
        llm::transport::LlmMemoryExtractor,
        retriever::{ReasoningRetriever, RetrievalOptions},
        text::now_iso_at,
    },
    model::http::{HyperModelTransport, ModelHttpTransport, TransportFuture},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Notify, Semaphore};

const ORACLE: &str = include_str!("fixtures/edgeclaw-retriever/oracle.json");

struct Scratch(PathBuf);
impl Scratch {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("pdrs-retriever-{name}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&path).expect("#767 scratch");
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn js(value: Value) -> JsValue {
    parse_json(&value.to_string()).expect("#767 oracle JSON")
}

fn wire(value: &JsValue) -> String {
    match stringify_json(value).expect("#767 stringify_json") {
        Some(text) => text,
        None => "undefined".into(),
    }
}

fn as_json(value: &JsValue) -> Value {
    serde_json::from_str(&wire(value)).expect("#767 value is JSON")
}

fn normalized(value: &Value, origin: &str) -> Value {
    match value {
        Value::String(text) => Value::String(text.replace(origin, "$ORIGIN")),
        Value::Array(items) => {
            Value::Array(items.iter().map(|item| normalized(item, origin)).collect())
        }
        Value::Object(object) => {
            let mut out = serde_json::Map::new();
            for (key, item) in object {
                out.insert(key.clone(), normalized(item, origin));
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

fn config(origin: &str) -> JsValue {
    js(json!({
        "agents": {"defaults": {"model": {"primary": "p/m"}}},
        "models": {"providers": {"p": {
            "api": "chat",
            "baseUrl": origin,
            "apiKey": "fixture-key",
            "models": [{"id": "m"}]
        }}}
    }))
}

fn materialize(root: &Path, files: &Value) {
    for file in files.as_array().expect("#767 storage files") {
        let path = file["path"].as_str().expect("#767 storage path");
        let content = file["content"].as_str().expect("#767 storage content");
        let target = root.join(path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).expect("#767 storage parent");
        }
        fs::write(&target, content.as_bytes()).expect("#767 storage write");
    }
}

async fn open_stores(root: &Path, scenario: &Value, clock_ms: i64) -> Arc<EdgeClawFileStores> {
    let mode = scenario["mode"].as_str().expect("#767 mode");
    let memory = root.join("data").join("memory");
    let global = root.join("global");
    let workspace = if mode == "general" {
        root.join(".pilotdeck")
    } else {
        root.join("work")
    };
    let general = root.join(".pilotdeck");
    fs::create_dir_all(root.join("data")).expect("#767 data dir");
    fs::create_dir_all(&memory).expect("#767 memory dir");
    fs::create_dir_all(&global).expect("#767 global dir");
    fs::create_dir_all(&workspace).expect("#767 workspace dir");
    materialize(&memory, &scenario["storage"]["memoryFiles"]);
    materialize(&global, &scenario["storage"]["globalFiles"]);
    let clock: FileMemoryClock = Arc::new(move || now_iso_at(clock_ms));
    Arc::new(
        EdgeClawFileStores::with_workspace_clock(
            root.join("data").join("memory.db"),
            &memory,
            &global,
            &workspace,
            &general,
            clock,
        )
        .expect("#767 open real storage"),
    )
}

fn options(op: &Value) -> RetrievalOptions {
    let mut options = RetrievalOptions::default();
    let input = op.get("input").unwrap_or(&Value::Null);
    if let Some(messages) = input.get("recentMessages").and_then(Value::as_array) {
        options.recent_messages = messages
            .iter()
            .map(|message| {
                let object = js(message.clone());
                object
                    .as_object()
                    .cloned()
                    .expect("#767 recent message object")
            })
            .collect();
    }
    if let Some(hint) = input.get("workspaceHint").and_then(Value::as_str) {
        options.workspace_hint = JsString::from(hint);
    }
    if let Some(mode) = input.get("retrievalMode").and_then(Value::as_str) {
        options.retrieval_mode = Some(JsString::from(mode));
    }
    options
}

#[tokio::test]
async fn replay_scenarios() {
    let oracle_value = baseline::oracle(ORACLE);
    let _ = oracle_value;
    let oracle: Value = serde_json::from_str(ORACLE).expect("#767 oracle JSON");
    let clock_ms = oracle["clockMs"].as_i64().expect("#767 clockMs");
    let token = JsString::from(oracle["randomToken"].as_str().expect("#767 randomToken"));
    let scenarios = oracle["scenarios"].as_array().expect("#767 scenarios");
    let expected_ops = scenarios
        .iter()
        .flat_map(|scenario| scenario["operations"].as_array().expect("#767 operations"))
        .filter(|op| {
            matches!(
                op["kind"].as_str(),
                Some("retrieve" | "retrieveContext" | "search")
            )
        })
        .count();
    let mut replay_count = 0;
    for scenario in scenarios {
        let name = scenario["name"].as_str().expect("#767 scenario name");
        let scratch = Scratch::new(name);
        let stores = open_stores(&scratch.0, scenario, clock_ms).await;
        let steps = scenario["steps"].as_array().expect("#767 steps").to_vec();
        let clock = Arc::new(AtomicU64::new(clock_ms as u64));
        let server = LlmStub::start(steps, clock.clone()).await;
        let _closed = server.closed.clone();
        let extractor = LlmMemoryExtractor::new(
            config(&server.origin),
            Arc::new(HyperModelTransport::new()),
            Arc::new(()),
        );
        let retriever_clock = clock.clone();
        let scenario_token = token.clone();
        let retriever = ReasoningRetriever::with_dependencies(
            stores.clone(),
            Arc::new(extractor),
            Arc::new(move || retriever_clock.load(Ordering::SeqCst) as i64),
            Arc::new(move || scenario_token.clone()),
        );
        let operations = scenario["operations"].as_array().expect("#767 operations");
        for (index, op) in operations.iter().enumerate() {
            let kind = op["kind"].as_str().expect("#767 operation kind");
            if kind == "advance" {
                clock.fetch_add(
                    op["ms"].as_u64().expect("#767 advance ms"),
                    Ordering::SeqCst,
                );
                continue;
            }
            if kind == "reset" {
                retriever.reset_transient_state();
                continue;
            }
            let query = JsString::from(op["query"].as_str().expect("#767 query"));
            let before = server.requests.lock().expect("#767 stub requests").len();
            let options = options(op);
            let actual = tokio::time::timeout(Duration::from_secs(30), async {
                match kind {
                    "retrieve" => retriever.retrieve(&query, &options).await,
                    "retrieveContext" => retriever.retrieve_context(&query, &options).await,
                    "search" => retriever.search(&query, &options).await,
                    other => panic!("#767 unknown operation {other}"),
                }
            })
            .await
            .unwrap_or_else(|_| panic!("#767 {name}/{index} timed out"))
            .unwrap_or_else(|error| panic!("#767 {name}/{index} retrieve failed: {error}"));
            assert_eq!(
                as_json(&JsValue::Object(actual)),
                op["result"],
                "#767 result {name}/{index} ({kind})"
            );
            let stats = retriever.get_runtime_stats();
            assert_eq!(
                as_json(&JsValue::Object(stats)),
                op["stats"],
                "#767 stats {name}/{index} ({kind})"
            );
            let actual_requests = {
                let requests = server.requests.lock().expect("#767 stub request lock");
                serde_json::to_value(requests.iter().skip(before).collect::<Vec<_>>())
                    .expect("#767 requests JSON")
            };
            let expected_requests = op["requests"].clone();
            assert_eq!(
                normalized(&actual_requests, &server.origin),
                expected_requests,
                "#767 requests {name}/{index} ({kind})"
            );
            replay_count += 1;
        }
        stores.close().expect("#767 close stores");
        server.close().await;
    }
    assert_eq!(
        replay_count, expected_ops,
        "#767 replayed every oracle retrieval operation"
    );
}

const MATRIX_CLOCK: i64 = 1_792_972_800_000;

fn matrix_oracle() -> Value {
    let _ = baseline::oracle(ORACLE);
    serde_json::from_str(ORACLE).expect("#767 oracle JSON")
}

fn empty_scenario() -> Value {
    json!({"mode": "single", "storage": {"memoryFiles": [], "globalFiles": []}})
}

async fn matrix_store(name: &str) -> (Scratch, Arc<EdgeClawFileStores>) {
    let scratch = Scratch::new(name);
    let root = scratch.0.join("empty");
    let stores = open_stores(&root, &empty_scenario(), MATRIX_CLOCK).await;
    (scratch, stores)
}

fn user_message(content: &str) -> JsObject {
    js(json!({"role": "user", "content": content}))
        .as_object()
        .cloned()
        .expect("#767 user message object")
}

fn assert_matrix_stats(stats: &JsObject, cell: &Value, keys: &[&str], label: &str) {
    for key in keys {
        assert_eq!(
            stats.get(key),
            Some(&js(cell["stats"][*key].clone())),
            "#767 {label} stats {key}"
        );
    }
}

const ALL_STATS: &[&str] = &[
    "lastRecallMs",
    "recallTimeouts",
    "lastRecallMode",
    "lastRecallPath",
    "lastRecallInjected",
    "lastRecallCacheHit",
];

/// M10-1/2/5/6/9: empty miss, normalized hit, different key, reset.
#[tokio::test]
async fn m10_cache_miss_hit_and_reset() {
    let oracle = matrix_oracle();
    let matrix = oracle["matrix"].as_array().expect("#767 matrix");
    let cell = |name: &str| {
        matrix
            .iter()
            .find(|cell| cell["name"] == name)
            .unwrap_or_else(|| panic!("#767 missing matrix cell {name}"))
    };
    let (_scratch, stores) = matrix_store("m10-cache").await;
    let clock = Arc::new(AtomicU64::new(MATRIX_CLOCK as u64));
    let transport = Arc::new(GatedTransport::open(vec!["none", "none", "none"]));
    let retriever = matrix_retriever(&stores, &transport, clock);
    assert_eq!(
        as_json(&JsValue::Object(retriever.get_runtime_stats())),
        json!({
            "lastRecallMs": 0,
            "recallTimeouts": 0,
            "lastRecallMode": "none",
            "lastRecallPath": "explicit",
            "lastRecallInjected": false,
            "lastRecallCacheHit": false
        }),
        "#767 initial runtime stats"
    );
    let first = retriever
        .retrieve(
            &JsString::from("Hello  World"),
            &RetrievalOptions::default(),
        )
        .await
        .expect("#767 empty miss");
    assert_eq!(
        first
            .get("debug")
            .and_then(JsValue::as_object)
            .and_then(|debug| debug.get("cacheHit")),
        Some(&JsValue::Bool(false)),
        "#767 M10-1 empty miss"
    );
    assert_matrix_stats(
        &retriever.get_runtime_stats(),
        cell("empty-miss"),
        ALL_STATS,
        "#767 M10-1",
    );
    let hit = retriever
        .retrieve(&JsString::from("hello world"), &RetrievalOptions::default())
        .await
        .expect("#767 normalized hit");
    assert_eq!(
        hit.get("debug")
            .and_then(JsValue::as_object)
            .and_then(|debug| debug.get("cacheHit")),
        Some(&JsValue::Bool(true)),
        "#767 M10-2 normalized hit"
    );
    assert_eq!(
        hit.get("debug")
            .and_then(JsValue::as_object)
            .and_then(|debug| debug.get("elapsedMs")),
        Some(&JsValue::Number(0.0)),
        "#767 M10-2 hit elapsedMs is zero"
    );
    assert_matrix_stats(
        &retriever.get_runtime_stats(),
        cell("normalized-hit"),
        ALL_STATS,
        "#767 M10-2",
    );
    let other = retriever
        .retrieve(&JsString::from("other"), &RetrievalOptions::default())
        .await
        .expect("#767 different key");
    assert_eq!(
        other
            .get("debug")
            .and_then(JsValue::as_object)
            .and_then(|debug| debug.get("cacheHit")),
        Some(&JsValue::Bool(false)),
        "#767 M10-6 different key"
    );
    assert_matrix_stats(
        &retriever.get_runtime_stats(),
        cell("different-key-miss"),
        ALL_STATS,
        "#767 M10-6",
    );
    let stats_before_reset = retriever.get_runtime_stats();
    retriever.reset_transient_state();
    assert_eq!(
        retriever.get_runtime_stats(),
        stats_before_reset,
        "#767 M10-5 reset leaves stats untouched"
    );
    let after_reset = retriever
        .retrieve(&JsString::from("hello world"), &RetrievalOptions::default())
        .await
        .expect("#767 reset clears");
    assert_eq!(
        after_reset
            .get("debug")
            .and_then(JsValue::as_object)
            .and_then(|debug| debug.get("cacheHit")),
        Some(&JsValue::Bool(false)),
        "#767 M10-5 reset clears"
    );
    assert_matrix_stats(
        &retriever.get_runtime_stats(),
        cell("reset-clears"),
        ALL_STATS,
        "#767 M10-5",
    );
    assert_eq!(transport.count(), 3, "#767 M10-1/2/5/6 model request count");
    stores.close().expect("#767 close matrix stores");
}

/// M10: the cache key includes the retained recent user-message contents.
#[tokio::test]
async fn m10_recent_key_miss() {
    let (_scratch, stores) = matrix_store("m10-recent").await;
    let clock = Arc::new(AtomicU64::new(MATRIX_CLOCK as u64));
    let transport = Arc::new(GatedTransport::open(vec!["none", "none"]));
    let retriever = matrix_retriever(&stores, &transport, clock);
    let query = JsString::from("recent");
    let plain = retriever
        .retrieve(&query, &RetrievalOptions::default())
        .await
        .expect("#767 recent plain");
    assert_eq!(
        plain
            .get("debug")
            .and_then(JsValue::as_object)
            .and_then(|debug| debug.get("cacheHit")),
        Some(&JsValue::Bool(false)),
        "#767 M10 recent plain miss"
    );
    let with_recent = RetrievalOptions {
        recent_messages: vec![user_message("message one")],
        ..RetrievalOptions::default()
    };
    let keyed = retriever
        .retrieve(&query, &with_recent)
        .await
        .expect("#767 recent keyed");
    assert_eq!(
        keyed
            .get("debug")
            .and_then(JsValue::as_object)
            .and_then(|debug| debug.get("cacheHit")),
        Some(&JsValue::Bool(false)),
        "#767 M10 recent content is part of the cache key"
    );
    let hit = retriever
        .retrieve(&query, &with_recent)
        .await
        .expect("#767 recent hit");
    assert_eq!(
        hit.get("debug")
            .and_then(JsValue::as_object)
            .and_then(|debug| debug.get("cacheHit")),
        Some(&JsValue::Bool(true)),
        "#767 M10 same recent content hits"
    );
    assert_eq!(
        transport.count(),
        2,
        "#767 M10 recent key model request count"
    );
    stores.close().expect("#767 close matrix stores");
}

/// M10: the cache key includes the repository snapshot version.
#[tokio::test]
async fn m10_snapshot_key_miss() {
    let (_scratch, stores) = matrix_store("m10-snapshot").await;
    let clock = Arc::new(AtomicU64::new(MATRIX_CLOCK as u64));
    let transport = Arc::new(GatedTransport::open(vec!["none", "none"]));
    let retriever = matrix_retriever(&stores, &transport, clock);
    let query = JsString::from("snapshot");
    let options = RetrievalOptions::default();
    let first = retriever
        .retrieve(&query, &options)
        .await
        .expect("#767 snapshot setup");
    assert_eq!(
        first
            .get("debug")
            .and_then(JsValue::as_object)
            .and_then(|debug| debug.get("cacheHit")),
        Some(&JsValue::Bool(false)),
        "#767 M10 snapshot setup miss"
    );
    let hit = retriever
        .retrieve(&query, &options)
        .await
        .expect("#767 snapshot hit");
    assert_eq!(
        hit.get("debug")
            .and_then(JsValue::as_object)
            .and_then(|debug| debug.get("cacheHit")),
        Some(&JsValue::Bool(true)),
        "#767 M10 snapshot unchanged hits"
    );
    let candidate = js(json!({
        "type": "project",
        "scope": "project",
        "name": "Snapshot 计划",
        "description": "Snapshot 说明",
        "capturedAt": "2026-10-20T00:00:00.000Z",
        "sourceSessionKey": "s-snapshot",
        "summary": "Snapshot 内容。"
    }))
    .as_object()
    .cloned()
    .expect("#767 snapshot candidate");
    stores
        .workspace_memory
        .upsert_candidate(&candidate)
        .expect("#767 snapshot write");
    let after = retriever
        .retrieve(&query, &options)
        .await
        .expect("#767 snapshot changed");
    assert_eq!(
        after
            .get("debug")
            .and_then(JsValue::as_object)
            .and_then(|debug| debug.get("cacheHit")),
        Some(&JsValue::Bool(false)),
        "#767 M10 snapshot is part of the cache key"
    );
    assert_eq!(
        transport.count(),
        2,
        "#767 M10 snapshot key model request count"
    );
    stores.close().expect("#767 close matrix stores");
}

/// M10-3/4/9: TTL 29_999 ms hits and the exact 30_000 ms boundary misses.
#[tokio::test]
async fn m10_ttl_boundary() {
    let oracle = matrix_oracle();
    let matrix = oracle["matrix"].as_array().expect("#767 matrix");
    let cell = |name: &str| {
        matrix
            .iter()
            .find(|cell| cell["name"] == name)
            .unwrap_or_else(|| panic!("#767 missing matrix cell {name}"))
    };
    let (_scratch, stores) = matrix_store("m10-ttl").await;
    let clock = Arc::new(AtomicU64::new(MATRIX_CLOCK as u64));
    let transport = Arc::new(GatedTransport::open(vec!["none", "none"]));
    let retriever = matrix_retriever(&stores, &transport, clock.clone());
    let first = retriever
        .retrieve(&JsString::from("ttl"), &RetrievalOptions::default())
        .await
        .expect("#767 ttl setup");
    assert_eq!(
        first
            .get("debug")
            .and_then(JsValue::as_object)
            .and_then(|debug| debug.get("cacheHit")),
        Some(&JsValue::Bool(false)),
        "#767 M10 ttl setup miss"
    );
    assert_matrix_stats(
        &retriever.get_runtime_stats(),
        cell("ttl-setup"),
        ALL_STATS,
        "#767 M10 ttl setup",
    );
    clock.fetch_add(29_999, Ordering::SeqCst);
    let hit = retriever
        .retrieve(&JsString::from("ttl"), &RetrievalOptions::default())
        .await
        .expect("#767 ttl 29999");
    assert_eq!(
        hit.get("debug")
            .and_then(JsValue::as_object)
            .and_then(|debug| debug.get("cacheHit")),
        Some(&JsValue::Bool(true)),
        "#767 M10-3 ttl 29999 hit"
    );
    assert_eq!(
        hit.get("debug")
            .and_then(JsValue::as_object)
            .and_then(|debug| debug.get("elapsedMs")),
        Some(&JsValue::Number(0.0)),
        "#767 M10-3 ttl hit elapsedMs is zero"
    );
    assert_matrix_stats(
        &retriever.get_runtime_stats(),
        cell("ttl-29999-hit"),
        ALL_STATS,
        "#767 M10-3",
    );
    clock.fetch_add(1, Ordering::SeqCst);
    let miss = retriever
        .retrieve(&JsString::from("ttl"), &RetrievalOptions::default())
        .await
        .expect("#767 ttl boundary");
    assert_eq!(
        miss.get("debug")
            .and_then(JsValue::as_object)
            .and_then(|debug| debug.get("cacheHit")),
        Some(&JsValue::Bool(false)),
        "#767 M10-4 ttl boundary miss"
    );
    assert_matrix_stats(
        &retriever.get_runtime_stats(),
        cell("ttl-boundary-miss"),
        ALL_STATS,
        "#767 M10-4",
    );
    assert_eq!(transport.count(), 2, "#767 M10-3/4 model request count");
    stores.close().expect("#767 close matrix stores");
}

/// A lone surrogate is a valid JS string code unit; trace summaries and the
/// cache key must keep it instead of round-tripping through UTF-8. The fixed
/// TS probe (`_tmp/20261026-issue-767/probe-utf16.txt`, Node v22.22.0) records
/// the same code-unit arrays for query/inputSummary/outputSummary/path.
#[tokio::test]
async fn utf16_lone_surrogate_preserved() {
    let (_scratch, stores) = matrix_store("utf16").await;
    let clock = Arc::new(AtomicU64::new(MATRIX_CLOCK as u64));
    let transport = Arc::new(GatedTransport::open(vec!["user"]));
    let retriever = matrix_retriever(&stores, &transport, clock);
    let mut query_units = "lone ".encode_utf16().collect::<Vec<_>>();
    query_units.push(0xD800);
    query_units.extend(" surrogate".encode_utf16());
    let mut mode_units = "ex".encode_utf16().collect::<Vec<_>>();
    mode_units.push(0xD800);
    mode_units.extend("plicit".encode_utf16());
    let query = JsString::from_utf16(query_units.clone());
    let options = RetrievalOptions {
        retrieval_mode: Some(JsString::from_utf16(mode_units.clone())),
        ..RetrievalOptions::default()
    };
    let result = retriever
        .retrieve(&query, &options)
        .await
        .expect("#767 UTF-16 retrieve");
    assert_eq!(
        result
            .get("query")
            .and_then(JsValue::as_string)
            .expect("#767 UTF-16 query field")
            .units(),
        query_units.as_slice(),
        "#767 UTF-16 query keeps the lone surrogate"
    );
    let trace = result
        .get("trace")
        .and_then(JsValue::as_object)
        .expect("#767 UTF-16 trace");
    let steps = match trace.get("steps") {
        Some(JsValue::Array(steps)) => steps,
        _ => panic!("#767 UTF-16 steps"),
    };
    let start = steps
        .first()
        .and_then(JsValue::as_object)
        .expect("#767 UTF-16 start step");
    assert_eq!(
        start
            .get("inputSummary")
            .and_then(JsValue::as_string)
            .expect("#767 UTF-16 input summary")
            .units(),
        query_units.as_slice(),
        "#767 UTF-16 trace input summary keeps the lone surrogate"
    );
    let mut summary_units = "mode=".encode_utf16().collect::<Vec<_>>();
    summary_units.extend_from_slice(&mode_units);
    assert_eq!(
        start
            .get("outputSummary")
            .and_then(JsValue::as_string)
            .expect("#767 UTF-16 output summary")
            .units(),
        summary_units.as_slice(),
        "#767 UTF-16 trace output summary keeps the lone surrogate"
    );
    assert_eq!(
        result
            .get("debug")
            .and_then(JsValue::as_object)
            .and_then(|debug| debug.get("path"))
            .and_then(JsValue::as_string)
            .expect("#767 UTF-16 debug path")
            .units(),
        mode_units.as_slice(),
        "#767 UTF-16 debug path keeps the lone surrogate"
    );
    let hit = retriever
        .retrieve(&query, &options)
        .await
        .expect("#767 UTF-16 cache hit");
    assert_eq!(
        hit.get("query")
            .and_then(JsValue::as_string)
            .expect("#767 UTF-16 cached query")
            .units(),
        query_units.as_slice(),
        "#767 UTF-16 cache hit keeps the lone surrogate"
    );
    assert_eq!(
        hit.get("debug")
            .and_then(JsValue::as_object)
            .and_then(|debug| debug.get("cacheHit")),
        Some(&JsValue::Bool(true)),
        "#767 UTF-16 cache hit is recorded"
    );
    assert_eq!(
        hit.get("debug")
            .and_then(JsValue::as_object)
            .and_then(|debug| debug.get("elapsedMs")),
        Some(&JsValue::Number(0.0)),
        "#767 UTF-16 cache hit elapsedMs is zero"
    );
    assert_eq!(
        transport.count(),
        1,
        "#767 UTF-16 lone surrogate does not alter the cache key"
    );
    stores.close().expect("#767 close matrix stores");
}

/// M10-7/9: reset during the LLM await still caches the pre-reset result.
#[tokio::test]
async fn m10_reset_during_await() {
    let oracle = matrix_oracle();
    let matrix = oracle["matrix"].as_array().expect("#767 matrix");
    let cell = |name: &str| {
        matrix
            .iter()
            .find(|cell| cell["name"] == name)
            .unwrap_or_else(|| panic!("#767 missing matrix cell {name}"))
    };
    let (_scratch, stores) = matrix_store("m10-reset-await").await;
    let clock = Arc::new(AtomicU64::new(MATRIX_CLOCK as u64));
    let transport = Arc::new(GatedTransport::new(vec!["user", "user"]));
    let gate = transport.gate(0);
    let later_gate = transport.gate(1);
    let retriever = matrix_retriever(&stores, &transport, clock);
    let query = JsString::from("inflight");
    let options = RetrievalOptions::default();
    let pending = retriever.retrieve(&query, &options);
    let (first, ()) = tokio::join!(pending, async {
        transport
            .wait(1, "#767 M10-7 reset-during-await request")
            .await;
        retriever.reset_transient_state();
        gate.add_permits(1);
    });
    let first = first.expect("#767 inflight result");
    assert_eq!(
        first
            .get("intent")
            .and_then(JsValue::as_string)
            .map(|value| value.to_utf8().expect("#767 intent UTF-8")),
        Some("user".to_string()),
        "#767 M10-7 inflight intent"
    );
    later_gate.add_permits(1);
    let again = retriever
        .retrieve(&query, &options)
        .await
        .expect("#767 inflight cache");
    assert_eq!(
        again
            .get("debug")
            .and_then(JsValue::as_object)
            .and_then(|debug| debug.get("cacheHit")),
        Some(&JsValue::Bool(true)),
        "#767 M10-7 reset during await still caches"
    );
    let stats = retriever.get_runtime_stats();
    assert_matrix_stats(
        &stats,
        cell("reset-during-await"),
        &[
            "lastRecallMs",
            "recallTimeouts",
            "lastRecallPath",
            "lastRecallCacheHit",
        ],
        "#767 M10-7",
    );
    assert_eq!(
        stats.get("lastRecallMode"),
        again
            .get("debug")
            .and_then(JsValue::as_object)
            .and_then(|debug| debug.get("mode")),
        "#767 M10-7 cache-hit mode follows the cached result"
    );
    assert_eq!(
        stats.get("lastRecallInjected"),
        Some(&JsValue::Bool(false)),
        "#767 M10-7 empty store injects no context"
    );
    assert_eq!(transport.count(), 1, "#767 M10-7 model request count");
    stores.close().expect("#767 close matrix stores");
}

/// A cancelled retrieve (future dropped while the LLM await is pending) must
/// not insert a cache entry or move runtime stats: this is the "前驱永不完成 /
/// 被取消" lifetime cell with an explicit termination condition (one recorded
/// request, then the gate is never released and the future is dropped).
#[tokio::test]
async fn m10_cancelled_retrieve_leaves_cache_empty() {
    let (_scratch, stores) = matrix_store("m10-cancel").await;
    let clock = Arc::new(AtomicU64::new(MATRIX_CLOCK as u64));
    let transport = Arc::new(GatedTransport::new(vec!["none", "none"]));
    let later_gate = transport.gate(1);
    let retriever = matrix_retriever(&stores, &transport, clock);
    let query = JsString::from("cancelled");
    let options = RetrievalOptions::default();
    {
        let pending = retriever.retrieve(&query, &options);
        tokio::pin!(pending);
        tokio::select! {
            result = &mut pending => panic!(
                "#767 cancelled retrieve unexpectedly completed: ok={}",
                result.is_ok()
            ),
            _ = transport.wait(1, "#767 M10-9 cancellation request") => {}
        }
    }
    assert_eq!(
        transport.count(),
        1,
        "#767 cancelled retrieve sent one request"
    );
    assert_eq!(
        as_json(&JsValue::Object(retriever.get_runtime_stats())),
        json!({
            "lastRecallMs": 0,
            "recallTimeouts": 0,
            "lastRecallMode": "none",
            "lastRecallPath": "explicit",
            "lastRecallInjected": false,
            "lastRecallCacheHit": false
        }),
        "#767 cancellation leaves runtime stats untouched"
    );
    later_gate.add_permits(1);
    let after = retriever
        .retrieve(&query, &options)
        .await
        .expect("#767 cancelled then retry");
    assert_eq!(
        after
            .get("debug")
            .and_then(JsValue::as_object)
            .and_then(|debug| debug.get("cacheHit")),
        Some(&JsValue::Bool(false)),
        "#767 cancelled retrieve did not cache"
    );
    assert_eq!(
        transport.count(),
        2,
        "#767 retry after cancellation calls the model again"
    );
    stores.close().expect("#767 close matrix stores");
}

/// M10-8/9: same-key concurrent requests both call the model. The later
/// completion writes the cache entry and the stats together, so the cached
/// route names which call finished last.
#[tokio::test]
async fn m10_same_key_concurrent() {
    let oracle = matrix_oracle();
    let matrix = oracle["matrix"].as_array().expect("#767 matrix");
    let cell = |name: &str| {
        matrix
            .iter()
            .find(|cell| cell["name"] == name)
            .unwrap_or_else(|| panic!("#767 missing matrix cell {name}"))
    };
    let route = |result: &JsObject| {
        as_json(&JsValue::Object(result.clone()))["debug"]["route"]
            .as_str()
            .map(str::to_string)
    };
    let expected = cell("same-key-concurrent");
    let (_scratch, stores) = matrix_store("m10-concurrent").await;
    let clock = Arc::new(AtomicU64::new(MATRIX_CLOCK as u64));
    let transport = Arc::new(GatedTransport::new(vec!["user", "project"]));
    let first_gate = transport.gate(0);
    let second_gate = transport.gate(1);
    let retriever = matrix_retriever(&stores, &transport, clock);
    let query = JsString::from("concurrent");
    let options = RetrievalOptions::default();
    let first_done = Notify::new();
    let c1 = async {
        let result = retriever.retrieve(&query, &options).await;
        first_done.notify_one();
        result
    };
    let c2 = retriever.retrieve(&query, &options);
    let joined = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(c1, c2, async {
            transport
                .wait(2, "#767 M10-8 same-key concurrent requests")
                .await;
            first_gate.add_permits(1);
            first_done.notified().await;
            second_gate.add_permits(1);
        })
    })
    .await
    .unwrap_or_else(|_| panic!("#767 M10-8 same-key gated scenario timed out"));
    let (c1, c2, ()) = joined;
    let c1 = c1.expect("#767 concurrent first");
    let c2 = c2.expect("#767 concurrent second");
    assert_eq!(
        route(&c1).as_deref(),
        expected["firstRoute"].as_str(),
        "#767 M10-8 first completed route"
    );
    assert_eq!(
        route(&c2).as_deref(),
        expected["secondRoute"].as_str(),
        "#767 M10-8 second completed route"
    );
    let cached = retriever
        .retrieve(&query, &options)
        .await
        .expect("#767 concurrent cache");
    assert_eq!(
        transport.count(),
        2,
        "#767 M10-8 same-key concurrent both call the model"
    );
    assert_eq!(
        cached
            .get("debug")
            .and_then(JsValue::as_object)
            .and_then(|debug| debug.get("cacheHit")),
        Some(&JsValue::Bool(true)),
        "#767 M10-8 cached after concurrent"
    );
    assert_eq!(
        route(&cached).as_deref(),
        expected["cachedRoute"].as_str(),
        "#767 M10-8 later completion wins"
    );
    assert_eq!(
        expected["cachedRoute"].as_str(),
        Some("project"),
        "#767 TS matrix later completion wins"
    );
    assert_eq!(
        expected["decides"].as_u64(),
        Some(2),
        "#767 TS matrix both concurrent calls reached the model"
    );
    assert_matrix_stats(
        &retriever.get_runtime_stats(),
        expected,
        ALL_STATS,
        "#767 M10-8",
    );
    stores.close().expect("#767 close matrix stores");
}

/// M10-9 reverse: the second-started call completes first, so the first
/// completion (`user`) writes the cache last.
#[tokio::test]
async fn m10_same_key_concurrent_reverse() {
    let oracle = matrix_oracle();
    let matrix = oracle["matrix"].as_array().expect("#767 matrix");
    let cell = |name: &str| {
        matrix
            .iter()
            .find(|cell| cell["name"] == name)
            .unwrap_or_else(|| panic!("#767 missing matrix cell {name}"))
    };
    let route = |result: &JsObject| {
        as_json(&JsValue::Object(result.clone()))["debug"]["route"]
            .as_str()
            .map(str::to_string)
    };
    let expected = cell("same-key-concurrent-reverse");
    let (_scratch, stores) = matrix_store("m10-concurrent-reverse").await;
    let clock = Arc::new(AtomicU64::new(MATRIX_CLOCK as u64));
    let transport = Arc::new(GatedTransport::new(vec!["user", "project"]));
    let first_gate = transport.gate(0);
    let second_gate = transport.gate(1);
    let retriever = matrix_retriever(&stores, &transport, clock);
    let query = JsString::from("concurrent");
    let options = RetrievalOptions::default();
    let second_done = Notify::new();
    let c1 = retriever.retrieve(&query, &options);
    let c2 = async {
        let result = retriever.retrieve(&query, &options).await;
        second_done.notify_one();
        result
    };
    let joined = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(c1, c2, async {
            transport
                .wait(2, "#767 M10-9 same-key reverse concurrent requests")
                .await;
            second_gate.add_permits(1);
            second_done.notified().await;
            first_gate.add_permits(1);
        })
    })
    .await
    .unwrap_or_else(|_| panic!("#767 M10-9 same-key reverse gated scenario timed out"));
    let (c1, c2, ()) = joined;
    let c1 = c1.expect("#767 concurrent reverse first");
    let c2 = c2.expect("#767 concurrent reverse second");
    assert_eq!(
        route(&c1).as_deref(),
        expected["firstRoute"].as_str(),
        "#767 M10-9 reverse first route"
    );
    assert_eq!(
        route(&c2).as_deref(),
        expected["secondRoute"].as_str(),
        "#767 M10-9 reverse second route"
    );
    let cached = retriever
        .retrieve(&query, &options)
        .await
        .expect("#767 concurrent reverse cache");
    assert_eq!(
        transport.count(),
        2,
        "#767 M10-9 reverse both call the model"
    );
    assert_eq!(
        cached
            .get("debug")
            .and_then(JsValue::as_object)
            .and_then(|debug| debug.get("cacheHit")),
        Some(&JsValue::Bool(true)),
        "#767 M10-9 reverse cached after concurrent"
    );
    assert_eq!(
        route(&cached).as_deref(),
        expected["cachedRoute"].as_str(),
        "#767 M10-9 reverse later completion wins"
    );
    assert_eq!(
        expected["cachedRoute"].as_str(),
        Some("user"),
        "#767 TS matrix reverse later completion wins"
    );
    assert_matrix_stats(
        &retriever.get_runtime_stats(),
        expected,
        ALL_STATS,
        "#767 M10-9",
    );
    stores.close().expect("#767 close matrix stores");
}

fn matrix_retriever(
    stores: &Arc<EdgeClawFileStores>,
    transport: &Arc<GatedTransport>,
    clock: Arc<AtomicU64>,
) -> ReasoningRetriever {
    let extractor = LlmMemoryExtractor::new(
        config("http://127.0.0.1:12345"),
        transport.clone(),
        Arc::new(()),
    );
    let retriever_clock = clock.clone();
    ReasoningRetriever::with_dependencies(
        stores.clone(),
        Arc::new(extractor),
        Arc::new(move || retriever_clock.load(Ordering::SeqCst) as i64),
        Arc::new(|| JsString::from("4fzzzxjy")),
    )
}

struct Gate {
    permit: Arc<Semaphore>,
    response: String,
}

struct GatedTransport {
    gates: Mutex<VecDeque<Gate>>,
    requests: AtomicU64,
    started: Notify,
}

impl GatedTransport {
    /// A gate per response; the caller releases each one explicitly.
    fn new(responses: Vec<&str>) -> Self {
        Self::with_permits(responses, 0)
    }
    /// Every response is immediately available (the cache/TTL cells).
    fn open(responses: Vec<&str>) -> Self {
        Self::with_permits(responses, 1)
    }
    fn with_permits(responses: Vec<&str>, permits: usize) -> Self {
        let mut gates = VecDeque::new();
        for response in responses {
            let content = format!("{{\"route\":\"{response}\"}}");
            gates.push_back(Gate {
                permit: Arc::new(Semaphore::new(permits)),
                response: serde_json::to_string(
                    &json!({"choices": [{"message": {"content": content}}]}),
                )
                .expect("#767 response body"),
            });
        }
        Self {
            gates: Mutex::new(gates),
            requests: AtomicU64::new(0),
            started: Notify::new(),
        }
    }
    fn gate(&self, index: usize) -> Arc<Semaphore> {
        let gates = self.gates.lock().expect("#767 gates");
        gates
            .iter()
            .nth(index)
            .map(|gate| gate.permit.clone())
            .expect("#767 gate index")
    }
    fn count(&self) -> u64 {
        self.requests.load(Ordering::SeqCst)
    }
    /// Bounded wait: a merged/short-circuited concurrent request must fail
    /// with the calling M10 cell's message instead of hanging the suite.
    async fn wait(&self, count: u64, label: &str) {
        let waited = tokio::time::timeout(Duration::from_secs(5), async {
            while self.count() < count {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await;
        if waited.is_err() {
            panic!(
                "{label}: gated transport timed out after 5s waiting for {count} requests; saw {}",
                self.count()
            );
        }
    }
}

impl ModelHttpTransport for GatedTransport {
    fn send(&self, request: Request<Full<Bytes>>) -> TransportFuture {
        let gate = self
            .gates
            .lock()
            .expect("#767 gates")
            .pop_front()
            .unwrap_or_else(|| {
                panic!(
                    "#767 scripted response after {}",
                    self.requests.load(Ordering::SeqCst)
                )
            });
        let permit = gate.permit;
        let response = gate.response;
        self.requests.fetch_add(1, Ordering::SeqCst);
        self.started.notify_waiters();
        TransportFuture::io(async move {
            let _request = request;
            let _ = permit.acquire().await;
            let body = Full::new(Bytes::from(response))
                .map_err(|error| match error {})
                .boxed();
            Ok(Response::builder()
                .status(200)
                .header("content-type", "application/json")
                .body(body)
                .expect("#767 response"))
        })
    }
}

#[tokio::test]
#[ignore]
async fn regenerates_identically() {
    baseline::regenerates_identically("edgeclaw-retriever");
}
