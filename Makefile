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

# sizes
BIOS_IMG_SECTORS := 8192       # 4 MiB BIOS image
# How much room the loader has for the kernel image.  stage2 streams it to
# 0x100000 in batches, and the stack grows down from STACK_TOP, so the ceiling
# is STACK_TOP - 0x100000.  The guard below checks this *and* that the image is
# big enough to hold the kernel.
KERNEL_MAX_BYTES := 8388608    # 8 MiB (16384 sectors)
KERNEL_IMG_LBA   := 41         # must match KERNEL_DISK_LBA in stage2.asm

.PHONY: all kernel bios uefi iso vmdk clean check run-bios run-uefi fmt clippy help

all: kernel bios uefi iso

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

$(KERN_ELF): $(shell find $(KERN_DIR)/src -name '*.rs') $(KERN_DIR)/Cargo.toml $(KERN_DIR)/linker.ld $(KERN_DIR)/.cargo/config.toml $(BOOTIMG_RS)
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
	$(OBJCOPY) -O binary $(KERN_ELF) $(KERN_BIN)
	@sz=$$(stat -c%s $(KERN_BIN)); \
	img_max=$$(( ($(BIOS_IMG_SECTORS) - $(KERNEL_IMG_LBA)) * 512 )); \
	if [ $$sz -gt $(KERNEL_MAX_BYTES) ]; then \
	  echo "[kernel] FATAL: kernel.bin is $$sz bytes, over the $(KERNEL_MAX_BYTES) byte ceiling"; \
	  echo "[kernel] the loader streams it to 0x100000; the stack starts at STACK_TOP"; \
	  echo "[kernel] raise STACK_TOP in boot/bios/stage2.asm and kernel/src/main.rs"; \
	  exit 1; \
	fi; \
	if [ $$sz -gt $$img_max ]; then \
	  echo "[kernel] FATAL: kernel.bin is $$sz bytes, but the BIOS image only"; \
	  echo "[kernel] has $$img_max bytes after LBA $(KERNEL_IMG_LBA)"; \
	  echo "[kernel] raise BIOS_IMG_SECTORS in the Makefile"; \
	  exit 1; \
	fi; \
	echo "[kernel] $$sz bytes = $$(( ($$sz + 511) / 512 )) sectors"

# ---------------------------------------------------------------------------
#  BIOS boot chain
# ---------------------------------------------------------------------------
bios: $(BIOS_IMG)

$(MBR_BIN): $(BOOT_DIR)/bios/mbr.asm
	@mkdir -p $(BUILD)
	$(NASM) -f bin $(BOOT_DIR)/bios/mbr.asm -o $(MBR_BIN)

$(STAGE2_BIN): $(BOOT_DIR)/bios/stage2.asm
	@mkdir -p $(BUILD)
	$(NASM) -f bin $(BOOT_DIR)/bios/stage2.asm -o $(STAGE2_BIN)

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

$(BIOS_IMG): $(MBR_BIN) $(STAGE2_BIN) $(KERN_BIN)
	# Tell the loader how big the kernel is.  Written to a *copy*: stage2.bin
	# itself is embedded in the kernel image, so patching it in place would
	# change the embedded bytes every build and rebuild the kernel forever.
	cd $(ROOT) && $(PYTHON) scripts/patch-stage2.py build/stage2.bin build/kernel.bin build/stage2.patched.bin
	# create 4 MiB zeroed image
	dd if=/dev/zero of=$(BIOS_IMG) bs=512 count=$(BIOS_IMG_SECTORS) status=none
	# LBA 0: MBR
	dd if=$(MBR_BIN) of=$(BIOS_IMG) conv=notrunc bs=512 count=1 status=none
	# LBA 1..40: stage2 (20 KiB)
	dd if=$(STAGE2_IMG) of=$(BIOS_IMG) conv=notrunc bs=512 seek=1 status=none
	# LBA 41..: kernel  (must match KERNEL_DISK_LBA in stage2.asm)
	dd if=$(KERN_BIN) of=$(BIOS_IMG) conv=notrunc bs=512 seek=41 status=none
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
# Self-contained virtual disks (monolithicSparse), for attaching to a VM
# directly.  Kept out of `all` because it needs qemu-img, which is not part of
# the toolchain anywhere else -- a Windows host without qemu-utils still builds
# everything else.
VMDK_BIOS := $(BUILD)/barryOS-bios.vmdk
VMDK_UEFI := $(BUILD)/barryOS-uefi.vmdk

vmdk: $(BIOS_IMG) $(UEFI_IMG)
	$(QEMU_IMG) convert -f raw -O vmdk $(BIOS_IMG) $(VMDK_BIOS)
	$(QEMU_IMG) convert -f raw -O vmdk $(UEFI_IMG) $(VMDK_UEFI)
	@ls -la $(VMDK_BIOS) $(VMDK_UEFI)

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
