#!/usr/bin/env bash
# barryOS toolchain environment activation
# Source this in every shell: source /home/z/my-project/barryOS/scripts/env.sh

# Rust nightly (installed via rustup to ~/.cargo)
. "$HOME/.cargo/env"

# User-local prefix (NASM built from source)
export PATH="$HOME/.local/bin:$PATH"

# Extracted .deb toolchain prefix (QEMU, xorriso, mtools, OVMF)
export BARRYOS_PREFIX="/home/z/.opt"
export PATH="$BARRYOS_PREFIX/usr/bin:$PATH"
export LD_LIBRARY_PATH="$BARRYOS_PREFIX/usr/lib/x86_64-linux-gnu:$BARRYOS_PREFIX/lib/x86_64-linux-gnu:${LD_LIBRARY_PATH:-}"

# OVMF firmware location (UEFI testing) — Fedora build includes FatDxe
export OVMF_CODE="$BARRYOS_PREFIX/firmware/OVMF_CODE.fd"
export OVMF_VARS="$BARRYOS_PREFIX/firmware/OVMF_VARS.fd"
# SeaBIOS firmware dir (BIOS testing) — unified firmware dir holds both
export QEMU_BIOS_DIR="$BARRYOS_PREFIX/firmware"
# clang (used to build the EFI app; MS-ABI code OVMF accepts)
export PATH="$BARRYOS_PREFIX/usr/bin:$PATH"

# barryOS project root
export BARRYOS_ROOT="/home/z/my-project/barryOS"

# Verify
command -v rustc >/dev/null && command -v nasm >/dev/null && command -v qemu-system-x86_64 >/dev/null && command -v xorriso >/dev/null && command -v mformat >/dev/null || { echo "[env] WARN: some tools missing"; }
