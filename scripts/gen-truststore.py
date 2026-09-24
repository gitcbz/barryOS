#!/usr/bin/env python3
"""Generate the trust store the kernel compiles in.

A TLS client has to decide which certificate authorities it believes, and that
decision has to be baked in: there is nothing to fetch it from at boot, and a
store that can be replaced over the network is not a trust store.

What is stored per root is its subject name and its SubjectPublicKeyInfo —
not the whole certificate.  The key is what verifying a signature needs and the
name is what finds it; the rest of a root certificate (validity, extensions,
and its own signature) is either irrelevant once the root is trusted or would
cost a third of the kernel's size budget to carry.

Both fields are sliced out of the certificate's own bytes rather than re-encoded
from a parsed form.  A Name has more than one legal DER spelling — PrintableString
and UTF8String both encode a country code, and a re-encoder is free to pick
either — and the kernel compares names byte for byte against the name in a chain
that a server just handed it.  Re-encoding is how the store ends up full of
entries that never match anything.

Sizes

    A Mozilla bundle root averages about 350 bytes of (subject, SPKI).  All 121
    of them is roughly 42 KiB of DER.  The emitted byte strings are the DER
    escaped one byte at a time, so the store costs its own size in the kernel
    image and nothing more -- see `rust_bytes`.  The BIOS loader stages the
    kernel below 1 MiB and gives it 572 KiB in total, so how many roots fit is a
    question of how much else the kernel is carrying.

    --roots picks: "all" for everything, or a comma-separated list of
    substrings matched against each root's subject.

Usage: scripts/gen-truststore.py <cacert.pem> <out.rs> [--roots all|list]
"""

import base64
import io
import os
import re
import sys


def load_roots(pem_path):
    """Every certificate in a PEM bundle, as DER."""
    text = io.open(pem_path, encoding="utf-8", errors="replace").read()
    blocks = re.findall(
        r"-----BEGIN CERTIFICATE-----.*?-----END CERTIFICATE-----", text, re.S
    )
    return [base64.b64decode("".join(b.splitlines()[1:-1])) for b in blocks]


def tlv(data, at):
    """(tag, body offset, body length, total length) for the element at `at`."""
    if at + 2 > len(data):
        return None
    tag = data[at]
    if tag & 0x1F == 0x1F:
        return None                     # high-tag-number form: not used in X.509
    b = data[at + 1]
    if b < 0x80:
        length, hdr = b, 2
    else:
        n = b & 0x7F
        if at + 2 + n > len(data):
            return None
        length, hdr = int.from_bytes(data[at + 2:at + 2 + n], "big"), 2 + n
    if at + hdr + length > len(data):
        return None
    return tag, at + hdr, length, hdr + length


def fields(cert):
    """(subject DER, SubjectPublicKeyInfo DER, readable name), sliced raw.

    Certificate ::= SEQUENCE { tbsCertificate, signatureAlgorithm, signature }

    TBSCertificate fields are positional, with [0] version in front when it is
    present at all.  Counting to the sixth and seventh slot is the whole parse:
    everything after subjectPublicKeyInfo is optional and does not shift them.
    """
    outer = tlv(cert, 0)
    if not outer or outer[0] != 0x30:
        raise ValueError("not a SEQUENCE")
    tbs = tlv(cert, outer[1])
    if not tbs or tbs[0] != 0x30:
        raise ValueError("no tbsCertificate")

    at = tbs[1]
    end = tbs[1] + tbs[2]
    fields_seen = []
    while at < end:
        el = tlv(cert, at)
        if not el:
            raise ValueError("truncated tbsCertificate")
        fields_seen.append((el, at))
        at += el[3]

    # [0] version, serial, sigalg, issuer, validity, subject, spki
    if len(fields_seen) < 7:
        raise ValueError("too few tbsCertificate fields")
    subject_el, subject_at = fields_seen[5]
    spki_el, spki_at = fields_seen[6]
    if subject_el[0] != 0x30 or spki_el[0] != 0x30:
        raise ValueError("subject/spki are not SEQUENCEs")

    subject = cert[subject_at:subject_at + subject_el[3]]
    spki = cert[spki_at:spki_at + spki_el[3]]
    return subject, spki, readable_name(subject)


def readable_name(subject):
    """The RDN attributes of a subject, as "C=US, O=..., CN=...".

    Used for the comment beside each entry and for --roots matching, so it has
    to be the real thing: a search over the raw bytes finds the length octet of
    one attribute and the value of the next, which is how "GlobalSign Root CA -
    R3" failed to match a certificate that says exactly that.
    """
    outer = tlv(subject, 0)
    if not outer:
        return "(unnamed)"
    parts = []
    at = outer[1]
    end = outer[1] + outer[2]
    while at < end:
        set_el = tlv(subject, at)
        if not set_el or set_el[0] != 0x31:
            break
        a = set_el[1]
        set_end = set_el[1] + set_el[2]
        while a < set_end:
            attr = tlv(subject, a)
            if not attr or attr[0] != 0x30:
                break
            oid = tlv(subject, attr[1])
            if oid:
                value = tlv(subject, oid[1] + oid[2])
                if value:
                    text = subject[value[1]:value[1] + value[2]]
                    parts.append(
                        "%s=%s" % (OID_NAMES.get(
                            subject[oid[1]:oid[1] + oid[2]].hex(), "?"),
                            text.decode("utf-8", "replace"))
                    )
            a += attr[3]
        at += set_el[3]
    return ", ".join(parts) if parts else "(unnamed)"


# Only the attributes that appear in a CA name, which is all this needs to
# print.  2.5.4.x is id-at; the rest are the legacy PKCS#9 email attributes.
OID_NAMES = {
    "550403": "CN",
    "550404": "SN",
    "550406": "C",
    "550407": "L",
    "550408": "ST",
    "550409": "street",
    "55040a": "O",
    "55040b": "OU",
    "55042a": "DC",
    "55042b": "DC",
    "2a864886f70d010901": "email",
}


def rust_bytes(name, data):
    """A &[u8] constant holding `data` as real bytes.

    Escaped one byte at a time rather than written as hex text.  Hex text is
    two bytes of kernel image for every byte of certificate, and it also has to
    be decoded at boot before it is DER -- which is a step that does not exist
    here, so what the store held was never the certificate at all.  `\\xNN` is
    one image byte per certificate byte and is DER the moment it is linked.

    A long line, deliberately: a wrapped byte string silently gains the newline
    and the indentation as bytes, and a key that is 300 bytes of DER and 312
    bytes long is one that never matches.
    """
    return 'const %s: &[u8] = b"%s";' % (
        name, "".join("\\x%02x" % b for b in data)
    )


def main():
    if len(sys.argv) < 3:
        sys.exit(__doc__)
    pem, out = sys.argv[1], sys.argv[2]
    # The list can come from the environment as well as the command line:
    # make hands a comma-separated argument through a shell, and the quoting
    # survives differently on msys than it does here.
    which = os.environ.get("TRUST_ROOTS", "all")
    if "--roots" in sys.argv:
        which = sys.argv[sys.argv.index("--roots") + 1]

    certs = load_roots(pem)
    chosen = []
    for d in certs:
        try:
            subject, spki, name = fields(d)
        except Exception:
            continue
        if which != "all":
            wanted = [w.strip().lower() for w in which.split(",") if w.strip()]
            if not any(w in name.lower() for w in wanted):
                continue
        chosen.append((subject, spki, name))

    if not chosen:
        sys.exit("gen-truststore: no roots matched %r" % which)

    lines = [
        "// Generated by scripts/gen-truststore.py -- do not edit.",
        "//",
        "// Each entry is a root's subject name and its public key, as raw DER",
        "// behind \\x escapes.  The bytes here are the bytes of the certificate.",
        "",
        "/// One trusted root: the Name to match an issuer against, and the key",
        "/// to verify the signature under it with.",
        "pub struct Root {",
        "    pub subject: &'static [u8],",
        "    pub spki: &'static [u8],",
        "}",
        "",
    ]
    for i, (subject, spki, _) in enumerate(chosen):
        lines.append(rust_bytes("SUBJ_%d" % i, subject))
        lines.append(rust_bytes("SPKI_%d" % i, spki))
    lines.append("")
    lines.append("pub const ROOTS: &[Root] = &[")
    for i, (_, _, name) in enumerate(chosen):
        lines.append('    // %s' % name[:90])
        lines.append("    Root { subject: SUBJ_%d, spki: SPKI_%d }," % (i, i))
    lines.append("];")
    lines.append("")

    text = "\n".join(lines)
    io.open(out, "w", encoding="utf-8", newline="\n").write(text)

    der = sum(len(s) + len(k) for s, k, _ in chosen)
    print("[truststore] %d root(s), %d bytes of DER, %d bytes of source"
          % (len(chosen), der, len(text)))


if __name__ == "__main__":
    main()
