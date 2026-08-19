//! Execution helpers for the rebase dialog (running bootc switch and access key formatting).

use adw::prelude::*;
use gtk::glib;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use crate::ui::bootc_progress::{BootcProgress, run_bootc_switch};

/// Mark the first character of a button label as its access key.
///
/// Applied at the widget layer rather than baked into the label strings so
/// [`compute_stream_switch_action`] keeps returning plain text its unit tests
/// can assert on, and so the underscore convention lives in exactly one place.
///
/// This is a HIG item — the dialog's primary action had no access key — but it
/// is also what makes the action reachable from the GUI suite at all: pointer
/// events do not reach dialog content under Broadway, so a mnemonic is the
/// only way to press this button in a test. The most consequential command the
/// app can issue was previously undrivable, and the check that claimed to
/// cover it merely opened the dialog and stopped.
///
/// Any literal underscore in `label` is escaped, otherwise it would silently
/// become a second, wrong access key.
pub(super) fn with_access_key(label: &str) -> String {
    let escaped = label.replace('_', "__");
    format!("_{escaped}")
}

/// Run a switch. Call this for every confirmed switch, in every mode.
///
/// Suppression belongs to `privileged_async` in run_bootc_switch, which
/// decides from `Suppressed::from_flags(dev_mode, dry_run)`, writes the
/// journal entry, and reports "Dry run — image switch recorded, not
/// performed". Keep the decision there and there only.
pub(super) fn run_rebase(full_ref: String, stack: gtk::Stack, dialog: adw::Dialog) {
    // Build a progress page with a pulsing ProgressBar + elapsed-time label.
    // A live `bootc switch` measured against ghcr.io took 2m28s for a full
    // dakota-nvidia pull on a residential link — too long for a bare spinner.
    // Pulse mode (no fraction) is the honest representation until we parse
    // bootc's per-layer progress lines (task #24 phase 2).
    let progress_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_start(24)
        .margin_end(24)
        .margin_top(12)
        .margin_bottom(24)
        .build();

    let progress_bar = gtk::ProgressBar::new();
    progress_bar.set_pulse_step(0.08);
    progress_box.append(&progress_bar);

    let elapsed_label = gtk::Label::new(Some("Elapsed: 0:00"));
    elapsed_label.add_css_class("dim-label");
    elapsed_label.add_css_class("caption");
    progress_box.append(&elapsed_label);

    let progress_page = adw::StatusPage::builder()
        .title("Switching...")
        .description("Pulling the new image layers. This typically takes 2–5 minutes.")
        .build();
    progress_page.set_child(Some(&progress_box));
    stack.add_named(&progress_page, Some("switching"));
    stack.set_visible_child_name("switching");

    // The bar pulses until we see a parseable Fraction event from bootc; from
    // that point on we drive `set_fraction` directly and stop pulsing. A flag
    // shared with the pulse timer lets the progress consumer disable it.
    let start = std::time::Instant::now();
    let bar_clone = progress_bar.clone();
    let label_clone = elapsed_label.clone();
    let known_fraction: Rc<Cell<bool>> = Rc::new(Cell::new(false));
    let known_fraction_pulse = known_fraction.clone();
    let pulse_handle: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
    let pulse_handle_store = pulse_handle.clone();
    let id = glib::timeout_add_local(std::time::Duration::from_millis(200), move || {
        if !known_fraction_pulse.get() {
            bar_clone.pulse();
        }
        let secs = start.elapsed().as_secs();
        label_clone.set_text(&format!("Elapsed: {}:{:02}", secs / 60, secs % 60));
        glib::ControlFlow::Continue
    });
    *pulse_handle_store.borrow_mut() = Some(id);

    // Cross-thread channel for streaming BootcProgress events from the
    // subprocess reader (background thread) to the GTK main loop where the
    // ProgressBar lives. std::sync::mpsc is enough here — we drain it from a
    // glib timeout below.
    let (prog_tx_std, prog_rx_std) = std::sync::mpsc::channel::<BootcProgress>();

    let result_slot: Arc<Mutex<Option<Result<(), String>>>> = Arc::new(Mutex::new(None));
    let result_bg = result_slot.clone();

    std::thread::spawn(move || {
        crate::runtime::block_on(async move {
            // tokio mpsc bridges the async readers to a sync channel the GTK
            // thread can poll. Each parsed BootcProgress flows: stdout/stderr
            // reader → tokio channel → forwarder → std::sync::mpsc → GTK.
            let (tokio_tx, mut tokio_rx) = tokio::sync::mpsc::unbounded_channel::<BootcProgress>();
            let prog_tx_std_inner = prog_tx_std.clone();
            let forward = tokio::spawn(async move {
                while let Some(p) = tokio_rx.recv().await {
                    let _ = prog_tx_std_inner.send(p);
                }
            });
            let result = run_bootc_switch(&full_ref, tokio_tx).await;
            let _ = forward.await;
            *result_bg.lock().unwrap() = Some(result);
        });
    });

    let progress_page_for_status = progress_page.clone();
    let bar_for_progress = progress_bar.clone();
    let known_fraction_for_progress = known_fraction.clone();
    glib::timeout_add_local(std::time::Duration::from_millis(150), move || {
        // Drain everything currently in the queue so a burst of layer events
        // doesn't lag behind. We don't break on Empty — that just means no
        // new events yet, keep the timer going.
        loop {
            match prog_rx_std.try_recv() {
                Ok(BootcProgress::Fraction { current, total }) => {
                    known_fraction_for_progress.set(true);
                    let frac = (current as f64) / (total as f64);
                    bar_for_progress.set_fraction(frac.clamp(0.0, 1.0));
                }
                Ok(BootcProgress::Status(s)) => {
                    progress_page_for_status.set_description(Some(&s));
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    return glib::ControlFlow::Break;
                }
            }
        }
        glib::ControlFlow::Continue
    });

    glib::timeout_add_local(std::time::Duration::from_millis(200), move || {
        let Some(result) = result_slot.lock().ok().and_then(|mut g| g.take()) else {
            return glib::ControlFlow::Continue;
        };
        // Stop the pulse animation now that we have a final result.
        if let Some(id) = pulse_handle.borrow_mut().take() {
            id.remove();
        }
        match result {
            Ok(()) => {
                // Success means "the chokepoint returned Ok", which in a dry
                // run means the switch was recorded and deliberately not
                // performed. Saying "Restart your system to boot into the
                // selected version" there would be a lie the user has no way
                // to catch — nothing changed and rebooting proves nothing.
                let settings = crate::settings::Settings::load();
                let suppressed = crate::action_journal::Suppressed::from_flags(
                    settings.dev_mode,
                    settings.dry_run,
                )
                .blocks_execution();

                let (title, description) = if suppressed {
                    (
                        "Switch Recorded",
                        "Dry run — the image switch was recorded but not \
                         performed. Nothing on this system has changed.",
                    )
                } else {
                    (
                        "Switch Complete",
                        "Restart your system to boot into the selected version.",
                    )
                };
                let done_page = adw::StatusPage::builder()
                    .title(title)
                    .description(description)
                    .icon_name("object-select-symbolic")
                    .build();
                let close_btn = gtk::Button::builder()
                    .label("Close")
                    .halign(gtk::Align::Center)
                    .build();
                close_btn.add_css_class("suggested-action");
                close_btn.add_css_class("pill");
                let dialog_close = dialog.clone();
                close_btn.connect_clicked(move |_| {
                    dialog_close.close();
                });
                done_page.set_child(Some(&close_btn));
                stack.add_named(&done_page, Some("done"));
                stack.set_visible_child_name("done");
            }
            Err(msg) => {
                let fail_page = adw::StatusPage::builder()
                    .title("Switch Failed")
                    .description(msg)
                    .icon_name("dialog-error-symbolic")
                    .build();
                let close_btn = gtk::Button::builder()
                    .label("Close")
                    .halign(gtk::Align::Center)
                    .build();
                close_btn.add_css_class("pill");
                let dialog_close = dialog.clone();
                close_btn.connect_clicked(move |_| {
                    dialog_close.close();
                });
                fail_page.set_child(Some(&close_btn));
                stack.add_named(&fail_page, Some("fail"));
                stack.set_visible_child_name("fail");
            }
        }
        glib::ControlFlow::Break
    });
}
