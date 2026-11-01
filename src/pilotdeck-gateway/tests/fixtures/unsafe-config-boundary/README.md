# #448 unsafe-guard cargo-configuration boundary controls

`negative_controls.py` injects the cargo-configuration conditions that
`compiler_enforces_gateway_boundary` (in `tests/unsafe_boundary.rs`) must
reject and that no file inside the test cwd can express: an out-of-tree
`.cargo/config.toml` with `rustflags = ["--cfg", "pdrs_448_negative_control"]`
on the invocation directory chain, `--config` tokens in the runner argv, the
toolchain-selection variables (`RUSTDOC`, `RUSTC_BOOTSTRAP`, an exported or
foreign `RUSTUP_TOOLCHAIN`), and a runner process image named `cargo` that the
`CARGO` variable does not point at (the out-of-tree `[env] force = true`
impersonation). No product source is replaced; the tree stays untouched.

Run (Linux only — the `/proc` invocation boundary is Linux-specific):

```
CARGO_TARGET_DIR=<shared target> python3 negative_controls.py NEW_OUTPUT_DIR [scenario ...]
```

Scenario order: `baseline-cargo`, `baseline-direct`, the attack scenarios
(`env-*`, `cli-config-*`, `cargo-identity-fake`,
`cargo-out-of-tree-config`), and `baseline-cargo-restored`. Every scenario
runs before the summary; a row outside its expected state (`PASS` for the
baselines, `KILLED` at the dedicated `#448` assertion otherwise) makes the
exit code non-zero. The out-of-tree attack recompiles the guard target under
the injected rustflags and the restored baseline rebuilds nothing else, so the
full run takes a few minutes on a warm target directory.
