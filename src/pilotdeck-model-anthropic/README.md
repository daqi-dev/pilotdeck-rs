# PilotDeck Anthropic protocol library

Pure request and stream protocol code extracted from the original gateway implementation. Dependencies are `pilotdeck-protocol` boundary values and `regex`; there is no gateway, Kernel, network, filesystem or credential dependency.

Public APIs include the original Anthropic `build_request`, `StreamState`, `normalize_stream_event`, normalized usage, shared thinking plan, canonical content block projection, provider header projection, and UTF-16 JSON tool-argument repair. The gateway keeps thin adapters back to its existing error type and injects its original test string-length limit. Its existing fixtures/oracles are retained unchanged.

`pilotdeck-protocol::coercion` now owns the original generic ECMAScript boundary coercions. Existing gateway and budget APIs re-export these functions. The move keeps missing/undefined/null, UTF-16, numeric and object-tag behavior in the shared library.

The independent `anthropic-provider` executable in the integration repository supplies transport, injected credentials, actual session lifecycle and endpoint authorization through the public external ModelSession wire. This library neither grants those capabilities nor claims they have executed.
