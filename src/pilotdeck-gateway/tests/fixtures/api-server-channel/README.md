# api-server-channel oracle (#1333)

Real `ApiServerChannel` wire cells captured from the TypeScript baseline and
replayed against the Rust port over a loopback HTTP/1.1 listener.

## Provenance

- Baseline checkout: `resources/PilotDeck` at commit
  `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`.
- Node `v22.22.0`, ICU `78.2` (recorded in `oracle.json`).
- Hashed sources (full map in `oracle.json.sourceSha256`):
  - `src/adapters/channel/api-server/ApiServerChannel.ts`
    `b24f2fcdab8c34c5d3382f72df3842f4eb4b55f26600c73c691e39d2bb0231a0`
  - `src/adapters/channel/api-server/ApiServerSessionMapper.ts`
    `a82fe26d96526c9ad650a2ecb0d127d3ac10877bb444b824a29122cf96ee09e4`
  - `src/adapters/channel/api-server/api-server-render.ts`
    `77fcaf9ce303019a61779a90657bc7a41cd084af1f057b85fdbfc03a6617bb7e`
  - `src/status/agentStatus.ts`
    `9b3f11e574fc33652594afa0a7b74f574542df57b50bb7226111cc40f2d6233b`
- The generator imports the real adapter, a real `ApiServerSessionMapper`, and
  the real render helpers; only the A7 gateway and the model side are scripted
  (`submitTurn`/`runTurn` replaced in-process). No account, model, or user home
  is touched.

## Regenerate

```sh
cd src/pilotdeck-gateway
PDRS_BASELINE=/path/to/resources/PilotDeck PDRS_GIT=/usr/bin/git \
  node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  tests/fixtures/api-server-channel/generate.mts \
  tests/fixtures/api-server-channel
```

`generate.mts` writes `oracle.json` through `mkdtemp`, `writeFile(..., {flag:
'wx'})`, then `rename`; a stale `.tmp` never survives. The guard
`generator_provenance.rs::api_server_channel_regenerates_identically` (#280)
re-runs the generator and requires byte-identical output; it is `#[ignore]` and
needs `PDRS_BASELINE`.

## Masking

To keep the oracle deterministic the generator masks only nondeterminism:

- `Date` response header → `Thu, 01 Jan 1970 00:00:00 GMT`.
- SSE `chatcmpl-<digits>` ids → `chatcmpl-FIXED`.
- SSE `created` epoch seconds → `0`.
- Generated chat ids `api-<uuid>` → `api-FIXED`; the mapper is given the fixed
  UUID `00000000-0000-4000-8000-000000000000`.
- Response headers are lowercased and sorted; request headers are lowercased.
- Loopback ports are ephemeral on both sides and never appear in a cell.

## Cells

| id | surface |
| --- | --- |
| `method-dispatch` | `GET`/`HEAD /health`, `GET /v1/models`, unknown path `404`, wrong method `404`, `OPTIONS *`, `//elsewhere/health` raw target |
| `unknown-method` | `FROB /health` (outside Node's `http.METHODS`): llhttp's bare `400`, no `Date`, zero body bytes |
| `missing-host` | HTTP/1.1 `GET /health` sent with `setHost: false` (no `Host` header): Node's `400` with `Date` and a chunked empty body |
| `cors-exact` | exact `Origin` match: `Allow-Origin`, `Allow-Methods`, `Allow-Headers`, `Vary: Origin` |
| `cors-star` | configured `*`: literal `*` wins; `Vary` absent |
| `auth` | no key configured (open), missing/wrong bearer `401`, correct bearer passes auth |
| `chat-validation` | bad JSON `400`, missing `messages` `400`, empty content `400`, empty `user` content `400`, `/v1/completions` `404` |
| `buffered` | non-streaming completion body and `X-Hermes-Session-Id` |
| `stream` | `text/event-stream` framing, content deltas, `[DONE]`, session header |
| `new-command` | `/new` buffered ack, `/new ` streaming ack (no session header), normal turn on the same chat |
| `gateway-missing` | no `turn_gateway`: buffered and streaming `503 gateway_unavailable` JSON |
| `oversized-0..2` | request body > 1,000,000 bytes closes the socket with zero bytes (three runs) |
| `body-limit-exact` | request body of exactly 1,000,000 bytes is read and rejected as `400 invalid_json` (the cap is exclusive) |
| `body-read-timeout` | `Content-Length: 64` declared but only 8 bytes sent, connection held open: Node's `requestTimeout` answers with the 408 and closes (the generator waits out the full 300 s) |
| `keep-alive` | `Connection: keep-alive` request answered with `Connection: keep-alive` / `Keep-Alive: timeout=5` on one recorded request (socket reuse is *not* covered; see Known gaps) |

The Rust replay (`channel::api_server::tests::replays_real_ts_api_server_cells`)
skips `oversized-*` (it is a separate raw-socket test,
`oversized_body_closes_without_response`) and `body-read-timeout` (it takes
Node's full 300 s and the Rust 408 carries hyper's chunked framing; the
separate `partial_body_is_answered_at_the_body_read_deadline` pins the Rust side
with an injected short deadline, and the replay still checks the TS-side 408
shape) and otherwise compares status, headers, and body for every cell. A recorded request body of the form
`<N bytes>` is replayed as `N` repeated `x` bytes — the oracle stores the
request length instead of the filler. `submit_inputs_match_real_ts` compares
the exact `submitTurn` inputs recorded by the TS generator.

The two parser-admission cells (#1683) carry a knob the generator uses and the
replay must reproduce:

- `unknown-method` records `method: "FROB"`, a token outside Node's
  `http.METHODS`, so llhttp rejects it before routing.
  `assert_parser_admission_cell_records_its_knob` pins the method; if the cell
  degraded to `GET`, the replay would compare `200 == 200` and the gate could be
  removed unnoticed.
- `missing-host` records `setHost: false`, Node `http.request`'s option that
  suppresses the automatic `Host` header. The replay's `ReqSpec` reads it
  fail-closed (only absent or a boolean is accepted) and omits the `Host:` line
  it otherwise writes; sending one turns the cell into an admitted `200`.
- The bare 400's `content-length: 0` is hyper's transport shape where Node
  sends no length. The replay masks that line (A8.4-NATIVE-HTTP-001, extended to
  this lane by A8.3-API-SERVER-005), and
  `parser_rejections_keep_the_registered_transport_shape` pins the raw bytes so
  the masking cannot hide a missing rejection.
  `parser_admission_cells_stay_justified_by_the_oracle` holds the registered
  set against the oracle in all three directions — every entry exists and
  records no length line the mask could hide, every bare-400 cell (a 400 whose
  only header is `connection`; an empty expectation list is not one) is
  registered, and the registered set is exactly those bare cells plus the inert
  `missing-host` entry, so a stale widening fails by name (#2775) — and
  `only_the_registered_cells_mask_a_content_length_zero` injects the line on
  the unregistered `stream` cell to prove the mask cannot normalize it away.

The placeholder is a fixture protocol with a fail-closed reader
(`channel::api_server::tests::recorded_body`), so a generator change cannot
silently degrade a cell into "some short body":

- `N` must be plain decimal — `[0-9]+` only. A near miss such as
  `<1_000_000 bytes>` is a fixture bug and panics the replay instead of being
  sent verbatim; so is a signed `<+5 bytes>` (which `usize::from_str` would
  otherwise accept).
- `N` is capped at `REPLAY_BODY_BUDGET`, the literal `1_000_001` that is also
  the recorded over-cap length; a larger count is refused before allocation.
- A `body` field that is present but not a string panics; an absent one means
  no body (GET/HEAD cells). A header value that is not a string panics too, and
  so does a `headers` that is present but not an object (only an absent one
  means "no headers"). The same rule covers `options.apiKey` / `options.modelName`
  (absent takes the cell default, present-but-not-a-string panics) and
  `options.corsOrigins` (absent means no CORS, present-but-not-an-array-of-strings
  panics). Expected (oracle-side) response header values are read the same way.
  The containers follow it too: an `options` that is present but not an object
  panics (absent every key takes its default), and so does a `gateway` that is
  present but not an object, a `gateway.turns` that is present but not an array,
  a turn that is not an object (only an absent `turns` key means "no turn"; a
  `null` element is drift like any other non-object), or a turn
  `events` / `error` of the wrong type — only absent (or `null`) means
  "no gateway / no turns / no events / no error".
- The `options` **key set** is closed. Only `apiKey` / `modelName` /
  `corsOrigins` may be recorded; any other key panics the replay
  (`REPLAY_OPTION_KEYS` in `tests.rs`). The generator spreads the recorded
  options verbatim into the real `ApiServerChannel` (`...(options ?? {})`), so a
  key the fixture records takes effect on the TS side; a reader that ignored it
  would compare a differently-configured Rust server and stay green. Adding a
  key therefore means teaching the reader first, then recording it.
- The `oversized-*` cells are never re-sent by the replay, so their recorded
  response used to be compared by nothing at all. Both ends now pin its shape:
  the generator refuses to record anything but a zero-byte close, and the
  replay asserts **every** `expect[i]` — not only the `oversized-*` ones — is
  either a complete response (numeric `status` that fits the `u16` the
  comparison narrows to, object `headers`, no `error`) or a clean error close
  (`error` string, `bytes: 0`, no `status`, no `headers`); a missing or
  mistyped required field panics (`assert_recorded_error_shape`). The rule is
  shape-driven so a renamed or newly added error cell cannot keep r8's measured
  fields uncompared, and the two shapes are mutually exclusive so a cell cannot
  drop its `error` key and leave the rest unread. The `oversized-*` branch
  additionally requires one recorded expectation per request, every one of them
  a connection error, so neither an emptied `expect` (vacuously true for
  `Iterator::all`) nor a baseline cap raised above the recorded
  `OVER_CAP_BYTES` can leave the surface green.
  `bytes` is a **measurement**, not a constant: it is counted on the socket, so
  it is recorded even when the connection dies before the `response` callback
  runs
  (Node raises `req`'s `'error'` synchronously and `IncomingMessage`'s on the
  next tick). A connection that dies after the status line and part of the body
  records the byte count it actually received and the status line it actually
  saw, so that shape fails the mutual-exclusion pin as well instead of being
  written down as a clean zero-byte close.
- `body-limit-exact` must expand to exactly `MAX_REQUEST_BYTES` and
  `oversized-*` to exactly `MAX_REQUEST_BYTES + 1`, asserted on every replay:
  the cap and the fixture must not drift apart. The replay's allocation ceiling
  (`REPLAY_BODY_BUDGET`) is a **literal** `1_000_001`, deliberately not
  `MAX_REQUEST_BYTES + 1`: derived, it moved with the `body-limit` negative
  control, which rewrites the cap in `mod.rs` to `10_000_000` — the budget rose
  with it and `<9000000 bytes>` stopped being refused, so the mutant switched
  off the guard it exists to trip. The tie is asserted instead — at both ends,
  by a test the `body-limit` row does not target — so raising the product cap
  fails by name, and the literal budget cannot drift wider unnoticed either.
- `body-read-timeout` records the declared `content-length` and a
  `stalledAfter` count that must be smaller — a cell that actually completed
  its body would exercise nothing. The replay also pins the TS answer itself
  (status `408`, empty body, no connection error), so a baseline that reset the
  connection instead of writing `requestTimeoutResponse` fails loudly rather
  than silently making the Rust deadline test look equivalent.
- Every cell must record exactly as many `expect` responses as `requests`; a
  surplus response would never be compared. A cell must also record at least
  one request — `0 == 0` would otherwise count as a covered cell that replays
  nothing. The same reasoning pins the `submits` roster: `submit_inputs_match_real_ts`
  asserts the cells carrying `submitTurn` inputs are exactly
  `buffered` / `stream` / `new-command`.
- The generator derives each placeholder count from the same constant as the
  body it sends, so the two cannot disagree.

One coupling to keep in mind when adding a negative control: the replay budget
(`REPLAY_BODY_BUDGET`) is a **literal** `1_000_001`, not `MAX_REQUEST_BYTES + 1`
(#1759 E). Derived, a mutation that *raised* the cap also raised the budget past
`recorded_body_refuses_an_unbounded_count`'s literal (`<9000000 bytes>`), so that
guard stopped panicking under exactly the mutant it exists to catch — and the
only red left was `assert_boundary_body_len` reporting it as fixture drift. The
tie is asserted instead (`assert_replay_budget_covers_the_cap`, plus the
`REPLAY_BODY_BUDGET == 1_000_001` pin in a test the row does not target), so
raising the cap fails by name, and the `replay-budget-vs-cap` row pins that.

## Known gaps

- `keep-alive` covers the response headers of a single request. Real connection
  reuse (a second request on the same socket) is not recorded: the generator's
  `send()` uses `agent: false`, so every request opens a new connection. Tracked
  as **#1710** (precedes closing #1333); the cell name is about the header
  contract only.

## Known divergence

- A JSON `null` body is not a cell: TS crashes the whole process at
  `ApiServerChannel.ts:213`; Rust answers `400 messages_required`.
  Registered in `specs/gateway-parity/diffs/A8.3-API-SERVER-001.md`.
- With an explicit request `Connection: close`, TS still emits `Connection:
  keep-alive` for SSE while hyper forces `close`. The oracle therefore uses
  keep-alive for SSE requests (normal SSE client behavior); the corner is
  registered in `specs/gateway-parity/diffs/A8.3-API-SERVER-002.md`.

## Negative controls

`negative_controls.py` applies compile-checked product mutations through
`tests/fixtures/negative_suite.py`; its `mutations` table is the register of
what is covered (health / models routing, auth bypass, CORS `Vary`, SSE
`[DONE]`, session header, visible-failure name, gateway status, active-chat
guard, body limit, body-limit boundary, body-read timeout, request-timeout 408
shape, default port, parser-level admission, the scope of the
`content-length: 0` mask, and the staleness of its registered cells). Each
row requires the positive control to pass and the named `#1333` assertion to
fail. It must run
after this change is committed (the shared guard compares against `HEAD`); see
the script header for the exact command.

The `api-server-parser-admission` row replaces the whole
`native_http::node_parser_admission` call site with a no-op: `FROB /health` and
the Host-less request then fall through to the `/health` route and answer `200`,
so the recorded `400` cells go red. That is the #1683 defect itself, which is
why the negative control mutates the wiring rather than a mutant switch.

Two #2775 rows target the mask's own scope instead of the product: the
`api-server-cl0-mask-always` row rewrites the mask condition in
`actual_headers` so the `content-length: 0` drop fires for every cell, which
the injected-line unit test (`only_the_registered_cells_mask_a_content_length_zero`)
must reject; and the `api-server-admission-stale` row appends a third,
never-inert cell (`body-read-timeout`) to `PARSER_ADMISSION_CELLS`, which the
oracle guard's upper-bound assertion must reject as a stale entry.
