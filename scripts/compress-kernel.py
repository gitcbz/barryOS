#!/usr/bin/env python3
"""Compress the kernel image for the BIOS loader, in LZ4 block format.

The BIOS can only be asked to write to a 16-bit segment:offset, so the whole
image has to be staged below 1 MiB before it is copied to 0x100000 — and that
window is 572 KiB, which the kernel outgrew.  The window cannot be made
bigger, and copying it up in pieces means switching back to real mode between
pieces, which this boot chain has already been through once and did not
survive.

Compressing it removes the problem instead of working around it: 591 KiB of
kernel is about 240 KiB on disk, and the staging window stops being a
constraint at all — which matters for more than size, since the TLS trust
store is a set of certificates and wanted to be bigger than it could be.

LZ4 block format because it is small enough to decode in the boot loader:
a token byte, some literals, a two-byte offset back into what has already been
written, and lengths extended by a run of 255s.  No entropy coding, no
dictionary, no bit reader — about sixty instructions in assembly.

The decoder here exists to check the encoder.  Whatever this writes is read
back and compared before it is written out, so the only untested part left is
the assembly, and a boot either is a boot or is not.

Usage: scripts/compress-kernel.py <kernel.bin> <out.lz>
"""

import io
import sys

MIN_MATCH = 4
MAX_OFFSET = 65535
LAST_LITERALS = 5


def compress(data):
    """LZ4 block format, greedy with a single-entry hash table."""
    n = len(data)
    out = bytearray()
    table = {}
    i = 0
    lit = 0

    def emit_sequence(literal_len, offset, match_len):
        token_at = len(out)
        out.append(0)
        token = 0
        if literal_len >= 15:
            token = 0xF0
        else:
            token = literal_len << 4
        if match_len - MIN_MATCH >= 15:
            token |= 0x0F
        else:
            token |= match_len - MIN_MATCH
        out[token_at] = token
        if literal_len >= 15:
            rest = literal_len - 15
            while rest >= 255:
                out.append(255)
                rest -= 255
            out.append(rest)
        out.extend(data[i - literal_len:i])
        out.append(offset & 0xFF)
        out.append((offset >> 8) & 0xFF)
        if match_len - MIN_MATCH >= 15:
            rest = (match_len - MIN_MATCH) - 15
            while rest >= 255:
                out.append(255)
                rest -= 255
            out.append(rest)

    while i < n:
        best_len = 0
        best_off = 0
        if i + MIN_MATCH <= n:
            key = data[i:i + MIN_MATCH]
            cand = table.get(key)
            if cand is not None:
                off = i - cand
                if off <= MAX_OFFSET:
                    l = MIN_MATCH
                    while (i + l < n and data[cand + l] == data[i + l]
                           and l < 65535 + MIN_MATCH + 15):
                        l += 1
                    best_len = l
                    best_off = off
            table[key] = i

        if best_len >= MIN_MATCH:
            lit_len = i - lit
            emit_sequence(lit_len, best_off, best_len)
            # Every position the match covered is a place a future match could
            # start; indexing them is what makes the next search find
            # something.
            end = min(i + best_len, n - MIN_MATCH + 1)
            for k in range(i + 1, end):
                table[data[k:k + MIN_MATCH]] = k
            i += best_len
            lit = i
        else:
            i += 1

    # The last literals, and the rule that a block ends with a literal run.
    lit_len = n - lit
    if lit_len > 0 or len(out) == 0:
        token_at = len(out)
        out.append(0)
        token = 0xF0 if lit_len >= 15 else (lit_len << 4)
        out[token_at] = token
        if lit_len >= 15:
            rest = lit_len - 15
            while rest >= 255:
                out.append(255)
                rest -= 255
            out.append(rest)
        out.extend(data[lit:])
    return bytes(out)


def decompress(src, out_len):
    """The format's decoder, written from the description rather than from
    the encoder, so that a mistake shared between the two would show up."""
    out = bytearray()
    i = 0
    while i < len(src):
        token = src[i]
        i += 1
        lit_len = token >> 4
        if lit_len == 15:
            while True:
                b = src[i]
                i += 1
                lit_len += b
                if b != 255:
                    break
        out.extend(src[i:i + lit_len])
        i += lit_len
        if i >= len(src):
            break
        offset = src[i] | (src[i + 1] << 8)
        i += 2
        if offset == 0:
            raise ValueError("zero offset at %d" % i)
        match_len = (token & 0x0F) + MIN_MATCH
        if (token & 0x0F) == 15:
            while True:
                b = src[i]
                i += 1
                match_len += b
                if b != 255:
                    break
        start = len(out) - offset
        if start < 0:
            raise ValueError("offset before the start at %d" % i)
        for k in range(match_len):
            out.append(out[start + k])
    if len(out) != out_len:
        raise ValueError("decoded %d bytes, expected %d" % (len(out), out_len))
    return bytes(out)


def main():
    if len(sys.argv) < 3:
        sys.exit(__doc__)
    src_path, out_path = sys.argv[1], sys.argv[2]
    data = io.open(src_path, "rb").read()

    packed = compress(data)
    back = decompress(packed, len(data))
    if back != data:
        where = next((k for k in range(len(data)) if back[k] != data[k]), None)
        sys.exit("compress-kernel: round trip failed at byte %s" % where)

    io.open(out_path, "wb").write(packed)
    ratio = 100.0 * len(packed) / max(1, len(data))
    print("[kernel] %d bytes -> %d compressed (%.0f%%)"
          % (len(data), len(packed), ratio))


if __name__ == "__main__":
    main()
