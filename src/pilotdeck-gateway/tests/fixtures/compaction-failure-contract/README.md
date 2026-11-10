# Compaction failure result contract

Run `node --import BASELINE/node_modules/tsx/dist/loader.mjs generate.mts BASELINE FRESH_OUTPUT` with Node 22.22.0 / ICU 78.2. The producer checks the fixed baseline commit and CompactionEngine source bytes, then actually invokes the TS engine with a throwing stream, throwing iterator and a signal-driven cancellation. `protectedToolNames: null` matches Rust `ProtectedTools::Disabled` and makes these four-message inputs reach summarization.

All three runs resolve a structured `summaryGenerated: false` result with `compact_summary_failed` diagnostics; the outer engine does not reject the whole operation. Rust integration tests compare the error, diagnostics, boundary and retained messages against this capture, while separately proving provider-child cancellation and parent isolation. This product fixture does not modify any frozen scoring rule.
