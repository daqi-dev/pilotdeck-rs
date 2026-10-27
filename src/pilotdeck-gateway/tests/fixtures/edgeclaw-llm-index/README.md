# EdgeClaw index-side LLM fixture (#768)

`generate.mts` directly imports `LlmMemoryExtractor` from the read-only
`cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85` source tree. `bindSourceTree` and
`sourceClosureSha256` verify the imported closure. Node **v22.22.0** is required.
The generator creates the output directory recursively and opens oracle.json
with `wx`; it never replaces a golden file. The Rust ignored regeneration test
runs this same generator and compares every byte.

```sh
export PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck
LANG=en_US.UTF-8 node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-llm-index/verify.mts
scripts/cargo-slots.sh
export CARGO_TARGET_DIR=/Users/da/ws/pilotdeck-rs/_build/wt-768/_build/cargo-target
export CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_PROFILE_TEST_DEBUG=line-tables-only
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway \
  --test edgeclaw_llm_index -- --include-ignored
```

## Recorded contract

73 cells cover the three index-side LLM calls of `llm-extraction.ts`
(`:2519-2684, :3018-3125`) plus their `:994-1106` helpers:
`classifyMemoryTurn` (~31 cells: label normalization, dedupe, unknown types,
`should_store` truthiness, role filtering, focus by reference/value/absent,
first/last window edges, truncation of the 320/400-unit limits, plain and
surrounding text, bad JSON, literal control characters inside JSON strings,
HTTP 400, empty agent id), the three `create*MemoryNote` wrappers (~21 cells:
all three kinds, missing session key, `skip === true` strictness, missing
name/empty description/whitespace markdown, generic project names, 80/180-unit
truncation, non-string fields, the loose-fallback parse, its failure without a
fallback, envelope-free text, HTTP 400, a window `TypeError` from a project-meta
record that escapes before the request, and a full meta record), and
`selectIndexProject` (~21 cells: attach/create-new, invalid ids with and without
a reason, id trimming, blank/non-string reasons, non-string/case variants of
`decision` and `selected_project_id`, empty shortlist, rich and minimal
candidates, partial shortlist records, no recent messages, HTTP 400, bad JSON).

Every case runs against a real loopback HTTP stub (`../edgeclaw-llm/stub.mts`)
recording `{method, url, headers, body, at}`; `requests` is the wire assertion
and `at` is the constant stub clock (0) because no case uses a retryable status,
so no backoff timers exist. `result` records the returned classification /
candidate / decision (or the escaped `TypeError` for the pre-try window build),
`debug` the full debug-trace event list, and `calls` the request count. Key
order is immaterial in the comparison (both sides sort object keys); string
bytes, including prompt text and request bodies, are compared exactly after
replacing the random listener origin with `$ORIGIN`.

The generator isolates HOME, freezes `Date.now` at 1792972800000 and
`Math.random` at 0.25, and gives every case a 5 s real watchdog. Each spec is
snapshotted through JSON before it is recorded so `undefined`-valued keys drop
exactly as they do in oracle.json.

`negative.py` holds 14 single-anchor mutants of
`src/pilotdeck-gateway/src/runtime/memory/edgeclaw/llm/index.rs`, each bound to
one named assertion of `tests/edgeclaw_llm_index.rs` (`oracle_matrix`).
`index.rs` is a new file, so `mutating_sources` refuses to run before the
coordinator commits it; `--check-anchors` runs pre-commit and the full executor
runs post-commit.
