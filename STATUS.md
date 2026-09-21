# barryOS — STATUS

**Current round:** Round 5 COMPLETE — Stage 5 (VFS + Filesystem) ✅
**Last updated:** 2026-09-21 09:50 (Asia/Shanghai)
**Mode:** AUTONOMOUS

## Verification result
**PASS=36  FAIL=0  SKIP=0** — see CHECK_REPORT.md

| Gate | Result |
|------|--------|
| L0  make all                            | ✅ PASS |
| L1  8/8 artifacts + format              | ✅ PASS |
| L2  BIOS QEMU → "barryOS booted"        | ✅ PASS |
| L3  UEFI QEMU → "barryOS booted"        | ✅ PASS |
| L4  Stage 2 memory (5 checks)           | ✅ PASS |
| L5  Stage 3 interrupts (5 checks)       | ✅ PASS |
| L6  Stage 4 processes (5 checks)        | ✅ PASS |
| L6  syscall write() works               | ✅ PASS |
| L7  Stage 5 filesystem online           | ✅ PASS |
| L7  VFS initialized (vnode table)        | ✅ PASS |
| L7  RAM filesystem initialized           | ✅ PASS |
| L7  root directory listing works         | ✅ PASS |
| L7  file read works (open+read+close)   | ✅ PASS |

## Stage 5 deliverables
1. **VFS** (`kernel/src/fs/vfs.rs`): Vnode abstraction with 64-slot
   vnode table. Each vnode has id, type (File/Dir/Symlink), name,
   size, parent_id, child_ids, data_offset. Raw pointer access
   throughout (avoids Rust 2024 `static mut` reference UB).
   `create_file`, `create_dir`, `lookup` (byte-by-byte name comparison).
2. **RAM filesystem** (`kernel/src/fs/ramfs.rs`): 8 KiB static data
   pool for file content. `alloc_data`, `write_file`, `read_file`.
   No block device needed — proves VFS layer works. 4 test files
   created at boot (motd, hello, version, hostname).
3. **File operations** (`kernel/src/fs/file.rs`): `open(path)` → fd,
   `read(fd, buf)` → bytes, `close(fd)`. 16-slot file handle table.
   Smoke test: open("/hello") → fd=0, read 45 bytes. open("/nonexistent")
   → -1.
4. **Stage 2 loader fix** (`boot/bios/stage2.asm`): Increased kernel
   load from 128 sectors (64 KiB) to 256 sectors (128 KiB) using
   4-chunk int 13h reads (64 sectors each). The kernel.bin grew to
   ~84 KiB with the fs module, exceeding the old 64 KiB limit.

## Key fixes this round
- **kernel.bin exceeded 64 KiB stage2 limit** → 4-chunk BIOS reads (256 sectors).
- **`&mut` reference UB on static VNODES** → raw pointer access throughout.
- **`core::slice::from_raw_parts` + `==` comparison caused #UD** → byte-by-byte
  volatile comparison in `lookup()`.
- **Vnode ID collision (root + first file both id=1)** → NEXT_ID starts at 2.
- **`#[derive(Clone, Copy)]` needed for array init** → added to Vnode.
- **`implicit autoref creates reference to dereference of raw pointer`** →
  replaced `&(*child).name[..len]` with `from_raw_parts` + volatile reads.

## Known limitations (Stage 5b)
- "motd" file has size=0 (alloc_data returns offset 0, treated as "unset").
- No block device driver (ATA-PIO/AHCI) — in-memory only.
- No FAT32 or ext2 — RAMfs only.
- No directory hierarchy (all files in root).

## Next round (Stage 6 — Device Drivers)
- [ ] PS/2 keyboard driver (scancode → ASCII, already in IRQ1 handler)
- [ ] Framebuffer graphics driver (GOP linear framebuffer, pixel plotting)
- [ ] Serial debug (already working — COM1)

## Gates status
- L0-L7: ✅ PASS
- L8 compat layer: deferred (Stage 9-10)
- L9 VMware: PENDING (Stage 7+)
