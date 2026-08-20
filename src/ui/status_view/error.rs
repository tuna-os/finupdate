//! Error state page builder for StatusView.

use adw::prelude::*;
use relm4::prelude::*;

use crate::app::AppState;

use super::{StatusView, StatusViewInput, StatusViewOutput};

/// Build the "Update Failed" status page: warning icon, a suggested
/// "Retry" action and a flat "Dismiss" fallback. The error detail text
/// is filled in dynamically by `update()` when an `AppState::Error` is
/// dispatched.
pub(super) fn build_error_page(sender: &ComponentSender<StatusView>) -> adw::StatusPage {
    let page = adw::StatusPage::new();
    page.set_icon_name(Some("dialog-warning-symbolic"));
    page.set_title("Update Failed");
    page.set_description(Some("Something went wrong."));

    let actions = gtk::Box::new(gtk::Orientation::Vertical, 8);
    actions.set_halign(gtk::Align::Center);

    let retry_btn = gtk::Button::with_label("Retry");
    retry_btn.add_css_class("suggested-action");
    retry_btn.add_css_class("pill");
    let retry_sender = sender.output_sender().clone();
    retry_btn.connect_clicked(move |_| {
        let _ = retry_sender.send(StatusViewOutput::StartUpdate);
    });
    actions.append(&retry_btn);

    let dismiss_btn = gtk::Button::with_label("Dismiss");
    dismiss_btn.add_css_class("flat");
    let dismiss_sender = sender.input_sender().clone();
    dismiss_btn.connect_clicked(move |_| {
        dismiss_sender.emit(StatusViewInput::StateChanged(AppState::Idle));
    });
    actions.append(&dismiss_btn);

    page.set_child(Some(&actions));
    page
}
