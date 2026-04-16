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

# ── profiling (sysprof) ──────────────────────────────────────────────────────

# Capture a sysprof profile.  Run the app, reproduce the slow path, then Ctrl-C.
# Writes hikyaku.syscap in the project root.
# Requires: sysprof-cli  (dnf install sysprof  OR  apt install sysprof)
profile:
    cargo build
    sysprof-cli --gtk --speedtrack hikyaku.syscap -- ./target/debug/hikyaku

# Convert the last captured profile to a human-readable call-graph text file.
# Search the output for your function names to find hot call chains.
profile-analyze:
    sysprof-cat hikyaku.syscap > hikyaku-profile.txt
    @echo "Written to hikyaku-profile.txt — grep for your function names."

# Capture and immediately analyze in one step.
profile-full:
    cargo build
    sysprof-cli --gtk --speedtrack hikyaku.syscap -- ./target/debug/hikyaku
    sysprof-cat hikyaku.syscap > hikyaku-profile.txt
    @echo "Written to hikyaku-profile.txt"

# ── flatpak ──────────────────────────────────────────────────────────────────

# Number of parallel jobs for flatpak-builder.  Default: half the cores so
# the system stays responsive during a build.  Override: just flatpak-build jobs=12
jobs := `nproc --ignore=4`

# Build and install the flatpak locally (user install, for testing).
# Uses the .flatpak-builder cache — only changed modules rebuild.
# Pass jobs=N to override parallelism (default: nproc - 4).
flatpak-build:
    nice -n 10 flatpak-builder --user --install --jobs={{ jobs }} {{ build_dir }} {{ manifest }}

# Full rebuild from scratch (nukes cache — slow, use rarely)
flatpak-build-clean:
    nice -n 10 flatpak-builder --user --install --force-clean --jobs={{ jobs }} {{ build_dir }} {{ manifest }}

# Build the flatpak without installing
flatpak-build-only:
    nice -n 10 flatpak-builder --jobs={{ jobs }} {{ build_dir }} {{ manifest }}

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
