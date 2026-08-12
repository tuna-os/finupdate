//! `bootc switch` progress parsing and the subprocess runner behind the
//! rebase dialog's progress page.
//!
//! Extracted from `rebase_dialog.rs` (finupdate#30): the pure line parser
//! (`parse_bootc_progress`) and the async subprocess plumbing
//! (`run_bootc_switch`) are the only parts of the dialog that deal with
//! bootc's unstable textual output, so they get their own module with the
//! parser's unit tests colocated. The dialog module stays focused on GTK UI
//! concerns.

/// One progress update parsed from a single `bootc switch` output line.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum BootcProgress {
    /// Layer-count fraction (e.g. "Layer 3/12" → 3/12 = 0.25).
    Fraction { current: u32, total: u32 },
    /// A human-readable status label to show under the progress bar
    /// (e.g. "Pulling…", "Importing…", "Staging deployment…").
    Status(String),
}

/// Parse a single line of `bootc switch` output for progress signal.
///
/// `bootc` doesn't have a stable machine-readable progress format, so this
/// is best-effort against the patterns we observe:
///   - `Layer N/M ...`            → Fraction
///   - `Pulled N/M layers`        → Fraction
///   - `N of M layers pulled`     → Fraction (alternative phrasing)
///   - lines starting with `Pulling` / `Fetching` / `Importing` /
///     `Staging` / `Writing`     → Status (so the page description tracks
///     the current high-level phase even when we can't extract a fraction)
///
/// Returns `None` for any line that doesn't match — the caller treats those
/// as "no signal, keep the bar pulsing" rather than letting them disrupt the
/// last-known progress.
fn parse_bootc_progress(line: &str) -> Option<BootcProgress> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Look for any "N/M" token where both sides are integers and N <= M.
    // bootc's output uses this for layer pulls. We don't require an explicit
    // "Layer" prefix because the wording has drifted across releases.
    for token in trimmed.split_ascii_whitespace() {
        if let Some((a, b)) = token.split_once('/') {
            if let (Ok(current), Ok(total)) = (a.parse::<u32>(), b.parse::<u32>()) {
                if total > 0 && current <= total && total <= 1_000 {
                    return Some(BootcProgress::Fraction { current, total });
                }
            }
        }
    }

    // Fall back to phase-label detection. First word match is enough.
    let first_word = trimmed
        .split_ascii_whitespace()
        .next()
        .unwrap_or("")
        .trim_end_matches(|c: char| !c.is_alphabetic());
    let phase = match first_word.to_ascii_lowercase().as_str() {
        "pulling" => Some("Pulling new image layers…"),
        "fetching" => Some("Fetching image…"),
        "importing" => Some("Importing layers…"),
        "staging" => Some("Staging deployment…"),
        "writing" => Some("Writing deployment…"),
        "deploying" => Some("Deploying…"),
        _ => None,
    };
    phase.map(|s| BootcProgress::Status(s.to_string()))
}

/// Spawn `bootc switch`, stream stdout+stderr line-by-line, parse progress,
/// and forward each parsed event to the caller via `progress_tx`. Returns the
/// final result.
///
/// Splitting capture+parse out of `run_rebase` keeps the UI plumbing (timeouts,
/// widget updates) free of subprocess details, and lets us unit-test
/// [`parse_bootc_progress`] separately.
async fn run_bootc_switch(
    full_ref: &str,
    progress_tx: tokio::sync::mpsc::UnboundedSender<BootcProgress>,
) -> Result<(), String> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    // The single most consequential command finupdate can issue — it changes
    // what the machine boots. Routed through the chokepoint so a dry run
    // records the exact target ref without performing the switch, which is what
    // makes "clicking Switch would run `bootc switch <the right ref>`"
    // assertable in the GUI suite.
    let settings = crate::settings::Settings::load();
    let suppressed =
        crate::action_journal::Suppressed::from_flags(settings.dev_mode, settings.dry_run);

    let mut cmd = match crate::privileged::privileged_async(
        "switch_image",
        serde_json::json!({ "target": full_ref }),
        &["bootc", "switch", full_ref],
        crate::privileged::Privilege::Pkexec,
        suppressed,
    ) {
        crate::privileged::ExecAsync::Suppressed => {
            // Report the same shape a real switch would: a completed staging
            // with no progress events. The caller then shows its success page.
            let _ = progress_tx.send(BootcProgress::Status(
                "Dry run — image switch recorded, not performed".to_string(),
            ));
            return Ok(());
        }
        crate::privileged::ExecAsync::Run(cmd) => cmd,
    };

    let mut child = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to spawn bootc switch: {}", e))?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let tx_out = progress_tx.clone();
    let stdout_task = tokio::spawn(async move {
        if let Some(stdout) = stdout {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(prog) = parse_bootc_progress(&line) {
                    let _ = tx_out.send(prog);
                }
            }
        }
    });
    let tx_err = progress_tx;
    let stderr_task = tokio::spawn(async move {
        if let Some(stderr) = stderr {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(prog) = parse_bootc_progress(&line) {
                    let _ = tx_err.send(prog);
                }
            }
        }
    });

    let status = child
        .wait()
        .await
        .map_err(|e| format!("Failed to wait for bootc switch: {}", e))?;
    let _ = stdout_task.await;
    let _ = stderr_task.await;

    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "bootc switch exited with code {}",
            status.code().unwrap_or(-1)
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_bootc_progress ─────────────────────────────────────────────
    // Pins the bootc-switch stdout/stderr parsing so the layer-by-layer
    // progress bar keeps working when bootc's output format drifts (or so we
    // notice when it does).

    #[test]
    fn progress_parses_layer_ratio() {
        let r = parse_bootc_progress("Layer 3/12 sha256:abc...");
        assert_eq!(
            r,
            Some(BootcProgress::Fraction {
                current: 3,
                total: 12
            })
        );
    }

    #[test]
    fn progress_parses_pulled_ratio_phrasing() {
        let r = parse_bootc_progress("Pulled 8/8 layers");
        assert_eq!(
            r,
            Some(BootcProgress::Fraction {
                current: 8,
                total: 8
            })
        );
    }

    #[test]
    fn progress_extracts_ratio_anywhere_in_line() {
        // Containers-image style: "Copying blob abc 5/12 (...) ETA 30s"
        let r = parse_bootc_progress("Copying blob abc 5/12 12.3 MiB / 30 MiB");
        assert_eq!(
            r,
            Some(BootcProgress::Fraction {
                current: 5,
                total: 12
            })
        );
    }

    #[test]
    fn progress_rejects_zero_total() {
        // 0/0 isn't a valid fraction; ignore so we don't divide by zero later.
        assert_eq!(parse_bootc_progress("0/0 something"), None);
    }

    #[test]
    fn progress_rejects_inverted_ratio() {
        // current > total is nonsense — likely matched something unrelated
        // like a file size; bail rather than show "150% complete".
        assert_eq!(parse_bootc_progress("Copying 100/5 something"), None);
    }

    #[test]
    fn progress_rejects_huge_denominator() {
        // Byte counts ("12345/67890 bytes") would set the bar bouncing all
        // over. Cap total at 1000 — no bootc image has that many layers.
        assert_eq!(parse_bootc_progress("123/45678 bytes"), None);
    }

    #[test]
    fn progress_returns_status_for_pulling_lines() {
        let r = parse_bootc_progress("Pulling manifest from ghcr.io/...");
        assert_eq!(
            r,
            Some(BootcProgress::Status(
                "Pulling new image layers…".to_string()
            ))
        );
    }

    #[test]
    fn progress_returns_status_for_staging_lines() {
        let r = parse_bootc_progress("Staging deployment for switch");
        assert_eq!(
            r,
            Some(BootcProgress::Status("Staging deployment…".to_string()))
        );
    }

    #[test]
    fn progress_prefers_fraction_over_status_when_both_match() {
        // "Pulling 4/8 layers" — first word matches a status, but the ratio
        // is the more useful signal; ensure we don't lose it.
        let r = parse_bootc_progress("Pulling 4/8 layers");
        assert_eq!(
            r,
            Some(BootcProgress::Fraction {
                current: 4,
                total: 8
            })
        );
    }

    #[test]
    fn progress_ignores_unrelated_lines() {
        assert_eq!(parse_bootc_progress(""), None);
        assert_eq!(parse_bootc_progress("   "), None);
        assert_eq!(parse_bootc_progress("info: starting bootc switch"), None);
    }
}
