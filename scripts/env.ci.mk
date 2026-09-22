# barryOS — toolchain paths for a plain Linux host (the CI runner).
#
# Selected by scripts/env.mk when neither the sandbox prefix (/home/z/.opt) nor
# an msys2 install is present: everything comes from distro packages on PATH.

NASM     ?= nasm
XORRISO  ?= xorriso
OBJCOPY  ?= objcopy
READELF  ?= readelf
FILE     ?= file
MFORMAT  ?= mformat
MCOPY    ?= mcopy
MMD      ?= mmd
# dosfstools installs mkfs.fat into /usr/sbin, which is not always on PATH for
# a non-root user -- and the CI runner is one.
MKFSFAT  ?= $(shell command -v mkfs.fat 2>/dev/null || echo /usr/sbin/mkfs.fat)
PYTHON   ?= python3

# The EFI application.  clang with the Windows target emits MS-ABI COFF, and
# then it has to be linked into PE32+.  Ubuntu's own `ld` has no i386pep
# emulation at all, so the linker must be the mingw-w64 cross binutils
# (package binutils-mingw-w64-x86-64).
EFI_CC     := clang
EFI_CFLAGS := -target x86_64-unknown-windows
LD         := x86_64-w64-mingw32-ld

# The same mingw-w64 toolchain compiles the test PE (package
# gcc-mingw-w64-x86-64).
MINGW_GCC  := x86_64-w64-mingw32-gcc

# QEMU for scripts/check.sh.  The ovmf package puts the 4 MiB images here.
QEMU          := qemu-system-x86_64
QEMU_BIOS_DIR := /usr/share/qemu
OVMF_CODE     := /usr/share/OVMF/OVMF_CODE_4M.fd
OVMF_VARS     := /usr/share/OVMF/OVMF_VARS_4M.fd
