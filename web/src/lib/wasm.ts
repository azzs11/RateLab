// Loads the Rust core compiled to WebAssembly, once.

import init, * as core from "../wasm-pkg/wasm.js";

let ready: Promise<typeof core> | null = null;

export function wasm(): Promise<typeof core> {
  ready ??= init().then(() => core);
  return ready;
}

export type Core = typeof core;
