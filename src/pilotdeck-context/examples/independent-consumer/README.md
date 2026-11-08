# Independent source consumer

This small binary has its own `[workspace]` and consumes the three extracted
libraries plus `pilotdeck-protocol` through relative paths. It does not depend on
`pilotdeck-gateway`, Kernel, credentials, a model endpoint, or global application
configuration. The assertions demonstrate API linkage, not full behavioral parity
or actual budget enforcement.

From the PilotDeck repository root:

```sh
cargo run --manifest-path src/pilotdeck-context/examples/independent-consumer/Cargo.toml
```

After generating and retaining this consumer's `Cargo.lock`, subsequent runs can
use the same command with `--locked`. No build/run result is implied by this README.

This is a source-checkout example, not a registry or Mint distribution package.
Keep the source workspace manifest, root `LICENSE`, protocol/tokenizer crates and
`src/vendor/tiktoken-rs` (including its embedded ranks asset). Copying only these
three library directories is insufficient: their package metadata is inherited
from the source workspace and budget/context transitively use tokenizer/vendor.
The independent lockfile resolves this consumer's dependency graph; it does not
require building the gateway or the complete source workspace.
