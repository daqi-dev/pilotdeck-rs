# channel-sms

`#1339` A8.3: gold master for the baseline SMS channel
(`src/adapters/channel/sms/`, #1327 protocol shape). The real baseline TS
closure runs against mock external services only:

- `twilio` is replaced at the `require` seam by a mock REST client that
  records every `messages.create` call. No network beyond loopback, no
  real account. The real `node:crypto`/`node:http` builtins are used.
- The A7 gateway is a mock that models the stream contract the helper
  comments describe: after the turn's events are consumed, a turn with
  `holdAfter: "elicit"`/`"decide"` stays open until `respondElicitation` /
  `permissionDecide` releases it, so a pending answer reply arrives while
  the turn is still in flight, exactly like the product.
- The mapper's injected `uuid` is fixed, which makes session keys
  (`sms:chat=<From>:s_<uuid>` vs `:general`) deterministic.

The probe client speaks raw HTTP over a socket (`Connection: close`), so
the oracle records the exact response status, `Content-Type` and body per
request — including the destructive branches (payload over 1 MiB
destroys the socket with zero response bytes; bad signature answers 401)
and the loopback listener lifecycle (ECONNREFUSED after stop).

## oracle.json

Each case is run twice per generation and must produce identical
records; the run is not deterministic otherwise. Cases cover the 23-cell
state×event×timing matrix from `plan.md` (start failure modes, webhook
request validation, turn fragment rendering, dedupe/`/new` session
switching, elicitation and permission answering, stop while a turn is
held). The fix-r1 round added the signature-query, JSON-number-body,
missing-requestId (permission and elicitation), empty-host, bind-error
and truncated-body cells. The `port-busy-preflight` case has no replay
cell of its own: the Rust harness holds its port busy in
`port_busy_preflight_fails_fast`, and a second cell would race that hold
in the parallel test suite. `expected` records, per case:

- `logs` — `[level, text]` pairs through the injected logger
- `responses` — one entry per HTTP probe: `{status, contentType, body}`,
  `{closed: true}` (socket destroyed without any response byte) or
  `{refused: true}` (nothing listening on the port)
- `sends` — recorded mock Twilio sends `{body, from, to}`
- `rpcs` — recorded gateway calls (`submitTurn`, `respondElicitation`,
  `permissionDecide`) with their cloned arguments
- `unhandled` — rejections the product leaves unhandled. A non-string
  JSON `Body` is a process-level uncaught exception in the baseline (the
  generator's harness records it); the Rust port contains it in the
  connection task, see diffs ledger `A8.3-CH-SMS-001`.
- `cronResults` — `deliverCronResult` return values

Megabyte request bodies appear as a `{repeat, count}` spec in `steps`;
the bytes only exist while the request is on the wire.

Steps carry the *effective* request headers: for a `sign: true` case the
generator computed the `X-Twilio-Signature` itself (over its own signing
URL — `publicUrl` branch or `http://127.0.0.1:{port}{path}`, without any
query string — with `Object.keys(params).sort()` concatenation) and
recorded it, so the Rust replay reuses the TS-computed signature bytes
instead of re-signing. `truncate` keeps the full Content-Length while the
wire stops short; `halfClose` finishes the request's write side.

## Regenerate

```
PDRS_BASELINE=<PilotDeck checkout at cc7c4e6> \
node --import $PDRS_BASELINE/node_modules/tsx/dist/loader.mjs \
  generate.mts <empty-out-dir>
```

Node must be v22.22.0 with ICU 78.2; the generator asserts both. The
`#[ignore]` replay guard `regenerates_identically("channel-sms")` in
the crate re-runs this and requires byte-identical output.

## Negative controls

`negative_controls.py` runs the 66 `#996` compile-time switches the
channel carries (`crate::mutants::on("sms-…")`; 26 → 66, where the fix-r1
round added 11, #1589 added 4, #1729 added 39, #1813 added 1 — the `switches` table is
the truth source) through the shared
`_shared/mutant_switch` executor: one precompile, then per switch a
positive run, the mutated kill run (bound oracle cell + assertion) and
a restored re-run. `mutating_sources` cannot run pre-commit (new files
are not at HEAD), which is why the switches are compile-time and the
script only needs the caller's `CARGO_TARGET_DIR`:

```
python3 -I negative_controls.py --check          # bindings preflight
python3 -I negative_controls.py <new-output-dir> # full run, exit != 0
                                                 # unless all KILLED
```

#1729：`negative_controls.py` 现有 **65** 个开关（以 `switches` 表为准），新增 39 个录制字段/预算保护。每项先编译开关分支，再对普通断言运行正控、变异、复原；错型诊断包含 #1339、字段和实际值。

#1813：现有 **66** 个开关。`wait-sends.count` / `wait-rpcs.count` 改为必填（缺席即失败：TS 侧 `length >= undefined` 永不满足、3 s 超时，Rust 侧过去按 0 读成空操作，同一输入两端结论相反）；`turn` / `permissionError` / `permissionGate` / `permissionDelivered` / `sendBlockAt` / `sendError` 的 `null` 哨兵统一走 `recorded_present`（生成器对前五个写 `?? null`、对 `permissionDelivered` 写 `?? true`，读取端一律把显式 `null` 当缺席，手改 oracle 不会让某一个字段的口径漂移）。
