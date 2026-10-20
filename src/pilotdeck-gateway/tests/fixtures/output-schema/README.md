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
count of **379** cases. Each case is either:

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
to return an explicit unsupported error for JavaScript-only regex syntax or
class syntax that it cannot translate. These twenty-five cases are never counted
as equal validation results, and the replay also asserts that
`check_schema` rejects them.

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
| ECMAScript `/u` syntax V8 rejects (`\-`, lone `]`/`}`, `a**`, `\<a`, incomplete/duplicate quantifiers, bad escapes/ranges) in `pattern`/`patternProperties` | `SchemaError` with V8's `Invalid regular expression: /…/u: <reason>` during `tools/list`; never falls through to Rust's more permissive parser |
| JavaScript-only regex (lookarounds, backreferences, named groups, `\u` / `\p` / `\P` escapes) in `pattern`/`patternProperties` | `SchemaError` prefixed with `unsupported` during `tools/list`; `validate` projects it as `ValidationError::Unsupported` |
| relative/remote `$ref` that needs an `$id` base | `check_schema` error (`can't resolve reference … from id #`), same as Ajv when there is no base |
| schema nesting > 256 for the compile check | explicit `SchemaError`; Ajv throws `RangeError` at ~600 nested schemas |
| validation nesting > 10000 `check` frames (~5000 data levels) | explicit `ValidationError::Threw`; Ajv handles 4000 data levels (measured) and throws above its own V8 limit |
| classes with nested `[`, `&&` / `~~` / `--`, or empty classes | explicit `unsupported` schema compilation error; preserve real Ajv success/failure in the oracle |
