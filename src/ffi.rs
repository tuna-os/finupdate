//! C ABI surface for the gnome-control-center panel.
//!
//! The downstream `gnome-control-center` build in
//! [Path 1 of the integration plan](../../docs/control-center-integration.md)
//! links against `libfinupdate.so` (built from this crate's `cdylib`
//! target) and calls into the functions defined here. The C-side panel
//! is in `cc-panel/`.
//!
//! ## Lifetime + threading
//!
//! All entry points take a `*mut Handle` obtained from
//! [`finupdate_new`] and freed by [`finupdate_free`]. The handle owns
//! a `tokio` runtime and the cached service state — caller is
//! responsible for keeping it alive for the lifetime of the panel.
//! Concurrent calls on the same handle are safe (`tokio` + the
//! service's internal `Mutex`); calls from multiple threads are not
//! recommended only because the eventual GTK callbacks need to land on
//! the GLib main loop, which is the C panel's job.
//!
//! ## Async results
//!
//! Functions that perform I/O take a `*const c_void user_data` plus a
//! C callback `extern "C" fn`. The runtime invokes the callback from a
//! worker thread when the operation completes — the C panel must
//! marshal the result back to the GLib main loop (via `g_idle_add` or
//! `g_main_context_invoke`) before touching widgets.
//!
//! ## Strings
//!
//! Every `*mut c_char` returned from this module is heap-allocated by
//! Rust (`CString::into_raw`) and **must** be freed with
//! [`finupdate_string_free`]. `*const c_char` parameters going INTO
//! Rust are borrowed for the duration of the call — Rust will not
//! retain them.
//!
//! ## Logging
//!
//! There is no Rust `main()` in this configuration, so [`finupdate_new`]
//! installs the `tracing` subscriber that `main.rs`/`cli.rs` would
//! otherwise install. Without it every event from `finupdate-core` is
//! dropped and a failed update check reaches the C panel as a bare `-1`.
//! The default filter is `warn`; set `RUST_LOG=finupdate=debug` when
//! debugging the panel. A subscriber already installed by the host process
//! wins — we never replace one.
//!
//! ## Stability
//!
//! This surface is the contract between the Rust backend and the
//! downstream C panel. Treat it as a public API: additions are fine,
//! removals or signature changes break the panel's ABI and require a
//! coordinated rebuild.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::ptr;
use std::sync::Arc;

use gtk::glib::translate::IntoGlibPtr;
use gtk::prelude::*;
use relm4::{Component, ComponentController};

use crate::service::{self, UpdaterService};

/// Opaque handle the C panel passes back into every entry point. Holds a
/// tokio runtime + the service trait object so each FFI call doesn't have
/// to spin up its own runtime (which would deadlock under
/// `Runtime::block_on` if called from inside another runtime).
pub struct Handle {
    rt: tokio::runtime::Runtime,
    service: Arc<dyn UpdaterService>,
}

/// Construct a new handle. Returns `NULL` if the tokio runtime can't be
/// built (e.g. file descriptor exhaustion). The caller owns the result and
/// must release it via [`finupdate_free`].
///
/// # Safety
///
/// Safe to call from C; the returned pointer is to a Rust-allocated
/// `Box<Handle>` and must not be freed with `free()`.
#[unsafe(no_mangle)]
pub extern "C" fn finupdate_new() -> *mut Handle {
    init_tracing();

    let Ok(rt) = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .thread_name("finupdate-ffi")
        .build()
    else {
        tracing::error!("finupdate_new: tokio runtime build failed, returning NULL handle");
        return ptr::null_mut();
    };

    // Initialize Gtk and Libadwaita on the Rust side so that widget
    // builders do not panic when called from C/GObject host contexts.
    let _ = adw::init();

    // Lazily initialise the process-wide service the same way the GUI
    // does. The C panel will be the only caller in its process, so this
    // OnceLock write is uncontested.
    let svc = service::ensure_initialised();

    Box::into_raw(Box::new(Handle { rt, service: svc }))
}

/// Release a handle previously returned by [`finupdate_new`].
///
/// # Safety
///
/// `handle` must be a pointer returned by [`finupdate_new`] that has not
/// yet been freed. Passing NULL is permitted and is a no-op.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn finupdate_free(handle: *mut Handle) {
    if handle.is_null() {
        return;
    }
    // SAFETY: caller has promised the pointer originated from `Box::into_raw`.
    drop(unsafe { Box::from_raw(handle) });
}

/// Release a string previously returned by any `finupdate_*` getter.
///
/// # Safety
///
/// `s` must be either NULL or a pointer returned by a `finupdate_*`
/// function. Double-free is undefined behaviour.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn finupdate_string_free(s: *mut c_char) {
    if s.is_null() {
        return;
    }
    // SAFETY: caller has promised the pointer originated from CString::into_raw.
    drop(unsafe { CString::from_raw(s) });
}

/// Pretty title for the currently booted image, e.g. "Bluefin Dakota".
/// Returns a heap-allocated NUL-terminated UTF-8 string the caller owns
/// (free with [`finupdate_string_free`]). Never returns NULL — falls back
/// to a generic "System Image" string when detection fails.
///
/// # Safety
///
/// `handle` must be a valid non-NULL pointer returned by [`finupdate_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn finupdate_current_image_title(handle: *mut Handle) -> *mut c_char {
    let Some(handle) = (unsafe { handle.as_ref() }) else {
        tracing::warn!("finupdate_current_image_title: NULL handle, using fallback title");
        return string_to_c("System Image");
    };
    let title = handle
        .rt
        .block_on(async {
            handle
                .service
                .current_image()
                .await
                .inspect_err(|e| {
                    tracing::warn!(error = %e, "current_image failed, using fallback title");
                })
                .ok()
        })
        .map(|img| format!("{}/{}", img.org, img.image))
        .unwrap_or_else(|| "System Image".to_string());
    string_to_c(&title)
}

/// Full registry reference for the currently booted image, e.g.
/// `ghcr.io/projectbluefin/dakota:latest`. Returns NULL when the booted
/// image can't be detected (caller should hide the row).
///
/// # Safety
///
/// `handle` must be a valid non-NULL pointer returned by [`finupdate_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn finupdate_current_image_ref(handle: *mut Handle) -> *mut c_char {
    let Some(handle) = (unsafe { handle.as_ref() }) else {
        tracing::warn!("finupdate_current_image_ref: NULL handle");
        return ptr::null_mut();
    };
    let Some(img) = handle.rt.block_on(async {
        handle
            .service
            .current_image()
            .await
            .inspect_err(|e| {
                tracing::warn!(
                    error = %e,
                    "current_image failed, panel will hide the image row"
                );
            })
            .ok()
    }) else {
        return ptr::null_mut();
    };
    string_to_c(&format!(
        "{}/{}/{}:{}",
        img.registry, img.org, img.image, img.tag
    ))
}

/// Probe the registry for a newer build of the booted image. Asynchronous —
/// invokes `callback(update_available, user_data)` from a worker thread
/// when the check completes. `update_available` is 1 when a newer build
/// exists, 0 when up-to-date, -1 on error.
///
/// The C panel is expected to marshal the callback back onto the GLib
/// main loop via `g_idle_add_full` before touching widgets.
///
/// # Safety
///
/// `handle` must be a valid non-NULL pointer returned by [`finupdate_new`].
/// `callback` must outlive the call (typically a static function pointer).
/// `user_data` is opaque to Rust; ensure it remains valid until the
/// callback fires (or until [`finupdate_free`] is called, which won't
/// cancel the in-flight check — TODO: add a cancellation token).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn finupdate_check_for_updates(
    handle: *mut Handle,
    callback: extern "C" fn(update_available: c_int, user_data: *mut c_void),
    user_data: *mut c_void,
) {
    let Some(handle) = (unsafe { handle.as_ref() }) else {
        tracing::warn!("finupdate_check_for_updates: NULL handle, reporting error to caller");
        callback(-1, user_data);
        return;
    };
    let svc = handle.service.clone();
    // Wrap user_data so we can pass it into the async task. It's a raw
    // pointer the caller has promised will stay valid until the callback
    // fires — we just relay it.
    let user_data_addr = user_data as usize;
    handle.rt.spawn(async move {
        let result: c_int = match svc.current_image().await {
            Ok(image) => match svc.list_versions(&image, 4).await {
                Ok(versions) => {
                    // "Update available" heuristic: registry has a newer
                    // build than the booted one. The GUI does a richer
                    // check (digest comparison, SBOM diff); this is the
                    // minimal signal for the cc-panel status row.
                    if versions.iter().any(|v| v.version != image.tag) {
                        1
                    } else {
                        0
                    }
                }
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        "update check failed: could not list registry versions"
                    );
                    -1
                }
            },
            Err(e) => {
                tracing::error!(error = %e, "update check failed: could not detect booted image");
                -1
            }
        };
        callback(result, user_data_addr as *mut c_void);
    });
}

/// Construct the main updates panel widget for embedding in a host container —
/// the gnome-control-center updates panel uses this to display the entire app.
///
/// Returns a `GtkWidget *` (typed as `void *`; cast on the C side).
///
/// MUST be called from the thread that owns the GLib main loop.
///
/// # Safety
///
/// `handle` must be a valid non-NULL pointer returned by [`finupdate_new`].
/// Caller must be on the GLib main thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn finupdate_panel_widget_new(handle: *mut Handle) -> *mut c_void {
    let Some(_) = (unsafe { handle.as_ref() }) else {
        return ptr::null_mut();
    };

    let controller = crate::app::UpdatesPanel::builder().launch(true).detach();

    let widget = controller.widget().clone();

    // Tie the lifecycle of the Relm4 controller to the GTK widget
    unsafe {
        widget.set_data("finupdate-controller", controller);
    }

    IntoGlibPtr::<*mut gtk::ffi::GtkWidget>::into_glib_ptr(widget.upcast::<gtk::Widget>())
        as *mut c_void
}

/// Construct the rebase/image-switch widget for embedding in a host
/// container — the gnome-control-center panel uses this to fill its
/// AdwNavigationView "Change image" subpage.
///
/// Returns a `GtkWidget *` (typed as `void *`; cast on the C side). The
/// widget is an `AdwPreferencesPage` with variant toggles, stream
/// dropdown, recent-builds list, and a Switch button — same ownership
/// transfer rules as [`finupdate_changelog_widget_new`].
///
/// MUST be called from the thread that owns the GLib main loop.
///
/// # Safety
///
/// `handle` must be a valid non-NULL pointer returned by [`finupdate_new`].
/// Caller must be on the GLib main thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn finupdate_rebase_widget_new(handle: *mut Handle) -> *mut c_void {
    let Some(handle) = (unsafe { handle.as_ref() }) else {
        return ptr::null_mut();
    };
    let widget: gtk::Widget =
        crate::rebase_widget::build_rebase_widget(handle.service.clone(), &handle.rt).upcast();
    IntoGlibPtr::<*mut gtk::ffi::GtkWidget>::into_glib_ptr(widget) as *mut c_void
}

/// Construct the "What's New" changelog widget for embedding in a host
/// container — the gnome-control-center panel uses this to fill its
/// AdwNavigationView "Changelog" subpage.
///
/// Returns a `GtkWidget *` (typed as `void *` so this header has no
/// dependency on gtk4-sys; cast on the C side). The widget is an
/// `AdwPreferencesPage` ready to be added to any AdwNavigationView /
/// AdwLeaflet / AdwBin. Ownership: the caller becomes the parent, GTK's
/// floating-ref discipline takes care of the rest — do not free
/// manually.
///
/// MUST be called from the thread that owns the GLib main loop. The
/// returned widget kicks off its own async data fetches via the tokio
/// runtime inside `handle`; results land on the main loop via
/// `g_timeout_add_full`, so the panel's GTK loop is the synchronisation
/// point.
///
/// # Safety
///
/// `handle` must be a valid non-NULL pointer returned by [`finupdate_new`].
/// Caller must be on the GLib main thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn finupdate_changelog_widget_new(handle: *mut Handle) -> *mut c_void {
    let Some(handle) = (unsafe { handle.as_ref() }) else {
        return ptr::null_mut();
    };
    let widget: gtk::Widget =
        crate::changelog_widget::build_changelog_widget(handle.service.clone(), &handle.rt)
            .upcast();
    // IntoGlibPtr transfers ownership of the floating ref to the caller,
    // who will sink it when adding to a container.
    IntoGlibPtr::<*mut gtk::ffi::GtkWidget>::into_glib_ptr(widget) as *mut c_void
}

// ── apply-update event stream ────────────────────────────────────────────

/// Event kind for the apply-update stream. Maps 1:1 to
/// [`crate::update_worker::UpdateEvent`] so the C side can switch on the
/// integer.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinupdateEventKind {
    /// One line of stdout/stderr from the update process. `message`
    /// carries the text.
    Output = 0,
    /// A module's update started. `module` identifies which one.
    ModuleStarted = 1,
    /// A module's update finished. `module` + `status` carry the
    /// outcome; `exit_code` is set when status is `Failed`.
    ModuleFinished = 2,
    /// The whole update completed successfully.
    Complete = 3,
    /// The system was already up to date — no work performed.
    UpToDate = 4,
    /// The update process failed with the message in `message`.
    Error = 5,
}

/// Which of the four update modules an event refers to. `Unknown` is
/// emitted for non-module events (Output / Complete / UpToDate / Error).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinupdateModule {
    Unknown = 0,
    System = 1,
    Flatpak = 2,
    Brew = 3,
    Distrobox = 4,
}

/// Module completion status — only meaningful for `ModuleFinished`
/// events; pass `Unknown` otherwise.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinupdateModuleStatus {
    Unknown = 0,
    Success = 1,
    UpToDate = 2,
    Skipped = 3,
    Failed = 4,
}

/// Start a full update run (system + flatpak + brew + distrobox per
/// the user's "Include app updates" setting). The `callback` fires once
/// per event from a worker thread — the C panel must marshal each
/// invocation onto the GLib main loop via `g_idle_add_full` before
/// touching widgets. The stream terminates with one of
/// `Complete` / `UpToDate` / `Error`.
///
/// `exit_code` is set when an event reports a failed module (status =
/// `Failed`); zero otherwise. `message` is non-NULL for `Output` and
/// `Error` events; NULL otherwise. Both `message` and the strings
/// passed are Rust-owned and freed AFTER the callback returns — the C
/// side must copy them if it needs them past the callback's scope.
///
/// # Safety
///
/// `handle` must be a valid non-NULL pointer returned by [`finupdate_new`].
/// `callback` must outlive the run (static function pointer). `user_data`
/// must remain valid until the terminating event fires (or until
/// [`finupdate_free`] is called, which won't cancel the in-flight run
/// — TODO: add a cancel handle).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn finupdate_apply_update_start(
    handle: *mut Handle,
    callback: extern "C" fn(
        kind: FinupdateEventKind,
        module: FinupdateModule,
        status: FinupdateModuleStatus,
        exit_code: i32,
        message: *const c_char,
        user_data: *mut c_void,
    ),
    user_data: *mut c_void,
) {
    let Some(handle) = (unsafe { handle.as_ref() }) else {
        // Synthesise an Error and signal completion so callers don't
        // hang waiting for a stream that will never start.
        let err = CString::new("invalid handle").unwrap();
        callback(
            FinupdateEventKind::Error,
            FinupdateModule::Unknown,
            FinupdateModuleStatus::Unknown,
            0,
            err.as_ptr(),
            user_data,
        );
        return;
    };

    let user_data_addr = user_data as usize;
    handle.rt.spawn(async move {
        let (_cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
        let mut rx = crate::update_worker::UpdateWorker::new()
            .run(cancel_rx)
            .await;

        while let Some(event) = rx.recv().await {
            emit_event(callback, user_data_addr as *mut c_void, event);
        }
    });
}

fn emit_event(
    callback: extern "C" fn(
        FinupdateEventKind,
        FinupdateModule,
        FinupdateModuleStatus,
        i32,
        *const c_char,
        *mut c_void,
    ),
    user_data: *mut c_void,
    event: crate::update_worker::UpdateEvent,
) {
    use crate::orchestrator::ModuleStatus;
    use crate::update_worker::UpdateEvent;

    // Hold the CString alive across the callback so the pointer
    // remains valid for the duration of the call.
    let mut _msg_owner: Option<CString> = None;
    let mut msg_ptr: *const c_char = ptr::null();

    let (kind, module, status, exit_code) = match event {
        UpdateEvent::Output(line) => {
            _msg_owner = CString::new(line).ok();
            msg_ptr = _msg_owner
                .as_ref()
                .map(|c| c.as_ptr())
                .unwrap_or(ptr::null());
            (
                FinupdateEventKind::Output,
                FinupdateModule::Unknown,
                FinupdateModuleStatus::Unknown,
                0,
            )
        }
        UpdateEvent::ModuleStarted(m) => (
            FinupdateEventKind::ModuleStarted,
            module_to_ffi(m),
            FinupdateModuleStatus::Unknown,
            0,
        ),
        UpdateEvent::ModuleFinished(m, s) => {
            let (status_ffi, exit) = match s {
                ModuleStatus::Success => (FinupdateModuleStatus::Success, 0),
                ModuleStatus::UpToDate => (FinupdateModuleStatus::UpToDate, 0),
                ModuleStatus::Skipped => (FinupdateModuleStatus::Skipped, 0),
                ModuleStatus::Failed(code) => (FinupdateModuleStatus::Failed, code),
            };
            (
                FinupdateEventKind::ModuleFinished,
                module_to_ffi(m),
                status_ffi,
                exit,
            )
        }
        UpdateEvent::Complete => (
            FinupdateEventKind::Complete,
            FinupdateModule::Unknown,
            FinupdateModuleStatus::Unknown,
            0,
        ),
        UpdateEvent::UpToDate => (
            FinupdateEventKind::UpToDate,
            FinupdateModule::Unknown,
            FinupdateModuleStatus::Unknown,
            0,
        ),
        UpdateEvent::Error(msg) => {
            _msg_owner = CString::new(msg).ok();
            msg_ptr = _msg_owner
                .as_ref()
                .map(|c| c.as_ptr())
                .unwrap_or(ptr::null());
            (
                FinupdateEventKind::Error,
                FinupdateModule::Unknown,
                FinupdateModuleStatus::Unknown,
                0,
            )
        }
    };

    callback(kind, module, status, exit_code, msg_ptr, user_data);
}

fn module_to_ffi(m: crate::orchestrator::Module) -> FinupdateModule {
    use crate::orchestrator::Module;
    match m {
        Module::System => FinupdateModule::System,
        Module::Flatpak => FinupdateModule::Flatpak,
        Module::Brew => FinupdateModule::Brew,
        Module::Distrobox => FinupdateModule::Distrobox,
    }
}

// ── helpers ─────────────────────────────────────────────────────────────────

/// Install a `tracing` subscriber for the FFI consumer.
///
/// `main.rs` and `cli.rs` do this for their own processes, but when this
/// crate is loaded as `libfinupdate.so` by gnome-control-center there is no
/// Rust `main()` — without this, every `tracing` event emitted by
/// `finupdate-core` (privileged-action suppression, registry failures,
/// update-worker errors) is discarded and the panel has no diagnostic trail.
///
/// Defaults to `warn` rather than `info` because the host process's stderr
/// belongs to gnome-control-center, not to us; `RUST_LOG=finupdate=debug`
/// opts into the detail. `try_init` (not `init`) so that a host which has
/// already installed a global subscriber keeps it instead of us panicking.
fn init_tracing() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
            )
            .try_init();
    });
}

fn string_to_c(s: &str) -> *mut c_char {
    // Strip interior NULs — UTF-8 strings from Rust shouldn't contain
    // them, but the panel will treat the result as a C string regardless.
    let cleaned: String = s.chars().filter(|&c| c != '\0').collect();
    match CString::new(cleaned) {
        Ok(c) => c.into_raw(),
        Err(e) => {
            tracing::error!(
                error = %e,
                "string_to_c: CString conversion failed, returning NULL"
            );
            ptr::null_mut()
        }
    }
}

#[allow(dead_code)]
fn cstr_to_str<'a>(s: *const c_char) -> Option<&'a str> {
    if s.is_null() {
        return None;
    }
    // SAFETY: caller guarantees the pointer is a NUL-terminated C string.
    unsafe { CStr::from_ptr(s) }.to_str().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_to_c_roundtrip() {
        let p = string_to_c("Bluefin Dakota");
        // SAFETY: just allocated by string_to_c.
        let back = unsafe { CStr::from_ptr(p) }.to_str().unwrap().to_string();
        assert_eq!(back, "Bluefin Dakota");
        unsafe { finupdate_string_free(p) };
    }

    #[test]
    fn string_to_c_strips_interior_nul() {
        // Defensive: shouldn't happen from Rust but guards against future
        // changes that leak a NUL into a label.
        let p = string_to_c("foo\0bar");
        let back = unsafe { CStr::from_ptr(p) }.to_str().unwrap().to_string();
        assert_eq!(back, "foobar");
        unsafe { finupdate_string_free(p) };
    }

    #[test]
    fn free_null_string_is_noop() {
        unsafe { finupdate_string_free(ptr::null_mut()) };
    }

    #[test]
    fn free_null_handle_is_noop() {
        unsafe { finupdate_free(ptr::null_mut()) };
    }
}
