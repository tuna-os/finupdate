# GNOME Control Center integration — exploration

**Goal:** make finupdate appear as a page inside `gnome-control-center` (GNOME
Settings) on existing installations, not as a separate window.

## What we found

On stock `gnome-control-center` 50.1 (the version Dakota ships):

- Panels are **statically linked** into the `gnome-control-center` binary —
  there are no `lib*-panel.so` files anywhere on disk under
  `/usr/lib*/gnome-control-center-1/panels/`. The directory itself doesn't
  exist on this install.
- A baked-in registry satisfies `cc_panel_loader_load_by_name`. The
  symbol `cc_panel_loader_override_vtable` exists but is internal — used by
  CC's own swappable-panel infrastructure, not exposed as a plugin point.
- `gnome-control-center --list` enumerates a closed set of ~23 panels:
  `applications, background, bluetooth, color, display, keyboard, mouse,
  multitasking, network, wifi, notifications, online-accounts, power,
  printers, privacy, search, sharing, sound, system, universal-access,
  wacom, wellbeing, wwan`. There's no `updates` slot.
- The `.desktop`-file route (`X-GNOME-Settings-Panel` category) gives you
  **a launcher tile** in the All Settings sidebar. But the click runs `Exec=`
  in a separate process — it doesn't embed.

**Conclusion:** there is no upstream extension point for adding a panel to
an existing gnome-control-center installation today. Every path below
either patches the OS package or gives up the "embedded
inside Settings window" requirement.

---

## Realistic paths, ordered by deployment difficulty

### Path 1 — Distro-patched gnome-control-center *(Bluefin/Dakota only)*

Bluefin/Dakota controls the image, so we can carry a downstream patch that
adds an `updates` panel to gnome-control-center and rebuilds it.

**What we'd ship:**
- A `cc-updates-panel.c` panel that subclasses `CcPanel` (the standard
  GObject/AdwBin interface every CC panel uses), registered in the panel
  loader.
- The panel's UI widgets call into `libfinupdate.so`. This is a Rust cdylib
  that we'd produce from the existing crate via `crate-type = ["cdylib"]`,
  with a thin C header that `cbindgen` generates.
- A patched `gnome-control-center.spec` (or rpm-ostree layer) that links
  the new panel in.

**Pros:** the panel truly embeds in the window, the user's real goal. Works the
same way every other panel does.
**Cons:** only works on Bluefin/Dakota images. Each upstream release of
gnome-control-center needs a rebase of the patch. To distribute the Rust
backend as a cdylib, we need a stable surface for the C ABI — we don't have one yet.

**Effort:** ~1–2 weeks. The C panel is small (~500 lines) but the cdylib
boundary needs careful design.

### Path 2 — Upstream a "Software Updates" panel into gnome-control-center

The right long-term answer. Write the panel in C (to match the rest of
gnome-control-center) and submit it upstream. Once it lands, every
distribution that ships gnome-control-center gets it for free.

**Reality check:** GNOME has historically rejected an "updates" panel
because GNOME Software owns that surface. There's been
discussion (cc#1640, software#1234-ish) but no consensus. A bootc-centric
panel might land more easily because it is for bootable container
images, not generic package updates — that's a niche gnome-software
doesn't cover.

**Effort:** indefinite. Months of design discussion before any code is
acceptable.

### Path 3 — `.desktop`-file launcher tile in Settings

Ship `org.tunaos.finupdate.desktop` with
`Categories=GNOME;GTK;Settings;X-GNOME-Settings-Panel;` and a
`Keywords=update;upgrade;bootc;rebase;...` line so it surfaces in the
"All Settings" sidebar and the Settings search bar. A click on it launches
finupdate as a separate window.

**Pros:** zero patches, works on every GNOME install. Immediate
deployability via Flatpak or RPM.
**Cons:** not truly embedded. The user sees a Settings tile that
opens a separate window — which is what most third-party "settings"
apps do (GNOME Tweaks, GNOME Disks, etc.).

**Effort:** ~1 hour. Metadata only.

### Path 4 — Custom-styled standalone that mimics gnome-control-center

Make finupdate's main window look and behave like a CC panel: same
sidebar/content split, same widget style, same window chrome. Launch
it from a Settings tile (Path 3). The user "feels" like they're in
Settings even though it's a separate window.

**Pros:** under our control, no distro patches, no upstream politics.
The UI is already close to this — adw::PreferencesPage gives us the CC
look for free.
**Cons:** doesn't satisfy the literal "page inside Settings" goal.

**Effort:** mostly already done; a few CSS tweaks plus the desktop file.

---

## Recommended sequence

If the goal is "Dakota users see this in their Settings as soon as
possible":

1. **Path 3 now** — ship the `.desktop` tile so finupdate is at least
   discoverable from Settings. ~1 hour.
2. **Path 1 in parallel** — start the cdylib + C panel work so a future
   Dakota image can embed the panel. ~2 weeks.
3. **Path 2 eventually** — once the Dakota patch is stable, propose it
   upstream. Time horizon: 6+ months.

## Prep work that helps all paths

Regardless of which path lands first, these changes make it easier to
embed the panel:

- **Factor the idle-page widget tree** (`src/ui/status_view.rs`'s
  `idle_page: adw::PreferencesPage`) into a reusable `Widget` builder
  that's not coupled to the top-level `adw::ApplicationWindow`. Path 1
  must drop this widget into the content area of a `CcPanel`. Path 4
  doesn't need this, but it's clean code anyway.
- **Decouple the Settings/state model from the GTK loop**: the
  `Settings::load()` / `BOOTC_STATUS_CACHE` patterns assume a single
  process. If gnome-control-center's process loads the panel, we
  share that loop with everyone else. We must not spawn a runtime
  inside a runtime (that panics).
- **Define a stable C-ABI surface for the backend.** Today the service
  layer (`crate::service::global()`) is async Rust. Path 1 needs a
  GMainLoop-friendly subset that blocks:
  - `finupdate_init() -> *Service`
  - `finupdate_check_for_updates(svc, cb, user_data)`
  - `finupdate_apply_update(svc, cb, user_data)`
  - `finupdate_free(svc)`

  Each call that runs for a long time takes a callback. It dispatches the
  result on the main loop via `g_idle_add` so the CC main loop doesn't block.

## Open questions

- **Which Dakota release should the patched gnome-control-center ship
  in?** Coordination with the image build pipeline is a separate workstream.
- **What's the panel's identity inside CC?** "Software Updates"? "System
  Image"? "Bootc"? The label drives discoverability via Settings search.
- **Where does it slot in the sidebar?** Probably between **System** and
  **About**, or as a sub-section of **System** itself.
- **Does the panel show on non-bootc systems?** Upstream, it needs a
  graceful no-op (or hidden state) without `/usr/bin/bootc`.
