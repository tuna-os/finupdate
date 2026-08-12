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
use chrono::{Datelike, Local, NaiveDate};
use gtk::glib;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use crate::registry_client::{ImageVersion, strip_date_suffix};
use crate::service::{self, FamilyInfo};
use crate::ui::bootc_progress::{BootcProgress, run_bootc_switch};
use crate::update_worker::is_flatpak;
use crate::ui::rebase_target::{
    compute_stream_switch_action, days_in_month, derive_initial_toggle_state, resolve_dx_nvidia,
    resolve_dx_nvidia_with_stream,
};

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

/// Initial number of versions fetched when the rebase dialog opens.
/// Powers the dropdown of recent builds (the user only sees the top 4 in the
/// dropdown, but we pre-fetch a slightly larger window so the calendar has
/// content the instant "Show older builds" is clicked). The "Load older
/// builds" affordance inside the calendar re-fetches with EXPANDED_FETCH_COUNT.
const INITIAL_FETCH_COUNT: usize = 12;
const EXPANDED_FETCH_COUNT: usize = 120;

#[allow(clippy::too_many_arguments)]
fn start_version_fetch(
    stack: gtk::Stack,
    loaded_box: gtk::Box,
    dialog: adw::Dialog,
    parent: gtk::Widget,
    error_page: adw::StatusPage,
    variant: &str,
    current_family: Rc<RefCell<Option<FamilyInfo>>>,
    selected_features: Rc<RefCell<Vec<String>>>,
    selected_stream: Rc<RefCell<String>>,
    booted_image: Rc<RefCell<Option<service::ImageRef>>>,
    max_versions: usize,
    on_show_changelog: OnShowChangelog,
    target_override: Option<service::ImageRef>,
) {
    // No "loading" stack page anymore — the empty calendar built on dialog
    // open carries its own "Loading builds…" indicator. Just make sure the
    // error page isn't sticky from a previous failed run.
    error_page.set_description(Some("Check your internet connection and try again."));

    build_loaded_page(
        &loaded_box,
        &stack,
        &dialog,
        &parent,
        Vec::new(),
        current_family.clone(),
        selected_features.clone(),
        selected_stream.clone(),
        booted_image.clone(),
        None,
        on_show_changelog.clone(),
        true, // is_loading
    );
    stack.set_visible_child_name("loaded");

    let variant_str = variant.to_string();
    let result_slot: Arc<Mutex<Option<FetchResult>>> = Arc::new(Mutex::new(None));
    spawn_fetch_thread(
        result_slot.clone(),
        &variant_str,
        max_versions,
        target_override.clone(),
    );

    // Build a "reload with EXPANDED_FETCH_COUNT" closure that the loaded
    // page passes to the "Load older builds" button inside the calendar.
    // Has to be a Rc<dyn Fn()> so the calendar widget can clone it into
    // its click handler — and so it captures all the args this function
    // needs to recursively call itself.
    let reload_fn: Rc<dyn Fn()> = {
        let stack = stack.clone();
        let loaded_box = loaded_box.clone();
        let dialog = dialog.clone();
        let parent = parent.clone();
        let error_page = error_page.clone();
        let variant_owned = variant_str.clone();
        let current_family = current_family.clone();
        let selected_features = selected_features.clone();
        let selected_stream = selected_stream.clone();
        let booted_image = booted_image.clone();
        let on_show_changelog = on_show_changelog.clone();
        let target_override_owned = target_override.clone();
        Rc::new(move || {
            start_version_fetch(
                stack.clone(),
                loaded_box.clone(),
                dialog.clone(),
                parent.clone(),
                error_page.clone(),
                &variant_owned,
                current_family.clone(),
                selected_features.clone(),
                selected_stream.clone(),
                booted_image.clone(),
                EXPANDED_FETCH_COUNT,
                on_show_changelog.clone(),
                target_override_owned.clone(),
            );
        })
    };

    // "is_expanded" flag drives whether we surface the "Load older builds"
    // button — if this call IS the expanded fetch, the button doesn't
    // belong (we already have all 120).
    let is_expanded = max_versions >= EXPANDED_FETCH_COUNT;

    let start_time = std::time::Instant::now();
    glib::timeout_add_local(std::time::Duration::from_millis(200), move || {
        if let Some(result) = result_slot.lock().ok().and_then(|mut guard| guard.take()) {
            match result {
                FetchResult::Ok(versions) => {
                    build_loaded_page(
                        &loaded_box,
                        &stack,
                        &dialog,
                        &parent,
                        versions,
                        current_family.clone(),
                        selected_features.clone(),
                        selected_stream.clone(),
                        booted_image.clone(),
                        if is_expanded {
                            None
                        } else {
                            Some(reload_fn.clone())
                        },
                        on_show_changelog.clone(),
                        false,
                    );
                    stack.set_visible_child_name("loaded");
                }
                FetchResult::DetectFailed => {
                    error_page.set_description(Some(
                        "Could not detect the current image. Is bootc installed and managing this system?",
                    ));
                    stack.set_visible_child_name("error");
                }
                FetchResult::Err(_) => {
                    error_page
                        .set_description(Some("Check your internet connection and try again."));
                    stack.set_visible_child_name("error");
                }
            }
            return glib::ControlFlow::Break;
        }

        // Generous timeout: the sha-tag probe path does ~120 manifest+config
        // round-trips against ghcr.io to surface recent builds whose tags
        // aren't date-stamped. 90s covers slower networks; cache absorbs the
        // cost on subsequent opens.
        if start_time.elapsed() > std::time::Duration::from_secs(90) {
            error_page.set_description(Some("Check your internet connection and try again."));
            stack.set_visible_child_name("error");
            return glib::ControlFlow::Break;
        }

        glib::ControlFlow::Continue
    });
}

fn spawn_fetch_thread(
    result_slot: Arc<Mutex<Option<FetchResult>>>,
    variant: &str,
    max_versions: usize,
    target_override: Option<service::ImageRef>,
) {
    let variant_str = variant.to_string();
    std::thread::spawn(move || {
        crate::runtime::block_on(async move {
            // `target_override` is set by the variant/stream switches in the
            // dialog: when the user flips DX/NVIDIA/stream, recompute resolves
            // the new target image and asks us to load THAT image's history
            // into the calendar instead of the booted one. None means use the
            // booted image (initial open, retry button).
            //
            // current_image() honours mock_identity → bootc status → os-release;
            // list_versions delegates to fetch_versions internally with the
            // config-blob date harvest included.
            let svc = service::global();
            let image_result = match target_override {
                Some(img) => Ok(img),
                None => svc.current_image().await,
            };
            let result = match image_result {
                Err(_) => FetchResult::DetectFailed,
                Ok(image) => match svc.list_versions(&image, max_versions).await {
                    Ok(mut versions) => {
                        if !variant_str.is_empty() && variant_str != "default" {
                            versions.retain(|v| v.version.contains(&variant_str));
                        }
                        FetchResult::Ok(versions)
                    }
                    Err(e) => FetchResult::Err(e.to_string()),
                },
            };
            *result_slot.lock().unwrap() = Some(result);
        });
    });
}

// ── Loaded page builder ──────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn build_loaded_page(
    container: &gtk::Box,
    stack: &gtk::Stack,
    dialog: &adw::Dialog,
    parent: &gtk::Widget,
    versions: Vec<ImageVersion>,
    current_family: Rc<RefCell<Option<FamilyInfo>>>,
    selected_features: Rc<RefCell<Vec<String>>>,
    selected_stream: Rc<RefCell<String>>,
    booted_image: Rc<RefCell<Option<service::ImageRef>>>,
    reload_fn: Option<Rc<dyn Fn()>>,
    on_show_changelog: OnShowChangelog,
    is_loading: bool,
) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }

    // Version lookup map
    let version_map: HashMap<NaiveDate, ImageVersion> =
        versions.iter().map(|v| (v.date, v.clone())).collect();
    let version_map = Rc::new(version_map);

    // Find "current" date from the version whose full_ref matches bootc status
    // (best-effort: use the most recent version as current for now).
    let current_date: Option<NaiveDate> = versions.last().map(|v| v.date);

    // ── Selected version state ──────────────────────────────────────────
    let selected: Rc<RefCell<Option<NaiveDate>>> = Rc::new(RefCell::new(None));

    // ── Details panel (hidden until selection) ──────────────────────────
    let details_group = adw::PreferencesGroup::builder()
        .title("Selected Version")
        .margin_start(16)
        .margin_end(16)
        .margin_top(8)
        .margin_bottom(8)
        .build();
    details_group.set_visible(false);

    let version_row = adw::ActionRow::builder().title("Version").build();
    let kernel_row = adw::ActionRow::builder().title("Kernel").build();
    let built_row = adw::ActionRow::builder().title("Built").build();
    let commit_row = adw::ActionRow::builder().title("Commit").build();

    details_group.add(&version_row);
    details_group.add(&kernel_row);
    details_group.add(&built_row);
    details_group.add(&commit_row);

    // ── See changelog button (disabled until selection) ─────────────────
    // Closes the dialog and routes the main view to the What's New page
    // filtered to the selected tag, so the user can preview the diff vs.
    // booted before committing to a rebase.
    let see_changelog_btn = gtk::Button::builder()
        .label("See changelog")
        .sensitive(false)
        .margin_start(16)
        .margin_end(16)
        .margin_top(8)
        .build();
    see_changelog_btn.add_css_class("flat");

    // ── Rebase button ───────────────────────────────────────────────────
    // The label and sensitivity update from THREE places:
    //   - this initial computation (stream / variant state at page-load)
    //   - the day-click handler (populate_details_for, when a date is picked)
    //   - the day-click handler's deselect branch (when a picked date is
    //     clicked again, falling back to the stream-switch label)
    // The shared cell `pending_stream_ref` carries the resolved full ref
    // for stream/variant-only switches, so the click handler can dispatch
    // without re-resolving (and so the day-pin path can override it).
    let pending_stream_ref: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let (initial_label, initial_sensitive, initial_ref) = compute_stream_switch_action(
        current_family.borrow().as_ref(),
        &selected_features.borrow(),
        &selected_stream.borrow(),
        booted_image.borrow().as_ref(),
    );
    *pending_stream_ref.borrow_mut() = initial_ref;
    let rebase_btn = gtk::Button::builder()
        .label(&with_access_key(&initial_label))
        .use_underline(true)
        .sensitive(initial_sensitive)
        .margin_start(16)
        .margin_end(16)
        .margin_top(4)
        .margin_bottom(16)
        .build();
    // Inline action button (in-page, not a centered StatusPage CTA) — per
    // GNOME HIG / control-center About "Donate" pattern, use .suggested-action
    // alone, without .pill. .pill is reserved for standalone hero CTAs.
    rebase_btn.add_css_class("suggested-action");

    // ── Build calendar grid ─────────────────────────────────────────────
    let calendar_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    calendar_box.set_margin_start(8);
    calendar_box.set_margin_end(8);
    calendar_box.set_margin_top(16);
    calendar_box.set_margin_bottom(8);

    // Current displayed month — start on the month containing the most-recent
    // published image so the user lands on something with highlighted days
    // instead of an empty calendar (Dakota's recent tags are sha-only, not
    // date-stamped, so "today" might have nothing visible).
    let today = Local::now().date_naive();
    let initial_month = versions
        .last()
        .map(|v| NaiveDate::from_ymd_opt(v.date.year(), v.date.month(), 1).unwrap_or(v.date))
        .unwrap_or_else(|| {
            NaiveDate::from_ymd_opt(today.year(), today.month(), 1).unwrap_or(today)
        });
    let displayed_month: Rc<RefCell<NaiveDate>> = Rc::new(RefCell::new(initial_month));

    // ── Month nav row ───────────────────────────────────────────────────
    let prev_btn = gtk::Button::builder()
        .icon_name("go-previous-symbolic")
        .tooltip_text("Previous month")
        .build();
    prev_btn.add_css_class("flat");
    prev_btn.add_css_class("circular");

    let next_btn = gtk::Button::builder()
        .icon_name("go-next-symbolic")
        .tooltip_text("Next month")
        .build();
    next_btn.add_css_class("flat");
    next_btn.add_css_class("circular");
    // Initially disabled (already on current month)
    next_btn.set_sensitive(false);

    let month_label = gtk::Label::builder()
        .hexpand(true)
        .halign(gtk::Align::Center)
        .build();
    month_label.add_css_class("title-4");

    let nav_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(4)
        .margin_bottom(12)
        .build();
    nav_row.append(&prev_btn);
    nav_row.append(&month_label);
    nav_row.append(&next_btn);
    calendar_box.append(&nav_row);

    // Weekday headers
    let header_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .homogeneous(true)
        .margin_bottom(4)
        .build();
    for day in ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"] {
        let lbl = gtk::Label::new(Some(day));
        lbl.add_css_class("caption");
        lbl.add_css_class("dim-label");
        lbl.set_hexpand(true);
        lbl.set_halign(gtk::Align::Center);
        header_row.append(&lbl);
    }
    calendar_box.append(&header_row);

    // Day grid — 7 columns × 6 rows, pre-populated
    let grid = gtk::Grid::builder()
        .row_spacing(2)
        .column_spacing(2)
        .row_homogeneous(true)
        .column_homogeneous(true)
        .build();
    for row in 0..6i32 {
        for col in 0..7i32 {
            let btn = gtk::Button::new();
            btn.add_css_class("flat");
            btn.add_css_class("day-btn");
            grid.attach(&btn, col, row, 1, 1);
        }
    }
    calendar_box.append(&grid);

    // Empty-state hint shown when the displayed month has no highlighted days
    // (e.g. user navigated to a month with no published builds). Toggled by
    // redraw_grid based on the count of `day-available` cells. When the
    // dialog is in its initial loading state we relabel this to "Loading
    // builds…" + spinner so the empty calendar reads as "in progress",
    // not "definitively empty".
    let empty_hint = gtk::Label::builder()
        .label(if is_loading {
            "Loading builds…"
        } else {
            "No builds in this month"
        })
        .halign(gtk::Align::Center)
        .margin_top(8)
        .build();
    empty_hint.add_css_class("dim-label");
    empty_hint.add_css_class("caption");
    empty_hint.set_visible(false);
    calendar_box.append(&empty_hint);

    // Persistent "Loading builds…" row at the bottom of the calendar while
    // the registry fetch is in flight. Sits below the grid so the user
    // sees activity even when the grid itself has some highlights (e.g.
    // first batch of dated tags arrived but sha probe is still running).
    if is_loading {
        let loading_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        loading_row.set_halign(gtk::Align::Center);
        loading_row.set_margin_top(8);
        let spinner = gtk::Spinner::new();
        spinner.set_spinning(true);
        spinner.set_size_request(14, 14);
        let lbl = gtk::Label::new(Some("Loading builds…"));
        lbl.add_css_class("dim-label");
        lbl.add_css_class("caption");
        loading_row.append(&spinner);
        loading_row.append(&lbl);
        calendar_box.append(&loading_row);
    }

    // "Load older builds" button at the bottom of the calendar — only when
    // the initial fetch was the small INITIAL_FETCH_COUNT cut. Clicking
    // restarts start_version_fetch with EXPANDED_FETCH_COUNT=120 so the
    // user can roll back past the initial window. Hidden when the page is
    // already showing expanded results.
    if let Some(reload) = reload_fn.as_ref() {
        let load_older_btn = gtk::Button::builder()
            .label("Load older builds")
            .halign(gtk::Align::Center)
            .margin_top(8)
            .margin_bottom(4)
            .build();
        load_older_btn.add_css_class("flat");
        let reload = reload.clone();
        let btn = load_older_btn.clone();
        load_older_btn.connect_clicked(move |_| {
            // Spinner-as-label + insensitive so the user knows the fetch
            // is in flight; start_version_fetch swaps the whole loaded
            // page out so we don't have to reset this state ourselves.
            btn.set_label("Loading…");
            btn.set_sensitive(false);
            reload();
        });
        calendar_box.append(&load_older_btn);
    }

    inject_calendar_css();

    // See changelog mirrors the details panel: enabled iff a date is selected.
    // Use property binding so we don't have to thread the button through every
    // call site that toggles the details panel visibility.
    details_group
        .bind_property("visible", &see_changelog_btn, "sensitive")
        .sync_create()
        .build();

    // Wire the See changelog button — closes the dialog, routes to What's New
    // for the selected tag.
    {
        let selected_rc = selected.clone();
        let version_map_rc = version_map.clone();
        let dialog_rc = dialog.clone();
        let cb = on_show_changelog.clone();
        see_changelog_btn.connect_clicked(move |_| {
            let Some(date) = *selected_rc.borrow() else {
                return;
            };
            let Some(v) = version_map_rc.get(&date).cloned() else {
                return;
            };
            dialog_rc.close();
            cb(v.version);
        });
    }

    // ── Assemble container ──────────────────────────────────────────────
    // Calendar is the primary navigator: highlighted days have published
    // images, click one to load its details below.
    container.append(&calendar_box);
    container.append(&details_group);
    container.append(&see_changelog_btn);
    container.append(&rebase_btn);

    // ── Helpers for re-drawing the grid ────────────────────────────────
    let version_map_rc = version_map.clone();
    let selected_rc = selected.clone();
    let details_group_rc = details_group.clone();
    let version_row_rc = version_row.clone();
    let kernel_row_rc = kernel_row.clone();
    let built_row_rc = built_row.clone();
    let commit_row_rc = commit_row.clone();
    let rebase_btn_rc = rebase_btn.clone();
    let month_label_rc = month_label.clone();
    let next_btn_rc = next_btn.clone();
    let empty_hint_rc = empty_hint.clone();

    // Closure invoked when the user clicks an already-selected day to clear
    // their selection. Restores the stream-switch label so the button
    // doesn't go dead if the user was changing streams via the dropdown.
    let on_deselect: Rc<dyn Fn()> = {
        let rebase_btn = rebase_btn.clone();
        let current_family = current_family.clone();
        let selected_features = selected_features.clone();
        let selected_stream = selected_stream.clone();
        let booted_image = booted_image.clone();
        let pending_stream_ref = pending_stream_ref.clone();
        Rc::new(move || {
            let (label, sensitive, full_ref) = compute_stream_switch_action(
                current_family.borrow().as_ref(),
                &selected_features.borrow(),
                &selected_stream.borrow(),
                booted_image.borrow().as_ref(),
            );
            *pending_stream_ref.borrow_mut() = full_ref;
            rebase_btn.set_label(&with_access_key(&label));
            rebase_btn.set_sensitive(sensitive);
        })
    };

    let on_deselect_for_redraw = on_deselect.clone();
    let redraw = Rc::new(move |grid: &gtk::Grid, displayed: NaiveDate| {
        redraw_grid(
            grid,
            displayed,
            &version_map_rc,
            current_date,
            &selected_rc,
            &details_group_rc,
            &version_row_rc,
            &kernel_row_rc,
            &built_row_rc,
            &commit_row_rc,
            &rebase_btn_rc,
            &month_label_rc,
            &next_btn_rc,
            &empty_hint_rc,
            Some(on_deselect_for_redraw.clone()),
        );
    });

    // Initial draw
    redraw(&grid, *displayed_month.borrow());

    // ── Month navigation ────────────────────────────────────────────────
    {
        let grid = grid.clone();
        let displayed_month = displayed_month.clone();
        let redraw = redraw.clone();
        prev_btn.connect_clicked(move |_| {
            let current = *displayed_month.borrow();
            let prev = if current.month() == 1 {
                NaiveDate::from_ymd_opt(current.year() - 1, 12, 1).unwrap_or(current)
            } else {
                NaiveDate::from_ymd_opt(current.year(), current.month() - 1, 1).unwrap_or(current)
            };
            *displayed_month.borrow_mut() = prev;
            redraw(&grid, prev);
        });
    }

    {
        let grid = grid.clone();
        let displayed_month = displayed_month.clone();
        let redraw = redraw.clone();
        next_btn.connect_clicked(move |_| {
            let current = *displayed_month.borrow();
            let next = if current.month() == 12 {
                NaiveDate::from_ymd_opt(current.year() + 1, 1, 1).unwrap_or(current)
            } else {
                NaiveDate::from_ymd_opt(current.year(), current.month() + 1, 1).unwrap_or(current)
            };
            *displayed_month.borrow_mut() = next;
            redraw(&grid, next);
        });
    }

    // ── Rebase button click → confirm → run bootc switch ───────────────
    {
        let selected_rc = selected.clone();
        let version_map_rc = version_map.clone();
        let dialog_rc = dialog.clone();
        let parent_rc = parent.clone();
        let stack_rc = stack.clone();
        let current_family_rc = current_family.clone();
        let selected_features_rc = selected_features.clone();
        let pending_stream_ref_rc = pending_stream_ref.clone();
        let selected_stream_rc = selected_stream.clone();

        rebase_btn.connect_clicked(move |_| {
            // ── No date selected: stream/variant-only switch path ───────
            // Triggered by the user changing the stream dropdown (or DX /
            // NVIDIA toggles) and clicking the button without picking a
            // specific date. Commits to the floating stream tag, so future
            // upgrades follow it instead of pinning to a single build.
            let Some(date) = *selected_rc.borrow() else {
                let Some(full_ref) = pending_stream_ref_rc.borrow().clone() else {
                    return;
                };
                let stream = selected_stream_rc.borrow().clone();
                let confirm = adw::AlertDialog::builder()
                    .heading(format!("Switch to :{}?", stream))
                    .body(format!(
                        "Your system will follow the floating `{}` tag and resume receiving automatic updates from it:\n\n{}\n\nA restart is required and the full image will be re-downloaded.",
                        stream, full_ref,
                    ))
                    .build();
                confirm.add_response("cancel", "_Cancel");
                confirm.add_response("switch", "_Switch");
                confirm.set_response_appearance("switch", adw::ResponseAppearance::Suggested);
                confirm.set_default_response(Some("cancel"));
                confirm.set_close_response("cancel");

                let stack = stack_rc.clone();
                let dialog_close = dialog_rc.clone();
                let full_ref_for_run = full_ref.clone();
                confirm.connect_response(None, move |_, response| {
                    if response == "switch" {
                        // Unconditionally. Suppression is decided at the
                        // chokepoint inside run_bootc_switch, not here — see
                        // the note above run_rebase.
                        run_rebase(
                            full_ref_for_run.clone(),
                            stack.clone(),
                            dialog_close.clone(),
                        );
                    }
                });
                confirm.present(Some(&parent_rc));
                return;
            };
            let Some(version) = version_map_rc.get(&date).cloned() else {
                return;
            };

            // Resolve the target image from the feature switches. If a family
            // is detected, swap the image name in `version.full_ref` to the
            // one whose suffix matches the selected features (e.g. flipping
            // `nvidia` on bluefin → `bluefin-nvidia`). If the combination
            // isn't published, fall back to the booted image so the user
            // doesn't end up on a bogus ref.
            let family_ref = current_family_rc.borrow();
            let target_full_ref = resolve_target_ref(
                &version.full_ref,
                family_ref.as_ref(),
                &selected_features_rc.borrow(),
            );
            drop(family_ref);
            let switching_image = target_full_ref != version.full_ref;

            let body = if switching_image {
                format!(
                    "Your system will be pinned to:\n\n{}\n\nThis is a different image variant than what you're currently running. A restart is required and the full image will be re-downloaded. Automatic updates pause until you unpin.",
                    target_full_ref,
                )
            } else {
                let display_version = strip_date_suffix(&version.version)
                    .unwrap_or_else(|| version.version.clone());
                format!(
                    "Your system will be pinned to the {} build (version {}).\n\nA restart is required and the full image will be re-downloaded. Automatic updates pause until you unpin.",
                    date.format("%B %-d, %Y"),
                    display_version,
                )
            };

            let confirm = adw::AlertDialog::builder()
                .heading("Pin to this build?")
                .body(body)
                .build();

            confirm.add_response("cancel", "_Cancel");
            confirm.add_response("rebase", "_Pin");
            confirm.set_response_appearance("rebase", adw::ResponseAppearance::Suggested);
            confirm.set_default_response(Some("cancel"));
            confirm.set_close_response("cancel");

            let full_ref = target_full_ref;
            let stack = stack_rc.clone();
            let dialog_close = dialog_rc.clone();

            confirm.connect_response(None, move |_, response| {
                if response == "rebase" {
                    run_rebase(full_ref.clone(), stack.clone(), dialog_close.clone());
                }
            });

            confirm.present(Some(&parent_rc));
        });
    }
}

/// Substitute the image name in `registry/org/image:tag` based on the
/// resolved family + feature selection. Returns the original ref unchanged
/// if no family was detected or the feature combination has no published
/// image — keeps us from constructing refs the registry doesn't serve.
///
/// Delegates the family → image resolution to the service layer
/// ([`UpdaterService::resolve_target`]) so a future alternative frontend can
/// share the same logic without re-implementing it.
fn resolve_target_ref(
    full_ref: &str,
    family: Option<&FamilyInfo>,
    selected_features: &[String],
) -> String {
    let Some(family) = family else {
        return full_ref.to_string();
    };
    let Some(target) = service::global().resolve_target(family, selected_features) else {
        return full_ref.to_string();
    };
    // full_ref = registry/org/image:tag — swap `image` only, preserving the
    // tag the user picked from the calendar.
    let Some((before_tag, tag)) = full_ref.rsplit_once(':') else {
        return full_ref.to_string();
    };
    let Some((reg_org, _old_image)) = before_tag.rsplit_once('/') else {
        return full_ref.to_string();
    };
    format!("{reg_org}/{}:{tag}", target.image)
}

// ── Grid redraw ──────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn redraw_grid(
    grid: &gtk::Grid,
    displayed: NaiveDate,
    versions: &HashMap<NaiveDate, ImageVersion>,
    current_date: Option<NaiveDate>,
    selected: &Rc<RefCell<Option<NaiveDate>>>,
    details_group: &adw::PreferencesGroup,
    version_row: &adw::ActionRow,
    kernel_row: &adw::ActionRow,
    built_row: &adw::ActionRow,
    commit_row: &adw::ActionRow,
    rebase_btn: &gtk::Button,
    month_label: &gtk::Label,
    next_btn: &gtk::Button,
    empty_hint: &gtk::Label,
    // Called when the user clicks an already-selected day to deselect it.
    // Restores the rebase button's stream-switch label/sensitivity (e.g.
    // "Switch to :testing") so the button stays useful instead of going
    // disabled in the middle of a stream change.
    on_deselect: Option<Rc<dyn Fn()>>,
) {
    let today = Local::now().date_naive();

    // Update label
    month_label.set_label(&displayed.format("%B %Y").to_string());

    // Disable next if we're on current month
    let current_month = NaiveDate::from_ymd_opt(today.year(), today.month(), 1).unwrap_or(today);
    next_btn.set_sensitive(displayed < current_month);

    let days_in_month = days_in_month(displayed);
    // ISO weekday Mon=0, Sun=6
    let first_weekday = displayed.weekday().num_days_from_monday() as i32;
    let selected_date = *selected.borrow();

    let mut available_count = 0u32;
    let mut slot = 0i32;
    for row in 0..6i32 {
        for col in 0..7i32 {
            let btn = grid
                .child_at(col, row)
                .and_then(|w| w.downcast::<gtk::Button>().ok());
            let Some(btn) = btn else {
                slot += 1;
                continue;
            };

            let day_num = slot - first_weekday + 1;

            if day_num < 1 || day_num > days_in_month as i32 {
                btn.set_label("");
                btn.set_visible(false);
                btn.set_sensitive(false);
            } else {
                btn.set_visible(true);
                btn.set_label(&day_num.to_string());

                let date =
                    NaiveDate::from_ymd_opt(displayed.year(), displayed.month(), day_num as u32);

                // Clear state classes
                for cls in ["day-available", "day-current", "day-selected", "day-today"] {
                    btn.remove_css_class(cls);
                }

                if let Some(d) = date {
                    let is_available = versions.contains_key(&d);
                    let is_current = current_date == Some(d);
                    let is_selected = selected_date == Some(d);
                    let is_today = d == today;
                    let is_future = d > today;

                    btn.set_sensitive(is_available && !is_future);

                    if is_today {
                        btn.add_css_class("day-today");
                    }
                    if is_available {
                        btn.add_css_class("day-available");
                        available_count += 1;
                    }
                    if is_current {
                        btn.add_css_class("day-current");
                    }
                    if is_selected {
                        btn.add_css_class("day-selected");
                    }

                    if is_available && !is_future {
                        // Wire click — disconnect any existing handler first
                        if let Some(hid) =
                            unsafe { btn.steal_data::<glib::SignalHandlerId>("day-handler") }
                        {
                            btn.disconnect(hid);
                        }

                        let selected_inner = selected.clone();
                        let grid_inner = grid.clone();
                        let displayed_inner = displayed;
                        let versions_inner = versions.clone();
                        let current_date_inner = current_date;
                        let details_group_inner = details_group.clone();
                        let version_row_inner = version_row.clone();
                        let kernel_row_inner = kernel_row.clone();
                        let built_row_inner = built_row.clone();
                        let commit_row_inner = commit_row.clone();
                        let rebase_btn_inner = rebase_btn.clone();
                        let month_label_inner = month_label.clone();
                        let next_btn_inner = next_btn.clone();
                        let empty_hint_inner = empty_hint.clone();
                        let on_deselect_inner = on_deselect.clone();
                        let on_deselect_for_redraw = on_deselect.clone();

                        let hid = btn.connect_clicked(move |_| {
                            // Toggle or set selection
                            let prev = *selected_inner.borrow();
                            if prev == Some(d) {
                                *selected_inner.borrow_mut() = None;
                            } else {
                                *selected_inner.borrow_mut() = Some(d);
                            }

                            // Redraw to update selection highlight
                            redraw_grid(
                                &grid_inner,
                                displayed_inner,
                                &versions_inner,
                                current_date_inner,
                                &selected_inner,
                                &details_group_inner,
                                &version_row_inner,
                                &kernel_row_inner,
                                &built_row_inner,
                                &commit_row_inner,
                                &rebase_btn_inner,
                                &month_label_inner,
                                &next_btn_inner,
                                &empty_hint_inner,
                                on_deselect_for_redraw.clone(),
                            );

                            // Update details panel
                            if let Some(sel_date) = *selected_inner.borrow() {
                                if let Some(v) = versions_inner.get(&sel_date) {
                                    update_details(
                                        &details_group_inner,
                                        &version_row_inner,
                                        &kernel_row_inner,
                                        &built_row_inner,
                                        &commit_row_inner,
                                        &rebase_btn_inner,
                                        v,
                                        &sel_date,
                                        current_date_inner,
                                    );
                                }
                            } else {
                                details_group_inner.set_visible(false);
                                // Restore stream-switch state on the button
                                // rather than just disabling it. The user
                                // may have been mid-stream-change; clearing
                                // back to "Switch to :testing" is more
                                // honest than a dead button.
                                if let Some(ref f) = on_deselect_inner {
                                    f();
                                } else {
                                    rebase_btn_inner.set_sensitive(false);
                                }
                            }
                        });

                        unsafe { btn.set_data("day-handler", hid) };
                    }
                } else {
                    btn.set_sensitive(false);
                }
            }
            slot += 1;
        }
    }

    empty_hint.set_visible(available_count == 0);
}

fn update_details(
    group: &adw::PreferencesGroup,
    version_row: &adw::ActionRow,
    kernel_row: &adw::ActionRow,
    built_row: &adw::ActionRow,
    commit_row: &adw::ActionRow,
    rebase_btn: &gtk::Button,
    v: &ImageVersion,
    date: &NaiveDate,
    current_date: Option<NaiveDate>,
) {
    version_row.set_subtitle(&v.version);
    kernel_row.set_subtitle(&v.kernel);
    built_row.set_subtitle(&v.created.format("%b %-d, %Y · %H:%M UTC").to_string());
    commit_row.set_subtitle(if v.revision.is_empty() {
        "—"
    } else {
        &v.revision
    });

    group.set_visible(true);

    let is_current = current_date == Some(*date);
    if is_current {
        rebase_btn.set_label(&with_access_key("Currently Installed"));
        rebase_btn.set_sensitive(false);
    } else {
        // YYYYMMDD format — matches the registry's actual tag scheme and
        // is what the user types when they reference a build.
        rebase_btn.set_label(&with_access_key(&format!(
            "Pin to {}",
            date.format("%Y%m%d")
        )));
        rebase_btn.set_sensitive(true);
    }
}

/// Compute what the rebase button should say + do when NO calendar day is
/// Mark the first character of a button label as its access key.
///
/// Applied at the widget layer rather than baked into the label strings so
/// [`compute_stream_switch_action`] keeps returning plain text its unit tests
/// can assert on, and so the underscore convention lives in exactly one place.
///
/// This is a HIG item — the dialog's primary action had no access key — but it
/// is also what makes the action reachable from the GUI suite at all: pointer
/// events do not reach dialog content under Broadway, so a mnemonic is the
/// only way to press this button in a test. The most consequential command the
/// app can issue was previously undrivable, and the check that claimed to
/// cover it merely opened the dialog and stopped.
///
/// Any literal underscore in `label` is escaped, otherwise it would silently
/// become a second, wrong access key.
fn with_access_key(label: &str) -> String {
    let escaped = label.replace('_', "__");
    format!("_{escaped}")
}



// ── Rebase worker ────────────────────────────────────────────────────────────

/// Run a switch. Call this for every confirmed switch, in every mode.
///
/// There used to be a `run_rebase_simulated` sibling, selected by an `if
/// dev_mode` at each call site — where `dev_mode` was actually
/// `dev_mode || dry_run` (app.rs computed `suppress_real` and passed it to a
/// parameter of the wrong name). It rendered its own "(simulated)" pages and
/// returned without ever reaching the privileged chokepoint.
///
/// That is precisely the bypass the chokepoint exists to prevent. Dry run is
/// supposed to *record* the intent it suppresses; this path suppressed the
/// command and the record together, so the most consequential action in the
/// app produced no journal entry and nothing could assert what it would have
/// done. A safety check that diverts around the audit point is not a safety
/// check.
///
/// Suppression belongs to `privileged_async` in run_bootc_switch, which
/// decides from `Suppressed::from_flags(dev_mode, dry_run)`, writes the
/// journal entry, and reports "Dry run — image switch recorded, not
/// performed". Keep the decision there and there only.
fn run_rebase(full_ref: String, stack: gtk::Stack, dialog: adw::Dialog) {
    // Build a progress page with a pulsing ProgressBar + elapsed-time label.
    // A live `bootc switch` measured against ghcr.io took 2m28s for a full
    // dakota-nvidia pull on a residential link — too long for a bare spinner.
    // Pulse mode (no fraction) is the honest representation until we parse
    // bootc's per-layer progress lines (task #24 phase 2).
    let progress_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_start(24)
        .margin_end(24)
        .margin_top(12)
        .margin_bottom(24)
        .build();

    let progress_bar = gtk::ProgressBar::new();
    progress_bar.set_pulse_step(0.08);
    progress_box.append(&progress_bar);

    let elapsed_label = gtk::Label::new(Some("Elapsed: 0:00"));
    elapsed_label.add_css_class("dim-label");
    elapsed_label.add_css_class("caption");
    progress_box.append(&elapsed_label);

    let progress_page = adw::StatusPage::builder()
        .title("Switching...")
        .description("Pulling the new image layers. This typically takes 2–5 minutes.")
        .build();
    progress_page.set_child(Some(&progress_box));
    stack.add_named(&progress_page, Some("switching"));
    stack.set_visible_child_name("switching");

    // The bar pulses until we see a parseable Fraction event from bootc; from
    // that point on we drive `set_fraction` directly and stop pulsing. A flag
    // shared with the pulse timer lets the progress consumer disable it.
    let start = std::time::Instant::now();
    let bar_clone = progress_bar.clone();
    let label_clone = elapsed_label.clone();
    let known_fraction: Rc<Cell<bool>> = Rc::new(Cell::new(false));
    let known_fraction_pulse = known_fraction.clone();
    let pulse_handle: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
    let pulse_handle_store = pulse_handle.clone();
    let id = glib::timeout_add_local(std::time::Duration::from_millis(200), move || {
        if !known_fraction_pulse.get() {
            bar_clone.pulse();
        }
        let secs = start.elapsed().as_secs();
        label_clone.set_text(&format!("Elapsed: {}:{:02}", secs / 60, secs % 60));
        glib::ControlFlow::Continue
    });
    *pulse_handle_store.borrow_mut() = Some(id);

    // Cross-thread channel for streaming BootcProgress events from the
    // subprocess reader (background thread) to the GTK main loop where the
    // ProgressBar lives. std::sync::mpsc is enough here — we drain it from a
    // glib timeout below.
    let (prog_tx_std, prog_rx_std) = std::sync::mpsc::channel::<BootcProgress>();

    let result_slot: Arc<Mutex<Option<Result<(), String>>>> = Arc::new(Mutex::new(None));
    let result_bg = result_slot.clone();

    std::thread::spawn(move || {
        crate::runtime::block_on(async move {
            // tokio mpsc bridges the async readers to a sync channel the GTK
            // thread can poll. Each parsed BootcProgress flows: stdout/stderr
            // reader → tokio channel → forwarder → std::sync::mpsc → GTK.
            let (tokio_tx, mut tokio_rx) = tokio::sync::mpsc::unbounded_channel::<BootcProgress>();
            let prog_tx_std_inner = prog_tx_std.clone();
            let forward = tokio::spawn(async move {
                while let Some(p) = tokio_rx.recv().await {
                    let _ = prog_tx_std_inner.send(p);
                }
            });
            let result = run_bootc_switch(&full_ref, tokio_tx).await;
            let _ = forward.await;
            *result_bg.lock().unwrap() = Some(result);
        });
    });

    let progress_page_for_status = progress_page.clone();
    let bar_for_progress = progress_bar.clone();
    let known_fraction_for_progress = known_fraction.clone();
    glib::timeout_add_local(std::time::Duration::from_millis(150), move || {
        // Drain everything currently in the queue so a burst of layer events
        // doesn't lag behind. We don't break on Empty — that just means no
        // new events yet, keep the timer going.
        loop {
            match prog_rx_std.try_recv() {
                Ok(BootcProgress::Fraction { current, total }) => {
                    known_fraction_for_progress.set(true);
                    let frac = (current as f64) / (total as f64);
                    bar_for_progress.set_fraction(frac.clamp(0.0, 1.0));
                }
                Ok(BootcProgress::Status(s)) => {
                    progress_page_for_status.set_description(Some(&s));
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    return glib::ControlFlow::Break;
                }
            }
        }
        glib::ControlFlow::Continue
    });

    glib::timeout_add_local(std::time::Duration::from_millis(200), move || {
        let Some(result) = result_slot.lock().ok().and_then(|mut g| g.take()) else {
            return glib::ControlFlow::Continue;
        };
        // Stop the pulse animation now that we have a final result.
        if let Some(id) = pulse_handle.borrow_mut().take() {
            id.remove();
        }
        match result {
            Ok(()) => {
                // Success means "the chokepoint returned Ok", which in a dry
                // run means the switch was recorded and deliberately not
                // performed. Saying "Restart your system to boot into the
                // selected version" there would be a lie the user has no way
                // to catch — nothing changed and rebooting proves nothing.
                let settings = crate::settings::Settings::load();
                let suppressed = crate::action_journal::Suppressed::from_flags(
                    settings.dev_mode,
                    settings.dry_run,
                )
                .blocks_execution();

                let (title, description) = if suppressed {
                    (
                        "Switch Recorded",
                        "Dry run — the image switch was recorded but not \
                         performed. Nothing on this system has changed.",
                    )
                } else {
                    (
                        "Switch Complete",
                        "Restart your system to boot into the selected version.",
                    )
                };
                let done_page = adw::StatusPage::builder()
                    .title(title)
                    .description(description)
                    .icon_name("object-select-symbolic")
                    .build();
                let close_btn = gtk::Button::builder()
                    .label("Close")
                    .halign(gtk::Align::Center)
                    .build();
                close_btn.add_css_class("suggested-action");
                close_btn.add_css_class("pill");
                let dialog_close = dialog.clone();
                close_btn.connect_clicked(move |_| {
                    dialog_close.close();
                });
                done_page.set_child(Some(&close_btn));
                stack.add_named(&done_page, Some("done"));
                stack.set_visible_child_name("done");
            }
            Err(msg) => {
                let fail_page = adw::StatusPage::builder()
                    .title("Switch Failed")
                    .description(msg)
                    .icon_name("dialog-error-symbolic")
                    .build();
                let close_btn = gtk::Button::builder()
                    .label("Close")
                    .halign(gtk::Align::Center)
                    .build();
                close_btn.add_css_class("pill");
                let dialog_close = dialog.clone();
                close_btn.connect_clicked(move |_| {
                    dialog_close.close();
                });
                fail_page.set_child(Some(&close_btn));
                stack.add_named(&fail_page, Some("fail"));
                stack.set_visible_child_name("fail");
            }
        }
        glib::ControlFlow::Break
    });
}

// ── Family + feature-switch UI ──────────────────────────────────────────────

/// Detect the booted (or mocked) image's Family and render one SwitchRow per
/// atomic feature. As switches toggle, recompute the target image and write
/// it into `target_row`'s subtitle. The dialog uses this to show the user
/// the *resolved* image they'd land on without exposing the raw image names.
///
/// Runs the detection on a background thread (the same pattern as
/// [`spawn_fetch_thread`]) so the dialog stays responsive while bootc/os-release
/// IO completes.
fn populate_family_switches(
    features_group: &adw::PreferencesGroup,
    family_label: &gtk::Label,
    target_row: &adw::ActionRow,
    stream_row: &adw::ComboRow,
    current_family: Rc<RefCell<Option<FamilyInfo>>>,
    selected_features: Rc<RefCell<Vec<String>>>,
    selected_stream: Rc<RefCell<String>>,
    booted_image: Rc<RefCell<Option<service::ImageRef>>>,
    on_target_change: Rc<dyn Fn(Option<service::ImageRef>)>,
) {
    // Two pieces of state get resolved in the background thread:
    //   - the family the booted image belongs to (drives WHICH toggles show)
    //   - the booted ImageRef itself (drives the toggles' INITIAL state, so
    //     a user already on bluefin-dx-nvidia-open sees both toggles ON
    //     when the dialog opens, not OFF as if rebasing would downgrade)
    let slot: Arc<Mutex<Option<(Option<FamilyInfo>, Option<service::ImageRef>)>>> =
        Arc::new(Mutex::new(None));

    {
        let slot = slot.clone();
        std::thread::spawn(move || {
            let detected = crate::runtime::block_on(async move {
                let svc = service::global();
                let family = svc.current_family().await.ok().flatten();
                let image = svc.current_image().await.ok();
                (family, image)
            });
            *slot.lock().unwrap() = Some(detected);
        });
    }

    let features_group = features_group.clone();
    let family_label = family_label.clone();
    let target_row = target_row.clone();
    let stream_row = stream_row.clone();

    glib::timeout_add_local(std::time::Duration::from_millis(200), move || {
        let Some((family_opt, image_opt)) = slot.lock().ok().and_then(|mut g| g.take()) else {
            return glib::ControlFlow::Continue;
        };

        let Some(family) = family_opt else {
            family_label.set_label("Family not recognized");
            target_row.set_subtitle("(this image isn't in the KNOWN_FAMILIES catalogue)");
            return glib::ControlFlow::Break;
        };

        family_label.set_label(&format!("Family: {}", family.name));

        // Populate stream dropdown with available streams for this family
        let stream_model = gtk::StringList::new(&[]);
        for stream in &family.streams {
            stream_model.append(stream);
        }
        stream_row.set_model(Some(&stream_model));

        // Set default stream to the first one (canonical)
        if !family.streams.is_empty() {
            stream_row.set_selected(0);
            *selected_stream.borrow_mut() = family.streams[0].clone();
        }

        // Stash booted image for build_loaded_page's rebase-button label
        // logic (it needs the booted tag to detect "stream/variant change vs
        // no change"). Cloned because image_opt is consumed by the toggle-
        // state derivation below.
        *booted_image.borrow_mut() = image_opt.clone();

        // Derive the initial toggle state from the booted image's suffix so
        // users already on -dx / -nvidia / -dx-nvidia-open see the dialog
        // open with their current configuration represented, not with
        // everything OFF. Without this the dialog implies that rebasing
        // would *downgrade* to the base image.
        let (initial_dx, initial_nvidia) = derive_initial_toggle_state(&family, image_opt.as_ref());
        target_row.set_subtitle(
            image_opt
                .as_ref()
                .map(|img| format!("{} (currently booted)", img.image))
                .unwrap_or_else(|| format!("{} (no extras)", family.base_image))
                .as_str(),
        );

        // Two opinionated toggles instead of one-per-atomic-feature. Per user
        // direction: "we should have toggle for Developer Mode and Nvidia".
        // Granular features (hwe, deck, asus, surface, framework) aren't
        // user-facing here — KNOWN_FAMILIES still lists them so the resolver
        // can land on a published image if the user is currently booted on
        // one, but the rebase dialog only exposes the two switches users
        // think about.
        let supports_dx = family.features.iter().any(|f| f.id == "dx");
        let supports_nvidia = family
            .features
            .iter()
            .any(|f| f.id == "nvidia" || f.id == "open");

        let dx_state = Rc::new(Cell::new(initial_dx));
        let nvidia_state = Rc::new(Cell::new(initial_nvidia));

        let recompute = {
            let family = family.clone();
            let selected_features = selected_features.clone();
            let selected_stream = selected_stream.clone();
            let target_row = target_row.clone();
            let dx_state = dx_state.clone();
            let nvidia_state = nvidia_state.clone();
            let on_target_change = on_target_change.clone();
            move || {
                let stream = selected_stream.borrow().clone();
                let (feats, target) = resolve_dx_nvidia_with_stream(
                    &family,
                    dx_state.get(),
                    nvidia_state.get(),
                    &stream,
                );
                *selected_features.borrow_mut() = feats;
                match &target {
                    Some(t) => target_row.set_subtitle(&format!("{} (resolved)", t.image)),
                    None => {
                        target_row.set_subtitle("(combination doesn't match any published image)")
                    }
                }
                // Refetch the calendar for the resolved target image so the
                // day grid reflects builds for THAT image (the user just
                // toggled DX/NVIDIA/stream — they expect the calendar to
                // follow). Debounced inside on_target_change.
                on_target_change(target);
            }
        };

        if supports_dx {
            let row = adw::SwitchRow::builder()
                .title("Developer Mode")
                .subtitle("Container tools, IDEs, and language SDKs")
                .active(initial_dx)
                .build();
            let recompute_ = recompute.clone();
            let dx_state_ = dx_state.clone();
            row.connect_active_notify(move |sr| {
                dx_state_.set(sr.is_active());
                recompute_();
            });
            features_group.add(&row);
        }

        if supports_nvidia {
            // Adaptive subtitle: if we detect NVIDIA hardware, call that out so
            // the user understands why this toggle matters for them
            // specifically. Otherwise show the generic description.
            let nvidia_subtitle = if crate::gpu::has_nvidia_gpu() {
                "NVIDIA GPU detected — keep on for hardware-accelerated graphics (open kernel modules preferred)"
            } else {
                "Picks the open kernel modules where available, falls back to the proprietary driver"
            };
            let row = adw::SwitchRow::builder()
                .title("NVIDIA drivers")
                .subtitle(nvidia_subtitle)
                .active(initial_nvidia)
                .build();
            // Guard prevents the warn-and-revert path from re-firing this
            // handler when we programmatically flip the switch back to
            // its previous state after a "Cancel" on the warning dialog.
            let nvidia_guard: Rc<Cell<bool>> = Rc::new(Cell::new(false));
            let recompute_ = recompute.clone();
            let nvidia_state_ = nvidia_state.clone();
            let guard_ = nvidia_guard.clone();
            row.connect_active_notify(move |sr| {
                if guard_.get() {
                    return;
                }
                let new_value = sr.is_active();
                let prev_value = nvidia_state_.get();
                let turning_off = prev_value && !new_value;
                if turning_off && crate::gpu::has_nvidia_gpu() {
                    let confirm = adw::AlertDialog::builder()
                        .heading("NVIDIA hardware detected")
                        .body("Your system has an NVIDIA GPU. Switching to a non-NVIDIA image will fall back to software rendering or the open Mesa driver — graphics performance will degrade significantly until you switch back.\n\nContinue?")
                        .build();
                    confirm.add_response("cancel", "_Cancel");
                    confirm.add_response("disable", "_Disable anyway");
                    confirm.set_response_appearance(
                        "disable",
                        adw::ResponseAppearance::Destructive,
                    );
                    confirm.set_default_response(Some("cancel"));
                    confirm.set_close_response("cancel");

                    let sr_clone = sr.clone();
                    let nvidia_state_clone = nvidia_state_.clone();
                    let recompute_clone = recompute_.clone();
                    let guard_clone = guard_.clone();
                    confirm.connect_response(None, move |_, response| {
                        if response == "disable" {
                            nvidia_state_clone.set(false);
                            recompute_clone();
                        } else {
                            // Revert the switch back to on without re-firing
                            // the handler (would re-trigger this dialog).
                            guard_clone.set(true);
                            sr_clone.set_active(true);
                            guard_clone.set(false);
                        }
                    });
                    confirm.present(None::<&gtk::Widget>);
                    // Don't apply yet — wait for the response callback.
                    return;
                }
                nvidia_state_.set(new_value);
                recompute_();
            });
            features_group.add(&row);
        }

        // Wire up stream selection to recompute target image
        let selected_stream_clone = selected_stream.clone();
        let recompute_clone = recompute.clone();
        stream_row.connect_selected_notify(move |combo| {
            if let Some(item) = combo.selected_item() {
                if let Ok(obj) = item.downcast::<gtk::StringObject>() {
                    let stream_str = obj.string();
                    *selected_stream_clone.borrow_mut() = stream_str.to_string();
                    recompute_clone();
                }
            }
        });

        *current_family.borrow_mut() = Some(family);
        glib::ControlFlow::Break
    });
}







// ── Helpers ──────────────────────────────────────────────────────────────────



fn inject_calendar_css() {
    let css = gtk::CssProvider::new();
    css.load_from_string(
        r#"
        .day-btn {
            min-width: 30px;
            min-height: 30px;
            padding: 0;
            border-radius: 15px;
            font-size: 0.82em;
        }
        .day-btn:not(:sensitive) { opacity: 0.3; }
        .day-available           { color: @accent_color; font-weight: bold; }
        .day-current             { background-color: @accent_bg_color; color: @accent_fg_color; }
        .day-selected:not(.day-current) {
            outline: 2px solid @accent_color;
            outline-offset: -2px;
        }
        .day-today label { text-decoration: underline; }
        "#,
    );
    gtk::style_context_add_provider_for_display(
        &gtk::gdk::Display::default().expect("display"),
        &css,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

// ── Message type for background fetch ────────────────────────────────────────

enum FetchResult {
    Ok(Vec<ImageVersion>),
    #[allow(dead_code)]
    Err(String),
    DetectFailed,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Once;

    static INIT: Once = Once::new();

    /// Tests need a process-wide UpdaterService since resolve_target_ref calls
    /// service::global(). Install the default BootcUpdaterService once;
    /// service::init() will panic on the second call so guard with Once.
    fn ensure_service() {
        INIT.call_once(|| {
            service::init(service::BootcUpdaterService::new());
        });
    }

    fn bluefin_stable_info() -> FamilyInfo {
        // The features list isn't consulted by resolve_target_ref — the
        // service routes through KNOWN_FAMILIES via family.name — so we leave
        // it empty here. Service-level tests in service::tests cover the
        // feature-resolution paths.
        FamilyInfo {
            name: "Bluefin Stable".to_string(),
            base_image: "bluefin".to_string(),
            streams: vec![],
            features: vec![],
        }
    }

    #[test]
    fn resolve_passthrough_with_no_family() {
        ensure_service();
        let r = resolve_target_ref(
            "ghcr.io/ublue-os/bluefin:stable-daily-43.20260527",
            None,
            &[],
        );
        assert_eq!(r, "ghcr.io/ublue-os/bluefin:stable-daily-43.20260527");
    }

    #[test]
    fn resolve_no_features_keeps_base_image() {
        ensure_service();
        let fam = bluefin_stable_info();
        let r = resolve_target_ref(
            "ghcr.io/ublue-os/bluefin:stable-daily-43.20260527",
            Some(&fam),
            &[],
        );
        assert_eq!(r, "ghcr.io/ublue-os/bluefin:stable-daily-43.20260527");
    }

    #[test]
    fn resolve_swaps_image_to_nvidia_variant() {
        ensure_service();
        let fam = bluefin_stable_info();
        let r = resolve_target_ref(
            "ghcr.io/ublue-os/bluefin:stable-daily-43.20260527",
            Some(&fam),
            &["nvidia".to_string()],
        );
        assert_eq!(
            r,
            "ghcr.io/ublue-os/bluefin-nvidia:stable-daily-43.20260527"
        );
    }

    #[test]
    fn resolve_combines_dx_and_nvidia() {
        ensure_service();
        let fam = bluefin_stable_info();
        let r = resolve_target_ref(
            "ghcr.io/ublue-os/bluefin:stable",
            Some(&fam),
            &["dx".to_string(), "nvidia".to_string()],
        );
        assert_eq!(r, "ghcr.io/ublue-os/bluefin-dx-nvidia:stable");
    }

    #[test]
    fn resolve_unpublished_combination_falls_back() {
        ensure_service();
        // "open" alone (without nvidia) isn't a published image — keep the
        // original ref so we don't pkexec a bogus bootc switch.
        let fam = bluefin_stable_info();
        let original = "ghcr.io/ublue-os/bluefin:stable";
        let r = resolve_target_ref(original, Some(&fam), &["open".to_string()]);
        assert_eq!(r, original);
    }

    #[test]
    fn resolve_handles_missing_tag() {
        ensure_service();
        // Defensive: a malformed ref with no ':' should pass through.
        let fam = bluefin_stable_info();
        let r = resolve_target_ref(
            "ghcr.io/ublue-os/bluefin",
            Some(&fam),
            &["nvidia".to_string()],
        );
        assert_eq!(r, "ghcr.io/ublue-os/bluefin");
    }

    // ── resolve_dx_nvidia ────────────────────────────────────────────────
    // Pins the toggle-to-features fallback chain so the two-switch UI
    // (Developer Mode + NVIDIA) lands on the right image per family.

    fn dakota_info() -> FamilyInfo {
        FamilyInfo {
            name: "Bluefin Dakota".to_string(),
            base_image: "dakota".to_string(),
            streams: vec![],
            features: vec![],
        }
    }

    fn bazzite_kde_info() -> FamilyInfo {
        FamilyInfo {
            name: "Bazzite KDE".to_string(),
            base_image: "bazzite".to_string(),
            streams: vec![],
            features: vec![],
        }
    }

    #[test]
    fn dx_nvidia_both_off_returns_base() {
        ensure_service();
        let (feats, img) = resolve_dx_nvidia(&bluefin_stable_info(), false, false);
        assert_eq!(feats, Vec::<String>::new());
        assert_eq!(img.unwrap().image, "bluefin");
    }

    #[test]
    fn dx_nvidia_dx_only_resolves_dx() {
        ensure_service();
        let (feats, img) = resolve_dx_nvidia(&bluefin_stable_info(), true, false);
        assert_eq!(feats, vec!["dx".to_string()]);
        assert_eq!(img.unwrap().image, "bluefin-dx");
    }

    #[test]
    fn dx_nvidia_nvidia_only_on_bluefin_prefers_open() {
        ensure_service();
        // Bluefin's plain -nvidia is deprecated; the toggle should land on
        // -nvidia-open (the current variant).
        let (feats, img) = resolve_dx_nvidia(&bluefin_stable_info(), false, true);
        assert_eq!(feats, vec!["nvidia".to_string(), "open".to_string()]);
        assert_eq!(img.unwrap().image, "bluefin-nvidia-open");
    }

    #[test]
    fn dx_nvidia_both_on_bluefin_yields_dx_nvidia_open() {
        ensure_service();
        let (feats, img) = resolve_dx_nvidia(&bluefin_stable_info(), true, true);
        assert_eq!(
            feats,
            vec!["dx".to_string(), "nvidia".to_string(), "open".to_string()]
        );
        assert_eq!(img.unwrap().image, "bluefin-dx-nvidia-open");
    }

    #[test]
    fn dx_nvidia_nvidia_on_dakota_falls_back_to_plain_nvidia() {
        ensure_service();
        // Dakota has no -nvidia-open variant published; the first probe
        // (`["nvidia", "open"]`) misses, the fallback (`["nvidia"]`)
        // lands on dakota-nvidia.
        let (feats, img) = resolve_dx_nvidia(&dakota_info(), false, true);
        assert_eq!(feats, vec!["nvidia".to_string()]);
        assert_eq!(img.unwrap().image, "dakota-nvidia");
    }

    #[test]
    fn dx_nvidia_nvidia_on_bazzite_prefers_open() {
        ensure_service();
        // Bazzite KDE publishes both bazzite-nvidia AND bazzite-nvidia-open.
        // The resolver's -open-first preference picks the latter. Pin this
        // so a future KNOWN_FAMILIES trim (dropping plain -nvidia) doesn't
        // silently change which variant new users land on.
        let (feats, img) = resolve_dx_nvidia(&bazzite_kde_info(), false, true);
        assert_eq!(feats, vec!["nvidia".to_string(), "open".to_string()]);
        assert_eq!(img.unwrap().image, "bazzite-nvidia-open");
    }

    #[test]
    fn dx_nvidia_dx_on_dakota_has_no_published_image() {
        ensure_service();
        // Dakota has no -dx variant — the resolver returns None, the UI
        // shows the "doesn't match any published image" subtitle.
        let (feats, img) = resolve_dx_nvidia(&dakota_info(), true, false);
        assert_eq!(feats, vec!["dx".to_string()]);
        assert!(img.is_none());
    }

    // ── derive_initial_toggle_state ──────────────────────────────────────
    // The dialog must open with toggles reflecting the booted variant so a
    // user already on -dx-nvidia-open doesn't think rebasing would downgrade.

    fn image_ref(image: &str) -> service::ImageRef {
        service::ImageRef {
            registry: "ghcr.io".to_string(),
            org: "ublue-os".to_string(),
            image: image.to_string(),
            tag: "stable".to_string(),
            digest: String::new(),
        }
    }

    #[test]
    fn initial_toggles_no_image_returns_off() {
        let (dx, nvidia) = derive_initial_toggle_state(&bluefin_stable_info(), None);
        assert!(!dx);
        assert!(!nvidia);
    }

    #[test]
    fn initial_toggles_base_image_returns_off() {
        let img = image_ref("bluefin");
        let (dx, nvidia) = derive_initial_toggle_state(&bluefin_stable_info(), Some(&img));
        assert!(!dx);
        assert!(!nvidia);
    }

    #[test]
    fn initial_toggles_dx_only() {
        let img = image_ref("bluefin-dx");
        let (dx, nvidia) = derive_initial_toggle_state(&bluefin_stable_info(), Some(&img));
        assert!(dx);
        assert!(!nvidia);
    }

    #[test]
    fn initial_toggles_plain_nvidia() {
        let img = image_ref("bluefin-nvidia");
        let (dx, nvidia) = derive_initial_toggle_state(&bluefin_stable_info(), Some(&img));
        assert!(!dx);
        assert!(nvidia);
    }

    #[test]
    fn initial_toggles_nvidia_open() {
        let img = image_ref("bluefin-nvidia-open");
        let (dx, nvidia) = derive_initial_toggle_state(&bluefin_stable_info(), Some(&img));
        assert!(!dx);
        assert!(nvidia);
    }

    #[test]
    fn initial_toggles_dx_and_nvidia_open() {
        let img = image_ref("bluefin-dx-nvidia-open");
        let (dx, nvidia) = derive_initial_toggle_state(&bluefin_stable_info(), Some(&img));
        assert!(dx);
        assert!(nvidia);
    }

    #[test]
    fn initial_toggles_unrelated_image_returns_off() {
        // Booted image's name doesn't share the family's base prefix — could
        // happen if KNOWN_FAMILIES drifts vs reality. Show off rather than
        // lie about state.
        let img = image_ref("aurora-dx");
        let (dx, nvidia) = derive_initial_toggle_state(&bluefin_stable_info(), Some(&img));
        assert!(!dx);
        assert!(!nvidia);
    }

    #[test]
    fn initial_toggles_dakota_plain_nvidia() {
        // Dakota's -nvidia variant — no -open suffix, but still "NVIDIA on".
        let img = service::ImageRef {
            registry: "ghcr.io".to_string(),
            org: "projectbluefin".to_string(),
            image: "dakota-nvidia".to_string(),
            tag: "latest".to_string(),
            digest: String::new(),
        };
        let (dx, nvidia) = derive_initial_toggle_state(&dakota_info(), Some(&img));
        assert!(!dx);
        assert!(nvidia);
    }
}
