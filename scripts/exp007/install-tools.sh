#!/bin/sh
# Run at worktree root. Never changes a global cargo installation.
set -eu
probe_root=$(pwd)
export CARGO_HOME="$probe_root/_build/cargo-home"
mkdir -p "$probe_root/_build/tools" "$probe_root/_tmp/exp007"
# Release trunk build scripts were SIGKILLed on this host; debug install succeeded.
CARGO_TARGET_DIR="$probe_root/_build/install-trunk-debug" cargo install trunk --version 0.21.14 --locked --debug --root "$probe_root/_build/tools" -j 2
CARGO_TARGET_DIR="$probe_root/_build/install-wbg" cargo install wasm-bindgen-cli --version 0.2.129 --locked --root "$probe_root/_build/tools" -j 2
CARGO_TARGET_DIR="$probe_root/_build/install-dx" cargo install dioxus-cli --version 0.7.9 --locked --features disable-telemetry --root "$probe_root/_build/tools" -j 2
