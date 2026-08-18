//! Unit tests for rebase dialog target resolution and toggle derivation.

use std::sync::Once;

use crate::service::{self, FamilyInfo};
use crate::ui::rebase_target::{derive_initial_toggle_state, resolve_dx_nvidia};
use super::switches::resolve_target_ref;

static INIT: Once = Once::new();

/// Tests need a process-wide UpdaterService since resolve_target_ref calls
/// service::global(). Install the default BootcUpdaterService once;
/// service::init() will panic on the second call so guard with Once.
fn ensure_service() {
    INIT.call_once(|| {
        service::init(service::BootcUpdaterService::new());
    });
}

fn bluefin_stable_info() -> FamilyInfo {
    FamilyInfo {
        name: "Bluefin Stable".to_string(),
        base_image: "bluefin".to_string(),
        streams: vec![],
        features: vec![],
    }
}

#[test]
fn resolve_passthrough_with_no_family() {
    ensure_service();
    let r = resolve_target_ref(
        "ghcr.io/ublue-os/bluefin:stable-daily-43.20260527",
        None,
        &[],
    );
    assert_eq!(r, "ghcr.io/ublue-os/bluefin:stable-daily-43.20260527");
}

#[test]
fn resolve_no_features_keeps_base_image() {
    ensure_service();
    let fam = bluefin_stable_info();
    let r = resolve_target_ref(
        "ghcr.io/ublue-os/bluefin:stable-daily-43.20260527",
        Some(&fam),
        &[],
    );
    assert_eq!(r, "ghcr.io/ublue-os/bluefin:stable-daily-43.20260527");
}

#[test]
fn resolve_swaps_image_to_nvidia_variant() {
    ensure_service();
    let fam = bluefin_stable_info();
    let r = resolve_target_ref(
        "ghcr.io/ublue-os/bluefin:stable-daily-43.20260527",
        Some(&fam),
        &["nvidia".to_string()],
    );
    assert_eq!(
        r,
        "ghcr.io/ublue-os/bluefin-nvidia:stable-daily-43.20260527"
    );
}

#[test]
fn resolve_combines_dx_and_nvidia() {
    ensure_service();
    let fam = bluefin_stable_info();
    let r = resolve_target_ref(
        "ghcr.io/ublue-os/bluefin:stable",
        Some(&fam),
        &["dx".to_string(), "nvidia".to_string()],
    );
    assert_eq!(r, "ghcr.io/ublue-os/bluefin-dx-nvidia:stable");
}

#[test]
fn resolve_unpublished_combination_falls_back() {
    ensure_service();
    let fam = bluefin_stable_info();
    let original = "ghcr.io/ublue-os/bluefin:stable";
    let r = resolve_target_ref(original, Some(&fam), &["open".to_string()]);
    assert_eq!(r, original);
}

#[test]
fn resolve_handles_missing_tag() {
    ensure_service();
    let fam = bluefin_stable_info();
    let r = resolve_target_ref(
        "ghcr.io/ublue-os/bluefin",
        Some(&fam),
        &["nvidia".to_string()],
    );
    assert_eq!(r, "ghcr.io/ublue-os/bluefin");
}

fn dakota_info() -> FamilyInfo {
    FamilyInfo {
        name: "Bluefin Dakota".to_string(),
        base_image: "dakota".to_string(),
        streams: vec![],
        features: vec![],
    }
}

fn bazzite_kde_info() -> FamilyInfo {
    FamilyInfo {
        name: "Bazzite KDE".to_string(),
        base_image: "bazzite".to_string(),
        streams: vec![],
        features: vec![],
    }
}

#[test]
fn dx_nvidia_both_off_returns_base() {
    ensure_service();
    let (feats, img) = resolve_dx_nvidia(&bluefin_stable_info(), false, false);
    assert_eq!(feats, Vec::<String>::new());
    assert_eq!(img.unwrap().image, "bluefin");
}

#[test]
fn dx_nvidia_dx_only_resolves_dx() {
    ensure_service();
    let (feats, img) = resolve_dx_nvidia(&bluefin_stable_info(), true, false);
    assert_eq!(feats, vec!["dx".to_string()]);
    assert_eq!(img.unwrap().image, "bluefin-dx");
}

#[test]
fn dx_nvidia_nvidia_only_on_bluefin_prefers_open() {
    ensure_service();
    let (feats, img) = resolve_dx_nvidia(&bluefin_stable_info(), false, true);
    assert_eq!(feats, vec!["nvidia".to_string(), "open".to_string()]);
    assert_eq!(img.unwrap().image, "bluefin-nvidia-open");
}

#[test]
fn dx_nvidia_both_on_bluefin_yields_dx_nvidia_open() {
    ensure_service();
    let (feats, img) = resolve_dx_nvidia(&bluefin_stable_info(), true, true);
    assert_eq!(
        feats,
        vec!["dx".to_string(), "nvidia".to_string(), "open".to_string()]
    );
    assert_eq!(img.unwrap().image, "bluefin-dx-nvidia-open");
}

#[test]
fn dx_nvidia_nvidia_on_dakota_falls_back_to_plain_nvidia() {
    ensure_service();
    let (feats, img) = resolve_dx_nvidia(&dakota_info(), false, true);
    assert_eq!(feats, vec!["nvidia".to_string()]);
    assert_eq!(img.unwrap().image, "dakota-nvidia");
}

#[test]
fn dx_nvidia_nvidia_on_bazzite_prefers_open() {
    ensure_service();
    let (feats, img) = resolve_dx_nvidia(&bazzite_kde_info(), false, true);
    assert_eq!(feats, vec!["nvidia".to_string(), "open".to_string()]);
    assert_eq!(img.unwrap().image, "bazzite-nvidia-open");
}

#[test]
fn dx_nvidia_dx_on_dakota_has_no_published_image() {
    ensure_service();
    let (feats, img) = resolve_dx_nvidia(&dakota_info(), true, false);
    assert_eq!(feats, vec!["dx".to_string()]);
    assert!(img.is_none());
}

fn image_ref(image: &str) -> service::ImageRef {
    service::ImageRef {
        registry: "ghcr.io".to_string(),
        org: "ublue-os".to_string(),
        image: image.to_string(),
        tag: "stable".to_string(),
        digest: String::new(),
    }
}

#[test]
fn initial_toggles_no_image_returns_off() {
    let (dx, nvidia) = derive_initial_toggle_state(&bluefin_stable_info(), None);
    assert!(!dx);
    assert!(!nvidia);
}

#[test]
fn initial_toggles_base_image_returns_off() {
    let img = image_ref("bluefin");
    let (dx, nvidia) = derive_initial_toggle_state(&bluefin_stable_info(), Some(&img));
    assert!(!dx);
    assert!(!nvidia);
}

#[test]
fn initial_toggles_dx_only() {
    let img = image_ref("bluefin-dx");
    let (dx, nvidia) = derive_initial_toggle_state(&bluefin_stable_info(), Some(&img));
    assert!(dx);
    assert!(!nvidia);
}

#[test]
fn initial_toggles_plain_nvidia() {
    let img = image_ref("bluefin-nvidia");
    let (dx, nvidia) = derive_initial_toggle_state(&bluefin_stable_info(), Some(&img));
    assert!(!dx);
    assert!(nvidia);
}

#[test]
fn initial_toggles_nvidia_open() {
    let img = image_ref("bluefin-nvidia-open");
    let (dx, nvidia) = derive_initial_toggle_state(&bluefin_stable_info(), Some(&img));
    assert!(!dx);
    assert!(nvidia);
}

#[test]
fn initial_toggles_dx_and_nvidia_open() {
    let img = image_ref("bluefin-dx-nvidia-open");
    let (dx, nvidia) = derive_initial_toggle_state(&bluefin_stable_info(), Some(&img));
    assert!(dx);
    assert!(nvidia);
}

#[test]
fn initial_toggles_unrelated_image_returns_off() {
    let img = image_ref("aurora-dx");
    let (dx, nvidia) = derive_initial_toggle_state(&bluefin_stable_info(), Some(&img));
    assert!(!dx);
    assert!(!nvidia);
}

#[test]
fn initial_toggles_dakota_plain_nvidia() {
    let img = service::ImageRef {
        registry: "ghcr.io".to_string(),
        org: "projectbluefin".to_string(),
        image: "dakota-nvidia".to_string(),
        tag: "latest".to_string(),
        digest: String::new(),
    };
    let (dx, nvidia) = derive_initial_toggle_state(&dakota_info(), Some(&img));
    assert!(!dx);
    assert!(nvidia);
}
