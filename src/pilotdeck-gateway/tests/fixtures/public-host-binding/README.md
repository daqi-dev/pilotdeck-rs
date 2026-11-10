# Public host project binding — real fixed TS

Fixed PilotDeck cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85; Node22.22.0 / ICU78.2 / libuv1.52.1. `generate.mts` uses the existing shared assertBaseline/baselineSourceHashes guard and records the actual relative import closure (728 source files in this capture), not manually declared provenance.

The 21 cases invoke original projectForStaffDeckPrincipal and projectBindingEnvironment on exclusively owned stores. A real local Node HTTP server runs original publicHostHttpTransport; a minimal resolver applies createLocalGateway's original falsy-root guard and advertises one inert operation. For numeric/boolean/array roots only, the resolver calls the original ProjectRuntimeRegistry.resolve on an actual registry instance: those inputs throw at its path boundary, before any runtime construction. This is a binding/transport component probe, not full Gateway boot, provider execution or a formal host-suite sample.

Coverage: disabled, bound, binding with/without a real live own-process .lock, duplicate rows, principal owner/tenant/actor/agent mismatch, origin mismatch/missing store, null row/null store and nonobject stores, empty root, both-origin-fields absent, and three nonstring roots. The lock belongs only to the current fixture process; no foreign PID is signalled and no account is used. Source TS functions read PID liveness themselves for projectBindingEnvironment.

Important distinctions actually observed: public selection accepts only bound, while runtime environment accepts binding+runtimeReady+live lock; both absent origins compare equal in JS; null row/store throw TypeError and become502; number/string/array stores are409; empty root is502 PROJECT_BINDING_PRINCIPAL_REQUIRED; nonstring truthy roots reach original registry and return502 ERR_INVALID_ARG_TYPE. These are separate surfaces, not inferred from another binding policy.

Set fixed PDRS_BASELINE and fixed Node PATH, then run:

`node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" src/pilotdeck-gateway/tests/fixtures/public-host-binding/generate.mts "$OUT"`

OUT may exist; oracle/raw publication uses wx. Only absolute fixture home/project paths become <HOME>/<PROJECT>; response statuses, codes and messages are unchanged. Raw HTTP body strings and actual live-lock PID stay in OUT/raw-capture.json. Oracle contains logical inputs and normalized exact results; no network port, PID or Date byte enters the reproducible fixture. All traffic is to the fixture's own loopback listener; external runtime/provider construction is not invoked.

Rust production_public_binding_matches_original_ts_selection_and_error_cells calls production bound_project directly. Success compares the selected root; refusal compares its status/code/message at the established transport default mapping. It does not treat the minimal fixture's one-operation advertisement as the production capability list. Full resolver rejection/HTTP mounting has separate native transport consumers. The ignored shared regeneration test compares the complete original-TS fixture bytes.
