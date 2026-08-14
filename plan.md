# Finupdate — current state and remaining work

Updated 2026-07-26. The previous version of this file listed six task groups all
marked `[x]`; every one of them predated the work below and several were no
longer true. Treat this as the live picture.

All builds and tests run on the **build host** (`~/dev/finupdate`, toolbox
`finupdate`, Fedora 43 / GTK 4.20.4 / libadwaita 1.8.6). The local machine is a
lightweight VPS with GTK 4.14 and cannot build this crate.

---

## Done

### Correctness — seven real bugs, all found by running the app

See `docs/BUGS-FOUND.md` for the full write-ups and measurements.

1. **Every GUI update was silently simulated.** `--dev-mode` persisted itself
   into `settings.json`, and `Settings::default()` set `dev_mode: true` for any
   plain `cargo build`. Fixed: CLI flags are per-run only
   (`settings::RuntimeOverrides`), dev builds default to `dry_run` instead, and
   `--no-dev-mode` / `--no-dry-run` exist to escape either.
2. **Startup storm: 1213 changelog fetches + 1216 SBOM diffs per launch.**
   Repopulating the tag dropdown fired `selected_notify` ~2× per tag. Fixed with
   a blocked handler around a single `splice()`. → 1 fetch, 2 diffs, 11 threads.
3. **Ten ad-hoc tokio runtimes** causing thread exhaustion. Consolidated into
   `src/runtime.rs`.
4. **`block_on` inside a runtime** panic in `detect_bootc_image_info`; also
   memoised, since it re-ran the whole detection chain per rendered row.
5. **GApplication rejected the app's own CLI flags.**
6. **Window could not reach the HIG 360px minimum.** `gtk::Stack` is
   homogeneous by default, so it requested the widest of *all* pages including
   hidden ones. Content minimum 579px → 240px.
7. **Late async results panicked after component teardown.**
   `ComponentSender::input()` unwraps internally, so `let _ =` was cosmetic;
   background deliveries now use the fallible `input_sender().send(..)`.

Plus a **flaky test** (`test_is_uupd_installed`, ~1 run in 3): two modules
mutated the process-global `PATH` under separate mutexes. Now share
`src/test_support.rs::env_lock()`. 5/5 clean runs.

### Dry-run is structural, not remembered

`src/privileged.rs` is the single chokepoint — you cannot obtain a runnable
`Command` without passing a suppression state, so a newly added destructive
action cannot silently execute under dry-run. `src/action_journal.rs` records
every intent as JSONL with the exact `would_run` argv.

Converted: `switch_image`, `unpin`, `factory_reset`, `powerwash` steps,
`reboot`, `schedule_reboot`, `set_uupd_timer` (both duplicate implementations),
`write_uupd_config`. Read-only probes route through the same chokepoint with
`Suppressed::No` — journalled, but still executed.

### GUI test suite

`tests/gui/` — Broadway + Playwright + the action journal. Runs headless with no
GNOME session and no `gnome-ponytail-daemon`, which was the blocker that stalled
GUI coverage. See `docs/GUI_TESTING.md`.

### Packaging — both deliverables build *and* launch

**Standalone Flatpak.** `just flatpak` builds, installs, launches, and renders
(`tests/gui/screenshots/light/flatpak-devel-launch.png`). The build had been
failing with `Failed to export bpf: System failure beyond the control of
libseccomp`; the fix is `--disable-rofiles-fuse`, now in the recipe. Note the
running app shows a real "Update available" preflight result — dry-run withholds
only the destructive command, so production code paths genuinely execute.

**Control-center panel.** `libfinupdate.so` builds, exports all 10 FFI symbols,
installs via `install-libfinupdate.sh`, resolves through `pkg-config`, and
`examples/panel-demo` compiles, links, and renders the embedded widgets
(`tests/gui/screenshots/light/cc-panel-demo.png`) including the SBOM stack diff.

---

## Remaining

### 1. HIG findings — see `docs/GNOME-HIG-AUDIT.md`

**All six are now closed.**

| # | Finding | State |
|---|---|---|
| 3 | `AdwNavigationView` | ✅ fixed |
| 2 | GSettings migration | ✅ fixed |
| 1 | Adaptive width | ✅ fixed |
| 4 | Tooltips | ✅ fixed |
| 5 | Access keys | ✅ fixed |
| 6 | Preferences search | ✅ fixed |

### 2. cc-panel end-to-end

`wip/cc-panel-toolbox` on the build host holds a full gnome-control-center build
harness (`build-aux/test-cc-panel-in-toolbox.sh` plus dakota PR #743 patches),
committed but unmerged. Needs validating and merging.

### 3. Crate split — `finupdate-core` done, god objects remain

`finupdate-core` is extracted, following gtk-office-suite's `-core`/UI shape.
It holds the whole backend (service, registry, SBOM, orchestrator, update
worker, uupd compat, settings, privileged, action journal, runtime, gpu,
config) and depends on `glib`/`gio` but **not** `gtk4`, `libadwaita` or
`relm4` — verified: 0 matches in `cargo tree -p finupdate-core`, 435 deps
versus the GUI crate's 663.

The abstraction already existed (`UpdaterService` + `FixtureRegistry` + the
headless CLI); what was missing was *enforcement*. Now a stray `use gtk::…` in
the backend is a compile error rather than a review comment. Tests split
165 core / 126 GUI — the same 291 as before.

Still to do: break up `src/ui/status_view.rs` (~4900 lines) and
`src/ui/rebase_dialog.rs` (2438). Both are inside the GUI crate, so this is
pure module extraction with no build-system involvement.

**Greenfield was considered and rejected.** Only five dead-code warnings exist
across ~21k lines, three of which are new helpers; 291 tests pass; the
`UpdaterService` / `FixtureRegistry` / headless-CLI abstraction already exists.
A rewrite would discard `registry_client.rs`, `sbom_diff.rs`, and
`orchestrator.rs` — the parts that encode how ublue actually publishes images.
