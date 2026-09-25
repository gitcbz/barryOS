#!/usr/bin/env bash
# Build and run the host-side image decoders (tests/img-host.rs).
#
#   tests/run-img-host.sh picture.png another.jpg
#
# Each file is decoded with the kernel's own decoder and written beside itself
# as a PPM.  Nothing here needs the kernel toolchain — only a rustc.
set -u

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${TMPDIR:-/tmp}/barryos-img-host"
RUSTC="${RUSTC:-rustc}"

"$RUSTC" -O --edition 2021 -A dead_code -A unused_imports \
    "$ROOT/tests/img-host.rs" -o "$OUT" || exit 1

exec "$OUT" "$@"
