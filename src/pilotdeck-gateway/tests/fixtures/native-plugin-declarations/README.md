# Native disk plugin declarations

`oracle.json` derives from the unmodified fixed TypeScript `loadPluginFromPath` at
`cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`, using Node 22.22.0 / ICU 78.2 on mbp2.
No provider, subprocess contribution, MCP endpoint, LSP provider, or user home was used.

The raw 15 per-case JSON records, exact UTF-8 input bytes (base64 plus SHA256),
original TS output, fixed source hashes and Git blob IDs are retained in
`_tmp/20261108-cli-completion/plugin-declarations/oracle-capture/project-source/`
in the main evidence repository. Its `driver.mts` imports the real loader; it does
not reimplement projection. The source files are hash-checked before and after.
The normalized source record has SHA256
`e738e4c02e3efad8fc68d0a92b573ee2581f5dee2a575514469c491f2254eb13`.
The earlier capture with the unconstrained runtime source string `user` is kept
separately as a diagnostic; this fixture uses the formal `project` source.

Normalization for this fixture is limited to: decoding the retained UTF-8 file
bytes; replacing each private case directory in contribution paths by a relative
path; sorting frontmatter object keys because the existing Rust surface is a
HashMap; and selecting fields represented by `LoadedPlugin`. Manifest metadata
and source projection are retained in the raw TS records, not fabricated as Rust
fields. MCP/LSP strings and object order, including the lone surrogate in MCP
args, remain raw `JsValue` expectations. JSON missing fields remain missing.

One expectation deliberately differs from the original TS output:
`external-configured-dir` contributes no Markdown. This is the already accepted
`A6.3-PLUGIN-MARKDOWN-ROOT-001` scope (configured base `..`), approval chain R5.
The real TS output containing `EXTERNAL` remains in the raw record. This approval
does not cover hooks: the `external-hook-file` case must load its regular JSON
file as TS does. Legitimate plugin-root and manifest aliases are covered by a
separate production-reader test. Descriptor NOFOLLOW/NONBLOCK, regular-file
checks and the native reload byte/value budgets are native resource/safety
constraints; they are not claimed as identical to the unbounded TS reader.

Tests under `runtime::plugins::loader::native::tests` call the same loader and
generation owner as production. The small-budget checks do not change global
limits. The generation test uses the real 10,000-entry limit and verifies failure
preserves both publication and an already acquired contribution lease.

The separate four-case SDK MCP composition oracle is in the main evidence
`oracle-capture/mcp-final/`. It runs the real registry getter and session bundle
up to an injected provider capture, then throws a sentinel before any MCP process
starts. Disabled SDK entries are filtered before the real plugin/SDK composition;
they do not suppress the plugin name. That evidence does not exercise SDK
admission or an actual MCP connection.
