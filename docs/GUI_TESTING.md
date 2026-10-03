# GUI Testing for Finupdate

Two suites, for two different jobs.

| | `just gui-test` (primary) | `just gui-test-onhardware` (smoke) |
|---|---|---|
| Backend | Broadway + Playwright | dogtail/behave + AT-SPI |
| Needs a GNOME session | no | **yes** |
| Needs `gnome-ponytail-daemon` | no | **yes** |
| Runs on a build host | yes | no |
| Exercises real pkexec/polkit | no | **yes** |
| Runs the real Flatpak | no | **yes** |

Use the first for everyday work. Keep the second to verify the things
Broadway can never reach — a real elevation prompt, a real Flatpak sandbox.

---

## Primary suite: Broadway + screenshots + action journal

### Why this shape

The dogtail suite was effectively unrunnable: it needs a live GNOME session and
`gnome-ponytail-daemon`, which isn't in the Dakota image. That blocker is why
GUI coverage stalled.

Broadway (`gtk4-broadwayd`) renders GTK4 into a browser over WebSockets, so the
app runs headless with no compositor at all. Ported from
`tuna-os/gtk-office-suite`'s `broadway-inspect` skill — with one important
difference that we found here:

> **GTK4 rasterises text into textures.** Broadway's DOM has real element nodes
> but **zero text nodes** (measured on this app: 133 `<div>`, 19 `<img>`, 0
> text). The office-suite approach, which scrapes text and clicks via
> `get_by_text()`, therefore cannot work.

What does work, and what the suite is built on:

* **Rendered output** — pixel-accurate. Screenshots are excellent.
* **Input** — Broadway forwards mouse and keyboard to GTK. `AdwDialog` and
  popovers render and respond correctly.
* **Backend intent** — the JSONL action journal (`src/action_journal.rs`).

We investigated AT-SPI as the selector layer and **rejected** it: GTK4 under the
Broadway backend reports `Unrecognized accessibility backend "atspi"`. Targets
are therefore coordinates in a fixed-size window (see `WIDGETS` in
`tests/gui/harness.py`), or keyboard traversal via `activate_by_keyboard()`.

### Each check is a triple

```python
with FinupdateApp() as app:                # 1. launch, dry-run, isolated config
    app.click("automatic_updates_switch")  #    drive
    app.screenshot("uupd-timer-toggled")   # 2. look
    app.assert_action(                     # 3. assert the BACKEND intent
        "set_uupd_timer",
        would_run_contains=["pkexec", "systemctl", "--now", "uupd.timer"],
        suppressed=True,
    )
```

Step 3 is the one a screenshot can never make. It separates
"the look of the rebase dialog is right" and "a click on Switch would have run
`bootc switch ghcr.io/ublue-os/bluefin:stable` — and did not run it."

### Running

```bash
just gui-test-setup           # once: playwright + chromium
just gui-test                 # everything
just gui-test "idle narrow"   # named checks
just broadway 360x640         # launch and poke by hand at localhost:8085
```

Screenshots land in `tests/gui/screenshots/<theme>/`. They are artifacts for a
human to review, **not** golden-image comparisons. A pixel diff of GTK across
libadwaita releases is a maintenance sink. The behavioural assertion lives
in the journal instead.

### Safety and determinism

Nothing destructive runs. The launcher starts the app with `--dry-run --no-dev-mode`.
The app runs its real code paths, but `privileged()` withholds every privileged command and
records it instead. In addition the launcher pins:

* an **isolated `XDG_CONFIG_HOME`** — the suite never reads or writes your real
  `~/.config/finupdate/settings.json`;
* `GTK_ENABLE_ANIMATIONS=0`, so no capture lands mid-transition;
* a fixed window geometry via `FINUPDATE_WINDOW_SIZE`;
* a fixed mock image via `FINUPDATE_IMAGE`.

### Gotchas worth knowing

* **A blank screenshot usually means a stale instance.** `pkill -x finupdate`
  does not match processes that `toolbox run` starts. Thus the old instances
  stay and accumulate. A leftover instance holds the D-Bus name and the Broadway surface.
  You then get a white PNG instead of an error. The launcher now matches on the full
  command line and warns if anything survives.
* **The toolbox shares `$HOME` but not `/var/tmp`.** The journal lives
  at `~/.finupdate-test-journal.jsonl` for exactly this reason. A process in the
  container writes to its own `/var/tmp` path, and the harness cannot see it.
* **`cargo test --lib` does not rebuild the binary.** Run `cargo build --bins`
  before the GUI suite or you will test a stale app. (This produced a real
  false failure during development.)
* `/tmp` on the build host is a small tmpfs, and it often fills up.
  Playwright's chromium then fails with an unclear profile error. Hence
  `TMPDIR=/var/tmp/pw-tmp`.

---

## On-hardware smoke suite

`tests/smoke/features/` — dogtail + behave via qecore, unchanged. It is the only
suite that exercises the real Flatpak in a real session, with polkit. So we
keep it, but we moved it out of the normal loop.

Requirements: an active GNOME session, `toolkit-accessibility` enabled, and
`gnome-ponytail-daemon` (`just install-ponytail` builds it into `~/.local` if
your image lacks it).

```bash
just gui-test-onhardware              # all
just gui-test-onhardware smoke @launch
```

GNOME Shell stays in `unsafe_mode` after qecore runs; the recipe resets it on exit via
`just _reset-unsafe-mode`.

---

## Adding a check

1. Add a `@check("name", "what it proves")` to `tests/gui/test_features.py`.
2. If you need a new click target, capture a screenshot and read the coordinate off
   it. Add the coordinate to `WIDGETS` in `harness.py`, with a comment that names
   the source capture. Prefer `activate_by_keyboard()` where the tab order is
   stable. If a pixel target moves, the check fails with an unclear screenshot
   diff, not an error.
3. If the feature touches the host, assert the journal entry too. A check that
   only screenshots is half a check.
4. Add the corresponding row to `docs/app-logic-map.md` §4 in the same change,
   so the test suite can still depend on the map.
