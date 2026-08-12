//! UI component modules.
//!
//! Each sub-module is a self-contained relm4 component that can be
//! composed into the main app or reused across Bluefin utility apps.

pub mod bootc_probe;
pub(crate) mod bootc_progress; // helper: bootc switch progress parse + subprocess runner
pub mod host_actions;
pub mod log_view;
pub mod preferences;
pub mod rebase_dialog;
pub mod segmented_progress;
pub(crate) mod settings_io; // helper: auto-update settings read/write + timer toggle
pub mod status_view;
pub mod update_check_dialog;
pub mod update_list;
pub(crate) mod version_parse; // helper: pure image-ref parsing
