# Installing finupdate

finupdate ships in **two surfaces from one codebase**:

| | What it is | Audience | Sandboxed? |
|---|---|---|---|
| **Flatpak** | Standalone GTK app launched from Activities | Any user on any GNOME distro | Yes (sandboxed via xdg portal) |
| **GNOME Settings panel** | New "Updates" page inside `gnome-control-center` | Bluefin/Dakota users on patched gnome-control-center | No (runs in cc's process) |

Both can be installed on the same machine. They use the same backend (registry probe, SBOM diff, update orchestrator) but have independent settings files and separate UI entry points.

---

## Path A — Flatpak (any GNOME distro)

The flatpak is the canonical "I want to try the app" path. It works on any GNOME distro that has `flatpak` + the GNOME runtime.

### As a user — install the published Flatpak

> Once finupdate ships to Flathub or a Bluefin Flatpak remote, this becomes a one-liner. Until then, see "Build from source" below.

```sh
flatpak install <remote> org.tunaos.finupdate
flatpak run org.tunaos.finupdate
```

### As a developer — build from source

The repo's `justfile` wraps the toolchain so the host doesn't need GTK / libadwaita dev headers (those live in a `finupdate` toolbox container).

```sh
# One-time toolbox setup
just setup

# Build + install the dev Flatpak (id: org.tunaos.finupdate.Devel)
just flatpak

# Run it
just run                # or: flatpak run org.tunaos.finupdate.Devel

# After install, refresh the GNOME dock so the launcher appears
just dock
```

The dev flatpak uses a different application ID (`…Finupdate.Devel`) so it can coexist with a stable build.

### Verifying it works

```sh
flatpak list --user | grep -i finupdate
# org.tunaos.finupdate.Devel    0.1.0    master
```

Logs go to the journal — `journalctl --user -f` while the app runs surfaces backend traces.

---

## Path B — GNOME Settings panel (Bluefin / Dakota)

The panel is a native `gnome-control-center` page that doesn't open a separate window. Because modern `gnome-control-center` (≥45) **statically links all panels** with no public plugin interface, shipping a panel means patching the gnome-control-center package itself.

Dakota's image is built with BuildStream so the patch lands as a downstream element — see [`projectbluefin/dakota#673`](https://github.com/projectbluefin/dakota/issues/673) for the tracking issue.

### As a user — install via the patched OS image

Nothing to do. Roll forward to a Dakota build that includes the patched `gnome-control-center` and `libfinupdate` packages and the panel appears in **Settings → Updates**.

### As a packager — produce the patched gnome-control-center

Six steps. Full detail in [`cc-panel/README.md`](cc-panel/README.md).

1. **Build + stage `libfinupdate.so` + `finupdate.h` + `libfinupdate.pc`** into the image's prefix:
   ```sh
   sudo build-aux/install-libfinupdate.sh /usr
   # or, via just (defaults to /usr/local):
   just panel-install /usr
   ```

2. **Vendor `cc-panel/panels/updates/`** into a downstream `gnome-control-center` source tree:
   ```sh
   cp -r /path/to/finupdate/cc-panel/panels/updates \
         /path/to/gnome-control-center/panels/
   ```

3. **Register the panel in the cc loader** (`shell/cc-panel-loader.c`):
   ```c
   extern GType cc_updates_panel_get_type (void);
   …
   PANEL_TYPE("updates", cc_updates_panel_get_type),
   ```

4. **Hook the panel into the meson build** by adding `subdir('updates')` to `panels/meson.build`.

5. **Build gnome-control-center** with `PKG_CONFIG_PATH` pointing at the staged `libfinupdate.pc`:
   ```sh
   PKG_CONFIG_PATH=/usr/lib/pkgconfig:$PKG_CONFIG_PATH \
     meson setup builddir
   meson compile -C builddir
   ```

6. **Ship the patched RPM** in the Dakota Containerfile / BuildStream element. The C ABI of `libfinupdate.so` is the contract between this repo and the patched cc — coordinate ABI changes with a finupdate release tag.

### Verifying it works

```sh
gnome-control-center --list | grep updates
# updates
gnome-control-center updates
```

Or just open Settings → there's a new **Updates** entry in the sidebar.

---

## Coexistence — when both are installed

Both surfaces use the same Rust backend, so behaviour is consistent. A handful of things to know:

- **Two settings files.** Flatpak writes to `~/.var/app/org.tunaos.finupdate.Devel/config/finupdate/settings.json`; the panel writes to `~/.config/finupdate/settings.json`. Preferences don't sync between them.
- **Polkit is shared.** Both paths invoke the same `pkexec finupdate-runner` for privileged actions; the polkit rule in `build-aux/49-finupdate.polkit.rules` covers both.
- **`bootc` detection is path-aware.** Inside the flatpak we use `flatpak-spawn --host bootc status --json`. The panel calls `bootc` directly. Same backend code, single `update_worker::is_flatpak()` switch picks the right transport.
- **Don't run an Install in both at the same time.** Nothing prevents it today; symptoms would be two `pkexec` prompts and a bootc-lock collision. A file lock is on the [carry-forward list](https://github.com/tuna-os/finupdate/pull/1) — file an issue if you hit it.

Most users will only notice one surface — pick based on muscle memory (Activities search vs. Settings).

---

## Developer workflow — iterating on shared code

A change in `src/service.rs` / `src/registry_client.rs` / `src/orchestrator.rs` etc. flows into both surfaces with one `cargo build`. The dev loops:

```sh
# Quick check — type-check + tests in the toolbox (~3 s warm)
just check
just test

# Iterate on the flatpak surface (~1 min)
just flatpak     # rebuild + reinstall the dev flatpak
just run         # launch it

# Iterate on the cc-panel widgets without rebuilding gnome-control-center
just panel-demo  # builds the cdylib + a tiny GTK harness that hosts the
                 # FFI widgets in a standalone window. Lets you visually
                 # check finupdate_changelog_widget_new and
                 # finupdate_rebase_widget_new in seconds instead of
                 # the minutes a full cc rebuild takes.
```

`just panel-demo` writes to `/tmp/finupdate-dev/` so it doesn't pollute system paths. Override with `PREFIX=/some/path just panel-demo` if you want it somewhere else.

When the cc panel UI design has stabilised, switch to a full `just panel-install /usr` + patched cc rebuild for end-to-end verification.
