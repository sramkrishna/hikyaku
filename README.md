# Hikyaku

A Matrix chat client for GNOME, designed around activity awareness.

Built with Rust, GTK4, and libadwaita using the matrix-rust-sdk.

![GNOME 50](https://img.shields.io/badge/GNOME-50-4A86CF)
![License: GPL v3](https://img.shields.io/badge/License-GPLv3-blue.svg)

## Features

- **End-to-end encryption** — full E2EE support with key backup and cross-signing
- **Rich messages** — Markdown/HTML rendering, reactions, edits, threaded replies
- **Spell checking** — via enchant
- **@mention notifications** — with rolodex contact completion
- **Local bookmarks** — pin messages locally without affecting the Matrix server
- **Room topic tracker** — badge when a room's topic changes since your last visit
- **AI summaries** — Ollama-backed room summaries and semantic interest watching
- **Community health monitor** — sentiment scoring per room for community managers
- **Customisable appearance** — font, tint colours, gradients for message area and sidebar

## Plugin system

Features are gated behind Cargo feature flags. The default build enables all plugins:

| Feature | Description |
|---|---|
| `ai` | Ollama summaries + semantic interest watcher |
| `rolodex` | Personal contact book driving @mention completion |
| `pinning` | Local message bookmarks |
| `motd` | Room topic change notifications |
| `community-health` | Per-room emotion/sentiment scoring |

See [docs/plugin-guide.md](docs/plugin-guide.md) for instructions on writing new plugins.

## Building

### Dependencies

- Rust (stable)
- GTK4 ≥ 4.14
- libadwaita ≥ 1.6
- GNOME Platform 50 (for Flatpak)
- enchant (spell checking)
- GStreamer (media playback)

### From source

```bash
# All plugins (default)
cargo build --release

# Without AI / embeddings
cargo build --release --no-default-features --features rolodex,pinning,motd

cargo run --release
```

### Flatpak

```bash
flatpak-builder --install --user build-dir flatpak/me.ramkrishna.hikyaku.json
flatpak run me.ramkrishna.hikyaku
```

An OpenVINO extension is available for accelerated AI inference on Intel hardware.

## Profiling

Build with frame pointers for sysprof callgraph support:

```toml
# .cargo/config.toml
[build]
rustflags = ["-Cforce-frame-pointers=yes", "-Csymbol-mangling-version=v0"]
```

```bash
sysprof-cli --gtk --speedtrack hikyaku.syscap -- ./target/debug/hikyaku
```

## License

GNU General Public License v3.0 — see [LICENSE](LICENSE) for details.
