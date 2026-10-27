# EdgeClaw LLM HTTP fixture (#763)

`generate.mts` directly imports `LlmMemoryExtractor` from the read-only
`cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85` source tree. `bindSourceTree` and
`sourceClosureSha256` verify the imported closure. Node **v22.22.0** is required.
The generator creates the output directory recursively and opens oracle.json
with `wx`; it never replaces a golden file. The Rust ignored regeneration test
runs this same generator and compares every byte.

```sh
export PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck
LANG=en_US.UTF-8 node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-llm/verify.mts
scripts/cargo-slots.sh
export CARGO_TARGET_DIR=/Users/da/ws/pilotdeck-rs/_build/wt-763/_build/cargo-target
export CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_PROFILE_TEST_DEBUG=line-tables-only
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway \
  --test edgeclaw_llm_transport -- --include-ignored
```

## Recorded contract

270 cells cover attempt 0/1/2, chat response_format/fallback and responses without
response_format, all eight retry statuses, four nonretry statuses, retry/nonretry
network messages, invalid JSON, all four output extractors, HTTP header case and
ByteString rules, Google paths/model aliases, API key/URL failures and debug.
The 237 ordinary cases replay against a real Tokio+Hyper listener on 127.0.0.1:0.
Only pre-connect failure injection returns a TypeError without contacting the
listener, matching the shared `ModelHttpTransport` failure contract. Auth and
parse callback errors remain Error instances. No provider or .env.local is used.

`requests` contains method, path+query, **all** lower-case incoming HTTP headers,
UTF-8 body bytes represented as a string, and virtual backoff time. Only random
host/port is replaced with `$HOST`/`$ORIGIN`; body text is not normalized. Header
value bytes use Latin-1 as Node IncomingMessage does. Rust compares body strings
exactly; object key order in the outer observation envelope is immaterial.
`fetches` additionally records the TS fetch input before the implicit Node
headers; these are diagnostic, while `requests` is the wire assertion. Invalid
URLs can enter TS fetch and fail before transport, so those cells compare the
error/debug and assert zero Rust transport sends rather than equating the two
counts.

The generator isolates HOME, freezes Date.now, and supplies the deterministic
Math.random sequence `[0.25]` (repeated). Only timers with the llm-extraction
source callsite in their stack are controlled; undici's own timers stay real.
Every case has a 5 s watchdog. The stub owns all sockets and closes them in
finally. Pending observations are snapshotted **before** forced cleanup.

## M6 timing and lifecycle

| Cells | Hook and termination | Rust test |
|---|---|---|
| Ordinary 237 | 5 s watchdog; real listener; injected sleep records 1000/2000 ms | wire_matrix |
| Header timeout 20 | Shared transport remains pending; Tokio virtual clock dispatches configured timer; drop counted | timeout_matrix |
| timeout <=0, auth never completes, held sleep 6 | Observe pending after advancing 100 s, then release or drop; no successor after cancellation | pending_and_cancellation_matrix |
| Body hang 4 | TS own timer was cleared, remains pending beyond requested deadline; close local sockets after snapshot | body_deadline_matrix |
| Response at 5/7/8 ms, deadline 7 ms, late poll 3 | Timestamp-ordered controlled fetch; Rust ExchangeFuture::after, next poll at 10 ms | timer_order_matrix |
| Real D-2 socket | Await response headers, advance virtual 30 s; await existing socket close **before** server shutdown | real_body_deadline_closes_loopback_connection |

TS has no caller cancellation parameter: dropping a Rust future is an additional
resource-ownership test, not a claim that TS offers the same cancellation API.
The finite pending observation does not claim to prove infinite wall-clock
nontermination; it proves the configured deadline no longer applies after
headers. D-2 is registered in
`specs/gateway-parity/diffs/A6.6-EDGECLAW-LLM-BODY-DEADLINE-001.md`.

The oracle retains the TS `model_request`, `model_response`, and trackError
observations (including model_request before missing API key). **#868 connects
the Rust telemetry callbacks** and compares the complete event arrays, including
auth order, terminal request failures and all six label cells. The four D-2 body
hang cells compare telemetry while pending, before the Rust-only body deadline;
their final results remain the registered D-2 difference. Dedicated mutation
assertions and the post-commit runner are in `../edgeclaw-llm-telemetry/` and
`../../edgeclaw_llm_telemetry.rs`. The source oracle remains unchanged.

## Negative controls and remaining transport prerequisite

`negative.py --check-anchors` checks every anchor exactly once. Mutations run
inside `negative_process.mutating_sources`; cargo uses `run_owned`,
`wait_for_slot`, and the caller's mandatory CARGO_TARGET_DIR. Compiler failures, wrong tests, missing panic blocks and unrelated assertion
messages are INVALID, never KILLED. Every mutant binds the full test name and
its specific assertion line; before/restored positives also require that name. Each mutation has before/restored positives. Stale
anchors exit 2. New files are not in HEAD before the coordinator commits them;
the source guard intentionally refuses to run here. Run the script after commit,
with an exclusive evidence directory, and review KILLED/restored logs before
acceptance. It must never be run in a shadow repository or alternate source tree.

变异表采用 `(name, path, before, after, test)`，供 main 的
`anchor_selfcheck.py` 在不导入执行器的前提下检查全部源码锚点。
执行器也直接使用同一行的 path，不再另设路径覆盖表；全部源码变异均参与静态检查。

`test_controls.py` 只测执行器框架（内存源码字符串和模拟的 cargo 返回值），
不修改产品源码。该命名将框架单测与检查器按 `*negative*.py` 发现的源码变异脚本
区分开，同时保留标准 `unittest` 发现能力。运行方式：

```sh
python3 -m unittest discover -s src/pilotdeck-gateway/tests/fixtures/edgeclaw-llm -p 'test_*.py' -v
```

The existing shared ModelHttpTransport returns encoded response bytes unchanged.
This package sends Node's default accept-encoding header for wire parity but does
not implement gzip/deflate/br decompression. A real TS loopback gzip probe succeeds;
the corresponding Rust body needs the shared transport follow-up. This is an
explicit prerequisite before enabling this extractor for providers that return
compressed responses. `model/http.rs` is read-only in this task, so that fix is
not bundled here. No dependency or lockfile changed.

## fix-r1 回归

头部构造/发送分层、逐跳编码、底层 300/600 秒截止与迟到 poll 的补充真实 TS 金样及运行方法见 `../edgeclaw-llm-fix-r1/README.md`。原 270 格保持原字节；负控锚点已同步新实现。
