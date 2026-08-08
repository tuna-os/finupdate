//! Preferences dialog for finupdate.
//!
//! Built as a standalone function following the `show_shortcuts_window` pattern
//! in `app.rs` — no full relm4 component needed since it's a one-shot modal.
//!
//! Settings are saved directly from signal handlers via `Rc<RefCell<Settings>>`
//! shared across all closures. This ensures each closure always reads the latest
//! values written by other closures, preventing stale-clone overwrite bugs.
//!
//! When the dialog closes, `on_change` is called with the final `Settings` value
//! so the parent `App` component can update its in-memory copy without requiring
//! the user to restart.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use relm4::gtk;
use relm4::gtk::prelude::AccessibleExt;

use crate::app::UpdatesPanelMsg;
use crate::settings::{Settings, UpdateInterval};
use crate::uupd_compat::{self, UupdConfig};

/// Build and present the Advanced dialog attached to `parent`.
///
/// `on_change` is called when the dialog closes, passing the final settings
/// snapshot so the caller can update its own state.
///
/// `app_sender` lets the dialog's action rows dispatch AppMsg variants
/// directly — Image Source / Image History / Rebase / Powerwash / Factory
/// Reset all live as panel-action rows in the dialog now (replacing the
/// hamburger-menu approach, per gnome-control-center convention).
pub fn show_preferences(
    parent: &gtk::Widget,
    mut settings: Settings,
    app_sender: relm4::Sender<UpdatesPanelMsg>,
    on_change: impl Fn(Settings) + 'static,
) {
    // Only read the timer state if uupd is actually installed.
    if uupd_compat::is_uupd_installed() {
        if let Some(auto_updates) = uupd_compat::is_uupd_timer_active() {
            settings.auto_updates = auto_updates;
        }
    }

    let shared = Rc::new(RefCell::new(settings));

    let dialog = adw::PreferencesDialog::new();
    // Title says "Advanced" rather than "Preferences" because this dialog
    // now hosts actions (Powerwash, Factory Reset, etc.) in addition to
    // settings — "Preferences" undersold its role after the main page was
    // pared back to just Check + Automatic Updates.
    dialog.set_title("Advanced");
    // The dialog spans several groups plus a nested Automatic Updates subpage
    // with eight-plus rows — well past the point where HIG expects search
    // (spec §9.4). This was disabled when the dialog was much smaller.
    dialog.set_search_enabled(true);

    // ── Page ─────────────────────────────────────────────────────────────
    let page = adw::PreferencesPage::builder()
        .title("General")
        .icon_name("preferences-system-symbolic")
        .build();

    build_updates_group(&page, &shared, &dialog);
    build_network_group(&page, &shared);
    build_system_group(&page, &dialog, app_sender.clone());
    // Developer-mode + simulator are CLI-only now (`finupdate --dev-mode`,
    // `finupdate --sim=<scenario>`); the toggle no longer appears in this
    // dialog. The build_developer_group fn is kept around in case dev
    // testing wants it back, but it isn't called.

    dialog.add(&page);

    // Notify the caller when the dialog is dismissed so in-memory settings stay current.
    let shared_close = shared.clone();
    dialog.connect_closed(move |_| {
        on_change(shared_close.borrow().clone());
    });

    dialog.present(Some(parent));
}

// ── Updates group ─────────────────────────────────────────────────────────────

fn build_updates_group(
    page: &adw::PreferencesPage,
    shared: &Rc<RefCell<Settings>>,
    dialog: &adw::PreferencesDialog,
) {
    let group = adw::PreferencesGroup::builder()
        .title("Automatic Updates")
        .description("Configure how and when finupdate checks for updates")
        .build();

    // Auto-update toggle — only shown when uupd is installed on the host.
    let auto_row = {
        let s = shared.borrow();
        adw::SwitchRow::builder()
            .title("_Automatic Background Updates")
            .use_underline(true)
            .subtitle("Allow uupd to run on its daily systemd timer")
            .active(s.auto_updates)
            .visible(uupd_compat::is_uupd_installed())
            .build()
    };

    {
        let shared = shared.clone();
        auto_row.connect_active_notify(move |row| {
            let active = row.is_active();
            shared.borrow_mut().auto_updates = active;
            shared.borrow().save();

            std::thread::spawn(move || {
                crate::runtime::block_on(async move {
                    if let Err(e) = uupd_compat::set_uupd_timer(active).await {
                        tracing::warn!("Failed to toggle uupd.timer: {}", e);
                    }
                });
            });
        });
    }
    group.add(&auto_row);

    // "Include app updates" — when off, the updater only refreshes the bootc
    // image and skips Flatpak / Homebrew / Distrobox. Default on for parity
    // with uupd's standard behaviour; off mirrors macOS's "system only"
    // update split and lets power users manage app updates separately.
    let apps_row = {
        let s = shared.borrow();
        adw::SwitchRow::builder()
            .title("_Include App Updates")
            .use_underline(true)
            .subtitle(
                "Refresh Flatpaks, Homebrew, and Distrobox containers alongside the bootc image",
            )
            .active(s.include_app_updates)
            .build()
    };
    {
        let shared = shared.clone();
        apps_row.connect_active_notify(move |row| {
            shared.borrow_mut().include_app_updates = row.is_active();
            shared.borrow().save();
        });
    }
    group.add(&apps_row);

    // "Configure automatic updates" — pushes a subpage that edits /etc/uupd/config.json.
    // Only shown when uupd is installed (the config file only matters then).
    let configure_row = adw::ActionRow::builder()
        .title("_Configure Automatic Updates")
        .use_underline(true)
        .subtitle("Hardware checks and per-module toggles in /etc/uupd/config.json")
        .activatable(true)
        .visible(uupd_compat::is_uupd_installed())
        .build();
    let chevron = gtk::Image::from_icon_name("go-next-symbolic");
    chevron.set_valign(gtk::Align::Center);
    configure_row.add_suffix(&chevron);
    {
        let dialog = dialog.clone();
        configure_row.connect_activated(move |_| {
            let subpage = build_uupd_config_subpage();
            dialog.push_subpage(&subpage);
        });
    }
    group.add(&configure_row);

    // Update interval combo
    let interval_row = {
        let s = shared.borrow();
        let row = adw::ComboRow::builder()
            .title("Check _Interval")
            .use_underline(true)
            .subtitle("How often automatic updates run")
            .build();
        let model = gtk::StringList::new(&["Hourly", "Daily", "Weekly", "Custom"]);
        row.set_model(Some(&model));
        row.set_selected(s.update_interval.to_index());
        row
    };

    // Custom interval revealer (shown only when "Custom" is selected)
    let custom_revealer = gtk::Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::SlideDown)
        .transition_duration(200)
        .reveal_child(shared.borrow().update_interval == UpdateInterval::Custom)
        .build();

    let custom_action_row = adw::ActionRow::builder()
        .title("Custom Inter_val")
        .use_underline(true)
        .subtitle("Number of hours between update checks")
        .build();

    let spin_adj = gtk::Adjustment::builder()
        .lower(1.0)
        .upper(168.0)
        .step_increment(1.0)
        .value(shared.borrow().custom_interval_hours as f64)
        .build();
    let custom_spin = gtk::SpinButton::builder()
        .adjustment(&spin_adj)
        .valign(gtk::Align::Center)
        .build();
    custom_action_row.add_suffix(&custom_spin);

    let custom_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    custom_box.append(&custom_action_row);
    custom_revealer.set_child(Some(&custom_box));

    {
        let shared = shared.clone();
        let custom_revealer = custom_revealer.clone();
        interval_row.connect_selected_notify(move |row| {
            let interval = UpdateInterval::from_index(row.selected());
            custom_revealer.set_reveal_child(interval == UpdateInterval::Custom);
            shared.borrow_mut().update_interval = interval;
            shared.borrow().save();
        });
    }
    {
        let shared = shared.clone();
        custom_spin.connect_value_changed(move |spin| {
            shared.borrow_mut().custom_interval_hours = spin.value() as u32;
            shared.borrow().save();
        });
    }

    group.add(&interval_row);
    group.add(&custom_revealer);

    page.add(&group);
}

// ── Network group ─────────────────────────────────────────────────────────────

fn build_network_group(page: &adw::PreferencesPage, shared: &Rc<RefCell<Settings>>) {
    let group = adw::PreferencesGroup::builder()
        .title("Network")
        .description(
            "Control update behavior based on connection type. \
             Manual updates always work regardless of these settings.",
        )
        .build();

    let metered_row = {
        let s = shared.borrow();
        adw::SwitchRow::builder()
            .title("_Pause on Metered Connections")
            .use_underline(true)
            .subtitle("Suspend automatic updates on limited or cellular connections")
            .active(s.pause_on_metered)
            .build()
    };

    // Connection status badge — only visible when the toggle is on.
    let status_revealer = gtk::Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::SlideDown)
        .transition_duration(200)
        .reveal_child(shared.borrow().pause_on_metered)
        .build();

    let monitor = gtk::gio::NetworkMonitor::default();
    let is_metered = monitor.is_network_metered();
    let (status_text, css_class) = if is_metered {
        (
            "⚠  Metered connection active — automatic updates are paused",
            "warning",
        )
    } else {
        (
            "✓  Unmetered connection — automatic updates will run normally",
            "success",
        )
    };

    let status_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .margin_start(12)
        .margin_end(12)
        .margin_top(4)
        .margin_bottom(8)
        .build();
    let status_label = gtk::Label::new(Some(status_text));
    status_label.add_css_class("caption");
    status_label.add_css_class(css_class);
    status_label.set_halign(gtk::Align::Start);
    status_box.append(&status_label);
    status_revealer.set_child(Some(&status_box));

    {
        let shared = shared.clone();
        let status_revealer = status_revealer.clone();
        metered_row.connect_active_notify(move |row| {
            let active = row.is_active();
            status_revealer.set_reveal_child(active);
            shared.borrow_mut().pause_on_metered = active;
            shared.borrow().save();
        });
    }

    group.add(&metered_row);
    group.add(&status_revealer);

    page.add(&group);
}

// ── System group (panel-action rows) ─────────────────────────────────────────
//
// Hosts the destructive reset operations (Powerwash, Factory Reset).
// Image Source and Rebase/Rollback are now surfaced on the main page.

fn build_system_group(
    page: &adw::PreferencesPage,
    dialog: &adw::PreferencesDialog,
    sender: relm4::Sender<UpdatesPanelMsg>,
) {
    // Helper: build a chevron-suffixed activatable ActionRow with the given
    // title / subtitle. The on-activate callback receives nothing; the caller
    // captures the dispatch sender.
    let make_row = |title: &str, subtitle: &str| -> adw::ActionRow {
        let row = adw::ActionRow::builder()
            .title(title)
            .subtitle(subtitle)
            .activatable(true)
            .build();
        row.set_accessible_role(gtk::AccessibleRole::Button);
        let chev = gtk::Image::from_icon_name("go-next-symbolic");
        chev.add_css_class("dim-label");
        row.add_suffix(&chev);
        row
    };

    // ── Image group ──────────────────────────────────────────────────────
    // These three pages exist as AdwNavigationPages in status_view but had no
    // entry point: ShowStatusPage was only ever emitted with "main", and the
    // changelog was reachable solely through the update banner's "What's New"
    // button — which only appears when an update is available. On an
    // up-to-date system the image history and changelog were therefore
    // unreachable, even though the Advanced row advertised them.
    let image_group = adw::PreferencesGroup::builder()
        .title("Image")
        .description("Where this system's OS image comes from, and what has changed")
        .build();

    for (title, subtitle, tag) in [
        (
            "Image _Source",
            "Registry, tag, and signing policy",
            "source",
        ),
        (
            "Image _History",
            "Previous deployments, rollback, and pinning",
            "history",
        ),
        (
            "What's _New",
            "Changelog and package differences for this image",
            "changelog",
        ),
    ] {
        let row = make_row(title, subtitle);
        row.set_use_underline(true);
        let s = sender.clone();
        let tag = tag.to_string();
        // Close the dialog first. These pages are pushed onto the *main
        // window's* AdwNavigationView, so navigating while this modal is still
        // up moves the view behind it and the user sees nothing happen.
        let dlg = dialog.clone();
        row.connect_activated(move |_| {
            // force_close(), not close(): AdwDialog::close() is a *request* that
            // emits ::close-attempt and can be refused, and it was being ignored
            // here — the row navigated the main window while the modal stayed up,
            // so nothing appeared to happen.
            dlg.force_close();
            let _ = s.send(UpdatesPanelMsg::ShowStatusPage(tag.clone()));
        });
        image_group.add(&row);
    }
    page.add(&image_group);

    // ── Reset group (destructive) ────────────────────────────────────────
    // Separate group so the destructive actions are visually segregated from
    // the navigational rows above. Factory Reset gets the destructive-title
    // CSS class (red label) the same way the old main-page row did.
    let reset_group = adw::PreferencesGroup::builder()
        .title("Reset")
        .description(
            "Reversible: powerwash keeps your files. Destructive: factory reset erases everything.",
        )
        .build();

    let powerwash_row = make_row(
        "Powerwash",
        "Uninstall apps and reset containers; keep your home folder",
    );
    {
        let s = sender.clone();
        powerwash_row.connect_activated(move |_| {
            s.emit(UpdatesPanelMsg::TriggerPowerwash);
            // Don't close the dialog — the confirmation modal opens on top
            // and the user may want to back out and inspect other rows.
        });
    }
    reset_group.add(&powerwash_row);

    let factory_row = make_row(
        "Factory Reset",
        "Erase everything and redeploy the original image",
    );
    factory_row.add_css_class("destructive-title");
    {
        let s = sender.clone();
        factory_row.connect_activated(move |_| {
            s.emit(UpdatesPanelMsg::TriggerFactoryReset);
        });
    }
    reset_group.add(&factory_row);

    page.add(&reset_group);
}

// ── Developer group ───────────────────────────────────────────────────────────
//
// Kept around behind an explicit `#[allow(dead_code)]` because re-enabling
// it from the dialog is one call site (build_updates_group above) — handy
// when iterating on dev_mode UX without re-deriving the SwitchRow plumbing.

#[allow(dead_code)]
fn build_developer_group(page: &adw::PreferencesPage, shared: &Rc<RefCell<Settings>>) {
    let group = adw::PreferencesGroup::builder()
        .title("Developer")
        .description(
            "Tools for UI development and testing. \
             These settings do not affect normal operation.",
        )
        .build();

    let dev_row = {
        let s = shared.borrow();
        adw::SwitchRow::builder()
            .title("_Developer Mode")
            .use_underline(true)
            .subtitle("Simulate updates without running uupd or touching the real system")
            .active(s.dev_mode)
            .build()
    };

    {
        let shared = shared.clone();
        dev_row.connect_active_notify(move |row| {
            shared.borrow_mut().dev_mode = row.is_active();
            shared.borrow().save();
        });
    }
    group.add(&dev_row);

    page.add(&group);
}

// ── uupd config subpage ───────────────────────────────────────────────────────
//
// Edits /etc/uupd/config.json — hardware-check thresholds (battery / CPU / RAM /
// network) and per-module enable toggles. Writes happen through a single pkexec
// invocation when the user hits "Apply".

fn build_uupd_config_subpage() -> adw::NavigationPage {
    let config = Rc::new(RefCell::new(uupd_compat::read_config()));

    let page = adw::PreferencesPage::new();

    // ── Hardware checks ─────────────────────────────────────────────────
    let hw_group = adw::PreferencesGroup::builder()
        .title("Hardware Checks")
        .description(
            "Skip automatic updates when the system is busy or on battery. \
             Manual updates always run regardless.",
        )
        .build();

    let enable_hw_row = adw::SwitchRow::builder()
        .title("_Enable Hardware Checks")
        .use_underline(true)
        .subtitle("Honor the thresholds below before running automatic updates")
        .active(config.borrow().checks.hardware.enable)
        .build();
    {
        let config = config.clone();
        enable_hw_row.connect_active_notify(move |row| {
            config.borrow_mut().checks.hardware.enable = row.is_active();
        });
    }
    hw_group.add(&enable_hw_row);

    let bat_row = make_spin_row(
        "Minimum Battery",
        "Skip updates if battery is below this percent",
        0.0,
        100.0,
        config.borrow().checks.hardware.bat_min_percent as f64,
        "%",
    );
    {
        let config = config.clone();
        bat_row.1.connect_value_changed(move |s| {
            config.borrow_mut().checks.hardware.bat_min_percent = s.value() as u32;
        });
    }
    hw_group.add(&bat_row.0);

    let cpu_row = make_spin_row(
        "Maximum CPU Load",
        "Skip updates if CPU usage is above this percent",
        0.0,
        100.0,
        config.borrow().checks.hardware.cpu_max_percent as f64,
        "%",
    );
    {
        let config = config.clone();
        cpu_row.1.connect_value_changed(move |s| {
            config.borrow_mut().checks.hardware.cpu_max_percent = s.value() as u32;
        });
    }
    hw_group.add(&cpu_row.0);

    let mem_row = make_spin_row(
        "Maximum Memory Use",
        "Skip updates if RAM usage is above this percent",
        0.0,
        100.0,
        config.borrow().checks.hardware.mem_max_percent as f64,
        "%",
    );
    {
        let config = config.clone();
        mem_row.1.connect_value_changed(move |s| {
            config.borrow_mut().checks.hardware.mem_max_percent = s.value() as u32;
        });
    }
    hw_group.add(&mem_row.0);

    let net_row = make_spin_row(
        "Maximum Network Activity",
        "Skip updates if traffic is above this many bytes/second",
        0.0,
        100_000_000.0,
        config.borrow().checks.hardware.net_max_bytes as f64,
        "B/s",
    );
    {
        let config = config.clone();
        net_row.1.connect_value_changed(move |s| {
            config.borrow_mut().checks.hardware.net_max_bytes = s.value() as u64;
        });
    }
    hw_group.add(&net_row.0);

    page.add(&hw_group);

    // ── Modules ─────────────────────────────────────────────────────────
    let mod_group = adw::PreferencesGroup::builder()
        .title("Modules")
        .description("Enable or disable specific update modules")
        .build();

    let system_row = module_switch_row(
        "System",
        "OS image updates via bootc / rpm-ostree",
        !config.borrow().modules.system.disable,
    );
    {
        let config = config.clone();
        system_row.connect_active_notify(move |row| {
            config.borrow_mut().modules.system.disable = !row.is_active();
        });
    }
    mod_group.add(&system_row);

    let flatpak_row = module_switch_row(
        "Flatpak",
        "Sandboxed applications",
        !config.borrow().modules.flatpak.disable,
    );
    {
        let config = config.clone();
        flatpak_row.connect_active_notify(move |row| {
            config.borrow_mut().modules.flatpak.disable = !row.is_active();
        });
    }
    mod_group.add(&flatpak_row);

    let brew_row = module_switch_row(
        "Brew",
        "Homebrew packages",
        !config.borrow().modules.brew.disable,
    );
    {
        let config = config.clone();
        brew_row.connect_active_notify(move |row| {
            config.borrow_mut().modules.brew.disable = !row.is_active();
        });
    }
    mod_group.add(&brew_row);

    let distrobox_row = module_switch_row(
        "Distrobox",
        "Containers managed by distrobox",
        !config.borrow().modules.distrobox.disable,
    );
    {
        let config = config.clone();
        distrobox_row.connect_active_notify(move |row| {
            config.borrow_mut().modules.distrobox.disable = !row.is_active();
        });
    }
    mod_group.add(&distrobox_row);

    page.add(&mod_group);

    // ── Save row ────────────────────────────────────────────────────────
    let save_group = adw::PreferencesGroup::new();
    let save_button = gtk::Button::builder()
        .label("Apply Changes")
        .halign(gtk::Align::End)
        .css_classes(["suggested-action", "pill"])
        .build();
    let save_row = adw::ActionRow::builder()
        .title("_Save to /etc/uupd/config.json")
        .use_underline(true)
        .subtitle("Requires administrator authentication")
        .build();
    save_row.add_suffix(&save_button);
    save_group.add(&save_row);
    page.add(&save_group);

    let nav_page = adw::NavigationPage::builder()
        .title("Automatic Updates")
        .child(&page)
        .build();

    {
        let config = config.clone();
        save_button.connect_clicked(move |btn| {
            let snapshot: UupdConfig = config.borrow().clone();
            btn.set_sensitive(false);
            btn.set_label("Saving…");
            // GTK widgets are !Send, so we can't move `btn` into a std::thread.
            // Use glib's local async executor — it runs on the GTK main thread
            // and can await futures without blocking the UI.
            let btn = btn.clone();
            gtk::glib::spawn_future_local(async move {
                let result = uupd_compat::write_config(&snapshot).await;
                btn.set_sensitive(true);
                match result {
                    Ok(()) => btn.set_label("Saved ✓"),
                    Err(e) => {
                        tracing::warn!("uupd config save failed: {e}");
                        btn.set_label("Save failed");
                    }
                }
            });
        });
    }

    nav_page
}

/// Helper: build an AdwActionRow with a SpinButton suffix and a unit label.
fn make_spin_row(
    title: &str,
    subtitle: &str,
    min: f64,
    max: f64,
    value: f64,
    suffix: &str,
) -> (adw::ActionRow, gtk::SpinButton) {
    let row = adw::ActionRow::builder()
        .title(title)
        .subtitle(subtitle)
        .build();
    let adj = gtk::Adjustment::builder()
        .lower(min)
        .upper(max)
        .step_increment(1.0)
        .value(value)
        .build();
    let spin = gtk::SpinButton::builder()
        .adjustment(&adj)
        .valign(gtk::Align::Center)
        .build();
    let unit_label = gtk::Label::builder()
        .label(suffix)
        .valign(gtk::Align::Center)
        .css_classes(["dim-label"])
        .build();
    row.add_suffix(&spin);
    row.add_suffix(&unit_label);
    (row, spin)
}

fn module_switch_row(title: &str, subtitle: &str, enabled: bool) -> adw::SwitchRow {
    adw::SwitchRow::builder()
        .title(title)
        .subtitle(subtitle)
        .active(enabled)
        .build()
}
