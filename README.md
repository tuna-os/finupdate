# Finupdate

**A modern GTK4/libadwaita system update frontend for Bluefin and Universal Blue**

Finupdate provides a graphical interface to run system updates on [Bluefin](https://projectbluefin.io) and Universal Blue systems. It orchestrates `bootc`, `flatpak`, `brew`, and `distrobox` directly — no `uupd` required. It's the first app in the Bluefin utility suite and serves as a reference implementation for future apps.

![GNOME 47+](https://img.shields.io/badge/GNOME-47%2B-blue)
![Rust](https://img.shields.io/badge/Rust-2024_edition-orange)
![License: MIT](https://img.shields.io/badge/License-MIT-green)

## Features

- **One-click system updates** — orchestrates `bootc`, `flatpak`, `brew`, and `distrobox` via a single pkexec elevation
- **Live log stream** — real-time stdout/stderr from each update module
- **Elapsed timer** — shows how long the update has run
- **Copy log** — clipboard integration to share output
- **Cancel support** — gracefully cancel an update in progress
- **Desktop notifications** — GNotification when update completes/fails
- **Reboot prompt** — confirmation dialog to restart after updates
- **Window close guard** — prevents accidental close during active updates
- **Last update time** — shows when `uupd` last ran successfully
- **Keyboard shortcuts** — Ctrl+Q (quit), Ctrl+? (shortcuts window)
- **About dialog** — accessible via hamburger menu
- **Flatpak sandbox aware** — uses `flatpak-spawn --host` when sandboxed
- **Dark mode** — automatic via libadwaita (follows system preference)
- **GNOME HIG compliant** — symbolic icons, proper space between elements, accessibility

## Screenshots

The app has four states:

| Idle | Updating | Complete | Error |
|------|----------|----------|-------|
| Status page with "Check for Updates" button | Progress bar + live log + timer | Success page with reboot option | Error page with retry |

## Requirements

### Runtime
- GTK 4.16+ (GNOME 47+)
- libadwaita 1.7+
- `bootc` or `rpm-ostree` on the host system
- `flatpak`, `brew`, `distrobox` — optional; finupdate skips each module if the tool is absent
- `uupd` — optional; if present, enables the "Automatic background updates" toggle in Preferences

### Build
- Rust 1.85+ (edition 2024)
- Meson 0.59+
- GTK4 and libadwaita development headers

## Installing

We publish released builds to the TunaOS Flatpak remote for x86_64 and
aarch64:

```bash
flatpak remote-add --if-not-exists tuna-os https://tunaos.org/flatpak/tuna-os.flatpakrepo
flatpak install tuna-os org.tunaos.finupdate
```

The remote is an OCI index backed by `ghcr.io/tuna-os/finupdate`; see
[tuna-os/flatpak-index](https://github.com/tuna-os/flatpak-index). The
`.github/workflows/publish-flatpak.yml` workflow pushes builds on every push to `main`.

If a published build has a defect, see
[runbooks/rollback-a-bad-finupdate-release.md](runbooks/rollback-a-bad-finupdate-release.md).

## Building

### Option A: Flatpak (recommended for testing)

```bash
# One-time: install the GNOME SDK and Rust extension
flatpak install flathub org.gnome.Sdk//50 org.gnome.Platform//50
flatpak install flathub org.freedesktop.Sdk.Extension.rust-stable//25.08
flatpak install flathub org.flatpak.Builder

# Build and install locally
flatpak run org.flatpak.Builder --user --install --force-clean _flatpak \
  build-aux/org.tunaos.finupdate.Devel.json

# Run
flatpak run org.tunaos.finupdate.Devel
```

### Option B: Native Meson build

You need the dev packages for GTK4 and libadwaita:

```bash
# Fedora/Bluefin:
sudo dnf install gtk4-devel libadwaita-devel meson cargo

# Build
meson setup _build
meson compile -C _build

# Run
./_build/src/finupdate
```

### Option C: Cargo only (dev iteration)

If you have GTK4/libadwaita headers available (e.g., in a devcontainer):

```bash
cargo build          # Debug
cargo build --release  # Release
./target/release/finupdate
```

## Development

### Architecture

```
finupdate-core/src/       # GTK-free backend, reusable by GUI and CLI
├── service.rs           # UpdaterService interface and bootc implementation
├── registry_client/     # Image discovery, tag history, and family resolution
├── orchestrator.rs      # Privileged update runner protocol
├── update_worker.rs     # Update event stream and simulator
└── settings.rs          # GSettings preferences with JSON fallback

src/                      # GTK/libadwaita frontend and shared-library surface
├── main.rs              # GUI entry point and per-run test flags
├── app.rs               # Top-level relm4 application component
├── cli.rs               # Headless CLI entry point
├── ffi.rs               # C ABI used by the GNOME Settings panel
├── changelog_widget.rs  # Embeddable changelog widget
├── rebase_widget.rs     # Embeddable image-switching widget
└── ui/                  # Focused application views and dialogs
```

### State Machine

```
Idle ──[StartUpdate]──→ Updating ──[Complete]──→ Complete ──[Dismiss]──→ Idle
                            │                                              ↑
                            └──────[Error]──→ Error ──────[Retry/Dismiss]──┘
                            │
                            └──────[Cancel]──→ Idle
```

The separate `finupdate-core` crate enforces the backend boundary. It
does not depend on GTK, and you can build and test it on a headless host. The GUI
crate re-exports that backend for compatibility, adds the relm4 application,
and exposes reusable widgets through the C ABI for the GNOME Settings panel.
See the module-level documentation in
[`finupdate-core/src/lib.rs`](finupdate-core/src/lib.rs) for the complete
backend map.

### Key Design Decisions

1. **relm4 over raw gtk4-rs** — Component model with messages prevents callback spaghetti
2. **Tokio in a separate thread** — GTK owns the main thread; async I/O needs its own runtime
3. **mpsc channels (not callbacks)** — Decouples worker from UI; lets unit tests run in isolation
4. **gtk::Stack (not show/hide)** — Built-in crossfade transitions, no manual visibility management
5. **Imperative widget construction** — Some complex widgets built in `init()` when the view! macro can't express them

### Flatpak Sandbox Notes

When it runs in Flatpak, the app uses `flatpak-spawn --host` to execute commands on the host:
- Needs `--talk-name=org.freedesktop.Flatpak` in Flatpak manifest
- Detection: checks for `/.flatpak-info` file
- The app automatically wraps all host commands (uupd, systemctl)

### Environment Variables

| Variable | Effect |
|----------|--------|
| `RUST_LOG=finupdate=debug` | Enable debug logging |
| `RUST_LOG=trace` | Full trace output |
| `GTK_DEBUG=interactive` | GTK Inspector |

### Testing

```bash
# Run unit tests (Cargo or just)
cargo test --all-targets
just test

# Run Broadway GUI test suite
just gui-test
```

## Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md) for development workflow and guidelines.

## Reusable Patterns

See [PATTERNS.md](PATTERNS.md) for documented architectural patterns. All future utility apps for Bluefin should use them.

## License

MIT — see [Cargo.toml](Cargo.toml)

## Related

- [Project Bluefin](https://projectbluefin.io) — the desktop OS this is built for
- [GNOME HIG](https://developer.gnome.org/hig/) — the design guidelines we follow
- [uupd](https://github.com/ublue-os/uupd) — optional host daemon; if installed, you can toggle its timer from Preferences
