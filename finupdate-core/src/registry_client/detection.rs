//! Host image identity discovery for the registry client.
//!
//! This module owns the precedence and host-command policy for determining the
//! currently booted image. Network access and registry queries remain in the
//! parent module.

use super::{RegistryClient, parse_image_ref, strip_date_suffix};

pub(super) async fn detect() -> Option<RegistryClient> {
    detect_with_settings(&crate::settings::Settings::load()).await
}

pub(super) async fn detect_with_settings(
    settings: &crate::settings::Settings,
) -> Option<RegistryClient> {
    println!("[debug] RegistryClient::detect_with_settings()");

    if let Some(mock) = settings.mock_identity.as_ref() {
        let stream = strip_date_suffix(&mock.tag).unwrap_or_else(|| mock.tag.clone());
        println!(
            "[debug] RegistryClient::detect_with_settings() mock_identity = {}/{}/{} stream={}",
            mock.registry, mock.org, mock.image, stream
        );
        return Some(RegistryClient::new(
            &mock.registry,
            &mock.org,
            &mock.image,
            &stream,
        ));
    }

    // FINUPDATE_IMAGE is a lenient development override: unlike bootc status,
    // its tag does not need to carry a date suffix.
    if let Ok(override_ref) = std::env::var("FINUPDATE_IMAGE") {
        if !override_ref.is_empty() {
            if let Some((without_tag, tag)) = override_ref.rsplit_once(':') {
                let parts: Vec<&str> = without_tag.splitn(3, '/').collect();
                if parts.len() >= 3 {
                    let stream = strip_date_suffix(tag).unwrap_or_else(|| tag.to_string());
                    println!(
                        "[debug] RegistryClient::detect_with_settings() FINUPDATE_IMAGE = {}",
                        override_ref
                    );
                    return Some(RegistryClient::new(parts[0], parts[1], parts[2], &stream));
                }
            }
        }
    }

    if let Some(client) = detect_from_bootc().await {
        return Some(client);
    }

    let fallback = detect_from_os_release();
    println!(
        "[debug] RegistryClient::detect() fallback os-release = {:?}",
        fallback.as_ref().map(|client| client.stream.clone())
    );
    fallback
}

async fn detect_from_bootc() -> Option<RegistryClient> {
    let cmd_name = if crate::update_worker::is_flatpak() {
        "flatpak-spawn --host bootc status --json"
    } else {
        "bootc status --json"
    };
    println!("[debug] RegistryClient::detect_from_bootc() running {cmd_name}");

    let mut output = if crate::update_worker::is_flatpak() {
        tokio::process::Command::new("flatpak-spawn")
            .args(["--host", "bootc", "status", "--json"])
            .output()
            .await
            .ok()?
    } else {
        tokio::process::Command::new("bootc")
            .args(["status", "--json"])
            .output()
            .await
            .ok()?
    };

    if !output.status.success() {
        let privileged = if crate::update_worker::is_flatpak() {
            tokio::process::Command::new("flatpak-spawn")
                .args(["--host", "pkexec", "bootc", "status", "--json"])
                .output()
                .await
        } else {
            tokio::process::Command::new("pkexec")
                .args(["bootc", "status", "--json"])
                .output()
                .await
        };
        if let Ok(result) = privileged {
            if result.status.success() {
                output = result;
            } else {
                return None;
            }
        } else {
            return None;
        }
    }

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let image_ref = json
        .pointer("/status/booted/image/image/image")
        .or_else(|| json.pointer("/status/booted/image/image"))
        .and_then(|value| value.as_str())?;
    parse_image_ref(image_ref)
}

fn read_os_release_content() -> Option<String> {
    if crate::update_worker::is_flatpak() {
        let output = std::process::Command::new("flatpak-spawn")
            .args(["--host", "cat", "/etc/os-release"])
            .output()
            .ok()?;
        if output.status.success() {
            String::from_utf8(output.stdout).ok()
        } else {
            None
        }
    } else {
        std::fs::read_to_string("/etc/os-release").ok()
    }
}

pub(super) fn detect_from_os_release() -> Option<RegistryClient> {
    read_os_release_content().and_then(|content| parse_os_release_content(&content))
}

fn parse_os_release_content(content: &str) -> Option<RegistryClient> {
    let mut image_ref = None;
    let mut image_tag = None;
    let mut image_id = None;
    let mut version_id = None;
    for line in content.lines() {
        if let Some(value) = line.strip_prefix("IMAGE_REF=") {
            image_ref = Some(value.trim_matches('"').to_string());
        } else if let Some(value) = line.strip_prefix("IMAGE_TAG=") {
            image_tag = Some(value.trim_matches('"').to_string());
        } else if let Some(value) = line.strip_prefix("IMAGE_ID=") {
            image_id = Some(value.trim_matches('"').to_string());
        } else if let Some(value) = line.strip_prefix("VERSION_ID=") {
            version_id = Some(value.trim_matches('"').to_string());
        }
    }

    if let Some(reference) = image_ref {
        let clean_ref = if let Some(position) = reference.find("docker://") {
            &reference[position + 9..]
        } else {
            &reference
        };
        let parts: Vec<&str> = clean_ref.split('/').collect();
        if parts.len() >= 3 {
            let image = parts[2..].join("/");
            let tag = image_tag.unwrap_or_else(|| "latest".to_string());
            let stream = strip_date_suffix(&tag).unwrap_or(tag);
            return Some(RegistryClient::new(parts[0], parts[1], &image, &stream));
        }
    }

    if let (Some(image), Some(version)) = (image_id, version_id) {
        let org =
            if image.contains("dakota") || image.contains("bluefin") || image.contains("aurora") {
                "projectbluefin"
            } else {
                "ublue-os"
            };
        let stream = if version == "latest" {
            "latest".to_string()
        } else {
            format!("stable-daily-{version}")
        };
        return Some(RegistryClient::new("ghcr.io", org, &image, &stream));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_client(client: RegistryClient, registry: &str, org: &str, image: &str, stream: &str) {
        assert_eq!(client.registry(), registry);
        assert_eq!(client.org(), org);
        assert_eq!(client.image(), image);
        assert_eq!(client.stream(), stream);
    }

    #[test]
    fn parses_docker_image_ref_and_strips_date_from_image_tag() {
        let client = parse_os_release_content(
            r#"IMAGE_REF="docker://ghcr.io/ublue-os/bluefin"
IMAGE_TAG="stable-daily-43.20260222"
"#,
        )
        .expect("image ref should be detected");

        assert_client(client, "ghcr.io", "ublue-os", "bluefin", "stable-daily-43");
    }

    #[test]
    fn image_ref_defaults_to_latest_when_image_tag_is_absent() {
        let client = parse_os_release_content("IMAGE_REF=ghcr.io/projectbluefin/dakota\n")
            .expect("image ref should be detected");

        assert_client(client, "ghcr.io", "projectbluefin", "dakota", "latest");
    }

    #[test]
    fn image_id_fallback_maps_known_families_to_projectbluefin() {
        let client = parse_os_release_content("IMAGE_ID=aurora\nVERSION_ID=42\n")
            .expect("image id fallback should be detected");

        assert_client(
            client,
            "ghcr.io",
            "projectbluefin",
            "aurora",
            "stable-daily-42",
        );
    }

    #[test]
    fn image_id_fallback_uses_ublue_for_unknown_families() {
        let client = parse_os_release_content("IMAGE_ID=custom-os\nVERSION_ID=latest\n")
            .expect("image id fallback should be detected");

        assert_client(client, "ghcr.io", "ublue-os", "custom-os", "latest");
    }

    #[test]
    fn malformed_image_ref_falls_back_to_image_id() {
        let client = parse_os_release_content(
            "IMAGE_REF=not-a-container-reference\nIMAGE_ID=bluefin\nVERSION_ID=43\n",
        )
        .expect("image id fallback should be detected");

        assert_client(
            client,
            "ghcr.io",
            "projectbluefin",
            "bluefin",
            "stable-daily-43",
        );
    }

    #[test]
    fn missing_identity_fields_return_none() {
        assert!(parse_os_release_content("NAME=Fedora Linux\nVERSION_ID=43\n").is_none());
    }
}
