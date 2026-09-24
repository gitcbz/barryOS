#!/usr/bin/env python3
"""Issue a CA and a server certificate for the local TLS test.

Written out as `build/tls-test/ca.pem` and `build/tls-test/server.pem`.  The CA
is what gets added to the trust store for the duration of the test, so the run
exercises chain building and the trust store too, and not just the handshake.

Usage: tests/gen-test-certs.py <outdir> [hostname]
"""

import datetime
import ipaddress
import os
import sys

from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import ec
from cryptography.x509.oid import NameOID


def issue(subject_cn, issuer, issuer_key, key, *, ca, host=None, days=30):
    now = datetime.datetime.now(datetime.timezone.utc)
    name = x509.Name([
        x509.NameAttribute(NameOID.ORGANIZATION_NAME, "barryOS test"),
        x509.NameAttribute(NameOID.COMMON_NAME, subject_cn),
    ])
    b = (x509.CertificateBuilder()
         .subject_name(name)
         .issuer_name(issuer)
         .public_key(key.public_key())
         .serial_number(x509.random_serial_number())
         .not_valid_before(now - datetime.timedelta(minutes=5))
         .not_valid_after(now + datetime.timedelta(days=days))
         .add_extension(x509.BasicConstraints(ca=ca, path_length=None),
                        critical=True))
    if host:
        b = b.add_extension(
            x509.SubjectAlternativeName([x509.DNSName(host),
                                         x509.IPAddress(ipaddress.ip_address("127.0.0.1"))]),
            critical=False)
    return b.sign(issuer_key, hashes.SHA256())


def main():
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    out = sys.argv[1]
    host = sys.argv[2] if len(sys.argv) > 2 else "localhost"
    os.makedirs(out, exist_ok=True)

    ca_key = ec.generate_private_key(ec.SECP256R1())
    ca_name = x509.Name([
        x509.NameAttribute(NameOID.ORGANIZATION_NAME, "barryOS test"),
        x509.NameAttribute(NameOID.COMMON_NAME, "barryOS test CA"),
    ])
    ca = issue("barryOS test CA", ca_name, ca_key, ca_key, ca=True)

    leaf_key = ec.generate_private_key(ec.SECP256R1())
    leaf = issue(host, ca.subject, ca_key, leaf_key, ca=False, host=host)

    def write(path, data, private=False):
        with open(path, "wb") as f:
            if private:
                f.write(data.private_bytes(
                    serialization.Encoding.PEM,
                    serialization.PrivateFormat.PKCS8,
                    serialization.NoEncryption()))
            else:
                f.write(data.public_bytes(serialization.Encoding.PEM))

    write(os.path.join(out, "ca.pem"), ca, private=False)
    write(os.path.join(out, "ca-key.pem"), ca_key, private=True)
    write(os.path.join(out, "server.pem"), leaf, private=False)
    write(os.path.join(out, "server-key.pem"), leaf_key, private=True)
    print("[certs] issued a CA and a certificate for %s in %s" % (host, out))


if __name__ == "__main__":
    main()
