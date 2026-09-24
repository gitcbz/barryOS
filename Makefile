#!/usr/bin/env bash
# barryOS — build & orchestration
# Sources scripts/env.sh automatically so `make` works in a fresh shell.
SHELL := /bin/bash
ROOT  := $(abspath $(dir $(lastword $(MAKEFILE_LIST))))

include $(ROOT)/scripts/env.mk

KERN_DIR    := $(ROOT)/kernel
BOOT_DIR    := $(ROOT)/boot
BUILD       := $(ROOT)/build
LOGS        := $(BUILD)/logs
ISO_ROOT    := $(BUILD)/iso_root

# `?=` so scripts/env.msys.mk can point these at absolute msys2 paths when
# building on the Windows host; a plain PATH lookup is used otherwise.
CARGO       ?= cargo
NASM        ?= nasm
GCC         ?= gcc
LD          ?= ld
OBJCOPY     ?= objcopy
XORRISO     ?= xorriso
MFORMAT     ?= mformat
MCOPY       ?= mcopy
MMD         ?= mmd
FILE        ?= file
READELF     ?= readelf
QEMU_IMG    ?= qemu-img
# Cross compiler for the test PE.  Both mingw-w64 gcc (msys2) and
# x86_64-w64-mingw32-gcc (Linux) produce the same PE.
MINGW_GCC   ?= gcc
EFI_CC      ?= clang-19
EFI_CFLAGS  ?= -target x86_64-unknown-windows
PYTHON      ?= python3
MKFSFAT     ?= mkfs.fat

TARGET      := x86_64-unknown-none
PROFILE     := release
KERN_PROFILE_FLAG := $(if $(filter release,$(PROFILE)),--release,)

# outputs
KERN_ELF    := $(BUILD)/kernel.elf
KERN_BIN    := $(BUILD)/kernel.bin
# The image the BIOS path reads.  The loader stages everything below 1 MiB
# before moving it to 0x100000, and that window is 572 KiB and cannot grow, so
# what goes on the disk is compressed and decompressed on the way up.  The
# UEFI loader reads the file through EFI and uses the plain one.
KERN_LZ     := $(BUILD)/kernel.lz
MBR_BIN     := $(BUILD)/mbr.bin
STAGE2_BIN  := $(BUILD)/stage2.bin
# The boot chain, embedded into the kernel so the installer can write it to a
# target disk whatever medium it booted from.
BOOTIMG_RS  := $(BUILD)/bootimg.rs
# A tiny PE32+ program, embedded the same way so the loader has something to
# run without needing a disk first.
TEST_EXE    := $(BUILD)/hello.exe
# stage2 with the kernel size patched in, for the BIOS image only.
STAGE2_IMG  := $(BUILD)/stage2.patched.bin
BIOS_IMG    := $(BUILD)/barryOS-bios.img
EFI_APP     := $(BUILD)/BOOTX64.EFI
UEFI_IMG    := $(BUILD)/barryOS-uefi.img
ISO         := $(BUILD)/barryOS.iso

# Which certificate authorities the TLS client believes, generated from the
# Mozilla bundle.  Defined here rather than beside its rule below, because a
# `:=` variable expands where it is defined and the kernel rule needs to have
# already seen it.  TRUST_ROOTS prunes the list: the full 121 roots are 42 KiB
# of DER and the BIOS loader gives the kernel 572 KiB in total.  For a name to
# survive pruning it has to be one that actually issues to the sites this
# browser is expected to reach, or a root that several intermediates chain to.
# Raise the ceiling in boot/bios/stage2.asm to carry the whole set.
CACERT        := $(BUILD)/cacert.pem
TRUSTSTORE_RS := $(BUILD)/truststore.rs
TRUST_ROOTS   ?= GlobalSign Root CA - R3,\
                 GlobalSign Root CA - R6,\
                 GlobalSign Root E46,\
                 DigiCert Global Root G2,\
                 DigiCert Global Root G3,\
                 DigiCert Global Root CA,\
                 ISRG Root X1,\
                 ISRG Root X2,\
                 USERTrust ECC,\
                 USERTrust RSA,\
                 GTS Root R1,\
                 GTS Root R4,\
                 SSL.com TLS ECC Root CA 2022,\
                 Sectigo Public Server Authentication Root,\
                 TrustAsia,\
                 GlobalSign RSA OV SSL,\
                 Certum Trusted Network CA 2,\
                 COMODO Certification Authority
# The trust store generator parses certificates, so it needs the `cryptography`
# package -- which the msys2 python does not have, and which is not needed for
# anything else here.  Kept as its own variable rather than reusing PYTHON, so
# that pointing it somewhere else does not also change how the images are made.
# On Linux (CI) this is a python3 with the cryptography package installed; on
# Windows scripts/env.msys.mk points it at an interpreter that has it.
TRUST_PYTHON  ?= python3

# sizes
BIOS_IMG_SECTORS := 8192       # 4 MiB BIOS image
# How much room the loader has for the kernel image.  The BIOS path stages the
# whole image in conventional memory -- 0x10000 to 0x9F000, the top being the
# start of video RAM -- and copies it up to 0x100000 in one protected-mode
# switch, so that window is the ceiling.  Must match KERNEL_STAGE_MAX in
# boot/bios/stage2.asm; changing one without the other means a kernel that
# builds and then refuses to load.
KERNEL_MAX_BYTES := 585728     # 0x8F000 = 0x9F000 - 0x10000
                               # the UEFI loader has no such limit (it reads the
                               # file through EFI), but shares this guard so one
                               # image size has to satisfy every boot path
KERNEL_IMG_LBA   := 41         # must match KERNEL_DISK_LBA in stage2.asm

.PHONY: all kernel bios uefi iso vmdk pe-check truststore clean check run-bios run-uefi fmt clippy help

# pe-check is part of `all`: the test PE and the loader's Win32 table are two
# files that have to agree, and nothing else in the build would notice if they
# stopped.  See tests/pe-contract.py.
all: kernel bios uefi iso pe-check

help:
	@echo "barryOS build targets:"
	@echo "  all        build kernel + BIOS img + UEFI img + hybrid ISO"
	@echo "  kernel     build kernel ELF + flat binary"
	@echo "  bios       assemble MBR + stage2, build BIOS disk image"
	@echo "  uefi       build EFI app + FAT UEFI image"
	@echo "  iso        build hybrid El Torito ISO (BIOS+UEFI bootable)"
	@echo "  vmdk       convert the raw images to VMDKs (needs qemu-img)"
	@echo "  check      run scripts/check.sh (L0-L3 self-verification)"
	@echo "  run-bios   boot BIOS image in QEMU (with display)"
	@echo "  run-uefi   boot UEFI image in QEMU (with display)"
	@echo "  fmt        cargo fmt"
	@echo "  clippy     cargo clippy"
	@echo "  clean      remove build/"

# ---------------------------------------------------------------------------
#  Kernel
# ---------------------------------------------------------------------------
kernel: $(KERN_ELF) $(KERN_BIN)

$(KERN_ELF): $(shell find $(KERN_DIR)/src -name '*.rs') $(KERN_DIR)/Cargo.toml $(KERN_DIR)/linker.ld $(KERN_DIR)/.cargo/config.toml $(BOOTIMG_RS) $(TRUSTSTORE_RS)
	@mkdir -p $(BUILD)
	# Do NOT set RUSTFLAGS here.  The environment variable *overrides*
	# `[target.x86_64-unknown-none] rustflags` in kernel/.cargo/config.toml,
	# which silently dropped `-C relocation-model=static` and produced a
	# PIC kernel: a .got full of zeroes plus 92 unapplied .rela.dyn entries.
	# Neither bootloader applies relocations, so every GOT slot stayed 0 and
	# `zero_bss()` became a no-op (it read start = end = 0 off the GOT).
	# The kernel then ran with whatever .bss the previous boot had left in
	# RAM -- invisible on a cold QEMU boot, fatal on a VMware reset.
	# The linker script is already passed via the config file, so nothing
	# is lost by letting the config flags through.
	cd $(KERN_DIR) && $(CARGO) build $(KERN_PROFILE_FLAG) --target $(TARGET)
	@cp $(KERN_DIR)/target/$(TARGET)/$(PROFILE)/barryos-kernel $(KERN_ELF)

$(KERN_BIN): $(KERN_ELF)
	# Relative paths, and `cd` first, for the same reason the EFI recipe below
	# uses them: a native Windows binutils handed an absolute path under a
	# directory with non-ASCII characters in it fails to open the file, with an
	# error that shows the name mangled past recognition.
	cd $(ROOT) && $(OBJCOPY) -O binary build/kernel.elf build/kernel.bin
	@echo "[kernel] $$(stat -c%s $(KERN_BIN)) bytes uncompressed"

# LZ4 block format.  The encoder decodes its own output and compares before
# writing, so a compressor bug fails the build rather than the boot.
$(KERN_LZ): $(KERN_BIN) $(ROOT)/scripts/compress-kernel.py
	cd $(ROOT) && $(PYTHON) scripts/compress-kernel.py build/kernel.bin build/kernel.lz
	@sz=$$(stat -c%s $(KERN_LZ)); \
	img_max=$$(( ($(BIOS_IMG_SECTORS) - $(KERNEL_IMG_LBA)) * 512 )); \
	if [ $$sz -gt $(KERNEL_MAX_BYTES) ]; then \
	  echo "[kernel] FATAL: the compressed image is $$sz bytes, over the"; \
	  echo "[kernel] $(KERNEL_MAX_BYTES) byte staging window the BIOS loader has."; \
	  echo "[kernel] That window cannot grow: the BIOS writes to 16-bit"; \
	  echo "[kernel] segment:offset addresses and video RAM starts above it."; \
	  exit 1; \
	fi; \
	if [ $$sz -gt $$img_max ]; then \
	  echo "[kernel] FATAL: the compressed image is $$sz bytes, but the BIOS"; \
	  echo "[kernel] image has only $$img_max bytes after LBA $(KERNEL_IMG_LBA)"; \
	  echo "[kernel] raise BIOS_IMG_SECTORS in the Makefile"; \
	  exit 1; \
	fi

# ---------------------------------------------------------------------------
#  BIOS boot chain
# ---------------------------------------------------------------------------
bios: $(BIOS_IMG)

# `cd $(ROOT) &&` in front of every tool invocation, not just the ones with
# several commands: make runs a single command line itself rather than through
# a shell, and the PATH the toolchain lives on is exported to recipe *shells*.
# Without the `cd` the assembler is looked for on the wrong PATH, or on a path
# that does not exist because the tools are named by absolute msys paths.
$(MBR_BIN): $(BOOT_DIR)/bios/mbr.asm
	@mkdir -p $(BUILD)
	cd $(ROOT) && $(NASM) -f bin boot/bios/mbr.asm -o build/mbr.bin

$(STAGE2_BIN): $(BOOT_DIR)/bios/stage2.asm
	@mkdir -p $(BUILD)
	cd $(ROOT) && $(NASM) -f bin boot/bios/stage2.asm -o build/stage2.bin

# Embed the boot chain — and the test program — into a Rust source file the
# kernel includes.  rustc records included files in its dep-info, so cargo
# picks up changes here.
$(TEST_EXE): $(ROOT)/test/hello.c
	@mkdir -p $(BUILD)
	# -nostdlib: no CRT, no startup code, no __main.  The entry point is the
	# function named on the link line, which is exactly what the loader wants
	# to call.  -lkernel32 gives it a real import table to bind.
	cd $(ROOT) && $(MINGW_GCC) -nostdlib -Os -Wl,-e,entry -Wl,--subsystem,console \
	    -o build/hello.exe test/hello.c -lkernel32

$(BOOTIMG_RS): $(MBR_BIN) $(STAGE2_BIN) $(TEST_EXE) $(ROOT)/scripts/gen-bootimg.py
	cd $(ROOT) && $(PYTHON) scripts/gen-bootimg.py \
	    build/mbr.bin build/stage2.bin build/hello.exe

# --- trust store ------------------------------------------------------------
# See the definitions near the top of the file for why the list is pruned.
$(CACERT):
	@mkdir -p $(BUILD)
	curl -sS --proto '=https' --max-time 60 -o $@.tmp https://curl.se/ca/cacert.pem
	mv $@.tmp $@

$(TRUSTSTORE_RS): $(CACERT) $(ROOT)/scripts/gen-truststore.py
	cd $(ROOT) && TRUST_ROOTS="$(TRUST_ROOTS)" "$(TRUST_PYTHON)" -W ignore \
	    scripts/gen-truststore.py build/cacert.pem build/truststore.rs

truststore: $(TRUSTSTORE_RS)

# The loader refuses any image it cannot rebase and any import its table does
# not know.  Both are silent at build time and loud at boot time, and there is
# no hypervisor in CI to boot and find out -- so check the contract directly
# against the real bytes of the real test program.
pe-check: $(TEST_EXE)
	cd $(ROOT) && $(PYTHON) tests/pe-contract.py build/hello.exe kernel/src/compat/win32.rs

$(BIOS_IMG): $(MBR_BIN) $(STAGE2_BIN) $(KERN_LZ) $(KERN_BIN)
	# Tell the loader how big the kernel is.  Written to a *copy*: stage2.bin
	# itself is embedded in the kernel image, so patching it in place would
	# change the embedded bytes every build and rebuild the kernel forever.
	cd $(ROOT) && $(PYTHON) scripts/patch-stage2.py build/stage2.bin build/kernel.lz build/kernel.bin build/stage2.patched.bin
	# create 4 MiB zeroed image
	dd if=/dev/zero of=$(BIOS_IMG) bs=512 count=$(BIOS_IMG_SECTORS) status=none
	# LBA 0: MBR
	dd if=$(MBR_BIN) of=$(BIOS_IMG) conv=notrunc bs=512 count=1 status=none
	# LBA 1..40: stage2 (20 KiB)
	dd if=$(STAGE2_IMG) of=$(BIOS_IMG) conv=notrunc bs=512 seek=1 status=none
	# LBA 41..: the compressed kernel  (must match KERNEL_DISK_LBA in stage2.asm)
	dd if=$(KERN_LZ) of=$(BIOS_IMG) conv=notrunc bs=512 seek=41 status=none
	@echo "[bios] $(BIOS_IMG) ready"

# ---------------------------------------------------------------------------
#  UEFI boot chain
# ---------------------------------------------------------------------------
uefi: $(UEFI_IMG)

$(EFI_APP): $(BOOT_DIR)/uefi/efi_main.c $(BOOT_DIR)/uefi/efi_types.h $(BOOT_DIR)/uefi/efi.ld
	@mkdir -p $(BUILD)
	# clang with the windows target produces MS-ABI code OVMF accepts; GNU ld
	# then links to a PE32+ EFI application.  On msys2, EFI_CC is the mingw-w64
	# gcc, whose native ABI already *is* the MS ABI, so EFI_CFLAGS is empty and
	# the same source compiles unchanged.
	# Relative paths are deliberate: mingw-w64 gcc rewrites absolute POSIX
	# paths to Windows form before handing them to `as`, and that conversion
	# mangles non-ASCII characters.  This project lives under a path that has
	# them, so build from $(ROOT) with bare relative file names instead.
	cd $(ROOT) && $(EFI_CC) $(EFI_CFLAGS) -ffreestanding -fno-stack-protector \
	       -mno-red-zone -fshort-wchar -Wall -Wextra -std=gnu11 \
	       -c boot/uefi/efi_main.c -o build/efi_main.o
	cd $(ROOT) && $(LD) -m i386pep --subsystem 10 -e efi_main --image-base 0x1000000 \
	      --disable-runtime-pseudo-reloc -s \
	      build/efi_main.o -o build/BOOTX64.EFI
	@echo "[uefi] $(EFI_APP) ready"

$(UEFI_IMG): $(EFI_APP) $(KERN_BIN)
	# partitioned image: MBR + ESP (type 0xEF) populated by mkfs.fat + mtools.
	# Relative paths for the same non-ASCII reason as the EFI app above --
	# mtools is a native binary and mishandles the converted path.
	cd $(ROOT) && MKFS_FAT="$(MKFSFAT)" MMD="$(MMD)" MCOPY="$(MCOPY)" \
	    $(PYTHON) scripts/mk-uefi-img.py build/barryOS-uefi.img build/BOOTX64.EFI build/kernel.bin
	@echo "[uefi] $(UEFI_IMG) ready"

# ---------------------------------------------------------------------------
#  Hybrid ISO
# ---------------------------------------------------------------------------
iso: $(ISO)

$(ISO): $(BIOS_IMG) $(UEFI_IMG)
	@mkdir -p $(ISO_ROOT)
	cp $(BIOS_IMG) $(ISO_ROOT)/barryOS-bios.img
	cp $(UEFI_IMG) $(ISO_ROOT)/efiboot.img
	# add some user-visible files for the ISO filesystem
	cp $(ROOT)/README.md $(ISO_ROOT)/ 2>/dev/null || true
	$(XORRISO) -as mkisofs \
	    -R -J -V BARRYOS -joliet-long \
	    -eltorito-boot barryOS-bios.img -no-emul-boot -boot-load-size 4 -boot-info-table \
	    -eltorito-alt-boot -e efiboot.img -no-emul-boot -boot-info-table \
	    -append_partition 2 0xef $(UEFI_IMG) \
	    -o $(ISO) $(ISO_ROOT)/ 2>&1 | grep -vi "^libisofs:" || true
	@echo "[iso] $(ISO) ready"

# ---------------------------------------------------------------------------
#  VMDKs
# ---------------------------------------------------------------------------
# Descriptors pointing at the raw images, so the same file boots under VMware
# as under anything else.  Written by a script rather than converted by
# qemu-img: a flat VMDK is a text file naming the image, and generating it
# costs nothing and removes a tool from the list a host has to have.
#
# Always rebuilt, because the descriptor names a file whose size changes: a
# stale one declares a geometry the image no longer has.
VMDK_BIOS := $(BUILD)/barryOS-bios.vmdk
VMDK_UEFI := $(BUILD)/barryOS-uefi.vmdk

vmdk: $(BIOS_IMG) $(UEFI_IMG)
	cd $(ROOT) && $(PYTHON) scripts/mk-vmdk.py build/barryOS-bios.img build/barryOS-bios.vmdk
	cd $(ROOT) && $(PYTHON) scripts/mk-vmdk.py build/barryOS-uefi.img build/barryOS-uefi.vmdk

.PHONY: vmdk-bios
vmdk-bios: $(BIOS_IMG)
	cd $(ROOT) && $(PYTHON) scripts/mk-vmdk.py build/barryOS-bios.img build/barryOS-bios.vmdk

# ---------------------------------------------------------------------------
#  QEMU runners
# ---------------------------------------------------------------------------
run-bios: $(BIOS_IMG)
	mkdir -p $(LOGS)
	$(QEMU) -L $(QEMU_BIOS_DIR) -drive format=raw,file=$(BIOS_IMG) \
	        -serial stdio -display none -no-reboot -no-shutdown

run-uefi: $(UEFI_IMG)
	mkdir -p $(LOGS)
	cp $(OVMF_VARS) $(BUILD)/OVMF_VARS.copy.fd
	$(QEMU) -L $(QEMU_BIOS_DIR) -drive if=pflash,format=raw,readonly=on,file=$(OVMF_CODE) \
	        -drive if=pflash,format=raw,file=$(BUILD)/OVMF_VARS.copy.fd \
	        -drive format=raw,file=$(UEFI_IMG) \
	        -serial stdio -display none -no-reboot -no-shutdown

# ---------------------------------------------------------------------------
#  Static checks
# ---------------------------------------------------------------------------
fmt:
	cd $(KERN_DIR) && cargo fmt

clippy:
	cd $(KERN_DIR) && cargo clippy --target $(TARGET) -- -D warnings

check:
	bash $(ROOT)/scripts/check.sh

clean:
	rm -rf $(BUILD)
	rm -rf $(KERN_DIR)/target
