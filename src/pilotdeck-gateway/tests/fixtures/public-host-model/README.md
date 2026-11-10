# Public model boundary — real fixed TS

Baseline: PilotDeck cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85. Node22.22.0 / ICU78.2 / libuv1.52.1. The generator imports the original composition/activeRuntimeModelPorts.ts and invokes it through the original publicHostRuntimeAdapter.ts provider. All 38 prepare results are captured by invoking those implementations, with explicit in-memory catalog/capability inputs and no provider/network requests.

Provenance uses ../_shared/baseline.mts assertBaseline and baselineSourceHashes: fixed commit, clean tracked tree, index-flag checks, source bytes against Git blobs, sanitized Git environment, and complete relative source-import closure. sourceSha256 is the actual closure returned by that shared guard; it is not a handwritten declaration of four direct dependencies.

Set PDRS_BASELINE to the untouched fixed checkout and place the fixed Node on PATH, then execute:

`node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" src/pilotdeck-gateway/tests/fixtures/public-host-model/generate.mts "$OUT"`

OUT may already exist, but oracle.json must not exist; publication uses wx. The Rust ignored guard fixed_ts_public_model_oracle_regenerates_identically invokes this generator and compares the complete committed bytes. Normal consumer all_fixed_ts_public_prepare_cases_have_exact_native_consumers replays every captured case through the actual native public prepare boundary.

Original34 rows remain unchanged; an independent standard text prepare success plus legal equalities: output=selected capability256, request output64=budget64, timeout=MAX_SAFE_INTEGER. The original config has no text modality, so some original structural-valid rows reach unsupported_modality; their old observed bytes are retained. Only the four new positive rows carry explicit per-case config supporting text, and generation requires all four to return200. Existing refusals cover model selection, safe-integer budgets, malformed public messages and underlying request validation. Positive equalities are separate rows rather than inferred from neighbouring rejects.

Known scope: prepare only. Runtime model capabilities and catalog are controlled inputs; they do not prove concrete HTTP provider success, streaming/cancellation, public HTTP transport, project binding, filesystem parsing or SOP authority. Those use separate actual consumers. Error classes and status mapping are handled by the original public runtime adapter, not a catch block that labels every exception400. No fixed-TS implementation is copied or replaced and no oracle response is invented. Case IDs/count/protocol bytes are test contracts; adding cases requires actual regeneration.
