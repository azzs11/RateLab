#!/bin/sh
# Builds the Rust core for the browser and generates the JS bindings into src/wasm-pkg.
# Needs: rustup target add wasm32-unknown-unknown
#        cargo install wasm-bindgen-cli --version 0.2.130 --locked
set -eu
cd "$(dirname "$0")/../.."
cargo build -p wasm --target wasm32-unknown-unknown --release --locked
wasm-bindgen --target web --out-dir web/src/wasm-pkg \
  target/wasm32-unknown-unknown/release/wasm.wasm
