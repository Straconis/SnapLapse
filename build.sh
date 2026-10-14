#!/bin/sh
# Builds SnapLapse inside a Podman container, so nothing needs installing on this machine.
#
#   ./build.sh          Build Linux and Windows release versions into dist/
#   ./build.sh test     Run the automated tests and code checks
#   ./build.sh linux    Build only the Linux version
#   ./build.sh windows  Build only the Windows version and its installer
set -e
cd "$(dirname "$0")"

IMAGE=localhost/snaplapse-build:latest
podman build -q -t "$IMAGE" -f container/Containerfile container >/dev/null

in_container() {
    podman run --rm \
        -v "$PWD":/src:Z \
        -v snaplapse-cargo-registry:/usr/local/cargo/registry \
        -v snaplapse-cargo-git:/usr/local/cargo/git \
        -w /src "$IMAGE" sh -c "$1"
}

build_linux() {
    in_container "cargo build --release --locked"
    mkdir -p dist
    cp target/release/snaplapse dist/snaplapse-linux-x86_64
    echo "Built dist/snaplapse-linux-x86_64"
}

build_windows() {
    in_container "cargo build --release --locked --target x86_64-pc-windows-gnu"
    mkdir -p dist
    cp target/x86_64-pc-windows-gnu/release/snaplapse.exe dist/SnapLapse.exe
    version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
    in_container "makensis -V2 -DVERSION=$version installer/snaplapse.nsi"
    echo "Built dist/SnapLapse.exe and dist/SnapLapse-$version-setup.exe"
}

case "${1:-all}" in
    test)
        in_container "cargo fmt --check && cargo clippy --all-targets --locked -- -D warnings \
            && cargo clippy --all-targets --locked --target x86_64-pc-windows-gnu -- -D warnings \
            && cargo test --locked"
        ;;
    linux) build_linux ;;
    windows) build_windows ;;
    all) build_linux; build_windows ;;
    *) echo "Unknown command: $1" >&2; exit 2 ;;
esac
