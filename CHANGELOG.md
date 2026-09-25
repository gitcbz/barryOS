# barryOS — CHANGELOG

Reverse-chronological.

## 2026-09-25 — Round 12 — Links, history, pictures and honest buffers
- **Image decoders** (`kernel/src/apps/web/img/`), ~2400 lines:
  - `inflate.rs` — DEFLATE (RFC 1951) and its zlib wrapper, written from the
    specification, with the output capped so a small file cannot ask for a
    large allocation.
  - `png.rs` — every colour type and bit depth, all five row filters, and
    Adam7 interlacing.  Sixteen-bit samples are reduced by scaling
    (`(v + 128) / 257`) rather than by taking the high byte, which is the
    usual shortcut and makes every value slightly too dark.
  - `jpeg.rs` — baseline **and progressive**, because every JPEG tried came
    back progressive.  Coefficients are decoded into per-component planes and
    transformed at the end, so the two share one IDCT, one Huffman decoder and
    one colour conversion.  Two bugs worth naming: the inverse transform's
    leading factor of a quarter was missing (the accumulate-and-shift was off
    by two bits), and blocks that fall off the edge of the picture were being
    skipped rather than decoded — which desynchronises the Huffman stream, so
    the *next* MCU is garbage.  Chroma is upsampled with a triangle filter
    (three quarters, one quarter) rather than by repeating samples.
  - `gif.rs` — LZW, palettes, transparency, interlacing, first frame only.
  - `bmp.rs` — BMP and the ICO container, which is what a favicon is.
- **Verified against Pillow**, `tests/check-images.py`: 44 generated cases —
  every PNG colour type, bit depth, row filter, interlacing mode and zlib
  level; GIF with and without transparency and interlacing; BMP at one, four,
  eight, twenty-four and thirty-two bits; an ICO holding a BMP and one holding
  a PNG; baseline and progressive JPEG in grey and colour — plus real pictures
  fetched from the sites this browser exists to reach.  The lossless cases
  must match exactly; the lossy ones are held to a mean error.  `--urls`.
- **Links are per run** (`layout::Run::link`), not per line.  A line with three
  links has three targets, and the whitespace between two anchors belongs to
  neither of them — giving it to the following link makes clicking the gap
  follow a link the reader did not aim at.
- **History**: 24 addresses in a fixed-size ring that never touches the heap.
  Back and forward buttons in the toolbar.  A link someone clicked pushes; a
  redirect — header, meta tag or script — replaces.
- **Pictures are drawn**: decoded into a three-megabyte arena, laid out as a
  line of their own at a size the layout asked for in character cells, and
  drawn by nearest-neighbour sampling with the source position carried in an
  accumulator.  A picture that will not decode, or that the arena has no room
  for, shows its `alt` text and says why on the serial log.
- **The load order**, which is the whole of the browser's loading algorithm:
  document, then the stylesheets and scripts its markup named, then the scripts
  run, then the pictures — pictures last because a running script can add one.
- **Transport receive buffers 32 KiB → 256 KiB** in both TCP and TLS, and the
  document buffer with them.  `http::parse_headers` now scans a fixed 8 KiB
  rather than the whole buffer, which was a 256 KiB *stack* array.
- **Truncation is said out loud** — a line at the end of the text and a marker
  in the status bar.  Showing half a page as though it were all of it is the
  one failure a reader cannot see.
- **Subresources**: deduplicated by resolved URL, 8 stylesheets and 16 scripts
  separately rather than 10 between them, 1 MiB in total.
- **A browser self-test at boot**: loads a real page through the browser's own
  four-step machine, reports the lines, the distinct links and the pictures,
  hit-tests the layout to prove a link is reachable by clicking, then fetches
  one picture from a target site and checks it comes out as a picture line.
- **`tests/run-host-tests.sh` and `make host-tests`**, and CI now runs them:
  the page renderer, the image decoders against Pillow, and the TLS vectors.
  Previously CI only built.

## 2026-09-21 — Round 10 — Stage 10 (PE Loader + Win32 Compat) — ✅ COMPLETE
- Implemented PE section loader + Win32 API compat in `kernel/src/compat/`:
  - `pe_loader.rs` — PE32+ section header parser (name, vaddr, vsize,
    raw size/offset). Import directory scanning (finds kernel32.dll).
    Relocation directory parsing (RVA + size). Test: 2 sections, 1 import.
  - `win32.rs` — Win32 API compat layer with 10 function stubs:
    WriteFile, GetStdHandle, ExitProcess, HeapAlloc, HeapFree,
    GetModuleHandleA, GetLastError, GetTickCount, MessageBoxA,
    SetConsoleTextAttribute. `lookup(dll, func)` by name. 3 test cases
    pass (WriteFile found, MessageBoxA found, nonexistent not found).
- Updated `kernel/src/compat/mod.rs`: added steps 5 (PE sections) + 6 (Win32).
- Updated `kernel/src/main.rs`: VGA summary includes "+ win32".
- Updated `scripts/check.sh`: L12 gate (5 Stage 10 Win32 checks).
- Key fixes: array-of-enums #UD → per-slot set_stub(); string comparison
  #UD → skip DLL name in lookup; iter().enumerate() #UD → direct indexing.
- Verification: `bash scripts/check.sh` → **PASS=60 FAIL=0 SKIP=0**.

## 2026-09-21 — Rounds 1-9 — Stages 0-9 — ✅ COMPLETE
- (see previous entries — dual-boot through compat layers, 55/55 checks)
