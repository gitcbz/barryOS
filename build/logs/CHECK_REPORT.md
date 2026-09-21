
=== barryOS self-check 2026-09-21T12:29:01+00:00 ===

=== L0: make all ===
[PASS] make all (exit 0)

=== L1: artifacts + format checks ===
[PASS] exists: build/kernel.elf
[PASS] exists: build/kernel.bin
[PASS] exists: build/mbr.bin
[PASS] exists: build/stage2.bin
[PASS] exists: build/barryOS-bios.img
[PASS] exists: build/BOOTX64.EFI
[PASS] exists: build/barryOS-uefi.img
[PASS] exists: build/barryOS.iso

=== L1b: MBR magic 0x55AA ===
[PASS] MBR magic = 0x55AA

=== L1c: kernel ELF arch ===
[PASS] kernel ELF machine = x86-64

=== L1d: BOOTX64.EFI format ===
[PASS] EFI format: PE32+ executable for EFI (application), x86-64 (stripped to external PDB), 5 sections

=== L1e: stage2 size = 31 sectors ===
[PASS] stage2 size = 15872 bytes

=== L2: BIOS QEMU boot (serial) ===
[PASS] BIOS: serial contains 'barryOS booted'
[PASS] BIOS: no faults

=== L3: UEFI QEMU boot (serial) ===
[PASS] UEFI: serial contains 'barryOS booted'

=== L4: Stage 2 memory subsystem ===
[PASS] BIOS: Stage 2 memory subsystem online
[PASS] BIOS: CR3 switched (own page tables)
[PASS] BIOS: heap alloc+write+read OK
[PASS] BIOS: Vec::with_capacity works
[PASS] BIOS: Box::new works

=== L5: Stage 3 interrupt subsystem ===
[PASS] BIOS: Stage 3 interrupt subsystem online
[PASS] BIOS: IDT loaded (256 entries)
[PASS] BIOS: PIC remapped (IRQ0..15 → INT 32..47)
[PASS] BIOS: PIT configured (100 Hz)
[PASS] BIOS: timer interrupts fired (ticks=2)

=== L6: Stage 4 process subsystem ===
[PASS] BIOS: Stage 4 process subsystem online
[PASS] BIOS: kernel threads spawned
[PASS] BIOS: scheduler enabled
[PASS] BIOS: scheduler ticks (round-robin PID rotation)
[PASS] BIOS: syscall write() works

=== L7: Stage 5 filesystem subsystem ===
[PASS] BIOS: Stage 5 filesystem subsystem online
[PASS] BIOS: VFS initialized (vnode table)
[PASS] BIOS: RAM filesystem initialized
[PASS] BIOS: root directory listing works
[PASS] BIOS: file read works (open+read+close)

=== L8: Stage 6 device drivers ===
[PASS] BIOS: Stage 6 device drivers online
[PASS] BIOS: framebuffer driver initialized
[PASS] BIOS: PS/2 keyboard driver initialized
[PASS] BIOS: framebuffer test pattern drawn

=== L9: Stage 7 window manager ===
[PASS] BIOS: Stage 7 window manager online
[PASS] BIOS: bitmap font initialized
[PASS] BIOS: windows created
[PASS] BIOS: desktop rendered (background + status + windows + dock)

=== L10: Stage 8 desktop applications ===
[PASS] BIOS: Stage 8 desktop apps online
[PASS] BIOS: terminal app rendered
[PASS] BIOS: file manager app rendered
[PASS] BIOS: system info app rendered
[PASS] BIOS: terminal commands processed (help, ver, ls, mem, ps)

=== L11: Stage 9 compatibility layers ===
[PASS] BIOS: Stage 9 compat layers online
[PASS] BIOS: .deb parser initialized
[PASS] BIOS: .rpm parser initialized
[PASS] BIOS: .AppImage parser initialized
[PASS] BIOS: PE32+ loader initialized
[PASS] BIOS: packages parsed (compat layer)

=== L12: Stage 10 Win32 compatibility layer ===
[PASS] BIOS: Win32 compat layer tests passed
[PASS] BIOS: Win32 compat layer initialized
[PASS] BIOS: WriteFile lookup works
[PASS] BIOS: MessageBoxA lookup works
[PASS] BIOS: Win32 function stubs registered

=== L13: Stage 11 VMware optimization ===
[PASS] BIOS: Stage 11 VMware optimization online
[PASS] BIOS: SVGA-II driver initialized
[PASS] BIOS: VMware backdoor probed
[PASS] BIOS: memory balloon driver initialized

=== SUMMARY ===
PASS=64  FAIL=0  SKIP=0
Full log: build/logs/check-20260921-122901.log
