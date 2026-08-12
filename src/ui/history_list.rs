//! Collapsible deployment history panel — the row widget builder and its
//! data model.
//!
//! Extracted from `status_view.rs` (finupdate#42): `MockDeployment`,
//! `get_sample_deployments`, and `rebuild_history_list` build the history
//! list with pin / rollback / set-default interactions. They only need the
//! component's message enum and sender, so the widget-building code now
//! lives outside the view file.

use gtk::prelude::*;
use relm4::prelude::*;

use super::bootc_probe::get_real_deployments;
use super::status_view::{StatusView, StatusViewInput};

/// Mock deployment representation for the collapsible version history list.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct MockDeployment {
    pub id: String,
    pub state: String, // "current" | "staged" | "previous" | "archived"
    pub title: String,
    pub image: String,
    pub tag: String,
    pub digest: String,
    pub deployed: String,
    pub deployed_full: String,
    pub size: String,
    pub kernel: String,
    pub package_count: u32,
    pub signer: String,
    pub pinned: bool,
}

pub fn get_sample_deployments(_reboot_pending: bool) -> Vec<MockDeployment> {
    // Always try real data first; return empty if unavailable rather than
    // hardcoding Fedora-specific mock data that doesn't apply to other images.
    if let Some(ds) = get_real_deployments() {
        return ds;
    }
    Vec::new()
}

pub fn rebuild_history_list(
    list_box: &gtk::ListBox,
    deployments: &[MockDeployment],
    expanded_id: Option<&str>,
    sender: &ComponentSender<StatusView>,
) {
    while let Some(child) = list_box.first_child() {
        list_box.remove(&child);
    }

    for d in deployments {
        let row_container = gtk::Box::new(gtk::Orientation::Vertical, 0);

        let row_header = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row_header.set_margin_start(16);
        row_header.set_margin_end(16);
        row_header.set_margin_top(12);
        row_header.set_margin_bottom(12);

        let indicator = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let indicator_class = match d.state.as_str() {
            "current" => "deploy-indicator-current",
            "staged" => "deploy-indicator-staged",
            "remote" => "deploy-indicator-staged", // available to pull
            _ => "deploy-indicator-archive",
        };
        indicator.add_css_class(indicator_class);
        row_header.append(&indicator);

        let text_box = gtk::Box::new(gtk::Orientation::Vertical, 2);
        text_box.set_hexpand(true);

        let title_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let name_label = gtk::Label::builder()
            .label(&d.title)
            .halign(gtk::Align::Start)
            .build();
        name_label.add_css_class("heading");
        title_box.append(&name_label);

        if d.state == "current" {
            let pill = gtk::Label::new(Some("Booted"));
            pill.add_css_class("success");
            pill.add_css_class("caption");
            title_box.append(&pill);
        } else if d.state == "staged" {
            let pill = gtk::Label::new(Some("Staged"));
            pill.add_css_class("accent");
            pill.add_css_class("caption");
            title_box.append(&pill);
        } else if d.state == "remote" {
            let pill = gtk::Label::new(Some("Remote"));
            pill.add_css_class("accent");
            pill.add_css_class("caption");
            title_box.append(&pill);
        }
        if d.pinned {
            let pill = gtk::Label::new(Some("Pinned"));
            pill.add_css_class("warning");
            pill.add_css_class("caption");
            title_box.append(&pill);
        }
        text_box.append(&title_box);

        let digest_short = if d.digest.len() >= 12 {
            &d.digest[0..12]
        } else {
            &d.digest
        };
        let submeta_label = gtk::Label::builder()
            .label(&format!(
                "{}:{}  ·  {}…  ·  {}",
                d.image, d.tag, digest_short, d.deployed
            ))
            .halign(gtk::Align::Start)
            .build();
        submeta_label.add_css_class("caption");
        submeta_label.add_css_class("dim-label");
        text_box.append(&submeta_label);
        row_header.append(&text_box);

        let actions_box = gtk::Box::new(gtk::Orientation::Horizontal, 4);

        let pin_btn = gtk::Button::builder()
            .icon_name("pin-symbolic")
            .tooltip_text(if d.pinned { "Unpin" } else { "Pin" })
            .build();
        pin_btn.add_css_class("flat");
        if d.pinned {
            pin_btn.add_css_class("warning");
        }
        let pin_sender = sender.input_sender().clone();
        let pin_id = d.id.clone();
        pin_btn.connect_clicked(move |_| {
            pin_sender.emit(StatusViewInput::TogglePin(pin_id.clone()));
        });
        if d.state != "remote" {
            actions_box.append(&pin_btn);
        }

        if d.state == "remote" {
            let pull_btn = gtk::Button::builder()
                .icon_name("document-save-symbolic")
                .tooltip_text("Pull this image from registry")
                .build();
            pull_btn.add_css_class("flat");
            let pull_d = d.clone();
            pull_btn.connect_clicked(move |_| {
                println!("[debug] Pull requested for {}:{}", pull_d.image, pull_d.tag);
            });
            actions_box.append(&pull_btn);
        } else if d.state != "current" && d.state != "staged" {
            let rb_btn = gtk::Button::builder()
                .icon_name("edit-undo-symbolic")
                .tooltip_text("Roll back to this image")
                .build();
            rb_btn.add_css_class("flat");
            let rb_sender = sender.input_sender().clone();
            let rb_d = d.clone();
            rb_btn.connect_clicked(move |_| {
                rb_sender.emit(StatusViewInput::RollbackTo(rb_d.clone()));
            });
            actions_box.append(&rb_btn);
        }

        let is_expanded = expanded_id == Some(&d.id);
        let chevron_icon = if is_expanded {
            "go-up-symbolic"
        } else {
            "go-down-symbolic"
        };
        let chev_btn = gtk::Button::builder()
            .icon_name(chevron_icon)
            // Icon-only, so it needs both a tooltip and an accessible name —
            // otherwise a screen reader announces an unlabelled button.
            .tooltip_text(if is_expanded {
                "Hide details"
            } else {
                "Show details"
            })
            .build();
        chev_btn.update_property(&[gtk::accessible::Property::Label(if is_expanded {
            "Hide details"
        } else {
            "Show details"
        })]);
        chev_btn.add_css_class("flat");

        let toggle_sender = sender.input_sender().clone();
        let toggle_id = d.id.clone();
        let text_click_sender = sender.input_sender().clone();
        let text_click_id = d.id.clone();

        let gesture = gtk::GestureClick::new();
        gesture.connect_pressed(move |_, _, _, _| {
            text_click_sender.emit(StatusViewInput::TogglePin(format!(
                "expand:{}",
                text_click_id
            )));
        });
        text_box.add_controller(gesture);

        chev_btn.connect_clicked(move |_| {
            toggle_sender.emit(StatusViewInput::TogglePin(format!("expand:{}", toggle_id)));
        });
        actions_box.append(&chev_btn);

        row_header.append(&actions_box);
        row_container.append(&row_header);

        let revealer = gtk::Revealer::new();
        revealer.set_transition_type(gtk::RevealerTransitionType::SlideDown);
        revealer.set_transition_duration(200);
        revealer.set_reveal_child(is_expanded);

        let detail_box = gtk::Box::new(gtk::Orientation::Vertical, 10);
        detail_box.set_margin_start(56);
        detail_box.set_margin_end(24);
        detail_box.set_margin_top(8);
        detail_box.set_margin_bottom(16);

        let grid = gtk::Grid::builder()
            .row_spacing(6)
            .column_spacing(16)
            .build();

        let fields = [
            ("Image", d.image.as_str()),
            ("Tag", d.tag.as_str()),
            ("Digest", d.digest.as_str()),
            ("Deployed", d.deployed_full.as_str()),
            ("Kernel", d.kernel.as_str()),
        ];

        for (row_idx, &(label, val)) in fields.iter().enumerate() {
            let lbl = gtk::Label::builder()
                .label(label)
                .halign(gtk::Align::Start)
                .build();
            lbl.add_css_class("caption");
            lbl.add_css_class("dim-label");

            let val_lbl = gtk::Label::builder()
                .label(val)
                .halign(gtk::Align::Start)
                .build();
            val_lbl.add_css_class("caption");
            val_lbl.add_css_class("monospace");

            grid.attach(&lbl, 0, row_idx as i32, 1, 1);
            grid.attach(&val_lbl, 1, row_idx as i32, 1, 1);
        }

        let pkg_lbl = gtk::Label::builder()
            .label("Packages")
            .halign(gtk::Align::Start)
            .build();
        pkg_lbl.add_css_class("caption");
        pkg_lbl.add_css_class("dim-label");

        let pkg_val = gtk::Label::builder()
            .label(format!("{} installed", d.package_count))
            .halign(gtk::Align::Start)
            .build();
        pkg_val.add_css_class("caption");
        pkg_val.add_css_class("monospace");
        grid.attach(&pkg_lbl, 0, fields.len() as i32, 1, 1);
        grid.attach(&pkg_val, 1, fields.len() as i32, 1, 1);

        let sig_lbl = gtk::Label::builder()
            .label("Signature")
            .halign(gtk::Align::Start)
            .build();
        sig_lbl.add_css_class("caption");
        sig_lbl.add_css_class("dim-label");

        let sig_val = gtk::Label::builder()
            .label(format!("✓ Verified  ·  {}", d.signer))
            .halign(gtk::Align::Start)
            .build();
        sig_val.add_css_class("caption");
        sig_val.add_css_class("success");
        grid.attach(&sig_lbl, 0, (fields.len() + 1) as i32, 1, 1);
        grid.attach(&sig_val, 1, (fields.len() + 1) as i32, 1, 1);

        detail_box.append(&grid);

        let bottom_actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);

        if d.state == "remote" {
            let pull_btn = gtk::Button::builder()
                .label("Pull this image")
                .icon_name("document-save-symbolic")
                .build();
            pull_btn.add_css_class("suggested-action");
            let pull_d = d.clone();
            pull_btn.connect_clicked(move |_| {
                println!("[debug] Pull requested for {}:{}", pull_d.image, pull_d.tag);
            });
            bottom_actions.append(&pull_btn);
        } else if d.state != "current" && d.state != "staged" {
            let rb_btn = gtk::Button::builder()
                .label("Roll back to this")
                .icon_name("edit-undo-symbolic")
                .build();
            rb_btn.add_css_class("suggested-action");
            let rb_sender = sender.input_sender().clone();
            let rb_d = d.clone();
            rb_btn.connect_clicked(move |_| {
                rb_sender.emit(StatusViewInput::RollbackTo(rb_d.clone()));
            });
            bottom_actions.append(&rb_btn);
        }

        if d.state != "current" && d.state != "remote" {
            let def_btn = gtk::Button::builder().label("Set as default boot").build();
            let def_sender = sender.input_sender().clone();
            let def_d = d.clone();
            def_btn.connect_clicked(move |_| {
                def_sender.emit(StatusViewInput::SetDefaultBoot(def_d.clone()));
            });
            bottom_actions.append(&def_btn);
        }

        let ch_btn = gtk::Button::builder().label("View changelog").build();
        ch_btn.add_css_class("flat");
        let ch_sender = sender.input_sender().clone();
        let ch_tag = d.tag.clone();
        ch_btn.connect_clicked(move |_| {
            ch_sender.emit(StatusViewInput::SelectChangelogVersion(ch_tag.clone()));
        });
        bottom_actions.append(&ch_btn);

        detail_box.append(&bottom_actions);
        revealer.set_child(Some(&detail_box));
        row_container.append(&revealer);

        let sep = gtk::Separator::new(gtk::Orientation::Horizontal);
        row_container.append(&sep);

        list_box.append(&row_container);
    }
}
