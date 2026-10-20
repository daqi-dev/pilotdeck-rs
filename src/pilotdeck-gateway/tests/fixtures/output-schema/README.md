# outputSchema Ajv oracle (A6.5-pr10b)

Gold cases generated from the **real** SDK validator, not from this port:

- `@modelcontextprotocol/sdk@1.29.0`
  `dist/esm/validation/ajv-provider.js`:
  `new Ajv({strict:false, validateFormats:true, validateSchema:false, allErrors:true})`
  then `addFormats(ajv)` **(no `mode`, i.e. ajv-formats' full mode)**.
- `ajv@8.20.0`, `ajv-formats@3.0.1`.

`ajv-oracle.json` records `sdk` / `ajv` / `ajvFormats` and the SHA-256 of the
three files that determine the behaviour (`providerSha256`, `ajvSha256`,
`formatsSha256`). The generator pins their observed hashes and versions,
binds the clean baseline commit with `bindSourceTree`, and records the
`McpClient.ts` import closure. Replay uses `baseline::oracle` and an exact
count of **526** cases. Each case is either:

- `compileError`: `getValidator` threw (the SDK does this in `tools/list`);
- `compileErrorPrefix`: same, but only the V8 portion is stable (invalid
  regex text); `compileErrorDetail` additionally checks the reason suffix and
  `exactCompileError` compares the whole `actualCompileError` string;
- `errorText`: `ajv.errorsText(validator(data).errors)`;
- no error field: valid.

The `invalid-pattern/*` cases are ECMAScript `/u` syntax rejected by V8.
Ajv feeds both `pattern` and every `patternProperties` key to
`new RegExp(pattern, "u")`, so the Rust port must report a `SchemaError` at
`tools/list` time rather than compiling a permissive validator. The
`invalid-pattern/grok/*` block is the exact set from Grok r3 P1-A and compares
the complete V8 message.

`portUnsupported: true` leaves the real Ajv result intact, but requires Rust
to accept the schema (Ajv compiles it) and to return an explicit unsupported
error only when the pattern is used. These seventeen cases are never counted as
equal validation results. Valid `/u` syntax is no longer a compile error:
lookarounds, backreferences, named groups, `\u` escapes and `\p{...}` names
all pass `check_schema`; the ones Rust can translate are evaluated normally.

`positive-pattern/*` is the r4 regression set: 38 common patterns the real SDK
accepts (`\uXXXX`, `\u{...}`, named groups, `\p{L}`, `\p{Script=Han}`,
`[\w-]`, `[\w.-]`, `[a-z\s]`, `[-\d]`, `[\d-]`, CJK ranges, astral classes,
…) with matching and non-matching data. They are normal cases, so the replay
checks both `check_schema` and the exact Ajv validation result.

`format-regex/*` exercises ajv-formats' non-`/u` `regex` format (59 cases),
including repeated quantifiers, Annex B identity/octal escapes, `\Z`, named
groups and class ranges. `validate` returns `ValidationError::Unsupported` if
the port cannot decide a value, never a silent pass.

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck LANG=en_US.UTF-8
node src/pilotdeck-gateway/tests/fixtures/output-schema/generate.mjs \
  "$PWD/src/pilotdeck-gateway/tests/fixtures/output-schema"
```

The generator is deterministic (run twice, `cmp` byte-identical; the ignored
test `mcp_runtime::output_schema_generator_reproduces_the_committed_oracle_twice`
does the same under `PDRS_BASELINE`). The Rust replay is
`runtime::mcp::output_schema::tests::replays_the_ajv_oracle`.

The generator intercepts the actual `RegExp` constructions inside the SDK
provider and records `regexCompilation`: both `pattern` and
`patternProperties` compile with `"u"`. Gold cases cover ASCII word/digit
classes and their complements, JS whitespace (BOM included, NEL excluded),
ASCII word boundaries, all four dot line terminators, astral characters,
class-local backspace/dot and literal escaped backslashes. Each atom is
exercised both as a value pattern and as a property-name pattern.

## Known divergences (explicit, never silently accepted)

| feature | this port |
|---|---|
| ECMAScript `/u` syntax V8 rejects (`\-`, lone `]`/`}`, `a**`, `\<a`, incomplete/duplicate quantifiers, bad escapes/ranges, invalid `\p{...}` names) in `pattern`/`patternProperties` | `SchemaError` with V8's `Invalid regular expression: /…/u: <reason>` during `tools/list`; never falls through to Rust's more permissive parser |
| valid `/u` regex Rust cannot evaluate (lookarounds, backreferences, some `\p{...}` values) in `pattern`/`patternProperties` | `check_schema` accepts it, matching Ajv; `validate` returns `ValidationError::Unsupported` when the pattern is used, never a silent pass |
| relative/remote `$ref` that needs an `$id` base | `check_schema` error (`can't resolve reference … from id #`), same as Ajv when there is no base |
| schema nesting > 256 for the compile check | explicit `SchemaError`; Ajv throws `RangeError` at ~600 nested schemas |
| validation nesting > 10000 `check` frames (~5000 data levels) | explicit `ValidationError::Threw`; Ajv handles 4000 data levels (measured) and throws above its own V8 limit |
| classes with nested `[`, `&&` / `~~` / `--`, or empty classes | accepted at compile time; using the pattern returns `ValidationError::Unsupported` (the real Ajv result is preserved in the oracle) |
| `patternProperties` whose children are all always-valid and whose sibling `additionalProperties` is absent/always-valid | Ajv's `patternProperties` codegen early-returns without compiling the keys; both `check_schema` and validation skip them (`alwaysValid` early-exit, r4 GLM P1) |
