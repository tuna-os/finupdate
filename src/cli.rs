//! Headless CLI for the finupdate backend.
//!
//! Exists as a separate `[[bin]]` to prove the UpdaterService abstraction
//! actually decouples the backend from the GTK frontend — this binary doesn't
//! link app/ui/dbus_progress/gpu, only the service trait + its dependencies.
//!
//! Subcommands:
//!   - `status`   — print the currently-booted image ref
//!   - `family`   — print the detected family + its switchable features
//!   - `versions` — list recent dated versions of the booted image
//!   - `tags`     — list tags published for the booted image's stream
//!
//! Honours the same precedence chain as the GUI for image detection:
//! `Settings::mock_identity` → `FINUPDATE_IMAGE` env → `bootc status` →
//! `/etc/os-release`. So `FINUPDATE_IMAGE=ghcr.io/ublue-os/aurora:stable
//! finupdate-cli versions` works without touching the host's bootc state.

// Backend modules now live in the `finupdate-core` crate; aliased here so the
// existing `crate::settings::…` paths keep resolving.
use finupdate_core::{
    action_journal, config, orchestrator, privileged, registry_client, runtime, sbom_diff, service,
    settings, update_worker, uupd_compat,
};

use std::process::ExitCode;

const USAGE: &str = "\
finupdate-cli — headless image queries via the UpdaterService trait.

Usage: finupdate-cli <command> [args]

Commands:
  status              Show the currently-booted image
  family              Show the detected family and available feature toggles
  versions            List recent dated versions for the booted image
  tags                List published tags for the booted image's stream
  changelog [tag]     Print recent GitHub commits + SBOM package diff
                      between the booted image and the named tag (defaults
                      to the booted tag — useful for previewing what an
                      Install would actually change).
  timer [cmd]         Show uupd.timer status. Command can be 'enable' or
                      'disable' to modify timer status.
  update              Run the update process and stream progress live.
                      Pass '--system-only' to only update system image.
  help                Print this help

Environment:
  FINUPDATE_IMAGE   Override detected image (registry/org/image:tag)
";

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    service::init(service::BootcUpdaterService::new());

    let cmd = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "help".to_string());
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    match cmd.as_str() {
        "help" | "-h" | "--help" => {
            print!("{}", USAGE);
            ExitCode::SUCCESS
        }
        "status" => rt.block_on(cmd_status()),
        "family" => rt.block_on(cmd_family()),
        "versions" => {
            let count = std::env::args()
                .nth(2)
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(8);
            rt.block_on(cmd_versions(count))
        }
        "tags" => rt.block_on(cmd_tags()),
        "changelog" => {
            let target_tag = std::env::args().nth(2);
            rt.block_on(cmd_changelog(target_tag))
        }
        "timer" => {
            let arg = std::env::args().nth(2);
            rt.block_on(cmd_timer(arg))
        }
        "update" => {
            let system_only = std::env::args().any(|x| x == "--system-only");
            rt.block_on(cmd_update(system_only))
        }
        other => {
            eprintln!("finupdate-cli: unknown command '{}'\n", other);
            eprint!("{}", USAGE);
            ExitCode::from(2)
        }
    }
}

async fn cmd_status() -> ExitCode {
    match service::global().current_image().await {
        Ok(img) => {
            println!("{}", img);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("finupdate-cli: no booted image detected: {}", e);
            ExitCode::FAILURE
        }
    }
}

async fn cmd_family() -> ExitCode {
    match service::global().current_family().await {
        Ok(Some(fam)) => {
            println!("family: {}", fam.name);
            println!("base:   {}", fam.base_image);
            if fam.features.is_empty() {
                println!("features: (none)");
            } else {
                println!("features:");
                for f in &fam.features {
                    println!("  - {} ({})", f.id, f.display_name);
                }
            }
            ExitCode::SUCCESS
        }
        Ok(None) => {
            eprintln!("finupdate-cli: booted image is not in KNOWN_FAMILIES");
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("finupdate-cli: {}", e);
            ExitCode::FAILURE
        }
    }
}

/// Resolve a user-supplied target tag against the live registry.
///
/// Returns `(actual_tag, Some(original))` if the original was translated
/// (e.g. YYYYMMDD → sha-tag), or `(original, None)` if it was used as-is.
///
/// Triggers a `list_versions(120)` probe so date-only inputs can be matched
/// against the sha-tag config-blob `created` timestamps the GUI uses.
async fn resolve_target_tag(
    svc: &dyn service::UpdaterService,
    booted: &service::ImageRef,
    raw: &str,
) -> (String, Option<String>) {
    // Heuristic: 8-digit YYYYMMDD → date lookup. Anything else is taken
    // literally unless the lookup happens to find a matching version.
    let parsed_date = if raw.len() == 8 && raw.chars().all(|c| c.is_ascii_digit()) {
        chrono::NaiveDate::parse_from_str(raw, "%Y%m%d").ok()
    } else {
        None
    };
    let Some(target_date) = parsed_date else {
        return (raw.to_string(), None);
    };

    match svc.list_versions(booted, 120).await {
        Ok(versions) => match versions.iter().find(|v| v.date == target_date) {
            // `full_ref` is the actual `registry/org/image:tag` so extract
            // just the tag suffix. `version` is a synthetic date string for
            // sha-tagged builds and won't resolve as a manifest.
            Some(v) => {
                let tag = v
                    .full_ref
                    .rsplit_once(':')
                    .map(|(_, t)| t.to_string())
                    .unwrap_or_else(|| v.version.clone());
                (tag, Some(raw.to_string()))
            }
            None => {
                eprintln!(
                    "finupdate-cli: no published image found for date {} — using {} literally",
                    target_date, raw
                );
                (raw.to_string(), None)
            }
        },
        Err(e) => {
            eprintln!(
                "finupdate-cli: list_versions failed ({}) — using {} literally",
                e, raw
            );
            (raw.to_string(), None)
        }
    }
}

async fn cmd_versions(count: usize) -> ExitCode {
    let svc = service::global();
    let image = match svc.current_image().await {
        Ok(i) => i,
        Err(e) => {
            eprintln!("finupdate-cli: {}", e);
            return ExitCode::FAILURE;
        }
    };
    match svc.list_versions(&image, count).await {
        Ok(versions) if versions.is_empty() => {
            eprintln!("finupdate-cli: no versions found for {}", image);
            ExitCode::FAILURE
        }
        Ok(versions) => {
            for v in versions {
                println!(
                    "{}  {}  kernel={}",
                    v.date.format("%Y-%m-%d"),
                    v.version,
                    if v.kernel.is_empty() { "?" } else { &v.kernel }
                );
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("finupdate-cli: {}", e);
            ExitCode::FAILURE
        }
    }
}

async fn cmd_tags() -> ExitCode {
    let svc = service::global();
    let image = match svc.current_image().await {
        Ok(i) => i,
        Err(e) => {
            eprintln!("finupdate-cli: {}", e);
            return ExitCode::FAILURE;
        }
    };
    match svc.list_available_tags(&image).await {
        Ok(tags) => {
            // Each entry is a (display, raw) pair — print both when they
            // differ (sha tags get a "Build YYYY-MM-DD" display label) so
            // users can pipe the raw column for further automation.
            for t in tags {
                if t.display == t.raw {
                    println!("{}", t.raw);
                } else {
                    println!("{}\t{}", t.raw, t.display);
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("finupdate-cli: {}", e);
            ExitCode::FAILURE
        }
    }
}

async fn cmd_changelog(target_tag: Option<String>) -> ExitCode {
    let svc = service::global();
    let booted = match svc.current_image().await {
        Ok(i) => i,
        Err(e) => {
            eprintln!("finupdate-cli: {}", e);
            return ExitCode::FAILURE;
        }
    };
    let raw_target = target_tag.unwrap_or_else(|| booted.tag.clone());

    // Mirror the GUI's resolution: if the user passed an 8-digit YYYYMMDD or
    // any non-existent literal tag, resolve it via list_versions which probes
    // sha-tag config-blob `created` timestamps. This way `changelog 20260530`
    // works even though Dakota stopped publishing date-stamped tags after
    // Feb 2026 — we map the date to whatever sha-tag was built that day.
    let (target_tag, resolved_via) = resolve_target_tag(&*svc, &booted, &raw_target).await;

    println!(
        "Changelog for {}/{}/{}:",
        booted.registry, booted.org, booted.image
    );
    println!("  booted tag: {}", booted.tag);
    if let Some(via) = resolved_via {
        println!("  target tag: {} (resolved from {})", target_tag, via);
    } else {
        println!("  target tag: {}", target_tag);
    }
    println!();

    // GitHub commits (same source the GUI's "What's new" page uses). Recent
    // 30; users pipe through head if they want fewer.
    let url = format!(
        "https://api.github.com/repos/{}/{}/commits",
        booted.org, booted.image
    );
    println!("== Recent commits ({}) ==", url);
    match fetch_recent_commits(&url).await {
        Ok(commits) if !commits.is_empty() => {
            for (sha, message, author) in commits.iter().take(30) {
                let short = &sha[..sha.len().min(8)];
                let summary = message.lines().next().unwrap_or("");
                println!("  {}  {}  ({})", short, summary, author);
            }
        }
        Ok(_) => println!("  (no commits returned)"),
        Err(e) => println!("  (fetch failed: {})", e),
    }
    println!();

    // SBOM diff. Skip if the booted and target tags are identical — there's
    // nothing to compare.
    if booted.tag == target_tag {
        println!("== SBOM diff ==");
        println!("  (booted == target; no diff to compute)");
        return ExitCode::SUCCESS;
    }
    let booted_ref = format!(
        "{}/{}/{}:{}",
        booted.registry, booted.org, booted.image, booted.tag
    );
    let target_ref = format!(
        "{}/{}/{}:{}",
        booted.registry, booted.org, booted.image, target_tag
    );
    println!("== SBOM package diff ({} → {}) ==", booted_ref, target_ref);
    match sbom_diff::fetch_and_diff_sboms(booted_ref, target_ref).await {
        Some(diff) => {
            if diff.upgraded.is_empty() && diff.added.is_empty() && diff.removed.is_empty() {
                println!("  (no package changes)");
            } else {
                if !diff.upgraded.is_empty() {
                    println!("  Upgraded ({}):", diff.upgraded.len());
                    for p in &diff.upgraded {
                        println!("    {}  {} → {}", p.name, p.old_version, p.new_version);
                    }
                }
                if !diff.added.is_empty() {
                    println!("  Added ({}):", diff.added.len());
                    for p in &diff.added {
                        println!("    {}  {}", p.name, p.new_version);
                    }
                }
                if !diff.removed.is_empty() {
                    println!("  Removed ({}):", diff.removed.len());
                    for name in &diff.removed {
                        println!("    {}", name);
                    }
                }
            }
        }
        None => println!("  (couldn't fetch SBOMs — neither image may publish SPDX referrers)"),
    }
    ExitCode::SUCCESS
}

async fn fetch_recent_commits(url: &str) -> Result<Vec<(String, String, String)>, reqwest::Error> {
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
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent("finupdate-cli/0.1.0")
        .build()
        .unwrap_or_default();
    let commits_json: Vec<GithubCommit> = client.get(url).send().await?.json().await?;
    Ok(commits_json
        .into_iter()
        .map(|c| (c.sha, c.commit.message, c.commit.author.name))
        .collect())
}

async fn cmd_timer(action: Option<String>) -> ExitCode {
    if let Some(act) = action {
        let enable = match act.as_str() {
            "enable" => true,
            "disable" => false,
            other => {
                eprintln!(
                    "finupdate-cli: unknown timer action '{}' (expected 'enable' or 'disable')",
                    other
                );
                return ExitCode::from(2);
            }
        };
        println!(
            "Transitioning uupd.timer to: {}...",
            if enable { "enabled" } else { "disabled" }
        );
        match uupd_compat::set_uupd_timer(enable).await {
            Ok(_) => {
                println!("Successfully configured uupd.timer.");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("Failed to configure uupd.timer: {}", e);
                ExitCode::FAILURE
            }
        }
    } else {
        println!("uupd daemon state:");
        println!("  installed: {}", uupd_compat::is_uupd_installed());
        match uupd_compat::is_uupd_timer_active() {
            Some(true) => println!("  timer:     enabled"),
            Some(false) => println!("  timer:     disabled"),
            None => println!("  timer:     unknown / unmanaged"),
        }
        ExitCode::SUCCESS
    }
}

async fn cmd_update(system_only: bool) -> ExitCode {
    println!("Starting update sequence...");

    let mut original_settings = None;
    if system_only {
        println!("  (system-only mode forced via CLI flag)");
        let mut settings = settings::Settings::load();
        original_settings = Some(settings.clone());
        settings.include_app_updates = false;
        settings.save();
    }

    let (_tx_cancel, rx_cancel) = tokio::sync::oneshot::channel();
    let mut rx = orchestrator::run(rx_cancel).await;

    let mut exit_code = ExitCode::SUCCESS;
    while let Some(event) = rx.recv().await {
        match event {
            update_worker::UpdateEvent::Output(line) => {
                println!("  [output] {}", line);
            }
            update_worker::UpdateEvent::ModuleStarted(module) => {
                println!("=== MODULE STARTED: {:?} ===", module);
            }
            update_worker::UpdateEvent::ModuleFinished(module, status) => {
                println!("=== MODULE FINISHED: {:?} ({:?}) ===", module, status);
            }
            update_worker::UpdateEvent::Complete => {
                println!("Update completed successfully!");
            }
            update_worker::UpdateEvent::UpToDate => {
                println!("System is already up to date.");
            }
            update_worker::UpdateEvent::Error(err) => {
                eprintln!("Error during update: {}", err);
                exit_code = ExitCode::FAILURE;
            }
        }
    }

    if let Some(orig) = original_settings {
        orig.save();
    }

    exit_code
}
