# Compaction failure result contract

Run `node --import BASELINE/node_modules/tsx/dist/loader.mjs generate.mts BASELINE FRESH_OUTPUT` with Node 22.22.0 / ICU 78.2. The producer checks the fixed baseline commit and CompactionEngine source bytes, then actually invokes the TS engine with a throwing stream, throwing iterator and a signal-driven cancellation. `protectedToolNames: null` matches Rust `ProtectedTools::Disabled` and makes these four-message inputs reach summarization.

All three runs resolve a structured `summaryGenerated: false` result with `compact_summary_failed` diagnostics; the outer engine does not reject the whole operation. Rust integration tests compare the error, diagnostics, boundary and retained messages against this capture, while separately proving provider-child cancellation and parent isolation. This product fixture does not modify any frozen scoring rule.

`providerSignalIsParent` records JavaScript object identity inside the TS engine. It is retained and asserted as a baseline observation; it is not a cross-language protocol field. Native compaction gives its provider an owned child signal: parent cancellation reaches that child, while a failed summary cancels only the child. The integration tests separately assert those observable cancellation effects rather than treating Rust allocation identity as an equivalent TS object reference.
