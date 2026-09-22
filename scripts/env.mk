# barryOS — toolchain paths for Make (sourced by Makefile via `include`)
# Mirrors scripts/env.sh but in Make syntax.

BARRYOS_PREFIX ?= /home/z/.opt
MSYS2_ROOT     ?= /e/other/msys64

# Three environments, picked by what is actually present:
#
#   $(BARRYOS_PREFIX)  the Linux build sandbox this project grew up in
#   $(MSYS2_ROOT)      the Windows host, using the msys2 toolchain
#   neither            a plain Linux host — the CI runner
#
# Testing for the msys2 root rather than for Windows is deliberate: `OS` is not
# set in the msys make environment we run under, so it cannot be used here, and
# /e/other/msys64 is a path no Linux host will ever have.  Getting this wrong
# is not subtle — the msys branch points every tool at a Windows path.
ifeq ($(wildcard $(BARRYOS_PREFIX)/usr/bin),)

ifneq ($(wildcard $(MSYS2_ROOT)/usr/bin),)
include $(ROOT)/scripts/env.msys.mk
else
include $(ROOT)/scripts/env.ci.mk
endif

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
