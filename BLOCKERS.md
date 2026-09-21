# barryOS — BLOCKERS

Only items that genuinely cannot be resolved autonomously appear here.
Everything else is decided in DECISIONS.md and worked around.

## Open
- (none currently)

## Resolved
- [RESOLVED 2026-09-21] No passwordless sudo in sandbox.
  Worked around by `apt-get download` + `dpkg-deb -x` into `~/.opt` (see
  BOOTSTRAP.md). QEMU 10.0.13 + xorriso + mtools + OVMF all functional.

## Deferred (not blockers, pending later stages)
- VMware Workstation 17 Pro final manual acceptance (BIOS + UEFI boot to
  desktop) — requires Stage 7+ desktop. Will be scripted where possible and
  marked VMware-PENDING until then. QEMU acceptance is the proxy for now.
- Real hardware tests (USB boot, printer, GPU passthrough) — out of scope
  until Stage 11 and only if hardware is provided.
