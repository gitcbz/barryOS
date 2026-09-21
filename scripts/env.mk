# barryOS — toolchain paths for Make (sourced by Makefile via `include`)
# Mirrors scripts/env.sh but in Make syntax.

BARRYOS_PREFIX ?= /home/z/.opt

export PATH := $(BARRYOS_PREFIX)/usr/bin:$(HOME)/.local/bin:$(HOME)/.cargo/bin:$(PATH)
export LD_LIBRARY_PATH := $(BARRYOS_PREFIX)/usr/lib/x86_64-linux-gnu:$(BARRYOS_PREFIX)/lib/x86_64-linux-gnu:$(LD_LIBRARY_PATH)

QEMU       := qemu-system-x86_64
QEMU_BIOS_DIR := $(BARRYOS_PREFIX)/firmware
OVMF_CODE  := $(BARRYOS_PREFIX)/usr/share/OVMF/OVMF_CODE_4M.fd
OVMF_VARS  := $(BARRYOS_PREFIX)/usr/share/OVMF/OVMF_VARS_4M.fd
