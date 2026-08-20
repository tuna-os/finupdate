//! Up-to-date state page builder for StatusView.

use adw::prelude::*;
use relm4::prelude::*;

use crate::app::AppState;

use super::{StatusView, StatusViewInput};

/// Build the "You're already up to date" status page with a single
/// "Done" button that returns to the idle overview.
pub(super) fn build_uptodate_page(sender: &ComponentSender<StatusView>) -> adw::StatusPage {
    let page = adw::StatusPage::new();
    page.set_icon_name(Some("emblem-ok-symbolic"));
    page.set_title("Up to Date");
    page.set_description(Some("No updates available."));

    let done_btn = gtk::Button::with_label("Done");
    done_btn.add_css_class("pill");
    done_btn.set_halign(gtk::Align::Center);
    let done_sender = sender.input_sender().clone();
    done_btn.connect_clicked(move |_| {
        done_sender.emit(StatusViewInput::StateChanged(AppState::Idle));
    });

    page.set_child(Some(&done_btn));
    page
}
