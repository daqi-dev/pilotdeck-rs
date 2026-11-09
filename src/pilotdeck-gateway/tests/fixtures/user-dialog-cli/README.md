# User-dialog first checkpoint

This candidate registers the four opt-in kinds on the existing Gateway dialog bus. It provides the port, bounded blocking operations, a 100 ms poll, cancellation ownership, input/select/confirm outputs, and a legacy form validator using the existing UTF-16 value model. A failed registration rolls back the registrations from the current call. The tests exercise a real in-process dialog channel and cancellation, and compare 15 legacy schema/answer records with the fixed TypeScript oracle.

This checkpoint is not complete: it explicitly rejects Draft 2020-12 schemas, and the legacy regular-expression evaluator deliberately accepts only a conservative subset (with unsupported patterns rejected at schema admission). It refuses cyclic or very large values at the bounded native validation limit. Regex processing on unpaired surrogates is refused. These are compatibility gaps; the two draft records in the oracle are retained as the next implementation targets and are not counted as passing. The full four-kind feature must not be reported as complete before these gaps are implemented and independently validated. No Rust compilation or tests have been run by the candidate author.

The baseline oracle was captured from unmodified `src/tool/dialog/FormDialogSchema.ts` at `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85` on mbp2 with Node v22.22.0; `capture.mts` verifies HEAD and the source blob before and after loading and creates an exclusive output directory. Example on the authorized host:

```
PDRS_BASELINE=/Users/da/ws/pdrs-runner/resources/PilotDeck /Users/da/opt/node-v22.22.0-icu78-darwin-arm64/bin/node --experimental-strip-types capture.mts /private/tmp/NEW-EXCLUSIVE-DIRECTORY
```

Original output: `/private/tmp/pdrs-user-dialog-oracle-20261109-a/oracle.json`. The candidate oracle is byte-identical; SHA-256 is `f946d0cf2a414d4524074f6f2d1cc9410eaa27c91ff67bdade33939f8c4252b3`.

Integrate `user_dialog.rs` and `form_schema.rs` under `src/pilotdeck-gateway/src/runtime/tools/builtin/` and add `pub(crate) mod user_dialog; pub(crate) mod form_schema;` to that module. Put oracle and capture driver under `src/pilotdeck-gateway/tests/fixtures/user-dialog-cli/`. `NativeUserDialogPort::new(ChannelOptions)` and `register_user_dialog_tools(&ToolRegistry, &[JsString])` are the host entry points; `accepts_form_dialog_answer` is shared with recovery. Suggested test selectors: `runtime::tools::builtin::user_dialog::tests` and `runtime::tools::builtin::form_schema::tests`.
