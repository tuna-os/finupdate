//! Finupdate library — shared modules for the GUI and CLI binaries.
//!
//! The service, registry, and settings modules come from the `finupdate-core`
//! crate, which both binaries (finupdate GUI and finupdate-cli headless) depend
//! on. This lib.rs re-exports them so tests can access the shared logic without
//! depending on the full GUI/CLI stack.
//!
//! Also exposed via cdylib: see [`ffi`] for the C ABI consumed by the
//! gnome-control-center panel under `cc-panel/`.

// The backend now lives in the `finupdate-core` crate. Re-exported under their
// original paths so the UI's `crate::settings::…` / `crate::service::…`
// references keep resolving — the split is about enforcing the boundary, not
// about churning every call site.
pub use finupdate_core::{
    action_journal, config, gpu, orchestrator, privileged, registry_client, runtime, sbom_diff,
    service, settings, update_worker, uupd_compat,
};

pub mod app;
pub mod changelog_widget;
pub mod dbus_progress;
pub mod ffi;
pub mod rebase_widget;
pub mod ui;
