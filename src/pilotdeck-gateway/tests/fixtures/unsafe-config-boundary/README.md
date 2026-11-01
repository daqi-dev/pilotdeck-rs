# #448 unsafe-guard cargo-configuration boundary controls

`negative_controls.py` injects the cargo-configuration conditions that
`compiler_enforces_gateway_boundary` (in `tests/unsafe_boundary.rs`) must
reject and that no file inside the test cwd can express: an out-of-tree
`.cargo/config` or `.cargo/config.toml` with
`rustflags = ["--cfg", "pdrs_448_negative_control"]` on the invocation
directory chain (the direct parent's, and every live cargo ancestor's own
invocation directory), `--config` tokens in the runner argv (the direct
parent's and every live cargo ancestor's), the
toolchain-selection variables (`RUSTDOC`, `RUSTC_BOOTSTRAP`, an exported,
foreign, or linked-non-host `RUSTUP_TOOLCHAIN`), and a runner process image
named `cargo` that the `CARGO` variable does not point at (the out-of-tree
`[env] force = true` impersonation). No product source is replaced; the tree
stays untouched.

The toolchain channel is parsed with the same strict shape as the Rust guard
(`[toolchain]` section, `channel = "` prefix, `"` suffix): a differently
formatted `rust-toolchain.toml` fails loudly on both sides instead of parsing
here while the guard fails closed on it.

Run (Linux only — the `/proc` invocation boundary is Linux-specific; on other
platforms the guard itself fails closed unless
`PDRS_UNSAFE_BOUNDARY_ALLOW_NON_LINUX=1` acknowledges the manual-review
boundary, and this runner refuses to start because it cannot inject the
conditions):

```
CARGO_TARGET_DIR=<shared target> python3 negative_controls.py NEW_OUTPUT_DIR [scenario ...]
```

rustup is required to locate the real toolchain cargo (the `from-file`,
`linked`, and `cargo-test-runner-layout-*` scenarios need it; the runner
currently refuses to start without it).

Scenario order: `baseline-cargo`, `baseline-direct`, the attack scenarios
(`env-*`, `cli-config-*`, `cargo-identity-fake`,
`cargo-out-of-tree-config` and its bare-`.cargo/config` twin,
`cargo-test-runner-layout-*`), and `baseline-cargo-restored`. Positive controls frame the attacks:
`env-rustup-toolchain-from-file` (the pinned channel plus the host triple the
running cargo reports, with the toolchain-file source) and
`cargo-test-runner-layout-baseline` (a matching cargo grandparent) prove the allow
branches stay reachable while their paired attack rows
(`env-rustup-toolchain-linked`, `cargo-test-runner-layout-config`,
`cargo-test-runner-layout-config-file`) must die at the same
assertions. These are two-layer runner layouts in which a real cargo process
provably stays alive as the guard's grandparent; they are not the real
`cargo nextest run` ancestry (cb8 probes show cargo exec-replaces into
external subcommands, so by test time no live cargo carries the `--config`
token or its invocation directory — that absence is the guard module
header's declared manual-review boundary). The layout is reproduced where
cargo provably spawns a child that stays alive: a copy of the real cargo
binary running `cargo test` on a one-target sleeper whose harness-free test
binary is named `cargo-nextest` and spawns the guard;
`cargo-test-runner-layout-config` puts the `--config` token in the cargo
grandparent's argv and `cargo-test-runner-layout-config-file` puts the
out-of-tree `.cargo/config.toml` on the cargo grandparent's own invocation
directory. Every scenario runs before the summary;
a row outside its expected state (`PASS` for the positive controls, `KILLED`
at the dedicated `#448` assertion otherwise) makes the exit code non-zero.
The out-of-tree attack recompiles the guard target under the injected
rustflags and the restored baseline rebuilds nothing else, so the full run
takes a few minutes on a warm target directory.
