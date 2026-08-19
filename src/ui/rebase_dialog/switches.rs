//! Family and feature-switch UI setup and image ref resolution.

use adw::prelude::*;
use gtk::glib;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use crate::service::{self, FamilyInfo};
use crate::ui::rebase_target::{derive_initial_toggle_state, resolve_dx_nvidia_with_stream};

/// Detect the booted (or mocked) image's Family and render one SwitchRow per
/// atomic feature. As switches toggle, recompute the target image and write
/// it into `target_row`'s subtitle. The dialog uses this to show the user
/// the *resolved* image they'd land on without exposing the raw image names.
///
/// Runs the detection on a background thread (the same pattern as
/// [`spawn_fetch_thread`]) so the dialog stays responsive while bootc/os-release
/// IO completes.
pub(super) fn populate_family_switches(
    features_group: &adw::PreferencesGroup,
    family_label: &gtk::Label,
    target_row: &adw::ActionRow,
    stream_row: &adw::ComboRow,
    current_family: Rc<RefCell<Option<FamilyInfo>>>,
    selected_features: Rc<RefCell<Vec<String>>>,
    selected_stream: Rc<RefCell<String>>,
    booted_image: Rc<RefCell<Option<service::ImageRef>>>,
    on_target_change: Rc<dyn Fn(Option<service::ImageRef>)>,
) {
    // Two pieces of state get resolved in the background thread:
    //   - the family the booted image belongs to (drives WHICH toggles show)
    //   - the booted ImageRef itself (drives the toggles' INITIAL state, so
    //     a user already on bluefin-dx-nvidia-open sees both toggles ON
    //     when the dialog opens, not OFF as if rebasing would downgrade)
    let slot: Arc<Mutex<Option<(Option<FamilyInfo>, Option<service::ImageRef>)>>> =
        Arc::new(Mutex::new(None));

    {
        let slot = slot.clone();
        std::thread::spawn(move || {
            let detected = crate::runtime::block_on(async move {
                let svc = service::global();
                let family = svc.current_family().await.ok().flatten();
                let image = svc.current_image().await.ok();
                (family, image)
            });
            *slot.lock().unwrap() = Some(detected);
        });
    }

    let features_group = features_group.clone();
    let family_label = family_label.clone();
    let target_row = target_row.clone();
    let stream_row = stream_row.clone();

    glib::timeout_add_local(std::time::Duration::from_millis(200), move || {
        let Some((family_opt, image_opt)) = slot.lock().ok().and_then(|mut g| g.take()) else {
            return glib::ControlFlow::Continue;
        };

        let Some(family) = family_opt else {
            family_label.set_label("Family not recognized");
            target_row.set_subtitle("(this image isn't in the KNOWN_FAMILIES catalogue)");
            return glib::ControlFlow::Break;
        };

        family_label.set_label(&format!("Family: {}", family.name));

        // Populate stream dropdown with available streams for this family
        let stream_model = gtk::StringList::new(&[]);
        for stream in &family.streams {
            stream_model.append(stream);
        }
        stream_row.set_model(Some(&stream_model));

        // Set default stream to the first one (canonical)
        if !family.streams.is_empty() {
            stream_row.set_selected(0);
            *selected_stream.borrow_mut() = family.streams[0].clone();
        }

        // Stash booted image for build_loaded_page's rebase-button label
        // logic (it needs the booted tag to detect "stream/variant change vs
        // no change"). Cloned because image_opt is consumed by the toggle-
        // state derivation below.
        *booted_image.borrow_mut() = image_opt.clone();

        // Derive the initial toggle state from the booted image's suffix so
        // users already on -dx / -nvidia / -dx-nvidia-open see the dialog
        // open with their current configuration represented, not with
        // everything OFF. Without this the dialog implies that rebasing
        // would *downgrade* to the base image.
        let (initial_dx, initial_nvidia) = derive_initial_toggle_state(&family, image_opt.as_ref());
        target_row.set_subtitle(
            image_opt
                .as_ref()
                .map(|img| format!("{} (currently booted)", img.image))
                .unwrap_or_else(|| format!("{} (no extras)", family.base_image))
                .as_str(),
        );

        // Two opinionated toggles instead of one-per-atomic-feature. Per user
        // direction: "we should have toggle for Developer Mode and Nvidia".
        let supports_dx = family.features.iter().any(|f| f.id == "dx");
        let supports_nvidia = family
            .features
            .iter()
            .any(|f| f.id == "nvidia" || f.id == "open");

        let dx_state = Rc::new(Cell::new(initial_dx));
        let nvidia_state = Rc::new(Cell::new(initial_nvidia));

        let recompute = {
            let family = family.clone();
            let selected_features = selected_features.clone();
            let selected_stream = selected_stream.clone();
            let target_row = target_row.clone();
            let dx_state = dx_state.clone();
            let nvidia_state = nvidia_state.clone();
            let on_target_change = on_target_change.clone();
            move || {
                let stream = selected_stream.borrow().clone();
                let (feats, target) = resolve_dx_nvidia_with_stream(
                    &family,
                    dx_state.get(),
                    nvidia_state.get(),
                    &stream,
                );
                *selected_features.borrow_mut() = feats;
                match &target {
                    Some(t) => target_row.set_subtitle(&format!("{} (resolved)", t.image)),
                    None => {
                        target_row.set_subtitle("(combination doesn't match any published image)")
                    }
                }
                // Refetch the calendar for the resolved target image so the
                // day grid reflects builds for THAT image (the user just
                // toggled DX/NVIDIA/stream — they expect the calendar to
                // follow). Debounced inside on_target_change.
                on_target_change(target);
            }
        };

        if supports_dx {
            let row = adw::SwitchRow::builder()
                .title("Developer Mode")
                .subtitle("Container tools, IDEs, and language SDKs")
                .active(initial_dx)
                .build();
            let recompute_ = recompute.clone();
            let dx_state_ = dx_state.clone();
            row.connect_active_notify(move |sr| {
                dx_state_.set(sr.is_active());
                recompute_();
            });
            features_group.add(&row);
        }

        if supports_nvidia {
            // Adaptive subtitle: if we detect NVIDIA hardware, call that out so
            // the user understands why this toggle matters for them
            // specifically. Otherwise show the generic description.
            let nvidia_subtitle = if crate::gpu::has_nvidia_gpu() {
                "NVIDIA GPU detected — keep on for hardware-accelerated graphics (open kernel modules preferred)"
            } else {
                "Picks the open kernel modules where available, falls back to the proprietary driver"
            };
            let row = adw::SwitchRow::builder()
                .title("NVIDIA drivers")
                .subtitle(nvidia_subtitle)
                .active(initial_nvidia)
                .build();
            // Guard prevents the warn-and-revert path from re-firing this
            // handler when we programmatically flip the switch back to
            // its previous state after a "Cancel" on the warning dialog.
            let nvidia_guard: Rc<Cell<bool>> = Rc::new(Cell::new(false));
            let recompute_ = recompute.clone();
            let nvidia_state_ = nvidia_state.clone();
            let guard_ = nvidia_guard.clone();
            row.connect_active_notify(move |sr| {
                if guard_.get() {
                    return;
                }
                let new_value = sr.is_active();
                let prev_value = nvidia_state_.get();
                let turning_off = prev_value && !new_value;
                if turning_off && crate::gpu::has_nvidia_gpu() {
                    let confirm = adw::AlertDialog::builder()
                        .heading("NVIDIA hardware detected")
                        .body("Your system has an NVIDIA GPU. Switching to a non-NVIDIA image will fall back to software rendering or the open Mesa driver — graphics performance will degrade significantly until you switch back.\n\nContinue?")
                        .build();
                    confirm.add_response("cancel", "_Cancel");
                    confirm.add_response("disable", "_Disable anyway");
                    confirm.set_response_appearance(
                        "disable",
                        adw::ResponseAppearance::Destructive,
                    );
                    confirm.set_default_response(Some("cancel"));
                    confirm.set_close_response("cancel");

                    let sr_clone = sr.clone();
                    let nvidia_state_clone = nvidia_state_.clone();
                    let recompute_clone = recompute_.clone();
                    let guard_clone = guard_.clone();
                    confirm.connect_response(None, move |_, response| {
                        if response == "disable" {
                            nvidia_state_clone.set(false);
                            recompute_clone();
                        } else {
                            // Revert the switch back to on without re-firing
                            // the handler (would re-trigger this dialog).
                            guard_clone.set(true);
                            sr_clone.set_active(true);
                            guard_clone.set(false);
                        }
                    });
                    confirm.present(None::<&gtk::Widget>);
                    // Don't apply yet — wait for the response callback.
                    return;
                }
                nvidia_state_.set(new_value);
                recompute_();
            });
            features_group.add(&row);
        }

        // Wire up stream selection to recompute target image
        let selected_stream_clone = selected_stream.clone();
        let recompute_clone = recompute.clone();
        stream_row.connect_selected_notify(move |combo| {
            if let Some(item) = combo.selected_item() {
                if let Ok(obj) = item.downcast::<gtk::StringObject>() {
                    let stream_str = obj.string();
                    *selected_stream_clone.borrow_mut() = stream_str.to_string();
                    recompute_clone();
                }
            }
        });

        *current_family.borrow_mut() = Some(family);
        glib::ControlFlow::Break
    });
}

/// Substitute the image name in `registry/org/image:tag` based on the
/// resolved family + feature selection. Returns the original ref unchanged
/// if no family was detected or the feature combination has no published
/// image — keeps us from constructing refs the registry doesn't serve.
///
/// Delegates the family → image resolution to the service layer
/// ([`UpdaterService::resolve_target`]) so a future alternative frontend can
/// share the same logic without re-implementing it.
pub(super) fn resolve_target_ref(
    full_ref: &str,
    family: Option<&FamilyInfo>,
    selected_features: &[String],
) -> String {
    let Some(family) = family else {
        return full_ref.to_string();
    };
    let Some(target) = service::global().resolve_target(family, selected_features) else {
        return full_ref.to_string();
    };
    // full_ref = registry/org/image:tag — swap `image` only, preserving the
    // tag the user picked from the calendar.
    let Some((before_tag, tag)) = full_ref.rsplit_once(':') else {
        return full_ref.to_string();
    };
    let Some((reg_org, _old_image)) = before_tag.rsplit_once('/') else {
        return full_ref.to_string();
    };
    format!("{reg_org}/{}:{tag}", target.image)
}
