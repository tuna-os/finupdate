//! Status view component — the main content area of the app.
//!
//! Pattern: State-driven view switching
//! Uses a `gtk::Stack` to switch between different visual states:
//! - Idle: Card-based overview with hero, update banner, and settings actions
//! - Updating: Progress indicator + image badge + UpdateList + live log + timer + cancel
//! - Complete: Success status page with reboot option
//! - UpToDate: "You're already up to date" status page
//! - Error: Error status page with retry option

use adw::prelude::*;
use relm4::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use crate::app::{AppState, PreflightStatus};
use crate::registry_client::ImageVersion;
use crate::settings::Settings;
use crate::ui::changelog::{SbomStatus, spawn_changelog_fetch};
use crate::ui::history_list::{MockDeployment, get_sample_deployments, rebuild_history_list};
use crate::ui::settings_io::read_auto_updates_enabled;

mod changelog_page;
mod dialogs;
mod helpers;
mod idle;
mod source_page;
mod updating;

#[cfg(test)]
mod tests;

use helpers::{VERSION_MAX_CHARS, allow_narrow, version_change_class, version_diff_box};
use source_page::build_source_page;
use updating::build_updating_page;

// Host introspection moved to `bootc_probe`; glob-imported so the call
// sites here — and the unit tests — keep referring to these by their bare names.
use super::bootc_probe::*;
use crate::ui::log_view::{LogView, LogViewInput};
use crate::ui::segmented_progress::{SegmentedProgress, same_segment};
use crate::ui::update_list::{UpdateList, UpdateListInput};

/// Input messages for the StatusView component.
#[derive(Debug)]
pub enum StatusViewInput {
    /// Parent tells us the app state changed.
    StateChanged(AppState),
    /// Append a log line to the output view.
    AppendLog(String),
    /// Clear the log buffer.
    ClearLog,
    /// Timer tick — update elapsed time display.
    TimerTick,
    /// Result of the startup preflight update check.
    PreflightResult(PreflightStatus),
    /// Dismiss the staged reboot banner.
    DismissBanner,
    /// Hero action button clicked — dispatch to StartUpdate or Reboot based
    /// on current state. The single inline button does double duty per the
    /// macOS Tahoe "Install" / "Restart" pattern on the Software Update card.
    HeroActionClicked,
    /// Parent App pushed updated Settings (Advanced dialog closed, CLI flag
    /// applied, etc.). StatusView refreshes any front-page widgets that
    /// mirror persistent settings — currently just the Auto Updates switch.
    SettingsChanged(Settings),
    /// "Restart Tonight" button clicked — schedules the host to reboot at
    /// 02:00 via `pkexec shutdown -r 02:00`. Only meaningful when
    /// reboot_pending is true (a deployment is staged); the button is hidden
    /// otherwise. Toast confirms; user can cancel manually with
    /// `sudo shutdown -c`.
    ScheduleRebootTonight,
    /// Copy log to clipboard.
    CopyLog,
    /// Navigate stack to a page name
    ShowPage(String),
    /// Save registry URI — fired by the EntryRow's `apply` signal (Enter
    /// or built-in ✓ button).
    SaveRegistryUri(String),
    /// Select tag in Image Source
    SelectTag(String),
    /// Toggle pinned status of history deployment
    TogglePin(String),
    /// Roll back to a specific deployment
    RollbackTo(MockDeployment),
    /// Confirm rollback
    ConfirmRollback,
    /// Set a deployment as default boot
    SetDefaultBoot(MockDeployment),
    /// Select a version in Changelog
    SelectChangelogVersion(String),
    /// Registry versions loaded in background
    RegistryVersionsLoaded(Vec<crate::registry_client::ImageVersion>),
    /// Available tags loaded from registry for the tag selector
    AvailableTagsLoaded(Vec<crate::registry_client::AvailableTag>),
    /// Github commits loaded in background: (sha, message, author, date)
    GithubCommitsLoaded(Vec<(String, String, String, String)>),
    /// SBOM package diff loaded in background
    SbomDiffLoaded(crate::sbom_diff::SbomDiffResult),
    /// SBOM fetch finished but no diff was computable (e.g. the registry
    /// didn't publish SPDX referrers for one of the images). Used to swap
    /// the in-flight spinner placeholder out for a "not available" hint.
    SbomDiffUnavailable,
    /// SBOM fetch has been kicked off — switch the changelog Stack section
    /// to a "Comparing packages…" placeholder until SbomDiffLoaded or
    /// SbomDiffUnavailable fires.
    SbomDiffStarted,
    /// Unpin the booted system back to a floating stream tag. Opens a
    /// confirmation dialog; on confirm, runs `bootc switch <registry>:<stream>`
    /// and toasts the result.
    UnpinToStream(String),
    /// A module has started running (from orchestrator).
    ModuleStarted(crate::orchestrator::Module),
    /// A module has finished (from orchestrator).
    ModuleFinished(
        crate::orchestrator::Module,
        crate::orchestrator::ModuleStatus,
    ),
}

/// Output messages the StatusView sends to its parent.
#[derive(Debug)]
pub enum StatusViewOutput {
    /// User wants to trigger an update.
    StartUpdate,
    /// User wants to cancel the running update.
    CancelUpdate,
    /// User wants to reboot the system.
    Reboot,
    /// User wants to open the rollback/rebase dialog.
    ShowRebase,
    /// User wants to open the update check dialog.
    OpenCheckDialog,
    /// Notify parent when page changes
    PageChanged(String),
    /// User clicked the "Advanced…" row on the main page. Parent opens the
    /// Advanced PreferencesDialog which hosts Image Source / Image History /
    /// Rebase / Powerwash / Factory Reset / settings.
    OpenAdvanced,
}

/// The status view model.
pub struct StatusView {
    state: AppState,
    log_view: Controller<LogView>,
    update_list: Controller<UpdateList>,
    /// Direct reference to the root stack for page switching in update().
    /// The five mutually-exclusive *state* pages of the root screen.
    stack: gtk::Stack,
    /// Real page navigation for the drill-down subpages. Owns back, swipe-back,
    /// Escape/Alt+Left and focus restoration, none of which the previous
    /// hand-rolled stack navigation provided.
    nav: adw::NavigationView,
    /// When the current update started (for elapsed timer).
    update_start: Option<Instant>,
    /// Label showing elapsed time during updates.
    elapsed_label: gtk::Label,
    /// Accumulated log text for clipboard copy.
    log_text: String,
    /// Toast overlay for copy confirmation.
    toast_overlay: adw::ToastOverlay,
    /// Root widget for the idle page.
    idle_page: adw::PreferencesPage,
    /// Hero row showing the current image summary.
    hero_row: adw::ActionRow,
    /// Status pill shown in the hero row suffix.
    status_pill: gtk::Label,
    /// Primary action button in the hero row — "Install" or "Restart"
    /// depending on state, hidden when neither applies. macOS Tahoe-inspired
    /// layout: put the CTA inline on the hero card.
    hero_action_btn: gtk::Button,
    /// "Restart Tonight" button on the hero row — only shown when
    /// reboot_pending. Schedules a 02:00 reboot via `pkexec shutdown -r`.
    hero_schedule_btn: gtk::Button,
    /// Banner group shown when action is needed.
    update_banner_group: adw::PreferencesGroup,
    /// Banner row with dynamic title/subtitle.
    banner_title_row: adw::ActionRow,
    /// Banner install button.
    banner_install_btn: gtk::Button,
    /// Banner whats new button.
    banner_whats_new_btn: gtk::Button,
    /// Banner restart button.
    banner_restart_btn: gtk::Button,
    /// Banner discard button.
    banner_discard_btn: gtk::Button,
    /// Automatic updates toggle in the settings card.
    auto_update_switch: adw::SwitchRow,
    /// Preflight check result.
    preflight_status: PreflightStatus,
    /// Cached last-update text.
    last_update_text: Option<String>,
    /// Cached image info text.
    image_info: Option<String>,
    /// Segmented progress bar shown while updating.
    seg_progress: SegmentedProgress,
    /// The module key that is currently active (drives segment coloring).
    active_module: Option<&'static str>,
    /// Whether an update has been staged and needs a reboot.
    reboot_pending: bool,

    // Redesigned settings & subpage state variables.
    registry_uri: String,
    selected_tag: String,
    deployments: Vec<MockDeployment>,
    expanded_deployment_id: Option<String>,
    changelog_version: String,
    registry_versions: Vec<crate::registry_client::ImageVersion>,
    github_commits: Vec<(String, String, String, String)>,
    sbom_diff: Option<crate::sbom_diff::SbomDiffResult>,
    sbom_status: SbomStatus,

    // Image Source subpage widget references for dynamic updates.
    registry_entry_row: adw::EntryRow,
    registry_row_sub: gtk::Label,
    tag_row: adw::ComboRow,
    tag_model: gtk::StringList,
    /// Parallel list of raw tag strings, indexed the same as `tag_model`'s
    /// display entries.
    tag_raws: Rc<RefCell<Vec<String>>>,
    /// Handler id for `tag_row`'s `selected` notification, so it can be blocked
    /// while the model is repopulated. See `AvailableTagsLoaded`.
    tag_row_handler: gtk::glib::SignalHandlerId,
    history_list_box: gtk::ListBox,
    images_count_label: gtk::Label,
    changelog_box: gtk::Box,
    changelog_install_bar: gtk::Box,
    /// "Pinned to {tag}" front-page group — visible only when the booted tag
    /// is a specific build (date or sha) rather than a floating stream.
    pin_group: adw::PreferencesGroup,
    pin_row: adw::ActionRow,

    // Dialog rollback state
    rollback_target: Option<MockDeployment>,
}

impl StatusView {
    fn hero_title(&self) -> String {
        self.image_info.clone().unwrap_or_else(|| {
            read_image_info()
                .or_else(|| detect_bootc_image_info().map(|(title, _, _)| title))
                .unwrap_or_else(|| "System Image".to_string())
        })
    }

    fn idle_subtitle(&self) -> String {
        // Per user direction: "Booted 3 days ago" wasn't insightful. Prefer
        // "VERSION · shaXXXXXXXX" from bootc-status (read_booted_image_summary)
        // so the user can see exactly which build is on disk. Falls back
        // through the cached last-update text, then a generic message.
        if self.reboot_pending {
            return "Reboot to update".to_string();
        }
        read_booted_image_summary()
            .or_else(|| self.last_update_text.clone())
            .unwrap_or_else(|| "Current image".to_string())
    }

    fn refresh_idle_description(&self) {
        // Pin-state UI mirrors the booted tag: shown when pinned, hidden
        // when the user is back on a floating stream.
        let pinned = is_pinned_tag(&self.selected_tag);
        self.pin_group.set_visible(pinned);
        if pinned {
            self.pin_row
                .set_title(&format!("Pinned to :{}", self.selected_tag));
        }

        self.hero_row.set_title(&self.hero_title());
        self.hero_row.set_subtitle(&self.idle_subtitle());

        for class in ["accent", "success", "warning", "dim-label"] {
            self.status_pill.remove_css_class(class);
        }

        let (pill_text, pill_class) = if self.reboot_pending {
            ("Staged", "warning")
        } else {
            match self.preflight_status {
                PreflightStatus::UpdateAvailable => ("Update ready", "accent"),
                PreflightStatus::UpToDate => ("Up to date", "success"),
                PreflightStatus::Checking => ("Checking", "dim-label"),
                PreflightStatus::Unknown => ("Ready", "dim-label"),
            }
        };
        self.status_pill.set_label(pill_text);
        self.status_pill.add_css_class(pill_class);

        // Hero action buttons (Install / Restart / Restart Tonight) are now
        // RESERVED for the reboot_pending state per user direction. When an
        // update is merely available (not yet installed), the action lives
        // on the banner row below. Hero stays minimal: just identity + (i).
        if self.reboot_pending {
            self.hero_action_btn.set_label("Restart");
            self.hero_action_btn.set_visible(true);
            self.hero_schedule_btn.set_visible(true);
            self.status_pill.set_visible(false);
        } else {
            self.hero_action_btn.set_visible(false);
            self.hero_schedule_btn.set_visible(false);
            self.status_pill.set_visible(!matches!(
                self.preflight_status,
                PreflightStatus::UpdateAvailable
            ));
        }

        if self.reboot_pending {
            self.update_banner_group.set_visible(true);
            self.banner_title_row.set_title("Reboot to finish updating");
            self.banner_title_row
                .set_subtitle("A new image is staged and will be used on next boot.");
            self.banner_install_btn.set_visible(false);
            self.banner_whats_new_btn.set_visible(true);
            self.banner_restart_btn.set_visible(false);
            self.banner_discard_btn.set_visible(true);
        } else if matches!(self.preflight_status, PreflightStatus::UpdateAvailable) {
            self.update_banner_group.set_visible(true);
            self.banner_title_row.set_title("Update available");
            self.banner_title_row
                .set_subtitle("A new system image is ready to install.");
            self.banner_install_btn.set_visible(true);
            self.banner_whats_new_btn.set_visible(true);
            self.banner_restart_btn.set_visible(false);
            self.banner_discard_btn.set_visible(false);
        } else {
            self.update_banner_group.set_visible(false);
        }
    }
}

#[relm4::component(pub)]
impl SimpleComponent for StatusView {
    type Init = AppState;
    type Input = StatusViewInput;
    type Output = StatusViewOutput;

    view! {
        #[root]
        adw::NavigationView {
            add = &adw::NavigationPage {
                set_title: "Updates",
                set_tag: Some("main"),

                #[wrap(Some)]
                set_child = &state_stack.clone() -> gtk::Stack {
                    set_transition_type: gtk::StackTransitionType::Crossfade,
                    set_transition_duration: 200,
                    set_hhomogeneous: false,
                    set_vhomogeneous: false,

                    // ─── Idle page ──────────────────────────────────────────────
                    add_child = &model.idle_page.clone() -> adw::PreferencesPage {} -> {
                        set_name: "idle",
                    },

                    // ─── Updating page ──────────────────────────────────────────
                    add_child = &model.toast_overlay.clone() -> adw::ToastOverlay {} -> {
                        set_name: "updating",
                    },

                    // ─── Complete page ──────────────────────────────────────────
                    add_child = &adw::StatusPage {
                        set_icon_name: Some("object-select-symbolic"),
                        set_title: "Update Complete",
                        set_description: Some("Restart to apply changes."),

                        #[wrap(Some)]
                        set_child = &gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            set_halign: gtk::Align::Center,
                            set_spacing: 8,

                            gtk::Button {
                                set_label: "Restart…",
                                add_css_class: "suggested-action",
                                add_css_class: "pill",
                                connect_clicked[sender] => move |_| {
                                    sender.output(StatusViewOutput::Reboot).unwrap();
                                },
                            },

                            gtk::Button {
                                set_label: "Restart Later",
                                add_css_class: "flat",
                                connect_clicked[sender] => move |_| {
                                    sender.input(StatusViewInput::StateChanged(AppState::Idle));
                                },
                            },
                        },
                    } -> {
                        set_name: "complete",
                    },

                    // ─── Up to date page ────────────────────────────────────────
                    add_child = &adw::StatusPage {
                        set_icon_name: Some("emblem-ok-symbolic"),
                        set_title: "Up to Date",
                        set_description: Some("No updates available."),

                        #[wrap(Some)]
                        set_child = &gtk::Button {
                            set_label: "Done",
                            add_css_class: "pill",
                            set_halign: gtk::Align::Center,
                            connect_clicked[sender] => move |_| {
                                sender.input(StatusViewInput::StateChanged(AppState::Idle));
                            },
                        },
                    } -> {
                        set_name: "up_to_date",
                    },

                    // ─── Error page ─────────────────────────────────────────────
                    add_child = &adw::StatusPage {
                        set_icon_name: Some("dialog-warning-symbolic"),
                        set_title: "Update Failed",
                        set_description: Some("Something went wrong."),

                        #[wrap(Some)]
                        set_child = &gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            set_halign: gtk::Align::Center,
                            set_spacing: 8,

                            gtk::Button {
                                set_label: "Retry",
                                add_css_class: "suggested-action",
                                add_css_class: "pill",
                                connect_clicked[sender] => move |_| {
                                    sender.output(StatusViewOutput::StartUpdate).unwrap();
                                },
                            },

                            gtk::Button {
                                set_label: "Dismiss",
                                add_css_class: "flat",
                                connect_clicked[sender] => move |_| {
                                    sender.input(StatusViewInput::StateChanged(AppState::Idle));
                                },
                            },
                        },
                    } -> {
                        set_name: "error",
                    },
                },
            },
        }
    }

    fn init(
        init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let state_stack = gtk::Stack::new();

        let log_view = LogView::builder().launch(()).detach();
        let update_list = UpdateList::builder().launch(()).detach();

        let toast_overlay = adw::ToastOverlay::new();

        // ── Idle page (built imperatively) ──────────────────────────────
        let initial_image_info = read_image_info();
        let initial_registry_uri = read_registry_uri().unwrap_or_else(String::new);
        let initial_selected_tag = read_selected_tag();
        let initial_last_update = get_last_update_time();
        let auto_updates_enabled = read_auto_updates_enabled();
        let initial_subtitle = read_booted_image_summary()
            .or_else(|| initial_last_update.clone())
            .unwrap_or_else(|| "Current image".to_string());

        let idle_page = adw::PreferencesPage::new();

        let hero_group = adw::PreferencesGroup::new();
        let hero_row = adw::ActionRow::builder()
            .title(initial_image_info.as_deref().unwrap_or("System Image"))
            .subtitle(&initial_subtitle)
            .subtitle_lines(2)
            .subtitle_selectable(true)
            .title_selectable(true)
            .build();
        hero_row.set_activatable(false);

        let hero_icon = match read_logo_icon_name() {
            HeroLogo::Themed(name) => {
                let img = gtk::Image::from_icon_name(&name);
                img.add_css_class("accent");
                img
            }
            HeroLogo::File(path) => gtk::Image::from_file(&path),
        };
        hero_icon.set_pixel_size(32);
        hero_row.add_prefix(&hero_icon);

        let hero_change_btn = gtk::Button::with_label("Change");
        hero_change_btn.add_css_class("flat");
        hero_change_btn.set_tooltip_text(Some("Change image variant or stream"));
        hero_change_btn.set_valign(gtk::Align::Center);
        let change_sender = sender.output_sender().clone();
        hero_change_btn.connect_clicked(move |_| {
            change_sender.emit(StatusViewOutput::ShowRebase);
        });
        hero_row.add_suffix(&hero_change_btn);

        let status_pill = gtk::Label::new(Some("Checking"));
        status_pill.add_css_class("caption");
        status_pill.add_css_class("dim-label");
        status_pill.set_valign(gtk::Align::Center);
        hero_row.add_suffix(&status_pill);

        let hero_schedule_btn = gtk::Button::with_label("Restart Tonight");
        hero_schedule_btn.set_valign(gtk::Align::Center);
        hero_schedule_btn.set_visible(false);
        let schedule_sender = sender.input_sender().clone();
        hero_schedule_btn.connect_clicked(move |_| {
            schedule_sender.emit(StatusViewInput::ScheduleRebootTonight);
        });
        hero_row.add_suffix(&hero_schedule_btn);

        let hero_action_btn = gtk::Button::with_label("Install");
        hero_action_btn.add_css_class("suggested-action");
        hero_action_btn.set_valign(gtk::Align::Center);
        hero_action_btn.set_visible(false);
        let hero_action_sender = sender.input_sender().clone();
        hero_action_btn.connect_clicked(move |_| {
            hero_action_sender.emit(StatusViewInput::HeroActionClicked);
        });
        hero_row.add_suffix(&hero_action_btn);

        hero_group.add(&hero_row);
        idle_page.add(&hero_group);

        // ── Pin group ─────────────────────────────────────────────────────
        let pin_group = adw::PreferencesGroup::new();
        let pin_row = adw::ActionRow::builder()
            .title("Pinned to a specific build")
            .subtitle("Automatic updates are paused. Unpin to resume.")
            .build();
        pin_row.set_activatable(false);
        let pin_icon = gtk::Image::from_icon_name("emblem-important-symbolic");
        pin_icon.set_pixel_size(20);
        pin_icon.add_css_class("warning");
        pin_row.add_prefix(&pin_icon);

        let unpin_btn = gtk::Button::with_label("Unpin");
        unpin_btn.add_css_class("suggested-action");
        unpin_btn.set_valign(gtk::Align::Center);
        let unpin_sender = sender.input_sender().clone();
        unpin_btn.connect_clicked(move |_| {
            unpin_sender.emit(StatusViewInput::UnpinToStream("latest".to_string()));
        });
        pin_row.add_suffix(&unpin_btn);
        pin_group.add(&pin_row);
        pin_group.set_visible(is_pinned_tag(&initial_selected_tag));
        idle_page.add(&pin_group);

        // Banner group
        let update_banner_group = adw::PreferencesGroup::new();
        let banner_title_row = adw::ActionRow::builder()
            .title("Update available")
            .subtitle("A new system image is ready to install.")
            .build();
        banner_title_row.set_activatable(false);

        let banner_icon = gtk::Image::from_icon_name("software-update-available-symbolic");
        banner_icon.set_pixel_size(24);
        banner_icon.add_css_class("accent");
        banner_title_row.add_prefix(&banner_icon);

        let banner_whats_new_btn = gtk::Button::from_icon_name("view-list-symbolic");
        banner_whats_new_btn.add_css_class("flat");
        banner_whats_new_btn.add_css_class("circular");
        banner_whats_new_btn.set_tooltip_text(Some("What's new in this update"));
        banner_whats_new_btn.set_valign(gtk::Align::Center);
        let initial_selected_tag_3 = initial_selected_tag.clone();
        let whats_new_sender_2 = sender.input_sender().clone();
        banner_whats_new_btn.connect_clicked(move |_| {
            let ver = initial_selected_tag_3.clone();
            whats_new_sender_2.emit(StatusViewInput::SelectChangelogVersion(ver));
        });

        let banner_install_btn = gtk::Button::with_label("Install");
        banner_install_btn.add_css_class("accent");
        banner_install_btn.set_valign(gtk::Align::Center);
        let install_sender_2 = sender.output_sender().clone();
        banner_install_btn.connect_clicked(move |_| {
            let _ = install_sender_2.send(StatusViewOutput::StartUpdate);
        });

        let banner_restart_btn = gtk::Button::with_label("Restart");
        banner_restart_btn.add_css_class("accent");
        banner_restart_btn.set_valign(gtk::Align::Center);
        let restart_sender = sender.output_sender().clone();
        banner_restart_btn.connect_clicked(move |_| {
            let _ = restart_sender.send(StatusViewOutput::Reboot);
        });

        let banner_discard_btn = gtk::Button::with_label("Discard");
        banner_discard_btn.add_css_class("flat");
        let discard_sender = sender.input_sender().clone();
        banner_discard_btn.connect_clicked(move |_| {
            discard_sender.emit(StatusViewInput::DismissBanner);
        });

        banner_title_row.add_suffix(&banner_whats_new_btn);
        banner_title_row.add_suffix(&banner_install_btn);
        banner_title_row.add_suffix(&banner_restart_btn);
        banner_title_row.add_suffix(&banner_discard_btn);
        update_banner_group.add(&banner_title_row);
        update_banner_group.set_visible(false);
        idle_page.add(&update_banner_group);

        let idle_settings = idle::build_settings(&sender, auto_updates_enabled);
        let auto_update_switch = idle_settings.auto_update_switch;
        let images_count_label = gtk::Label::new(Some("3 versions"));
        images_count_label.add_css_class("dim-label");

        idle_page.add(&idle_settings.group);
        idle_page.add(&idle_settings.advanced_group);

        // ── Image Source Subpage ─────────────────────────────────────────
        let source_widgets = build_source_page(&sender, &initial_registry_uri, &initial_selected_tag);

        // ── Version History Subpage ──────────────────────────────────────
        let history_page = adw::PreferencesPage::new();
        let history_group = adw::PreferencesGroup::builder()
            .description(
                "Past images stay on disk so you can roll back. Pin a version to keep it across upgrades.",
            )
            .build();
        let history_list_box = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .build();
        history_list_box.add_css_class("boxed-list");
        history_group.add(&history_list_box);
        history_page.add(&history_group);
        let history_nav_page = adw::NavigationPage::builder()
            .title("Image History")
            .tag("history")
            .child(&history_page)
            .build();

        // ── Changelogs Subpage ───────────────────────────────────────────
        let changelog_page = gtk::ScrolledWindow::new();
        changelog_page.set_hscrollbar_policy(gtk::PolicyType::Never);
        changelog_page.set_vexpand(true);
        let changelog_clamp = adw::Clamp::new();
        changelog_clamp.set_maximum_size(600);
        let changelog_content = gtk::Box::new(gtk::Orientation::Vertical, 16);
        changelog_content.set_margin_start(24);
        changelog_content.set_margin_end(24);
        changelog_content.set_margin_top(24);
        changelog_content.set_margin_bottom(24);
        changelog_clamp.set_child(Some(&changelog_content));
        changelog_page.set_child(Some(&changelog_clamp));

        let changelog_box = gtk::Box::new(gtk::Orientation::Vertical, 16);
        changelog_content.append(&changelog_box);

        let changelog_install_bar = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        changelog_install_bar.set_margin_top(12);
        changelog_install_bar.set_margin_bottom(12);
        let ch_install_label = gtk::Label::new(Some("A newer version is available."));
        ch_install_label.add_css_class("caption");
        ch_install_label.add_css_class("dim-label");
        let ch_install_btn = gtk::Button::with_label("Install");
        ch_install_btn.add_css_class("suggested-action");
        let ch_inst_sender = sender.output_sender().clone();
        ch_install_btn.connect_clicked(move |_| {
            let _ = ch_inst_sender.send(StatusViewOutput::StartUpdate);
        });
        changelog_install_bar.append(&ch_install_label);
        changelog_install_bar.append(&ch_install_btn);
        changelog_install_bar.set_visible(false);
        changelog_content.append(&changelog_install_bar);

        let changelog_nav_page = adw::NavigationPage::builder()
            .title("What's New")
            .tag("changelog")
            .child(&changelog_page)
            .build();

        // ── Updating page ────────────────────────────────────────────────
        let updating_widgets = build_updating_page(&sender, &log_view, &update_list);
        toast_overlay.set_child(Some(&updating_widgets.updating_content));

        spawn_changelog_fetch(
            initial_registry_uri.clone(),
            initial_selected_tag.clone(),
            sender.clone(),
        );

        let initial_selected_tag_3 = initial_selected_tag.clone();
        let model = StatusView {
            state: init,
            log_view,
            update_list,
            stack: state_stack.clone(),
            nav: root.clone(),
            update_start: None,
            elapsed_label: updating_widgets.elapsed_label,
            log_text: String::new(),
            toast_overlay,
            idle_page,
            hero_row,
            status_pill,
            hero_action_btn,
            hero_schedule_btn,
            update_banner_group,
            banner_title_row,
            banner_install_btn,
            banner_whats_new_btn,
            banner_restart_btn,
            banner_discard_btn,
            auto_update_switch,
            preflight_status: PreflightStatus::Checking,
            last_update_text: initial_last_update,
            image_info: initial_image_info,
            seg_progress: updating_widgets.seg_progress,
            active_module: None,
            reboot_pending: false,

            registry_uri: initial_registry_uri.clone(),
            selected_tag: initial_selected_tag.clone(),
            deployments: get_sample_deployments(false),
            expanded_deployment_id: None,
            changelog_version: initial_selected_tag_3.clone(),
            registry_versions: Vec::new(),
            github_commits: Vec::new(),
            sbom_diff: None,
            sbom_status: SbomStatus::Pending,

            registry_entry_row: source_widgets.registry_entry_row,
            registry_row_sub: source_widgets.registry_row_sub,
            tag_row: source_widgets.tag_row,
            tag_model: source_widgets.tag_model,
            tag_raws: source_widgets.tag_raws,
            tag_row_handler: source_widgets.tag_row_handler,
            history_list_box: history_list_box.clone(),
            images_count_label,
            changelog_box: changelog_box.clone(),
            changelog_install_bar: changelog_install_bar.clone(),
            pin_group: pin_group.clone(),
            pin_row: pin_row.clone(),
            rollback_target: None,
        };

        let widgets = view_output!();

        root.add(&source_widgets.source_nav_page);
        root.add(&history_nav_page);
        root.add(&changelog_nav_page);

        model.refresh_idle_description();
        model.stack.set_visible_child_name("idle");

        rebuild_history_list(
            &model.history_list_box,
            &model.deployments,
            model.expanded_deployment_id.as_deref(),
            &sender,
        );
        model
            .images_count_label
            .set_label(&format!("{} images", model.deployments.len()));
        model.rebuild_changelog_page(&sender);

        let stack_ref = model.stack.clone();
        let timer_sender = sender.input_sender().clone();
        gtk::glib::timeout_add_local(std::time::Duration::from_millis(250), move || {
            if stack_ref.visible_child_name().as_deref() == Some("updating") {
                timer_sender.emit(StatusViewInput::TimerTick);
            }
            gtk::glib::ControlFlow::Continue
        });

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        match msg {
            StatusViewInput::StateChanged(new_state) => {
                let stack_name = match &new_state {
                    AppState::Idle => "idle",
                    AppState::Updating => "updating",
                    AppState::Complete => "complete",
                    AppState::UpToDate => "up_to_date",
                    AppState::Error(_) => "error",
                };
                self.stack.set_visible_child_name(stack_name);

                match &new_state {
                    AppState::Updating => {
                        self.update_start = Some(Instant::now());
                        self.elapsed_label.set_label("0:00");
                        self.update_list.emit(UpdateListInput::Reset);
                        self.seg_progress.reset();
                        self.active_module = None;
                        self.reboot_pending = false;
                    }
                    AppState::Complete => {
                        self.update_start = None;
                        self.update_list.emit(UpdateListInput::MarkAllComplete);
                        self.seg_progress.mark_all_complete();
                        self.active_module = None;
                        self.preflight_status = PreflightStatus::UpToDate;
                        self.reboot_pending = true;
                        self.refresh_idle_description();
                        self.deployments = get_sample_deployments(true);
                        rebuild_history_list(
                            &self.history_list_box,
                            &self.deployments,
                            self.expanded_deployment_id.as_deref(),
                            &sender,
                        );
                        self.images_count_label
                            .set_label(&format!("{} images", self.deployments.len()));
                    }
                    AppState::Error(_) => {
                        self.update_start = None;
                        self.update_list.emit(UpdateListInput::MarkCurrentFailed);
                        if let Some(key) = self.active_module {
                            self.seg_progress.set_module_failed(key);
                        }
                        self.active_module = None;
                    }
                    AppState::UpToDate => {
                        self.update_start = None;
                        self.preflight_status = PreflightStatus::UpToDate;
                        self.reboot_pending = false;
                        self.refresh_idle_description();
                    }
                    AppState::Idle => {
                        self.update_start = None;
                        self.last_update_text = get_last_update_time();
                        self.image_info = read_image_info();
                        self.refresh_idle_description();
                    }
                }

                if let AppState::Error(ref err) = new_state {
                    if let Some(page) = self.stack.child_by_name("error") {
                        if let Ok(status_page) = page.downcast::<adw::StatusPage>() {
                            status_page.set_description(Some(err.as_str()));
                        }
                    }
                }

                self.state = new_state;
            }

            StatusViewInput::AppendLog(line) => {
                if !self.log_text.is_empty() {
                    self.log_text.push('\n');
                }
                self.log_text.push_str(&line);
                self.update_list
                    .emit(UpdateListInput::ProcessLine(line.clone()));
                self.log_view.emit(LogViewInput::AppendLine(line.clone()));
            }

            StatusViewInput::ClearLog => {
                self.log_text.clear();
                self.log_view.emit(LogViewInput::Clear);
            }

            StatusViewInput::TimerTick => {
                if let Some(start) = self.update_start {
                    let elapsed = start.elapsed();
                    let secs = elapsed.as_secs();
                    let mins = secs / 60;
                    let remaining_secs = secs % 60;
                    self.elapsed_label
                        .set_label(&format!("{}:{:02}", mins, remaining_secs));
                }
            }

            StatusViewInput::PreflightResult(status) => {
                self.preflight_status = status;
                self.refresh_idle_description();
            }

            StatusViewInput::DismissBanner => {
                self.reboot_pending = false;
                self.preflight_status = PreflightStatus::UpToDate;
                self.refresh_idle_description();
            }

            StatusViewInput::SettingsChanged(new_settings) => {
                if self.auto_update_switch.is_active() != new_settings.auto_updates {
                    self.auto_update_switch
                        .set_active(new_settings.auto_updates);
                }
            }

            StatusViewInput::HeroActionClicked => {
                if self.reboot_pending {
                    let _ = sender.output(StatusViewOutput::Reboot);
                } else {
                    let _ = sender.output(StatusViewOutput::StartUpdate);
                }
            }

            StatusViewInput::ScheduleRebootTonight => {
                super::host_actions::schedule_reboot_tonight(&self.toast_overlay);
            }

            StatusViewInput::CopyLog => {
                if let Some(display) = gtk::gdk::Display::default() {
                    let clipboard = display.clipboard();
                    clipboard.set_text(&self.log_text);
                    let toast = adw::Toast::new("Log copied to clipboard");
                    toast.set_timeout(3);
                    self.toast_overlay.add_toast(toast);
                }
            }

            StatusViewInput::ShowPage(page) => {
                if page == "main" || page == "idle" {
                    self.nav.pop_to_tag("main");
                    self.stack.set_visible_child_name("idle");
                } else {
                    self.nav.push_by_tag(&page);
                }
                let _ = sender.output(StatusViewOutput::PageChanged(page));
            }

            StatusViewInput::SaveRegistryUri(uri) => {
                if !uri.trim().is_empty() {
                    self.registry_uri = uri;
                    self.registry_entry_row.set_text(&self.registry_uri);

                    let name = self
                        .registry_uri
                        .split('/')
                        .next_back()
                        .unwrap_or(&self.registry_uri);
                    self.registry_row_sub
                        .set_label(&format!("{}:{}", name, self.selected_tag));

                    let toast = adw::Toast::new("Image source updated");
                    self.toast_overlay.add_toast(toast);

                    spawn_changelog_fetch(
                        self.registry_uri.clone(),
                        self.selected_tag.clone(),
                        sender.clone(),
                    );
                }
            }

            StatusViewInput::SelectTag(tag) => {
                if tag == self.selected_tag {
                    return;
                }
                self.selected_tag = tag.clone();
                let desc = match tag.as_str() {
                    "latest" => "Always the newest stable build",
                    _ if tag.chars().all(|c| c.is_ascii_digit()) => "Pinned to this version",
                    _ => "Custom tag",
                };
                self.tag_row.set_subtitle(desc);

                let name = self
                    .registry_uri
                    .split('/')
                    .last()
                    .unwrap_or(&self.registry_uri);
                self.registry_row_sub
                    .set_label(&format!("{}:{}", name, self.selected_tag));

                let toast = adw::Toast::new(&format!("Tag set to :{}", tag));
                self.toast_overlay.add_toast(toast);

                spawn_changelog_fetch(
                    self.registry_uri.clone(),
                    self.selected_tag.clone(),
                    sender.clone(),
                );
            }

            StatusViewInput::TogglePin(action) => {
                if let Some(id) = action.strip_prefix("expand:") {
                    if self.expanded_deployment_id.as_deref() == Some(id) {
                        self.expanded_deployment_id = None;
                    } else {
                        self.expanded_deployment_id = Some(id.to_string());
                    }
                    rebuild_history_list(
                        &self.history_list_box,
                        &self.deployments,
                        self.expanded_deployment_id.as_deref(),
                        &sender,
                    );
                    self.images_count_label
                        .set_label(&format!("{} images", self.deployments.len()));
                } else if action == "powerwash" {
                    self.show_powerwash_dialog();
                } else if action == "factory" {
                    self.show_factory_reset_dialog();
                } else {
                    for d in &mut self.deployments {
                        if d.id == action {
                            d.pinned = !d.pinned;
                            let toast_msg = if d.pinned {
                                format!("Pinned {} (preventing pruning)", d.tag)
                            } else {
                                format!("Unpinned {} (allowing pruning)", d.tag)
                            };
                            let toast = adw::Toast::new(&toast_msg);
                            self.toast_overlay.add_toast(toast);
                            break;
                        }
                    }
                    rebuild_history_list(
                        &self.history_list_box,
                        &self.deployments,
                        self.expanded_deployment_id.as_deref(),
                        &sender,
                    );
                    self.images_count_label
                        .set_label(&format!("{} images", self.deployments.len()));
                }
            }

            StatusViewInput::RollbackTo(d) => {
                self.show_rollback_dialog(d, &sender);
            }

            StatusViewInput::ConfirmRollback => {
                if let Some(target) = self.rollback_target.take() {
                    let toast = adw::Toast::new(&format!("Rolling back to {}…", target.tag));
                    self.toast_overlay.add_toast(toast);
                }
            }

            StatusViewInput::SetDefaultBoot(d) => {
                let toast = adw::Toast::new(&format!("Set {} as default boot", d.tag));
                self.toast_overlay.add_toast(toast);
            }

            StatusViewInput::SelectChangelogVersion(version) => {
                let target = if version.is_empty() {
                    self.changelog_version.clone()
                } else {
                    version
                };
                if target != self.changelog_version {
                    let t0 = std::time::Instant::now();
                    self.changelog_version = target;
                    self.rebuild_changelog_page(&sender);
                    tracing::debug!("changelog: rebuild took {}ms", t0.elapsed().as_millis());
                }
                self.nav.push_by_tag("changelog");
                let _ = sender.output(StatusViewOutput::PageChanged("changelog".to_string()));
            }

            StatusViewInput::RegistryVersionsLoaded(versions) => {
                let existing: std::collections::HashSet<String> = self
                    .registry_versions
                    .iter()
                    .map(|v| v.version.clone())
                    .collect();
                for v in versions {
                    if !existing.contains(&v.version) {
                        self.registry_versions.push(v);
                    }
                }
                self.registry_versions.sort_by_key(|v| v.date);
                if let Some(latest) = self.registry_versions.last() {
                    self.changelog_version = latest.version.clone();
                }
                self.rebuild_changelog_page(&sender);

                const HISTORY_MAX: usize = 8;
                let mut seen_tags: std::collections::HashSet<String> =
                    self.deployments.iter().map(|d| d.tag.clone()).collect();
                let mut merged = self.deployments.clone();
                for v in self.registry_versions.iter().rev() {
                    if merged.len() >= HISTORY_MAX {
                        break;
                    }
                    if seen_tags.insert(v.version.clone()) {
                        let date_str = v.date.format("%b %-d, %Y").to_string();
                        merged.push(MockDeployment {
                            id: format!("remote-{}", v.version),
                            state: "remote".to_string(),
                            title: self
                                .image_info
                                .clone()
                                .unwrap_or_else(|| "System Image".to_string()),
                            image: self.registry_uri.clone(),
                            tag: v.version.clone(),
                            digest: v.revision.clone(),
                            deployed: format!("Available · {}", date_str),
                            deployed_full: format!(
                                "Built: {} · {}",
                                date_str,
                                v.created.format("%H:%M UTC")
                            ),
                            size: "—".to_string(),
                            kernel: v.kernel.clone(),
                            package_count: 0,
                            signer: "Remote registry".to_string(),
                            pinned: false,
                        });
                    }
                }
                self.deployments = merged;
                rebuild_history_list(
                    &self.history_list_box,
                    &self.deployments,
                    self.expanded_deployment_id.as_deref(),
                    &sender,
                );
                self.images_count_label
                    .set_label(&format!("{} images", self.deployments.len()));
            }

            StatusViewInput::AvailableTagsLoaded(tags) => {
                self.tag_row.block_signal(&self.tag_row_handler);

                let displays: Vec<&str> = tags.iter().map(|t| t.display.as_str()).collect();
                self.tag_model
                    .splice(0, self.tag_model.n_items(), &displays);

                let raws: Vec<String> = tags.iter().map(|t| t.raw.clone()).collect();
                let active_idx = raws
                    .iter()
                    .position(|raw| raw == &self.selected_tag)
                    .unwrap_or(0) as u32;
                *self.tag_raws.borrow_mut() = raws;
                self.tag_row.set_selected(active_idx);
                self.tag_row.set_sensitive(tags.len() > 1);

                self.tag_row.unblock_signal(&self.tag_row_handler);
            }

            StatusViewInput::GithubCommitsLoaded(commits) => {
                self.github_commits = commits;
                self.rebuild_changelog_page(&sender);
            }

            StatusViewInput::SbomDiffLoaded(diff) => {
                self.sbom_diff = Some(diff);
                self.sbom_status = SbomStatus::Loaded;
                self.rebuild_changelog_page(&sender);
            }

            StatusViewInput::SbomDiffStarted => {
                self.sbom_status = SbomStatus::Loading;
                self.rebuild_changelog_page(&sender);
            }

            StatusViewInput::SbomDiffUnavailable => {
                self.sbom_status = SbomStatus::NotAvailable;
                self.rebuild_changelog_page(&sender);
            }

            StatusViewInput::UnpinToStream(stream_tag) => {
                self.show_unpin_dialog(&stream_tag);
            }

            StatusViewInput::ModuleStarted(module) => {
                let key = module.key();
                let is_same_seg = self
                    .active_module
                    .map(|prev| same_segment(prev, key))
                    .unwrap_or(false);
                if !is_same_seg {
                    if let Some(prev) = self.active_module {
                        self.seg_progress.set_module_complete(prev);
                    }
                    self.seg_progress.set_module_active(key);
                }
                self.active_module = Some(key);
                self.update_list.emit(UpdateListInput::ProcessLine(format!(
                    "Starting module: {}",
                    key
                )));
            }

            StatusViewInput::ModuleFinished(module, status) => {
                use crate::orchestrator::ModuleStatus;
                let key = module.key();
                match status {
                    ModuleStatus::Success | ModuleStatus::UpToDate | ModuleStatus::Skipped => {
                        self.seg_progress.set_module_complete(key);
                    }
                    ModuleStatus::Failed(_) => {
                        self.seg_progress.set_module_failed(key);
                    }
                }
            }
        }
    }
}
