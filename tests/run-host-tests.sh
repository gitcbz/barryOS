#!/usr/bin/env bash
# Everything that can be verified on the host, with no kernel toolchain and no
# boot: the page renderer, the image decoders, and the TLS client.
#
# These three suites exist because the parts they cover cannot be checked from
# the outside.  A page that renders slightly wrong, a picture that decodes to
# nearly the right colours and a handshake that succeeds against the wrong
# server all look like working software from a screenshot, and all three are
# cheaper to catch in a second here than in a VM boot.
#
#     tests/run-host-tests.sh
#
# Pillow is needed by the image checker and nothing else; without it that one
# suite runs the lossless cases alone and says so.
set -u

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

pass=0
fail=0
failed=""

run() {
    name="$1"
    shift
    printf '%-28s ' "$name"
    if out=$("$@" 2>&1); then
        pass=$((pass + 1))
        echo "ok"
    else
        fail=$((fail + 1))
        failed="$failed $name"
        echo "FAILED"
        echo "$out" | sed 's/^/    /'
    fi
}

# The page renderer, over its own cases and over a page whose answer is known.
run "page renderer" bash tests/run-web-host.sh
run "page renderer self-test" env SELFTEST=1 bash tests/run-web-host.sh

# The image decoders, against a decoder nobody here wrote.
if bash tests/run-img-host.sh >/dev/null 2>&1; then
    if python3 -c "import PIL" 2>/dev/null; then
        run "image decoders" python3 tests/check-images.py
    else
        printf '%-28s ' "image decoders"
        if python3 tests/check-images.py --no-pillow | tail -3 | grep -q "SOME DISAGREE"; then
            fail=$((fail + 1))
            failed="$failed images"
            echo "FAILED"
        else
            pass=$((pass + 1))
            echo "ok (no Pillow: lossless cases only)"
        fi
    fi
else
    printf '%-28s FAILED\n' "image decoders"
    fail=$((fail + 1))
    failed="$failed images"
fi

# The TLS client, against its own vectors.  A real handshake is skipped: this
# is a test of the code, and the network is tested by booting.
if [ -f build/truststore.rs ]; then
    run "TLS vectors" bash tests/run-tls-host.sh --vectors
else
    printf '%-28s skipped (no build/truststore.rs; run make truststore)\n' "TLS vectors"
fi

echo
if [ "$fail" = 0 ]; then
    echo "$pass suite(s) passed"
    exit 0
fi
echo "$pass passed, $fail failed:$failed"
exit 1
