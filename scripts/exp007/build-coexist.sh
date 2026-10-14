#!/bin/sh
# Build only from Rust sources; never download tools or browsers as binaries.
set -eu
cd "$(dirname "$0")/../.."
export CARGO_HOME="$PWD/_build/cargo-home"
export CARGO_TARGET_DIR="$PWD/_build/exp007-target"
export CARGO_BUILD_JOBS=2
cargo build --locked --manifest-path scripts/exp007/prefix-router/Cargo.toml
cargo build --locked --manifest-path scripts/exp008/proxy/Cargo.toml
if [ ! -x _build/tools/bin/wasm-bindgen ]; then
  CARGO_TARGET_DIR="$PWD/_build/install-wbg" cargo install wasm-bindgen-cli --version 0.2.129 --locked --root "$PWD/_build/tools" -j 2
fi
python3 scripts/exp007/build.py
