// Used by the GUI binary only — finupdate-cli compiles the module but
// doesn't call into it. Module-level allow to keep the multi-bin
// warnings list manageable.
#![allow(dead_code)]

//! SBOM diff — pure-Rust OCI referrer discovery and SBOM parsing.
//!
//! Replaces the previous `oras` subprocess approach with `oci-client`.
//!
//! ## Flow
//!
//! 1. For each image ref (booted + target), call the OCI Distribution v1.1
//!    referrers API to find a manifest with `artifactType: application/vnd.spdx+json`.
//! 2. Pull the SBOM blob from that referrer manifest.
//! 3. Parse it into a `name → version` map. The referrer is *advertised* as
//!    SPDX, but Universal Blue attaches **Syft JSON** under that artifact
//!    type, so both shapes are accepted — see the note above `SyftDocument`.
//! 4. Cache the map by referrer digest under `$XDG_CACHE_HOME/finupdate/sbom-cache/`.
//! 5. Diff the two maps and return `SbomDiffResult`.
//!
//! GHCR allows anonymous pulls for public images; `oci-client` handles the
//! `WWW-Authenticate: Bearer` token flow automatically.

use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr;

use oci_client::manifest::OciManifest;
use oci_client::secrets::RegistryAuth;
use oci_client::{Client, Reference};
use serde::Deserialize;

const SPDX_ARTIFACT_TYPE: &str = "application/vnd.spdx+json";

// ── Public types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct PackageDiff {
    pub name: String,
    pub old_version: String,
    pub new_version: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct SbomDiffResult {
    /// Packages whose version moved *forward*. Named `upgraded` and treated
    /// as such by the UI, so it must not also carry downgrades — it did until
    /// the split below, because the test was `booted != target`.
    pub upgraded: Vec<PackageDiff>,
    /// Packages whose version moved backward — switching to an older stream,
    /// or rolling back. `serde(default)` so a cached diff written before this
    /// field existed still deserialises.
    #[serde(default)]
    pub downgraded: Vec<PackageDiff>,
    pub removed: Vec<String>,
    pub added: Vec<PackageDiff>,
    #[serde(default)]
    pub stack_info: HashMap<String, (String, String)>,
}

// ── Internal SBOM types ───────────────────────────────────────────────────────
//
// Two formats, because the referrer is discovered by artifact type and what
// comes back is whatever the image publisher chose to attach.
//
// Universal Blue publishes **Syft JSON** — top-level `artifacts`, each with
// `name`/`version`/`type`. This code originally understood only SPDX
// (`packages[]` with `versionInfo`), and because every SPDX field is optional,
// serde parsed a Syft document into a document with no packages rather than
// failing. The result was a successful fetch, a successful parse, an empty
// map, and a package diff that silently rendered nothing on every machine —
// no error anywhere in the chain to notice.

#[derive(Deserialize)]
struct SyftDocument {
    artifacts: Option<Vec<SyftArtifact>>,
}

#[derive(Deserialize)]
struct SyftArtifact {
    name: String,
    version: Option<String>,
}

#[derive(Deserialize)]
struct SpdxDocument {
    packages: Option<Vec<SpdxPackage>>,
}

#[derive(Deserialize)]
struct SpdxPackage {
    name: String,
    #[serde(rename = "versionInfo")]
    version_info: Option<String>,
    #[serde(rename = "SPDXID")]
    spdx_id: Option<String>,
}

fn is_newer_version(
    name: &str,
    new_ver: &str,
    new_spdx_id: &str,
    old_ver: &str,
    old_spdx_id: &str,
) -> bool {
    if new_ver == "unknown" || new_ver.is_empty() {
        return false;
    }
    if old_ver == "unknown" || old_ver.is_empty() {
        return true;
    }

    // Check if either is a long git commit hash (e.g. 40-char or 64-char hex)
    let is_hash = |s: &str| -> bool { s.len() >= 32 && s.chars().all(|c| c.is_ascii_hexdigit()) };

    if is_hash(old_ver) && !is_hash(new_ver) {
        return true;
    }
    if is_hash(new_ver) && !is_hash(old_ver) {
        return false;
    }

    // Specific component prioritization based on SPDXID containing element names
    let pref_score = |name: &str, spdx_id: &str| -> i32 {
        match name {
            "linux" => {
                if spdx_id.contains("components-linux.bst") {
                    2
                } else {
                    0
                }
            }
            "gnome-control-center" => {
                if spdx_id.contains("gnome-control-center.bst") {
                    2
                } else {
                    0
                }
            }
            "mesa" => {
                if spdx_id.contains("mesa-mesa.bst") {
                    2
                } else if spdx_id.contains("mesa.bst") {
                    1
                } else {
                    0
                }
            }
            "podman" => {
                if spdx_id.contains("podman.bst") {
                    2
                } else {
                    0
                }
            }
            "bootc" => {
                if spdx_id.contains("bootc.bst") {
                    2
                } else {
                    0
                }
            }
            "systemd" => {
                if spdx_id.contains("systemd-base.bst") {
                    2
                } else if spdx_id.contains("systemd.bst") {
                    1
                } else {
                    0
                }
            }
            "pipewire" => {
                if spdx_id.contains("pipewire-base.bst") {
                    2
                } else if spdx_id.contains("pipewire.bst") {
                    1
                } else {
                    0
                }
            }
            "flatpak" => {
                if spdx_id.contains("flatpak.bst") {
                    2
                } else {
                    0
                }
            }
            _ => 0,
        }
    };

    let new_score = pref_score(name, new_spdx_id);
    let old_score = pref_score(name, old_spdx_id);

    if new_score > old_score {
        return true;
    }
    if old_score > new_score {
        return false;
    }

    // Prefer version strings containing dots
    let new_has_dots = new_ver.contains('.');
    let old_has_dots = old_ver.contains('.');
    if new_has_dots && !old_has_dots {
        return true;
    }
    if old_has_dots && !new_has_dots {
        return false;
    }

    false
}

/// Parse an SBOM blob into `name -> version`, accepting Syft or SPDX.
///
/// Returns `None` when neither shape yields any packages. That distinction
/// matters: an empty map used to be indistinguishable from a successful parse
/// of a package-less document, and it was cached as if it were real.
fn parse_sbom(bytes: &[u8]) -> Option<HashMap<String, String>> {
    if let Some(map) = parse_syft(bytes) {
        return Some(map);
    }
    parse_spdx(bytes)
}

/// Syft JSON — what Universal Blue actually attaches to its images.
fn parse_syft(bytes: &[u8]) -> Option<HashMap<String, String>> {
    let doc: SyftDocument = serde_json::from_slice(bytes).ok()?;
    let artifacts = doc.artifacts?;
    let map: HashMap<String, String> = artifacts
        .into_iter()
        .map(|a| (a.name, a.version.unwrap_or_else(|| "unknown".to_string())))
        .collect();
    (!map.is_empty()).then_some(map)
}

fn parse_spdx(bytes: &[u8]) -> Option<HashMap<String, String>> {
    let doc: SpdxDocument = serde_json::from_slice(bytes).ok()?;
    let mut map: HashMap<String, (String, String)> = HashMap::new();
    for pkg in doc.packages.unwrap_or_default() {
        let ver = pkg.version_info.unwrap_or_else(|| "unknown".to_string());
        let spdx_id = pkg.spdx_id.unwrap_or_default();
        if let Some((existing_ver, existing_spdx_id)) = map.get(&pkg.name) {
            if is_newer_version(&pkg.name, &ver, &spdx_id, existing_ver, existing_spdx_id) {
                map.insert(pkg.name, (ver, spdx_id));
            }
        } else {
            map.insert(pkg.name, (ver, spdx_id));
        }
    }
    let map: HashMap<String, String> = map.into_iter().map(|(k, (v, _))| (k, v)).collect();
    (!map.is_empty()).then_some(map)
}

// ── Cache helpers ─────────────────────────────────────────────────────────────

fn cache_dir() -> PathBuf {
    let base = std::env::var("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
            PathBuf::from(home).join(".cache")
        });
    base.join("finupdate").join("sbom-cache")
}

fn cache_path(digest: &str) -> PathBuf {
    cache_dir().join(digest.replace(':', "_"))
}

/// Read a cached package map, treating an empty one as a miss.
///
/// Not just belt-and-braces for the write-side guard: builds before that guard
/// existed cached empty maps on every parse miss, and those entries are still
/// on real machines. Without this, fixing the parser would have left anyone
/// who ran an older build with a permanently blank package diff and no way to
/// discover why. An empty SBOM is never a legitimate answer anyway.
fn load_cache(digest: &str) -> Option<HashMap<String, String>> {
    let data = std::fs::read(cache_path(digest)).ok()?;
    let map: HashMap<String, String> = serde_json::from_slice(&data).ok()?;
    (!map.is_empty()).then_some(map)
}

fn save_cache(digest: &str, map: &HashMap<String, String>) {
    let dir = cache_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    if let Ok(data) = serde_json::to_vec(map) {
        let _ = std::fs::write(cache_path(digest), data);
    }
}

// ── OCI helpers ───────────────────────────────────────────────────────────────

fn make_client() -> Client {
    Client::default()
}

/// Raw OCI image index entry — `oci_client::ImageIndexEntry` doesn't expose
/// `artifactType`, which is what we need to filter referrers by media type.
/// Parsing the raw JSON ourselves is one extra struct vs. round-tripping
/// every entry to check its manifest type.
#[derive(Deserialize)]
struct ReferrersIndex {
    manifests: Vec<ReferrerEntry>,
}
#[derive(Deserialize)]
struct ReferrerEntry {
    digest: String,
    #[serde(rename = "artifactType")]
    artifact_type: Option<String>,
}

/// Find the digest of the SPDX referrer manifest for `image_ref`.
///
/// GHCR returns HTTP 303 (redirect to a non-OCI URL) for the OCI v1.1
/// referrers API endpoint, which `oci_client::pull_referrers` doesn't follow
/// — so that path silently returns no results. We use the spec-defined
/// fallback tag convention instead: `<image>:sha256-<hex-digest>` resolves
/// to an image index whose manifests are the referrers. Filter client-side
/// by `artifactType`. See OCI Distribution Spec §referrers (fallback).
async fn find_spdx_referrer(client: &Client, image_ref: &Reference) -> Option<String> {
    let (_, subject_digest) = client
        .pull_manifest(image_ref, &RegistryAuth::Anonymous)
        .await
        .ok()?;

    tracing::debug!("subject digest for {}: {}", image_ref, subject_digest);

    // Fetch the fallback referrers tag as raw JSON — oci_client's typed
    // ImageIndexEntry strips the artifactType field we need.
    let fallback_tag = subject_digest.replace(':', "-");
    let url = format!(
        "https://{}/v2/{}/manifests/{}",
        image_ref.registry(),
        image_ref.repository(),
        fallback_tag
    );

    let token = ghcr_anonymous_token(image_ref.repository()).await;
    tracing::debug!(
        "referrers fallback: url={} token_present={}",
        url,
        token.is_some()
    );
    let http = reqwest::Client::new();
    let mut req = http
        .get(&url)
        .header("Accept", "application/vnd.oci.image.index.v1+json");
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("referrers fallback fetch failed: {}", e);
            return None;
        }
    };
    if !resp.status().is_success() {
        tracing::warn!(
            "referrers fallback tag {} returned HTTP {}",
            fallback_tag,
            resp.status()
        );
        return None;
    }
    let body = match resp.text().await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("referrers fallback body read failed: {}", e);
            return None;
        }
    };
    let index: ReferrersIndex = match serde_json::from_str(&body) {
        Ok(i) => i,
        Err(e) => {
            tracing::warn!(
                "referrers fallback JSON parse failed: {} (body head: {})",
                e,
                &body.chars().take(200).collect::<String>()
            );
            return None;
        }
    };
    tracing::debug!(
        "referrers fallback: parsed {} entries",
        index.manifests.len()
    );
    for m in &index.manifests {
        tracing::trace!(
            "  entry: digest={} artifactType={:?}",
            m.digest,
            m.artifact_type
        );
    }

    let found = index
        .manifests
        .into_iter()
        .find(|m| m.artifact_type.as_deref() == Some(SPDX_ARTIFACT_TYPE))
        .map(|m| m.digest);
    if found.is_none() {
        tracing::warn!("no SPDX referrer found in fallback index for {}", image_ref);
    }
    found
}

/// Mint a short-lived anonymous bearer token for pulling from a public
/// ghcr.io repository. GHCR requires a token even for anonymous reads.
/// Returns None on non-GHCR registries or token-endpoint failure (the
/// caller falls back to unauthenticated requests).
async fn ghcr_anonymous_token(repository: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct TokenResp {
        token: String,
    }
    let url = format!(
        "https://ghcr.io/token?service=ghcr.io&scope=repository:{}:pull",
        repository
    );
    reqwest::Client::new()
        .get(&url)
        .send()
        .await
        .ok()?
        .json::<TokenResp>()
        .await
        .ok()
        .map(|t| t.token)
}

/// Pull the SBOM blob from a referrer manifest digest and parse it into a
/// `name → version` map. Caches the result keyed by referrer digest.
async fn pull_sbom(
    client: &Client,
    image_ref: &Reference,
    referrer_digest: &str,
) -> Option<HashMap<String, String>> {
    if let Some(cached) = load_cache(referrer_digest) {
        tracing::debug!("SBOM cache hit for {}", referrer_digest);
        return Some(cached);
    }

    let referrer_ref = Reference::with_digest(
        image_ref.registry().to_string(),
        image_ref.repository().to_string(),
        referrer_digest.to_string(),
    );

    let (manifest, _) = client
        .pull_manifest(&referrer_ref, &RegistryAuth::Anonymous)
        .await
        .ok()?;

    // The SBOM is the first (and usually only) layer in the referrer manifest.
    let blob_digest = match manifest {
        OciManifest::Image(img) => img.layers.first()?.digest.clone(),
        OciManifest::ImageIndex(_) => return None,
    };

    let mut blob_bytes: Vec<u8> = Vec::new();
    client
        .pull_blob(&referrer_ref, blob_digest.as_str(), &mut blob_bytes)
        .await
        .ok()?;

    let Some(map) = parse_sbom(&blob_bytes) else {
        // Worth a warning, not a debug line: it means we fetched a real blob
        // and understood none of it, which is a format change we need to know
        // about rather than an empty diff the user quietly sees.
        tracing::warn!(
            bytes = blob_bytes.len(),
            "SBOM blob parsed to zero packages — unrecognised format?"
        );
        return None;
    };
    tracing::debug!(
        "parsed {} packages from SBOM {}",
        map.len(),
        referrer_digest
    );
    // Only cache a real result. An empty map used to be written here on any
    // parse miss, and load_cache() would then serve it forever — so a single
    // bad fetch made the package diff permanently empty on that machine, and
    // fixing the parser would not have fixed the symptom.
    if !map.is_empty() {
        save_cache(referrer_digest, &map);
    }
    Some(map)
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Diff the SBOM packages between `booted_ref` and `target_ref`.
///
/// Both refs are full OCI image references, e.g.
/// `ghcr.io/projectbluefin/dakota:latest` or with a digest suffix.
///
/// Returns `None` if either SBOM cannot be fetched.
pub async fn fetch_and_diff_sboms(
    booted_ref: String,
    target_ref: String,
) -> Option<SbomDiffResult> {
    tracing::info!("SBOM diff: {} -> {}", booted_ref, target_ref);

    let client = make_client();
    let booted = Reference::from_str(&booted_ref).ok()?;
    let target = Reference::from_str(&target_ref).ok()?;

    let (booted_referrer, target_referrer) = tokio::join!(
        find_spdx_referrer(&client, &booted),
        find_spdx_referrer(&client, &target),
    );

    let booted_digest = booted_referrer?;
    let target_digest = target_referrer?;
    tracing::debug!("booted SPDX referrer: {}", booted_digest);
    tracing::debug!("target SPDX referrer: {}", target_digest);

    let (booted_map, target_map) = tokio::join!(
        pull_sbom(&client, &booted, &booted_digest),
        pull_sbom(&client, &target, &target_digest),
    );

    let booted_map = booted_map?;
    let target_map = target_map?;

    tracing::info!(
        "SBOM diff: {} booted packages, {} target packages",
        booted_map.len(),
        target_map.len()
    );

    Some(diff_packages(&booted_map, &target_map))
}

/// Compute the diff between two package maps.
pub fn diff_packages(
    booted_map: &HashMap<String, String>,
    target_map: &HashMap<String, String>,
) -> SbomDiffResult {
    use crate::version_compare::{VersionChange, classify};

    let mut upgraded = Vec::new();
    let mut downgraded = Vec::new();
    let mut removed = Vec::new();
    let mut added = Vec::new();

    for (name, booted_ver) in booted_map {
        match target_map.get(name) {
            Some(target_ver) if booted_ver != target_ver => {
                let entry = PackageDiff {
                    name: name.clone(),
                    old_version: booted_ver.clone(),
                    new_version: target_ver.clone(),
                };
                // Direction, not mere difference. Everything landed in
                // `upgraded` before, so a rollback reported dozens of
                // "upgrades" that were all going backwards. Versions we
                // cannot order (Unknown) stay in `upgraded` — it is the
                // general "changed" bucket and the UI renders those neutrally.
                match classify(booted_ver, target_ver) {
                    VersionChange::Downgrade => downgraded.push(entry),
                    _ => upgraded.push(entry),
                }
            }
            Some(_) => {}
            None => removed.push(name.clone()),
        }
    }

    for (name, target_ver) in target_map {
        if !booted_map.contains_key(name) {
            added.push(PackageDiff {
                name: name.clone(),
                old_version: String::new(),
                new_version: target_ver.clone(),
            });
        }
    }

    upgraded.sort_by(|a, b| a.name.cmp(&b.name));
    downgraded.sort_by(|a, b| a.name.cmp(&b.name));
    removed.sort();
    added.sort_by(|a, b| a.name.cmp(&b.name));

    let stack_info = extract_stack_info(booted_map, target_map);

    SbomDiffResult {
        upgraded,
        downgraded,
        removed,
        added,
        stack_info,
    }
}

/// Key software stack components to surface in the changelog Stack section.
/// Each entry is (display_label, sbom_package_name).
const STACK_COMPONENTS: &[(&str, &str)] = &[
    ("Kernel", "linux"),
    ("GNOME", "gnome-control-center"),
    ("Mesa", "mesa"),
    ("Podman", "podman"),
    ("Nvidia", "NVIDIA-Linux-x86"),
    ("bootc", "bootc"),
    ("systemd", "systemd"),
    ("pipewire", "pipewire"),
    ("Flatpak", "flatpak"),
];

/// Extract (booted_version, target_version) pairs for each known stack
/// component. Only includes components where at least one side has a
/// meaningful (non-empty, non-"unknown") version string.
fn extract_stack_info(
    booted_map: &HashMap<String, String>,
    target_map: &HashMap<String, String>,
) -> HashMap<String, (String, String)> {
    let is_meaningful = |v: &str| -> bool { !v.is_empty() && v != "unknown" };

    let mut out = HashMap::new();
    for (label, pkg_name) in STACK_COMPONENTS {
        let booted_ver = booted_map.get(*pkg_name).map(|s| s.as_str()).unwrap_or("");
        let target_ver = target_map.get(*pkg_name).map(|s| s.as_str()).unwrap_or("");
        if is_meaningful(booted_ver) || is_meaningful(target_ver) {
            out.insert(
                label.to_string(),
                (booted_ver.to_string(), target_ver.to_string()),
            );
        }
    }
    out
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn map(entries: &[(&str, &str)]) -> HashMap<String, String> {
        entries
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn diff_identical_maps_yields_no_changes() {
        let m = map(&[("kernel", "7.0.7"), ("bash", "5.2.32")]);
        let result = diff_packages(&m, &m);
        assert!(result.upgraded.is_empty());
        assert!(result.added.is_empty());
        assert!(result.removed.is_empty());
    }

    #[test]
    fn diff_detects_version_upgrade() {
        let booted = map(&[("kernel", "7.0.6")]);
        let target = map(&[("kernel", "7.0.7")]);
        let r = diff_packages(&booted, &target);
        assert_eq!(
            r.upgraded,
            vec![PackageDiff {
                name: "kernel".into(),
                old_version: "7.0.6".into(),
                new_version: "7.0.7".into(),
            }]
        );
        assert!(r.added.is_empty());
        assert!(r.removed.is_empty());
    }

    #[test]
    fn diff_detects_added_and_removed_packages() {
        let booted = map(&[("kernel", "7.0.6"), ("old-tool", "1.0")]);
        let target = map(&[("kernel", "7.0.6"), ("new-tool", "2.0")]);
        let r = diff_packages(&booted, &target);
        assert!(r.upgraded.is_empty());
        assert_eq!(r.removed, vec!["old-tool".to_string()]);
        assert_eq!(
            r.added,
            vec![PackageDiff {
                name: "new-tool".into(),
                old_version: String::new(),
                new_version: "2.0".into(),
            }]
        );
    }

    #[test]
    fn diff_outputs_are_sorted_alphabetically() {
        let booted = map(&[("zlib", "1.3"), ("apr", "1.7"), ("middle", "1.0")]);
        let target = map(&[("zlib", "1.4"), ("apr", "1.8"), ("middle", "1.0")]);
        let r = diff_packages(&booted, &target);
        let names: Vec<_> = r.upgraded.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["apr", "zlib"]);
    }

    #[test]
    fn diff_unchanged_package_excluded_from_upgraded() {
        let booted = map(&[("stable", "1.0"), ("changed", "1.0")]);
        let target = map(&[("stable", "1.0"), ("changed", "2.0")]);
        let r = diff_packages(&booted, &target);
        assert_eq!(r.upgraded.len(), 1);
        assert_eq!(r.upgraded[0].name, "changed");
    }

    #[test]
    fn parse_spdx_extracts_name_and_version() {
        let json = br#"{
            "packages": [
                {"name": "kernel", "versionInfo": "7.0.7"},
                {"name": "bash", "versionInfo": "5.2.32"}
            ]
        }"#;
        let m = parse_spdx(json).unwrap();
        assert_eq!(m.get("kernel"), Some(&"7.0.7".to_string()));
        assert_eq!(m.get("bash"), Some(&"5.2.32".to_string()));
    }

    #[test]
    fn parse_spdx_merges_duplicates_intelligently() {
        let json = br#"{
            "packages": [
                {"name": "linux", "versionInfo": "6.8.0-git-commit-hash-long-hex-32chars", "SPDXID": "SPDXRef-other.bst"},
                {"name": "linux", "versionInfo": "6.8.9", "SPDXID": "SPDXRef-components-linux.bst"},
                {"name": "linux", "versionInfo": "unknown", "SPDXID": "SPDXRef-components-linux.bst"},
                {"name": "systemd", "versionInfo": "255.4-1.fc40", "SPDXID": "SPDXRef-systemd.bst"},
                {"name": "systemd", "versionInfo": "255.4", "SPDXID": "SPDXRef-systemd-base.bst"}
            ]
        }"#;
        let m = parse_spdx(json).unwrap();
        // Priorities components-linux.bst with 6.8.9 over git commit and unknown.
        assert_eq!(m.get("linux"), Some(&"6.8.9".to_string()));
        // Prioritizes systemd-base.bst over systemd.bst when versions are otherwise compatible.
        assert_eq!(m.get("systemd"), Some(&"255.4".to_string()));
    }

    #[test]
    fn parse_spdx_treats_missing_version_as_unknown() {
        let json = br#"{"packages": [{"name": "mystery"}]}"#;
        let m = parse_spdx(json).unwrap();
        assert_eq!(m.get("mystery"), Some(&"unknown".to_string()));
    }

    #[test]
    /// A package-less document is a parse *failure*, not an empty result.
    ///
    /// This test previously asserted the opposite — that `{"packages": []}`
    /// and `{}` both yield `Some(empty)`. That contract is what let a Syft
    /// document (which has no `packages` key at all) parse "successfully" as
    /// an SPDX document with nothing in it, so the package diff came back
    /// empty with no error and the empty map was then cached as real. Every
    /// caller here treats "no packages" as a failure, so say so in the type.
    fn parse_spdx_rejects_package_less_documents() {
        assert!(parse_spdx(br#"{"packages": []}"#).is_none());
        assert!(parse_spdx(br#"{}"#).is_none());
    }

    #[test]
    fn parse_spdx_rejects_malformed_json() {
        assert!(parse_spdx(b"not json").is_none());
    }

    #[test]
    fn cache_path_replaces_colons() {
        let p = cache_path("sha256:abc123");
        assert!(p.to_string_lossy().ends_with("sha256_abc123"));
    }
}

#[cfg(test)]
mod direction_tests {
    use super::*;

    fn map(entries: &[(&str, &str)]) -> HashMap<String, String> {
        entries
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// A rollback must not be reported as a list of upgrades.
    ///
    /// Every changed package used to land in `upgraded`, so switching from
    /// Dakota's F44 to Bluefin's F43 produced a "Updated · 65" heading over
    /// sixty-five packages that were all moving backwards.
    #[test]
    fn splits_downgrades_out_of_upgrades() {
        let booted = map(&[
            ("gnome-shell", "50.3-1.fc44"),
            ("bootc", "1.15.1-1.fc43"),
            ("mesa", "26.1.4-4.fc44"),
        ]);
        let target = map(&[
            ("gnome-shell", "49.7-1.fc43"), // back
            ("bootc", "1.16.3-1.fc44"),     // forward
            ("mesa", "26.1.4-4.fc44"),      // unchanged
        ]);

        let d = diff_packages(&booted, &target);

        assert_eq!(
            d.upgraded
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            ["bootc"]
        );
        assert_eq!(
            d.downgraded
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            ["gnome-shell"]
        );
        assert!(d.removed.is_empty() && d.added.is_empty());
    }

    /// Versions we cannot order stay in `upgraded`, the general "changed"
    /// bucket, rather than being asserted as a downgrade on a guess.
    #[test]
    fn unorderable_versions_are_not_called_downgrades() {
        let booted = map(&[("weird", "")]);
        let target = map(&[("weird", "deadbeef")]);
        let d = diff_packages(&booted, &target);
        assert!(d.downgraded.is_empty());
        assert_eq!(d.upgraded.len(), 1);
    }
}

#[cfg(test)]
mod parse_tests {
    use super::*;

    /// The shape Universal Blue actually publishes. Before this was supported,
    /// serde parsed it as an SPDX document with no packages — a successful
    /// parse producing an empty diff, with no error anywhere to notice.
    #[test]
    fn parses_syft_artifacts() {
        let blob = br#"{
            "artifacts": [
                {"name": "7zip", "version": "26.02-1.fc44", "type": "rpm"},
                {"name": "bash", "version": "5.3.0-1.fc44", "type": "rpm"}
            ],
            "schema": {"version": "16.0.0"}
        }"#;
        let map = parse_sbom(blob).expect("syft document should parse");
        assert_eq!(map.get("7zip").map(String::as_str), Some("26.02-1.fc44"));
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn still_parses_spdx_packages() {
        let blob = br#"{
            "packages": [
                {"name": "bash", "versionInfo": "5.3.0-1.fc44", "SPDXID": "SPDXRef-1"}
            ]
        }"#;
        let map = parse_sbom(blob).expect("spdx document should parse");
        assert_eq!(map.get("bash").map(String::as_str), Some("5.3.0-1.fc44"));
    }

    /// A document in neither format must be None, not an empty map. The
    /// difference is what stops a parse miss being cached as a real answer.
    #[test]
    fn unknown_format_is_none_not_empty() {
        assert!(parse_sbom(br#"{"componenets": []}"#).is_none());
        assert!(parse_sbom(br#"{"artifacts": []}"#).is_none());
        assert!(parse_sbom(b"not json at all").is_none());
    }
}

#[cfg(test)]
mod network_tests {
    use super::*;

    /// Hits ghcr.io. `--ignored` because the rest of the suite must stay
    /// offline and zero-privilege, but the SBOM path cannot be validated any
    /// other way: every failure mode below is in the fetch/parse chain, not in
    /// `diff_packages`, which the offline tests already cover thoroughly.
    ///
    ///     cargo test -p finupdate-core --lib -- --ignored --nocapture sbom_live
    /// Diagnostic: dump what the registry actually hands back, so a zero
    /// package count can be attributed to the fetch or to the parse.
    #[tokio::test]
    #[ignore = "network"]
    async fn sbom_live_blob_shape() {
        let client = make_client();
        let r = Reference::from_str("ghcr.io/ublue-os/bluefin:stable").unwrap();
        let referrer = find_spdx_referrer(&client, &r).await;
        println!("referrer: {referrer:?}");
        let Some(digest) = referrer else { return };

        let referrer_ref = Reference::with_digest(
            r.registry().to_string(),
            r.repository().to_string(),
            digest.clone(),
        );
        let (manifest, _) = client
            .pull_manifest(&referrer_ref, &RegistryAuth::Anonymous)
            .await
            .unwrap();
        let blob_digest = match &manifest {
            OciManifest::Image(img) => {
                for l in &img.layers {
                    println!("layer mediaType={} size={}", l.media_type, l.size);
                }
                img.layers.first().unwrap().digest.clone()
            }
            OciManifest::ImageIndex(_) => {
                println!("referrer is an index, not an image");
                return;
            }
        };
        let mut blob: Vec<u8> = Vec::new();
        client
            .pull_blob(&referrer_ref, blob_digest.as_str(), &mut blob)
            .await
            .unwrap();
        println!("blob bytes: {}", blob.len());
        println!(
            "first 300: {}",
            String::from_utf8_lossy(&blob[..blob.len().min(300)])
        );
        let parsed: Result<serde_json::Value, _> = serde_json::from_slice(&blob);
        match parsed {
            Ok(v) => {
                println!(
                    "top-level keys: {:?}",
                    v.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>())
                );
                println!(
                    "packages len: {:?}",
                    v.get("packages")
                        .and_then(|p| p.as_array())
                        .map(|a| a.len())
                );
            }
            Err(e) => println!("not JSON: {e}"),
        }
    }

    #[tokio::test]
    #[ignore = "network"]
    async fn sbom_live_fetch_returns_packages() {
        // Point the cache somewhere disposable: save_cache() stores empty maps
        // too, so one bad fetch otherwise sticks around and every later run
        // reports zero packages from a cache hit.
        let tmp = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CACHE_HOME", tmp.path()) };

        let result = fetch_and_diff_sboms(
            "ghcr.io/ublue-os/bluefin:stable".to_string(),
            "ghcr.io/ublue-os/bluefin:latest".to_string(),
        )
        .await;

        let diff = result.expect("SBOM fetch returned None — referrer or blob pull failed");
        let total = diff.added.len() + diff.removed.len() + diff.upgraded.len();
        println!(
            "added={} removed={} upgraded={}",
            diff.added.len(),
            diff.removed.len(),
            diff.upgraded.len()
        );
        assert!(
            total > 0,
            "both SBOMs parsed to zero packages — What's New would render an \
             empty package diff on every machine"
        );
    }
}
