# networkFetch error continuation order (#452)

`generate.mts` runs the real fixed-baseline `networkFetch` with an injected fetch
port. It records raw no-abort, fulfilled rejection plus synchronous abort,
blocked-loop earlier error/later parent, earlier parent, pre-normalized error,
and the last error of a two-attempt GET. Both timer callbacks are overdue in
the blocked-loop cases. Expected errors and attempt counts come from TS.

The generator checks the fixed commit, tracked cleanliness and committed source
hash before and after execution. Node 22, no sockets, no baseline writes:

```sh
PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck \
node --import /Users/da/ws/pilotdeck-rs/resources/PilotDeck/node_modules/tsx/dist/loader.mjs \
src/pilotdeck-gateway/tests/fixtures/network-error-order/generate.mts /tmp/pdrs-network-error-order
```

Web replays the five raw-fetch cases. Its ModelHttpTransport error type only
represents raw rejections; already-normalized timeout/build errors remain on
their existing typed paths. MCP replays all six cases and separately asserts
that Plain mode preserves the bare fetch error. No MCP Plain/SSE behavior is
changed by Network-mode normalization.
