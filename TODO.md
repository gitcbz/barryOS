# barryOS — Project TODO

Autonomous task list, ordered by priority. Items marked `[PENDING]` are not started.
Checked items `[x]` are complete. See STATUS.md for current round.

## Stage 0 — Planning & Scaffolding
- [x] Inspect environment, document tool availability
- [x] Install toolchain (Rust nightly, NASM, QEMU, xorriso, mtools, OVMF) without sudo
- [x] Create project directory tree
- [x] Write state files (STATUS/TODO/CHANGELOG/DECISIONS/ASSUMPTION/BOOTSTRAP/BLOCKERS)
- [x] Write docs/ARCHITECTURE.md (dual-boot chain, kernel, userland, desktop, compat layers)

## Stage 1 — Dual-Boot MVP ✅
- [x] all items complete (Round 1)
- [x] BIOS MBR bootloader (512 B, 0x55AA, loads stage2)
- [x] BIOS stage2 bootloader (real→protected→long mode, loads kernel to 0x100000)
- [x] UEFI EFI application (GOP framebuffer, memmap, ExitBootServices, jump to kernel)
- [x] Unified kernel entry `_start` + `kernel_main` printing "barryOS booted"
- [x] kernel VGA text-mode + serial (COM1) output drivers
- [x] panic handler + linker script (kernel at 0x100000)
- [x] Makefile producing BIOS img, UEFI img, hybrid ISO
- [x] scripts/check.sh self-verification harness
- [x] GitHub Actions CI workflow + Dockerfile
- [x] QEMU BIOS boot test → assert "barryOS booted"
- [x] QEMU UEFI boot test → assert "barryOS booted"
- [x] CHECK_REPORT.md generated with real outputs
- [x] Rootless toolchain (Rust nightly, NASM, QEMU, xorriso, mtools, OVMF, clang)
- [x] Fedora OVMF (has FatDxe) extracted from RPM and installed

## Stage 2 — Memory Management ✅
- [x] BootInfo consumption (UEFI memmap + BIOS fallback)
- [x] Physical page frame allocator (bitmap, 8 KiB / 256 MiB)
- [x] x86_64 4-level page tables + CR3 switch (1 GiB identity map)
- [x] Heap allocator (bump, #[global_allocator], Box/Vec work)
- [ ] Vec reallocation (deferred to Stage 2b)
- [ ] Higher-half kernel remap (deferred to Stage 2b)


## Stage 3 — Interrupts & Exceptions ✅
- [x] IDT setup (256 entries, interrupt gates)
- [x] CPU exception handlers (#DE..#VE, all 21 vectors)
- [x] PIC 8259 remap (IRQ 0-15 → INT 32-47)
- [x] PIT 8253 at 100 Hz (timer IRQ0)
- [x] Keyboard IRQ1 (scancode→ASCII)
- [ ] TSS/IST1 for #DF (deferred — Stage 3b)
- [ ] APIC/LAPIC (deferred — Stage 4b)

## Stage 4 — Processes & Syscalls ✅
- [x] PCB (Process Control Block) + 16-slot process table
- [x] Kernel threads (spawn, trampoline, exit)
- [x] Round-robin scheduler (tick, yield, rotation verified)
- [x] Syscall interface (int 0x80, 5 calls: write/exit/getpid/yield/getticks)
- [x] Context switch infrastructure (context_switch inline asm)
- [ ] Actual preemptive context switch (deferred — Stage 4b)
- [ ] Ring 0 → Ring 3 user-mode (deferred — Stage 4b)

## Stage 5 — VFS + Filesystem ✅
- [ ] VFS (inode/dentry/file/superblock)
- [ ] Simple FS (FAT32 or self-made)
- [ ] ATA-PIO or AHCI block driver

## Stage 6 — Device Drivers
- [ ] PS/2 keyboard + mouse
- [ ] Framebuffer console / graphics
- [ ] Serial debug

## Stage 7-8 — Desktop Environment
- [ ] Compositor, window manager, base widgets, theme engine
- [ ] File manager, terminal, editor, screenshot, image viewer

## Stage 9-10 — Compatibility Layers
- [ ] .deb / .rpm parsers + installer
- [ ] AppImage mount
- [ ] PE loader + Win32 compat (NTDLL/KERNEL32 MVP)

## Stage 11-12 — VMware + Release
- [ ] VMware VBE auto-resolution / paravirt drivers
- [ ] Shared folders, clipboard, time sync
- [ ] Full docs, ISO release, VMware BIOS/UEFI acceptance test

## Cross-cutting
- [ ] Expand CHECK_REPORT.md each round with real command outputs
- [ ] Keep CHANGELOG.md / DECISIONS.md / STATUS.md in sync every round
