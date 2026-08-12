//! Pure parsing helpers for image/registry references.
//!
//! Extracted from `status_view.rs` (finupdate#43): `parse_org_repo` has no
//! GTK dependency and is used by the changelog fetch path and the history
//! page, but lived buried inside a view file where it was only testable via
//! the module's widget test harness. Colocating it with its unit tests here
//! makes it reusable by `rebase_widget`, the CLI, and other non-GTK
//! consumers (same role as the `bootc_probe` helpers).

/// Split a container image URI into `(org, repo)`. Accepts registry
/// prefixes (`ghcr.io/org/repo`), bare `org/repo`, `docker://` URLs, and
/// nested GHCR paths (`org/sub/repo`); returns `None` for single segments.
pub(crate) fn parse_org_repo(uri: &str) -> Option<(String, String)> {
    let clean_uri = if let Some(pos) = uri.find("docker://") {
        &uri[pos + 9..]
    } else {
        uri
    };
    let parts: Vec<&str> = clean_uri.split('/').collect();
    if parts.len() >= 3 {
        let org = parts[1].to_string();
        let repo = parts[2..].join("/");
        Some((org, repo))
    } else if parts.len() == 2 {
        Some((parts[0].to_string(), parts[1].to_string()))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_org_repo ───────────────────────────────────────────────────

    #[test]
    fn parse_org_repo_ghcr_three_parts() {
        let r = parse_org_repo("ghcr.io/ublue-os/bluefin");
        assert_eq!(r, Some(("ublue-os".to_string(), "bluefin".to_string())));
    }

    #[test]
    fn parse_org_repo_two_parts() {
        // No registry prefix — treat as org/repo directly.
        let r = parse_org_repo("ublue-os/bluefin");
        assert_eq!(r, Some(("ublue-os".to_string(), "bluefin".to_string())));
    }

    #[test]
    fn parse_org_repo_strips_docker_prefix() {
        let r = parse_org_repo("docker://ghcr.io/ublue-os/bluefin");
        assert_eq!(r, Some(("ublue-os".to_string(), "bluefin".to_string())));
    }

    #[test]
    fn parse_org_repo_handles_nested_path() {
        // GHCR allows nested paths like /org/sub/image. We keep everything
        // past the first split as the repo so downstream code can construct
        // a valid GitHub URL.
        let r = parse_org_repo("ghcr.io/ublue-os/sub/bluefin");
        assert_eq!(r, Some(("ublue-os".to_string(), "sub/bluefin".to_string())));
    }

    #[test]
    fn parse_org_repo_rejects_single_segment() {
        assert!(parse_org_repo("bluefin").is_none());
    }
}
