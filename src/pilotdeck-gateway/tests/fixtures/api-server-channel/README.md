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
| `keep-alive` | `Connection: keep-alive` request answered with `Connection: keep-alive` / `Keep-Alive: timeout=5` on one recorded request (socket reuse is *not* covered; see Known gaps) |

The Rust replay (`channel::api_server::tests::replays_real_ts_api_server_cells`)
skips `oversized-*` (it is a separate raw-socket test,
`oversized_body_closes_without_response`) and otherwise compares status,
headers, and body for every cell. A recorded request body of the form
`<N bytes>` is replayed as `N` repeated `x` bytes — the oracle stores the
request length instead of the filler. `submit_inputs_match_real_ts` compares
the exact `submitTurn` inputs recorded by the TS generator.

The placeholder is a fixture protocol with a fail-closed reader
(`channel::api_server::tests::recorded_body`), so a generator change cannot
silently degrade a cell into "some short body":

- `N` must be plain decimal — `[0-9]+` only. A near miss such as
  `<1_000_000 bytes>` is a fixture bug and panics the replay instead of being
  sent verbatim; so is a signed `<+5 bytes>` (which `usize::from_str` would
  otherwise accept).
- `N` is capped at `MAX_REQUEST_BYTES + 1`; a larger count is refused before
  allocation.
- A `body` field that is present but not a string panics; an absent one means
  no body (GET/HEAD cells). A header value that is not a string panics too, and
  so does a `headers` that is present but not an object (only an absent one
  means "no headers"). The same rule covers `options.apiKey` / `options.modelName`
  (absent takes the cell default, present-but-not-a-string panics) and
  `options.corsOrigins` (absent means no CORS, present-but-not-an-array-of-strings
  panics). Expected (oracle-side) response header values are read the same way.
- The `oversized-*` cells are never re-sent by the replay, so their recorded
  response used to be compared by nothing at all. Both ends now pin its shape:
  the generator refuses to record anything but a zero-byte close, and the
  replay asserts the recorded `expect[0]` has `bytes: 0`, an `error` string and
  no `status`. A baseline cap raised above the recorded `OVER_CAP_BYTES` would
  therefore fail loudly instead of leaving the surface green.
- `body-limit-exact` must expand to exactly `MAX_REQUEST_BYTES` and
  `oversized-*` to exactly `MAX_REQUEST_BYTES + 1`, asserted on every replay:
  the cap and the fixture must not drift apart.
- Every cell must record exactly as many `expect` responses as `requests`; a
  surplus response would never be compared. A cell must also record at least
  one request — `0 == 0` would otherwise count as a covered cell that replays
  nothing. The same reasoning pins the `submits` roster: `submit_inputs_match_real_ts`
  asserts the cells carrying `submitTurn` inputs are exactly
  `buffered` / `stream` / `new-command`.
- The generator derives each placeholder count from the same constant as the
  body it sends, so the two cannot disagree.

One coupling to keep in mind when adding a negative control: the replay budget
(`REPLAY_BODY_BUDGET`) is derived from `MAX_REQUEST_BYTES` on purpose, so a
mutation that *raises* the cap also raises the budget past
`recorded_body_refuses_an_unbounded_count`'s literal (`<9000000 bytes>`). That
guard would stop panicking under such a mutant. It is harmless today because
api-server controls run one named test each through
`negative_suite.run_cases`; a move to the matrix runner (which requires the
positive suite to stay green) needs the literal re-derived or the mutant
excluded.

## Known gaps

- `keep-alive` covers the response headers of a single request. Real connection
  reuse (a second request on the same socket) is not recorded: the generator's
  `send()` uses `agent: false`, so every request opens a new connection. Tracked
  as a follow-up; the cell name is about the header contract only.

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
guard, body limit, body-limit boundary, default port). Each row requires the
positive control to pass and the named `#1333` assertion to fail. It must run
after this change is committed (the shared guard compares against `HEAD`); see
the script header for the exact command.
