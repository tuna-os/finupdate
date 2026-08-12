//! Changelog / What's-New subsystem — SBOM diff state and the registry +
//! GitHub commit + SBOM fetch pipeline.
//!
//! Extracted from `status_view.rs` (finupdate#41, first step): `SbomStatus`
//! and `spawn_changelog_fetch` are the non-widget half of the changelog
//! feature (network fetch + message dispatch). The widget construction in
//! `rebuild_changelog_page` stays in `status_view` for the follow-up step.

use relm4::prelude::*;
use std::time::Instant;

use super::bootc_probe::{get_cached_bootc_status, read_selected_tag, strip_date_suffix};
use super::status_view::{StatusView, StatusViewInput};
use super::version_parse::parse_org_repo;
use crate::settings::Settings;

/// State of the SBOM diff fetch for the changelog page. Renders a different
/// section in `rebuild_changelog_page` for each value so the user sees
/// "comparing packages…" while we wait, instead of a silent gap that takes
/// 30+ seconds to fill in on a slow connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SbomStatus {
    /// Initial state — no changelog fetch has started yet.
    Pending,
    /// SBOM fetch is in flight (tokio task spawned).
    Loading,
    /// fetch_and_diff_sboms returned None — the registry didn't publish
    /// SPDX referrers for one of the images. Show a dim "not available"
    /// note instead of a spinner that never resolves.
    NotAvailable,
    /// Diff is loaded (stored in `sbom_diff`).
    Loaded,
}

pub fn spawn_changelog_fetch(
    registry_uri: String,
    selected_tag: String,
    sender: ComponentSender<StatusView>,
) {
    std::thread::spawn(move || {
        crate::runtime::block_on(async move {
            let total_start = std::time::Instant::now();
            println!(
                "[debug] changelog: starting fetch for registry_uri={}",
                registry_uri
            );

            // Build an ImageRef from registry_uri + selected_tag for the
            // service-layer calls. The stream-level tag (strip the date
            // suffix so e.g. "stable-daily-43.20260527" becomes
            // "stable-daily-43") drives both list_versions and
            // list_available_tags.
            let mut newest_full_ref: Option<String> = None;
            let parts: Vec<&str> = registry_uri.split('/').collect();
            if parts.len() >= 3 {
                let stream =
                    strip_date_suffix(&selected_tag).unwrap_or_else(|| selected_tag.clone());
                let image_ref = crate::service::ImageRef {
                    registry: parts[0].to_string(),
                    org: parts[1].to_string(),
                    image: parts[2..].join("/"),
                    tag: stream,
                    digest: String::new(),
                };
                let svc = crate::service::global();

                // Each network round-trip is timed independently so we can
                // tell which path is the bottleneck (per #48). Look for
                // [debug] changelog: phase= lines in stdout / RUST_LOG output.
                let t = std::time::Instant::now();
                match svc.list_available_tags(&image_ref).await {
                    Ok(available) if !available.is_empty() => {
                        println!(
                            "[debug] changelog: phase=list_available_tags ms={} count={}",
                            t.elapsed().as_millis(),
                            available.len()
                        );
                        let _ = sender
                            .input_sender()
                            .send(StatusViewInput::AvailableTagsLoaded(available));
                    }
                    Ok(_) => println!(
                        "[debug] changelog: phase=list_available_tags ms={} count=0",
                        t.elapsed().as_millis()
                    ),
                    Err(e) => println!(
                        "[debug] changelog: phase=list_available_tags ms={} err={}",
                        t.elapsed().as_millis(),
                        e
                    ),
                }

                let t = std::time::Instant::now();
                match svc.list_versions(&image_ref, 8).await {
                    Ok(versions) => {
                        println!(
                            "[debug] changelog: phase=list_versions ms={} count={}",
                            t.elapsed().as_millis(),
                            versions.len()
                        );
                        // versions are sorted ASCENDING by date — `.last()` is
                        // the newest. Using `.first()` here picked the oldest
                        // ref (e.g. Feb date-stamped tags that don't publish
                        // SPDX SBOMs), breaking the diff every time.
                        newest_full_ref = versions.last().map(|v| v.full_ref.clone());
                        let _ = sender
                            .input_sender()
                            .send(StatusViewInput::RegistryVersionsLoaded(versions));
                    }
                    Err(e) => println!(
                        "[debug] changelog: phase=list_versions ms={} err={}",
                        t.elapsed().as_millis(),
                        e
                    ),
                }
            }

            // 2. Fetch GitHub commits (with dates for fallback version building)
            let t_github = std::time::Instant::now();
            if let Some((org, repo)) = parse_org_repo(&registry_uri) {
                #[derive(serde::Deserialize)]
                struct GithubCommit {
                    sha: String,
                    commit: CommitDetails,
                }
                #[derive(serde::Deserialize)]
                struct CommitDetails {
                    message: String,
                    author: AuthorDetails,
                }
                #[derive(serde::Deserialize)]
                struct AuthorDetails {
                    name: String,
                    date: String,
                }

                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(10))
                    .user_agent("Finupdate/0.1.0")
                    .build()
                    .unwrap_or_default();

                let mut all_commits: Vec<(String, String, String, String)> = Vec::new();

                // Fetch commits from main repository
                let url = format!("https://api.github.com/repos/{}/{}/commits", org, repo);
                println!("[debug] changelog: phase=github_commits url={}", url);
                if let Ok(resp) = client.get(&url).send().await {
                    if let Ok(commits_json) = resp.json::<Vec<GithubCommit>>().await {
                        all_commits.extend(commits_json.into_iter().map(|c| {
                            (
                                c.sha,
                                c.commit.message,
                                c.commit.author.name,
                                c.commit.author.date,
                            )
                        }));
                    }
                }

                // Fetch commits from projectbluefin/common submodule (feature implementations)
                let common_url = "https://api.github.com/repos/projectbluefin/common/commits";
                println!(
                    "[debug] changelog: phase=github_commits_common url={}",
                    common_url
                );
                if let Ok(resp) = client.get(common_url).send().await {
                    if let Ok(commits_json) = resp.json::<Vec<GithubCommit>>().await {
                        // Only include feat: commits from common repo to avoid clutter
                        all_commits.extend(
                            commits_json
                                .into_iter()
                                .filter(|c| c.commit.message.starts_with("feat:"))
                                .map(|c| {
                                    (
                                        c.sha,
                                        c.commit.message,
                                        c.commit.author.name,
                                        c.commit.author.date,
                                    )
                                }),
                        );
                    }
                }

                // Sort to put feat: commits first, then others
                all_commits.sort_by(|a, b| {
                    let a_is_feat = a.1.starts_with("feat:");
                    let b_is_feat = b.1.starts_with("feat:");
                    match (a_is_feat, b_is_feat) {
                        (true, false) => std::cmp::Ordering::Less, // feat comes first
                        (false, true) => std::cmp::Ordering::Greater, // non-feat comes last
                        _ => std::cmp::Ordering::Equal,            // same priority, preserve order
                    }
                });

                println!(
                    "[debug] changelog: phase=github_commits ms={} count={}",
                    t_github.elapsed().as_millis(),
                    all_commits.len()
                );
                let _ = sender
                    .input_sender()
                    .send(StatusViewInput::GithubCommitsLoaded(all_commits));
            }
            println!(
                "[debug] changelog: phase=total ms={}",
                total_start.elapsed().as_millis()
            );

            // 3. Fetch and diff SBOMs — lazily, in a detached task. SPDX
            //    artifacts are MB-scale tarballs that parse to thousands of
            //    package entries; running this on the same critical-path
            //    thread as the home-page registry fetch was the freeze the
            //    user reported. With tokio::spawn the task survives this
            //    runtime's scope and only emits SbomDiffLoaded when the
            //    user has already seen commits + history rendered.
            //
            //    IMPORTANT: Do NOT use strip_date_suffix() refs here. If both
            //    booted and target resolve to the same floating tag (e.g. both
            //    "ghcr.io/projectbluefin/dakota:latest") the diff is trivially
            //    empty. Use the actual full_ref from the newest registry version,
            //    and the actual booted image's digest from bootc status.
            let settings = Settings::load();
            // Prefer the actual date-stamped full_ref of the newest build;
            // fall back to the stream tag only if we got no versions.
            let target_ref =
                newest_full_ref.unwrap_or_else(|| format!("{}:{}", registry_uri, selected_tag));

            // Get the booted image's actual digest-pinned ref from bootc
            // status or mock_identity so we compare two distinct manifests.
            let booted_ref = if let Some(ref mock) = settings.mock_identity {
                if let Some(ref digest) = mock.digest {
                    format!("{}/{}/{}@{}", mock.registry, mock.org, mock.image, digest)
                } else {
                    format!("{}/{}/{}:{}", mock.registry, mock.org, mock.image, mock.tag)
                }
            } else {
                get_cached_bootc_status()
                    .and_then(|json| {
                        let booted = json.pointer("/status/booted")?;
                        let img = booted
                            .pointer("/image/image/image")
                            .or_else(|| booted.pointer("/image/image"))
                            .and_then(|v| v.as_str())?;
                        let digest = booted
                            .pointer("/image/imageDigest")
                            .and_then(|v| v.as_str())?;
                        Some(format!("{}@{}", img, digest))
                    })
                    .unwrap_or_else(|| format!("{}:{}", registry_uri, read_selected_tag()))
            };

            if booted_ref != target_ref {
                // Tell the UI we're starting so it can render the
                // "Comparing packages…" placeholder. Without this the
                // Stack section is silently blank for 30+ seconds on
                // slow connections.
                let _ = sender.input_sender().send(StatusViewInput::SbomDiffStarted);
                let sbom_sender = sender.clone();
                tokio::spawn(async move {
                    tracing::debug!(
                        "sbom_diff: deferred fetch booted_ref={} target_ref={}",
                        booted_ref,
                        target_ref
                    );
                    match crate::sbom_diff::fetch_and_diff_sboms(booted_ref, target_ref).await {
                        Some(diff) => {
                            let _ = sbom_sender
                                .input_sender()
                                .send(StatusViewInput::SbomDiffLoaded(diff));
                        }
                        None => {
                            tracing::info!(
                                "sbom_diff: no diff available (registry didn't return SPDX referrers)"
                            );
                            let _ = sbom_sender
                                .input_sender()
                                .send(StatusViewInput::SbomDiffUnavailable);
                        }
                    }
                });
            } else {
                tracing::debug!("sbom_diff: skipped (booted == target, same image)");
            }
        });
    });
}
