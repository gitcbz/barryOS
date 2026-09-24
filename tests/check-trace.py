#!/usr/bin/env python3
"""Check a handshake trace from tests/tls-host.rs with an independent verifier.

When the kernel's TLS client refuses a server's CertificateVerify, the reason
it prints is "the server could not prove it holds the key" — which is what a
forged signature says, and also what a correct signature over the wrong bytes
says, and also what a broken verifier says.  The trace names the difference.

Four files are written per host by the driver:

    <host>-messages.bin   the handshake messages, length-prefixed, up to but
                          not including CertificateVerify
    <host>-signed.bin     the 66 bytes the signature was checked against
    <host>-sig.bin        the signature itself
    <host>-leaf.der       the end-entity certificate

The transcript hash is recomputed from the messages as RFC 8446 §4.4.1 says —
SHA-256 over the concatenation, with the context string and a zero byte in
front — and the signature is verified with `cryptography`.  If this agrees with
the client, the client is right and the server is wrong; if it disagrees, the
line it disagrees on is the bug.

Usage: tests/check-trace.py build/trace/<host>
"""

import sys
import hashlib

from cryptography import x509
from cryptography.hazmat.primitives import hashes
from cryptography.hazmat.primitives.asymmetric import ec, padding
from cryptography.exceptions import InvalidSignature

CONTEXT = b"TLS 1.3, server CertificateVerify"

SCHEMES = {
    0x0403: ("ecdsa", hashes.SHA256),
    0x0503: ("ecdsa", hashes.SHA384),
    0x0603: ("ecdsa", hashes.SHA512),
    0x0804: ("pss", hashes.SHA256),
    0x0805: ("pss", hashes.SHA384),
    0x0806: ("pss", hashes.SHA512),
    0x0401: ("pkcs1", hashes.SHA256),
    0x0501: ("pkcs1", hashes.SHA384),
    0x0601: ("pkcs1", hashes.SHA512),
}


def split_messages(blob):
    """The length-prefixed messages, as a list, plus the concatenation."""
    out = []
    at = 0
    while at + 4 <= len(blob):
        n = int.from_bytes(blob[at:at + 4], "big")
        at += 4
        out.append(blob[at:at + n])
        at += n
    return out, b"".join(out)


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    stem = sys.argv[1]

    def read(suffix):
        with open("%s-%s" % (stem, suffix), "rb") as f:
            return f.read()

    messages, transcript = split_messages(read("messages.bin"))
    signed = read("signed.bin")
    sig = read("sig.bin")
    leaf = x509.load_der_x509_certificate(read("leaf.der"))

    print("messages in the transcript:")
    names = {1: "ClientHello", 2: "ServerHello", 8: "EncryptedExtensions",
             11: "Certificate", 15: "CertificateVerify", 20: "Finished"}
    for m in messages:
        if not m:
            continue
        print("   %-20s %6d bytes" % (names.get(m[0], "type %d" % m[0]), len(m)))

    digest = hashlib.sha256(transcript).digest()
    print("\ntranscript hash  %s" % digest.hex())
    print("client signed    %s" % signed[-32:].hex())
    print("match            %s" % (signed == CONTEXT + b"\x00" + digest))

    # The scheme is the first two bytes of what the client assembled, but it is
    # not in `signed`, so it is recovered from the signature shape: PSS over a
    # 2048-bit key is 256 bytes and so is PKCS#1, and an ECDSA one is a DER
    # SEQUENCE.  Ask the driver to print it if this guess matters.
    key = leaf.public_key()
    print("\nleaf key         %s" % type(key).__name__)

    ok = False
    for scheme, (kind, alg) in sorted(SCHEMES.items()):
        if kind == "ecdsa" and not isinstance(key, ec.EllipticCurvePublicKey):
            continue
        if kind != "ecdsa" and isinstance(key, ec.EllipticCurvePublicKey):
            continue
        try:
            if kind == "ecdsa":
                key.verify(sig, signed, ec.ECDSA(alg()))
            elif kind == "pkcs1":
                key.verify(sig, signed, padding.PKCS1v15(), alg())
            else:
                key.verify(sig, signed,
                           padding.PSS(mgf=padding.MGF1(alg()),
                                       salt_length=alg().digest_size),
                           alg())
        except InvalidSignature:
            continue
        except Exception:
            continue
        print("verifies under   scheme 0x%04x (%s/%s)" % (scheme, kind, alg().name))
        ok = True
        break

    if not ok:
        print("verifies under   nothing — the signature does not match these bytes")
        print("\nSo the transcript the client built is not the one the server")
        print("signed, or the signature is not this certificate's.")
        sys.exit(1)
    print("\nThis library agrees the signature is good over these bytes.")
    sys.exit(0)


if __name__ == "__main__":
    main()
