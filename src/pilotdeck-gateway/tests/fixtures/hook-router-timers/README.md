# Hook/router timers (#440)

`generate.mts` checks fixed-baseline provenance, observes Node 22's actual timer
list integer delays, and wraps `setTimeout` while the real TS `classifyAndRoute`
runs against a fulfilled judge port. The wrapper calls the original Node timer;
no expected delay is calculated by the generator. `_idleTimeout` alone is not
used because it retains valid fractions before Node truncates the scheduled
list delay. Every observed timer is cleared by the probe or the real classifier.

Generate twice into different directories and compare `oracle.json`:

```sh
PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck \
node --import /Users/da/ws/pilotdeck-rs/resources/PilotDeck/node_modules/tsx/dist/loader.mjs \
src/pilotdeck-gateway/tests/fixtures/hook-router-timers/generate.mts /tmp/pdrs-hook-router-timers
```

Rust's hook conversion and full classifier timeout/cancellation tests consume
all 13 cases, including fractional upper bound, infinity, negative and NaN.
Router config parsing is deliberately outside this fixture's runtime scope.
