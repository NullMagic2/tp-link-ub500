#!/usr/bin/env bash
# Module: magic_usb.build_script
# Purpose: Test and build both userspace clients.
# Created: 2026-10-13
# Architecture: Builds against distro libusb and libzstd, no kernel module.
set -euo pipefail
cd -- "$(dirname -- "$(realpath -- "$0")")"
cargo fmt --all -- --check
cargo test --workspace --locked --offline
cargo build --release --workspace --locked --offline
mkdir -p bin
install -m 0755 target/release/magic-usb bin/magic-usb
install -m 0755 target/release/csr8510 bin/csr8510
