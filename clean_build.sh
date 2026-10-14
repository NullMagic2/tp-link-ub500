#!/usr/bin/env bash
# Module: magic_usb.clean_build
# Purpose: Remove build intermediates and retain deliverable binaries.
# Created: 2026-10-13
# Architecture: Only the project's target directory is removed.
set -euo pipefail
cd -- "$(dirname -- "$(realpath -- "$0")")"
cargo clean
