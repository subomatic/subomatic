#!/bin/sh
# Build the Subomatic web app: compile the WASM core into web/pkg with wasm-pack.
# Run this before serving web/ locally, and it's the build step the Pages
# workflow uses before publishing. Idempotent.
set -e

# Prefer the rustup toolchain over a system/Homebrew rustc, which may not carry
# the wasm32 target in its sysroot (wasm-pack uses the first rustc on PATH).
if [ -d "$HOME/.cargo/bin" ]; then
  PATH="$HOME/.cargo/bin:$PATH"
  export PATH
fi

if ! command -v wasm-pack >/dev/null 2>&1; then
  echo "error: wasm-pack not found — install it from" >&2
  echo "       https://rustwasm.github.io/wasm-pack/installer/" >&2
  exit 1
fi

if command -v rustup >/dev/null 2>&1; then
  rustup target add wasm32-unknown-unknown
fi

wasm-pack build crates/subomatic-wasm --target web --out-dir ../../web/pkg
