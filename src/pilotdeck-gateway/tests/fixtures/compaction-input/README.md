# Compaction input oracle (#387)

`generate.mts` imports the fixed real TS orchestrator, DefaultContextRuntime,
TokenBudgetManager and projectCompactionBudget. It checks the baseline commit,
tracked cleanliness and committed source hashes before and after execution.
The 48 input cases cover nullish fallback, raw non-number values, malformed
messages and evaluator bypass; six fallback, five summary numeric conversion
and six projection cases are recorded without handwritten expected values.

Run from the repository root (Node 22):

```sh
PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck \
node --import /Users/da/ws/pilotdeck-rs/resources/PilotDeck/node_modules/tsx/dist/loader.mjs \
src/pilotdeck-gateway/tests/fixtures/compaction-input/generate.mts /tmp/pdrs-compaction-input
```

The integration test also offers an ignored `compaction_input_generator_twice_is_byte_identical`
reproduction check. The shared projection now lives in pilotdeck-protocol and is
used by the production transport serializer as well as the gateway replay.
