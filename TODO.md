# barryOS — Project TODO

Ordered by dependency: things near the top unblock things below them.
`[x]` = done and verified, `[ ]` = outstanding, `[~]` = partly done, see the note.

Last reconciled against the code: 2026-09-22.

---

## Recently completed

Kept here so the delta is visible; older rounds are in CHANGELOG.md.

### Boot chain
- [x] **BIOS VBE mode setup** — stage2 never called `int 10h`. It passed `RDI=0` and
  the kernel fell back to a hardcoded `0xE0000000`, which is QEMU's Bochs VBE
  address and means nothing under VMware. stage2 now queries `4F00`/`4F01`/`4F02`
  and hands the kernel a real BootInfo.
- [x] **GOP mode selection** — the UEFI loader used whatever mode firmware picked
  (VMware: 400x300). It now enumerates and selects the largest mode ≤ 1024x768.
- [x] **Streaming kernel loader** — stage2 reads a 32 KiB batch into conventional
  memory, copies it to `0x100000` from protected mode, repeats. Removes the old
  "must fit below 1 MiB" ceiling (was 832 sectors); size now comes from a
  build-time patch, and the ceiling is `STACK_TOP - 0x100000` = 8 MiB.
- [x] **Build-time size guard** — refuses to build a kernel that does not fit, with
  a message saying which constant to raise. It has already caught one overflow
  that would previously have been a silent truncation and an unbootable image.
- [x] **El Torito BIOS entry** — still broken; see "Known broken" below.

### Kernel core fixes
- [x] **`zero_bss` was a no-op** — `Makefile` set `RUSTFLAGS`, which overrides
  `target.*.rustflags` in `.cargo/config.toml`, silently dropping
  `relocation-model=static`. The kernel linked PIC, `.got` held zeroes, and
  `zero_bss()` read start == end == 0 off it. `.bss` was never cleared. Invisible
  on a cold QEMU boot; fatal on a VMware reset.
- [x] **`gdt::init()` was never called** — every IDT gate names selector `0x08`,
  which only means "64-bit kernel code" in the GDT the kernel builds. Under UEFI
  the firmware's GDT was still live, so the first timer IRQ took a #GP → #DF →
  triple fault.
- [x] **`lgdt` did not reload CS** — the running code segment stayed on the
  firmware's selector, outside the new table's limit; the first `iretq` would
  fault restoring it.
- [x] **`GDT`/`TSS` were immutable statics written through `addr_of!() as *mut`** —
  UB, and LLVM deleted all six TSS descriptor stores. `ltr` then loaded a
  non-present descriptor. They are `static mut` now, as are `PML4`/`PDPT0`.
- [x] **TSS/IST1 for #DF is now live** — the `#DF` gate was already `ist=1`, but
  with `gdt::init()` never called the TSS was never loaded, so the IST1 stack
  pointer was meaningless. Fixed as a consequence of the above.
- [x] **BootInfo `memmap_size`** — the UEFI loader stored its *buffer capacity*
  (64 KiB) rather than the used size, so the kernel parsed ~1500 uninitialised
  pool descriptors. Three `AllocatePages` results were also unchecked.

### Desktop
- [x] **PS/2 mouse** — 8042 aux port, IRQ12, 3-byte packets with resync and sign
  extension, cursor with save/restore.
- [x] **Compositor with a real event loop** — the kernel used to render one frame
  and halt. It now idles on `hlt`, repaints on input, and composes into a
  backbuffer that is flipped whole (this is what removed the drag flicker).
- [x] **Window management** — click to focus, drag by the title bar,
  minimise/maximise/close buttons, z-order, taskbar with one button per window,
  and an app launcher.
- [x] **Focus-based input routing** — keystrokes go to whichever window has focus.
- [x] **Font** — was 7 scanlines of a 16-row cell with seven glyphs blank
  entirely; replaced with a complete VGA 8x16 face.
- [x] **Text editor** — open from the file manager or the launcher, insert /
  delete / arrows / Enter, Save and Revert, dirty marker, scrolling.
- [x] **File manager** — real VFS browsing, navigation, selection, and
  create / rename / delete with an inline name prompt.
- [x] **Terminal is a real shell** — see the command list below.
- [x] **Boot splash and login screen**.

### Filesystem and accounts
- [x] **VFS operations** — `mkdir`, `unlink`, `rename`, `read_dir`, path
  resolution, `path_of`.
- [x] **Two real storage bugs** — the first file created was permanently
  unreadable (offset 0 doubled as "no data"); and rewriting a file reused its old
  offset with no capacity check, so growing it ran over the next allocation.
  There is now a per-vnode capacity and a free-list allocator.
- [x] **Permissions** — every vnode has an owning uid/gid and nine mode bits,
  checked the way Unix does it (root bypasses; owner / group / other).
  Enforced in the terminal, file manager and editor.
- [x] **`/etc/passwd` and `/etc/shadow`** — Linux layout, shadow is 0600 and
  root-owned. Hashes are salted SHA-256, stretched 1000 times, compared in
  constant time. SHA-256 is verified against the FIPS vectors by
  `tests/sha256-host.rs`.
- [x] **Executable files** — a file with the execute bit can be `run`; the shell
  interprets its lines (there is no user mode yet, so programs are scripts).

### Network
- [x] **PCI bus enumeration** — ports 0xCF8/0xCFC, BAR decoding and size probing,
  lookup by vendor/device or by class. This was the prerequisite for any NIC.
- [x] **e1000 driver, frame level** — find the adapter, reset, RX/TX descriptor
  rings, link up, `send_frame` / `recv_frame`.
- [x] **ARP** — request, reply, and a small mapping cache.

### Tooling
- [x] **Windows/msys2 build** — `scripts/env.msys.mk`, mingw-w64 gcc for the EFI
  app (its native ABI already is the MS ABI, so no clang needed), plus TMP and
  rustup-home handling that msys does not inherit.
- [x] **`tests/sha256-host.rs`** — the first test in this project that checks
  behaviour rather than grepping a log line.

---

## Outstanding

### P0 — unlocks everything else

- [ ] **Preemptive context switching** (`proc/scheduler.rs`, `proc/context.rs`,
  DECISIONS D27). `tick()` only rotates the current-PID pointer and updates
  counters; it never swaps stacks. `context_switch` is written but was disabled
  because the initial frame / trampoline return address were wrong. Until this
  works there are no real processes, no preemption, and Ring 3 has nothing to
  switch to. **This is the single highest-value item.**
- [ ] **Ring 0 → Ring 3** — GDT user code/data segments, per-process page tables,
  a stack switch on entry, and `int 0x80` (or `syscall`/`sysret`) actually
  reached from CPL 3. Vector 0x80 is registered but has never been invoked from
  a lower privilege level.
- [ ] **`exit()` does not reclaim the PCB** — the process table is never cleaned
  up, so a long-running session leaks slots.

### P1 — usability

- [ ] **Free-list heap allocator** (`mem/heap.rs`). `dealloc` is a no-op, so
  `Vec` growth fails; heap test 3 is skipped. The RAM filesystem got a free list
  for its data pool; the kernel heap has not.
- [ ] **Block device + a disk filesystem** — ATA-PIO or AHCI, then FAT32 or
  something of our own. Everything currently lives in RAM and vanishes on reset.
- [ ] **`check.sh` is not a test suite.** ~50 of its ~64 checks grep the serial
  log for strings the kernel itself printed. That is how eleven rounds of
  "64/64 PASS" coexisted with a kernel that could not boot under UEFI at all.
  Needs rewriting so each check observes an independent result.
- [ ] **UEFI path PIT** (DECISIONS D30) — ticks were 0 under OVMF. The GDT fix
  may have changed this; re-verify before assuming it is still broken.
- [ ] **Higher-half kernel remap** — `PML4[511]` is reserved and unused; the
  kernel still runs at physical `0x100000`.
- [ ] **Serial input / IRQ4** — serial is output-only.

### P2 — completeness

- [ ] **APIC/LAPIC + IOAPIC** — still on the 8259 PIC.
- [ ] **SMP** — no IPIs, no application-processor startup. Needs the above first.
- [ ] **Window resize** — maximise/restore works; dragging an edge does not.
- [ ] **Widget toolkit** — buttons and a dock exist as ad-hoc drawing, not as a
  reusable control library. No menus, scrollbars, list views or text fields.
- [ ] **Screenshot tool and image viewer** (planned in ARCHITECTURE.md).
- [ ] **Script arguments** — `run script a b` does not pass `$1`/`$2` through.
- [ ] **`su` has no logout / no session lock**, and there is no way to change a
  password.

### P3 — compatibility layers (currently header parsers only)

- [ ] **`.deb` / `.rpm` installer** — headers are parsed; `control.tar.gz` /
  `data.tar.gz` are never decompressed and nothing is installed.
- [ ] **AppImage** — detected, never mounted.
- [ ] **PE loading and execution** — sections, imports and relocations are
  parsed; there is no image mapping, no address space and no entry jump
  (deferred to "Stage 10b"). Needs virtual memory per process first.
- [ ] **Win32 emulation** — ten function *names* in a table with no call
  dispatch, no import binding and no execution path.

### P4 — VMware

- [x] **Backdoor register protocol** — the register shuttling lives in
  `global_asm!`, because Rust forbids `rbx` as an inline-asm operand (LLVM
  reserves it) and the protocol needs EBX, EBP and EDI.  Cross-checked against
  open-vm-tools and OpenBSD's `vmt.c` rather than reconstructed from memory.
- [x] **Direct commands** — GETVERSION, GETHWVERSION, GETUUID, GETMEMSIZE,
  clipboard length, and GETTIMEFULL with a fallback to the deprecated GETTIME.
  Unsupported commands are told apart by EAX coming back as `0xFFFFFFFF`.
- [x] **RPCI on the TCLO channel** — open (with the cookie flag), set length,
  bulk data out over port 0x5659, get length, bulk data in, acknowledge, close.
  Exposed as `vmx rpci <command>`.
- [x] **Host clock** — `vmx time` prints the host's wall clock as a readable UTC
  date.  This is the one tools feature with no substitute: a VM restored from a
  saved state resumes with whatever clock it was saved with.
- [x] **SVGA-II register pair corrected** to 0x1CE/0x1CF.
- [ ] **SVGA-II programming** — the register pair is correct, but nothing
  programs the device.  Detection is honest about this.
- [ ] **HGFS shared folders** — needs the whole HGFS protocol over RPCI, which
  is a filesystem in its own right rather than a command.  This would give the
  guest a file input path without installing anything.
- [ ] **Clipboard sync** — the length command is wired up; nothing reads or
  displays the contents.
- [ ] **Memory balloon** — stub reporting zero pages.
- [ ] **Paravirtual drivers** — vmxnet3, PVSCSI.
- [ ] **Stage 12 release documentation.**

### P5 — install to disk

- [x] **ATA-PIO block driver** — probes all four IDE positions, `IDENTIFY`,
  LBA28 read/write, timeouts on every wait loop.
- [x] **barryFS** — superblock, fixed inode table, data bitmap, 4 KiB blocks,
  one contiguous run per file.
- [x] **Installer** — writes MBR + stage2 + kernel + filesystem to a chosen
  disk.  The boot chain is embedded in the kernel so installing works from a
  CD, which has no ATAPI driver.  The MBR goes last, so an interrupted install
  leaves a disk that still will not boot rather than a half-bootable one.
- [x] **Boot menu** — `[1] Install` / `[2] Start`, with a 5-second timeout so
  an unattended boot does not sit at a prompt.
- [x] **Mount from disk at boot** — an installed system wins over the built-in
  tree; `sync` writes it back.
- [ ] **UEFI install** — the installer writes a BIOS boot chain only.  Booting
  the installed disk under UEFI firmware would need an ESP written too.
- [ ] **Partitioning** — the installer treats the whole disk as ours and
  overwrites from LBA 0.  No partition table, no choice of layout.
- [ ] **A real block-level filesystem** — barryFS keeps everything in RAM and
  rewrites the image on `sync`.  Fine for a few dozen small files, wrong in
  general.

---

## Known broken / known fake

Things that exist, look like they work, and do not.

- [ ] **`check.sh`** — see P1. The single largest source of false confidence in
  this project.
- [ ] **The ISO's BIOS boot entry.** `-boot-info-table` overwrites the MBR at
  offsets 8..61 (the write happens to land on the code), and under El Torito
  no-emulation a CD presents 2048-byte sectors while the MBR computes LBA on
  512-byte ones. The UEFI entry is fine. BIOS boot works today via the raw
  `barryOS-bios.img` as a hard disk, which is what the `.vmdk` descriptor does.
- [ ] **Scheduler** — accounting only (P0).
- [ ] **`proc::process::count()`** always reports the same four slot entries.
- [ ] **`[compat] total packages parsed`** prints a plausible-looking but wrong
  value; the counter's storage has not been tracked down.

---

## Out of scope until asked

- Real hardware: USB boot, GPU passthrough, printers.
- Secure Boot signing (the EFI app is unsigned, so Secure Boot must stay off).
