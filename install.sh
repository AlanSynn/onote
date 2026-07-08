#!/usr/bin/env sh
# onote installer — build from source and install the optimized release binary.
#
# Usage:
#   curl -L https://raw.githubusercontent.com/AlanSynn/onote/main/install.sh | sh
#   curl -L https://raw.githubusercontent.com/AlanSynn/onote/main/install.sh | sh -s -- --prefix ~/.local/bin
#   sh install.sh                 # from a clone
#   sh install.sh --prefix /usr/local/bin
#
# Builds the release binary with the toolchain in `rustup`, then copies it to a
# prefix on PATH (default ~/.local/bin, following the freedesktop convention).
# set -eu aborts on any failure so a half install never happens.
set -eu

PREFIX="${ONOTE_PREFIX:-$HOME/.local/bin}"
REPO_DIR=""

# Default to the latest release tag so a bare `curl | sh` builds an audited,
# pinned release instead of mutable `main` HEAD (supply-chain hardening).
# Bump this in lockstep with each `git tag v0.x.y` and the Release workflow.
ONOTE_TAG="${ONOTE_TAG:-v0.2.0}"

usage() {
    cat <<EOF
onote installer

Options:
  --prefix <dir>   Install prefix (default: \$ONOTE_PREFIX or ~/.local/bin)
  --repo <dir>     Build from an existing clone instead of a temp git clone
  -h, --help       Show this help

Environment:
  ONOTE_PREFIX     Same as --prefix
  ONOTE_REPO_URL   Clone URL override (default: github.com/AlanSynn/onote.git)
  ONOTE_TAG        Release tag to build (default: v0.2.0, the latest release)
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --prefix)
            [ $# -ge 2 ] || { printf 'onote: --prefix requires a value\n' >&2; exit 2; }
            PREFIX="$2"; shift 2 ;;
        --repo)
            [ $# -ge 2 ] || { printf 'onote: --repo requires a value\n' >&2; exit 2; }
            REPO_DIR="$2"; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) printf 'onote: unknown option: %s\n' "$1" >&2; usage >&2; exit 2 ;;
    esac
done

# ── Preflight ────────────────────────────────────────────────────────────
need() {
    command -v "$1" >/dev/null 2>&1 || {
        printf 'onote: missing required command: %s\n' "$1" >&2
        printf '  install it first, then re-run this script.\n' >&2
        exit 1
    }
}
need cargo    # a Rust toolchain (preferably via rustup)
need install  # coreutils

# ── Obtain source ────────────────────────────────────────────────────────
WORK_DIR=""
cleanup() {
    # Idempotent: a signal-induced exit re-runs the EXIT trap, so clear
    # WORK_DIR after the rm so a second call is a no-op (defense against the
    # EXIT/INT/TERM double-fire).
    [ -z "$WORK_DIR" ] || { rm -rf "$WORK_DIR"; WORK_DIR=""; }
}
trap cleanup EXIT INT TERM

if [ -n "$REPO_DIR" ]; then
    SRC_DIR="$REPO_DIR"
elif [ -d ".git" ] && [ -f "Cargo.toml" ] && grep -q '^name = "onote"' Cargo.toml 2>/dev/null; then
    # Already inside an onote checkout.
    SRC_DIR="$(pwd)"
else
    # No hosted release tarball yet — clone from source. Edit REPO_URL when
    # publishing. `ONOTE_TAG` pins the clone to a specific release tag for a
    # reproducible, auditable build (supply-chain hardening) instead of whatever
    # is at HEAD of the default branch.
    REPO_URL="${ONOTE_REPO_URL:-https://github.com/AlanSynn/onote.git}"
    need git
    WORK_DIR="$(mktemp -d 2>/dev/null || mktemp -d -t onote)"
    if [ -n "${ONOTE_TAG:-}" ]; then
        printf 'cloning %s @ %s ...\n' "$REPO_URL" "$ONOTE_TAG" >&2
        git clone --depth 1 --branch "$ONOTE_TAG" "$REPO_URL" "$WORK_DIR/onote" >&2
    else
        printf 'cloning %s ...\n' "$REPO_URL" >&2
        git clone --depth 1 "$REPO_URL" "$WORK_DIR/onote" >&2
    fi
    SRC_DIR="$WORK_DIR/onote"
fi

# ── Build release ────────────────────────────────────────────────────────
printf 'building release (LTO + strip; first build is slow) ...\n'
( cd "$SRC_DIR" && cargo build --release --locked ) >&2

BIN="$SRC_DIR/target/release/onote"
[ -f "$BIN" ] || { printf 'onote: release binary not found after build\n' >&2; exit 1; }

# ── Install ──────────────────────────────────────────────────────────────
mkdir -p "$PREFIX"
# Atomic, symlink-proof install: write to a fresh mktemp name in $PREFIX, then
# rename(2) into place. rename replaces the destination directory entry without
# following a symlink planted at $PREFIX/onote, closing the rm->install race.
NEW="$(mktemp "$PREFIX/.onote.new.XXXXXX")" || exit 1
install -m 0755 "$BIN" "$NEW" || { rm -f "$NEW"; exit 1; }
mv -f "$NEW" "$PREFIX/onote"

VERSION="$("$PREFIX/onote" --version 2>/dev/null || echo onote)"
# ASCII glyphs ([ok], ->): non-UTF-8 / C-locale shells render these reliably
# where a checkmark or arrow would show as "?" or mojibake.
printf '[ok] installed %s -> %s/onote\n' "$VERSION" "$PREFIX"

case ":$PATH:" in
    *":$PREFIX:"*) ;;  # already on PATH
    *)
        printf '\n' >&2
        printf 'NOTE: %s is not on your PATH.\n' "$PREFIX" >&2
        printf 'Add this to your shell profile (~/.zshrc or ~/.bashrc):\n' >&2
        printf '  export PATH="%s:$PATH"\n' "$PREFIX" >&2
        ;;
esac

cat <<EOF

Next: point onote at your Obsidian vault. Create ~/.config/onote/config.toml:

    vault = "$HOME/Notes/Vault"

then run:  $PREFIX/onote
EOF
