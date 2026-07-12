---
title: Install
description: "Install onote on macOS, Linux, and Windows via Homebrew, Scoop, curl|sh, .deb, or cargo — every host ships a prebuilt binary."
section: Get Started
order: 2
---

# Install

onote ships a **prebuilt binary for every primary host** — no Rust toolchain,
no compile step. Building from source needs Rust >= 1.82. Pick the path for your
platform; all of them land the same `onote` binary.

## Homebrew (macOS, recommended)

```bash
brew install alansynn/tap/onote
```

The tap installs the prebuilt darwin binary (arm64 or x86_64) in seconds and
tracks each tagged release — upgrades are equally fast.

## Scoop (Windows, recommended)

```powershell
scoop bucket add alansynn/scoop
scoop install onote
```

## One-line installer (macOS or Linux)

Downloads the matching prebuilt binary — macOS arm64 / x86_64, Linux x86_64
(static musl) and arm64 — and falls back to a from-source build only on other
hosts (`install.sh`):

```bash
curl -L https://raw.githubusercontent.com/AlanSynn/onote/main/install.sh | sh
```

### Installer options

| Option / env     | Effect                                                          |
| ---------------- | --------------------------------------------------------------- |
| `--prefix <dir>` | Install prefix (default `~/.local/bin`)                         |
| `--from-source`  | Skip the prebuilt binary and build with `cargo`                 |
| `--repo <dir>`   | Build from an existing clone instead of a temp clone            |
| `ONOTE_PREFIX`   | Env equivalent of `--prefix`                                    |
| `ONOTE_TAG`      | Release tag to install (default `v0.4.1`, the latest release)   |
| `ONOTE_REPO_URL` | Clone / download URL override                                   |

A bare `curl | sh` installs the pinned `v0.4.1` release rather than mutable
`main` HEAD. Pin an older release with `ONOTE_TAG=v0.x.y`; force a source build
with `--from-source`.

## Debian / Ubuntu (.deb)

The release ships a fully **static musl binary** wrapped in a `.deb` with no
runtime dependencies — it installs on any Debian, Ubuntu, or Mint regardless of
glibc version.

```bash
curl -L https://github.com/AlanSynn/onote/releases/latest/download/onote-x86_64-linux.deb -o /tmp/onote.deb
sudo dpkg -i /tmp/onote.deb
```

## Build from source (dev / latest)

```bash
git clone https://github.com/AlanSynn/onote.git
cd onote
cargo install --path . --locked
# or, with `just` installed:  just install
```

`just install` runs the same `cargo install --path . --locked` (`justfile`).

## Platform matrix

Prebuilt release artifacts, built in `.github/workflows/release.yml`:

| Platform                   | Target                              | Artifact                                |
| -------------------------- | ----------------------------------- | --------------------------------------- |
| macOS arm64                | `aarch64-apple-darwin`              | `onote-aarch64-apple-darwin.tar.gz`     |
| macOS x86_64               | `x86_64-apple-darwin` (cross-built) | `onote-x86_64-apple-darwin.tar.gz`      |
| Linux x86_64 (gnu)         | `x86_64-unknown-linux-gnu`          | `onote-x86_64-unknown-linux-gnu.tar.gz` |
| Linux x86_64 (static musl) | `x86_64-unknown-linux-musl`         | `onote-x86_64-unknown-linux-musl.tar.gz`|
| Linux x86_64 (.deb)        | musl, apt format, no runtime deps   | `onote-x86_64-linux.deb`                |
| Linux arm64                | `aarch64-unknown-linux-gnu`         | `onote-aarch64-unknown-linux-gnu.tar.gz`|
| Windows x86_64             | `x86_64-pc-windows-msvc`            | `onote-x86_64-pc-windows-msvc.zip`      |

## Verify

```bash
onote --version
```

> Next: [Getting Started](./getting-started.md), or the [editor](./editor.md) guide.
