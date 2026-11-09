# Subagent history TS fixtures

`../subagent-bundle.json` is the actual `GatewaySessionHistoryBundle` with injected file-backed storage callbacks; `../subagent-provider.json` uses the real JSONL storage provider. They cover the first parent reference, missing references/files, the execution-only replay, compaction order, tool/thinking, background input, and the provider's distinct sidechain path. `capture-bundle.mjs` and `provider.mjs` are independently written capture drivers; neither copies TS implementation code. They require checkout `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`, Node 22.22.0 and ICU 78.2, and assert the history entry source hash.

Run each driver in a separate empty, isolated output directory with the fixed checkout and its installed tsx loader, for example:

```sh
NODE=/Users/da/opt/node-v22.22.0-icu78-darwin-arm64/bin/node
BASE=/Users/da/ws/pdrs-runner/resources/PilotDeck
HOME="$OUT/home" TMPDIR="$OUT/tmp" PILOT_HOME="$OUT/home" TSX_DISABLE_CACHE=1 NODE_NO_WARNINGS=1 \
  "$NODE" --import "$BASE/node_modules/tsx/dist/loader.mjs" capture-bundle.mjs "$BASE" "$OUT/results"
```

Create `home`, `tmp`, and `results` before running; the driver creates unique case directories under `results`. Repeat with `provider.mjs` and a different output directory. Network access is disabled by the drivers. Raw TS process records (including paths) are retained separately at `/tmp/pdrs-subagent-terra-20261108` on mbp2; checked-in oracle paths are normalized to `<CASE>`. No claim is made here about Rust execution, provider backends other than JSONL, or terminal UI acceptance.
