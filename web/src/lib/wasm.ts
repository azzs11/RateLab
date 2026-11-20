// Loads the Rust core compiled to WebAssembly, once. Called as early as possible so the
// download and compile overlap with the first render.

import init, * as core from "../wasm-pkg/wasm.js";
import wasmUrl from "../wasm-pkg/wasm_bg.wasm?url";

let ready: Promise<typeof core> | null = null;

export function wasm(): Promise<typeof core> {
  ready ??= init({ module_or_path: wasmUrl }).then(() => core);
  return ready;
}

export type Core = typeof core;
