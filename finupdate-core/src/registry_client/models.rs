//! Registry data models: image versions, available tags, image families and
//! the internal GHCR API response types. No I/O, no parsing — pure types.
//!
//! Extracted from the former single-file `registry_client.rs` (see the
//! registry-module split); `mod.rs` re-exports these for callers.
//!
//! ## Tag format
//!
//! Universal Blue images use the pattern:
//! ```text
//! {stream}-{YYYYMMDD}    e.g.  stable-daily-43-20260222
//! {stream}.{YYYYMMDD}    e.g.  stable-daily-43.20260222   (dot variant)
//! ```
//! Both separators are supported; the dot form is preferred.

use chrono::{DateTime, NaiveDate, Utc};
use serde::Deserialize;
use std::collections::HashMap;

// ── Public data types ─────────────────────────────────────────────────────────

/// Metadata for a single dated image build available for rebasing.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ImageVersion {
    /// Calendar date the image was built (UTC, YYYYMMDD from the tag).
    pub date: NaiveDate,
    /// Full OCI image reference — pass this to `bootc switch`.
    pub full_ref: String,
    /// Human-readable version string from `org.opencontainers.image.version`.
    pub version: String,
    /// Kernel version from `ostree.linux` annotation.
    pub kernel: String,
    /// Short git commit hash (first 8 chars of `org.opencontainers.image.revision`).
    pub revision: String,
    /// Build timestamp from `org.opencontainers.image.created`.
    pub created: DateTime<Utc>,
}

/// Error type for registry operations.
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("Network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("No dated tags found for stream '{0}'")]
    NoTags(String),
    #[error("Unable to detect current image — is bootc installed?")]
    #[allow(dead_code)]
    NoCurrentImage,
}

/// One entry in the available-tags list.
///
/// `raw` is the actual tag string the registry serves (what gets passed to
/// `bootc switch` / `image:tag` refs). `display` is what the tag dropdown
/// shows the user — typically equal to `raw`, but for sha-tagged manifests
/// (dakota-nvidia and other Project Bluefin images) we substitute a human-
/// friendly `"Build YYYY-MM-DD"` label so the dropdown isn't full of 40-char
/// hex hashes.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AvailableTag {
    pub display: String,
    pub raw: String,
}

/// One coherent product family — a user-facing concept that groups a set of
/// sibling image *names* (the GPU/hardware variants like `-nvidia`, `-dx`,
/// `-deck`) and the tag streams (channels) under which they're published.
///
/// A given GHCR image can belong to multiple families — Bluefin Stable and
/// Bluefin LTS, for instance, share the `ublue-os/bluefin` image but use
/// disjoint stream sets (`stable*` vs `lts*`).
#[derive(Debug, Clone, PartialEq)]
pub struct Family {
    /// Display name for menus / dropdowns: "Bluefin Stable", "Bluefin LTS".
    pub name: &'static str,
    /// Registry org owning every image in this family.
    pub org: &'static str,
    /// Sibling image names — what the rebase dialog's variant chips render.
    /// First entry is the canonical/default for chip rendering. Each entry
    /// resolves to `ghcr.io/{org}/{name}:{stream}` at rebase time.
    pub images: &'static [&'static str],
    /// Tag streams this family publishes under. The rebase / changelog UI
    /// can offer a stream picker. First entry is the canonical default.
    pub streams: &'static [&'static str],
}

/// Catalogue of Universal Blue + Project Bluefin product families.
///
/// **Add new families / variants here as Universal Blue ships them.** Source
/// of truth for the user-visible "family" concept across the app. The rebase
/// dialog's two-toggle UI resolves user picks against this table via
/// [`Family::best_match`] + [`UpdaterService::resolve_target`], so adding a
/// variant here automatically makes it selectable.
pub const KNOWN_FAMILIES: &[Family] = &[
    Family {
        name: "Bluefin Stable",
        org: "ublue-os",
        images: &[
            "bluefin",
            "bluefin-nvidia",
            "bluefin-nvidia-open",
            "bluefin-dx",
            "bluefin-dx-nvidia",
            "bluefin-dx-nvidia-open",
            "bluefin-asus",
            "bluefin-asus-nvidia",
            "bluefin-surface",
            "bluefin-framework",
        ],
        streams: &["latest", "stable", "stable-daily", "beta", "gts"],
    },
    Family {
        name: "Bluefin LTS",
        org: "ublue-os",
        images: &[
            "bluefin",
            "bluefin-nvidia",
            "bluefin-dx",
            "bluefin-dx-nvidia",
            "bluefin-gdx",
        ],
        streams: &["lts", "lts-hwe", "lts-amd64", "lts-arm64", "gdx"],
    },
    Family {
        name: "Aurora",
        org: "ublue-os",
        images: &[
            "aurora",
            "aurora-nvidia",
            "aurora-nvidia-open",
            "aurora-dx",
            "aurora-dx-nvidia",
            "aurora-dx-nvidia-open",
        ],
        streams: &["latest", "stable", "stable-daily", "beta"],
    },
    Family {
        name: "Bazzite KDE",
        org: "ublue-os",
        images: &[
            "bazzite",
            "bazzite-nvidia",
            "bazzite-nvidia-open",
            "bazzite-deck",
            "bazzite-deck-nvidia",
            "bazzite-asus",
            "bazzite-framework",
        ],
        streams: &["stable", "testing", "unstable", "latest"],
    },
    Family {
        name: "Bazzite GNOME",
        org: "ublue-os",
        images: &["bazzite-gnome", "bazzite-gnome-nvidia"],
        streams: &["stable", "testing", "unstable", "latest"],
    },
    // ucore intentionally omitted — server image, out of scope for the
    // desktop bootc settings app. If you ever booted a finupdate user onto
    // ucore by accident, the "Family not recognized" fallback in the rebase
    // dialog catches it.
    Family {
        name: "Bluefin Dakota",
        org: "projectbluefin",
        // Only `dakota` and `dakota-nvidia` are published on GHCR (verified
        // 2026-05-30). The Bluefin/Aurora-style -dx and -nvidia-open variants
        // don't exist for Dakota — leaving them here would let the rebase
        // dialog show feature switches that resolve to bogus refs.
        images: &["dakota", "dakota-nvidia"],
        streams: &["latest", "testing"],
    },
];

impl Family {
    /// Find every family that contains `image` under `org`. An image can
    /// belong to more than one family (Bluefin's image is shared between
    /// Bluefin Stable and Bluefin LTS; the stream tells them apart).
    pub fn all_for_image(org: &str, image: &str) -> Vec<&'static Family> {
        KNOWN_FAMILIES
            .iter()
            .filter(|f| f.org == org && f.images.iter().any(|i| *i == image))
            .collect()
    }

    /// Pick the family that best matches an `(org, image, stream)` triple by
    /// preferring families whose streams contain `stream` exactly. Falls back
    /// to any family containing the image, then `None`.
    pub fn best_match(org: &str, image: &str, stream: &str) -> Option<&'static Family> {
        let candidates = Self::all_for_image(org, image);
        candidates
            .iter()
            .find(|f| f.streams.iter().any(|s| *s == stream))
            .copied()
            .or_else(|| candidates.first().copied())
    }

    /// The first image name is treated as the family's *base* — every other
    /// image in `images` is derived from it by adding feature suffixes.
    /// E.g. Bluefin Stable's base is "bluefin"; "bluefin-nvidia" is "bluefin"
    /// plus the {nvidia} feature; "bluefin-dx-nvidia" is base + {dx, nvidia}.
    pub fn base_image(&self) -> &'static str {
        self.images.first().copied().unwrap_or("")
    }

    /// Atomic feature suffixes available in this family — derived from the
    /// image names by splitting each non-base image's suffix on '-'. Powers
    /// the SwitchRow list in the rebase dialog: e.g. Bluefin Stable yields
    /// `["asus", "dx", "framework", "nvidia", "open", "surface"]`.
    ///
    /// The order is alphabetical for stable UI rendering. Not every
    /// combination is valid — call [`Family::select_image_for_features`] to
    /// resolve a switch state to a concrete image (returns `None` if no
    /// image in the family has that exact combination).
    pub fn available_features(&self) -> Vec<&'static str> {
        let base = self.base_image();
        let mut set: std::collections::BTreeSet<&'static str> = Default::default();
        for img in self.images {
            if *img == base {
                continue;
            }
            if let Some(suffix) = img.strip_prefix(&format!("{}-", base)) {
                for atom in suffix.split('-') {
                    set.insert(atom);
                }
            }
        }
        set.into_iter().collect()
    }

    /// Given a set of selected atomic features (`features`), find the image
    /// name in this family whose suffix is exactly that set.
    ///
    /// Returns `Some(image_name)` when the combination matches a published
    /// image (`"bluefin"` for `[]`, `"bluefin-nvidia"` for `["nvidia"]`,
    /// `"bluefin-dx-nvidia"` for `["dx", "nvidia"]`), or `None` if no image
    /// matches (e.g. `["open"]` alone — open driver requires nvidia).
    #[allow(dead_code)]
    pub fn select_image_for_features(&self, features: &[&str]) -> Option<&'static str> {
        let base = self.base_image();
        if features.is_empty() {
            return self.images.iter().copied().find(|i| *i == base);
        }
        for img in self.images {
            if *img == base {
                continue;
            }
            let suffix = match img.strip_prefix(&format!("{}-", base)) {
                Some(s) => s,
                None => continue,
            };
            let mut have: Vec<&str> = suffix.split('-').collect();
            have.sort();
            let mut want: Vec<&str> = features.iter().copied().collect();
            want.sort();
            if have == want {
                return Some(img);
            }
        }
        None
    }
}

// ── Internal GHCR API types ───────────────────────────────────────────────────

#[derive(Deserialize)]
pub(crate) struct TokenResponse {
    pub(crate) token: String,
}

#[derive(Deserialize)]
pub(crate) struct TagListResponse {
    pub(crate) tags: Vec<String>,
}

#[derive(Deserialize)]
pub(crate) struct ManifestResponse {
    pub(crate) annotations: Option<HashMap<String, String>>,
}
