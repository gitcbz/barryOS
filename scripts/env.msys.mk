# barryOS — toolchain paths for Make on Windows + msys2.
#
# Included automatically by scripts/env.mk when the Linux sandbox prefix
# (/home/z/.opt) is not present, i.e. when building on the Windows host.
#
# Tools live in the msys2 install:
#   <root>/usr/bin      nasm, xorriso, ld, objcopy, readelf, file, mkfs.fat
#   <root>/mingw64/bin  gcc (MS ABI), mformat/mcopy/mmd/mdir
#
# Override the root if msys2 lives elsewhere:
#   make MSYS2_ROOT=/c/msys64 all

MSYS2_ROOT ?= /e/other/msys64

export PATH := $(MSYS2_ROOT)/usr/bin:$(MSYS2_ROOT)/mingw64/bin:$(PATH)

# --- assembler / image tools (msys repo) ------------------------------------
NASM     := $(MSYS2_ROOT)/usr/bin/nasm
XORRISO  := $(MSYS2_ROOT)/usr/bin/xorriso
LD       := $(MSYS2_ROOT)/usr/bin/ld
OBJCOPY  := $(MSYS2_ROOT)/usr/bin/objcopy
READELF  := $(MSYS2_ROOT)/usr/bin/readelf
FILE     := $(MSYS2_ROOT)/usr/bin/file

# --- FAT tooling (mingw64 repo: mingw-w64-x86_64-mtools) --------------------
MFORMAT  := $(MSYS2_ROOT)/mingw64/bin/mformat
MCOPY    := $(MSYS2_ROOT)/mingw64/bin/mcopy
MMD      := $(MSYS2_ROOT)/mingw64/bin/mmd
MKFSFAT  := $(MSYS2_ROOT)/usr/bin/mkfs.fat

# --- EFI application compiler -------------------------------------------------
# efi_types.h defines EFIAPI as __attribute__((ms_abi)), which is the native
# ABI of the mingw-w64 x86_64 target, so gcc produces the same PE32+ the Linux
# build got from `clang -target x86_64-unknown-windows`.  No clang needed.
EFI_CC     := $(MSYS2_ROOT)/mingw64/bin/gcc
EFI_CFLAGS :=

# --- python (for scripts/mk-uefi-img.py) -------------------------------------
PYTHON   ?= $(MSYS2_ROOT)/mingw64/bin/python3

# --- rustup / cargo ----------------------------------------------------------
# msys's $HOME is not the Windows user profile, so RUSTUP_HOME arrives unset
# here and the cargo shim fails with "rustup could not choose a version of
# cargo to run ... no default is configured".  Derive the real directories from
# wherever `cargo` sits on PATH.  Override if your rustup lives elsewhere:
#   make CARGO_HOME='C:/Users/me/.cargo' RUSTUP_HOME='C:/Users/me/.rustup' all
CARGO_BIN   := $(shell command -v cargo 2>/dev/null)
# NOTE: $(dir) returns its argument unchanged when it already ends in '/', so
# strip the trailing separator between the two $(dir) calls.
CARGO_HOME  ?= $(shell cygpath -m "$(patsubst %/,%,$(dir $(patsubst %/,%,$(dir $(CARGO_BIN)))))" 2>/dev/null)
RUSTUP_HOME ?= $(patsubst %/.cargo,%/.rustup,$(CARGO_HOME))
export CARGO_HOME
export RUSTUP_HOME

# --- temp directory ----------------------------------------------------------
# TMP/TEMP arrive empty in the msys shell.  `build-std` compiles
# compiler_builtins' build script for the *host* (msvc) toolchain, and with no
# TMP the MSVC linker falls back to C:\WINDOWS and dies with
#   LNK1104: cannot open file 'C:\WINDOWS\lnk{...}.tmp'
# Point it at the msys2 tmp dir, whose path is plain ASCII (the Windows profile
# path for this user contains non-ASCII characters).
TMP    := $(or $(TMP),$(shell cygpath -m "$(MSYS2_ROOT)/tmp" 2>/dev/null))
TEMP   := $(or $(TEMP),$(TMP))
TMPDIR := $(or $(TMPDIR),$(MSYS2_ROOT)/tmp)
export TMP
export TEMP
export TMPDIR

# --- QEMU: not installed on the Windows host ---------------------------------
# The Linux sandbox remains the place to run scripts/check.sh (L2/L3 boot
# tests).  `make all` works here; run-bios/run-uefi/check do not.
QEMU          :=
QEMU_BIOS_DIR :=
OVMF_CODE     :=
OVMF_VARS     :=
