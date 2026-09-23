#!/usr/bin/env bash
set -euo pipefail

# Build and install paths below are relative, so run from the repo root no
# matter where the script was invoked from.
cd "$(dirname "$0")"

if ! command -v cargo >/dev/null 2>&1; then
    echo "ERROR: cargo not found on PATH. Install Rust from https://rustup.rs"
    echo "The pinned nightly in rust-toolchain.toml installs itself on first build."
    exit 1
fi

echo "Building ALMS (release)..."
cargo build --release

TARGET_DIR="${CARGO_TARGET_DIR:-target}"
BINARY="$TARGET_DIR/release/alms"
if [[ "$OSTYPE" == "msys" || "$OSTYPE" == "cygwin" || "$OSTYPE" == "win32" ]]; then
    BINARY="$TARGET_DIR/release/alms.exe"
fi

if [[ ! -f "$BINARY" ]]; then
    echo "ERROR: Build succeeded but binary not found at $BINARY"
    exit 1
fi

INSTALL_DIR="$HOME/.cargo/bin"
mkdir -p "$INSTALL_DIR"
cp "$BINARY" "$INSTALL_DIR/"

echo "Installed alms to $INSTALL_DIR/alms"
echo "Make sure $INSTALL_DIR is in your PATH."
