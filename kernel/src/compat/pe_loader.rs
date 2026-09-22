//! PE loader — parse, map, relocate, bind imports, and run.
//!
//! The previous version parsed headers and stopped there, which made "PE
//! support" a claim rather than a feature.  This one actually loads an image
//! and transfers control to it.
//!
//! It runs the image at an address it picked rather than the one the linker
//! wanted, because ImageBase for a 64-bit binary is 0x140000000 — well above
//! the 4 GiB the kernel identity-maps.  So the base-relocation table is not
//! optional and has to be applied properly.
//!
//! ## What runs and what does not
//!
//! PE32+ (x86-64) executes directly: the kernel is already in long mode and
//! the MS ABI is just a calling convention.  PE32 (i386) is parsed and mapped
//! but not executed — that needs a 32-bit compatibility-mode segment and a set
//! of 32-bit thunks, which is a separate piece of work.  A PE32 file gets a
//! clear message rather than a mysterious fault.

use crate::compat::win32;
use crate::mem;
use crate::serial;

// --- little-endian field readers, all bounds-checked -----------------------

fn u16_at(d: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(o..o + 2)?.try_into().ok()?))
}
fn u32_at(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(o..o + 4)?.try_into().ok()?))
}
fn u64_at(d: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_le_bytes(d.get(o..o + 8)?.try_into().ok()?))
}

const DOS_MAGIC: u16 = 0x5A4D;          // 'MZ'
const PE_SIGNATURE: u32 = 0x0000_4550;  // 'PE\0\0'
const OPT_MAGIC_PE32: u16 = 0x010B;
const OPT_MAGIC_PE32P: u16 = 0x020B;

const MACHINE_I386: u16 = 0x014C;
const MACHINE_AMD64: u16 = 0x8664;

// Section characteristics worth knowing about.
const SCN_MEM_EXECUTE: u32 = 0x2000_0000;
const SCN_MEM_READ: u32 = 0x4000_0000;
const SCN_MEM_WRITE: u32 = 0x8000_0000;

// Base relocation types.
const REL_ABSOLUTE: u16 = 0;
const REL_HIGHLOW: u16 = 3;   // 32-bit, applied only if we ever run PE32
const REL_DIR64: u16 = 10;    // 64-bit, what x86-64 actually uses

/// Everything the loader needs out of the headers.
#[derive(Clone, Copy)]
pub struct PeInfo {
    pub is_64: bool,
    pub image_base: u64,
    pub entry_rva: u32,
    pub size_of_image: u32,
    pub size_of_headers: u32,
    pub section_count: u16,
    /// File offset of the section table.
    pub sections_at: usize,
    pub import_rva: u32,
    pub import_size: u32,
    pub reloc_rva: u32,
    pub reloc_size: u32,
}

/// A section header, resolved.
#[derive(Clone, Copy)]
struct Section {
    name: [u8; 8],
    virtual_size: u32,
    virtual_address: u32,
    raw_size: u32,
    raw_ptr: u32,
    characteristics: u32,
}

impl Section {
    fn name_str(&self) -> &str {
        let n = self.name.iter().position(|&b| b == 0).unwrap_or(8);
        core::str::from_utf8(&self.name[..n]).unwrap_or("?")
    }
}

fn read_section(d: &[u8], at: usize) -> Option<Section> {
    let mut name = [0u8; 8];
    name.copy_from_slice(d.get(at..at + 8)?);
    Some(Section {
        name,
        virtual_size: u32_at(d, at + 8)?,
        virtual_address: u32_at(d, at + 12)?,
        raw_size: u32_at(d, at + 16)?,
        raw_ptr: u32_at(d, at + 20)?,
        characteristics: u32_at(d, at + 36)?,
    })
}

/// Parse the headers.  Every read is bounds-checked, so a truncated or
/// malformed file is an error rather than a fault.
pub fn parse(d: &[u8]) -> Result<PeInfo, &'static str> {
    if d.len() < 0x40 {
        return Err("too small to be an executable");
    }
    if u16_at(d, 0) != Some(DOS_MAGIC) {
        return Err("not an executable (no MZ)");
    }
    let pe = u32_at(d, 0x3C).ok_or("bad e_lfanew")? as usize;
    if u32_at(d, pe) != Some(PE_SIGNATURE) {
        return Err("no PE signature");
    }

    let machine = u16_at(d, pe + 4).ok_or("truncated COFF header")?;
    let section_count = u16_at(d, pe + 6).ok_or("truncated COFF header")?;
    let opt_size = u16_at(d, pe + 20).ok_or("truncated COFF header")? as usize;
    let opt = pe + 24;

    let magic = u16_at(d, opt).ok_or("truncated optional header")?;
    let is_64 = match (magic, machine) {
        (OPT_MAGIC_PE32P, MACHINE_AMD64) => true,
        (OPT_MAGIC_PE32, MACHINE_I386) => false,
        (OPT_MAGIC_PE32P, _) => return Err("PE32+ but not an x86-64 machine"),
        (OPT_MAGIC_PE32, _) => return Err("PE32 but not an i386 machine"),
        _ => return Err("unknown optional header magic"),
    };

    let entry_rva = u32_at(d, opt + 16).ok_or("truncated optional header")?;
    // ImageBase is 8 bytes into a PE32+ optional header and 4 into a PE32 one;
    // every data-directory offset past it shifts by the same 4.
    let (image_base, dir_at) = if is_64 {
        (u64_at(d, opt + 24).ok_or("truncated optional header")?, opt + 112)
    } else {
        (u32_at(d, opt + 28).ok_or("truncated optional header")? as u64, opt + 96)
    };
    let size_of_image = u32_at(d, opt + 56).ok_or("truncated optional header")?;
    let size_of_headers = u32_at(d, opt + 60).ok_or("truncated optional header")?;

    // Data directory index 1 is the import table, 5 the base relocations.
    let import_rva = u32_at(d, dir_at + 8).unwrap_or(0);
    let import_size = u32_at(d, dir_at + 12).unwrap_or(0);
    let reloc_rva = u32_at(d, dir_at + 40).unwrap_or(0);
    let reloc_size = u32_at(d, dir_at + 44).unwrap_or(0);

    if size_of_image == 0 || size_of_image > 64 * 1024 * 1024 {
        return Err("implausible image size");
    }

    Ok(PeInfo {
        is_64,
        image_base,
        entry_rva,
        size_of_image,
        size_of_headers,
        section_count,
        sections_at: opt + opt_size,
        import_rva,
        import_size,
        reloc_rva,
        reloc_size,
    })
}

/// What a run produced, for reporting.
#[derive(Clone, Copy)]
pub struct RunResult {
    pub exit_code: u32,
    pub base: u64,
    pub sections: usize,
    pub imports: usize,
    pub relocs: usize,
    pub entry: u64,
}

/// Load and execute a PE image.
pub fn run(d: &[u8]) -> Result<RunResult, &'static str> {
    let info = parse(d)?;

    if !info.is_64 {
        return Err("32-bit executable: needs compatibility mode, not implemented");
    }

    let pages = ((info.size_of_image as usize) + 4095) / 4096;
    let base = mem::frame_allocator().alloc_contig(pages);
    if base == 0 {
        return Err("could not allocate the image");
    }

    serial::print_str("[pe] loading ");
    serial::print_hex(info.size_of_image as u64);
    serial::print_str(" bytes at 0x");
    serial::print_hex(base);
    serial::print_str(" (preferred 0x");
    serial::print_hex(info.image_base);
    serial::print_str(")\n");

    // Map the image there.  Sections land at their virtual addresses; the gaps
    // between them have to be zeroed, because the allocator hands back
    // whatever was in those pages before.
    unsafe {
        for i in 0..(info.size_of_image as usize) {
            core::ptr::write_volatile((base as *mut u8).add(i), 0);
        }
    }

    let copy = (info.size_of_headers as usize).min(d.len());
    unsafe {
        for i in 0..copy {
            core::ptr::write_volatile((base as *mut u8).add(i), d[i]);
        }
    }

    let mut mapped = 0usize;
    for i in 0..info.section_count as usize {
        let Some(s) = read_section(d, info.sections_at + i * 40) else {
            return Err("truncated section table");
        };
        // A section with no raw data (uninitialised .bss) is already zeroed.
        if s.raw_size == 0 || s.raw_ptr == 0 {
            mapped += 1;
            continue;
        }
        let src = s.raw_ptr as usize;
        let n = (s.raw_size as usize)
            .min(s.virtual_size.max(s.raw_size) as usize)
            .min(d.len().saturating_sub(src));
        for j in 0..n {
            unsafe {
                core::ptr::write_volatile((base as *mut u8).add(s.virtual_address as usize + j), d[src + j]);
            }
        }
        let _ = SCN_MEM_EXECUTE | SCN_MEM_READ | SCN_MEM_WRITE;
        mapped += 1;
    }

    // Relocations are not optional here: the image is almost never at the base
    // its linker chose.
    let relocs = apply_relocations(d, &info, base)?;

    let imports = bind_imports(d, &info, base)?;

    let entry = base + info.entry_rva as u64;
    serial::print_str("[pe] entry 0x");
    serial::print_hex(entry);
    serial::print_str(", ");
    serial::print_hex(imports as u64);
    serial::print_str(" imports, ");
    serial::print_hex(relocs as u64);
    serial::print_str(" relocations\n");

    let exit_code = unsafe { call_image(entry as usize) };

    Ok(RunResult { exit_code, base, sections: mapped, imports, relocs, entry })
}

/// Walk the base relocation table and add the load delta to every fixup.
fn apply_relocations(d: &[u8], info: &PeInfo, base: u64) -> Result<usize, &'static str> {
    let delta = base.wrapping_sub(info.image_base);
    if info.reloc_rva == 0 || info.reloc_size == 0 {
        // No relocation table.  Only acceptable if the image landed where it
        // wanted to, which it almost certainly did not.
        return if delta == 0 {
            Ok(0)
        } else {
            Err("no relocation table and the image cannot load at its base")
        };
    }
    if delta == 0 {
        return Ok(0);
    }

    let start = rva_to_offset(d, info, info.reloc_rva)?;
    let end = start + info.reloc_size as usize;
    let mut at = start;
    let mut applied = 0usize;

    while at + 8 <= end && at + 8 <= d.len() {
        let page_rva = u32_at(d, at).ok_or("bad relocation block")?;
        let block_size = u32_at(d, at + 4).ok_or("bad relocation block")? as usize;
        if block_size < 8 {
            break;
        }
        let entries = (block_size - 8) / 2;
        for e in 0..entries {
            let entry = u16_at(d, at + 8 + e * 2).ok_or("bad relocation entry")?;
            let kind = entry >> 12;
            let offset = (entry & 0x0FFF) as u32;
            let target = base + (page_rva + offset) as u64;
            match kind {
                REL_ABSOLUTE => {}
                REL_DIR64 => unsafe {
                    // Read through a raw pointer: the field is inside the image,
                    // at an address the compiler knows nothing about.
                    let p = target as *mut u64;
                    let v = core::ptr::read_unaligned(p);
                    core::ptr::write_unaligned(p, v.wrapping_add(delta));
                    applied += 1;
                },
                REL_HIGHLOW => unsafe {
                    let p = target as *mut u32;
                    let v = core::ptr::read_unaligned(p);
                    core::ptr::write_unaligned(p, v.wrapping_add(delta as u32));
                    applied += 1;
                },
                _ => return Err("unsupported relocation type"),
            }
        }
        at += block_size;
    }
    Ok(applied)
}

/// Resolve every imported name against the Win32 stub table and write the
/// addresses into the import address table.
fn bind_imports(d: &[u8], info: &PeInfo, base: u64) -> Result<usize, &'static str> {
    if info.import_rva == 0 {
        return Ok(0);
    }
    let mut at = rva_to_offset(d, info, info.import_rva)?;
    let mut bound = 0usize;
    let ptr_size = if info.is_64 { 8 } else { 4 };

    loop {
        let name_rva = u32_at(d, at + 12).ok_or("bad import descriptor")?;
        let first_thunk = u32_at(d, at + 16).ok_or("bad import descriptor")?;
        let original_thunk = u32_at(d, at).unwrap_or(0);
        if name_rva == 0 && first_thunk == 0 {
            break;                          // terminator
        }

        let name_at = rva_to_offset(d, info, name_rva)?;
        let dll = read_cstr(d, name_at, 32);
        serial::print_str("[pe] imports from ");
        serial::print_str(dll);
        serial::print_str("\n");

        // Prefer the original thunk (the name table) and fall back to the IAT,
        // which some linkers leave as the only copy.
        let lookup_rva = if original_thunk != 0 { original_thunk } else { first_thunk };
        let mut lt = rva_to_offset(d, info, lookup_rva)?;

        let mut i = 0usize;
        loop {
            let entry = if info.is_64 {
                u64_at(d, lt).ok_or("bad thunk")?
            } else {
                u32_at(d, lt).ok_or("bad thunk")? as u64
            };
            if entry == 0 {
                break;
            }
            let func_name = {
                // High bit set means "import by ordinal", which has no name to
                // look up; our table is by name only.
                if entry & (1 << 63) != 0 {
                    None
                } else {
                    let hint_at = rva_to_offset(d, info, entry as u32)?;
                    Some(read_cstr(d, hint_at + 2, 64))
                }
            };

            let target = match func_name.and_then(|n| win32::resolve(n)) {
                Some(addr) => addr,
                None => {
                    serial::print_str("[pe] unresolved import: ");
                    serial::print_str(func_name.unwrap_or("<by ordinal>"));
                    serial::print_str("\n");
                    return Err("an import could not be resolved");
                }
            };

            // Write into the IAT, not the lookup table.
            let iat = base + first_thunk as u64 + (i * ptr_size) as u64;
            unsafe {
                if info.is_64 {
                    core::ptr::write_unaligned(iat as *mut u64, target as u64);
                } else {
                    core::ptr::write_unaligned(iat as *mut u32, target as u32);
                }
            }
            bound += 1;
            i += 1;
            lt += ptr_size;
        }
        at += 20;
    }
    Ok(bound)
}

/// Translate a section-relative address into a file offset.
fn rva_to_offset(d: &[u8], info: &PeInfo, rva: u32) -> Result<usize, &'static str> {
    // Anything inside the headers is at the same offset.
    if rva < info.size_of_headers {
        return Ok(rva as usize);
    }
    for i in 0..info.section_count as usize {
        let Some(s) = read_section(d, info.sections_at + i * 40) else {
            continue;
        };
        let span = s.virtual_size.max(s.raw_size);
        if rva >= s.virtual_address && rva < s.virtual_address + span {
            let off = s.raw_ptr + (rva - s.virtual_address);
            if off as usize >= d.len() {
                return Err("rva points past the file");
            }
            return Ok(off as usize);
        }
    }
    Err("rva is not inside any section")
}

/// Read a NUL-terminated string, bounded.
fn read_cstr(d: &[u8], at: usize, max: usize) -> &str {
    let end = d[at.min(d.len())..]
        .iter()
        .position(|&b| b == 0)
        .map(|p| at + p)
        .unwrap_or(d.len());
    let end = end.min(at + max).min(d.len());
    core::str::from_utf8(d.get(at..end).unwrap_or(&[])).unwrap_or("?")
}

// ---------------------------------------------------------------------------
//  Calling the image
// ---------------------------------------------------------------------------

/// Call a loaded image's entry point, and come back when it calls ExitProcess.
///
/// A program that never returns would otherwise take the kernel with it, so
/// ExitProcess is a longjmp back here rather than a real exit.  The
/// callee-saved registers are preserved across the call by hand: the image is
/// foreign code and there is no reason to trust it.
core::arch::global_asm!(
    r#"
    .text
    .globl barryos_pe_call
    .type  barryos_pe_call, @function
barryos_pe_call:
    mov  [rip + pe_entry], rdi
    push rbx
    push rbp
    push rsi
    push rdi
    push r12
    push r13
    push r14
    push r15
    mov  [rip + pe_rsp], rsp
    sub  rsp, 40                 // 32 bytes of shadow space, 8 to realign
    call [rip + pe_entry]
    add  rsp, 40
    xor  eax, eax                // returned normally
    jmp  pe_restore

    .globl barryos_pe_resume
    .type  barryos_pe_resume, @function
barryos_pe_resume:               // ExitProcess lands here, code in EAX
    mov  rsp, [rip + pe_rsp]
pe_restore:
    pop  r15
    pop  r14
    pop  r13
    pop  r12
    pop  rdi
    pop  rsi
    pop  rbp
    pop  rbx
    ret

    .globl barryos_win32_ExitProcess
    .type  barryos_win32_ExitProcess, @function
barryos_win32_ExitProcess:       // Win64 ABI: the code arrives in ECX
    mov  eax, ecx
    jmp  barryos_pe_resume

    .data
    .align 8
pe_entry: .quad 0
pe_rsp:   .quad 0
"#
);

extern "C" {
    /// `entry` is the absolute address of the image's entry point.
    fn barryos_pe_call(entry: usize) -> u32;
}

/// Invoke a loaded image.  Returns its exit code.
unsafe fn call_image(entry: usize) -> u32 {
    barryos_pe_call(entry)
}
