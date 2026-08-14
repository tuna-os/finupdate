//! Pure rendering helpers for the status view (version rows).
//!
//! Extracted from the status view module so the widget/state machinery
//! stays readable: these are pure GTK construction and CSS classification.

use adw::prelude::*;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Let a preferences row shrink below its full text width.
///
/// `AdwActionRow` defaults to `title-lines`/`subtitle-lines` of 0, which means
/// "never ellipsize" — the label reports the width of its entire string as its
/// minimum, and that propagates all the way up. Measured on the idle page, the
/// rows demanded 543–549px each, which forced the whole window to a 579px
/// minimum and made the HIG's 360px target unreachable no matter what
/// `width-request` said.
///
/// Note the sense of these properties: `title-lines` is the number of lines
/// *after which the label ellipsizes*, and **0 means unlimited** — i.e. the
/// label is free to wrap. Setting it to 1 does the opposite of what is wanted
/// here: it pins the label to a single line, whose minimum width is the whole
/// string.
pub(super) fn allow_narrow(row: &impl IsA<adw::ActionRow>) {
    let row = row.as_ref();
    row.set_title_lines(0);
    row.set_subtitle_lines(0);
}

/// Spawn the registry/changelog fetch on a background thread.
///
/// Every result is delivered with `input_sender().send(..)` rather than
/// `sender.input(..)`. The latter unwraps internally, so a fetch that finishes
/// after its component has been dropped panics the worker with "The runtime of
/// the component was shutdown. Maybe you accidentally dropped a controller?".
/// A late result arriving for a page the user has already navigated away from
/// is normal, not exceptional — dropping it silently is the correct behaviour.
///
/// A `current → target` version pair for the What's New Stack list.
///
/// Both call sites built this inline and identically; the duplication is why
/// the width fix below needed applying twice to be correct, so they share one
/// constructor now.
///
/// The ellipsizing is the point. These labels carry raw RPM versions like
/// `5:5.8.4-1.fc44`, and without a width cap they set the row's natural width,
/// which propagates up and forces the whole window wider than its 750px
/// request — values ended up clipped off-screen entirely. It only became
/// visible once the SBOM parser was fixed and the group had real content in
/// it; before that the Stack list held three short rows and nothing pushed.
///
/// `max_width_chars` is what makes ellipsizing actually bite: an ellipsized
/// label still *requests* its full natural width unless a cap is set, so
/// setting the mode alone would have changed nothing.
pub(super) const VERSION_MAX_CHARS: i32 = 18;

/// CSS class for the target version, from how it actually compares.
///
/// Green is a claim that the user is moving forward, so it is only made when
/// that is established. `bumped` alone means "differs", which painted an
/// entire rollback success-green: switching from Dakota's F44 to Bluefin's
/// F43 showed GNOME 50.3 → 49.7, bootc 1.16.3 → 1.15.1 and every other row in
/// upgrade colours while every package went backwards. A downgrade is not an
/// error, so it reads as `warning` rather than `error` — it is a thing the
/// user may well have chosen, they just need to see it for what it is.
pub(super) fn version_change_class(current: &str, target: &str, bumped: bool) -> &'static str {
    use finupdate_core::version_compare::{VersionChange, classify};
    match classify(current, target) {
        VersionChange::Upgrade => "success",
        VersionChange::Downgrade => "warning",
        VersionChange::Same => "dim-label",
        // Unparseable or one-sided — e.g. the Image/Revision/Built rows, whose
        // values are digests and dates rather than versions. Fall back to the
        // caller's differs/doesn't signal rather than inventing a direction.
        VersionChange::Unknown => {
            if bumped {
                "accent"
            } else {
                "dim-label"
            }
        }
    }
}

pub(super) fn version_diff_box(current: &str, target: &str, bumped: bool) -> gtk::Box {
    const MAX_CHARS: i32 = VERSION_MAX_CHARS;

    let diff_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    diff_box.set_valign(gtk::Align::Center);

    let from_lbl = gtk::Label::new(Some(current));
    from_lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
    from_lbl.set_max_width_chars(MAX_CHARS);
    from_lbl.add_css_class("monospace");
    from_lbl.add_css_class("caption");
    from_lbl.add_css_class("dim-label");
    // The full string stays reachable on hover, since ellipsizing hides the
    // release suffix that is often the only part that changed.
    from_lbl.set_tooltip_text(Some(current));
    diff_box.append(&from_lbl);

    let arrow_lbl = gtk::Label::new(Some("→"));
    arrow_lbl.add_css_class("dim-label");
    diff_box.append(&arrow_lbl);

    let to_lbl = gtk::Label::new(Some(target));
    to_lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
    to_lbl.set_max_width_chars(MAX_CHARS);
    to_lbl.add_css_class("monospace");
    to_lbl.add_css_class("caption");
    to_lbl.set_tooltip_text(Some(target));
    to_lbl.add_css_class(version_change_class(current, target, bumped));
    diff_box.append(&to_lbl);

    diff_box
}
