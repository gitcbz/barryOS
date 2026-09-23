#!/usr/bin/env python3
"""Check the test PE against the loader's contract.

The loader maps the image at an arbitrary base, so it *needs* a relocation
directory; and it refuses to load if any imported name is missing from the
Win32 stub table.  Both are silent failures at build time and loud ones at
boot time, and there is no hypervisor in CI to boot and find out.

So assert them here instead: parse the real import directory of the real
`build/hello.exe` and every name in it, and diff the names against the
`resolve` arms of `kernel/src/compat/win32.rs`.  A change to either side that
breaks the pairing fails the build.

Usage: tests/pe-contract.py build/hello.exe kernel/src/compat/win32.rs
"""

import re
import struct
import sys

PE32 = 0x10B
PE32_PLUS = 0x20B
DIR_RELOC = 5
DIR_IMPORT = 1
SECT_MEM_EXECUTE = 0x20000000
RELOC_NAMES = {0: "ABSOLUTE", 3: "HIGHLOW", 10: "DIR64"}

failures = []


def fail(msg):
    failures.append(msg)
    print("  FAIL " + msg)


def ok(msg):
    print("  ok   " + msg)


def resolve_table(path):
    """The names `win32::resolve` knows, read out of the source."""
    text = open(path, encoding="utf-8").read()
    return set(re.findall(r'eq\(n,\s*"([^"]+)"\)', text))


class Pe:
    def __init__(self, data):
        self.d = data
        if data[:2] != b"MZ":
            raise ValueError("not an MZ image")
        pe_at = struct.unpack_from("<I", data, 0x3C)[0]
        if data[pe_at:pe_at + 4] != b"PE\0\0":
            raise ValueError("no PE signature at the header offset")
        self.pe_at = pe_at
        coff = pe_at + 4
        (machine, self.n_sections, _, _, _, self.opt_size, self.characteristics) = \
            struct.unpack_from("<HHIIIHH", data, coff)
        self.machine = machine
        self.sections_at = coff + 20 + self.opt_size
        opt = coff + 20
        self.magic = struct.unpack_from("<H", data, opt)[0]
        if self.magic == PE32_PLUS:
            self.entry_rva, self.base = struct.unpack_from("<II", data, opt + 16)
            self.size_of_image = struct.unpack_from("<I", data, opt + 56)[0]
            self.n_dirs = struct.unpack_from("<I", data, opt + 108)[0]
            self.dirs_at = opt + 112
        elif self.magic == PE32:
            self.entry_rva, self.base = struct.unpack_from("<II", data, opt + 16)
            self.size_of_image = struct.unpack_from("<I", data, opt + 56)[0]
            self.n_dirs = struct.unpack_from("<I", data, opt + 92)[0]
            self.dirs_at = opt + 96
        else:
            raise ValueError("unknown optional-header magic 0x%x" % self.magic)

    def directory(self, index):
        if index >= self.n_dirs:
            return (0, 0)
        return struct.unpack_from("<II", self.d, self.dirs_at + index * 8)

    def sections(self):
        out = []
        for i in range(self.n_sections):
            at = self.sections_at + i * 40
            name = self.d[at:at + 8].rstrip(b"\0").decode("ascii", "replace")
            vsize, vaddr, raw_size, raw_ptr = struct.unpack_from("<IIII", self.d, at + 8)
            ch = struct.unpack_from("<I", self.d, at + 36)[0]
            out.append(dict(name=name, vsize=vsize, vaddr=vaddr,
                            raw_size=raw_size, raw_ptr=raw_ptr, ch=ch))
        return out

    def rva_to_offset(self, rva):
        for s in self.sections():
            if s["vaddr"] <= rva < s["vaddr"] + max(s["vsize"], s["raw_size"]):
                delta = rva - s["vaddr"]
                if delta >= s["raw_size"]:
                    return None          # inside the virtual tail, no file bytes
                return s["raw_ptr"] + delta
        return rva if rva < self.size_of_image else None

    def cstr(self, off, limit=256):
        end = self.d.index(b"\0", off, off + limit)
        return self.d[off:end].decode("ascii", "replace")

    def imports(self):
        """(dll, [names]) for the whole import directory."""
        rva, size = self.directory(DIR_IMPORT)
        if rva == 0:
            return []
        out = []
        at = self.rva_to_offset(rva)
        while True:
            # IMAGE_IMPORT_DESCRIPTOR: OriginalFirstThunk, TimeDateStamp,
            # ForwarderChain, Name, FirstThunk.  The names live in the *lookup*
            # table (OriginalFirstThunk when present); FirstThunk is the IAT the
            # loader writes bound addresses into.
            oft, _, _, name_rva, first_thunk = struct.unpack_from("<IIIII", self.d, at)
            if name_rva == 0 and first_thunk == 0:
                break
            dll = self.cstr(self.rva_to_offset(name_rva))
            lookup = oft or first_thunk
            names, i = [], 0
            ptr = 8 if self.magic == PE32_PLUS else 4
            fmt = "<Q" if ptr == 8 else "<I"
            ordinal_bit = 1 << 63 if ptr == 8 else 1 << 31
            mask = ordinal_bit - 1
            while True:
                thunk_at = self.rva_to_offset(lookup + i * ptr)
                entry = struct.unpack_from(fmt, self.d, thunk_at)[0]
                if entry == 0:
                    break
                if entry & ordinal_bit:
                    names.append("#%d" % (entry & 0xFFFF))
                else:
                    hint_at = self.rva_to_offset(entry & mask)
                    names.append(self.cstr(hint_at + 2))
                i += 1
            out.append((dll, names))
            at += 20
        return out

    def relocations(self):
        """Every fixup as (type, rva), or None when there is no table."""
        rva, size = self.directory(DIR_RELOC)
        if rva == 0 or size == 0:
            return None
        start = self.rva_to_offset(rva)
        end = start + size
        out = []
        at = start
        while at + 8 <= end:
            page_rva, block_size = struct.unpack_from("<II", self.d, at)
            if block_size < 8:
                break
            for e in range((block_size - 8) // 2):
                entry = struct.unpack_from("<H", self.d, at + 8 + e * 2)[0]
                out.append((entry >> 12, page_rva + (entry & 0x0FFF)))
            at += block_size
        return out


def main():
    exe_path, win32_path = sys.argv[1], sys.argv[2]
    data = open(exe_path, "rb").read()
    pe = Pe(data)
    known = resolve_table(win32_path)

    print("headers")
    if pe.machine != 0x8664:
        fail("machine 0x%04x is not x86-64 (the loader only runs PE32+)" % pe.machine)
    else:
        ok("machine x86-64")
    if pe.magic != PE32_PLUS:
        fail("optional header is PE32, not PE32+ -- the loader rejects it outright")
    else:
        ok("PE32+ optional header")

    print("sections")
    for s in pe.sections():
        if s["raw_ptr"] + s["raw_size"] > len(data):
            fail("section %s runs past EOF" % s["name"])
        else:
            ok("%-8s raw %6d bytes -> rva 0x%05x" %
               (s["name"], s["raw_size"], s["vaddr"]))

    print("relocations")
    relocs = pe.relocations()
    if relocs is None:
        fail("no relocation directory: the image cannot be rebased, and the "
             "loader never places it at its preferred base")
    else:
        # The loader knows ABSOLUTE, HIGHLOW and DIR64 and errors on anything
        # else; and a table of nothing but ABSOLUTE terminators would mean the
        # relocation path is never actually exercised.
        kinds = {}
        for kind, rva in relocs:
            kinds[kind] = kinds.get(kind, 0) + 1
        ok("%d fixups: %s" % (
            len(relocs),
            ", ".join("%s x%d" % (RELOC_NAMES.get(k, "type %d" % k), v)
                      for k, v in sorted(kinds.items()))))
        unknown = [k for k in kinds if k not in (0, 3, 10)]
        if unknown:
            fail("relocation type(s) %s: the loader errors on these and the "
                 "load is refused" % unknown)
        if kinds.get(10, 0) == 0 and kinds.get(3, 0) == 0:
            fail("only ABSOLUTE fixups: rebasing this image would apply nothing, "
                 "so the loader's relocation step goes untested")
        else:
            n = kinds.get(10, 0) + kinds.get(3, 0)
            ok("%d real fixups -- the image genuinely needs rebasing" % n)

    print("entry point")
    entry_sec = None
    for s in pe.sections():
        if s["vaddr"] <= pe.entry_rva < s["vaddr"] + max(s["vsize"], s["raw_size"]):
            entry_sec = s
    if entry_sec is None:
        fail("entry RVA 0x%x is outside every section" % pe.entry_rva)
    elif not entry_sec["ch"] & SECT_MEM_EXECUTE:
        fail("entry RVA 0x%x is in %s, which is not executable" %
             (pe.entry_rva, entry_sec["name"]))
    else:
        ok("entry RVA 0x%x in %s" % (pe.entry_rva, entry_sec["name"]))

    print("imports vs win32::resolve")
    if pe.size_of_image == 0:
        fail("size_of_image is 0; the loader would map nothing")
    total = 0
    for dll, names in pe.imports():
        print("  %s:" % dll)
        for n in names:
            total += 1
            if n.startswith("#"):
                fail("import by ordinal (%s from %s); the table is by name only"
                     % (n, dll))
            elif n not in known:
                fail("%s is imported but win32::resolve does not know it -- "
                     "the load would be refused" % n)
            else:
                ok(n)
    if total == 0:
        fail("no imports at all: this image does not exercise import binding")
    else:
        ok("%d imports, all resolvable" % total)

    print()
    if failures:
        print("pe-contract: %d failure(s)" % len(failures))
        return 1
    print("pe-contract: the test PE satisfies every assumption the loader makes")
    return 0


if __name__ == "__main__":
    sys.exit(main())
