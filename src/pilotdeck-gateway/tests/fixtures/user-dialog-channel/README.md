# user-dialog-channel fixture

Real-TypeScript oracle for the Rust ports of:

- `src/gateway/user-dialog/GatewayUserDialogJournal.ts`
- `src/gateway/user-dialog/GatewayUserDialogChannel.ts`

The oracle is produced only by executing the real TS sources at the pinned
baseline commit (`cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`); no expected value
is written by hand. Source hashes for the Journal, Channel, Bus and
`FormDialogSchema` are recorded in `oracle.json` and checked by
`tests/common/baseline.rs` whenever `PDRS_BASELINE` is set.

## Regenerate

```sh
PDRS_BASELINE=/path/to/PilotDeck \
node --disable-warning=ExperimentalWarning \
  --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/user-dialog-channel/generate.mts /tmp/udc
```

Run it twice and compare `oracle.json` bytes. The ignored Rust test
`user_dialog_channel_fixtures_regenerate_identically_from_ts` performs the same
check through the shared helper.

## What is replayed

`channelCases` drive `GatewayUserDialogChannel` with an injected clock,
deterministic UUIDs, a scripted in-memory store, an owner map with a TTL, a
take-answer callback, the real TS bus, and configurable fault injection. Every
step records:

- the action result and the channel promise outcome (`null` while pending),
- bus `pending` and `pendingCount`,
- journal bytes and existence,
- hosted-store entries, owner leases and stored answers,
- exact callback order for journal/store/owner/take/emit/bus/uuid.

`journalCases` cover record/replace/remove/recover/clear, invalid records,
missing files, a directory at the journal path, and byte-exact write output.
`creationCases` cover undefined/empty/relative/absolute/trailing-dot transcript
paths.

Rust replays each action and compares `stringify_json` text, key order, number
spelling and lone surrogates. The clock is an atomic; `tick()` advances due polls
explicitly. Nothing sleeps or depends on wall time.

## Timeout and renewal semantics

The TS Channel has no dialog-timeout timer. Timeout-like behavior is therefore
modeled in the two ways the host actually observes it:

1. **owner lease TTL + renew failure** (`owner-lease-timeout`,
   `renew-failure-then-answer`): a failed renew skips that poll tick; a later
   tick retries, and a lease left expired is reported by the owner map.
2. **caller abort at the deadline** (`abort-after-register`,
   `abort-during-store-put`): the caller's `AbortSignal` fires, the channel
   consumes the pending dialog, emits `user_dialog_cancelled`, and settles.
3. **cross-gateway timeout result** (`cross-gateway-cancelled`): the hosted
   store contains a cancelled answer with reason `timeout`, which the channel
   passes through unchanged.

Form-schema validation is out of scope: the Rust channel receives an injected
`form_accepts` callback, while the TS oracle uses the real
`acceptsFormDialogAnswer`.

## Negative controls

```sh
PDRS_BASELINE=/path/to/PilotDeck \
python3 src/pilotdeck-gateway/tests/fixtures/user-dialog-channel/negative.py OUT
```

Each mutant is applied to an isolated `/tmp` crate. A control passes only when
the replay fails an oracle assertion for the intended branch (never a compile
error). The suite covers recover fields, empty-journal removal, owner release,
claim false, renew failure, abort-before-registration, cross-gateway invalid
values, emit failure, store-put failure, settle order and a tampered source hash.
