//! Image Source drill-down subpage builder for StatusView.

use adw::prelude::*;
use relm4::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

use crate::ui::bootc_probe::read_bootc_image_info_config;

use super::{StatusView, StatusViewInput};

pub(super) struct ImageSourceWidgets {
    pub(super) source_nav_page: adw::NavigationPage,
    pub(super) registry_entry_row: adw::EntryRow,
    pub(super) registry_row_sub: gtk::Label,
    pub(super) tag_row: adw::ComboRow,
    pub(super) tag_model: gtk::StringList,
    pub(super) tag_raws: Rc<RefCell<Vec<String>>>,
    pub(super) tag_row_handler: gtk::glib::SignalHandlerId,
}

pub(super) fn build_source_page(
    sender: &ComponentSender<StatusView>,
    initial_registry_uri: &str,
    initial_selected_tag: &str,
) -> ImageSourceWidgets {
    let source_page = adw::PreferencesPage::new();
    let source_group = adw::PreferencesGroup::builder()
        .description("Where this device pulls its OS image from. Changes apply on next update.")
        .build();

    let registry_entry_row = adw::EntryRow::builder()
        .title("Registry")
        .text(initial_registry_uri)
        .show_apply_button(true)
        .build();
    let save_sender = sender.input_sender().clone();
    registry_entry_row.connect_apply(move |row| {
        save_sender.emit(StatusViewInput::SaveRegistryUri(row.text().to_string()));
    });
    source_group.add(&registry_entry_row);

    let tag_row = adw::ComboRow::builder()
        .title("Tag")
        .subtitle("Always the newest stable release")
        .build();
    let tags = if let Some(config) = read_bootc_image_info_config() {
        config.tags
    } else {
        let cur = initial_selected_tag.to_string();
        if !cur.is_empty() && cur != "latest" {
            vec!["latest".to_string(), cur]
        } else {
            vec!["latest".to_string()]
        }
    };
    let tag_model = gtk::StringList::new(&[]);
    let tag_raws: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(tags.clone()));
    for t in &tags {
        tag_model.append(t);
    }
    tag_row.set_model(Some(&tag_model));
    let initial_idx = tags
        .iter()
        .position(|t| t == initial_selected_tag)
        .unwrap_or(0) as u32;
    tag_row.set_selected(initial_idx);
    tag_row.set_sensitive(tags.len() > 1);
    let select_sender = sender.input_sender().clone();
    let tag_raws_for_select = tag_raws.clone();
    let tag_row_handler = tag_row.connect_selected_notify(move |row| {
        let idx = row.selected() as usize;
        if let Some(raw) = tag_raws_for_select.borrow().get(idx).cloned() {
            select_sender.emit(StatusViewInput::SelectTag(raw));
        }
    });
    source_group.add(&tag_row);

    let sig_row = adw::ActionRow::builder()
        .title("Require signed images")
        .subtitle("Only install images signed by the publisher.")
        .build();
    let sig_badge = gtk::Label::new(Some("✓ On"));
    sig_badge.add_css_class("success");
    sig_badge.add_css_class("caption");
    sig_badge.set_valign(gtk::Align::Center);
    sig_row.add_suffix(&sig_badge);
    source_group.add(&sig_row);

    source_page.add(&source_group);

    let variants_group = adw::PreferencesGroup::builder()
        .title("Variants")
        .description(
            "Switch between feature variants of this image. \
             Apply the registry change above to take effect on the next update.",
        )
        .build();
    let dx_switch = adw::SwitchRow::builder()
        .title("Developer Mode")
        .subtitle("Container tools, IDEs, and language SDKs")
        .build();
    let nvidia_switch = adw::SwitchRow::builder()
        .title("NVIDIA drivers")
        .subtitle("Required for NVIDIA GPUs")
        .build();
    variants_group.add(&dx_switch);
    variants_group.add(&nvidia_switch);
    source_page.add(&variants_group);

    let slot: std::sync::Arc<
        std::sync::Mutex<
            Option<(
                Option<crate::service::FamilyInfo>,
                Option<crate::service::ImageRef>,
            )>,
        >,
    > = std::sync::Arc::new(std::sync::Mutex::new(None));
    {
        let slot = slot.clone();
        std::thread::spawn(move || {
            let detected = crate::runtime::block_on(async {
                let svc = crate::service::global();
                let family = svc.current_family().await.ok().flatten();
                let image = svc.current_image().await.ok();
                (family, image)
            });
            *slot.lock().unwrap() = Some(detected);
        });
    }

    let dx_switch_for_timer = dx_switch.clone();
    let nvidia_switch_for_timer = nvidia_switch.clone();
    let variants_group_for_timer = variants_group.clone();
    let registry_entry_for_timer = registry_entry_row.clone();
    let registry_uri_initial = initial_registry_uri.to_string();
    gtk::glib::timeout_add_local(std::time::Duration::from_millis(200), move || {
        let Some((family_opt, image_opt)) = slot.lock().ok().and_then(|mut g| g.take()) else {
            return gtk::glib::ControlFlow::Continue;
        };

        let Some(fam) = family_opt else {
            variants_group_for_timer.set_visible(false);
            return gtk::glib::ControlFlow::Break;
        };

        let suffix = image_opt
            .as_ref()
            .and_then(|i| i.image.strip_prefix(&format!("{}-", fam.base_image)))
            .map(|s| s.to_string())
            .unwrap_or_default();
        let initial_dx = suffix.split('-').any(|p| p == "dx");
        let initial_nvidia = suffix.split('-').any(|p| p == "nvidia" || p == "open");

        let supports_dx = fam.features.iter().any(|f| f.id == "dx");
        let supports_nvidia = fam
            .features
            .iter()
            .any(|f| f.id == "nvidia" || f.id == "open");

        dx_switch_for_timer.set_visible(supports_dx);
        nvidia_switch_for_timer.set_visible(supports_nvidia);
        dx_switch_for_timer.set_active(initial_dx);
        nvidia_switch_for_timer.set_active(initial_nvidia);
        variants_group_for_timer.set_visible(supports_dx || supports_nvidia);

        let recompute = {
            let dx_switch = dx_switch_for_timer.clone();
            let nvidia_switch = nvidia_switch_for_timer.clone();
            let entry = registry_entry_for_timer.clone();
            let registry_uri = registry_uri_initial.clone();
            let fam = fam.clone();
            move || {
                let dx = dx_switch.is_active();
                let nvidia = nvidia_switch.is_active();
                let svc = crate::service::global();
                let mut feats: Vec<String> = Vec::new();
                if dx {
                    feats.push("dx".to_string());
                }
                if nvidia {
                    feats.push("nvidia".to_string());
                    feats.push("open".to_string());
                }
                let resolved = svc.resolve_target(&fam, &feats).or_else(|| {
                    if nvidia {
                        let mut plain = if dx { vec!["dx".to_string()] } else { vec![] };
                        plain.push("nvidia".to_string());
                        svc.resolve_target(&fam, &plain)
                    } else {
                        None
                    }
                });
                if let Some(target) = resolved {
                    let parts: Vec<&str> = registry_uri.split('/').collect();
                    if parts.len() >= 2 {
                        entry.set_text(&format!("{}/{}/{}", parts[0], parts[1], target.image));
                    }
                }
            }
        };
        let rc = Rc::new(recompute);
        let rc2 = rc.clone();
        dx_switch_for_timer.connect_active_notify(move |_| rc2());
        let rc3 = rc.clone();
        nvidia_switch_for_timer.connect_active_notify(move |_| rc3());

        gtk::glib::ControlFlow::Break
    });

    let source_nav_page = adw::NavigationPage::builder()
        .title("Image Source")
        .tag("source")
        .child(&source_page)
        .build();

    let registry_row_sub = gtk::Label::new(Some(&format!(
        "{}:{}",
        initial_registry_uri, initial_selected_tag
    )));
    registry_row_sub.add_css_class("dim-label");

    ImageSourceWidgets {
        source_nav_page,
        registry_entry_row,
        registry_row_sub,
        tag_row,
        tag_model,
        tag_raws,
        tag_row_handler,
    }
}
