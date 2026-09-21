# barryOS — BOOTSTRAP

Reproducible environment setup. Two paths:

## A. Rootless install (this sandbox, no sudo)
Mirrored by `scripts/env.sh`.

```bash
# 1. Rust nightly + targets + components (no sudo)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o /tmp/rustup-init.sh
sh /tmp/rustup-init.sh -y --default-toolchain nightly --profile minimal
source "$HOME/.cargo/env"
rustup component add rust-src llvm-tools-preview
rustup target add x86_64-unknown-none

# 2. NASM from source → ~/.local/bin (no sudo)
curl -sL https://www.nasm.us/pub/nasm/releasebuilds/2.16.01/nasm-2.16.01.tar.xz | tar -xJ
cd nasm-2.16.01 && ./configure --prefix="$HOME/.local" && make -j"$(nproc)" && make install
cd ..

# 3. QEMU / xorriso / mtools / OVMF via apt .deb extraction (no sudo)
PREFIX="$HOME/.opt"; mkdir -p "$PREFIX"
cd /tmp && apt-get download \
  qemu-system-x86 qemu-system-common qemu-system-data seabios ipxe-qemu \
  xorriso mtools ovmf \
  libisoburn1t64 libisofs6t64 libjte2 libburn4t64 \
  $(apt-cache depends --recurse --no-recommends --no-suggests \
       --no-conflicts --no-breaks --no-replaces --no-enhances qemu-system-x86 \
     | grep -E '^(lib|seabios|ipxe)' | sort -u)
for d in *.deb; do dpkg-deb -x "$d" "$PREFIX/"; done

# 4. Activate
source /home/z/my-project/barryOS/scripts/env.sh
qemu-system-x86_64 --version
xorriso --version
nasm --version
```

## B. CI / root container (Dockerfile) — see ci/Dockerfile
```bash
docker build -t barryos-ci ci/
docker run --rm -v "$PWD":/work -w /work barryos-ci bash -lc 'source scripts/env.sh && make all && bash scripts/check.sh'
```

## Installed versions (reference)
| Tool   | Version          | Source            |
|--------|------------------|-------------------|
| rustc  | 1.100.0-nightly  | rustup            |
| nasm   | 2.16.01          | source → ~/.local |
| qemu   | 10.0.13          | apt .deb extract  |
| xorriso| 1.5.6            | apt .deb extract  |
| mtools | 4.0.48           | apt .deb extract  |
| OVMF   | 2025.02          | apt .deb extract  |
| gcc    | 14.2.0           | system            |
| ld     | 2.44             | system            |
