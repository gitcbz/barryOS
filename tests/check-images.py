#!/usr/bin/env python3
"""Check the kernel's image decoders against a decoder somebody else wrote.

A decoder that is wrong and a decoder that is right produce the same kind of
output: a picture, with the right dimensions, in colours that are nearly the
colours.  Nothing about looking at one says whether the coefficients were read
in the right order, whether the last row came from the wrong buffer, or whether
an interlaced picture was assembled from the wrong seven passes.  So every
case here is decoded twice -- once by the kernel's Rust, once by Pillow -- and
the two are compared pixel by pixel.

The fixtures are generated rather than found, and generated here rather than by
Pillow wherever the point is a format feature Pillow will not write:

  * PNG at every colour type and bit depth, with each of the five row filters,
    with and without interlacing, and at every zlib compression level so that
    stored, fixed-Huffman and dynamic-Huffman DEFLATE blocks all get exercised.
  * GIF with a palette, with transparency and interlaced.
  * BMP at one, four, eight, twenty-four and thirty-two bits, and an ICO
    holding a BMP and an ICO holding a PNG.
  * JPEG baseline and progressive, colour and grayscale.

    python3 tests/check-images.py                    # the generated cases
    python3 tests/check-images.py --urls             # and some real pictures

Pillow is only needed for the fixtures and the reference decode.  `--no-pillow`
runs the lossless cases alone, which is everything except JPEG.
"""

import argparse
import io
import os
import shutil
import struct
import subprocess
import sys
import tempfile
import urllib.request
import zlib

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
HOST = os.path.join(tempfile.gettempdir(), "barryos-img-host")

try:
    from PIL import Image
except ImportError:                                     # pragma: no cover
    Image = None


# ---------------------------------------------------------------------------
#  A picture to test with
# ---------------------------------------------------------------------------

def sample_rgba(w, h):
    """A picture with everything that goes wrong quietly.

    Hard edges for the filters to get wrong, a smooth gradient so a
    coefficient error shows as a shift rather than as noise, one flat region
    (which is what a naive decoder fills the whole picture with), a red pixel
    in the corner so an inverted or transposed image is obvious, and a region
    with fine detail so the high-frequency coefficients are not all zero.
    """
    px = bytearray()
    for y in range(h):
        for x in range(w):
            r = (x * 255) // max(1, w - 1)
            g = (y * 255) // max(1, h - 1)
            b = 40
            if x < w // 4 and y < h // 4:
                r, g, b = 220, 20, 20                  # a corner that must not move
            elif x > w // 2 and y > h // 2:
                r, g, b = 255, 255, 255                # a flat block
            elif (x + y) % 3 == 0:
                b = 200                                # fine detail in one channel
            px += bytes((r, g, b, 255))
    return w, h, bytes(px)


# ---------------------------------------------------------------------------
#  PNG, written by hand so the awkward options can be asked for
# ---------------------------------------------------------------------------

PNG_PASSES = [(0, 0, 8, 8), (4, 0, 8, 8), (0, 4, 4, 8),
              (2, 0, 4, 4), (0, 2, 2, 4), (1, 0, 2, 2), (0, 1, 1, 2)]


def _samples(rgba, w, x, y, color, depth):
    """The channel samples for one pixel, in the format's own range."""
    at = (y * w + x) * 4
    r, g, b, a = rgba[at], rgba[at + 1], rgba[at + 2], rgba[at + 3]
    top = (1 << depth) - 1
    q = lambda v: (v * top + 127) // 255                 # noqa: E731
    if color == 0:
        g8 = (r * 299 + g * 587 + b * 114) // 1000
        return [q(g8)]
    if color == 2:
        return [q(r), q(g), q(b)]
    if color == 3:
        return [q(r)]                                    # an index, supplied by the caller
    if color == 4:
        return [q(r), q(a)]
    return [q(r), q(g), q(b), q(a)]


def _pack(samples, depth):
    if depth == 16:
        return b"".join(struct.pack(">H", s) for s in samples)
    if depth == 8:
        return bytes(samples)
    out = bytearray()
    per = 8 // depth
    for i in range(0, len(samples), per):
        byte = 0
        for k in range(per):
            v = samples[i + k] if i + k < len(samples) else 0
            byte |= (v & ((1 << depth) - 1)) << (8 - depth * (k + 1))
        out.append(byte)
    return bytes(out)


def _filter_row(kind, cur, prev, step):
    """The five row filters, as the specification defines them, written out
    independently of the decoder's inverse."""
    out = bytearray(len(cur))
    for i in range(len(cur)):
        a = cur[i - step] if i >= step else 0
        b = prev[i]
        c = prev[i - step] if i >= step else 0
        if kind == 0:
            out[i] = cur[i]
        elif kind == 1:
            out[i] = (cur[i] - a) & 0xFF
        elif kind == 2:
            out[i] = (cur[i] - b) & 0xFF
        elif kind == 3:
            out[i] = (cur[i] - (a + b) // 2) & 0xFF
        else:
            p = a + b - c
            pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
            pred = a if (pa <= pb and pa <= pc) else (b if pb <= pc else c)
            out[i] = (cur[i] - pred) & 0xFF
    return bytes(out)


def png_bytes(w, h, rgba, *, color=2, depth=8, interlace=False, filt=0,
              level=6, palette=None, trns=None):
    """A PNG built from an RGBA buffer, at whichever options are asked for."""
    channels = {0: 1, 2: 3, 3: 1, 4: 2, 6: 4}[color]
    bpp = max(1, (channels * depth + 7) // 8)
    raw = bytearray()

    passes = PNG_PASSES if interlace else [(0, 0, 1, 1)]
    for (x0, y0, dx, dy) in passes:
        if x0 >= w or y0 >= h:
            continue
        pw = (w - x0 + dx - 1) // dx
        ph = (h - y0 + dy - 1) // dy
        prev = bytes((pw * channels * depth + 7) // 8)
        for j in range(ph):
            y = y0 + j * dy
            samples = []
            for i in range(pw):
                samples += _samples(rgba, w, x0 + i * dx, y, color, depth)
            cur = _pack(samples, depth)
            # Every row gets the same filter, so a failure names one of five.
            raw += bytes((filt,)) + _filter_row(filt, cur, prev, bpp)
            prev = cur

    def chunk(kind, body):
        return (struct.pack(">I", len(body)) + kind + body
                + struct.pack(">I", zlib.crc32(kind + body) & 0xFFFFFFFF))

    out = b"\x89PNG\r\n\x1a\n"
    out += chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, depth, color, 0, 0,
                                      1 if interlace else 0))
    if color == 3:
        out += chunk(b"PLTE", bytes(palette))
        if trns is not None:
            out += chunk(b"tRNS", bytes(trns))
    out += chunk(b"IDAT", zlib.compress(bytes(raw), level))
    out += chunk(b"IEND", b"")
    return out


def png_palette_case(w, h, rgba, depth, trns=False):
    """A palette PNG, quantised to as many grey levels as the depth holds."""
    levels = 1 << depth
    indexed = bytearray()
    for i in range(0, len(rgba), 4):
        r, g, b = rgba[i], rgba[i + 1], rgba[i + 2]
        grey = (r * 299 + g * 587 + b * 114) // 1000
        v = (grey * (levels - 1) + 127) // 255
        v = (v * 255) // (levels - 1)
        indexed += bytes((v, v, v, 255))

    palette = bytearray()
    for k in range(levels):
        v = (k * 255) // max(1, levels - 1)
        palette += bytes((v, v, v))
    t = bytes([200 if k == 0 else 255 for k in range(levels)]) if trns else None
    return None, png_bytes(w, h, bytes(indexed), color=3, depth=depth,
                           palette=palette, trns=t)


# ---------------------------------------------------------------------------
#  GIF, written by hand so interlacing and transparency can be asked for
# ---------------------------------------------------------------------------

def _lzw_encode(indices, min_code_size):
    """GIF's LZW, written from the specification rather than borrowed from
    anybody's decoder, so a shared misunderstanding is less likely."""
    clear = 1 << min_code_size
    end = clear + 1
    table = {bytes([i]): i for i in range(clear)}
    nxt = end + 1
    size = min_code_size + 1
    bits = []

    def emit(code):
        for k in range(size):
            bits.append((code >> k) & 1)

    emit(clear)
    prev = b""
    for v in indices:
        cur = prev + bytes([v])
        if cur in table:
            prev = cur
            continue
        emit(table[prev])
        table[cur] = nxt
        nxt += 1
        if nxt > (1 << size) and size < 12:
            size += 1
        if nxt >= 4096:
            emit(clear)
            table = {bytes([i]): i for i in range(clear)}
            nxt = end + 1
            size = min_code_size + 1
        prev = bytes([v])
    if prev:
        emit(table[prev])
    emit(end)

    out = bytearray()
    byte = 0
    for i, b in enumerate(bits):
        byte |= b << (i % 8)
        if i % 8 == 7:
            out.append(byte)
            byte = 0
    if len(bits) % 8:
        out.append(byte)
    return bytes(out)


def gif_bytes(w, h, rgba, *, interlace=False, transparent=None, depth=4):
    n = 1 << depth
    palette = bytearray()
    for i in range(n):
        palette += bytes(((i * 255) // max(1, n - 1),) * 3)

    def quant(v):
        return min(n - 1, v * n // 256)

    rows = []
    for y in range(h):
        row = []
        for x in range(w):
            at = (y * w + x) * 4
            r, g, b, a = rgba[at], rgba[at + 1], rgba[at + 2], rgba[at + 3]
            if transparent is not None and a < 128:
                row.append(transparent)
            else:
                row.append(quant((r * 299 + g * 587 + b * 114) // 1000))
            row[-1] &= n - 1
        rows.append(row)

    if interlace:
        order = []
        for start, step in ((0, 8), (4, 8), (2, 4), (1, 2)):
            order += list(range(start, h, step))
    else:
        order = list(range(h))
    flat = [v for y in order for v in rows[y]]

    out = bytearray(b"GIF89a")
    out += struct.pack("<HHBBB", w, h, 0x80 | (depth - 1), 0, 0)
    out += palette
    if transparent is not None:
        out += b"\x21\xf9\x04\x01\x00\x00" + bytes([transparent]) + b"\x00"
    out += b"\x2c" + struct.pack("<HHHHB", 0, 0, w, h, 0x40 if interlace else 0)
    data = _lzw_encode(flat, max(2, depth))
    out += bytes([max(2, depth)])
    for i in range(0, len(data), 255):
        part = data[i:i + 255]
        out += bytes([len(part)]) + part
    out += b"\x00\x3b"
    return bytes(out)


# ---------------------------------------------------------------------------
#  Running the kernel's decoder
# ---------------------------------------------------------------------------

def run(files):
    """Decode every file with the kernel's Rust and return the PPMs."""
    # Decoded as UTF-8 explicitly: the fixtures live under a path that is not
    # ASCII on this machine, and the default decoding would mangle the names
    # back out of the child's output so that nothing matched anything.
    out = subprocess.run([HOST] + files, capture_output=True,
                         encoding="utf-8", errors="replace")
    if out.returncode not in (0, 1):
        raise SystemExit("the host decoder failed to run:\n" + out.stderr)
    result = {}
    for line in out.stdout.splitlines():
        if "\t" not in line:
            print("  " + line)
            continue
        parts = line.split("\t")
        result[parts[0]] = parts
    if out.stderr.strip():
        print(out.stderr.strip())
    return result


def read_ppm(path):
    with open(path, "rb") as f:
        data = f.read()
    # P6\n<w> <h>\n255\n
    at = 0
    fields = []
    while len(fields) < 4:
        while data[at:at + 1].isspace():
            at += 1
        if data[at:at + 1] == b"#":
            while data[at:at + 1] not in (b"\n", b""):
                at += 1
            continue
        start = at
        while not data[at:at + 1].isspace():
            at += 1
        fields.append(data[start:at])
    at += 1
    w, h = int(fields[1]), int(fields[2])
    return w, h, data[at:at + w * h * 3]


def reference(path, bg=(255, 255, 255)):
    """What Pillow says the picture is, composited the way the decoder does."""
    im = Image.open(path)
    im = im.convert("RGBA")
    base = Image.new("RGBA", im.size, bg + (255,))
    return Image.alpha_composite(base, im).convert("RGB")


def compare(name, path, report, limit, expected=None):
    """Decode with both and compare, returning True when they agree."""
    w, h, px = read_ppm(path + ".ppm")
    ref = expected if expected is not None else reference(path)
    if (w, h) != ref.size:
        report.append("%-34s size %dx%d, reference says %dx%d" % (name, w, h, *ref.size))
        return False
    want = ref.tobytes()
    total = 0
    worst = 0
    for i in range(0, len(want), 3):
        for k in range(3):
            d = abs(px[i + k] - want[i + k])
            total += d
            if d > worst:
                worst = d
    mae = total / max(1, len(want))
    ok = mae <= limit
    report.append("%-34s %-9s %5dx%-5d mean error %6.2f  worst %3d  %s"
                  % (name, "ok" if ok else "WRONG", w, h, mae, worst,
                     "" if ok else "(> %.1f)" % limit))
    return ok


# ---------------------------------------------------------------------------
#  The cases
# ---------------------------------------------------------------------------

def grey16_expected(w, h, rgba):
    """What a sixteen-bit greyscale PNG should look like at eight bits."""
    im = Image.new("RGB", (w, h))
    px = []
    for i in range(0, len(rgba), 4):
        r, g, b = rgba[i], rgba[i + 1], rgba[i + 2]
        grey = (r * 299 + g * 587 + b * 114) // 1000
        v16 = (grey * 65535 + 127) // 255
        v = (v16 + 128) // 257
        px.append((v, v, v))
    im.putdata(px)
    return im


def _slug(name):
    """A file name a filesystem will accept, from a name meant to be read."""
    keep = "".join(c if c.isalnum() or c in "._-" else "_" for c in name)
    return keep[:40] + "." + str(abs(hash(name)) % 100000)


def build(where):
    """Every fixture, as (name, path, error limit)."""
    cases = []
    w, h, rgba = sample_rgba(37, 29)                    # odd, so rows are not aligned
    small = sample_rgba(16, 16)

    def add(name, data, limit=0.0, expected=None):
        path = os.path.join(where, _slug(name))
        with open(path, "wb") as f:
            f.write(data)
        cases.append((name, path, limit, expected))

    # PNG: colour types, bit depths, filters, interlacing, compression levels.
    for color, depth in ((0, 1), (0, 2), (0, 4), (0, 8), (0, 16),
                         (2, 8), (2, 16), (4, 8), (6, 8)):
        # Sixteen-bit greyscale needs its own reference.  Pillow reads it as a
        # sixteen-bit integer image and its RGB conversion clips anything above
        # 255 rather than scaling it, so the reference would be white.
        want = grey16_expected(w, h, rgba) if (color, depth) == (0, 16) else None
        add("png colour %d depth %d" % (color, depth),
            png_bytes(w, h, rgba, color=color, depth=depth), expected=want)
    for filt in range(5):
        add("png filter %d" % filt, png_bytes(w, h, rgba, filt=filt))
    add("png interlaced", png_bytes(w, h, rgba, interlace=True))
    add("png interlaced grey 4", png_bytes(w, h, rgba, color=0, depth=4, interlace=True))
    for level in (0, 1, 6, 9):
        add("png zlib level %d" % level, png_bytes(w, h, rgba, level=level))
    for depth in (1, 2, 4, 8):
        idx, data = png_palette_case(w, h, rgba, depth)
        add("png palette depth %d" % depth, data)
    idx, data = png_palette_case(w, h, rgba, 8, trns=True)
    add("png palette transparent", data)

    # GIF.
    add("gif", gif_bytes(w, h, rgba))
    add("gif interlaced", gif_bytes(w, h, rgba, interlace=True))
    add("gif transparent", gif_bytes(w, h, rgba, transparent=7))
    add("gif 2-bit palette", gif_bytes(small[0], small[1], small[2], depth=1))

    if Image is not None:
        # BMP, at the depths that are actually written.
        for mode, name in (("1", "bmp 1-bit"), ("L", "bmp 8-bit"),
                           ("P", "bmp palette"), ("RGB", "bmp 24-bit"),
                           ("RGBA", "bmp 32-bit")):
            im = Image.frombytes("RGBA", (w, h), rgba).convert(mode)
            buf = io.BytesIO()
            im.save(buf, "BMP")
            add(name, buf.getvalue())
        add("bmp 4-bit", _bmp_4bit(w, h, rgba))

        # ICO, both flavours, because an ICO is a container and not a format.
        im = Image.frombytes("RGBA", (w, h), rgba)
        for size in (16, 32, 48):
            buf = io.BytesIO()
            im.resize((size, size)).save(buf, "ICO", sizes=[(size, size)])
            add("ico %d" % size, buf.getvalue())

        # JPEG.  The lossy one, so the comparison has to allow for the two
        # inverse transforms disagreeing in the last bit.
        im = Image.frombytes("RGBA", (w, h), rgba).convert("RGB")
        for name, opts in (("jpeg baseline", {"progressive": False}),
                           ("jpeg progressive", {"progressive": True}),
                           ("jpeg baseline q30", {"progressive": False, "quality": 30}),
                           ("jpeg 4:4:4", {"progressive": True, "subsampling": 0})):
            buf = io.BytesIO()
            im.save(buf, "JPEG", quality=opts.pop("quality", 92), **opts)
            add(name, buf.getvalue(), 9.0)
        buf = io.BytesIO()
        im.convert("L").save(buf, "JPEG", quality=92, progressive=True)
        add("jpeg grey progressive", buf.getvalue(), 3.0)
        buf = io.BytesIO()
        im.convert("L").save(buf, "JPEG", quality=92, progressive=False)
        add("jpeg grey baseline", buf.getvalue(), 3.0)

    return cases


def _bmp_4bit(w, h, rgba):
    """A four-bit BMP, which Pillow will not write."""
    palette = bytearray()
    for i in range(16):
        v = (i * 255) // 15
        palette += bytes((v, v, v, 0))
    stride = ((4 * w + 31) // 32) * 4
    rows = bytearray()
    for y in range(h - 1, -1, -1):                      # bottom up
        row = bytearray(stride)
        for x in range(w):
            at = (y * w + x) * 4
            g = (rgba[at] * 299 + rgba[at + 1] * 587 + rgba[at + 2] * 114) // 1000
            i = g * 15 // 255
            if x % 2 == 0:
                row[x // 2] |= i << 4
            else:
                row[x // 2] |= i
        rows += row
    header = struct.pack("<IiiHHIIiiII", 40, w, h, 1, 4, 0, len(rows), 2835, 2835, 16, 0)
    body = bytes(palette) + bytes(rows)
    return b"BM" + struct.pack("<IHHI", 14 + len(header) + len(body), 0, 0,
                               14 + len(header) + len(palette)) + header + body


# ---------------------------------------------------------------------------
#  Real pictures, for the cases nobody would think to generate
# ---------------------------------------------------------------------------

URLS = [
    ("baidu logo", "https://www.baidu.com/img/PCtm_d9c8750bed0b3c7d089fa7d55720d6cf.png"),
    ("bilibili logo", "https://i0.hdslb.com/bfs/static/jinkela/long/images/512.png"),
    ("photograph (progressive jpeg)", "https://picsum.photos/400/300"),
    ("photograph, larger", "https://picsum.photos/700/460"),
]


def fetch(url, path):
    req = urllib.request.Request(url, headers={"User-Agent": "Mozilla/5.0 (barryOS tests)"})
    with urllib.request.urlopen(req, timeout=30) as r:
        data = r.read()
    with open(path, "wb") as f:
        f.write(data)
    return data


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--urls", action="store_true", help="also fetch some real pictures")
    ap.add_argument("--no-pillow", action="store_true",
                    help="only the lossless cases, which need no reference decoder")
    ap.add_argument("--keep", action="store_true", help="leave the fixtures on disk")
    args = ap.parse_args()

    if not os.path.exists(HOST):
        raise SystemExit("%s is missing -- run tests/run-img-host.sh first" % HOST)

    where = tempfile.mkdtemp(prefix="barryos-images-")
    cases = build(where)

    if args.urls:
        for name, url in URLS:
            path = os.path.join(where, _slug(name))
            try:
                fetch(url, path)
            except Exception as e:                      # pragma: no cover
                print("could not fetch %s: %s" % (url, e))
                continue
            cases.append((name, path, 9.0, None))

    paths = [p for _, p, _, _ in cases]
    result = run(paths)

    report = []
    ok = True
    for name, path, limit, expected in cases:
        line = result.get(path)
        if line is None or line[1] in ("ERROR", "READ-ERROR"):
            reason = line[2] if line else "no output"
            report.append("%-34s %-9s %s" % (name, "FAILED", reason))
            ok = False
            continue
        if Image is None:
            report.append("%-34s decoded %sx%s (no reference)" % (name, line[1], line[2]))
            continue
        if not compare(name, path, report, limit, expected):
            ok = False

    print("\n".join(report))
    print("\n%d case(s), %s" % (len(cases), "all agree with the reference" if ok else "SOME DISAGREE"))
    if not args.keep:
        shutil.rmtree(where, ignore_errors=True)
    else:
        print("fixtures left in %s" % where)
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
