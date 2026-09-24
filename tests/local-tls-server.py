#!/usr/bin/env python3
"""A local TLS 1.3 server, for testing the kernel's client against.

The problem with testing a TLS client against the internet is that "it did not
work" has at least three explanations — the client is wrong, the server is
unusual, or something between them is rewriting traffic — and no amount of
staring at the client separates them.  This runs a server whose certificate the
test issues itself, on the loopback interface, so both ends are known:

  * if the handshake completes, the client's transcript, key schedule, record
    layer and signature checks are all right, and any failure against a real
    server is that server or the path to it;
  * if it does not, OpenSSL says which part of it failed, on this side, with
    the certificate and the messages in hand.

Usage: tests/local-tls-server.py <cert.pem> <key.pem> <port>
"""

import socket
import ssl
import sys
import threading

BODY = (b"<!doctype html><html><head><title>barryOS test</title></head>"
        b"<body><h1>It works</h1><p>Served over TLS 1.3 on the loopback "
        b"interface.</p></body></html>")
RESPONSE = (b"HTTP/1.1 200 OK\r\n"
            b"Content-Type: text/html; charset=utf-8\r\n"
            b"Content-Length: " + str(len(BODY)).encode() + b"\r\n"
            b"Connection: close\r\n\r\n" + BODY)


def peek_client_hello(conn, path):
    """Read the ClientHello off the socket without consuming it.

    The point of the local server is that both ends are knowable, and the
    ClientHello is the one message the transcript starts with.  If the client's
    idea of what it sent and the server's idea of what it received differ by a
    byte, every signature check downstream fails with nothing to say why —
    so the bytes are captured here, from the socket, before OpenSSL sees them.

    MSG_PEEK leaves the data queued, so the TLS stack still gets its handshake.
    """
    buf = b""
    while len(buf) < 5:
        buf += conn.recv(5 - len(buf), socket.MSG_PEEK)
        if not buf:
            return b""
    need = 5 + int.from_bytes(buf[3:5], "big")
    while len(buf) < need:
        chunk = conn.recv(need - len(buf), socket.MSG_PEEK)
        if not chunk:
            break
        buf += chunk
    with open(path, "wb") as f:
        f.write(buf)
    return buf


def serve_one(ctx, conn_in, conn_out, tag):
    """One connection, with the handshake errors reported rather than raised."""
    try:
        with conn_in:
            conn_in.settimeout(20)
            hello = peek_client_hello(conn_in, "build/trace/local-client-hello.bin")
            print("[local] %s: ClientHello %d bytes" % (tag, len(hello)), flush=True)
            conn = ctx.wrap_socket(conn_in, server_side=True)
            print("[local] %s: %s" % (tag, conn.version()), flush=True)
            conn.settimeout(20)
            req = b""
            while b"\r\n\r\n" not in req:
                chunk = conn.recv(4096)
                if not chunk:
                    break
                req += chunk
            first = req.split(b"\r\n", 1)[0].decode("latin-1")
            print("[local] %s: %s" % (tag, first), flush=True)
            conn.sendall(RESPONSE)
            print("[local] %s: served %d bytes" % (tag, len(RESPONSE)), flush=True)
            try:
                conn.unwrap()
            except Exception:
                pass
    except ssl.SSLError as e:
        print("[local] %s: TLS error: %s" % (tag, e), flush=True)
    except Exception as e:
        print("[local] %s: %s: %s" % (tag, type(e).__name__, e), flush=True)


def main():
    if len(sys.argv) != 4:
        sys.exit(__doc__)
    cert, key, port = sys.argv[1], sys.argv[2], int(sys.argv[3])

    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    ctx.minimum_version = ssl.TLSVersion.TLSv1_2
    ctx.maximum_version = ssl.TLSVersion.TLSv1_3
    ctx.load_cert_chain(cert, key)
    # Same suites the kernel offers, so a handshake that gets this far is
    # testing the kernel's code and not OpenSSL's willingness to compromise.
    ctx.set_ciphers("ECDHE+AESGCM:ECDHE+CHACHA20")

    srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    # All interfaces, not just loopback: booting the kernel under a hypervisor
    # means the client is on another address, and a listener that only accepts
    # from itself would say "nothing arrived" for both a working stack and a
    # broken one.
    srv.bind(("0.0.0.0", port))
    srv.listen(4)
    print("[local] listening on 0.0.0.0:%d" % port, flush=True)

    n = 0
    while True:
        conn, addr = srv.accept()
        n += 1
        t = threading.Thread(target=serve_one,
                             args=(ctx, conn, conn, "#%d" % n), daemon=True)
        t.start()


if __name__ == "__main__":
    main()
