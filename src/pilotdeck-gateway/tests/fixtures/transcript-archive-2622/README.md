# transcript-archive-2622 fixtures

Evidence for #2622 (A7.5: the `export_session_transcript` /
`restore_session_transcript` delegate pair), generated against the TS baseline
`cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85` with Node v22.22.0.

- `generate.mts` — the baseline probe. Builds a real `createLocalGateway`
  (real delegates, real `SessionRouter`, real JSONL session storage on disk)
  and drives both archive methods directly. Same invocation contract as the
  other fixture generators: `node --import tsx/loader.mjs generate.mts
  <out-dir>` with `PDRS_BASELINE` set (the
  `regenerates_identically("transcript-archive-2622")` guard is the exact
  entry).
- `oracle.json` — 39 records, byte-identical across runs. The only masking
  applied is `<root>` for the scratch tree and `<uuid>` for the random
  uuid-shaped ids a restore writes (entryId, `sdk:restore:<uuid>` turn id);
  everything else (fixed clock timestamps, sequence numbers, byte sizes) is
  deterministic. The Rust replay tests assert against it, additionally
  folding the `createdAt`/`updatedAt` clock stamps to `<clock>` on both
  sides (the generator's clock is fixed, the Rust writer stamps real time).

Row groups map to the TS baseline (all in `src/cli/createLocalGateway.ts` +
its helpers) as:

| group | TS source | what it pins |
|---|---|---|
| `export.*` success | export delegate + `toPortableSessionArchive` | portable text rendering per content-block type (text / tool_call / tool_result / tool_result_reference / image), `"\n\n"` part join, synthetic-message filter, incomplete-turn durable skip, title `??`-chain with the 512-char ceiling (513 dropped, 512 kept, absent omitted), numeric-text JS coercion |
| `export.empty-text` | export delegate order | 0-message archives answer `INVALID_SESSION_ARCHIVE` (message count), so `SESSION_TRANSCRIPT_EMPTY` is unreachable |
| `export.bad-block` | `portableTranscriptText` | a tool_result image block without `data` throws the raw TypeError (`gateway_request_failed`) |
| `export.missing` / `export.invalid` | export delegate | `transcript_missing` → `SESSION_NOT_FOUND`; error-severity diagnostics → `SESSION_TRANSCRIPT_INVALID` |
| `export.blank-key` / `no-key` / `nonstring-key` / `nullish-params` | export delegate | the `#1604` family guard shapes (blank key, missing key, non-string key TypeError, nullish params TypeError) |
| `restore.fresh` / `written-bytes` / `reexport` / `no-title` | restore delegate | `{sessionKey, importedMessages}` result, the exact written transcript bytes (session_metadata-then-accepted_input, `metadata.restoredFromGatewayArchive`, parentEntryId chain) and the round-trip back through export |
| `restore.exists` | restore delegate | `SESSION_RESTORE_CONFLICT` when a persistent transcript already exists |
| `restore.invalid.*` | `validatePortableSessionArchive` | all five error texts (unsupported/malformed, message count incl. the 10001 ceiling, title, per-message role/text, 2 MiB size ceiling) |
| `export.busy` / `restore.busy` | both delegates | `SESSION_BUSY` while a REAL submitted turn is active: the agent loop runs in-process (the default `PILOTDECK_AGENT_LOOP_TRANSPORT=native` profile) against a local model endpoint that accepts connections and never responds, so `router.hasActiveTurn` is a live turn, not a fake |
| `restore.loaded-session` / `export.loaded-session` | both delegates | after `abortTurn` the session stays loaded: restore refuses (`hasSession` → `SESSION_BUSY`), export still succeeds (it only checks `hasActiveTurn`) |
| `chat.files` | restore delegate | restores rename their temp files away; only final transcripts remain |
| `capabilities` | `InProcessGateway` capability ad | `session_transcript_archive` is advertised when both callbacks are wired |

The busy rows also pin the guard ORDER: `busy-1` has a transcript AND an
active turn, and both methods answer `SESSION_BUSY`, never the restore
conflict.
