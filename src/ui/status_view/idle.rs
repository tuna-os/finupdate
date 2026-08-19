//! Builders for the idle status page.
//!
//! Keeping idle-only widget construction here prevents the component
//! initializer from becoming the owner of every state page's layout.

use adw::prelude::*;
use relm4::prelude::*;

use crate::ui::bootc_probe::{detect_bootc_image_info, read_booted_image_summary, read_image_info};
use crate::ui::settings_io::apply_auto_updates_setting;

use super::{StatusView, StatusViewOutput, allow_narrow};

pub(super) struct IdleSettings {
    pub(super) group: adw::PreferencesGroup,
    pub(super) advanced_group: adw::PreferencesGroup,
    pub(super) auto_update_switch: adw::SwitchRow,
}

pub(super) fn hero_title(image_info: Option<&str>) -> String {
    image_info.map(|s| s.to_string()).unwrap_or_else(|| {
        read_image_info()
            .or_else(|| detect_bootc_image_info().map(|(title, _, _)| title))
            .unwrap_or_else(|| "System Image".to_string())
    })
}

pub(super) fn idle_subtitle(reboot_pending: bool, last_update_text: Option<&str>) -> String {
    if reboot_pending {
        return "Reboot to update".to_string();
    }
    read_booted_image_summary()
        .or_else(|| last_update_text.map(|s| s.to_string()))
        .unwrap_or_else(|| "Current image".to_string())
}

pub(super) fn build_settings(
    sender: &ComponentSender<StatusView>,
    auto_updates_enabled: bool,
) -> IdleSettings {
    let check_row = adw::ActionRow::builder()
        .title("_Check for updates")
        .subtitle("System image, Flatpak, Homebrew, and Distrobox")
        .use_underline(true)
        .build();
    let check_btn = gtk::Button::with_label("Check");
    check_btn.set_valign(gtk::Align::Center);
    let check_sender = sender.output_sender().clone();
    check_btn.connect_clicked(move |_| {
        let _ = check_sender.send(StatusViewOutput::OpenCheckDialog);
    });
    check_row.add_suffix(&check_btn);
    allow_narrow(&check_row);

    let auto_update_switch = adw::SwitchRow::builder()
        .title("_Automatic updates")
        .subtitle("Refresh in the background on the systemd timer")
        .use_underline(true)
        .active(auto_updates_enabled)
        .build();
    allow_narrow(&auto_update_switch);
    auto_update_switch.connect_active_notify(move |row| {
        apply_auto_updates_setting(row.is_active());
    });

    let group = adw::PreferencesGroup::new();
    group.add(&check_row);
    group.add(&auto_update_switch);

    let advanced_row = adw::ActionRow::builder()
        .title("_Advanced")
        .subtitle("Automatic updates, network, and reset")
        .activatable(true)
        .use_underline(true)
        .build();
    advanced_row
        .upcast_ref::<gtk::Widget>()
        .set_accessible_role(gtk::AccessibleRole::Button);
    allow_narrow(&advanced_row);
    let chevron = gtk::Image::from_icon_name("go-next-symbolic");
    chevron.add_css_class("dim-label");
    advanced_row.add_suffix(&chevron);
    let advanced_sender = sender.output_sender().clone();
    advanced_row.connect_activated(move |_| {
        let _ = advanced_sender.send(StatusViewOutput::OpenAdvanced);
    });

    let advanced_group = adw::PreferencesGroup::new();
    advanced_group.add(&advanced_row);

    IdleSettings {
        group,
        advanced_group,
        auto_update_switch,
    }
}
