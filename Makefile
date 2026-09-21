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

CARGO       := cargo
NASM        := nasm
GCC         := gcc
LD          := ld
OBJCOPY     := objcopy
XORRISO     := xorriso
MFORMAT     := mformat
MCOPY       := mcopy
MMD         := mmd
FILE        := file
READELF     := readelf

TARGET      := x86_64-unknown-none
PROFILE     := release
KERN_PROFILE_FLAG := $(if $(filter release,$(PROFILE)),--release,)

# outputs
KERN_ELF    := $(BUILD)/kernel.elf
KERN_BIN    := $(BUILD)/kernel.bin
MBR_BIN     := $(BUILD)/mbr.bin
STAGE2_BIN  := $(BUILD)/stage2.bin
BIOS_IMG    := $(BUILD)/barryOS-bios.img
EFI_APP     := $(BUILD)/BOOTX64.EFI
UEFI_IMG    := $(BUILD)/barryOS-uefi.img
ISO         := $(BUILD)/barryOS.iso

# sizes
BIOS_IMG_SECTORS := 8192       # 4 MiB BIOS image
KERNEL_MAX_SECTORS := 128      # stage2 reads this many (see stage2.asm)

.PHONY: all kernel bios uefi iso clean check run-bios run-uefi fmt clippy help

all: kernel bios uefi iso

help:
	@echo "barryOS build targets:"
	@echo "  all        build kernel + BIOS img + UEFI img + hybrid ISO"
	@echo "  kernel     build kernel ELF + flat binary"
	@echo "  bios       assemble MBR + stage2, build BIOS disk image"
	@echo "  uefi       build EFI app + FAT UEFI image"
	@echo "  iso        build hybrid El Torito ISO (BIOS+UEFI bootable)"
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

$(KERN_ELF): $(wildcard $(KERN_DIR)/src/*.rs) $(KERN_DIR)/Cargo.toml $(KERN_DIR)/linker.ld $(KERN_DIR)/.cargo/config.toml
	@mkdir -p $(BUILD)
	cd $(KERN_DIR) && RUSTFLAGS="-C link-arg=-T$(KERN_DIR)/linker.ld" $(CARGO) build $(KERN_PROFILE_FLAG) --target $(TARGET)
	@cp $(KERN_DIR)/target/$(TARGET)/$(PROFILE)/barryos-kernel $(KERN_ELF)

$(KERN_BIN): $(KERN_ELF)
	$(OBJCOPY) -O binary $(KERN_ELF) $(KERN_BIN)

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

$(BIOS_IMG): $(MBR_BIN) $(STAGE2_BIN) $(KERN_BIN)
	# create 4 MiB zeroed image
	dd if=/dev/zero of=$(BIOS_IMG) bs=512 count=$(BIOS_IMG_SECTORS) status=none
	# LBA 0: MBR
	dd if=$(MBR_BIN) of=$(BIOS_IMG) conv=notrunc bs=512 count=1 status=none
	# LBA 1..31: stage2 (16 KiB)
	dd if=$(STAGE2_BIN) of=$(BIOS_IMG) conv=notrunc bs=512 seek=1 status=none
	# LBA 32..: kernel
	dd if=$(KERN_BIN) of=$(BIOS_IMG) conv=notrunc bs=512 seek=32 status=none
	@echo "[bios] $(BIOS_IMG) ready"

# ---------------------------------------------------------------------------
#  UEFI boot chain
# ---------------------------------------------------------------------------
uefi: $(UEFI_IMG)

$(EFI_APP): $(BOOT_DIR)/uefi/efi_main.c $(BOOT_DIR)/uefi/efi_types.h $(BOOT_DIR)/uefi/efi.ld
	@mkdir -p $(BUILD)
	# clang with the windows target produces MS-ABI code OVMF accepts;
	# GNU ld then links to a PE32+ EFI application.
	clang-19 -target x86_64-unknown-windows -ffreestanding -fno-stack-protector \
	       -mno-red-zone -fshort-wchar -Wall -Wextra -std=gnu11 \
	       -c $(BOOT_DIR)/uefi/efi_main.c -o $(BUILD)/efi_main.o
	$(LD) -m i386pep --subsystem 10 -e efi_main --image-base 0x1000000 \
	      --disable-runtime-pseudo-reloc -s \
	      $(BUILD)/efi_main.o -o $(EFI_APP)
	@echo "[uefi] $(EFI_APP) ready"

$(UEFI_IMG): $(EFI_APP) $(KERN_BIN)
	# partitioned image: MBR + ESP (type 0xEF) populated by mtools
	python3 $(ROOT)/scripts/mk-uefi-img.py $(UEFI_IMG) $(EFI_APP) $(KERN_BIN)
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
