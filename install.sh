#!/usr/bin/env bash
set -euo pipefail

echo "Building ALMS (release)..."
cargo build --release

BINARY="target/release/alms"
if [[ "$OSTYPE" == "msys" || "$OSTYPE" == "cygwin" || "$OSTYPE" == "win32" ]]; then
    BINARY="target/release/alms.exe"
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
