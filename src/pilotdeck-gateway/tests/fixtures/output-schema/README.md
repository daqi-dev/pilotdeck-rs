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
count of **904** cases. Each case is either:

- `compileError`: `getValidator` threw (the SDK does this in `tools/list`);
- `compileErrorPrefix`: same, but only the V8 portion is stable (invalid
  regex text); `compileErrorDetail` additionally checks the reason suffix and
  `exactCompileError` compares the whole `actualCompileError` string;
- `errorText`: `ajv.errorsText(validator(data).errors)`;
- `validationThrow`: compilation succeeded, but executing the validator
  threw; kept with `portUnsupported` for deep patterns this port refuses;
- `sdkError`: the real SDK Client.callTool error code and full message;
- `portDepthLimit`: Rust must return the exact SDK stack-overflow RPC error;
  `knownDifference: conservative-stack-budget` explicitly labels cases where
  the SDK returns a different result. Such cases are not counted as parity;
- no error field: valid.

The `invalid-pattern/*` cases are ECMAScript `/u` syntax rejected by V8.
Ajv feeds both `pattern` and every `patternProperties` key to
`new RegExp(pattern, "u")`, so the Rust port must report a `SchemaError` at
`tools/list` time rather than compiling a permissive validator. The
`invalid-pattern/grok/*` block is the exact set from Grok r3 P1-A and compares
the complete V8 message.

`portUnsupported: true` leaves the real Ajv result intact, but requires Rust
to accept the schema (Ajv compiles it) and to return an explicit unsupported
error only when the pattern is used. These cases are never counted as
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

`uri/upper`, `uri-reference/upper`, `uri-template/upper` and the
`long-s`/`kelvin`/`i-with-dot` cases pin #829: ajv-formats' full-format
regexes compile with `/i` and no `/u` (measured via a `RegExp` construction
proxy), so their case folding is ASCII-only. The port folds the haystack the
same way (`(?i)` plus masking U+017F/U+212A, the only non-ASCII code points
whose simple case fold is an ASCII letter); uppercase schemes and `{VAR}`
stay valid while `ſ`/`K`/`İ` stay invalid. `ipv6` needs no mask because its
`[0-9a-f]` class contains neither `s` nor `k`.

`deep-pattern/*`, `deep-pattern-properties/*`, and `deep-format-regex/*`
cover 10000 / 100000 nested capturing, noncapturing, and lookahead groups,
plus the capture boundary at 32767 / 32768. Node 22.22.0 (V8 12.4) constructs
32767 captures in both modes and rejects 32768 with `Too many captures`.
Noncapturing groups still construct at depth 100000. The syntax checker uses
an explicit heap stack; the capture-count limit also applies to flat groups.
Rust's regex engine may still reject valid deep patterns, in which case
schema compilation succeeds and validation returns `Unsupported`. Deep
captures/lookaheads can compile in V8 but throw `Stack overflow` when first
matched, recorded as `validationThrow` rather than `compileError`.
The worker-stack tests run both `check_schema` and `validate_inner` directly
on 2 MiB, as well as the public `validate` wrapper.

`recursive-data/*` records twelve recursive shapes (`properties`,
`patternProperties`, `additionalProperties`, object-wrapped recursive `items`,
`anyOf`, `oneOf`, `allOf`, `not`, `if`, `then`, `else`, and `ref`) at zero
depth, both sides of the port budget, and a dynamically found SDK overflow
sample. The original 1024-boundary samples are retained, with their port
expectations updated to the approved 2048 budget. Fifteen wide recipes cover
anyOf/oneOf/allOf/if/dependencies at widths 128/256/512. `wideAnyOf` adds 512 branches, including a shallow
SDK-valid / port-error sample and a real SDK overflow. Every observation runs
in a new default-stack Node process through the real SDK `Client.callTool`
and linked `InMemoryTransport` / `Server`, after `listTools` compiles the
schema. Imports are bound to the baseline in a temporary probe; SDK source
hashes are recorded in `recursiveSdkSha256`.

The uniform bound is **2048 active weighted units**. **深度预算差异已人批**
(human decision, 2026-10-21 14:25): this is the approved conservative support
policy, including earlier rejection while the SDK can still validate.
The schema compile-check bound remains independently **256**. Each schema invocation
occupies the number of JSON value nodes in its static subtree (objects include
all their values, arrays include all elements, scalars count one; string
contents are not expanded). References do not expand during this static pass.
Targets outside ordinary schema keywords are discovered lazily. Entering a
referenced target spends its own subtree weight, released on return. This
charges unused branches and non-schema code-generation inputs such as property
dependency arrays. Counting only recursive calls still falsely accepted wide
`allOf` at 42 data layers; counting only subschemas missed wide `dependencies`.
Definitions, ignored keywords and metadata also count deliberately, so a wide
schema can be refused even for shallow or empty data.

| fixture shape | last accepted data depth | first refused depth |
|---|---:|---:|
| properties / patternProperties | 291 | 292 |
| additionalProperties | 340 | 341 |
| object-wrapped items | 338 | 339 |
| anyOf / oneOf / allOf with one properties branch | 135 | 136 |
| not / if / then / else / ref | 96 / 112 / 119 / 101 / 225 | 97 / 113 / 120 / 102 / 226 |
| anyOf with 512 properties branches | none | 0 |

At width 512, anyOf still refuses depth zero (root weight 2563); allOf and
property dependencies now accept depth zero and refuse depth one. The synthetic
`{name, children: self[]}` directory-tree recipe supports **106 edges** and
refuses **107 or more**; the `{rows: [{column_0: string, ...}]}` query recipe
with per-column descriptions and required fields supports **168 columns** and
refuses **169 or more**, even though the data is only three container levels.
These values describe those exact recipes, not every directory/query schema;
metadata, ignored keywords and unused branches still count toward the budget.

Over budget returns `ValidationError::Threw`. The client projects code
`-32602` and body `Failed to validate structured content: Maximum call stack
size exceeded`; the SDK error class and this port both prefix the full message
with `MCP error -32602: `. Replay compares the actual Client error byte for byte
above its overflow boundary. Between the port boundary and the measured SDK
boundary it checks the explicit known-difference label instead of claiming
parity. `ref/recursive-499` retains its original SDK result and now carries that
label too.

`recursiveData: {shape, depth}` stores iterative data construction from an
empty object/array, avoiding JSON serialization/parser limits. The items recipe
wraps its array in `{a: ...}` to satisfy the SDK outputSchema object contract.
`comparisonData: {kind, depth}` also reconstructs deep const/enum literals and
uniqueItems data. The iterative fast-deep-equal port charges comparison depth
to the same remaining budget; otherwise const/enum/uniqueItems could bypass
schema recursion accounting. Their fixture boundaries are 1022/1023 (const),
1021/1022 (enum) and 2040/2041 (uniqueItems). The duplicate-items fixture has an
ordinary validation error below the budget; over-budget errors are explicit
message differences until the SDK also overflows.

The native V8 limit depends on shape, runtime, JIT and caller stack. The
generator asserts SDK success for supported valid samples, never a hard-coded
machine-specific overflow depth, and discovers overflow samples by a bounded
search. The round-7 RESULT records 14 keyword shapes, wide-schema controls,
per-layer stack-cost estimates and real Client measurements. This budget is a
conservative support policy, not a proof about arbitrary custom V8 stacks or
all possible compiler layouts. `recursiveDataRuntime` records the measured
runtime and budget. Schema structure and transport parser/serializer limits
are separate and are not claimed equivalent by these validator-only samples.

`named-group/*` checks matching and nonmatching data for `$`, escaped, and
Unicode group names. Names without backreferences become ordinary captures;
patterns with backreferences remain explicitly unsupported.
`format-regex-named/*` also covers forward named references and the Annex B
identity `\k`: without named groups only `k` is escaped, and subsequent
`<...>` must not hide nested groups from syntax checks or the capture limit.

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
| validation requiring >2048 active weighted JSON / comparison units | exact SDK stack-overflow RPC error; earlier rejection is explicitly labelled, including wide shallow schemas, metadata/ignored keyword costs and deep equality |
| classes with nested `[`, `&&` / `~~` / `--`, or empty classes | accepted at compile time; using the pattern returns `ValidationError::Unsupported` (the real Ajv result is preserved in the oracle) |
| `patternProperties` whose children are all always-valid and whose sibling `additionalProperties` is absent/always-valid | Ajv's `patternProperties` codegen early-returns without compiling the keys; both `check_schema` and validation skip them (`alwaysValid` early-exit, r4 GLM P1) |

## Long RegExp subjects (repair round 9, human plan A)

`string-data.mjs` runs one cold, default-stack real SDK `Client.callTool` per
process using linked in-memory transports. `stringData` records only `prefix`,
`unit`, `suffix`, `repeats`, UTF-16 `length`, and a `value`/`key` target. Replay
inflates the subject in memory; repeated multi-megabyte subjects are not stored.

All unbounded repetitions (`*`, `+`, `{n,}`) and large upper bounds receive a
weight of at least 1, independent of atom type or V8 optimizations. The weight
is the total number of groups in the pattern, with a minimum of 1. A bounded
repeat is large when its upper bound times that weight exceeds 1,048,576.
Escaped punctuation, class contents and Unicode/property escape braces are
not quantifiers. Small bounded repeats alone do not cap subject length.
The single shared matcher applies the same rule to active `patternProperties`
keys, including `additionalProperties` reuse. Ajv-skipped keys remain skipped.

**Approved limitation (human decision, 2026-10-21):** when an active regex with
unbounded/large repetition exceeds **1,048,576 (weight × UTF-16 length)**, the
port returns the SDK stack-overflow error even though the SDK may accept it.
**Output validation above 1 MB is outside this PR's acceptance requirements.**
The budget measures UTF-16 units, not bytes; group weights can reject shorter
strings (e.g. weight 3 supports at most 349,525 units). There is no uniform
one-megabyte wire-payload cap. Ordinary validation errors may also be replaced
by the overflow error above this budget.

Full formats with unbounded regexes use weight 1: email, uri, uri-reference,
uri-template, url, json-pointer, json-pointer-uri-fragment, relative-json-pointer,
byte, time, date-time, iso-time, iso-date-time, duration, hostname. This includes
helper expressions and hostname's length-gated expression. Bounded date,
ipv4/ipv6/uuid patterns, regex construction, password/binary and unknown formats
retain their existing behavior.

The 173 string cases include both sides of the budget for 51 recipes; dot,
negated classes, astral classes, Unicode properties, groups and counted repeats
use both BMP-only and mixed `a😀` subjects. The four Grok r9 patterns are covered.
Each mixed recipe has a dynamically found SDK overflow with the exact SDK error;
nonmixed optimized runs that do not overflow within the bounded search retain
an explicit valid observation and conservative-gap label. No machine-specific
breakpoint is hard-coded. Eighteen short invalid subjects are data negative
controls. Six long valid controls (five unbounded patterns at the budget, one
Ajv-skipped key above it) and two literal/bounded controls prevent a blanket
length cap. Six additional format classes retain SDK observations on both sides
without claiming that their atom-only or length-gated expressions overflow.

`portRegexpLimit` requires precisely code -32602 and
`MCP error -32602: Failed to validate structured content: Maximum call stack size exceeded`.
`knownDifference: conservative-regexp-stack-budget` labels an SDK success or a
different ordinary error where the port returns overflow; these observations
are never parity passes. Below the budget, unsupported regex syntax still has
`portUnsupported`. Source guards, SDK hashes and cold/default-stack probes are
unchanged. Measurements and difference intervals are recorded in
`_tmp/20261020-a65-pr10b/RESULT.md`, round 9. The separate depth budget was
later raised to 2048 and human-approved (2026-10-21); see round 10 and the
gateway ledger entry A6.5-MCP-OUTSCHEMA-DEPTH-001.
