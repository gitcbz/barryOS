# barryOS — ASSUMPTIONS

Assumptions made autonomously (no human available to confirm).
Each is tagged so it can be revisited if it proves wrong.

- [A01] Target machine is x86_64 with UEFI (CSM optional) or legacy BIOS.
        Both VMware Workstation 17 Pro BIOS mode and UEFI mode are in scope.
- [A02] Minimum RAM for boot = 64 MiB (Stage 1 needs ~1 MiB; later stages more).
- [A03] QEMU (SeaBIOS for BIOS, OVMF for UEFI) is an acceptable proxy for
        VMware acceptance until the final manual VMware run.
- [A04] Default serial port COM1 @ 0x3F8, IRQ4, 115200 8N1 — matches QEMU
        `-serial stdio` default and VMware default.
- [A05] VGA text mode (80x25, 0xB8000) is available on BIOS (SeaBIOS) and
        emulated by OVMF (which sets a GOP linear framebuffer; we use VGA
        text only as a fallback in Stage 1, GOP-backed drawing comes in Stage 6).
- [A06] Kernel flat binary is < 16 MiB so it fits in the region we load into
        (0x100000 .. 0x1100000) without colliding with stage2 at 0x7E00.
- [A07] Stage2 binary fits in 16 KiB (LBA 1..31).
- [A08] gcc + ld with `--subsystem 10` produces a valid PE32+ EFI application
        that OVMF will load. If not, fallback is the Rust `uefi` crate (D04).
- [A09] OVMF identity-maps all RAM and leaves paging intact after
        ExitBootServices; the kernel at 0x100000 will execute correctly
        without us rebuilding CR3 in the EFI app. (Will be replaced by our
        own paging in Stage 2.)
- [A10] For Stage 1, BIOS E820 / UEFI memory map is not consumed by the
        kernel (no allocator yet). Memory map plumbing arrives in Stage 2.
