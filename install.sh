#!/usr/bin/env bash
set -euo pipefail

usage() {
    cat <<'EOF'
Usage: ./install.sh [-h | --help]

Release-builds ALMS and installs the alms binary into $CARGO_HOME/bin
(~/.cargo/bin when CARGO_HOME is unset). Honours CARGO_TARGET_DIR.
EOF
}

if [[ $# -gt 0 ]]; then
    case "$1" in
        -h | --help)
            usage
            exit 0
            ;;
        *)
            echo "ERROR: unknown argument: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
fi

# Build and install paths below are relative, so run from the repo root no
# matter where the script was invoked from, or through which symlink. A loop
# because `readlink -f` only reached macOS in 12.3. A relative link target is
# relative to the link's directory, and `cd -P` resolves any `..` in it
# physically, as the kernel does. SCRIPT_DIR is its own assignment so that a
# failing `dirname` stops the script (`set -e`) instead of `cd ""` staying put.
SCRIPT="$0"
while [[ -L "$SCRIPT" ]]; do
    LINK="$(readlink -- "$SCRIPT")"
    case "$LINK" in
        /*) SCRIPT="$LINK" ;;
        *) SCRIPT="$(dirname -- "$SCRIPT")/$LINK" ;;
    esac
done
SCRIPT_DIR="$(dirname -- "$SCRIPT")"
cd -P -- "$SCRIPT_DIR" || exit 1

if ! command -v cargo >/dev/null 2>&1; then
    echo "ERROR: cargo not found on PATH. Install Rust from https://rustup.rs" >&2
    echo "With rustup, the pinned nightly in rust-toolchain.toml installs itself on first build." >&2
    exit 1
fi

# rust-toolchain.toml is honoured by rustup's cargo shim, not by cargo itself.
# A cargo from a package manager (brew, apt) passes the check above and then
# builds with whatever compiler it shipped with.
if ! command -v rustup >/dev/null 2>&1; then
    echo "WARNING: rustup not found, so the nightly pinned in rust-toolchain.toml is" >&2
    echo "ignored and cargo builds with its own compiler. If the build fails, install" >&2
    echo "Rust from https://rustup.rs instead." >&2
fi

echo "Building ALMS (release)..."
cargo build --release

TARGET_DIR="${CARGO_TARGET_DIR:-target}"
BINARY="$TARGET_DIR/release/alms"
if [[ "$OSTYPE" == "msys" || "$OSTYPE" == "cygwin" || "$OSTYPE" == "win32" ]]; then
    BINARY="$TARGET_DIR/release/alms.exe"
fi

if [[ ! -f "$BINARY" ]]; then
    echo "ERROR: Build succeeded but binary not found at $BINARY" >&2
    exit 1
fi

# Cargo's bin directory, which is the one rustup puts on PATH.
INSTALL_DIR="${CARGO_HOME:-$HOME/.cargo}/bin"
NAME="${BINARY##*/}"
DEST="$INSTALL_DIR/$NAME"
mkdir -p "$INSTALL_DIR"

# Copy beside the target, then rename over it. A plain `cp` onto a binary that a
# running gateway was started from fails on Linux ("Text file busy"); on macOS it
# succeeds but rewrites that inode in place, and the kernel then kills launches
# of it ("Killed: 9"). A rename swaps the directory entry instead: the running
# process keeps the old file and new launches get the new one.
TMP="$INSTALL_DIR/.$NAME.install.$$"
trap 'rm -f -- "$TMP"' EXIT
cp -- "$BINARY" "$TMP"
chmod 755 "$TMP"
if ! mv -f -- "$TMP" "$DEST"; then
    echo "ERROR: could not replace $DEST. On Windows a running alms locks its" >&2
    echo "executable; stop it and run ./install.sh again." >&2
    exit 1
fi

# Run the installed copy, not the build output: this is the check that the file
# now at $DEST works.
if ! VERSION="$("$DEST" --version)"; then
    echo "ERROR: installed $DEST, but running it with --version failed." >&2
    exit 1
fi
echo "Installed $VERSION to $DEST"

# Say which alms a new shell will actually run. A different one earlier on PATH
# shadows this install, and nothing else would tell you.
ON_PATH="$(command -v alms || true)"
if [[ -z "$ON_PATH" ]]; then
    echo "WARNING: $INSTALL_DIR is not on your PATH, so \`alms\` will not be found." >&2
    echo "Add it with: export PATH=\"$INSTALL_DIR:\$PATH\"" >&2
elif [[ ! "$ON_PATH" -ef "$DEST" ]]; then
    echo "WARNING: \`alms\` on your PATH is $ON_PATH, not the binary just installed." >&2
    echo "Remove that one, or put $INSTALL_DIR ahead of it in PATH." >&2
else
    echo "\`alms\` on your PATH is the binary just installed."
fi
