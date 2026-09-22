#!/usr/bin/env bash
# Builds the replay reader for `wasm32-unknown-unknown` and runs `wasm-bindgen` over it.
#
#   crates/v2xw-wasm/scripts/build-wasm.sh [--dev] [out-dir]
#
# Default out-dir is `target/wasm-pkg`, as a Node/CommonJS package; pass `--web` for an
# ES module for the browser.
#
# ── Why this script exists rather than a plain `cargo build --target wasm32-…` ──────────
#
# The recording container's compression codecs are C (`zstd-sys` and `lz4-sys`, reached
# through `mcap`), so building the reader for WebAssembly needs a `clang` with the
# WebAssembly back end and a set of freestanding libc headers.
#
#  * Apple's `clang` has **no** `wasm32` target: `clang -print-targets` does not list it,
#    and `cc` fails with "No available targets are compatible with triple
#    wasm32-unknown-unknown". Homebrew's `llvm` does have it (`brew install llvm`).
#  * `wasm32-unknown-unknown` has no sysroot, so `#include <string.h>` has nowhere to come
#    from. `zstd-sys` ships a `wasm-shim/` directory of exactly these stubs for this
#    reason; `lz4-sys` does not, and needs the same ones, so the shim is put on the
#    include path for both. `limits.h`, `stddef.h` and `stdint.h` are freestanding headers
#    and come from clang itself.
#
# Neither of those is a choice this crate made; both are properties of building a C codec
# for a target with no C library. If `mcap`'s `lz4` feature is ever turned off upstream
# (nothing in this workspace writes or reads lz4 — the wire specification names zstd and
# only zstd, §2.6) the shim is still needed for `zstd-sys`, which ships it.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../../.." && pwd)"

profile=release
target_dir=release
web=0
out=""
for arg in "$@"; do
  case "$arg" in
    --dev) profile=dev; target_dir=debug ;;
    --web) web=1 ;;
    *) out="$arg" ;;
  esac
done
out="${out:-$root/target/wasm-pkg}"

# A clang that can target WebAssembly. Override with WASM_CC.
cc="${WASM_CC:-}"
if [[ -z "$cc" ]]; then
  for candidate in /opt/homebrew/opt/llvm/bin/clang /usr/local/opt/llvm/bin/clang /usr/lib/llvm/bin/clang clang; do
    if [[ -x "$candidate" ]] && "$candidate" -print-targets 2>/dev/null | grep -q wasm32; then
      cc="$candidate"
      break
    fi
  done
fi
if [[ -z "$cc" ]]; then
  echo "error: no clang with a wasm32 target found. Install one (macOS: brew install llvm)" >&2
  echo "       or set WASM_CC to one." >&2
  exit 1
fi
ar="${WASM_AR:-$(dirname "$cc")/llvm-ar}"
[[ -x "$ar" ]] || ar="$(command -v llvm-ar || echo ar)"

# The freestanding libc stubs, from whichever `zstd-sys` the lockfile resolved.
shim="${WASM_LIBC_SHIM:-$(ls -d "${CARGO_HOME:-$HOME/.cargo}"/registry/src/*/zstd-sys-*/wasm-shim 2>/dev/null | head -1)}"
if [[ -z "$shim" || ! -d "$shim" ]]; then
  echo "error: no zstd-sys wasm-shim found; run a host build first so the crate is vendored," >&2
  echo "       or set WASM_LIBC_SHIM to a directory of freestanding libc headers." >&2
  exit 1
fi

echo "clang: $cc"
echo "shim:  $shim"

CC_wasm32_unknown_unknown="$cc" \
AR_wasm32_unknown_unknown="$ar" \
CFLAGS_wasm32_unknown_unknown="-I$shim" \
  cargo build --manifest-path "$root/Cargo.toml" \
    -p v2xw-wasm --lib --target wasm32-unknown-unknown --profile "$profile"

wasm="$root/target/wasm32-unknown-unknown/$target_dir/v2xw_wasm.wasm"
bindgen="${WASM_BINDGEN:-wasm-bindgen}"
command -v "$bindgen" >/dev/null || {
  echo "error: $bindgen not on PATH. cargo install wasm-bindgen-cli --version 0.2.128" >&2
  exit 1
}

if [[ "$web" == 1 ]]; then
  "$bindgen" --target web --out-dir "$out" --out-name v2xw_replay "$wasm"
else
  "$bindgen" --target nodejs --out-dir "$out" --out-name v2xw_replay "$wasm"
fi
cp "$here/../js/replay.js" "$out/replay.js"
echo "wrote $out"
ls -la "$out"
