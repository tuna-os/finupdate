//! Reading the host's bootc / os-release state, and the pure parsers over it.
//!
//! Split out of `status_view` (which was ~5100 lines) because this is
//! *introspection*, not UI: it answers "what image is this machine booted on,
//! what deployments exist, what's the kernel". Most of it is pure functions
//! over a `bootc status --json` blob, which is also why most of the unit tests
//! in this area target these names.
//!
//! Caching matters here. `get_cached_bootc_status` and
//! `BOOTC_IMAGE_INFO_CACHE` exist because these are called from per-row
//! rendering code; without them, displaying an image with a few hundred tags
//! spawned a `bootc status` subprocess per row and exhausted the process
//! thread limit.

use adw::prelude::*;
use gtk::prelude::*;
use relm4::gtk;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use crate::registry_client::ImageVersion;
use crate::settings::Settings;

use super::status_view::MockDeployment;

/// Read the current OS image name and variant from `/etc/os-release`.
/// Tries `/run/host/etc/os-release` first for Flatpak compatibility.
pub(super) fn read_image_info() -> Option<String> {
    // Compose "{NAME} {IMAGE_NAME-capitalised}" when both are present —
    // e.g. NAME="Bluefin", IMAGE_NAME="dakota" → "Bluefin Dakota". Gives
    // the hero row a richer identity than PRETTY_NAME alone (which on
    // Bluefin is just "Bluefin"). Falls back to PRETTY_NAME, then
    // detect_bootc_image_info's "org/image" form, then IMAGE_ID +
    // VARIANT_ID — same chain as before, just with the composition step
    // bolted on top.
    let name = read_os_release_field("NAME");
    let image = read_os_release_field("IMAGE_NAME");
    if let (Some(n), Some(i)) = (name.as_deref(), image.as_deref()) {
        let capped = capitalise_first(i);
        if !n.eq_ignore_ascii_case(&capped) && !n.contains(capped.as_str()) {
            return Some(format!("{} {}", n, capped));
        }
    }

    if let Some(pretty) = read_os_release_field("PRETTY_NAME") {
        return Some(pretty);
    }

    if let Some((title, _, _)) = detect_bootc_image_info() {
        return Some(title);
    }

    if let Some(id) = read_os_release_field("IMAGE_ID") {
        if let Some(var) = read_os_release_field("VARIANT_ID") {
            return Some(format!("{}  ·  {}", id, var));
        }
        return Some(id);
    }
    None
}

/// Title-case the first character of `s`, leave the rest unchanged.
/// Used to turn `dakota` → `Dakota` for the hero-row display.
pub(super) fn capitalise_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().chain(chars).collect(),
    }
}

pub(super) fn read_os_release_field(key: &str) -> Option<String> {
    // When in a Flatpak, use flatpak-spawn to read the host's os-release
    if crate::update_worker::is_flatpak() {
        if let Ok(output) = std::process::Command::new("flatpak-spawn")
            .args(["--host", "cat", "/etc/os-release"])
            .output()
        {
            if output.status.success() {
                if let Ok(content) = String::from_utf8(output.stdout) {
                    if let Some(v) = parse_os_release_field(&content, key) {
                        return Some(v);
                    }
                }
            }
        }
    }

    // Fall back to direct file reading
    for path in &[
        "/run/host/os-release",
        "/run/host/etc/os-release",
        "/run/host/usr/lib/os-release",
        "/etc/os-release",
        "/usr/lib/os-release",
    ] {
        if let Ok(content) = std::fs::read_to_string(path) {
            if let Some(v) = parse_os_release_field(&content, key) {
                return Some(v);
            }
        }
    }
    None
}

/// Pure-function counterpart of [`read_os_release_field`] — extracted so
/// the key=value parsing is unit-testable without filesystem fixtures.
/// Strips surrounding double-quotes; rejects empty values; first match wins.
pub(super) fn parse_os_release_field(content: &str, key: &str) -> Option<String> {
    let prefix = format!("{}=", key);
    for line in content.lines() {
        if let Some(v) = line.strip_prefix(prefix.as_str()) {
            let val = v.trim_matches('"').to_string();
            if !val.is_empty() {
                return Some(val);
            }
        }
    }
    None
}

/// Build a short subtitle for the hero row from bootc-status JSON:
/// "VERSION · sha1234567" when both are available, just one when only one is.
/// Per user direction this is more informative than "Booted N days ago".
pub(super) fn read_booted_image_summary() -> Option<String> {
    if let Some(mock) = Settings::load().mock_identity {
        let full_ref = mock.full_ref();
        let j = serde_json::json!({
            "status": {
                "booted": {
                    "image": {
                        "image": { "image": full_ref },
                        "imageDigest": mock.digest,
                        "timestamp": mock.booted_at
                    }
                }
            }
        });
        return parse_booted_image_summary(&j);
    }
    let json = get_cached_bootc_status()?;
    parse_booted_image_summary(&json)
}

/// Pure-function counterpart of [`read_booted_image_summary`] — extracted
/// for unit testing without spawning `bootc status --json`. Returns a
/// two-line subtitle:
///
/// ```text
/// ghcr.io/projectbluefin/dakota:latest
/// bc6d66c9 · 2026-05-30
/// ```
///
/// Image ref on line 1 (so the user can read the full identity);
/// short digest + build date on line 2 (small, dimmable in the row CSS).
/// The hero ActionRow needs `set_subtitle_lines(2)` to render both lines.
/// Missing pieces are skipped — the function never returns an empty
/// trailing " · " or a dangling newline.
pub(super) fn parse_booted_image_summary(json: &Value) -> Option<String> {
    let booted = json.pointer("/status/booted")?;
    let image_ref = booted
        .pointer("/image/image/image")
        .or_else(|| booted.pointer("/image/image"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let digest = booted
        .pointer("/image/imageDigest")
        .and_then(|v| v.as_str())
        .and_then(|s| s.strip_prefix("sha256:").or(Some(s)))
        .map(|s| s.chars().take(8).collect::<String>());
    let date = booted
        .pointer("/image/timestamp")
        .and_then(|v| v.as_str())
        .filter(|s| s.len() >= 10)
        .map(|s| s[..10].to_string());

    // Compose line 2 ("digest · date") from whichever pieces we have.
    let mut line2_parts: Vec<String> = Vec::new();
    if let Some(d) = digest {
        line2_parts.push(d);
    }
    if let Some(t) = date {
        line2_parts.push(t);
    }
    let line2 = line2_parts.join(" · ");

    match (image_ref, line2.is_empty()) {
        (Some(r), true) => Some(r),
        (Some(r), false) => Some(format!("{}\n{}", r, line2)),
        (None, false) => Some(line2),
        (None, true) => None,
    }
}

/// Extract the booted image's tag suffix (everything after the final `:`) from
/// the cached bootc-status JSON — e.g. `stable-daily-43.20260530`. Used to
/// pair the booted build with its entry in `registry_versions` (whose
/// `version` field is the `org.opencontainers.image.version` annotation, in
/// practice equal to the dated tag for Universal Blue images).
///
/// Falls back to os-release fields when bootc-status fails. Dakota's
/// composefs deployment hits a "Multiple extra entries in /boot" error in
/// `bootc status` which makes the JSON path unavailable; os-release still
/// carries the booted build identity via `IMAGE_VERSION` / `VERSION_ID`.
pub(super) fn read_booted_tag_suffix() -> Option<String> {
    if let Some(mock) = Settings::load().mock_identity {
        return Some(mock.tag);
    }
    if let Some(json) = get_cached_bootc_status() {
        if let Some(t) = parse_booted_tag_suffix(&json) {
            return Some(t);
        }
    }
    // os-release fallback. IMAGE_VERSION is more specific (Dakota writes
    // "20260530"), VERSION_ID is the broader handle (same value on Dakota,
    // version-id only on Bluefin). Either one anchors the booted entry in
    // registry_versions through find_booted_match's date/substring lookup.
    read_os_release_field("IMAGE_VERSION").or_else(|| read_os_release_field("VERSION_ID"))
}

/// Pure-function counterpart of [`read_booted_tag_suffix`] — extracted for
/// unit testing.
pub(super) fn parse_booted_tag_suffix(json: &Value) -> Option<String> {
    let img = json
        .pointer("/status/booted/image/image/image")
        .or_else(|| json.pointer("/status/booted/image/image"))
        .and_then(|v| v.as_str())?;
    let (_, tag) = img.rsplit_once(':')?;
    if tag.is_empty() {
        return None;
    }
    Some(tag.to_string())
}

/// Match the booted anchor string against the registry version list with
/// progressive fallbacks. Anchors come from `read_booted_tag_suffix`, which
/// may yield:
/// - A fully-qualified Bluefin tag: `stable-daily-43.20260602`
/// - A bare date string from Dakota's os-release: `20260530`
/// - A floating stream tag: `latest`
///
/// Strategy:
/// 1. Exact `v.version == anchor` — direct hit for Bluefin.
/// 2. `v.version.contains(anchor)` — handles Dakota where the booted side
///    is `20260530` and the registry side is `latest.20260530`.
/// 3. `v.date == parsed_date(anchor)` — last resort when the version string
///    diverges entirely (Dakota's commit-sha tags annotate as `latest`).
pub(super) fn find_booted_match<'a>(
    versions: &'a [ImageVersion],
    anchor: &str,
) -> Option<&'a ImageVersion> {
    if let Some(v) = versions.iter().find(|v| v.version == anchor) {
        return Some(v);
    }
    if let Some(v) = versions.iter().find(|v| v.version.contains(anchor)) {
        return Some(v);
    }
    if let Some(date) = extract_yyyymmdd_date(anchor) {
        return versions.iter().find(|v| v.date == date);
    }
    None
}

/// Find an embedded YYYYMMDD date anywhere in a tag/anchor string. Accepts
/// `20260530`, `latest.20260530`, `stable-daily-43.20260602`, etc. Returns
/// None when no 8-digit run parses as a valid Gregorian date.
pub(super) fn extract_yyyymmdd_date(s: &str) -> Option<chrono::NaiveDate> {
    let bytes = s.as_bytes();
    if bytes.len() < 8 {
        return None;
    }
    for i in 0..=(bytes.len() - 8) {
        let slice = &bytes[i..i + 8];
        if slice.iter().all(|b| b.is_ascii_digit()) {
            // Reject if preceded/followed by another digit — that means the
            // run is longer than 8 and we'd be slicing through it.
            let prev_ok = i == 0 || !bytes[i - 1].is_ascii_digit();
            let next_ok = i + 8 == bytes.len() || !bytes[i + 8].is_ascii_digit();
            if prev_ok && next_ok {
                if let Ok(d) =
                    chrono::NaiveDate::parse_from_str(std::str::from_utf8(slice).ok()?, "%Y%m%d")
                {
                    return Some(d);
                }
            }
        }
    }
    None
}

/// One row in the changelog "Stack" diff: a labelled component (Image,
/// Kernel, Revision, Build) plus its current and target value. `bumped` lets
/// the renderer flag rows whose value actually changes, so the user can
/// scan the section and see at a glance which components moved.
#[derive(Debug, Clone)]
pub(super) struct StackItem {
    pub(super) label: &'static str,
    pub(super) current: Option<String>,
    pub(super) target: String,
    pub(super) bumped: bool,
}

/// Build the rows for the changelog Stack section. Compares the booted
/// `ImageVersion` against the selected target and emits a labelled
/// from→to row for each meaningful component (image tag, kernel, git
/// revision, build date). Returns an empty Vec when there's no target
/// to compare against — caller suppresses the whole section in that
/// case.
///
/// `host_kernel` is the live `uname -r` for the booted host; used to fill
/// in the booted side of the Kernel row when the registry-side annotation
/// is missing. Dakota doesn't publish `ostree.linux` anywhere accessible,
/// so without this the booted kernel would render as "—" even though we
/// know it from the running system.
pub(super) fn build_stack_items(
    booted: Option<&ImageVersion>,
    target: Option<&ImageVersion>,
    host_kernel: Option<&str>,
) -> Vec<StackItem> {
    let Some(target) = target else {
        return Vec::new();
    };

    let mut out = Vec::new();

    // Image tag — what the user is on vs. what they're going to. Use the
    // version annotation (typically the dated tag) so the value stays
    // short enough to render in the row suffix. Full refs would overflow.
    let image_bumped = booted.map(|b| b.version != target.version).unwrap_or(true);
    out.push(StackItem {
        label: "Image",
        current: booted.map(|b| b.version.clone()),
        target: target.version.clone(),
        bumped: image_bumped,
    });

    // Kernel — the headline number for a bootc update. Only emit the row
    // when at least one side has a value; Dakota doesn't ship kernel data
    // for either side, and a "— → —" row is just noise.
    let target_kernel = if target.kernel.is_empty() {
        None
    } else {
        Some(target.kernel.clone())
    };
    let current_kernel = booted
        .map(|b| b.kernel.clone())
        .filter(|k| !k.is_empty())
        .or_else(|| host_kernel.map(|s| s.to_string()).filter(|s| !s.is_empty()));
    if target_kernel.is_some() || current_kernel.is_some() {
        let kernel_bumped = match (&current_kernel, &target_kernel) {
            (Some(c), Some(t)) => c != t,
            // We know the target but not the booted side → "new" value the
            // user would land on. Matches the Image/Revision/Built rows'
            // behaviour when booted info is missing.
            (None, Some(_)) => true,
            // Target side missing (Dakota: no kernel anywhere) — current is
            // just informational, not a change. Keep it dim.
            _ => false,
        };
        out.push(StackItem {
            label: "Kernel",
            current: current_kernel,
            target: target_kernel.unwrap_or_default(),
            bumped: kernel_bumped,
        });
    }

    // Short git commit — the actual code that built the image. Skipped on
    // images that don't publish `image.revision` (some Dakota builds).
    if !target.revision.is_empty() {
        let target_rev = short_sha(&target.revision);
        let current_rev = booted
            .map(|b| short_sha(&b.revision))
            .filter(|s| !s.is_empty());
        let rev_bumped = current_rev
            .as_deref()
            .map(|c| c != target_rev)
            .unwrap_or(true);
        out.push(StackItem {
            label: "Revision",
            current: current_rev,
            target: target_rev,
            bumped: rev_bumped,
        });
    }

    // Build date — surfaces "how recent" without making the user parse a
    // 40-char manifest digest.
    let target_built = target.created.format("%b %-d, %Y").to_string();
    let current_built = booted.map(|b| b.created.format("%b %-d, %Y").to_string());
    let built_bumped = current_built
        .as_deref()
        .map(|c| c != target_built)
        .unwrap_or(true);
    out.push(StackItem {
        label: "Built",
        current: current_built,
        target: target_built,
        bumped: built_bumped,
    });

    out
}

pub(super) fn short_sha(s: &str) -> String {
    if s.len() >= 7 {
        s[..7].to_string()
    } else {
        s.to_string()
    }
}

/// How the hero row should render the distro logo.
///
/// Two arms because the branded asset is usually *not* reachable through the
/// icon theme. Bluefin Dakota is the motivating case: os-release says
/// `LOGO=img-logo-icon` and the file really is installed, but as
/// `/usr/share/pixmaps/img-logo-icon.png`. GTK3 searched `/usr/share/pixmaps`
/// as a legacy icon-theme fallback; GTK4 dropped that. So `has_icon()` says
/// no, the chain falls all the way through to `computer-symbolic`, and the
/// most prominent row in the app renders a generic monitor glyph on a machine
/// that ships its own logo. A themed name alone cannot express "this file".
pub(super) enum HeroLogo {
    /// Resolvable through `gtk::IconTheme` — symbolic, recolourable.
    Themed(String),
    /// A full-colour bitmap/SVG on disk that the theme cannot see.
    File(PathBuf),
}

/// Directories to search for a `LOGO=` asset, host view first.
///
/// The `/run/host` prefixes are the Flatpak view of the host filesystem,
/// granted by `--filesystem=host-os:ro` in the manifest. Inside a Flatpak the
/// unprefixed paths belong to the *runtime*, which has no distro branding, so
/// order matters: host first, sandbox second.
const LOGO_SEARCH_ROOTS: &[&str] = &[
    "/run/host/usr/share/pixmaps",
    "/run/host/usr/share/icons/hicolor/scalable/apps",
    "/usr/share/pixmaps",
    "/usr/share/icons/hicolor/scalable/apps",
];

/// Look for `<logo>.svg` / `<logo>.png` under each root, in order.
///
/// Pure over its inputs so it can be tested without a display — which is the
/// half that was broken. The `IconTheme` half needs a GDK display and stays
/// untested; it was never the part that failed.
///
/// SVG is tried before PNG at each root because the hero icon renders at 32px
/// and the pixmaps PNGs are frequently 48px or smaller.
///
/// The `-dark` variants some distros ship alongside (Dakota has
/// `img-logo-icon-dark.png`) are deliberately ignored: honouring them means
/// tracking `AdwStyleManager::is-dark` and swapping at runtime, and a logo
/// that is right at launch but wrong after a theme toggle is worse than one
/// that is merely plain. Deferred, not overlooked.
pub(super) fn resolve_logo_file<P: AsRef<Path>>(roots: &[P], logo: &str) -> Option<PathBuf> {
    if logo.is_empty() {
        return None;
    }
    for root in roots {
        for ext in ["svg", "png"] {
            let candidate = root.as_ref().join(format!("{logo}.{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Parse every `LOGO=` value visible to us, most-authoritative first.
fn read_logo_names() -> Vec<String> {
    let mut names = Vec::new();
    // `/run/host/os-release` is mounted into *every* Flatpak with no
    // permission needed; the `/run/host/etc` path additionally requires
    // host-os. Try both so the logo still resolves if the manifest is
    // tightened later.
    for path in &[
        "/run/host/os-release",
        "/run/host/etc/os-release",
        "/etc/os-release",
    ] {
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        for line in content.lines() {
            if let Some(v) = line.strip_prefix("LOGO=") {
                let logo = v.trim_matches('"').to_string();
                if !logo.is_empty() && !names.contains(&logo) {
                    names.push(logo);
                }
            }
        }
    }
    names
}

pub(super) fn read_logo_icon_name() -> HeroLogo {
    let branded = read_logo_names();

    // Tier 1: the icon theme, for distros that install their logo properly.
    if let Some(display) = gtk::gdk::Display::default() {
        let theme = gtk::IconTheme::for_display(&display);
        for name in &branded {
            if theme.has_icon(name) {
                return HeroLogo::Themed(name.clone());
            }
        }
    }

    // Tier 2: the file the theme can't see. This is the arm that fires on
    // Bluefin/Dakota.
    for name in &branded {
        if let Some(path) = resolve_logo_file(LOGO_SEARCH_ROOTS, name) {
            return HeroLogo::File(path);
        }
    }

    // Fallbacks. `distributor-logo-symbolic` is the freedesktop spec name;
    // `computer-symbolic` is guaranteed by Adwaita, so the row is never blank.
    if let Some(display) = gtk::gdk::Display::default() {
        let theme = gtk::IconTheme::for_display(&display);
        if theme.has_icon("distributor-logo-symbolic") {
            return HeroLogo::Themed("distributor-logo-symbolic".to_string());
        }
    }
    HeroLogo::Themed("computer-symbolic".to_string())
}

pub(super) static BOOTC_STATUS_CACHE: Mutex<Option<Value>> = Mutex::new(None);

pub(super) fn get_cached_bootc_status() -> Option<Value> {
    // Mock identity wins over real bootc status — tests don't want to spawn
    // a privileged subprocess (and the cached result would lie about which
    // image is "booted"). Cache stays empty when mocked.
    if Settings::load().mock_identity.is_some() {
        return None;
    }

    {
        let cache = BOOTC_STATUS_CACHE.lock().unwrap();
        if cache.is_some() {
            return cache.clone();
        }
    }

    use std::time::Duration;

    let run_cmd = |cmd_path: &str, args: &[&str]| -> Option<std::process::Output> {
        let mut cmd = Command::new(cmd_path);
        cmd.args(args);
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::null());
        let mut child = cmd.spawn().ok()?;
        let start = std::time::Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    if status.success() {
                        return child.wait_with_output().ok();
                    } else {
                        return None;
                    }
                }
                Ok(None) if start.elapsed() > Duration::from_secs(3) => {
                    let _ = child.kill();
                    return None;
                }
                _ => {
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
    };

    let cmd_path = if crate::update_worker::is_flatpak() {
        "flatpak-spawn"
    } else {
        "bootc"
    };
    let args: &[&str] = if crate::update_worker::is_flatpak() {
        &["--host", "bootc", "status", "--json"]
    } else {
        &["status", "--json"]
    };

    let output = if let Some(out) = run_cmd(cmd_path, args) {
        out
    } else {
        let cmd_path_pk = if crate::update_worker::is_flatpak() {
            "flatpak-spawn"
        } else {
            "pkexec"
        };
        let args_pk: &[&str] = if crate::update_worker::is_flatpak() {
            &["--host", "pkexec", "bootc", "status", "--json"]
        } else {
            &["bootc", "status", "--json"]
        };
        run_cmd(cmd_path_pk, args_pk)?
    };

    let json: Value = serde_json::from_slice(&output.stdout).ok()?;
    let mut cache = BOOTC_STATUS_CACHE.lock().unwrap();
    *cache = Some(json.clone());
    Some(json)
}

/// Memoised result of [`detect_bootc_image_info`].
///
/// Without this the function is pathologically expensive. `read_selected_tag()`
/// is called from per-row rendering code (`status_view.rs:422`, `:437`, `:470`)
/// — once per version row — and each call previously performed the *entire*
/// detection chain: build a tokio runtime, run `current_image()`, and shell out
/// to `bootc status`. Rendering the tag list for an image with hundreds of
/// published tags therefore tried to create hundreds of runtimes and threads,
/// which exhausted the process thread limit and killed the app with
/// "OS can't spawn worker thread: Resource temporarily unavailable".
///
/// The booted image cannot change while the process is running, so a single
/// resolution per process is correct. `Option<Option<_>>`: the outer layer is
/// "have we looked yet", the inner is "did detection succeed" — a failed
/// detection is cached too, otherwise every row would retry the failing
/// subprocess.
pub(super) static BOOTC_IMAGE_INFO_CACHE: std::sync::OnceLock<Option<(String, String, String)>> =
    std::sync::OnceLock::new();

pub(super) fn detect_bootc_image_info() -> Option<(String, String, String)> {
    BOOTC_IMAGE_INFO_CACHE
        .get_or_init(detect_bootc_image_info_uncached)
        .clone()
}

pub(super) fn detect_bootc_image_info_uncached() -> Option<(String, String, String)> {
    // Delegate to the UpdaterService. current_image() already encapsulates the
    // full precedence chain (mock_identity → FINUPDATE_IMAGE → bootc status →
    // os-release) inside RegistryClient::detect_with_settings, so this site
    // just transforms the resulting ImageRef into the (title, registry_uri,
    // selected_tag) triple the UI is shaped around.
    //
    // This must work from two different kinds of caller:
    //
    //   * the GTK main thread (UI construction — status_view.rs:1065, :422),
    //     which is not a tokio runtime, and
    //   * a tokio worker (the changelog fetch path reaches read_selected_tag
    //     at :4226 while already inside the runtime).
    //
    // Building a current-thread runtime inline works for the first and panics
    // for the second with "Cannot start a runtime from within a runtime",
    // which crashed the app on launch as soon as the changelog fetch raced UI
    // construction. Running the async call on a dedicated thread that owns its
    // own runtime is correct from either context.
    //
    // The cost is one short-lived thread per call, which is acceptable: this
    // sits behind BOOTC_STATUS_CACHE and is only hit while building UI.
    let image = std::thread::spawn(|| {
        crate::runtime::block_on(async { crate::service::global().current_image().await.ok() })
    })
    .join()
    .ok()
    .flatten()?;

    let title = format!("{}/{}", image.org, image.image);
    let registry_uri = format!("{}/{}/{}", image.registry, image.org, image.image);
    let selected_tag = strip_date_suffix(&image.tag).unwrap_or(image.tag);
    println!(
        "[debug] service::current_image: title='{}' registry_uri='{}' tag='{}'",
        title, registry_uri, selected_tag
    );
    Some((title, registry_uri, selected_tag))
}

#[derive(serde::Deserialize, Debug, Clone)]
pub(super) struct BootcImageInfoConfig {
    pub(super) tags: Vec<String>,
}

pub(super) fn read_bootc_image_info_config() -> Option<BootcImageInfoConfig> {
    let content = if crate::update_worker::is_flatpak() {
        let output = Command::new("flatpak-spawn")
            .args(["--host", "cat", "/etc/bootc-image-info.json"])
            .output()
            .ok()?;
        if output.status.success() {
            String::from_utf8(output.stdout).ok()
        } else {
            None
        }
    } else {
        std::fs::read_to_string("/etc/bootc-image-info.json").ok()
    }?;

    serde_json::from_str(&content).ok()
}

pub(super) fn strip_date_suffix(tag: &str) -> Option<String> {
    for sep in ['.', '-'] {
        if let Some(pos) = tag.rfind(sep) {
            let suffix = &tag[pos + 1..];
            if suffix.len() == 8 && suffix.chars().all(|c| c.is_ascii_digit()) {
                return Some(tag[..pos].to_string());
            }
        }
    }
    None
}

pub(super) fn read_registry_uri() -> Option<String> {
    detect_bootc_image_info().map(|(_, registry_uri, _)| registry_uri)
}

pub(super) fn read_selected_tag() -> String {
    detect_bootc_image_info()
        .map(|(_, _, tag)| tag)
        .unwrap_or_else(|| "latest".to_string())
}

/// True when `tag` is a specific pinned build rather than a floating stream.
///
/// A "stream" tag is one of the canonical channel names a Family publishes
/// (`latest`, `testing`, `stable`, `gts`, `beta`, `lts`, `lts-hwe`, etc.).
/// Anything else is treated as pinned: 8-digit date strings (`20260530`),
/// dotted date suffixes (`latest.20260530`), and 40-char sha hex tags.
///
/// Used to decide whether to show the "Unpin to {stream}" affordance on the
/// front page — if the user is pinned, they're not getting auto-updates and
/// need a one-click escape hatch.
pub(super) fn is_pinned_tag(tag: &str) -> bool {
    const STREAM_TAGS: &[&str] = &[
        "latest",
        "testing",
        "stable",
        "stable-daily",
        "beta",
        "gts",
        "lts",
        "lts-hwe",
        "lts-amd64",
        "lts-arm64",
        "gdx",
        "unstable",
    ];
    if STREAM_TAGS.contains(&tag) {
        return false;
    }
    // Sha-only (40-char hex) — definitely pinned.
    if tag.len() == 40 && tag.chars().all(|c| c.is_ascii_hexdigit()) {
        return true;
    }
    // 8-digit date string — pinned.
    if tag.len() == 8 && tag.chars().all(|c| c.is_ascii_digit()) {
        return true;
    }
    // Dotted/dashed date suffix on a stream tag — pinned.
    if strip_date_suffix(tag).is_some() {
        return true;
    }
    // Anything else (unrecognised tag) — treat as pinned for safety; user
    // can still click unpin to go back to the family's default stream.
    true
}

/// Try to determine when the last successful update ran.
pub(super) fn get_last_update_time() -> Option<String> {
    let paths = ["/var/lib/uupd/last-run", "/var/lib/uupd/.last-run"];

    for path in &paths {
        if let Ok(metadata) = std::fs::metadata(path) {
            if let Ok(modified) = metadata.modified() {
                let elapsed = modified.elapsed().ok()?;
                let hours = elapsed.as_secs() / 3600;
                if hours < 1 {
                    return Some("Last update: less than an hour ago".to_string());
                } else if hours < 24 {
                    return Some(format!("Last update: {} hours ago", hours));
                } else {
                    let days = hours / 24;
                    return Some(format!("Last update: {} days ago", days));
                }
            }
        }
    }

    if let Ok(metadata) = std::fs::metadata("/sysroot/ostree/deploy") {
        if let Ok(modified) = metadata.modified() {
            if let Ok(elapsed) = modified.elapsed() {
                let hours = elapsed.as_secs() / 3600;
                if hours < 24 {
                    return Some(format!("System deployed: {} hours ago", hours));
                } else {
                    let days = hours / 24;
                    return Some(format!("System deployed: {} days ago", days));
                }
            }
        }
    }

    None
}

pub(super) fn parse_image_ref_fields(img_ref: &str) -> (String, String, String) {
    if img_ref.is_empty() {
        return (
            "Unknown".to_string(),
            "latest".to_string(),
            "unknown".to_string(),
        );
    }
    let (without_tag, tag) = img_ref.rsplit_once(':').unwrap_or((img_ref, "latest"));
    let parts: Vec<&str> = without_tag.split('/').collect();
    let name = parts
        .last()
        .map(|s| s.to_string())
        .unwrap_or_else(|| without_tag.to_string());
    let org = if parts.len() >= 2 {
        parts[parts.len() - 2].to_string()
    } else {
        "unknown".to_string()
    };
    (name, tag.to_string(), org)
}

pub(super) fn get_real_deployments_from_json(json: &Value) -> Option<Vec<MockDeployment>> {
    let mut ds = Vec::new();
    let status = json.get("status")?;
    let booted_kernel = get_host_kernel();

    // 1. Staged deployment
    if let Some(staged) = status
        .get("staged")
        .and_then(|v| if v.is_null() { None } else { Some(v) })
    {
        let img_ref = staged
            .pointer("/image/image/image")
            .or_else(|| staged.pointer("/image/image"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let digest = staged
            .pointer("/image/imageDigest")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let timestamp = staged
            .pointer("/image/timestamp")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let (name, tag, org) = parse_image_ref_fields(img_ref);
        let date_str = if timestamp.len() >= 10 {
            &timestamp[0..10]
        } else {
            "recently"
        };

        ds.push(MockDeployment {
            id: "d-staged".to_string(),
            state: "staged".to_string(),
            title: name,
            image: img_ref.to_string(),
            tag,
            digest: digest.to_string(),
            deployed: "Staged · pending reboot".to_string(),
            deployed_full: format!("Built: {}", date_str),
            size: "—".to_string(),
            kernel: "—".to_string(),
            package_count: 0,
            signer: format!("{} (sigstore)", org),
            pinned: false,
        });
    }

    // 2. Booted deployment
    if let Some(booted) = status
        .get("booted")
        .and_then(|v| if v.is_null() { None } else { Some(v) })
    {
        let img_ref = booted
            .pointer("/image/image/image")
            .or_else(|| booted.pointer("/image/image"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let digest = booted
            .pointer("/image/imageDigest")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let timestamp = booted
            .pointer("/image/timestamp")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let pinned = booted
            .get("pinned")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let (name, tag, org) = parse_image_ref_fields(img_ref);
        let date_str = if timestamp.len() >= 10 {
            &timestamp[0..10]
        } else {
            "recently"
        };

        ds.push(MockDeployment {
            id: "d-current".to_string(),
            state: "current".to_string(),
            title: name,
            image: img_ref.to_string(),
            tag,
            digest: digest.to_string(),
            deployed: "Currently booted".to_string(),
            deployed_full: format!("Built: {}", date_str),
            size: "—".to_string(),
            kernel: booted_kernel,
            package_count: 0,
            signer: format!("{} (sigstore)", org),
            pinned,
        });
    }

    // 3. Rollback deployment
    if let Some(rollback) = status
        .get("rollback")
        .and_then(|v| if v.is_null() { None } else { Some(v) })
    {
        let img_ref = rollback
            .pointer("/image/image/image")
            .or_else(|| rollback.pointer("/image/image"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let digest = rollback
            .pointer("/image/imageDigest")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let timestamp = rollback
            .pointer("/image/timestamp")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let pinned = rollback
            .get("pinned")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let (name, tag, org) = parse_image_ref_fields(img_ref);
        let date_str = if timestamp.len() >= 10 {
            &timestamp[0..10]
        } else {
            "recently"
        };

        ds.push(MockDeployment {
            id: "d-rollback".to_string(),
            state: "previous".to_string(),
            title: name,
            image: img_ref.to_string(),
            tag,
            digest: digest.to_string(),
            deployed: "Rollback target".to_string(),
            deployed_full: format!("Built: {}", date_str),
            size: "—".to_string(),
            kernel: "—".to_string(),
            package_count: 0,
            signer: format!("{} (sigstore)", org),
            pinned,
        });
    }

    if ds.is_empty() { None } else { Some(ds) }
}

pub(super) fn get_real_deployments() -> Option<Vec<MockDeployment>> {
    get_cached_bootc_status().and_then(|json| get_real_deployments_from_json(&json))
}

pub(super) fn get_host_kernel() -> String {
    let output = if crate::update_worker::is_flatpak() {
        Command::new("flatpak-spawn")
            .args(["--host", "uname", "-r"])
            .output()
    } else {
        Command::new("uname").arg("-r").output()
    };
    output
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "—".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression guard for the hero row rendering a generic monitor glyph on
    /// a machine that ships its own logo.
    ///
    /// Bluefin Dakota sets `LOGO=img-logo-icon` and installs
    /// `/usr/share/pixmaps/img-logo-icon.png`. GTK4 removed GTK3's legacy
    /// `/usr/share/pixmaps` icon-theme fallback, so the themed lookup misses
    /// and the branded asset has to be found on disk instead.
    #[test]
    fn resolves_a_pixmaps_logo_the_icon_theme_cannot_see() {
        let dir = tempfile::tempdir().unwrap();
        let pixmaps = dir.path().join("pixmaps");
        std::fs::create_dir_all(&pixmaps).unwrap();
        std::fs::write(pixmaps.join("img-logo-icon.png"), b"\x89PNG").unwrap();

        let found = resolve_logo_file(&[&pixmaps], "img-logo-icon");
        assert_eq!(found, Some(pixmaps.join("img-logo-icon.png")));
    }

    /// SVG wins over PNG at the same root: the hero icon renders at 32px and
    /// the pixmaps PNGs are routinely smaller than that.
    #[test]
    fn prefers_svg_over_png() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("logo.png"), b"png").unwrap();
        std::fs::write(dir.path().join("logo.svg"), b"<svg/>").unwrap();

        assert_eq!(
            resolve_logo_file(&[dir.path()], "logo"),
            Some(dir.path().join("logo.svg"))
        );
    }

    /// Roots are searched in order, so the host view (`/run/host/...`) beats
    /// the Flatpak runtime's own unbranded copy.
    #[test]
    fn earlier_roots_win() {
        let dir = tempfile::tempdir().unwrap();
        let host = dir.path().join("host");
        let sandbox = dir.path().join("sandbox");
        std::fs::create_dir_all(&host).unwrap();
        std::fs::create_dir_all(&sandbox).unwrap();
        std::fs::write(host.join("logo.png"), b"host").unwrap();
        std::fs::write(sandbox.join("logo.png"), b"sandbox").unwrap();

        assert_eq!(
            resolve_logo_file(&[&host, &sandbox], "logo"),
            Some(host.join("logo.png"))
        );
    }

    /// An empty or absent `LOGO=` must not turn into a bare-extension probe
    /// for `".svg"`, and a directory named like the logo is not an icon.
    #[test]
    fn rejects_empty_logo_and_directories() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(resolve_logo_file(&[dir.path()], ""), None);

        std::fs::create_dir(dir.path().join("logo.svg")).unwrap();
        assert_eq!(resolve_logo_file(&[dir.path()], "logo"), None);
    }
}
