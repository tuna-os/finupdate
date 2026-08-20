//! Builders for the idle status page.
//!
//! Keeping idle-only widget construction here prevents the component
//! initializer from becoming the owner of every state page's layout.

use adw::prelude::*;
use relm4::prelude::*;

use crate::ui::bootc_probe::{
    HeroLogo, detect_bootc_image_info, is_pinned_tag, read_booted_image_summary, read_image_info,
    read_logo_icon_name,
};
use crate::ui::settings_io::apply_auto_updates_setting;

use super::{StatusView, StatusViewInput, StatusViewOutput, allow_narrow};

/// Everything the parent component needs to keep referencing after the
/// idle page is built: the page itself plus every widget that `update()`
/// and `refresh_idle_description()` touch dynamically.
pub(super) struct IdleWidgets {
    pub(super) page: adw::PreferencesPage,
    pub(super) hero_row: adw::ActionRow,
    pub(super) status_pill: gtk::Label,
    pub(super) hero_action_btn: gtk::Button,
    pub(super) hero_schedule_btn: gtk::Button,
    pub(super) update_banner_group: adw::PreferencesGroup,
    pub(super) banner_title_row: adw::ActionRow,
    pub(super) banner_install_btn: gtk::Button,
    pub(super) banner_whats_new_btn: gtk::Button,
    pub(super) banner_restart_btn: gtk::Button,
    pub(super) banner_discard_btn: gtk::Button,
    pub(super) auto_update_switch: adw::SwitchRow,
    pub(super) pin_group: adw::PreferencesGroup,
    pub(super) pin_row: adw::ActionRow,
    pub(super) images_count_label: gtk::Label,
}

/// Build the whole idle page: hero row, pin group, update banner and the
/// settings card (check/auto-updates + advanced). Constructed in `init()`
/// before the `StatusView` model exists, so state is passed in and the
/// component sender is used for signal wiring — mirroring
/// `build_updating_page` / `build_source_page`.
pub(super) fn build_idle_page(
    sender: &ComponentSender<StatusView>,
    auto_updates_enabled: bool,
    initial_selected_tag: &str,
    initial_image_info: Option<String>,
    initial_subtitle: String,
) -> IdleWidgets {
    let page = adw::PreferencesPage::new();

    // ── Hero group ─────────────────────────────────────────────────────
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
    page.add(&hero_group);

    // ── Pin group ───────────────────────────────────────────────────────
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
    pin_group.set_visible(is_pinned_tag(initial_selected_tag));
    page.add(&pin_group);

    // ── Update banner group ─────────────────────────────────────────────
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
    let initial_selected_tag_3 = initial_selected_tag.to_string();
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
    page.add(&update_banner_group);

    // ── Settings card ───────────────────────────────────────────────────
    let settings = build_settings(sender, auto_updates_enabled);
    let images_count_label = gtk::Label::new(Some("3 versions"));
    images_count_label.add_css_class("dim-label");

    page.add(&settings.group);
    page.add(&settings.advanced_group);

    IdleWidgets {
        page,
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
        auto_update_switch: settings.auto_update_switch,
        pin_group,
        pin_row,
        images_count_label,
    }
}

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
