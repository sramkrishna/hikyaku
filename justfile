# Hikyaku build recipes
#
# Install just:  brew install just  OR  nix-env -iA nixpkgs.just
# Usage:         just <recipe>      (run `just` or `just --list` to see all)

app_id := "me.ramkrishna.hikyaku"
manifest := "flatpak/" + app_id + ".json"
build_dir := "build-dir"

# ── default ──────────────────────────────────────────────────────────────────

# Show available recipes
default:
    @just --list

# ── local dev build (cargo) ──────────────────────────────────────────────────

# Build in debug mode (fast, for development)
build:
    cargo build

# Build in release mode (optimised)
build-release:
    cargo build --release

# Run the app in debug mode
run:
    cargo run

# Run tests
test:
    cargo test

# Run tests with output (useful for debugging)
test-verbose:
    cargo test -- --nocapture

# Show build timing profile (opens HTML report in build/)
timings:
    cargo build --timings

# ── flatpak ──────────────────────────────────────────────────────────────────

# Build and install the flatpak locally (user install, for testing)
flatpak-build:
    flatpak-builder --user --install --force-clean {{ build_dir }} {{ manifest }}

# Build the flatpak without installing
flatpak-build-only:
    flatpak-builder --force-clean {{ build_dir }} {{ manifest }}

# Run the installed flatpak
flatpak-run:
    flatpak run {{ app_id }}

# Uninstall the user-installed flatpak
flatpak-uninstall:
    flatpak uninstall --user {{ app_id }}

# Rebuild cargo-sources.json from the current Cargo.lock
# Run this after updating any Cargo dependencies
flatpak-update-sources:
    #!/usr/bin/env bash
    set -euo pipefail
    if ! command -v flatpak-cargo-generator &>/dev/null; then
        echo "flatpak-cargo-generator not found."
        echo "Install: pip install flatpak-cargo-generator"
        echo "  or: https://github.com/flatpak/flatpak-builder-tools"
        exit 1
    fi
    flatpak-cargo-generator Cargo.lock -o flatpak/cargo-sources.json
    echo "flatpak/cargo-sources.json updated."

# ── cleanup ──────────────────────────────────────────────────────────────────

# Remove cargo build artifacts
clean:
    cargo clean

# Remove flatpak build directory
clean-flatpak:
    rm -rf {{ build_dir }} .flatpak-builder

# Remove everything
clean-all: clean clean-flatpak
