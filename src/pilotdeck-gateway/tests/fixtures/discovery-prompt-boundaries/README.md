# Discovery prompt dynamic boundary oracle

This is a new, unfrozen product-side oracle. It supplements the unchanged discovery-templates.json and discovery-static.json fixtures.

Generate with exact Node22.22.0/ICU78.2:
node --import BASELINE/node_modules/tsx/dist/loader.mjs generate.mts BASELINE FRESH_ORACLE_PATH

The generator verifies fixed TS commit cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85 and compares both prompt modules and four imported tool-name modules against git-show bytes before importing the real buildDiscoveryPrompt. No reimplementation computes expected strings.

The 328 cases cover both languages, workspace present/absent, actual ISO .678Z and no/one/two/four/non-digit milliseconds, suffix anchoring, LF-only flattening, preserved interior CR/LS/PS, all ECMAScript trim whitespace, empty prompts/list/digest, two-session ordering, existing plans, literal @@...@@ insertion values and lone UTF-16 surrogates. Expected text is compared as JsString units, without lossy UTF-8 conversion. Project/strategy remain valid UTF-8 as required by the Rust function's typed inputs; run/cwd/title/alias/prompt/plan values exercise raw UTF-16.

Oracle capture is real fixed-TS output. Rust execution is performed by the integration owner; generating this oracle alone does not claim the Rust tests passed or formal parity accepted.
