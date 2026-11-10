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

Boundary body lengths are pinned **by value, not by case name** (#1984):
`src/channel/sms/tests.rs` declares `BOUNDARY_BODY_CASES`, and any case whose
replayed `request` body (recorded bytes minus `truncate`) reaches
`MAX_BODY_BYTES` must be declared there with its expected length. A renamed or
newly added boundary cell that is not declared fails the replay instead of
passing silently, a truncated boundary cell no longer counts as probing the
cap, and `boundary_body_cases_match_the_fixture` checks the table and the
fixture against each other in both directions.

## oracle.json

Each case is run twice per generation and must produce identical
records; the run is not deterministic otherwise. Cases cover the 23-cell
state×event×timing matrix from `plan.md` (start failure modes, webhook
request validation, turn fragment rendering, dedupe/`/new` session
switching, elicitation and permission answering, stop while a turn is
held). The fix-r1 round added the signature-query, JSON-number-body,
missing-requestId (permission and elicitation), empty-host, bind-error,
truncated-body and body-read-timeout cells. `http10` (#1913) sends its
request on an `HTTP/1.0` request line that still asks
`Connection: keep-alive`: Node answers the empty TWIML close-delimited
and closes, and the replay's `raw_request` only returns on the close, so
a lost finalize times out the cell. `body-read-timeout` (#1901)
declares `Content-Length: 64`, sends 8 bytes and keeps the write side
open, so only Node's default `requestTimeout` (300 s) can end it: the
baseline answers the 51-byte `408 Request Timeout` + `Connection: close`.
That cell costs ~300 s per run, and every case runs twice. The two
parser-admission cells (#2152) send `FROB /sms/incoming` (a token outside
Node's `http.METHODS`, recorded verbatim in the step's `method`) and an
HTTP/1.1 request whose step records `omitHost: true` (no `Host:` line on the
wire — a Host would turn the cell into an admitted 200): llhttp's bare
parser 400 and `_http_server.js`'s headless 400 respectively, both before
the route guard could answer its 404. The `unknown-method` /
`missing-host` wire pins in `tests.rs` hold the shape the oracle cell cannot
see (the bare 400's exact head and body — `Connection: close`, hyper's
registered `content-length: 0`, zero body bytes — compared through
`normalize_wire`, which drops this lane's registered date/header-case
residuals; the headless 400's own `Date`, chunked framing and absent
`content-length`). The `port-busy-preflight` case has no replay
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
wire stops short; `halfClose` finishes the request's write side; `stall: true`
keeps the write side open and lets the probe wait out the server's own
`requestTimeout` (only the `body-read-timeout` cell uses it); `version`
overrides the request line's HTTP version (only the `http10` cell sets it,
#1913 — the Rust `build_raw` mirrors the field and stops adding its own
`Connection: close` when the recorded headers already carry one).

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

`negative_controls.py` runs the channel's 74 `#996` compile-time switches
(`crate::mutants::on("sms-…")`; 26 → 74, `#1984`, `#1957`, `#1913` and
`#2152` added the last four — the `switches` table is the truth source) through the shared
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

#1729：`negative_controls.py` 现有 **74** 个开关（以 `switches` 表为准；#1729 当时 69，`#1901` 加 1，`#1984` 加 `sms-oracle-boundary-unregistered`，`#1957` 加 `sms-oracle-signed-signature-header`，`#1913` 加 `sms-oracle-recorded-version`，`#2152` 加 `sms-oracle-recorded-omit-host`），新增 39 个录制字段/预算保护（#1813 另加 1 个 `sms-oracle-null-sentinel`，`#1790` 另加 3 个 permission 跨 await 标志守卫）。每项先编译开关分支，再对普通断言运行正控、变异、复原；错型诊断包含 #1339、字段和实际值。

#1813：当时 **66** 个开关（`#1790` 后为 69）。`wait-sends.count` / `wait-rpcs.count` 改为必填（缺席即失败：TS 侧 `length >= undefined` 永不满足、3 s 超时，Rust 侧过去按 0 读成空操作，同一输入两端结论相反）；`turn` / `permissionError` / `permissionGate` / `permissionDelivered` / `sendBlockAt` / `sendError` 的 `null` 哨兵统一走 `recorded_present`（生成器对前五个写 `?? null`、对 `permissionDelivered` 写 `?? true`，读取端一律把显式 `null` 当缺席，手改 oracle 不会让某一个字段的口径漂移）。

#1879：不新增开关，只改写已有读取点。`wait-log.level` / `wait-log.text` 改为必填（缺席即报字段级错误，不再读成空串静默比较）；`env` / `sendFailAt` / `turn.gate` / `turn.error` / `turn.events` / `turn.holdAfter` / `truncate` / `halfClose` / `headers` / `stop.reason` / `extra` 改走 `recorded_present`，显式 `null` 与生成器的 `?? {}`（`env`、`extra`）/ `?? []` / `?? 0` / `=== true` / `?? "no reason"` / falsy 判断同义（缺席），`sms-oracle-null-sentinel` 开关经 `OLD_NULL_REJECTING_FIELDS`（11 个字段）恢复旧的 fail-closed 读取。该开关随之改为**逐字段列表绑定**（12 条测试各配一条断言）：合成一个测试时变异只会死在第一条断言上，其余字段有没有恢复旧读取无从证明。

同一行的 `body` / `method` / `path` 在 TS 侧没有 `??` 默认（`REQUEST` 直接把它们拼进模板串，`body` 为 `null` 会直接抛），只有 `truncate`（形参默认 `= 0`）与 `halfClose`（`=== true`）需要按「null = 缺席」处理；`body` / `method` / `path` 保持 fail-closed。步对象上的 `headers` 是另一处同签名读法：`effectiveHeaders` 写 `{ ...(step.headers ?? {}) }`（`generate.mts:376`），与 `env: definition.env ?? {}` 完全同形，所以也按「null = 缺席」处理（录制端在 `generate.mts:391` 把 `headers` 换成 `effectiveHeaders` 的结果再写进 step，`REQUEST` 在 `:941` 直接消费这个归一化后的对象、自己不再做 `??`；所以手改成 `null` 的 step 与缺席同义，与 `?? {}` 的归一化结论一致）；`stop` 步的 `reason` 同理（`stop(null)` 录成 `{reason: null}`，处理器读 `reason ?? "no reason"`）。
签名场景另说：`sign: true` 的 request 步由 `effectiveHeaders`（`generate.mts:377`）把 `X-Twilio-Signature` 烘进录制后的 headers，Rust 侧重放录制字节、不重新签名。所以「录制产物里的 `headers: null`」只能来自手改，而它会连签名一起丢掉、`mod.rs` 的验签分支又在签名头为空时跳过验签（`!signature.is_empty()`），这种手改就查不出来了。因此 `validate_mock_recording` 加了一条反向约束：**`sign: true` 的用例，其 request 步的 `headers` 必须录成对象，为 `null` 直接报错**（`signed_recording_rejects_null_headers` 钉住）。未签名用例的 `headers: null` 仍然等同缺席——那里两端本来就等价。现有 59 例（`#1913` 之后、`#2152` 加 `unknown-method` / `missing-host` 两 cell）的 `headers` 无一处 `null`，读值不受影响。

#1957：上面那条只挡 `headers: null`，`{}`、删掉 `X-Twilio-Signature` 键、签名为 `""` 三种手改都会通过对象检查，而重放同样不带签名、`mod.rs` 的 `!signature.is_empty()` 同样为假 → 该 cell 照样按录制的 200 判过，签名覆盖被静默丢掉（对拍假 PASS）。约束收紧为：**`sign: true` 用例的每个 request 步，`headers` 里必须有一枚非空字符串的 `X-Twilio-Signature`（header 名大小写不敏感，`signed_recording_accepts_lowercase_signature_header` 钉正例；「每个步」由 `signed_recording_checks_every_request_step` 钉住——第二个 request 步缺签名头同样报错，只看首个步的弱化会被判红）**，负例各配一条断言（`signed_recording_rejects_empty_headers` / `_missing_signature_key` / `_blank_signature_header` / `_non_boolean_sign` / `_whitespace_signature_header` / `_non_string_signature_header` / `_checks_every_request_step`），全部由开关 `sms-oracle-signed-signature-header` 恢复旧读取后逐条击杀。约束不按 body 类型设例外：`effectiveHeaders` 只在 body 是字符串时烘签名，但 `"abc"` → `{repeat: "abc", count: 1}` 这种改写让重放字节逐字节相同、签名却没了，留 body 例外等于把同一个洞重新打开；代价是**生成端若真产出一个 `sign: true` + 非字符串 body 的用例，读取端会显式判红**（宁可响亮失败，不静默丢签名），届时须在 `generate.mts` 侧烘签名或拒绝该形状——当前 4 个 signed cell 的 body 全是字符串，不触发。判定「非空」用 `trim()`：HTTP 解析器会裁掉字段值两侧的 OWS，`" "` 到闸门时等同 `""`。守卫同时盯 `sign` 标志本身——**`sign` 缺失只在「该用例没有 request 步」时算未签名，有 request 步而缺 `sign` 视为手改并报错**（`signed_recording_requires_sign_flag`；`generate.mts:1099` 恒写 `sign: definition.sign ?? false`），`sign: true` 的用例还必须至少有一个 request 步（`signed_recording_requires_a_request_step`，与 `recorded_steps_checked` 的空数组边界同口径），`sign` 非 bool 按 fail-closed 报错。**守卫原理性限制**（记录在案，不给结论）：把 `sign: true` 翻成 `false` 并同时删掉签名头，两字段协同手改仍能重放为未签名——守卫锚的是 `sign` 元数据，不是录制字节本身；单字段手改（只删头 / 只删 `sign` / 只翻 `sign` / 非 bool `sign` / `{}` / 缺键 / 空串 / 空白串 / 非对象 headers）都已被封住。配对正控 `unsigned_recording_allows_null_headers` 钉住「未签名用例的 `headers: null` 仍然通过」「没有 request 步的用例可以不带 `sign`」，防止约束泄漏到未签名用例。现有 59 例的 4 个 `sign: true` cell 全部带非空签名头（`#2152` 的两个新 cell 均未签名），金样与场景计数不变。
