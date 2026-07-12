#!/usr/bin/env sh
# onote installer — prebuilt STATIC binary where one exists, else build source.
#
# Usage:
#   curl -L https://raw.githubusercontent.com/AlanSynn/onote/main/install.sh | sh
#   curl -L https://raw.githubusercontent.com/AlanSynn/onote/main/install.sh | sh -s -- --prefix ~/.local/bin
#   sh install.sh                 # from a clone
#   sh install.sh --from-source   # force a local cargo build (skip prebuilt)
#
# Fast path: every primary host has a matching prebuilt release binary —
# macOS arm64/x86_64, Linux x86_64 (static musl, no glibc dep), Linux arm64.
# This script downloads + installs the right one directly, skipping the ~2 min
# cargo build. Any other platform (and any download failure) falls back to
# `cargo build --release` from source.
# set -eu aborts on any failure so a half install never happens.
set -eu

PREFIX="${ONOTE_PREFIX:-$HOME/.local/bin}"
REPO_DIR=""
FORCE_SOURCE=0

# Default to the latest release tag so a bare `curl | sh` installs an audited,
# pinned release instead of mutable `main` HEAD (supply-chain hardening).
# Bump this in lockstep with each `git tag v0.x.y` and the Release workflow.
ONOTE_TAG="${ONOTE_TAG:-v0.4.1}"

usage() {
    cat <<EOF
onote installer

Options:
  --prefix <dir>   Install prefix (default: \$ONOTE_PREFIX or ~/.local/bin)
  --repo <dir>     Build from an existing clone instead of a temp git clone
  --from-source    Skip the prebuilt binary and build with cargo
  -h, --help       Show this help

Environment:
  ONOTE_PREFIX     Same as --prefix
  ONOTE_REPO_URL   Clone/download URL override (default: github.com/AlanSynn/onote)
  ONOTE_TAG        Release tag to install/build (default: v0.4.1, the latest release)
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
        --from-source) FORCE_SOURCE=1; shift ;;
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
need install  # coreutils — used by both the prebuilt and source paths

WORK_DIR=""
cleanup() {
    # Idempotent: a signal-induced exit re-runs the EXIT trap, so clear
    # WORK_DIR after the rm so a second call is a no-op (defense against the
    # EXIT/INT/TERM double-fire).
    [ -z "$WORK_DIR" ] || { rm -rf "$WORK_DIR"; WORK_DIR=""; }
}
trap cleanup EXIT INT TERM

# ── Helpers ──────────────────────────────────────────────────────────────
# http_get <url> <output-file> — prefer curl, fall back to wget.
http_get() {
    if command -v curl >/dev/null 2>&1; then
        curl -fL "$1" -o "$2"
    elif command -v wget >/dev/null 2>&1; then
        wget -O "$2" "$1"
    else
        return 1
    fi
}

warn_path() {
    case ":$PATH:" in
        *":$PREFIX:"*) ;;  # already on PATH
        *)
            printf '\n' >&2
            printf 'NOTE: %s is not on your PATH.\n' "$PREFIX" >&2
            printf 'Add this to your shell profile (~/.zshrc or ~/.bashrc):\n' >&2
            # $PATH is literal on purpose: the user's shell expands it when
            # the profile is sourced.
            # shellcheck disable=SC2016
            printf '  export PATH="%s:$PATH"\n' "$PREFIX" >&2
            ;;
    esac
}

print_next() {
    cat <<EOF

Next: point onote at your Obsidian vault. Create ~/.config/onote/config.toml:

    vault = "$HOME/Notes/Vault"

then run:  $PREFIX/onote
EOF
}

# install_binary <path> — atomic, symlink-proof install (rename into place),
# then report version + PATH note + next steps. Fatal errors exit; success
# returns so the caller controls flow.
install_binary() {
    BIN="$1"
    [ -f "$BIN" ] || { printf 'onote: release binary not found\n' >&2; exit 1; }
    mkdir -p "$PREFIX"
    # Write to a fresh mktemp name in $PREFIX, then rename(2) into place. rename
    # replaces the destination directory entry without following a symlink
    # planted at $PREFIX/onote, closing the rm->install race.
    NEW="$(mktemp "$PREFIX/.onote.new.XXXXXX")" || exit 1
    install -m 0755 "$BIN" "$NEW" || { rm -f "$NEW"; exit 1; }
    mv -f "$NEW" "$PREFIX/onote"

    VERSION="$("$PREFIX/onote" --version 2>/dev/null || echo onote)"
    # ASCII glyphs ([ok], ->): non-UTF-8 / C-locale shells render these reliably
    # where a checkmark or arrow would show as "?" or mojibake.
    printf '[ok] installed %s -> %s/onote\n' "$VERSION" "$PREFIX"
    warn_path
    print_next
}

REPO_URL="${ONOTE_REPO_URL:-https://github.com/AlanSynn/onote.git}"
DOWNLOAD_BASE="${REPO_URL%.git}"  # strip trailing .git for the releases/ URL

# ── Prebuilt fast path ───────────────────────────────────────────────────
# Returns 0 and installs on success; returns 1 to let the caller fall back to a
# source build. Maps the host `(uname -s, uname -m)` to a release asset; any
# host without a matching prebuilt (or `--from-source`, or a download failure)
# returns 1 → source fallback.
try_prebuilt() {
    [ "$FORCE_SOURCE" = 1 ] && return 1
    command -v curl >/dev/null 2>&1 || command -v wget >/dev/null 2>&1 || return 1
    command -v tar >/dev/null 2>&1 || return 1

    # Resolve host → release asset. macOS arm64 reports `arm64`; Linux arm64
    # reports `aarch64`. Linux x86_64 uses the static musl build (no glibc dep).
    OS="$(uname -s)"
    MACH="$(uname -m)"
    ASSET=""
    case "$OS:$MACH" in
        Darwin:arm64)   ASSET="onote-aarch64-apple-darwin.tar.gz" ;;
        Darwin:x86_64)  ASSET="onote-x86_64-apple-darwin.tar.gz" ;;
        Linux:x86_64)   ASSET="onote-x86_64-unknown-linux-musl.tar.gz" ;;
        Linux:aarch64)  ASSET="onote-aarch64-unknown-linux-gnu.tar.gz" ;;
        *) return 1 ;;  # no prebuilt for this host → source fallback
    esac

    URL="${DOWNLOAD_BASE}/releases/download/${ONOTE_TAG}/${ASSET}"
    WORK_DIR="$(mktemp -d 2>/dev/null || mktemp -d -t onote)"
    printf 'downloading prebuilt binary %s ...\n' "$URL" >&2
    if ! http_get "$URL" "$WORK_DIR/onote.tar.gz"; then
        printf 'download failed; falling back to build from source.\n' >&2
        rm -rf "$WORK_DIR"; WORK_DIR=""; return 1
    fi
    tar -xzf "$WORK_DIR/onote.tar.gz" -C "$WORK_DIR" >&2

    # The tarball lays out as ./onote [./install.sh]; locate the binary.
    BIN=""
    for cand in "$WORK_DIR/onote" "$WORK_DIR"/*/onote; do
        [ -f "$cand" ] && BIN="$cand" && break
    done
    if [ -z "$BIN" ]; then
        printf 'onote: binary not found in prebuilt archive; falling back to source.\n' >&2
        rm -rf "$WORK_DIR"; WORK_DIR=""; return 1
    fi
    install_binary "$BIN"
}

# ── Source path ──────────────────────────────────────────────────────────
build_from_source() {
    need cargo    # a Rust toolchain (preferably via rustup)

    if [ -n "$REPO_DIR" ]; then
        SRC_DIR="$REPO_DIR"
    elif [ -d ".git" ] && [ -f "Cargo.toml" ] && grep -q '^name = "onote"' Cargo.toml 2>/dev/null; then
        # Already inside an onote checkout.
        SRC_DIR="$(pwd)"
    else
        # Clone from source. `ONOTE_TAG` pins the clone to a specific release
        # tag for a reproducible, auditable build instead of HEAD of main.
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

    printf 'building release (LTO + strip; first build is slow) ...\n'
    ( cd "$SRC_DIR" && cargo build --release --locked ) >&2
    install_binary "$SRC_DIR/target/release/onote"
}

# ── Main ─────────────────────────────────────────────────────────────────
if try_prebuilt; then
    :  # installed via prebuilt binary
else
    build_from_source
fi
