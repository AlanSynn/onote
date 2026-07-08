# onote — task recipes (https://github.com/casey/just)
#
# `just` is the one-command entry point for every dev/DX action: build, test,
# lint, format, release, install, docs. Install `just` once (`brew install
# just` / `cargo install just`) then `just <recipe>`. Run `just` bare to list.

# Default: list available recipes (no-op is safer than a surprise build).
default:
    @just --list --unsorted

# ── Dev loop ──────────────────────────────────────────────────────────────

# Fast type-check without codegen (quickest signal during editing).
check:
    cargo check

# `cargo build` (debug).
build:
    cargo build

# Run onote, forwarding any args: `just run scratch`, `just run -- open robot`.
run *ARGS:
    cargo run -- {{ARGS}}

# Run the full test suite (unit + integration). `--all` mirrors CI.
test:
    cargo test --all

# Run only tests matching a filter: `just test-one vault`.
test-one FILTER:
    cargo test {{FILTER}}

# Open the TUI against the real configured vault (uses your ~/.config/onote).
go:
    cargo run

# ── Quality gate ──────────────────────────────────────────────────────────

# Clippy with warnings as errors (the project's gate).
clippy:
    cargo clippy --all-targets -- -D warnings

# Apply rustfmt.
fmt:
    cargo fmt

# Verify formatting without writing (CI parity). `--all` mirrors CI.
fmt-check:
    cargo fmt --all -- --check

# Lint = format check + clippy (order matches `ci` and the CI workflow).
lint: fmt-check clippy

# The full local CI gate: format + clippy + tests. Mirrors .github/workflows/ci.yml.
ci: fmt-check clippy test
    @echo "✓ CI gate passed (fmt + clippy + test)"

# ── Release & install ────────────────────────────────────────────────────

# Optimized release build (LTO + strip, per Cargo.toml [profile.release]).
release:
    cargo build --release

# Path of the release binary.
release-bin := "target/release/onote"

# Print where the release binary will land (useful for packaging).
release-path:
    @echo "{{release-bin}}"

# Install onote into ~/.cargo/bin via `cargo install` (dev install).
install:
    cargo install --path . --locked

# Install the optimized release binary into a prefix (default $HOME/.local/bin),
# creating the dir if needed. Override with an ABSOLUTE path:
#   just install-binary /usr/local
# The default uses `$HOME` (not `~`): `~` does NOT expand inside the double-
# quoted `install` target, so a `~`-bearing default would land the binary at a
# literal `~/...` path and fail. `$HOME` expands in double quotes. An override
# with `~` will hit the same shell limit, so pass an absolute path.
install-binary prefix='$HOME/.local/bin':
    @test -f {{release-bin}} || just release
    @mkdir -p {{prefix}}
    @install -m 0755 {{release-bin}} "{{prefix}}/onote"
    @echo "✓ installed {{release-bin}} → {{prefix}}/onote"
    @echo "  add {{prefix}} to your PATH if it isn't already."

# Build, then show the binary size (track release bloat).
size: release
    @ls -lh {{release-bin}} | awk '{print "release binary:", $$5}'

# ── Docs & maintenance ───────────────────────────────────────────────────

# Generate rustdoc and open it in a browser.
doc:
    cargo doc --no-deps --open

# Remove build artifacts.
clean:
    cargo clean

# Update dependencies in Cargo.lock (review the diff before committing).
update:
    cargo update
