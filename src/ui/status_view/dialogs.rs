//! Confirmation dialogs for host actions (powerwash, factory reset, rollback, unpin).

use adw::prelude::*;
use relm4::prelude::*;

use crate::settings::Settings;
use crate::ui::history_list::MockDeployment;

use super::{StatusView, StatusViewInput};

impl StatusView {
    pub(super) fn show_powerwash_dialog(&self) {
        let window = self
            .stack
            .root()
            .and_then(|r| r.downcast::<gtk::Window>().ok());

        let dialog = adw::AlertDialog::builder()
            .heading("Powerwash this device?")
            .body("`/etc` will be reset to image defaults and all installed apps will be removed. Your home directory, files, and signed-in accounts are kept.")
            .build();

        dialog.add_response("cancel", "Cancel");
        dialog.add_response("powerwash", "Powerwash");
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        dialog.set_response_appearance("powerwash", adw::ResponseAppearance::Suggested);

        let toast_overlay = self.toast_overlay.clone();
        let settings_snapshot = Settings::load();
        dialog.connect_response(None, move |dlg, response| {
            if response == "powerwash" {
                // Powerwash = uninstall every user Flatpak + remove
                // every Distrobox container. Leaves /var/home, /etc,
                // and the bootc image untouched.
                if settings_snapshot.dry_run || settings_snapshot.dev_mode {
                    tracing::warn!(
                        "POWERWASH suppressed (dry_run={}, dev_mode={}). \
                         Would have called:\n  \
                         1. flatpak uninstall --user --all -y\n  \
                         2. distrobox rm -f -a",
                        settings_snapshot.dry_run,
                        settings_snapshot.dev_mode
                    );
                    let toast =
                        adw::Toast::new("Powerwash staged (dry-run, no commands run)");
                    toast_overlay.add_toast(toast);
                } else {
                    crate::ui::host_actions::run_powerwash(&toast_overlay);
                }
            }
            dlg.close();
        });
        dialog.present(window.as_ref());
    }

    pub(super) fn show_factory_reset_dialog(&self) {
        let window = self
            .stack
            .root()
            .and_then(|r| r.downcast::<gtk::Window>().ok());

        let entry = gtk::Entry::builder()
            .placeholder_text("reset")
            .margin_top(12)
            .margin_bottom(12)
            .build();
        entry.add_css_class("entry");

        let dialog = adw::AlertDialog::builder()
            .heading("Factory reset?")
            .body("Erases all user data, accounts, apps, rollback images, and settings, then redeploys the factory image. This cannot be undone.")
            .extra_child(&entry)
            .build();

        dialog.add_response("cancel", "Cancel");
        dialog.add_response("reset", "Factory Reset");
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        dialog.set_response_appearance("reset", adw::ResponseAppearance::Destructive);
        dialog.set_response_enabled("reset", false);

        let dlg_clone = dialog.clone();
        entry.connect_changed(move |ent| {
            let text = ent.text().to_string();
            dlg_clone.set_response_enabled("reset", text == "reset");
        });

        let toast_overlay = self.toast_overlay.clone();
        let settings_snapshot = Settings::load();
        dialog.connect_response(None, move |dlg, response| {
            if response == "reset" {
                // Factory reset = bootc's canonical `install reset`.
                if settings_snapshot.dry_run || settings_snapshot.dev_mode {
                    tracing::warn!(
                        "FACTORY RESET suppressed (dry_run={}, dev_mode={}). \
                         Would have called:\n  \
                         pkexec bootc install reset --experimental --apply",
                        settings_snapshot.dry_run,
                        settings_snapshot.dev_mode
                    );
                    let toast =
                        adw::Toast::new("Factory reset queued (dry-run, no commands run)");
                    toast_overlay.add_toast(toast);
                } else {
                    crate::ui::host_actions::run_bootc_install_reset(
                        &toast_overlay,
                        "Factory reset",
                    );
                }
            }
            dlg.close();
        });
        dialog.present(window.as_ref());
    }

    pub(super) fn show_rollback_dialog(
        &mut self,
        d: MockDeployment,
        sender: &ComponentSender<StatusView>,
    ) {
        let window = self
            .stack
            .root()
            .and_then(|r| r.downcast::<gtk::Window>().ok());
        let dialog = adw::AlertDialog::builder()
            .heading(format!("Roll back to {}?", d.tag))
            .body(format!(
                "The next boot will use {}:{}.\nYour current image stays on disk and remains available to roll forward.",
                d.image, d.tag
            ))
            .build();

        dialog.add_response("cancel", "Cancel");
        dialog.add_response("rollback", "Roll back");
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        dialog.set_response_appearance("rollback", adw::ResponseAppearance::Suggested);

        let dialog_sender = sender.input_sender().clone();
        dialog.connect_response(None, move |dlg, response| {
            if response == "rollback" {
                dialog_sender.emit(StatusViewInput::ConfirmRollback);
            }
            dlg.close();
        });
        self.rollback_target = Some(d);
        dialog.present(window.as_ref());
    }

    pub(super) fn show_unpin_dialog(&self, stream_tag: &str) {
        let window = self
            .stack
            .root()
            .and_then(|r| r.downcast::<gtk::Window>().ok());
        let dialog = adw::AlertDialog::builder()
            .heading(format!("Unpin to :{}?", stream_tag))
            .body(format!(
                "Your system will switch back to the floating `{}` tag and resume receiving automatic updates. A restart is required after the switch.",
                stream_tag
            ))
            .build();
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("unpin", "Unpin");
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        dialog.set_response_appearance("unpin", adw::ResponseAppearance::Suggested);

        let toast_overlay = self.toast_overlay.clone();
        let registry_uri = self.registry_uri.clone();
        let stream_tag_owned = stream_tag.to_string();
        let settings_snapshot = Settings::load();
        dialog.connect_response(None, move |dlg, response| {
            if response == "unpin" {
                if settings_snapshot.dry_run || settings_snapshot.dev_mode {
                    tracing::warn!(
                        "unpin suppressed (dry_run={}, dev_mode={})",
                        settings_snapshot.dry_run,
                        settings_snapshot.dev_mode
                    );
                    let t = adw::Toast::new("Unpin staged (dry-run, no commands run)");
                    toast_overlay.add_toast(t);
                } else {
                    crate::ui::host_actions::run_unpin_to_stream(
                        &toast_overlay,
                        registry_uri.clone(),
                        stream_tag_owned.clone(),
                    );
                }
            }
            dlg.close();
        });
        dialog.present(window.as_ref());
    }
}
