//! Calendar grid, month navigation, and version details panel for the rebase dialog.

use adw::prelude::*;
use chrono::{Datelike, Local, NaiveDate};
use gtk::glib;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::registry_client::{ImageVersion, strip_date_suffix};
use crate::service::{self, FamilyInfo};
use crate::ui::rebase_target::{compute_stream_switch_action, days_in_month};

use super::OnShowChangelog;
use super::execution::{run_rebase, with_access_key};
use super::switches::resolve_target_ref;

pub(super) fn inject_calendar_css() {
    let css = gtk::CssProvider::new();
    css.load_from_string(
        r#"
        .day-btn {
            min-width: 30px;
            min-height: 30px;
            padding: 0;
            border-radius: 15px;
            font-size: 0.82em;
        }
        .day-btn:not(:sensitive) { opacity: 0.3; }
        .day-available           { color: @accent_color; font-weight: bold; }
        .day-current             { background-color: @accent_bg_color; color: @accent_fg_color; }
        .day-selected:not(.day-current) {
            outline: 2px solid @accent_color;
            outline-offset: -2px;
        }
        .day-today label { text-decoration: underline; }
        "#,
    );
    gtk::style_context_add_provider_for_display(
        &gtk::gdk::Display::default().expect("display"),
        &css,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

pub(super) fn update_details(
    group: &adw::PreferencesGroup,
    version_row: &adw::ActionRow,
    kernel_row: &adw::ActionRow,
    built_row: &adw::ActionRow,
    commit_row: &adw::ActionRow,
    rebase_btn: &gtk::Button,
    v: &ImageVersion,
    date: &NaiveDate,
    current_date: Option<NaiveDate>,
) {
    version_row.set_subtitle(&v.version);
    kernel_row.set_subtitle(&v.kernel);
    built_row.set_subtitle(&v.created.format("%b %-d, %Y · %H:%M UTC").to_string());
    commit_row.set_subtitle(if v.revision.is_empty() {
        "—"
    } else {
        &v.revision
    });

    group.set_visible(true);

    let is_current = current_date == Some(*date);
    if is_current {
        rebase_btn.set_label(&with_access_key("Currently Installed"));
        rebase_btn.set_sensitive(false);
    } else {
        // YYYYMMDD format — matches the registry's actual tag scheme and
        // is what the user types when they reference a build.
        rebase_btn.set_label(&with_access_key(&format!(
            "Pin to {}",
            date.format("%Y%m%d")
        )));
        rebase_btn.set_sensitive(true);
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn redraw_grid(
    grid: &gtk::Grid,
    displayed: NaiveDate,
    versions: &HashMap<NaiveDate, ImageVersion>,
    current_date: Option<NaiveDate>,
    selected: &Rc<RefCell<Option<NaiveDate>>>,
    details_group: &adw::PreferencesGroup,
    version_row: &adw::ActionRow,
    kernel_row: &adw::ActionRow,
    built_row: &adw::ActionRow,
    commit_row: &adw::ActionRow,
    rebase_btn: &gtk::Button,
    month_label: &gtk::Label,
    next_btn: &gtk::Button,
    empty_hint: &gtk::Label,
    on_deselect: Option<Rc<dyn Fn()>>,
) {
    let today = Local::now().date_naive();

    month_label.set_label(&displayed.format("%B %Y").to_string());

    let current_month = NaiveDate::from_ymd_opt(today.year(), today.month(), 1).unwrap_or(today);
    next_btn.set_sensitive(displayed < current_month);

    let days_in_month = days_in_month(displayed);
    let first_weekday = displayed.weekday().num_days_from_monday() as i32;
    let selected_date = *selected.borrow();

    let mut available_count = 0u32;
    let mut slot = 0i32;
    for row in 0..6i32 {
        for col in 0..7i32 {
            let btn = grid
                .child_at(col, row)
                .and_then(|w| w.downcast::<gtk::Button>().ok());
            let Some(btn) = btn else {
                slot += 1;
                continue;
            };

            let day_num = slot - first_weekday + 1;

            if day_num < 1 || day_num > days_in_month as i32 {
                btn.set_label("");
                btn.set_visible(false);
                btn.set_sensitive(false);
            } else {
                btn.set_visible(true);
                btn.set_label(&day_num.to_string());

                let date =
                    NaiveDate::from_ymd_opt(displayed.year(), displayed.month(), day_num as u32);

                for cls in ["day-available", "day-current", "day-selected", "day-today"] {
                    btn.remove_css_class(cls);
                }

                if let Some(d) = date {
                    let is_available = versions.contains_key(&d);
                    let is_current = current_date == Some(d);
                    let is_selected = selected_date == Some(d);
                    let is_today = d == today;
                    let is_future = d > today;

                    btn.set_sensitive(is_available && !is_future);

                    if is_today {
                        btn.add_css_class("day-today");
                    }
                    if is_available {
                        btn.add_css_class("day-available");
                        available_count += 1;
                    }
                    if is_current {
                        btn.add_css_class("day-current");
                    }
                    if is_selected {
                        btn.add_css_class("day-selected");
                    }

                    if is_available && !is_future {
                        if let Some(hid) =
                            unsafe { btn.steal_data::<glib::SignalHandlerId>("day-handler") }
                        {
                            btn.disconnect(hid);
                        }

                        let selected_rc = selected.clone();
                        let versions_rc = versions.clone();
                        let details_rc = details_group.clone();
                        let v_row = version_row.clone();
                        let k_row = kernel_row.clone();
                        let b_row = built_row.clone();
                        let c_row = commit_row.clone();
                        let reb_btn = rebase_btn.clone();
                        let grid_rc = grid.clone();
                        let displayed_date = displayed;
                        let cur_date = current_date;
                        let m_lbl = month_label.clone();
                        let n_btn = next_btn.clone();
                        let e_hint = empty_hint.clone();
                        let deselect_rc = on_deselect.clone();

                        let hid = btn.connect_clicked(move |_| {
                            let prev = *selected_rc.borrow();
                            if prev == Some(d) {
                                *selected_rc.borrow_mut() = None;
                                details_rc.set_visible(false);
                                if let Some(ref deselect) = deselect_rc {
                                    deselect();
                                }
                            } else {
                                *selected_rc.borrow_mut() = Some(d);
                                if let Some(v) = versions_rc.get(&d) {
                                    update_details(
                                        &details_rc,
                                        &v_row,
                                        &k_row,
                                        &b_row,
                                        &c_row,
                                        &reb_btn,
                                        v,
                                        &d,
                                        cur_date,
                                    );
                                }
                            }
                            redraw_grid(
                                &grid_rc,
                                displayed_date,
                                &versions_rc,
                                cur_date,
                                &selected_rc,
                                &details_rc,
                                &v_row,
                                &k_row,
                                &b_row,
                                &c_row,
                                &reb_btn,
                                &m_lbl,
                                &n_btn,
                                &e_hint,
                                deselect_rc.clone(),
                            );
                        });

                        unsafe {
                            btn.set_data("day-handler", hid);
                        }
                    }
                }
            }
            slot += 1;
        }
    }

    empty_hint.set_visible(available_count == 0);
}

#[allow(clippy::too_many_arguments)]
pub(super) fn build_loaded_page(
    container: &gtk::Box,
    stack: &gtk::Stack,
    dialog: &adw::Dialog,
    parent: &gtk::Widget,
    versions: Vec<ImageVersion>,
    current_family: Rc<RefCell<Option<FamilyInfo>>>,
    selected_features: Rc<RefCell<Vec<String>>>,
    selected_stream: Rc<RefCell<String>>,
    booted_image: Rc<RefCell<Option<service::ImageRef>>>,
    reload_fn: Option<Rc<dyn Fn()>>,
    on_show_changelog: OnShowChangelog,
    is_loading: bool,
) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }

    let version_map: HashMap<NaiveDate, ImageVersion> =
        versions.iter().map(|v| (v.date, v.clone())).collect();
    let version_map = Rc::new(version_map);

    let current_date: Option<NaiveDate> = versions.last().map(|v| v.date);

    let selected: Rc<RefCell<Option<NaiveDate>>> = Rc::new(RefCell::new(None));

    let details_group = adw::PreferencesGroup::builder()
        .title("Selected Version")
        .margin_start(16)
        .margin_end(16)
        .margin_top(8)
        .margin_bottom(8)
        .build();
    details_group.set_visible(false);

    let version_row = adw::ActionRow::builder().title("Version").build();
    let kernel_row = adw::ActionRow::builder().title("Kernel").build();
    let built_row = adw::ActionRow::builder().title("Built").build();
    let commit_row = adw::ActionRow::builder().title("Commit").build();

    details_group.add(&version_row);
    details_group.add(&kernel_row);
    details_group.add(&built_row);
    details_group.add(&commit_row);

    let see_changelog_btn = gtk::Button::builder()
        .label("See changelog")
        .sensitive(false)
        .margin_start(16)
        .margin_end(16)
        .margin_top(8)
        .build();
    see_changelog_btn.add_css_class("flat");

    let pending_stream_ref: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let (initial_label, initial_sensitive, initial_ref) = compute_stream_switch_action(
        current_family.borrow().as_ref(),
        &selected_features.borrow(),
        &selected_stream.borrow(),
        booted_image.borrow().as_ref(),
    );
    *pending_stream_ref.borrow_mut() = initial_ref;
    let rebase_btn = gtk::Button::builder()
        .label(&with_access_key(&initial_label))
        .use_underline(true)
        .sensitive(initial_sensitive)
        .margin_start(16)
        .margin_end(16)
        .margin_top(4)
        .margin_bottom(16)
        .build();
    rebase_btn.add_css_class("suggested-action");

    let calendar_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    calendar_box.set_margin_start(8);
    calendar_box.set_margin_end(8);
    calendar_box.set_margin_top(16);
    calendar_box.set_margin_bottom(8);

    let today = Local::now().date_naive();
    let initial_month = versions
        .last()
        .map(|v| NaiveDate::from_ymd_opt(v.date.year(), v.date.month(), 1).unwrap_or(v.date))
        .unwrap_or_else(|| {
            NaiveDate::from_ymd_opt(today.year(), today.month(), 1).unwrap_or(today)
        });
    let displayed_month: Rc<RefCell<NaiveDate>> = Rc::new(RefCell::new(initial_month));

    let prev_btn = gtk::Button::builder()
        .icon_name("go-previous-symbolic")
        .tooltip_text("Previous month")
        .build();
    prev_btn.add_css_class("flat");
    prev_btn.add_css_class("circular");

    let next_btn = gtk::Button::builder()
        .icon_name("go-next-symbolic")
        .tooltip_text("Next month")
        .build();
    next_btn.add_css_class("flat");
    next_btn.add_css_class("circular");
    next_btn.set_sensitive(false);

    let month_label = gtk::Label::builder()
        .hexpand(true)
        .halign(gtk::Align::Center)
        .build();
    month_label.add_css_class("title-4");

    let nav_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(4)
        .margin_bottom(12)
        .build();
    nav_row.append(&prev_btn);
    nav_row.append(&month_label);
    nav_row.append(&next_btn);
    calendar_box.append(&nav_row);

    let header_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .homogeneous(true)
        .margin_bottom(4)
        .build();
    for day in ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"] {
        let lbl = gtk::Label::new(Some(day));
        lbl.add_css_class("caption");
        lbl.add_css_class("dim-label");
        lbl.set_hexpand(true);
        lbl.set_halign(gtk::Align::Center);
        header_row.append(&lbl);
    }
    calendar_box.append(&header_row);

    let grid = gtk::Grid::builder()
        .row_spacing(2)
        .column_spacing(2)
        .row_homogeneous(true)
        .column_homogeneous(true)
        .build();
    for row in 0..6i32 {
        for col in 0..7i32 {
            let btn = gtk::Button::new();
            btn.add_css_class("flat");
            btn.add_css_class("day-btn");
            grid.attach(&btn, col, row, 1, 1);
        }
    }
    calendar_box.append(&grid);

    let empty_hint = gtk::Label::builder()
        .label(if is_loading {
            "Loading builds…"
        } else {
            "No builds in this month"
        })
        .halign(gtk::Align::Center)
        .margin_top(8)
        .build();
    empty_hint.add_css_class("dim-label");
    empty_hint.add_css_class("caption");
    empty_hint.set_visible(false);
    calendar_box.append(&empty_hint);

    if is_loading {
        let loading_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        loading_row.set_halign(gtk::Align::Center);
        loading_row.set_margin_top(8);
        let spinner = gtk::Spinner::new();
        spinner.set_spinning(true);
        spinner.set_size_request(14, 14);
        let lbl = gtk::Label::new(Some("Loading builds…"));
        lbl.add_css_class("dim-label");
        lbl.add_css_class("caption");
        loading_row.append(&spinner);
        loading_row.append(&lbl);
        calendar_box.append(&loading_row);
    }

    if let Some(reload) = reload_fn.as_ref() {
        let load_older_btn = gtk::Button::builder()
            .label("Load older builds")
            .halign(gtk::Align::Center)
            .margin_top(8)
            .margin_bottom(4)
            .build();
        load_older_btn.add_css_class("flat");
        let reload = reload.clone();
        let btn = load_older_btn.clone();
        load_older_btn.connect_clicked(move |_| {
            btn.set_label("Loading…");
            btn.set_sensitive(false);
            reload();
        });
        calendar_box.append(&load_older_btn);
    }

    inject_calendar_css();

    details_group
        .bind_property("visible", &see_changelog_btn, "sensitive")
        .sync_create()
        .build();

    {
        let selected_rc = selected.clone();
        let version_map_rc = version_map.clone();
        let dialog_rc = dialog.clone();
        let cb = on_show_changelog.clone();
        see_changelog_btn.connect_clicked(move |_| {
            let Some(date) = *selected_rc.borrow() else {
                return;
            };
            let Some(v) = version_map_rc.get(&date).cloned() else {
                return;
            };
            dialog_rc.close();
            cb(v.version);
        });
    }

    container.append(&calendar_box);
    container.append(&details_group);
    container.append(&see_changelog_btn);
    container.append(&rebase_btn);

    let version_map_rc = version_map.clone();
    let selected_rc = selected.clone();
    let details_group_rc = details_group.clone();
    let version_row_rc = version_row.clone();
    let kernel_row_rc = kernel_row.clone();
    let built_row_rc = built_row.clone();
    let commit_row_rc = commit_row.clone();
    let rebase_btn_rc = rebase_btn.clone();
    let month_label_rc = month_label.clone();
    let next_btn_rc = next_btn.clone();
    let empty_hint_rc = empty_hint.clone();

    let on_deselect: Rc<dyn Fn()> = {
        let rebase_btn = rebase_btn.clone();
        let current_family = current_family.clone();
        let selected_features = selected_features.clone();
        let selected_stream = selected_stream.clone();
        let booted_image = booted_image.clone();
        let pending_stream_ref = pending_stream_ref.clone();
        Rc::new(move || {
            let (label, sensitive, full_ref) = compute_stream_switch_action(
                current_family.borrow().as_ref(),
                &selected_features.borrow(),
                &selected_stream.borrow(),
                booted_image.borrow().as_ref(),
            );
            *pending_stream_ref.borrow_mut() = full_ref;
            rebase_btn.set_label(&with_access_key(&label));
            rebase_btn.set_sensitive(sensitive);
        })
    };

    let on_deselect_for_redraw = on_deselect.clone();
    let redraw = Rc::new(move |grid: &gtk::Grid, displayed: NaiveDate| {
        redraw_grid(
            grid,
            displayed,
            &version_map_rc,
            current_date,
            &selected_rc,
            &details_group_rc,
            &version_row_rc,
            &kernel_row_rc,
            &built_row_rc,
            &commit_row_rc,
            &rebase_btn_rc,
            &month_label_rc,
            &next_btn_rc,
            &empty_hint_rc,
            Some(on_deselect_for_redraw.clone()),
        );
    });

    redraw(&grid, *displayed_month.borrow());

    {
        let grid = grid.clone();
        let displayed_month = displayed_month.clone();
        let redraw = redraw.clone();
        prev_btn.connect_clicked(move |_| {
            let current = *displayed_month.borrow();
            let prev = if current.month() == 1 {
                NaiveDate::from_ymd_opt(current.year() - 1, 12, 1).unwrap_or(current)
            } else {
                NaiveDate::from_ymd_opt(current.year(), current.month() - 1, 1).unwrap_or(current)
            };
            *displayed_month.borrow_mut() = prev;
            redraw(&grid, prev);
        });
    }

    {
        let grid = grid.clone();
        let displayed_month = displayed_month.clone();
        let redraw = redraw.clone();
        next_btn.connect_clicked(move |_| {
            let current = *displayed_month.borrow();
            let next = if current.month() == 12 {
                NaiveDate::from_ymd_opt(current.year() + 1, 1, 1).unwrap_or(current)
            } else {
                NaiveDate::from_ymd_opt(current.year(), current.month() + 1, 1).unwrap_or(current)
            };
            *displayed_month.borrow_mut() = next;
            redraw(&grid, next);
        });
    }

    {
        let selected_rc = selected.clone();
        let version_map_rc = version_map.clone();
        let dialog_rc = dialog.clone();
        let parent_rc = parent.clone();
        let stack_rc = stack.clone();
        let current_family_rc = current_family.clone();
        let selected_features_rc = selected_features.clone();
        let pending_stream_ref_rc = pending_stream_ref.clone();
        let selected_stream_rc = selected_stream.clone();

        rebase_btn.connect_clicked(move |_| {
            let Some(date) = *selected_rc.borrow() else {
                let Some(full_ref) = pending_stream_ref_rc.borrow().clone() else {
                    return;
                };
                let stream = selected_stream_rc.borrow().clone();
                let confirm = adw::AlertDialog::builder()
                    .heading(format!("Switch to :{}?", stream))
                    .body(format!(
                        "Your system will follow the floating `{}` tag and resume receiving automatic updates from it:\n\n{}\n\nA restart is required and the full image will be re-downloaded.",
                        stream, full_ref,
                    ))
                    .build();
                confirm.add_response("cancel", "_Cancel");
                confirm.add_response("switch", "_Switch");
                confirm.set_response_appearance("switch", adw::ResponseAppearance::Suggested);
                confirm.set_default_response(Some("cancel"));
                confirm.set_close_response("cancel");

                let stack = stack_rc.clone();
                let dialog_close = dialog_rc.clone();
                let full_ref_for_run = full_ref.clone();
                confirm.connect_response(None, move |_, response| {
                    if response == "switch" {
                        run_rebase(
                            full_ref_for_run.clone(),
                            stack.clone(),
                            dialog_close.clone(),
                        );
                    }
                });
                confirm.present(Some(&parent_rc));
                return;
            };
            let Some(version) = version_map_rc.get(&date).cloned() else {
                return;
            };

            let family_ref = current_family_rc.borrow();
            let target_full_ref = resolve_target_ref(
                &version.full_ref,
                family_ref.as_ref(),
                &selected_features_rc.borrow(),
            );
            drop(family_ref);
            let switching_image = target_full_ref != version.full_ref;

            let body = if switching_image {
                format!(
                    "Your system will be pinned to:\n\n{}\n\nThis is a different image variant than what you're currently running. A restart is required and the full image will be re-downloaded. Automatic updates pause until you unpin.",
                    target_full_ref,
                )
            } else {
                let display_version = strip_date_suffix(&version.version)
                    .unwrap_or_else(|| version.version.clone());
                format!(
                    "Your system will be pinned to the {} build (version {}).\n\nA restart is required and the full image will be re-downloaded. Automatic updates pause until you unpin.",
                    date.format("%B %-d, %Y"),
                    display_version,
                )
            };

            let confirm = adw::AlertDialog::builder()
                .heading("Pin to this build?")
                .body(body)
                .build();

            confirm.add_response("cancel", "_Cancel");
            confirm.add_response("rebase", "_Pin");
            confirm.set_response_appearance("rebase", adw::ResponseAppearance::Suggested);
            confirm.set_default_response(Some("cancel"));
            confirm.set_close_response("cancel");

            let full_ref = target_full_ref;
            let stack = stack_rc.clone();
            let dialog_close = dialog_rc.clone();

            confirm.connect_response(None, move |_, response| {
                if response == "rebase" {
                    run_rebase(full_ref.clone(), stack.clone(), dialog_close.clone());
                }
            });

            confirm.present(Some(&parent_rc));
        });
    }
}
