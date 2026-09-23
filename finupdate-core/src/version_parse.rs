//! Pure parsing helpers for image and registry references.

/// Split a container image URI into `(org, repo)`. Accepts registry
/// prefixes (`ghcr.io/org/repo`), bare `org/repo`, `docker://` URLs, and
/// nested GHCR paths (`org/sub/repo`); returns `None` for single segments.
pub fn parse_org_repo(uri: &str) -> Option<(String, String)> {
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

    #[test]
    fn parse_org_repo_ghcr_three_parts() {
        let r = parse_org_repo("ghcr.io/ublue-os/bluefin");
        assert_eq!(r, Some(("ublue-os".to_string(), "bluefin".to_string())));
    }

    #[test]
    fn parse_org_repo_two_parts() {
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
        let r = parse_org_repo("ghcr.io/ublue-os/sub/bluefin");
        assert_eq!(r, Some(("ublue-os".to_string(), "sub/bluefin".to_string())));
    }

    #[test]
    fn parse_org_repo_rejects_single_segment() {
        assert!(parse_org_repo("bluefin").is_none());
    }
}
