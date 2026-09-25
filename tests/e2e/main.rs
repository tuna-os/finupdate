//! End-to-end coverage for finupdate's core user-facing flow: running
//! `finupdate-cli update` and letting it orchestrate the System / Flatpak /
//! Brew / Distrobox modules through `finupdate-runner`.
//!
//! Unlike `tests/cli_integration.rs` (which checks the CLI's stdout for a
//! single `--system-only` run), this suite drives the *whole* update
//! sequence end to end — real binary, real mocked subprocess tools on
//! `PATH`, real `finupdate-runner` script — and then inspects the action
//! journal file the run leaves behind on disk (`FINUPDATE_ACTION_JOURNAL`),
//! which is the persisted, machine-checkable record of what the tool
//! actually did. This is exactly the coverage
//! `finupdate-core/src/action_journal.rs` documents as living in
//! `tests/action_journal.rs` — spawning a real `finupdate-cli` process with
//! `FINUPDATE_ACTION_JOURNAL` set — just located under `tests/e2e/` so it
//! also satisfies the repo's E2E prerequisite.

use std::path::PathBuf;
use std::process::Command;

fn cli_exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_finupdate-cli"))
}

/// Isolated environment for one CLI run: its own `XDG_CONFIG_HOME` (so
/// `Settings::load/save` never touches a real user's config), its own
/// `bin/` directory of mock tools prepended to `PATH`, and its own action
/// journal path.
struct E2eEnv {
    _temp_dir: tempfile::TempDir,
    bin_dir: PathBuf,
    config_dir: PathBuf,
    journal_path: PathBuf,
}

impl E2eEnv {
    fn new() -> Self {
        let temp_dir = tempfile::tempdir().unwrap();
        let bin_dir = temp_dir.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let config_dir = temp_dir.path().join("config");
        std::fs::create_dir_all(&config_dir).unwrap();
        let journal_path = temp_dir.path().join("journal.jsonl");
        Self {
            _temp_dir: temp_dir,
            bin_dir,
            config_dir,
            journal_path,
        }
    }

    /// Drop a trivial mock tool (`bootc`, `flatpak`, `distrobox`, ...) onto
    /// `PATH` that just prints `stdout` and exits `0` — enough for the
    /// runner script's `command -v` probes to find it and treat the module
    /// as present, mirroring `tests/cli_integration.rs`'s `MockEnv`.
    fn create_mock_bin(&self, name: &str) {
        let path = self.bin_dir.join(name);
        std::fs::write(
            &path,
            format!("#!/bin/sh\necho '{name}: updated'\nexit 0\n"),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    fn path_var(&self) -> String {
        let original = std::env::var("PATH").unwrap_or_default();
        format!("{}:{}", self.bin_dir.display(), original)
    }

    fn runner_path(&self) -> PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("data")
            .join("finupdate-runner")
    }

    /// Run `finupdate-cli update [--system-only]` against this environment.
    fn run_update(&self, system_only: bool) -> std::process::Output {
        let mut cmd = Command::new(cli_exe());
        cmd.arg("update");
        if system_only {
            cmd.arg("--system-only");
        }
        cmd.env("PATH", self.path_var())
            .env("XDG_CONFIG_HOME", &self.config_dir)
            .env("FINUPDATE_ACTION_JOURNAL", &self.journal_path)
            .env("FINUPDATE_TEST_MOCK_RUNNER", self.runner_path());
        cmd.output().unwrap()
    }

    /// Parse the journal file into its JSONL records. Panics if the file is
    /// missing or contains invalid JSON — a real run always writes at least
    /// the `run_update` line.
    fn journal_records(&self) -> Vec<serde_json::Value> {
        let contents = std::fs::read_to_string(&self.journal_path)
            .expect("finupdate-cli should have written an action journal");
        contents
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).expect("journal line should be valid JSON"))
            .collect()
    }
}

/// The core "one-click update" flow: a full (non `--system-only`) update
/// walks all four modules end to end and leaves an accurate, on-disk
/// action-journal record of the run.
#[test]
fn e2e_full_update_orchestrates_all_modules_and_journals_intent() {
    let env = E2eEnv::new();
    env.create_mock_bin("bootc");
    env.create_mock_bin("flatpak");
    env.create_mock_bin("distrobox");
    // Deliberately no `brew` mock: the real user-facing behaviour when
    // Homebrew isn't installed is that the module is skipped, not that the
    // update fails.

    let output = env.run_update(false);

    assert!(
        output.status.success(),
        "update should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Starting update sequence..."));
    assert!(stdout.contains("=== MODULE STARTED: System ==="));
    assert!(stdout.contains("=== MODULE FINISHED: System (Success) ==="));
    assert!(stdout.contains("=== MODULE FINISHED: Flatpak (Success) ==="));
    assert!(stdout.contains("=== MODULE FINISHED: Brew (Skipped) ==="));
    assert!(stdout.contains("=== MODULE FINISHED: Distrobox (Success) ==="));
    assert!(stdout.contains("Update completed successfully!"));

    // The resulting file change: a journal entry proving this run really
    // intended to execute a full update (not a dry-run / system-only one),
    // and recording the exact argv finupdate would have run as root.
    let records = env.journal_records();
    let run_update = records
        .iter()
        .find(|r| r.get("action").and_then(|v| v.as_str()) == Some("run_update"))
        .expect("journal should contain a run_update record");
    assert_eq!(
        run_update
            .get("args")
            .and_then(|a| a.get("system_only"))
            .and_then(|v| v.as_bool()),
        Some(false)
    );
    assert_eq!(
        run_update.get("suppressed_by").and_then(|v| v.as_str()),
        Some("none")
    );
    let would_run: Vec<&str> = run_update
        .get("would_run")
        .and_then(|v| v.as_array())
        .expect("would_run should be a JSON array")
        .iter()
        .map(|v| v.as_str().expect("would_run entries should be strings"))
        .collect();
    assert_eq!(would_run, vec!["pkexec", "finupdate-runner"]);
}

/// The `--system-only` escape hatch: it must both change the CLI's visible
/// behaviour (app-layer modules skipped) *and* be reflected truthfully in
/// the persisted action journal, since that journal is what real-system
/// audits and GUI dry-run tests both rely on.
#[test]
fn e2e_system_only_update_skips_app_modules_and_journals_the_flag() {
    let env = E2eEnv::new();
    env.create_mock_bin("bootc");
    env.create_mock_bin("flatpak");
    env.create_mock_bin("distrobox");

    let output = env.run_update(true);

    assert!(
        output.status.success(),
        "system-only update should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("(system-only mode forced via CLI flag)"));
    assert!(stdout.contains("=== MODULE FINISHED: System (Success) ==="));
    assert!(stdout.contains("=== MODULE FINISHED: Flatpak (Skipped) ==="));
    assert!(stdout.contains("=== MODULE FINISHED: Brew (Skipped) ==="));
    assert!(stdout.contains("=== MODULE FINISHED: Distrobox (Skipped) ==="));

    let records = env.journal_records();
    let run_update = records
        .iter()
        .find(|r| r.get("action").and_then(|v| v.as_str()) == Some("run_update"))
        .expect("journal should contain a run_update record");
    assert_eq!(
        run_update
            .get("args")
            .and_then(|a| a.get("system_only"))
            .and_then(|v| v.as_bool()),
        Some(true)
    );
}
