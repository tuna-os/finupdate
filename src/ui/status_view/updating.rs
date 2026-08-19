//! Updating state page builder for StatusView.

use adw::prelude::*;
use relm4::prelude::*;

use crate::ui::bootc_probe::read_image_info;
use crate::ui::log_view::LogView;
use crate::ui::segmented_progress::SegmentedProgress;
use crate::ui::update_list::UpdateList;

use super::{StatusView, StatusViewInput, StatusViewOutput};

pub(super) struct UpdatingWidgets {
    pub(super) seg_progress: SegmentedProgress,
    pub(super) elapsed_label: gtk::Label,
    pub(super) updating_content: gtk::Box,
}

pub(super) fn build_updating_page(
    sender: &ComponentSender<StatusView>,
    log_view: &Controller<LogView>,
    update_list: &Controller<UpdateList>,
) -> UpdatingWidgets {
    let seg_progress = SegmentedProgress::new();

    let elapsed_label = gtk::Label::new(Some("0:00"));
    elapsed_label.add_css_class("dim-label");
    elapsed_label.add_css_class("caption");
    elapsed_label.add_css_class("monospace");

    let updating_image_label = gtk::Label::new(read_image_info().as_deref());
    updating_image_label.add_css_class("caption");
    updating_image_label.add_css_class("dim-label");
    updating_image_label.add_css_class("monospace");
    updating_image_label.set_margin_top(8);
    updating_image_label.set_margin_bottom(4);
    updating_image_label.set_visible(read_image_info().is_some());

    let log_clamp = adw::Clamp::new();
    log_clamp.set_maximum_size(800);
    log_clamp.set_vexpand(true);
    log_clamp.set_child(Some(log_view.widget()));

    let copy_btn = gtk::Button::from_icon_name("edit-copy-symbolic");
    copy_btn.set_tooltip_text(Some("Copy log output to clipboard"));
    copy_btn.add_css_class("flat");
    copy_btn.add_css_class("circular");
    let copy_sender = sender.input_sender().clone();
    copy_btn.connect_clicked(move |_| {
        copy_sender.emit(StatusViewInput::CopyLog);
    });

    let cancel_btn = gtk::Button::builder()
        .label("Cancel")
        .tooltip_text("Cancel the running update")
        .build();
    cancel_btn.add_css_class("destructive-action");
    let cancel_sender = sender.output_sender().clone();
    cancel_btn.connect_clicked(move |_| {
        let _ = cancel_sender.send(StatusViewOutput::CancelUpdate);
    });

    let bottom_bar = gtk::Box::new(gtk::Orientation::Horizontal, 24);
    bottom_bar.set_halign(gtk::Align::Center);
    bottom_bar.set_margin_top(12);
    bottom_bar.set_margin_bottom(12);
    bottom_bar.append(&elapsed_label);
    bottom_bar.append(&copy_btn);
    bottom_bar.append(&cancel_btn);

    let updating_content = gtk::Box::new(gtk::Orientation::Vertical, 0);

    let header_clamp = adw::Clamp::new();
    header_clamp.set_maximum_size(800);
    let header_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    header_box.append(&seg_progress.widget());
    header_box.append(&updating_image_label);
    header_box.append(update_list.widget());
    header_clamp.set_child(Some(&header_box));

    updating_content.append(&header_clamp);
    updating_content.append(&log_clamp);
    updating_content.append(&bottom_bar);

    UpdatingWidgets {
        seg_progress,
        elapsed_label,
        updating_content,
    }
}
