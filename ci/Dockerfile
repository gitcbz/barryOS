# barryOS — reproducible CI container (root inside)
FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
    nasm qemu-system-x86 ovmf xorriso mtools \
    build-essential clang lld llvm \
    xxd file curl ca-certificates git vim less \
    && rm -rf /var/lib/apt/lists/*

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o /tmp/rustup.sh \
    && sh /tmp/rustup.sh -y --default-toolchain nightly --profile minimal \
    && rm /tmp/rustup.sh

ENV PATH="/root/.cargo/bin:${PATH}"

RUN rustup component add rust-src llvm-tools-preview \
    && rustup target add x86_64-unknown-none

WORKDIR /work
# source scripts/env.sh or rely on system-wide installs here
CMD ["/bin/bash"]
