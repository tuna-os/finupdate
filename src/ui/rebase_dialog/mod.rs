//! Rebase history dialog — lets the user rebase to any OS image from the
//! last 90 days via a calendar grid.
//!
//! Entry point: [`show_rebase_dialog`] — opens a modal `adw::Dialog`.
//!
//! ## Flow
//!
//! ```text
//! show_rebase_dialog()
//!   └── spawn background thread
//!         └── RegistryClient::detect() → fetch_versions(90)
//!               → result slot + timeout poll on UI thread
//!                    ├── Success → show calendar + details panel
//!                    └── Error   → show error page with retry
//! ```
//!
//! When the user picks a date and confirms, `bootc switch {full_ref}` is
//! run on the host via the same `flatpak-spawn --host pkexec` pattern as
//! the main update worker.

use adw::prelude::*;
use gtk::glib;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::service::{self, FamilyInfo};

mod calendar;
mod execution;
mod fetch;
mod switches;

#[cfg(test)]
mod tests;

use calendar::build_loaded_page;
use fetch::{INITIAL_FETCH_COUNT, start_version_fetch};
use switches::populate_family_switches;

/// Callback invoked when the user clicks "See changelog" inside the rebase
/// dialog's selected-version panel. Receives the version tag; the app uses it
/// to navigate the main view to the What's New page filtered to that tag.
pub type OnShowChangelog = Rc<dyn Fn(String)>;

/// Open the rebase history dialog as a child of `parent`.
pub fn show_rebase_dialog(parent: &gtk::Widget, on_show_changelog: OnShowChangelog) {
    let dialog = adw::Dialog::builder()
        .title("Pin to a Previous Build")
        .content_width(520)
        // 720 + tighter day cells fits variants + calendar + details + buttons
        // without the inner ScrolledWindow needing to scroll on typical
        // laptop screens (≥900px tall). See inject_calendar_css below for
        // the matching cell-size reduction.
        .content_height(720)
        .build();

    let toolbar_view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    toolbar_view.add_top_bar(&header);

    // ── Feature-switch variant selector ───────────────────────────────────
    //
    // Replaces the hardcoded Dakota/Dakota-Nvidia pair with one SwitchRow
    // per atomic feature available in the current Family — derived live from
    // the Family taxonomy (KNOWN_FAMILIES). The user picks Nvidia / DX / HWE
    // etc. by name rather than choosing a raw image; the resolved target
    // image is shown in a preview row at the bottom of the group.
    //
    // The legacy `variant_state: Rc<RefCell<String>>` interface is preserved
    // for the existing `start_version_fetch` API — we feed it an empty string
    // when the user hasn't picked features (no extra filter), so the loaded
    // page renders the full base-image history.
    let variant_state = Rc::new(RefCell::new(String::new()));
    let selected_features: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let selected_stream: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
    let current_family: Rc<RefCell<Option<FamilyInfo>>> = Rc::new(RefCell::new(None));
    // Booted image — populated by populate_family_switches's background
    // detect, consumed by build_loaded_page so the rebase button can label
    // itself "Switch to :testing" when the user has changed the stream
    // away from whatever they're actually booted on (rather than only
    // labelling for date-pinned actions).
    let booted_image: Rc<RefCell<Option<service::ImageRef>>> = Rc::new(RefCell::new(None));

    let variant_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    variant_box.set_margin_start(16);
    variant_box.set_margin_end(16);
    variant_box.set_margin_top(12);
    variant_box.set_margin_bottom(12);

    let family_label = gtk::Label::new(Some("Loading family info…"));
    family_label.set_halign(gtk::Align::Start);
    family_label.add_css_class("heading");
    variant_box.append(&family_label);

    // Stream selector (populated once family is detected)
    let stream_row = adw::ComboRow::builder().title("Stream").build();
    let stream_group = adw::PreferencesGroup::new();
    stream_group.add(&stream_row);
    variant_box.append(&stream_group);

    // PreferencesGroup hosts the dynamic SwitchRow list. Populated once the
    // initial fetch completes and we know which family we're on.
    let features_group = adw::PreferencesGroup::new();
    variant_box.append(&features_group);

    let target_image_row = adw::ActionRow::builder()
        .title("Target image")
        .subtitle("(select features above)")
        .build();
    let target_chip = gtk::Image::from_icon_name("emblem-default-symbolic");
    target_chip.add_css_class("dim-label");
    target_image_row.add_suffix(&target_chip);
    let target_image_group = adw::PreferencesGroup::new();
    target_image_group.add(&target_image_row);
    variant_box.append(&target_image_group);

    // ── Stack: loaded / error ──────────────────────────────────────────
    // No separate "loading" page anymore — we render the calendar UI
    // immediately on dialog open (with a small inline "Loading builds…"
    // indicator) and rebuild it when the registry fetch returns. Skipping
    // the full-screen status page means the user sees the dialog chrome
    // and stream/feature toggles right away even on a slow connection.
    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .transition_duration(200)
        .build();

    // Error page
    let retry_button = gtk::Button::builder()
        .label("Retry")
        .halign(gtk::Align::Center)
        .build();
    let error_page = adw::StatusPage::builder()
        .icon_name("network-error-symbolic")
        .title("Couldn't Load Versions")
        .description("Check your internet connection and try again.")
        .build();
    error_page.set_child(Some(&retry_button));
    stack.add_named(&error_page, Some("error"));

    // Loaded page — built dynamically once data arrives
    let loaded_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let loaded_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .build();
    loaded_scroll.set_child(Some(&loaded_box));
    stack.add_named(&loaded_scroll, Some("loaded"));

    let main_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    main_box.append(&variant_box);
    let separator = gtk::Separator::new(gtk::Orientation::Horizontal);
    main_box.append(&separator);
    main_box.append(&stack);
    toolbar_view.set_content(Some(&main_box));
    dialog.set_child(Some(&toolbar_view));
    // Render an empty calendar immediately so the user has visual feedback
    // while the registry probe runs. The `is_loading` flag drives the
    // inline "Loading builds…" indicator below the grid.
    build_loaded_page(
        &loaded_box,
        &stack,
        &dialog,
        parent,
        Vec::new(),
        current_family.clone(),
        selected_features.clone(),
        selected_stream.clone(),
        booted_image.clone(),
        None,
        on_show_changelog.clone(),
        true,
    );
    stack.set_visible_child_name("loaded");

    let stack_for_retry = stack.clone();
    let loaded_box_for_retry = loaded_box.clone();
    let dialog_for_retry = dialog.clone();
    let parent_for_retry = parent.clone();
    let error_page_for_retry = error_page.clone();
    let variant_state_for_retry = variant_state.clone();
    let current_family_for_retry = current_family.clone();
    let selected_features_for_retry = selected_features.clone();
    let on_show_changelog_for_retry = on_show_changelog.clone();
    let selected_stream_for_retry = selected_stream.clone();
    let booted_image_for_retry = booted_image.clone();
    retry_button.connect_clicked(move |_| {
        let variant = variant_state_for_retry.borrow().clone();
        start_version_fetch(
            stack_for_retry.clone(),
            loaded_box_for_retry.clone(),
            dialog_for_retry.clone(),
            parent_for_retry.clone(),
            error_page_for_retry.clone(),
            &variant,
            current_family_for_retry.clone(),
            selected_features_for_retry.clone(),
            selected_stream_for_retry.clone(),
            booted_image_for_retry.clone(),
            INITIAL_FETCH_COUNT,
            on_show_changelog_for_retry.clone(),
            None,
        );
    });

    // Family + feature switches are populated AFTER the initial fetch
    // completes (we need the detected RegistryClient to know which family
    // we're on). When the user flips DX/NVIDIA/stream, recompute resolves
    // the new target image and invokes `on_target_change` below, which
    // kicks off a fresh registry fetch so the calendar reflects builds for
    // THAT image rather than the booted one. Debounced via a generation
    // counter so a flurry of toggles in <300 ms only fires one fetch.
    let refetch_gen: Rc<Cell<u64>> = Rc::new(Cell::new(0));
    let on_target_change: Rc<dyn Fn(Option<service::ImageRef>)> = {
        let stack = stack.clone();
        let loaded_box = loaded_box.clone();
        let dialog = dialog.clone();
        let parent = parent.clone();
        let error_page = error_page.clone();
        let current_family = current_family.clone();
        let selected_features = selected_features.clone();
        let on_show_changelog = on_show_changelog.clone();
        let refetch_gen = refetch_gen.clone();
        let selected_stream = selected_stream.clone();
        let booted_image = booted_image.clone();
        Rc::new(move |target: Option<service::ImageRef>| {
            let this_gen = refetch_gen.get().wrapping_add(1);
            refetch_gen.set(this_gen);
            let stack = stack.clone();
            let loaded_box = loaded_box.clone();
            let dialog = dialog.clone();
            let parent = parent.clone();
            let error_page = error_page.clone();
            let current_family = current_family.clone();
            let selected_features = selected_features.clone();
            let on_show_changelog = on_show_changelog.clone();
            let refetch_gen = refetch_gen.clone();
            let selected_stream = selected_stream.clone();
            let booted_image = booted_image.clone();
            glib::timeout_add_local(std::time::Duration::from_millis(300), move || {
                if refetch_gen.get() != this_gen {
                    // Newer toggle already came in — drop this firing.
                    return glib::ControlFlow::Break;
                }
                start_version_fetch(
                    stack.clone(),
                    loaded_box.clone(),
                    dialog.clone(),
                    parent.clone(),
                    error_page.clone(),
                    "", // variant filter empty; target_override carries the image identity
                    current_family.clone(),
                    selected_features.clone(),
                    selected_stream.clone(),
                    booted_image.clone(),
                    INITIAL_FETCH_COUNT,
                    on_show_changelog.clone(),
                    target.clone(),
                );
                glib::ControlFlow::Break
            });
        })
    };

    populate_family_switches(
        &features_group,
        &family_label,
        &target_image_row,
        &stream_row,
        current_family.clone(),
        selected_features.clone(),
        selected_stream.clone(),
        booted_image.clone(),
        on_target_change,
    );

    dialog.present(Some(parent));
    let initial_variant = variant_state.borrow().clone();
    start_version_fetch(
        stack.clone(),
        loaded_box.clone(),
        dialog.clone(),
        parent.clone(),
        error_page.clone(),
        &initial_variant,
        current_family.clone(),
        selected_features.clone(),
        selected_stream.clone(),
        booted_image.clone(),
        INITIAL_FETCH_COUNT,
        on_show_changelog.clone(),
        None,
    );
}
