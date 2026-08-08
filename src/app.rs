//! Top-level application component and inner updates panel.
//!
//! ## relm4 Design Rationale
//!
//! This module demonstrates the **canonical relm4 component pattern** for Bluefin apps:
//!
//! 1. **Single top-level component** owns the `AdwApplicationWindow` and the state machine.
//!    It is the sole orchestrator — child components communicate UP via `Output` messages
//!    and receive commands DOWN via `emit()` on their controller handle.
//!
//! 2. **Message-driven state** — all state transitions happen through `UpdatesPanelMsg` variants
//!    processed in a single `update()` method. This makes state transitions explicit,
//!    traceable (via `tracing`), and impossible to miss. No widget callbacks mutate
//!    state directly.
//!
//! 3. **Forward pattern** — child component outputs are mapped to parent inputs via
//!    `.forward(sender, |output| match output { ... })`. This decouples children from
//!    the parent's message type.
//!
//! 4. **Action groups** — menu items and keyboard shortcuts use relm4's action system
//!    (`new_action_group!`, `new_stateless_action!`) rather than raw GAction. This keeps
//!    type safety and connects naturally to the message bus.
//!
//! 5. **Separate async thread** — long-running work (subprocess) runs on a tokio runtime
//!    in `std::thread::spawn`. Results flow back via `sender.emit()` which is thread-safe
//!    and queues messages on the GLib main loop.
//!
//! ## State machine
//!
//!   Idle → Updating → (Complete | Error) → Idle
//!
//! ## Component hierarchy
//!
//!   App (window shell)
//!   └── UpdatesPanel (core updates logic)
//!       └── StatusView (content area, owns LogView)
//!           └── LogView (scrollable text output)

use adw::prelude::*;
use relm4::actions::{AccelsPlus, RelmAction, RelmActionGroup};
use relm4::prelude::*;

use crate::config;
use crate::dbus_progress::ProgressDBus;
use crate::settings::Settings;
use crate::ui::preferences::show_preferences;
use crate::ui::rebase_dialog::show_rebase_dialog;
use crate::ui::status_view::{StatusView, StatusViewInput, StatusViewOutput};
use crate::ui::update_check_dialog::{CheckResult, show_update_check_dialog};
use crate::update_worker::{SimulationScenario, UpdateEvent, UpdateWorker, run_simulated};

/// Application-level state.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum AppState {
    /// No update in progress; ready to start one.
    #[default]
    Idle,
    /// Update is actively running.
    Updating,
    /// Update completed successfully.
    Complete,
    /// uupd exited with code 77 — system is already current, nothing to do.
    UpToDate,
    /// Update failed with an error message.
    Error(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum PreflightStatus {
    Checking,
    UpdateAvailable,
    UpToDate,
    Unknown,
}

// ─── UpdatesPanel Component (Core Widget) ──────────────────────────────────

/// Core updates panel model.
pub struct UpdatesPanel {
    pub state: AppState,
    pub preflight_status: PreflightStatus,
    pub sim_scenario: SimulationScenario,
    pub log_lines: Vec<String>,
    pub toast_overlay: adw::ToastOverlay,
    pub status_view: Controller<StatusView>,
    pub cancel_tx: Option<tokio::sync::oneshot::Sender<()>>,
    pub current_page: String,
    pub dev_banner: adw::Banner,
    pub settings: Settings,
    pub progress_dbus: ProgressDBus,

    // Embedded navigation support
    pub is_embedded: bool,
    pub embedded_header: adw::HeaderBar,
    pub embedded_title: adw::WindowTitle,
}

/// Messages the UpdatesPanel component can receive.
#[derive(Debug)]
pub enum UpdatesPanelMsg {
    /// User clicked "Update" — optionally bypass the metered-network confirmation.
    StartUpdate { skip_metered_check: bool },
    /// User clicked "Check" on the main view — open the check dialog.
    OpenCheckDialog,
    /// The check dialog completed with results.
    CheckComplete(CheckResult),
    /// User clicked "Install all" in the check dialog.
    InstallFromCheck,
    /// A line of output arrived from the subprocess.
    OutputLine(String),
    /// A module has started running.
    ModuleStarted(crate::orchestrator::Module),
    /// A module has finished.
    ModuleFinished(
        crate::orchestrator::Module,
        crate::orchestrator::ModuleStatus,
    ),
    /// The subprocess exited successfully.
    UpdateComplete,
    /// The subprocess reported that the system is already up to date (exit 77).
    UpdateUpToDate,
    /// The subprocess failed.
    UpdateFailed(String),
    /// User wants to cancel the running update.
    CancelUpdate,
    /// User wants to reboot the system.
    RequestReboot,
    /// User confirmed reboot in the dialog.
    ConfirmReboot,
    /// User requested the Rebase History dialog.
    ShowRebaseDialog,
    /// User requested the About dialog.
    ShowAbout,
    /// User requested the Preferences dialog.
    ShowPreferences,
    /// Settings were updated in the preferences dialog.
    SettingsChanged(Settings),
    /// Result of the startup preflight update check.
    PreflightResult(PreflightStatus),
    /// Developer mode toggle from the hamburger menu.
    ToggleDevMode(bool),
    /// Update the selected developer-mode simulation scenario.
    SetSimScenario(SimulationScenario),
    /// Quit the application.
    Quit,
    /// Navigate between pages
    PageChanged(String),
    /// Go back to main page
    GoBack,
    /// Show "What's new" / changelog for the latest available version.
    ShowWhatsNew,
    /// Show "What's new" / changelog filtered to a specific image tag.
    ShowChangelogForTag(String),
    /// Dismiss the staged-reboot banner.
    DismissBanner,
    /// Open the powerwash confirmation dialog.
    TriggerPowerwash,
    /// Open the factory-reset confirmation dialog.
    TriggerFactoryReset,
    /// Navigate the StatusView stack to a specific subpage.
    ShowStatusPage(String),
}

/// Outputs emitted by UpdatesPanel to its parent.
#[derive(Debug, Clone)]
pub enum UpdatesPanelOutput {
    PageChanged(String),
}

#[relm4::component(pub)]
impl SimpleComponent for UpdatesPanel {
    type Init = bool; // is_embedded
    type Input = UpdatesPanelMsg;
    type Output = UpdatesPanelOutput;

    view! {
        #[root]
        gtk::Box {
            set_orientation: gtk::Orientation::Vertical,

            append = &model.embedded_header.clone() -> adw::HeaderBar {
                #[watch]
                set_visible: model.is_embedded && model.current_page != "main",
            },

            // Two distinct safety states deserve two distinct messages. Saying
            // "updates are simulated" during a dry run would be a lie: the real
            // orchestrator, registry and rebase code all run — only the
            // privileged commands at the end are withheld.
            append = &model.dev_banner.clone() -> adw::Banner {
                #[watch]
                set_title: if model.settings.dev_mode {
                    "Developer Mode — updates are simulated"
                } else {
                    "Dry run — actions are recorded, your system is not modified"
                },
                #[watch]
                set_revealed: model.settings.dev_mode || model.settings.dry_run,
            },

            append = &model.toast_overlay.clone() -> adw::ToastOverlay {
                set_child: Some(model.status_view.widget()),
                set_vexpand: true,
            },
        }
    }

    fn init(
        is_embedded: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        // Build child component: StatusView receives state updates and emits user actions.
        let status_view =
            StatusView::builder()
                .launch(AppState::Idle)
                .forward(sender.input_sender(), |output| match output {
                    StatusViewOutput::StartUpdate => UpdatesPanelMsg::StartUpdate {
                        skip_metered_check: false,
                    },
                    StatusViewOutput::CancelUpdate => UpdatesPanelMsg::CancelUpdate,
                    StatusViewOutput::Reboot => UpdatesPanelMsg::RequestReboot,
                    StatusViewOutput::ShowRebase => UpdatesPanelMsg::ShowRebaseDialog,
                    StatusViewOutput::OpenCheckDialog => UpdatesPanelMsg::OpenCheckDialog,
                    StatusViewOutput::PageChanged(page) => UpdatesPanelMsg::PageChanged(page),
                    StatusViewOutput::OpenAdvanced => UpdatesPanelMsg::ShowPreferences,
                });

        let toast_overlay = adw::ToastOverlay::new();
        let dev_banner = adw::Banner::new("Developer Mode — updates are simulated");

        let settings = Settings::load();

        inject_app_css();

        let initial_sim_scenario = match settings.sim_scenario.as_deref() {
            Some("Failure") | Some("failure") => SimulationScenario::Failure,
            Some("AlreadyUpToDate") | Some("up-to-date") | Some("uptodate") => {
                SimulationScenario::AlreadyUpToDate
            }
            _ => SimulationScenario::Success,
        };

        // Embedded navigation setup
        let embedded_header = adw::HeaderBar::new();
        let embedded_title = adw::WindowTitle::new("Finupdate", "");
        embedded_header.set_title_widget(Some(&embedded_title));

        let back_btn = gtk::Button::builder()
            .icon_name("go-previous-symbolic")
            .build();
        let back_sender = sender.input_sender().clone();
        back_btn.connect_clicked(move |_| {
            back_sender.emit(UpdatesPanelMsg::GoBack);
        });
        embedded_header.pack_start(&back_btn);

        let model = UpdatesPanel {
            state: AppState::Idle,
            preflight_status: PreflightStatus::Checking,
            sim_scenario: initial_sim_scenario,
            log_lines: Vec::new(),
            toast_overlay,
            status_view,
            cancel_tx: None,
            current_page: "main".to_string(),
            dev_banner,
            settings,
            progress_dbus: ProgressDBus::new(),
            is_embedded,
            embedded_header,
            embedded_title,
        };

        let widgets = view_output!();

        // ─── Action Registrations ───
        let about_action: RelmAction<AboutAction> = {
            let sender = sender.input_sender().clone();
            RelmAction::new_stateless(move |_| {
                sender.emit(UpdatesPanelMsg::ShowAbout);
            })
        };

        let preferences_action: RelmAction<PreferencesAction> = {
            let sender = sender.input_sender().clone();
            RelmAction::new_stateless(move |_| {
                sender.emit(UpdatesPanelMsg::ShowPreferences);
            })
        };

        let quit_action: RelmAction<QuitAction> = {
            let sender = sender.input_sender().clone();
            RelmAction::new_stateless(move |_| {
                sender.emit(UpdatesPanelMsg::Quit);
            })
        };

        let root_clone = root.clone();
        let shortcuts_action: RelmAction<ShortcutsAction> = RelmAction::new_stateless(move |_| {
            if let Some(window) = root_clone
                .root()
                .and_then(|r| r.downcast::<adw::ApplicationWindow>().ok())
            {
                show_shortcuts_window(&window);
            }
        });

        let rebase_action: RelmAction<RebaseAction> = {
            let sender = sender.input_sender().clone();
            RelmAction::new_stateless(move |_| {
                sender.emit(UpdatesPanelMsg::ShowRebaseDialog);
            })
        };

        let mut group = RelmActionGroup::<WindowActionGroup>::new();
        group.add_action(about_action);
        group.add_action(preferences_action);
        group.add_action(quit_action);
        group.add_action(shortcuts_action);
        group.add_action(rebase_action);

        let install_action: RelmAction<InstallAction> = {
            let sender = sender.input_sender().clone();
            RelmAction::new_stateless(move |_| {
                sender.emit(UpdatesPanelMsg::InstallFromCheck);
            })
        };
        group.add_action(install_action);

        let whats_new_action: RelmAction<WhatsNewAction> = {
            let s = sender.input_sender().clone();
            RelmAction::new_stateless(move |_| s.emit(UpdatesPanelMsg::ShowWhatsNew))
        };
        group.add_action(whats_new_action);

        let restart_action: RelmAction<RestartAction> = {
            let s = sender.input_sender().clone();
            RelmAction::new_stateless(move |_| s.emit(UpdatesPanelMsg::RequestReboot))
        };
        group.add_action(restart_action);

        let dismiss_action: RelmAction<DismissBannerAction> = {
            let s = sender.input_sender().clone();
            RelmAction::new_stateless(move |_| s.emit(UpdatesPanelMsg::DismissBanner))
        };
        group.add_action(dismiss_action);

        let powerwash_action: RelmAction<PowerwashAction> = {
            let s = sender.input_sender().clone();
            RelmAction::new_stateless(move |_| s.emit(UpdatesPanelMsg::TriggerPowerwash))
        };
        group.add_action(powerwash_action);

        let factory_reset_action: RelmAction<FactoryResetAction> = {
            let s = sender.input_sender().clone();
            RelmAction::new_stateless(move |_| s.emit(UpdatesPanelMsg::TriggerFactoryReset))
        };
        group.add_action(factory_reset_action);

        group.register_for_widget(&root);

        // Keyboard Shortcuts settings
        let app = relm4::main_application();
        app.set_accelerators_for_action::<QuitAction>(&["<primary>q"]);
        app.set_accelerators_for_action::<ShortcutsAction>(&["<primary>question"]);
        app.set_accelerators_for_action::<PreferencesAction>(&["<primary>comma"]);
        app.set_accelerators_for_action::<InstallAction>(&["<primary>i"]);
        app.set_accelerators_for_action::<RebaseAction>(&["<primary><shift>r"]);
        app.set_accelerators_for_action::<WhatsNewAction>(&["<primary>w"]);
        app.set_accelerators_for_action::<RestartAction>(&["<primary><shift>b"]);
        app.set_accelerators_for_action::<DismissBannerAction>(&["<primary>BackSpace"]);

        // Preflight update check
        if model.settings.mock_identity.is_some() {
            let input_sender = sender.input_sender().clone();
            gtk::glib::idle_add_local_once(move || {
                input_sender.emit(UpdatesPanelMsg::PreflightResult(
                    PreflightStatus::UpdateAvailable,
                ));
            });
        } else {
            let input_sender = sender.input_sender().clone();
            gtk::glib::idle_add_local_once(move || {
                std::thread::spawn(move || {
                    crate::runtime::block_on(async move {
                        // Read-only probe, so it is journalled but deliberately
                        // NOT suppressed: dry-run withholds commands that
                        // *change* the system, and blocking this one would
                        // leave the hero stuck on "Checking…" with no way to
                        // ever learn whether an update exists. Passing
                        // Suppressed::No keeps every privileged invocation
                        // visible in the journal while letting reads proceed.
                        let mut cmd = match crate::privileged::privileged_async(
                            "bootc_upgrade_check",
                            serde_json::json!({ "read_only": true }),
                            &["bootc", "upgrade", "--check"],
                            crate::privileged::Privilege::Pkexec,
                            crate::action_journal::Suppressed::No,
                        ) {
                            crate::privileged::ExecAsync::Run(c) => c,
                            // Unreachable: Suppressed::No never blocks.
                            crate::privileged::ExecAsync::Suppressed => return,
                        };
                        let timeout = std::time::Duration::from_secs(15);
                        let status = tokio::select! {
                            result = cmd.status() => {
                                match result {
                                    Ok(s) => Some(s),
                                    Err(_) => None,
                                }
                            }
                            _ = tokio::time::sleep(timeout) => None,
                        };

                        let result = match status {
                            Some(s) => match s.code() {
                                Some(0) => PreflightStatus::UpdateAvailable,
                                Some(77) => PreflightStatus::UpToDate,
                                _ => PreflightStatus::Unknown,
                            },
                            None => PreflightStatus::Unknown,
                        };
                        input_sender.emit(UpdatesPanelMsg::PreflightResult(result));
                    });
                });
            });
        }

        // Prefetch image version history
        gtk::glib::idle_add_local_once(|| {
            std::thread::spawn(|| {
                crate::runtime::block_on(async {
                    let svc = crate::service::global();
                    if let Ok(image) = svc.current_image().await {
                        let _ = svc.list_versions(&image, 120).await;
                    }
                });
            });
        });

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        match msg {
            UpdatesPanelMsg::StartUpdate { skip_metered_check } => {
                if self.state == AppState::Updating {
                    return;
                }

                if !skip_metered_check
                    && self.settings.pause_on_metered
                    && gtk::gio::NetworkMonitor::default().is_network_metered()
                {
                    let dialog = adw::AlertDialog::new(
                        Some("Metered Connection Detected"),
                        Some(
                            "You're on a limited or cellular connection. Automatic updates are paused, but you can continue manually.",
                        ),
                    );
                    dialog.add_response("cancel", "_Cancel");
                    dialog.add_response("proceed", "_Update Anyway");
                    dialog.set_default_response(Some("cancel"));
                    dialog.set_close_response("cancel");

                    let update_sender = sender.input_sender().clone();
                    dialog.connect_response(None, move |_, response| {
                        if response == "proceed" {
                            update_sender.emit(UpdatesPanelMsg::StartUpdate {
                                skip_metered_check: true,
                            });
                        }
                    });

                    let parent_widget = self.status_view.widget().clone().upcast::<gtk::Widget>();
                    dialog.present(Some(&parent_widget));
                    return;
                }

                tracing::info!("Routing StartUpdate through the update modal");
                sender.input(UpdatesPanelMsg::OpenCheckDialog);
                return;
            }

            UpdatesPanelMsg::OpenCheckDialog => {
                self.progress_dbus
                    .update("checking", 0.0, "Checking for updates…");
                let parent_widget = self.status_view.widget().clone().upcast::<gtk::Widget>();
                let result_sender = sender.input_sender().clone();
                let install_sender = sender.input_sender().clone();
                show_update_check_dialog(
                    &parent_widget,
                    self.settings.dev_mode || self.settings.dry_run,
                    self.sim_scenario,
                    move |result| {
                        result_sender.emit(UpdatesPanelMsg::CheckComplete(result));
                    },
                    move || {
                        install_sender.emit(UpdatesPanelMsg::InstallFromCheck);
                    },
                );
            }

            UpdatesPanelMsg::CheckComplete(result) => {
                tracing::info!(
                    system_update = result.system_update,
                    sources = result.sources_with_updates,
                    "Update check completed"
                );
                if result.sources_with_updates > 0 {
                    self.preflight_status = PreflightStatus::UpdateAvailable;
                    self.progress_dbus.update("idle", 0.0, "Updates available");
                } else {
                    self.preflight_status = PreflightStatus::UpToDate;
                    self.progress_dbus.reset();
                }
                self.status_view.emit(StatusViewInput::PreflightResult(
                    self.preflight_status.clone(),
                ));
            }

            UpdatesPanelMsg::InstallFromCheck => {
                sender.input(UpdatesPanelMsg::StartUpdate {
                    skip_metered_check: true,
                });
            }

            UpdatesPanelMsg::OutputLine(line) => {
                self.log_lines.push(line.clone());
                self.status_view.emit(StatusViewInput::AppendLog(line));
            }

            UpdatesPanelMsg::ModuleStarted(module) => {
                let key = module.key();
                tracing::debug!("Module started: {}", key);
                let module_count = match module {
                    crate::orchestrator::Module::System => 0,
                    crate::orchestrator::Module::Flatpak => 1,
                    crate::orchestrator::Module::Brew => 2,
                    crate::orchestrator::Module::Distrobox => 3,
                };
                let progress = (module_count as f64 / 4.0).min(0.95);
                self.progress_dbus.set_progress(progress);
                self.progress_dbus
                    .set_message(&format!("Updating {}…", key));
                self.status_view
                    .emit(StatusViewInput::ModuleStarted(module));
            }

            UpdatesPanelMsg::ModuleFinished(module, status) => {
                tracing::debug!("Module finished: {} {:?}", module.key(), status);
                self.status_view
                    .emit(StatusViewInput::ModuleFinished(module, status));
            }

            UpdatesPanelMsg::UpdateComplete => {
                tracing::info!("System update completed successfully");
                self.state = AppState::Complete;
                self.cancel_tx = None;
                self.progress_dbus
                    .update("complete", 1.0, "Update complete");
                self.update_subtitle();
                self.status_view
                    .emit(StatusViewInput::StateChanged(AppState::Complete));

                send_notification(
                    "update-complete",
                    "System Update Complete",
                    "Your system has been updated. Restart to apply changes.",
                );
            }

            UpdatesPanelMsg::UpdateUpToDate => {
                tracing::info!("System is already up to date (uupd exit 77)");
                self.state = AppState::UpToDate;
                self.cancel_tx = None;
                self.progress_dbus.update("complete", 1.0, "Up to date");
                self.update_subtitle();
                self.status_view
                    .emit(StatusViewInput::StateChanged(AppState::UpToDate));
            }

            UpdatesPanelMsg::UpdateFailed(err) => {
                tracing::error!("System update failed: {}", err);
                self.state = AppState::Error(err.clone());
                self.cancel_tx = None;
                self.progress_dbus.update("error", 0.0, &err);
                self.update_subtitle();

                send_notification("update-failed", "System Update Failed", &err);
                self.status_view
                    .emit(StatusViewInput::StateChanged(AppState::Error(err)));
            }

            UpdatesPanelMsg::CancelUpdate => {
                if let Some(tx) = self.cancel_tx.take() {
                    tracing::info!("User requested update cancellation");
                    let _ = tx.send(());
                }
            }

            UpdatesPanelMsg::RequestReboot => {
                let dialog = adw::AlertDialog::builder()
                    .heading("Restart System?")
                    .body("The system will restart to apply updates. Save any open work before continuing.")
                    .build();

                dialog.add_response("cancel", "_Cancel");
                dialog.add_response("restart", "_Restart");
                dialog.set_response_appearance("restart", adw::ResponseAppearance::Destructive);
                dialog.set_default_response(Some("cancel"));
                dialog.set_close_response("cancel");

                let reboot_sender = sender.input_sender().clone();
                dialog.connect_response(None, move |_, response| {
                    if response == "restart" {
                        reboot_sender.emit(UpdatesPanelMsg::ConfirmReboot);
                    }
                });

                let parent_widget = self.status_view.widget().clone().upcast::<gtk::Widget>();
                dialog.present(Some(&parent_widget));
            }

            UpdatesPanelMsg::ConfirmReboot => {
                let suppressed = crate::action_journal::Suppressed::from_flags(
                    self.settings.dev_mode,
                    self.settings.dry_run,
                );
                let reason = if self.settings.dry_run && !self.settings.dev_mode {
                    "dry-run"
                } else {
                    "developer mode"
                };

                match crate::privileged::privileged_async(
                    "reboot",
                    serde_json::json!({}),
                    &["systemctl", "reboot"],
                    crate::privileged::Privilege::Pkexec,
                    suppressed,
                ) {
                    crate::privileged::ExecAsync::Suppressed => {
                        let toast = adw::Toast::new(&format!("Reboot suppressed ({})", reason));
                        toast.set_timeout(3);
                        self.toast_overlay.add_toast(toast);
                    }
                    crate::privileged::ExecAsync::Run(mut cmd) => {
                        tracing::info!("User confirmed system reboot");
                        crate::runtime::spawn(async move {
                            if let Err(e) = cmd.status().await {
                                tracing::error!("Failed to initiate reboot: {}", e);
                            }
                        });
                    }
                }
            }

            UpdatesPanelMsg::ShowRebaseDialog => {
                let parent_widget = self.status_view.widget().clone().upcast::<gtk::Widget>();
                // No suppression flag is passed in. The dialog used to receive
                // `dev_mode || dry_run` under the parameter name `dev_mode`
                // and branch to a simulated path that never reached the
                // privileged chokepoint — so a dry run performed no switch
                // *and* recorded no intent. Suppression is decided at the
                // chokepoint now; see run_rebase in rebase_dialog.rs.
                let s = sender.input_sender().clone();
                let on_show_changelog: std::rc::Rc<dyn Fn(String)> = std::rc::Rc::new(move |tag| {
                    s.emit(UpdatesPanelMsg::ShowChangelogForTag(tag));
                });
                show_rebase_dialog(&parent_widget, on_show_changelog);
            }

            UpdatesPanelMsg::ShowAbout => {
                let about = adw::AboutDialog::builder()
                    .application_name("Finupdate")
                    .application_icon(config::APP_ID)
                    .developer_name("Project Bluefin")
                    .version(config::VERSION)
                    .website("https://projectbluefin.io")
                    .issue_url("https://github.com/castrojo/finupdate/issues")
                    .license_type(gtk::License::MitX11)
                    .developers(vec!["Project Bluefin Contributors"])
                    .comments("A friendly system update frontend for Bluefin")
                    .build();

                let parent_widget = self.status_view.widget().clone().upcast::<gtk::Widget>();
                about.present(Some(&parent_widget));
            }

            UpdatesPanelMsg::ShowPreferences => {
                let parent_widget = self.status_view.widget().clone().upcast::<gtk::Widget>();
                let s1 = sender.input_sender().clone();
                let s2 = sender.input_sender().clone();
                show_preferences(&parent_widget, self.settings.clone(), s1, move |updated| {
                    s2.emit(UpdatesPanelMsg::SettingsChanged(updated));
                });
            }

            UpdatesPanelMsg::SettingsChanged(new_settings) => {
                tracing::debug!("Settings updated: dev_mode={}", new_settings.dev_mode);
                self.settings = new_settings.clone();
                self.dev_banner.set_revealed(self.settings.dev_mode);
                self.status_view
                    .emit(StatusViewInput::SettingsChanged(new_settings));
            }

            UpdatesPanelMsg::PreflightResult(status) => {
                self.preflight_status = status.clone();
                self.status_view
                    .emit(StatusViewInput::PreflightResult(status));
            }

            UpdatesPanelMsg::PageChanged(page) => {
                self.current_page = page.clone();
                let page_label = match page.as_str() {
                    "main" => "Finupdate",
                    "history" => "Version History",
                    "source" => "Image Source",
                    "changelog" => "What’s New",
                    _ => "Finupdate",
                };
                self.embedded_title.set_title(page_label);
                sender
                    .output(UpdatesPanelOutput::PageChanged(page))
                    .unwrap();
            }

            UpdatesPanelMsg::GoBack => {
                self.status_view
                    .emit(StatusViewInput::ShowPage("main".to_string()));
            }

            UpdatesPanelMsg::ShowWhatsNew => {
                self.status_view
                    .emit(StatusViewInput::SelectChangelogVersion(String::new()));
            }

            UpdatesPanelMsg::ShowChangelogForTag(tag) => {
                self.status_view
                    .emit(StatusViewInput::SelectChangelogVersion(tag));
            }

            UpdatesPanelMsg::DismissBanner => {
                self.status_view.emit(StatusViewInput::DismissBanner);
            }

            UpdatesPanelMsg::TriggerPowerwash => {
                self.status_view
                    .emit(StatusViewInput::TogglePin("powerwash".to_string()));
            }

            UpdatesPanelMsg::TriggerFactoryReset => {
                self.status_view
                    .emit(StatusViewInput::TogglePin("factory".to_string()));
            }

            UpdatesPanelMsg::ShowStatusPage(page) => {
                // Logged because navigation is otherwise unobservable to the
                // GUI suite: GTK4 rasterises text into textures, so under
                // Broadway the DOM has no text nodes to assert against and a
                // screenshot is the only other evidence. This line lets a
                // check say "we landed on the changelog page" rather than
                // "nothing crashed".
                tracing::info!(page = %page, "Navigating to status page");
                self.status_view.emit(StatusViewInput::ShowPage(page));
            }

            UpdatesPanelMsg::ToggleDevMode(enabled) => {
                tracing::info!("Developer mode toggled via menu: {}", enabled);
                self.settings.dev_mode = enabled;
                self.settings.save();
                self.dev_banner.set_revealed(enabled);
            }

            UpdatesPanelMsg::SetSimScenario(scenario) => {
                tracing::info!(?scenario, "Selected developer simulation scenario");
                self.sim_scenario = scenario;
            }

            UpdatesPanelMsg::Quit => {
                if self.state == AppState::Updating {
                    let root_widget = self.status_view.widget().clone();
                    let dialog = adw::AlertDialog::builder()
                        .heading("Update in Progress")
                        .body("An update is currently running. Closing now may leave your system in an inconsistent state.")
                        .build();

                    dialog.add_response("cancel", "_Keep Waiting");
                    dialog.add_response("close", "_Close Anyway");
                    dialog.set_response_appearance("close", adw::ResponseAppearance::Destructive);
                    dialog.set_default_response(Some("cancel"));
                    dialog.set_close_response("cancel");

                    dialog.connect_response(None, move |_, response| {
                        if response == "close" {
                            relm4::main_application().quit();
                        }
                    });

                    if let Some(window) = root_widget
                        .root()
                        .and_then(|w| w.downcast::<gtk::Window>().ok())
                    {
                        dialog.present(Some(&window));
                    }
                } else {
                    relm4::main_application().quit();
                }
            }
        }
    }
}

impl UpdatesPanel {
    /// Update the header bar subtitle to reflect current state.
    fn update_subtitle(&self) {
        let subtitle = match &self.state {
            AppState::Idle => None,
            AppState::Updating => Some("Updating…"),
            AppState::Complete => Some("Update complete"),
            AppState::UpToDate => Some("Already up to date"),
            AppState::Error(_) => Some("Update failed"),
        };
        if let Some(window) = self
            .status_view
            .widget()
            .root()
            .and_then(|w| w.downcast::<gtk::Window>().ok())
        {
            let title = match subtitle {
                Some(s) => format!("System Update — {}", s),
                None => "System Update".to_string(),
            };
            window.set_title(Some(&title));
        }
    }
}

// ─── Standalone Window Wrapper Component (App) ─────────────────────────────

/// Top-level standalone window model.
pub struct App {
    header_bar: adw::HeaderBar,
    window_title: adw::WindowTitle,
    back_btn: gtk::Button,
    current_page: String,
    updates_panel: Controller<UpdatesPanel>,
}

/// Messages the App component can receive.
#[derive(Debug)]
pub enum AppMsg {
    UpdatesPanelOutput(UpdatesPanelOutput),
    GoBack,
    CloseRequest,
}

#[relm4::component(pub)]
impl SimpleComponent for App {
    type Init = ();
    type Input = AppMsg;
    type Output = ();

    view! {
        #[root]
        adw::ApplicationWindow {
            set_title: Some("Finupdate"),
            // Overridable at runtime via FINUPDATE_WINDOW_SIZE — applied in
            // init() below, since relm4's view! macro needs a literal pair here.
            set_default_size: (750, 700),
            // HIG requires a primary window to work down to 360px. The old
            // 400px floor made that impossible, and mattered beyond phones:
            // gnome-control-center resizes its content pane from the shell's
            // own breakpoints, so a panel that refuses to narrow forces the
            // whole Settings window wider than the user asked for.
            set_width_request: 360,
            set_height_request: 480,

            // Narrow layout: the window title collapses to the app name alone
            // so the subtitle can't force horizontal overflow in the header.
            add_breakpoint = adw::Breakpoint::new(
                adw::BreakpointCondition::new_length(
                    adw::BreakpointConditionLengthType::MaxWidth,
                    500.0,
                    adw::LengthUnit::Sp,
                )
            ) {
                add_setter: (&model.window_title, "subtitle", Some(&"".to_value())),
            },

            adw::ToolbarView {
                add_top_bar = &model.header_bar.clone() -> adw::HeaderBar {
                    set_title_widget: Some(&model.window_title.clone()),
                    pack_start = &model.back_btn.clone() -> gtk::Button {
                        set_tooltip_text: Some("Back"),
                        connect_clicked[sender] => move |_| {
                            sender.input(AppMsg::GoBack);
                        }
                    },
                    pack_end = &gtk::MenuButton {
                        set_icon_name: "open-menu-symbolic",
                        set_tooltip_text: Some("Main Menu"),
                        set_menu_model: Some(&main_menu),
                    },
                },

                #[wrap(Some)]
                set_content = model.updates_panel.widget(),
            }
        }
    }

    menu! {
        main_menu: {
            section! {
                "_Keyboard Shortcuts" => ShortcutsAction,
                "_About Finupdate" => AboutAction,
            },
            section! {
                "_Quit" => QuitAction,
            }
        }
    }

    fn init(
        _init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let updates_panel =
            UpdatesPanel::builder()
                .launch(false)
                .forward(sender.input_sender(), |output| match output {
                    UpdatesPanelOutput::PageChanged(page) => {
                        AppMsg::UpdatesPanelOutput(UpdatesPanelOutput::PageChanged(page))
                    }
                });

        let header_bar = adw::HeaderBar::new();
        let window_title = adw::WindowTitle::new("Finupdate", "");
        // Icon-only controls need both a tooltip and an accessible label —
        // without them the control is unusable with a screen reader and
        // unlabelled for AT-SPI-driven tests.
        let back_btn = gtk::Button::builder()
            .icon_name("go-previous-symbolic")
            .tooltip_text("Back")
            .visible(false)
            .build();
        back_btn.update_property(&[gtk::accessible::Property::Label("Back")]);

        // Standalone Window actions
        let updates_panel_sender = updates_panel.sender().clone();
        let about_action: RelmAction<AboutAction> = {
            let s = updates_panel_sender.clone();
            RelmAction::new_stateless(move |_| {
                s.send(UpdatesPanelMsg::ShowAbout).unwrap();
            })
        };

        let updates_panel_sender2 = updates_panel.sender().clone();
        let quit_action: RelmAction<QuitAction> = {
            let s = updates_panel_sender2.clone();
            RelmAction::new_stateless(move |_| {
                s.send(UpdatesPanelMsg::Quit).unwrap();
            })
        };

        let root_clone = root.clone();
        let shortcuts_action: RelmAction<ShortcutsAction> = RelmAction::new_stateless(move |_| {
            show_shortcuts_window(&root_clone);
        });

        let mut group = RelmActionGroup::<WindowActionGroup>::new();
        group.add_action(about_action);
        group.add_action(quit_action);
        group.add_action(shortcuts_action);
        group.register_for_widget(&root);

        let close_sender = sender.input_sender().clone();
        root.connect_close_request(move |_| {
            close_sender.emit(AppMsg::CloseRequest);
            gtk::glib::Propagation::Stop
        });

        let model = App {
            header_bar,
            window_title,
            back_btn,
            current_page: "main".to_string(),
            updates_panel,
        };

        let widgets = view_output!();

        // Applied *after* view_output!(), which sets the production default
        // size — doing it earlier just gets overwritten.
        if let Some((w, h)) = parse_window_size() {
            root.set_default_size(w, h);
        }

        // FINUPDATE_MEASURE=1 reports the window's real minimum width once the
        // tree is built. HIG wants a primary window usable at 360px, and
        // lowering `width-request` alone does not achieve that if some child
        // demands more — this says which number we are actually up against,
        // instead of guessing at candidates. Bisect by hiding subtrees.
        if std::env::var_os("FINUPDATE_MEASURE").is_some() {
            let win = root.clone();
            // Deferred: an idle callback fires before the tree is realised, and
            // an unrealised widget measures as 0, which looks like success.
            gtk::glib::timeout_add_local_once(std::time::Duration::from_secs(6), move || {
                let (wmin, wnat, _, _) = win.measure(gtk::Orientation::Horizontal, -1);
                println!("MEASURE window min_width={wmin} nat_width={wnat}");
                match win.content() {
                    Some(c) => {
                        let (cmin, cnat, _, _) = c.measure(gtk::Orientation::Horizontal, -1);
                        println!("MEASURE content min_width={cmin} nat_width={cnat}");
                        // Recurse, printing only widgets whose own minimum meets
                        // the threshold. Anything below it cannot be what is
                        // holding the window open, so this narrows straight to
                        // the offender instead of dumping the whole tree.
                        let threshold: i32 = std::env::var("FINUPDATE_MEASURE_MIN")
                            .ok()
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(400);
                        fn walk(w: &gtk::Widget, depth: usize, threshold: i32) {
                            let (m, n, _, _) = w.measure(gtk::Orientation::Horizontal, -1);
                            if m >= threshold {
                                let label =
                                    w.buildable_id().map(|s| s.to_string()).unwrap_or_default();
                                println!(
                                    "MEASURE {:indent$}{} min={m} nat={n} {label}",
                                    "",
                                    w.type_().name(),
                                    indent = depth * 2,
                                );
                            }
                            let mut child = w.first_child();
                            while let Some(c) = child {
                                walk(&c, depth + 1, threshold);
                                child = c.next_sibling();
                            }
                        }
                        walk(&c, 1, threshold);
                    }
                    None => println!("MEASURE content=None"),
                }
            });
        }

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, _sender: ComponentSender<Self>) {
        match msg {
            AppMsg::UpdatesPanelOutput(UpdatesPanelOutput::PageChanged(page)) => {
                self.current_page = page.clone();
                let page_label = match page.as_str() {
                    "main" => "Finupdate",
                    "history" => "Version History",
                    "source" => "Image Source",
                    "changelog" => "What’s New",
                    _ => "Finupdate",
                };
                self.window_title.set_title(page_label);
                self.window_title
                    .set_subtitle(if page == "main" { "" } else { "Finupdate" });

                if let Some(window) = self
                    .updates_panel
                    .widget()
                    .root()
                    .and_then(|r| r.downcast::<adw::ApplicationWindow>().ok())
                {
                    let win_title = if page == "main" {
                        "Finupdate".to_string()
                    } else {
                        format!("Finupdate — {}", page_label)
                    };
                    window.set_title(Some(&win_title));
                }
                self.back_btn.set_visible(page != "main");
            }
            AppMsg::GoBack => {
                self.updates_panel
                    .emit(UpdatesPanelMsg::ShowStatusPage("main".to_string()));
            }
            AppMsg::CloseRequest => {
                if self.updates_panel.model().state == AppState::Updating {
                    let dialog = adw::AlertDialog::builder()
                        .heading("Update in Progress")
                        .body("An update is currently running. Closing now may leave your system in an inconsistent state.")
                        .build();

                    dialog.add_response("cancel", "_Keep Waiting");
                    dialog.add_response("close", "_Close Anyway");
                    dialog.set_response_appearance("close", adw::ResponseAppearance::Destructive);
                    dialog.set_default_response(Some("cancel"));
                    dialog.set_close_response("cancel");

                    dialog.connect_response(None, move |_, response| {
                        if response == "close" {
                            relm4::main_application().quit();
                        }
                    });

                    if let Some(window) = self
                        .updates_panel
                        .widget()
                        .root()
                        .and_then(|r| r.downcast::<adw::ApplicationWindow>().ok())
                    {
                        dialog.present(Some(&window));
                    }
                } else {
                    relm4::main_application().quit();
                }
            }
        }
    }
}

// ─── Dialog and Helper Functions ──────────────────────────────────────────

/// Show the keyboard shortcuts window.
/// Parse `FINUPDATE_WINDOW_SIZE` (e.g. `360x640`) into a default window size.
///
/// Test-only affordance. Broadway derives the rendered surface from the GTK
/// window's own size, not the browser viewport, so the screenshot suite has no
/// other way to capture the narrow breakpoint layout. Returns None (keeping the
/// production default) when unset or malformed.
fn parse_window_size() -> Option<(i32, i32)> {
    let raw = std::env::var("FINUPDATE_WINDOW_SIZE").ok()?;
    let (w, h) = raw.split_once(['x', 'X'])?;
    let w = w.trim().parse::<i32>().ok()?;
    let h = h.trim().parse::<i32>().ok()?;
    // Guard against a typo silently producing an unusable window.
    if w < 200 || h < 200 {
        tracing::warn!("ignoring FINUPDATE_WINDOW_SIZE={raw}: below 200x200");
        return None;
    }
    Some((w, h))
}

fn show_shortcuts_window(window: &adw::ApplicationWindow) {
    let shortcuts = gtk::ShortcutsWindow::builder()
        .transient_for(window)
        .modal(true)
        .build();

    let section = gtk::ShortcutsSection::builder()
        .section_name("shortcuts")
        .visible(true)
        .build();

    let app_group = gtk::ShortcutsGroup::builder().title("Application").build();
    for (title, accel) in [
        ("Preferences", "<Primary>comma"),
        ("Keyboard Shortcuts", "<Primary>question"),
        ("Quit", "<Primary>q"),
    ] {
        app_group.add_shortcut(
            &gtk::ShortcutsShortcut::builder()
                .title(title)
                .accelerator(accel)
                .build(),
        );
    }
    section.add_group(&app_group);

    let updates_group = gtk::ShortcutsGroup::builder().title("Updates").build();
    for (title, accel) in [
        ("Install staged update", "<Primary>i"),
        ("What's new in this update", "<Primary>w"),
        ("Restart to apply", "<Primary><Shift>b"),
        ("Dismiss update banner", "<Primary>BackSpace"),
    ] {
        updates_group.add_shortcut(
            &gtk::ShortcutsShortcut::builder()
                .title(title)
                .accelerator(accel)
                .build(),
        );
    }
    section.add_group(&updates_group);

    let system_group = gtk::ShortcutsGroup::builder().title("System").build();
    for (title, accel) in [("Open Version History", "<Primary><Shift>r")] {
        system_group.add_shortcut(
            &gtk::ShortcutsShortcut::builder()
                .title(title)
                .accelerator(accel)
                .build(),
        );
    }
    section.add_group(&system_group);

    shortcuts.add_section(&section);
    shortcuts.set_visible(true);
}

/// Send a desktop notification via GApplication.
fn send_notification(id: &str, title: &str, body: &str) {
    let app = relm4::main_application();
    let notification = gtk::gio::Notification::new(title);
    notification.set_body(Some(body));
    notification.set_icon(&gtk::gio::ThemedIcon::new(
        "software-update-available-symbolic",
    ));
    app.send_notification(Some(id), &notification);
}

// Action group and actions for the window-level menu.
relm4::new_action_group!(WindowActionGroup, "win");
relm4::new_stateless_action!(AboutAction, WindowActionGroup, "about");
relm4::new_stateless_action!(PreferencesAction, WindowActionGroup, "preferences");
relm4::new_stateless_action!(QuitAction, WindowActionGroup, "quit");
relm4::new_stateless_action!(ShortcutsAction, WindowActionGroup, "show-shortcuts");
relm4::new_stateless_action!(RebaseAction, WindowActionGroup, "rebase-history");
relm4::new_stateless_action!(InstallAction, WindowActionGroup, "install");
relm4::new_stateless_action!(WhatsNewAction, WindowActionGroup, "whats-new");
relm4::new_stateless_action!(RestartAction, WindowActionGroup, "restart");
relm4::new_stateless_action!(DismissBannerAction, WindowActionGroup, "dismiss-banner");
relm4::new_stateless_action!(PowerwashAction, WindowActionGroup, "powerwash");
relm4::new_stateless_action!(FactoryResetAction, WindowActionGroup, "factory-reset");

fn inject_app_css() {
    let css = gtk::CssProvider::new();
    css.load_from_string(
        r#"
        .destructive-title label {
            color: @error_color;
        }
        .deploy-indicator-current {
            border: 2px solid @accent_color;
            background-color: @accent_color;
            min-width: 14px;
            min-height: 14px;
            border-radius: 8px;
        }
        .deploy-indicator-staged {
            border: 2px solid @accent_color;
            background-color: transparent;
            min-width: 14px;
            min-height: 14px;
            border-radius: 8px;
        }
        .deploy-indicator-archive {
            border: 2px solid @window_fg_color;
            opacity: 0.5;
            background-color: transparent;
            min-width: 14px;
            min-height: 14px;
            border-radius: 8px;
        }
        "#,
    );
    gtk::style_context_add_provider_for_display(
        &gtk::gdk::Display::default().expect("display"),
        &css,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service;
    use std::sync::Once;

    static INIT: Once = Once::new();
    static GTK_OK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    fn try_init_gtk() -> bool {
        INIT.call_once(|| {
            if std::env::var("GDK_BACKEND").is_err() {
                unsafe {
                    std::env::set_var("GDK_BACKEND", "broadway");
                }
            }
            if std::env::var("FINUPDATE_IMAGE").is_err() {
                unsafe {
                    std::env::set_var(
                        "FINUPDATE_IMAGE",
                        "ghcr.io/projectbluefin/dakota:latest-20260527",
                    );
                }
            }

            let gtk_ok = gtk::init().is_ok() && adw::init().is_ok();
            if gtk_ok {
                service::init(service::BootcUpdaterService::new());
            }
            GTK_OK.store(gtk_ok, std::sync::atomic::Ordering::SeqCst);
        });
        GTK_OK.load(std::sync::atomic::Ordering::SeqCst)
    }

    #[tokio::test]
    async fn test_app_ui_flow() {
        if !try_init_gtk() {
            eprintln!("test_app_ui_flow: skipping – GTK/broadway unavailable in this environment");
            return;
        }

        // Test inner UpdatesPanel directly
        let controller = UpdatesPanel::builder().launch(false).detach();

        assert_eq!(controller.model().state, AppState::Idle);
        assert_eq!(controller.model().current_page, "main");

        controller
            .sender()
            .send(UpdatesPanelMsg::ToggleDevMode(true))
            .unwrap();
        controller
            .sender()
            .send(UpdatesPanelMsg::SetSimScenario(SimulationScenario::Success))
            .unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        assert!(controller.model().settings.dev_mode);
        assert_eq!(controller.model().sim_scenario, SimulationScenario::Success);

        controller
            .sender()
            .send(UpdatesPanelMsg::PageChanged("preferences".to_string()))
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(controller.model().current_page, "preferences");

        controller.sender().send(UpdatesPanelMsg::GoBack).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(controller.model().current_page, "main");
    }
}
