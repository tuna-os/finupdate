//! Rebase target resolution — pure decision logic for the rebase dialog.
//!
//! Extracted from `rebase_dialog.rs` (finupdate#30): computing the stream
//! switch action, resolving the DX/NVIDIA feature set against the service,
//! reverse-engineering the booted image's toggle state, and calendar math.
//! None of these touch GTK; they only need the `service` layer, so they are
//! unit-testable without the dialog's widget surface. The dialog's tests
//! keep exercising them through the re-exported names.

use chrono::NaiveDate;

use crate::service::{self, FamilyInfo};

/// selected. Returns `(label, sensitive, target_full_ref)`.
///
/// Three cases:
/// - **Currently installed** — selected stream + resolved variant match
///   what's booted. Button disabled, label "Currently Installed".
/// - **Switch action available** — stream or variant differs from booted.
///   Button enabled, label "Switch to :testing" / "Switch to dakota-nvidia",
///   target_ref points at the floating stream tag for the resolved image.
/// - **Indeterminate** — booted image unknown (bootc-status failed, no
///   os-release fallback). Button disabled, label asks the user to pick a
///   build instead.
pub(crate) fn compute_stream_switch_action(
    family: Option<&FamilyInfo>,
    selected_features: &[String],
    selected_stream: &str,
    booted: Option<&service::ImageRef>,
) -> (String, bool, Option<String>) {
    let Some(family) = family else {
        return ("Pin to this build…".to_string(), false, None);
    };
    if selected_stream.is_empty() {
        return ("Pin to this build…".to_string(), false, None);
    }
    let Some(target) = service::global().resolve_target(family, selected_features) else {
        return (
            "(combination doesn't match a published image)".to_string(),
            false,
            None,
        );
    };
    // Detect "no-op switch": booted is on the same image AND same stream.
    if let Some(b) = booted {
        if b.image == target.image && b.tag == selected_stream {
            return ("Currently Installed".to_string(), false, None);
        }
    }
    let full_ref = format!(
        "{}/{}/{}:{}",
        target.registry, target.org, target.image, selected_stream
    );
    // Prefer the short "Switch to :stream" wording when only the stream
    // moved; fall back to "Switch to image:stream" when the image name
    // changes too (variant toggle), so the user sees exactly what they're
    // committing to.
    let label = match booted {
        Some(b) if b.image == target.image => format!("Switch to :{}", selected_stream),
        _ => format!("Switch to {}:{}", target.image, selected_stream),
    };
    (label, true, Some(full_ref))
}

/// Compute the selected feature set + target image for the current toggle
/// state + selected stream. Uses the new resolve_target_with_stream API.
///
/// Similar fallback chain as resolve_dx_nvidia: prefers -nvidia-open,
/// falls back to -nvidia if needed.
///
/// Returns (selected_features, resolved_image). The stream is embedded in
/// the ImageRef's tag field.
pub(crate) fn resolve_dx_nvidia_with_stream(
    family: &FamilyInfo,
    dx_on: bool,
    nvidia_on: bool,
    stream: &str,
) -> (Vec<String>, Option<service::ImageRef>) {
    let svc = service::global();
    let base: Vec<String> = if dx_on {
        vec!["dx".to_string()]
    } else {
        vec![]
    };

    if nvidia_on {
        // Prefer the -open variant (current for Bluefin / Bluefin LTS).
        let mut with_open = base.clone();
        with_open.push("nvidia".to_string());
        with_open.push("open".to_string());
        if let Some(img) = svc.resolve_target_with_stream(family, &with_open, stream) {
            return (with_open, Some(img));
        }
        // Fall back to plain -nvidia (Bazzite / Dakota / Bluefin's
        // pre-migration variant the user might currently be booted on).
        let mut plain = base.clone();
        plain.push("nvidia".to_string());
        let img = svc.resolve_target_with_stream(family, &plain, stream);
        return (plain, img);
    }

    let img = svc.resolve_target_with_stream(family, &base, stream);
    (base, img)
}

/// Compute the selected feature set + target image for the current toggle
/// state. The fallback chain is what makes the single "NVIDIA drivers" switch
/// usable across the families:
///
///   nvidia on, prefer -nvidia-open first (current Bluefin / Bluefin LTS
///   convention) → fall back to plain -nvidia (Bazzite / deprecated Bluefin
///   variant). The user just toggles "NVIDIA"; we resolve to whichever
///   variant their family actually publishes.
///
/// Returns (selected_features, resolved_image). The features list flows into
/// the Rebase button click handler so the bootc-switch ref matches what the
/// preview shows.
#[allow(dead_code)]
pub(crate) fn resolve_dx_nvidia(
    family: &FamilyInfo,
    dx_on: bool,
    nvidia_on: bool,
) -> (Vec<String>, Option<service::ImageRef>) {
    let svc = service::global();
    let base: Vec<String> = if dx_on {
        vec!["dx".to_string()]
    } else {
        vec![]
    };

    if nvidia_on {
        // Prefer the -open variant (current for Bluefin / Bluefin LTS).
        let mut with_open = base.clone();
        with_open.push("nvidia".to_string());
        with_open.push("open".to_string());
        if let Some(img) = svc.resolve_target(family, &with_open) {
            return (with_open, Some(img));
        }
        // Fall back to plain -nvidia (Bazzite / Dakota / Bluefin's
        // pre-migration variant the user might currently be booted on).
        let mut plain = base.clone();
        plain.push("nvidia".to_string());
        let img = svc.resolve_target(family, &plain);
        return (plain, img);
    }

    let img = svc.resolve_target(family, &base);
    (base, img)
}

/// Reverse-engineer the toggle state from the user's booted image suffix so the
/// rebase dialog opens with the toggles matching reality.
///
/// Example: booted on `bluefin-dx-nvidia-open` with family base `bluefin` →
/// suffix `dx-nvidia-open` → `(dx=true, nvidia=true)`.
///
/// Returns `(false, false)` when the booted image is unknown, is the bare base,
/// or doesn't match the expected `{base}-{suffix}` shape. Conservative default —
/// if we can't be sure, show everything off rather than mislead the user about
/// what they're running.
pub(crate) fn derive_initial_toggle_state(
    family: &FamilyInfo,
    image: Option<&service::ImageRef>,
) -> (bool, bool) {
    let Some(image) = image else {
        return (false, false);
    };
    let Some(suffix) = image.image.strip_prefix(&format!("{}-", family.base_image)) else {
        // Bare base image (no suffix) or completely unrelated image name.
        return (false, false);
    };
    let parts: Vec<&str> = suffix.split('-').collect();
    let dx = parts.contains(&"dx");
    // Either `-nvidia` or `-nvidia-open` (or future `-open`-only variants) all
    // count as "NVIDIA on" from the user's mental model. resolve_dx_nvidia()
    // picks the right specific variant when they save.
    let nvidia = parts.contains(&"nvidia") || parts.contains(&"open");
    (dx, nvidia)
}

pub(crate) fn days_in_month(date: NaiveDate) -> u32 {
    let next = if date.month() == 12 {
        NaiveDate::from_ymd_opt(date.year() + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(date.year(), date.month() + 1, 1)
    };
    next.unwrap_or(date)
        .signed_duration_since(
            NaiveDate::from_ymd_opt(date.year(), date.month(), 1).unwrap_or(date),
        )
        .num_days() as u32
}
