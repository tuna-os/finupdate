//! Background version fetch pipeline for the rebase dialog.

use adw::prelude::*;
use gtk::glib;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use crate::registry_client::ImageVersion;
use crate::service::{self, FamilyInfo};

use super::OnShowChangelog;
use super::calendar::build_loaded_page;

/// Initial number of versions fetched when the rebase dialog opens.
/// Powers the dropdown of recent builds (the user only sees the top 4 in the
/// dropdown, but we pre-fetch a slightly larger window so the calendar has
/// content the instant "Show older builds" is clicked). The "Load older
/// builds" affordance inside the calendar re-fetches with EXPANDED_FETCH_COUNT.
pub(super) const INITIAL_FETCH_COUNT: usize = 12;
pub(super) const EXPANDED_FETCH_COUNT: usize = 120;

pub(super) enum FetchResult {
    Ok(Vec<ImageVersion>),
    #[allow(dead_code)]
    Err(String),
    DetectFailed,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn start_version_fetch(
    stack: gtk::Stack,
    loaded_box: gtk::Box,
    dialog: adw::Dialog,
    parent: gtk::Widget,
    error_page: adw::StatusPage,
    variant: &str,
    current_family: Rc<RefCell<Option<FamilyInfo>>>,
    selected_features: Rc<RefCell<Vec<String>>>,
    selected_stream: Rc<RefCell<String>>,
    booted_image: Rc<RefCell<Option<service::ImageRef>>>,
    max_versions: usize,
    on_show_changelog: OnShowChangelog,
    target_override: Option<service::ImageRef>,
) {
    // No "loading" stack page anymore — the empty calendar built on dialog
    // open carries its own "Loading builds…" indicator. Just make sure the
    // error page isn't sticky from a previous failed run.
    error_page.set_description(Some("Check your internet connection and try again."));

    build_loaded_page(
        &loaded_box,
        &stack,
        &dialog,
        &parent,
        Vec::new(),
        current_family.clone(),
        selected_features.clone(),
        selected_stream.clone(),
        booted_image.clone(),
        None,
        on_show_changelog.clone(),
        true, // is_loading
    );
    stack.set_visible_child_name("loaded");

    let variant_str = variant.to_string();
    let result_slot: Arc<Mutex<Option<FetchResult>>> = Arc::new(Mutex::new(None));
    spawn_fetch_thread(
        result_slot.clone(),
        &variant_str,
        max_versions,
        target_override.clone(),
    );

    // Build a "reload with EXPANDED_FETCH_COUNT" closure that the loaded
    // page passes to the "Load older builds" button inside the calendar.
    let reload_fn: Rc<dyn Fn()> = {
        let stack = stack.clone();
        let loaded_box = loaded_box.clone();
        let dialog = dialog.clone();
        let parent = parent.clone();
        let error_page = error_page.clone();
        let variant_owned = variant_str.clone();
        let current_family = current_family.clone();
        let selected_features = selected_features.clone();
        let selected_stream = selected_stream.clone();
        let booted_image = booted_image.clone();
        let on_show_changelog = on_show_changelog.clone();
        let target_override_owned = target_override.clone();
        Rc::new(move || {
            start_version_fetch(
                stack.clone(),
                loaded_box.clone(),
                dialog.clone(),
                parent.clone(),
                error_page.clone(),
                &variant_owned,
                current_family.clone(),
                selected_features.clone(),
                selected_stream.clone(),
                booted_image.clone(),
                EXPANDED_FETCH_COUNT,
                on_show_changelog.clone(),
                target_override_owned.clone(),
            );
        })
    };

    let is_expanded = max_versions >= EXPANDED_FETCH_COUNT;

    let start_time = std::time::Instant::now();
    glib::timeout_add_local(std::time::Duration::from_millis(200), move || {
        if let Some(result) = result_slot.lock().ok().and_then(|mut guard| guard.take()) {
            match result {
                FetchResult::Ok(versions) => {
                    build_loaded_page(
                        &loaded_box,
                        &stack,
                        &dialog,
                        &parent,
                        versions,
                        current_family.clone(),
                        selected_features.clone(),
                        selected_stream.clone(),
                        booted_image.clone(),
                        if is_expanded {
                            None
                        } else {
                            Some(reload_fn.clone())
                        },
                        on_show_changelog.clone(),
                        false,
                    );
                    stack.set_visible_child_name("loaded");
                }
                FetchResult::DetectFailed => {
                    error_page.set_description(Some(
                        "Could not detect the current image. Is bootc installed and managing this system?",
                    ));
                    stack.set_visible_child_name("error");
                }
                FetchResult::Err(_) => {
                    error_page
                        .set_description(Some("Check your internet connection and try again."));
                    stack.set_visible_child_name("error");
                }
            }
            return glib::ControlFlow::Break;
        }

        if start_time.elapsed() > std::time::Duration::from_secs(90) {
            error_page.set_description(Some("Check your internet connection and try again."));
            stack.set_visible_child_name("error");
            return glib::ControlFlow::Break;
        }

        glib::ControlFlow::Continue
    });
}

pub(super) fn spawn_fetch_thread(
    result_slot: Arc<Mutex<Option<FetchResult>>>,
    variant: &str,
    max_versions: usize,
    target_override: Option<service::ImageRef>,
) {
    let variant_str = variant.to_string();
    std::thread::spawn(move || {
        crate::runtime::block_on(async move {
            let svc = service::global();
            let image_result = match target_override {
                Some(img) => Ok(img),
                None => svc.current_image().await,
            };
            let result = match image_result {
                Err(_) => FetchResult::DetectFailed,
                Ok(image) => match svc.list_versions(&image, max_versions).await {
                    Ok(mut versions) => {
                        if !variant_str.is_empty() && variant_str != "default" {
                            versions.retain(|v| v.version.contains(&variant_str));
                        }
                        FetchResult::Ok(versions)
                    }
                    Err(e) => FetchResult::Err(e.to_string()),
                },
            };
            *result_slot.lock().unwrap() = Some(result);
        });
    });
}
