# barryOS — CHECK_REPORT

**Generated:** 2026-09-21 07:55 (Asia/Shanghai)
**Round:** 3 — Stage 3 (Interrupts & Exceptions)
**Mode:** Autonomous, rootless sandbox

## Verification gates

| Gate | Description                          | Result |
|------|--------------------------------------|--------|
| L0   | `make all` exit 0                    | **PASS** |
| L1   | 8 artifacts exist + format-correct   | **PASS** (8/8) |
| L1b  | MBR magic 0x55AA                     | **PASS** |
| L1c  | kernel ELF machine = x86-64          | **PASS** |
| L1d  | BOOTX64.EFI = PE32+ EFI app          | **PASS** |
| L1e  | stage2 = 15872 bytes                 | **PASS** |
| L2   | BIOS QEMU → "barryOS booted"        | **PASS** |
| L3   | UEFI QEMU → "barryOS booted"        | **PASS** |
| L4   | Stage 2 memory subsystem online      | **PASS** |
| L4   | CR3 switched (own page tables)       | **PASS** |
| L4   | heap alloc+write+read OK              | **PASS** |
| L4   | Vec::with_capacity works             | **PASS** |
| L4   | Box::new works                        | **PASS** |
| L5   | Stage 3 interrupt subsystem online    | **PASS** |
| L5   | IDT loaded (256 entries)              | **PASS** |
| L5   | PIC remapped (IRQ0..15 → INT 32..47)  | **PASS** |
| L5   | PIT configured (100 Hz)               | **PASS** |
| L5   | timer interrupts fired (ticks=2)      | **PASS** |

**Summary: PASS=26  FAIL=0  SKIP=0**

## Stage 3 serial output (BIOS)

```
[stage3] initializing interrupt subsystem...
[irq] PIT configured: 100 Hz (divisor 2E9B)
[irq] step 1: build IDT (256 entries)
[irq] IDT[32]: off=0x1001D8 sel=0x8 ist=0 attr=0x8E
[irq] IDT loaded (256 entries @ 0x10F030)
[irq] step 2: remap PIC 8259
[irq] PIC masks after init: m1=0x0 m2=0x0
[irq] PIC remapped: IRQ0..15 → INT 32..47
[irq] step 3: enable interrupts (sti)
[irq] interrupt subsystem online
[irq] interrupts enabled; waiting for timer...
[irq] timer ticks: 2 (decimal, expect >0)
[irq] keyboard IRQs: 0
[stage3] interrupt subsystem online.
[ok] Stage 3 complete; halting.
```

## QEMU -d int confirmation
```
Servicing hardware INT=0x08   (1× — pre-remap, BIOS/SeaBIOS)
Servicing hardware INT=0x20  (12× — IRQ0 timer after remap)
```

This proves the PIC remap works: IRQ0 → INT 0x20 (32), and the timer
handler (irq0_timer) runs 12 times during the boot, incrementing
TIMER_TICKS each time.

## Build artifacts
| File                       | Size      | Format                     |
|----------------------------|-----------|----------------------------|
| build/kernel.elf           | ~50 KB    | ELF64 x86-64, entry 0x100000 |
| build/kernel.bin           | ~40 KB    | flat binary @ 0x100000     |
| build/mbr.bin              | 512 B     | MBR, magic 0x55AA          |
| build/stage2.bin           | 15,872 B  | 31 sectors, real→long mode  |
| build/barryOS-bios.img     | 4 MiB     | BIOS boot disk             |
| build/BOOTX64.EFI          | 6,144 B   | PE32+ EFI application      |
| build/barryOS-uefi.img     | 16 MiB    | MBR + FAT16 ESP            |
| build/barryOS.iso          | 38 MiB    | hybrid El Torito (BIOS+UEFI)|

## Issues encountered & resolved
1. **`naked_fn as u64 → 0`** → `lea [rip + sym]` macro (`fn_addr!`).
2. **`#[used]` + `#[unsafe(naked)]` incompatible** → `KEEP_HANDLERS` static.
3. **`test 0, 0` invalid** → separate macros for err/no-err exceptions.
4. **Binary asm labels `1:`** → used `2:`/`2b`.
5. **PIC remap not taking** → `io_wait()` + pre-mask all IRQs.
6. **`options(nomem)` on port I/O** → removed `nomem`.
7. **TSS `ltr` causes #GP** → deferred (Stage 3b).

## Next actions (Stage 4 — Processes & Syscalls)
- PCB, kernel threads, Ring 0 → Ring 3 transition.
- Round-robin scheduler.
- syscall/sysret ABI, basic calls (write/exit/fork/exec/wait).

## VMware acceptance
VMware-PENDING (Stage 7+ requires desktop). QEMU BIOS+UEFI is the proxy.
