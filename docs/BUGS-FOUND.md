# Bugs found while standing up the screenshot/validation harness

We found all of these when we ran the app under Broadway on the build host and
read what it did, not by inspection. The list order is by user impact.

---

## 1. Every GUI update was silently simulated ✅ fixed

**Symptom:** the app ran in Developer Mode permanently, so no update, rebase, or
reboot from the GUI ever touched the system.

**Cause:** two defects that made each other worse.

* `main.rs` applied `--dev-mode` when it changed `settings.json` and called
  `save()`. One run of `finupdate --dev-mode` left developer mode on
  forever. The build host's `~/.config/finupdate/settings.json` had
  `"dev_mode": true` written into it.
* `Settings::default()` set `dev_mode: is_dev_build`, and `is_dev_build` is true
  whenever `config::PROFILE` is empty — which is the case for *any* plain
  `cargo build`. So a locally built binary could never exercise the real
  orchestrator, registry, or rebase paths at all.

**Fix:**
* CLI flags now layer through `settings::RuntimeOverrides`, held in memory and
  never written back (`settings.rs`). A test flag on the app command line can no
  longer change stored configuration.
* The dev-build default is now `dry_run: true`, not `dev_mode: true`. Real code
  paths run; the `privileged()` chokepoint withholds the destructive command at
  the point of execution.
* Added `--no-dev-mode` so that you can escape from an already-polluted `settings.json`.

**Note for you:** your real config on the build host still has `dev_mode: true`. The
test harness now uses an isolated `XDG_CONFIG_HOME`, so the harness does not touch it. But
your interactive runs will continue to simulate until you clear that value.

---

## 2. Startup storm: 1213 changelog fetches and 1216 SBOM diffs per launch ✅ fixed

**Symptom:** the window frequently never painted at all. The process sat at
100% CPU with ~1261 threads, and GHCR/GitHub calls timed out. This then made
every *subsequent* run worse, because the runs used all of the API rate limits.

**Cause:** `AvailableTagsLoaded` repopulated the tag `StringList` with
`remove(0)` in a loop followed by `append` per item. Each mutation moves the
combo row's selection, firing `connect_selected_notify` — roughly 2N times for
N tags. Every one of those carried a *different* raw tag. The existing
idempotency guard in `SelectTag` only compares against the current tag, so it
let all of them through. Each one spawned a full changelog fetch + SBOM diff.

`ghcr.io/ublue-os/bluefin` publishes 612 tags, giving ~1213 fetches on a single
launch.

**Fix** (`status_view.rs`): block the `selected_notify` handler across the
repopulation, replace the remove/append loop with a single `splice()`, then
restore the selection and unblock. The code stores the handler id as `tag_row_handler`.

**Measured, same launch, before → after:**

| | before | after |
|---|---|---|
| changelog fetches | 1213 | 1 |
| SBOM diffs | 1216 | 2 |
| log lines | 14 293 | 26 |
| threads | 1261 | 11 |
| main thread state | `R` (spinning) | `S` (idle) |

---

## 3. Ten ad-hoc tokio runtimes → thread exhaustion ✅ fixed

**Symptom:** `OS can't spawn worker thread: Resource temporarily unavailable`.
It showed as a panic deep inside hyper's DNS resolver — far from the cause.

**Cause:** ten-plus call sites had their own open-coded GLib↔tokio bridge
(`app.rs` ×3, `rebase_dialog.rs` ×3, `status_view.rs` ×3, `rebase_widget.rs`,
`changelog_widget.rs`). Each one built a *fresh* runtime with its own pools of
worker and blocker threads. Some sat in the render code for each row, so the counts multiplied.

**Fix:** new `src/runtime.rs` — one shared runtime with many threads and bounded
pools (4 workers, 32 blocker threads). It also has a `block_on` that picks the right
strategy for the context of the caller. Ad-hoc runtimes removed.

`ffi.rs` was already correct (one runtime per `Handle`) and was left alone.

---

## 4. Panic: "Cannot start a runtime from within a runtime" ✅ fixed

**Symptom:** intermittent crash on launch — only when a background fetch
happened to race UI construction.

**Cause:** `detect_bootc_image_info` built a runtime and called `block_on`. Its
doc comment asserted "every caller here runs on the GTK thread", but the
changelog path reaches it via `read_selected_tag()` from *inside* the runtime,
where `block_on` panics.

**Fix:** route through `runtime::block_on`, which uses `block_in_place` when
already inside the runtime. Also memoised the whole function
(`BOOTC_IMAGE_INFO_CACHE`) — it ran the full detection chain again,
including a `bootc status` subprocess, once per rendered version row.

---

## 5. GApplication rejected the app's own CLI flags ✅ fixed

**Symptom:** `finupdate --dry-run` exited with `Unknown option --dry-run`
*after* logging that it had accepted the flag.

**Cause:** the code parsed flags by hand, then gave the full `argv` to
`RelmApp`. GApplication also parses argv, and it aborts on anything it does
not recognise.

**Fix:** pass only `argv[0]` to `RelmApp::with_args` — at that point, `RuntimeOverrides` already
holds every flag.

---

## 6. Window cannot reach the HIG minimum width ✅ fixed

We lowered `width-request` to 360 and added an `AdwBreakpoint`, but this was not enough —
the window still refused to narrow. So, to replace guesswork, we added `FINUPDATE_MEASURE=1`.
It walks the widget tree at startup and prints each widget's measured
minimum width (`FINUPDATE_MEASURE_MIN` filters to the offenders).

That showed that the window itself honoured 360 while its content demanded 579.
At the bottom of the chain were preference *rows* of 543–549px. But the rows on the
visible page were not that wide. **`gtk::Stack` is homogeneous by default**, so
it requests the largest width of *every* page, including hidden ones. The idle
page got the minimum width of the history/changelog rows it had never
displayed.

The fix turns off `hhomogeneous`/`vhomogeneous` on the status stack, so it
sizes to the visible child. An adaptive layout wants this anyway. The fix also
lets the row labels wrap (`title-lines`/`subtitle-lines` of **0**, which means
*unlimited*). Note that 1 does the opposite of what it looks like: it pins the
label to a single line whose minimum is the entire string.

Result: content minimum **579px → 240px**, natural 579 → 423. The window renders
correctly at 360×640, verified by the `narrow` screenshot check.

---

## 7. Late async result panics after component teardown ✅ fixed

```
The runtime of the component was shutdown. Maybe you accidentally dropped a
controller?: AvailableTagsLoaded([...])
```

Sometimes a fetch from the registry completed after the app dropped its relm4 component. The fetch then sent into a
closed channel and panicked the worker thread.

The trap is that `ComponentSender::input()` **unwraps internally**, so the
`let _ =` some call sites already had was purely cosmetic — it discards a `()`,
not an error.

The fix delivers every background-thread result through
`sender.input_sender().send(..)`, which returns a `Result` that the caller can safely
ignore. Six sites in the changelog/registry/SBOM fetch paths. A late result for
a page that the user already left is normal, not exceptional, so
the correct behaviour is to drop it silently.

Click handlers and `update()` arms still use `input()` deliberately — the
component is alive by definition at those points.

---

## 8. Flatpak build fails with a seccomp error ✅ fixed

```
error: Failed to export bpf: System failure beyond the control of libseccomp
```

Identical with `flatpak run org.flatpak.Builder` and native `flatpak-builder`,
which pointed at the host and not at the manifest. But bubblewrap alone worked
(`bwrap --ro-bind / / --unshare-all true`), and so did `flatpak build` against
the SDK — so the sandbox itself was fine and only flatpak-builder's module step
failed.

The fix is `--disable-rofiles-fuse`, which the gtk-office-suite build already
used. `just flatpak` now passes it.

---

## 9. Harness hazard: stale instances accumulate (not an app bug)

`pkill -x finupdate` does not match instances launched via `toolbox run`, so
each new test launch left more processes alive, up to four. A leftover instance
keeps the D-Bus name and the Broadway surface. This shows as a **blank
screenshot** and not as an error. Know about this trap when you read
failures. The launcher now matches on the full command line and warns if
anything survives.
