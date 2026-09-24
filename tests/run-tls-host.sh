#!/usr/bin/env bash
# Build and run the host-side TLS driver (tests/tls-host.rs).
#
# The kernel sources are compiled for the host here, so this needs nothing from
# the kernel toolchain — only a rustc and a trust store, which the kernel build
# generates into build/truststore.rs.  Runs on Linux and on msys.
#
#   tests/run-tls-host.sh                     # just the crypto vectors
#   tests/run-tls-host.sh www.bilibili.com    # ... and a real handshake
set -u

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${TMPDIR:-/tmp}/barryos-tls-host"

RUSTC="${RUSTC:-rustc}"
if ! command -v "$RUSTC" >/dev/null 2>&1; then
    echo "run-tls-host: no rustc on PATH" >&2
    exit 2
fi

if [ ! -f "$ROOT/build/truststore.rs" ]; then
    echo "run-tls-host: build/truststore.rs is missing" >&2
    echo "               run 'make truststore' first" >&2
    exit 2
fi

# The kernel is no_std and so is most of what is under test, but the driver is
# an ordinary host program and links std.  -A dead_code because half of each
# module exists for the kernel's benefit and is unused here.
"$RUSTC" -O --edition 2021 -A dead_code -A unused_imports \
    "$ROOT/tests/tls-host.rs" -o "$OUT" || exit 1

exec "$OUT" "$@"
