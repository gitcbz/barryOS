# barryOS — toolchain paths for Make (sourced by Makefile via `include`)
# Mirrors scripts/env.sh but in Make syntax.

BARRYOS_PREFIX ?= /home/z/.opt

# The Linux sandbox prefix only exists in the build sandbox.  On the Windows
# host it is absent, so fall back to scripts/env.msys.mk (msys2 toolchain).
ifeq ($(wildcard $(BARRYOS_PREFIX)/usr/bin),)

include $(ROOT)/scripts/env.msys.mk

else

export PATH := $(BARRYOS_PREFIX)/usr/bin:$(HOME)/.local/bin:$(HOME)/.cargo/bin:$(PATH)
export LD_LIBRARY_PATH := $(BARRYOS_PREFIX)/usr/lib/x86_64-linux-gnu:$(BARRYOS_PREFIX)/lib/x86_64-linux-gnu:$(LD_LIBRARY_PATH)

QEMU       := qemu-system-x86_64
QEMU_BIOS_DIR := $(BARRYOS_PREFIX)/firmware
OVMF_CODE  := $(BARRYOS_PREFIX)/usr/share/OVMF/OVMF_CODE_4M.fd
OVMF_VARS  := $(BARRYOS_PREFIX)/usr/share/OVMF/OVMF_VARS_4M.fd

# clang with the Windows target produces MS-ABI code OVMF accepts.
EFI_CC     := clang-19
EFI_CFLAGS := -target x86_64-unknown-windows
PYTHON     ?= python3
MKFSFAT    ?= $(BARRYOS_PREFIX)/usr/sbin/mkfs.fat

endif
