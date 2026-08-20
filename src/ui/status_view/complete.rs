//! Complete state page builder for StatusView.
//!
//! Success status page with reboot option and "Restart Tonight"
//! scheduling entry point (the latter lives on the idle hero row).

use adw::prelude::*;
use relm4::prelude::*;

use crate::app::AppState;

use super::{StatusView, StatusViewInput, StatusViewOutput};

/// Build the "Update Complete" status page: success icon, a suggested
/// "Restart…" action and a flat "Restart Later" fallback.
pub(super) fn build_complete_page(sender: &ComponentSender<StatusView>) -> adw::StatusPage {
    let page = adw::StatusPage::new();
    page.set_icon_name(Some("object-select-symbolic"));
    page.set_title("Update Complete");
    page.set_description(Some("Restart to apply changes."));

    let actions = gtk::Box::new(gtk::Orientation::Vertical, 8);
    actions.set_halign(gtk::Align::Center);

    let restart_btn = gtk::Button::with_label("Restart…");
    restart_btn.add_css_class("suggested-action");
    restart_btn.add_css_class("pill");
    let restart_sender = sender.output_sender().clone();
    restart_btn.connect_clicked(move |_| {
        let _ = restart_sender.send(StatusViewOutput::Reboot);
    });
    actions.append(&restart_btn);

    let later_btn = gtk::Button::with_label("Restart Later");
    later_btn.add_css_class("flat");
    let later_sender = sender.input_sender().clone();
    later_btn.connect_clicked(move |_| {
        later_sender.emit(StatusViewInput::StateChanged(AppState::Idle));
    });
    actions.append(&later_btn);

    page.set_child(Some(&actions));
    page
}
