# turn-replay

Real-TS golden for `GatewayTurnReplayStore` and `GatewayTurnEventCoordinator`, plus
the `sameTurnEpoch` retry loop in `InProcessGateway.readSessionMessages`.

- Baseline: PilotDeck `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`.
- Hashed sources: `GatewayTurnReplayStore.ts`, `GatewayTurnEventCoordinator.ts`,
  `InProcessGateway.ts` (`sourceSha256` in `oracle.json`).
- The generator transpiles the unchanged modules and injects only the coordinator
  dependency; `readSessionMessages` is the real prototype method with a fake
  durable reader and the real coordinator/store.

## What is recorded

- Store: default 500-event limit, default 256 KiB byte limit (an exactly-262144-byte
  padded event is generated and its actual byte count asserted), custom limits,
  UTF-8 `JSON.stringify` byte counts for CJK/emoji, lone surrogates and mixed
  strings, single-event-over-limit self-eviction, terminal retention (default
  30000 ms and custom), restart/mismatch/clear/dispose, sticky truncation and
  `withRunId` whitespace rules, and retention 0 keeping a terminal replay visible
  to the synchronous snapshot that follows `retainTerminal`. `withRunIdEdge`
  records the non-object edge of the same call: the TS spread
  `{ ...event, runId }` spreads a string by UTF-16 code unit (surrogate pairs
  split; a lone surrogate stays alone), an array by index, nothing for
  number/boolean, and a `TypeError` (by constructor name) for null reading
  `event.runId` — see `with-run-id-non-object-spread`.
- Coordinator: sink required for emit, runId fill/preserve, record vs emit,
  start replacing sink and buffer, terminal sink removal, dispose, expiry, and
  reentrancy: a nested `emit` from inside the session's own sink delivers (and
  fails) inside the nested call, pinned by markers around the return boundary —
  including the inner sink itself throwing, which surfaces at the nested call
  site while the outer sink continues.
- Epoch: snapshots around the real `sameTurnEpoch` checks: stable active/terminal/
  missing, terminal, run replacement, same-run restart (same epoch), truncation
  (same epoch), mismatch, restart after terminal, dispose, expiry, clear, and
  three changing attempts throwing. Each attempt's `sameEpoch` boolean is
  inferred from whether the real retry loop returned or retried on that attempt,
  not from a copied predicate.

`setTimeout` is intercepted for the whole fixture: every delay is recorded in
`timerDelays` (default terminal retention records `30000`) and `advanceTime`
fires due callbacks explicitly, so no wall clock enters the golden.

Rust keeps one cancellable deadline per session, the counterpart of the TS
`setTimeout`/`clearTimeout` pair: `start`, `clearTerminal` and a repeated
`retainTerminal` replace or cancel the session's timer instead of accumulating
entries, and expiry deletes the replay from the timer callback with no store call
in between. Production waits on `Instant` (monotonic), so a wall-clock jump
cannot shorten or extend retention; a zero delay is deferred by Node's own 1 ms
floor (via `pilotdeck_protocol::timer_delay_ms`, the single source of Node's
`setTimeout` delay coercion), which keeps the `setTimeout(0)` next-macrotask
boundary observable for a `snapshot` immediately after `retainTerminal` in the
common case. The JS timer-turn boundary itself is not promised on the production
path for **any** retention duration: Node only runs the expiry callback on a
later timer turn, after the caller's synchronous segment and pending
microtasks, while the Rust timer thread fires a due deadline without waiting.
The real TS store, with real timers, keeps the replay visible through the
caller's busy segment and a Promise microtask for retention 0, 5 and the
30000 default, and only the next timer turn reclaims it — verified by
`_tmp/20261022-a72-pr1-replay/probe-retention-real-timer.mts`. The divergence
is an implementation choice pending a decision (accept it, or align by
injecting a host-serial timer once A7.2-pr5 provides the host scheduling
loop), registered as A7.2-REPLAY-RETENTION-TIMER-001 in
`specs/gateway-parity/accepted-diffs.md`; the fixture's explicit
`advanceTime` — and the injected `ManualTimer` — plays that boundary
deterministically, and a host needing it in production injects its own
`ReplayTimer` through `TurnReplayOptions::with_timer`.

## Regenerate

```sh
export PDRS_BASELINE=/path/to/PilotDeck
export TMPDIR=/tmp TSX_DISABLE_CACHE=1
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/turn-replay/generate.mts /tmp/turn-replay-first
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/turn-replay/generate.mts /tmp/turn-replay-second
cmp /tmp/turn-replay-first/oracle.json /tmp/turn-replay-second/oracle.json
```

Expected SHA-256 of `oracle.json`:

```
bfacf7f6e536d9d7d76d1cde10ef96d4b84a23865cb13ba7c7b930943ac127e0
```

The generator calls the shared `../_shared/baseline.mts` guard before and after,
checks the fixed HEAD and tracked cleanliness, and records committed/working
source hashes.

## Replay and negative controls

```sh
cd src
PDRS_BASELINE=/path/to/PilotDeck cargo test --offline -p pilotdeck-gateway --lib runtime::turn::
```

```sh
PDRS_BASELINE=/path/to/PilotDeck python3 \
  src/pilotdeck-gateway/tests/fixtures/turn-replay/negative.py /tmp/turn-replay-negative
```

`negative.py <output-dir>` copies the two Rust sources and the oracle into an
owned `/tmp` crate, proves the unmutated sources pass, then applies one mutation
at a time and requires the targeted TS-oracle assertion to fail. It never edits
the checkout; the temporary workspace is removed at the end.
