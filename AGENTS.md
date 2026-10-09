# AGENTS.md — agent guide for tuna-os/finupdate

A GTK4/libadwaita **system update frontend** for bootc systems, in Rust with
relm4. It orchestrates `bootc`, `flatpak`, `brew` and `distrobox` behind a
single `pkexec` elevation.

Human docs: [`README.md`](README.md), [`PATTERNS.md`](PATTERNS.md) (the
architectural patterns this app defines for the rest of the Bluefin utility
suite), [`docs/`](docs/), [`CONTRIBUTING.md`](CONTRIBUTING.md).

## Two build products, not one

- **The app** — the `finupdate` binary and its Flatpak
  (`org.tunaos.finupdate`), Rust workspace root plus `finupdate-core`.
- **`libfinupdate` + a GNOME Control Center panel** — `cc-panel/` is **C**,
  built with meson, producing a `CcPanel` that gnome-control-center renders as
  a "Software Updates" sidebar entry. Dakota's BuildStream patch links against
  the cdylib via `libfinupdate.pc`.

That second consumer is the one that gets forgotten. `finupdate-core`'s
exported surface is an ABI for out-of-tree C code, so a signature change there
is not internal-only, and nothing in this repo's CI builds the panel.

## The justfile needs a toolbox that CI does not use

Nearly every recipe is `toolbox run --container finupdate …`, so `just check`,
`just build`, `just lint` and `just test` all fail on a machine without a
toolbox container of that name. CI does not use `just` at all — it runs cargo
directly inside a `fedora:45` container with `gtk4-devel libadwaita-devel
pango-devel cairo-devel openssl-devel` installed. Reproduce a CI failure with
that container, not with `just`.

The one recipe worth memorising: `just flatpak` passes
**`--disable-rofiles-fuse`, which is required, not optional**. Without it the
build dies with "Failed to export bpf: System failure beyond the control of
libseccomp" before compiling anything — an error that points at the sandbox
even though bubblewrap and `flatpak build` both work fine on their own.

## Green tests may mean skipped tests

The GTK tests need a display. CI starts `broadwayd :0` and sets
`GDK_BACKEND=broadway` / `BROADWAY_DISPLAY=:0`; where `broadwayd` is missing,
the workflow says so and **the headless GTK test self-skips**. A local
`cargo test` without those variables is therefore a weaker signal than it
looks. `tests/gui/broadway-launch.sh` and `docs/GUI_TESTING.md` cover the
setup.

Coverage deliberately excludes `src/ui/`, `src/app.rs`, `src/dbus_progress.rs`
and `src/main.rs` — they need a live display and are exercised by the GUI
smoke tests instead. Widening the exclusion regex hides real code; narrowing
it makes coverage flap on display availability.

## The clippy policy is a decision, not an oversight

```
-D clippy::correctness  -D clippy::suspicious
-W clippy::style  -W clippy::complexity  -W clippy::perf
-A deprecated  -A unused
```

Real bugs fail the build; everything else warns. **`-A deprecated` is
deliberate**: it keeps the libadwaita/GTK4 deprecation migration from breaking
CI, and that migration is tracked separately. Don't quietly promote it.

## Privilege boundary

`build-aux/49-finupdate.polkit.rules` gives members of `wheel`
**password-less** access to: `org.freedesktop.login1.reboot`,
`org.freedesktop.systemd1.manage-units` and `manage-unit-files`, and
`org.freedesktop.policykit.exec` where the target program's path exactly
matches an entry in `FINUPDATE_ALLOWED_PROGRAMS` (currently `/usr/bin/bootc`,
`/usr/sbin/bootc`, `/usr/bin/finupdate-runner`,
`/usr/libexec/finupdate-runner`). #110 replaced an earlier substring check
(`program.indexOf("bootc") >= 0`) with this exact array-membership test,
because a substring match authorises any path that merely contains the text —
including one an unprivileged process can create.

Any change to what `finupdate-runner` executes changes what this rule
effectively authorises, so treat that binary as a privilege boundary. A path
belongs on `FINUPDATE_ALLOWED_PROGRAMS` only if root owns it and no
unprivileged user can write to it or to any directory on the way to it.
`docs/POLKIT_RULES.md` documents this same exact-match rule and is the more
detailed of the two; keep them in step rather than treating the `.rules` file
as the only source of truth.

## Checks

```bash
cargo fmt --all -- --check          # needs only rustfmt; clean on main
cargo clippy --all-targets -- …     # the flag list above, or `just lint`
GDK_BACKEND=broadway BROADWAY_DISPLAY=:0 cargo test --workspace --all-targets
cargo test --test dakota_image_history -- --test-threads=1
```

`dakota_image_history` is a named regression suite (for the sha-only tag
probing fix in `de1dc08`) that CI runs separately with `--test-threads=1`;
keep it single-threaded.

`screenshots.yml` **builds from source** and regenerates the images the
AppStream metainfo points at, so an app store shows the current build rather
than a hand-uploaded capture left to rot. It is path-filtered to `src/`,
`finupdate-core/`, the metainfo and `tests/visual-audit/`.
