# TUI reducer TS fixture

`reducer.json` was captured from the actual exported `applyGatewayEventToTuiState`, fixed TS `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`, file SHA-256 `85e8528dff0c6a3596415aebbfbb01039b3f90c3a2d64f781e1667fe332696cc`. This is pure code: no network, home or provider access. To regenerate on mbp2, copy `generate.mjs` into a new empty temporary directory, then run:

```
PATH=/Users/da/opt/node-v22.22.0-icu78-darwin-arm64/bin:$PATH /Users/da/ws/pdrs-runner/resources/PilotDeck/node_modules/.bin/tsx /path/to/generate.mjs /Users/da/ws/pdrs-runner/resources/PilotDeck /new/empty/path/reducer.json
```

The Node version, checkout commit and source hash are checked before loading; output is exclusive-created. Compare byte-for-byte and record Node and ICU versions. Events are deterministic and each state comes directly from TS; command parsing has no exported TS function and is not claimed to have this evidence. PTY behavior and real integration remain future work.

Capture: mbp2 Node `22.22.0`, ICU `78.2`; generator SHA-256 `fbadc592f2fa3ecb9a898cee5228f594b6aa344027867bad98eebe238c91640b`; raw `reducer.json` SHA-256 `55cf9de5e8c16e3acedc59f2d3dcce4014da0740fb6cde338cc074a3e3f71422`. Original capture: `/Users/da/ws/pdrs-runner/_tmp/20261108-tui-reducer-capture-utf16/` on mbp2.
