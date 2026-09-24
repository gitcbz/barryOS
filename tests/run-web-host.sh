#!/usr/bin/env bash
# Build and run the host-side page renderer (tests/web-host.rs).
#
#   tests/run-web-host.sh                     # the built-in markup cases
#   tests/run-web-host.sh https://example.com/
#
# Nothing here needs the kernel toolchain — only a rustc.
set -u

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${TMPDIR:-/tmp}/barryos-web-host"
RUSTC="${RUSTC:-rustc}"

"$RUSTC" -O --edition 2021 -A dead_code -A unused_imports \
    "$ROOT/tests/web-host.rs" -o "$OUT" || exit 1

exec "$OUT" "$@"
