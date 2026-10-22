# Router timeout config (#440 remaining part)

The generator runs the fixed baseline's real `parseRouterConfig`, then passes
its result to real `classifyAndRoute`. It records the parsed timeout, diagnostics,
and the native Node 22 timer list integer delay (not `_idleTimeout`). All timers
are cleared by the real classifier. Source hashes and baseline guards run before
and after generation. Invalid values fall back to the TS default; large positive
integers remain valid config values and use Node's overflow-to-1ms timer behavior.

Generate twice into separate output directories and compare `oracle.json`:

```sh
PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck \
node --import /Users/da/ws/pilotdeck-rs/resources/PilotDeck/node_modules/tsx/dist/loader.mjs \
src/pilotdeck-gateway/tests/fixtures/router-timeout-config/generate.mts /tmp/pdrs-router-timeout-config
```

The Rust test projects raw snapshot values and drives the full classifier with
paused time, checking the parsed timeout, elapsed delay and judge cancellation.
`nonFinite` tags distinguish numeric NaN/infinities from an actual string input;
the omitted `input` property means an omitted config property.
