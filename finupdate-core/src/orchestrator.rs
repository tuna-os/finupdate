// finupdate-cli compiles every module the GUI binary does but only
// invokes a subset — the unused functions/enums here trip dead-code
// warnings under that bin even though the GUI bin uses them. Silence
// the noise at the module level.
#![allow(dead_code)]

//! Pure-Rust update orchestrator — replaces the host `uupd` binary.
//!
//! Invokes `finupdate-runner` (a small shell script; bundled at
//! `/app/bin/` for native execution, and expected at a fixed host path --
//! `/usr/bin/` or `/usr/libexec/` -- when running from inside a Flatpak,
//! since the host's `pkexec` cannot see anything under the sandbox's `/app/`)
//! via a single `pkexec` elevation, then parses structured marker lines from
//! its stdout to emit `ModuleStarted` / `ModuleFinished` events alongside the
//! raw output lines.
//!
//! ## Marker protocol (from finupdate-runner)
//!
//! ```text
//! ===MODULE:system===          → ModuleStarted(System)
//! ===MODULE:system:done:0===   → ModuleFinished(System, Success)
//! ===MODULE:system:done:77===  → ModuleFinished(System, UpToDate)
//! ===MODULE:system:done:1===   → ModuleFinished(System, Failed(1))
//! ===DONE===                   → all modules finished
//! ```
//!
//! All other lines are forwarded as `UpdateEvent::Output`.

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

use crate::update_worker::{UpdateEvent, is_flatpak};

/// The four update modules, in execution order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Module {
    System,
    Flatpak,
    Brew,
    Distrobox,
}

impl Module {
    pub fn key(&self) -> &'static str {
        match self {
            Module::System => "system",
            Module::Flatpak => "flatpak",
            Module::Brew => "brew",
            Module::Distrobox => "distrobox",
        }
    }

    fn from_key(s: &str) -> Option<Self> {
        match s {
            "system" => Some(Module::System),
            "flatpak" => Some(Module::Flatpak),
            "brew" => Some(Module::Brew),
            "distrobox" => Some(Module::Distrobox),
            _ => None,
        }
    }
}

/// Per-module completion status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModuleStatus {
    /// Module completed successfully.
    Success,
    /// Module found nothing to update (exit 77).
    UpToDate,
    /// Module exited with a non-zero, non-77 code.
    Failed(i32),
    /// Module was skipped (tool not present on host).
    Skipped,
}

/// Run the real update via `finupdate-runner`, streaming events to the returned channel.
///
/// A single `pkexec` elevation covers all modules. `cancel_rx` kills the child
/// process if the user cancels.
pub async fn run(
    cancel_rx: tokio::sync::oneshot::Receiver<()>,
) -> mpsc::UnboundedReceiver<UpdateEvent> {
    let (tx, rx) = mpsc::unbounded_channel();

    tokio::spawn(async move {
        // Honour the user's "Include app updates" toggle (Preferences →
        // Updates group). When OFF, the runner script skips flatpak / brew /
        // distrobox and only refreshes the bootc system image.
        let include_app_updates = crate::settings::Settings::load().include_app_updates;

        // Record the real update run. This path is only reached when neither
        // dev_mode nor dry_run is set — both route to the simulator in
        // `app.rs` before reaching here — so it is always an unsuppressed
        // execution. Journalling it anyway means a journal captured against a
        // live system shows the complete action sequence, not just the
        // withheld ones.
        crate::action_journal::record_str(
            "run_update",
            serde_json::json!({ "system_only": !include_app_updates }),
            &["pkexec", "finupdate-runner"],
            crate::action_journal::Suppressed::No,
        );

        let mut cmd = build_runner_command(!include_app_updates);
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(UpdateEvent::Error(format!(
                    "Failed to start finupdate-runner: {e}"
                )));
                return;
            }
        };

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        // Stream stderr as plain output lines.
        let tx_err = tx.clone();
        let stderr_task = stderr.map(|s| {
            tokio::spawn(async move {
                let mut lines = BufReader::new(s).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if tx_err.send(UpdateEvent::Output(line)).is_err() {
                        break;
                    }
                }
            })
        });

        // Stream stdout, parsing marker lines into structured events.
        let tx_out = tx.clone();
        let stdout_future = async move {
            if let Some(stdout) = stdout {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let send_result = match parse_line(&line) {
                        ParsedLine::Event(ev) => tx_out.send(ev),
                        ParsedLine::Consumed => continue,
                        ParsedLine::Plain => tx_out.send(UpdateEvent::Output(line)),
                    };
                    if send_result.is_err() {
                        break;
                    }
                }
            }
        };

        let cancelled = tokio::select! {
            _ = stdout_future => false,
            _ = cancel_rx => true,
        };

        if let Some(task) = stderr_task {
            task.abort();
            let _ = task.await;
        }

        if cancelled {
            let _ = child.kill().await;
            let _ = child.wait().await;
            let _ = tx.send(UpdateEvent::Error("Update cancelled by user".to_string()));
            return;
        }

        match child.wait().await {
            Ok(status) if status.success() => {
                let _ = tx.send(UpdateEvent::Complete);
            }
            Ok(status) if status.code() == Some(77) => {
                let _ = tx.send(UpdateEvent::UpToDate);
            }
            Ok(status) => {
                let code = status.code().unwrap_or(-1);
                let _ = tx.send(UpdateEvent::Error(format!(
                    "Update process exited with code {code}"
                )));
            }
            Err(e) => {
                let _ = tx.send(UpdateEvent::Error(format!(
                    "Error waiting for update process: {e}"
                )));
            }
        }
    });

    rx
}

/// Renders the host-side shell driver Flatpak mode hands to `flatpak-spawn
/// --host sh -c`. Tries the two polkit-allowlisted `finupdate-runner`
/// locations in order and `exec pkexec`s the first one that exists and is
/// executable; never touches `/tmp` or any other mutable, unauthorized path.
/// Split out so the exact-path selection can be tested without spawning
/// `flatpak-spawn`/`pkexec` (tuna-os/finupdate#124).
fn flatpak_runner_driver(env_prefix: &str) -> String {
    format!(
        r#"
for p in /usr/bin/finupdate-runner /usr/libexec/finupdate-runner; do
  if [ -x "$p" ]; then
    exec pkexec {env_prefix}"$p"
  fi
done
echo "finupdate: no host-installed finupdate-runner at /usr/bin or /usr/libexec -- install the finupdate native package (see INSTALL.md)" >&2
exit 127
"#
    )
}

/// Build the command that invokes `finupdate-runner` with a single pkexec.
///
/// Native builds run the copy meson installed on `PATH`. Flatpak mode runs
/// the copy the host packaging installed at a fixed, polkit-allowlisted
/// path (`/usr/bin/finupdate-runner` or `/usr/libexec/finupdate-runner`) --
/// never the sandbox's own bundled copy under `/app/bin/`, which the host's
/// pkexec cannot see and which motivated an earlier, less safe workaround
/// (see the comment inside the `is_flatpak()` branch).
fn build_runner_command(system_only: bool) -> Command {
    if let Ok(mock_path) = std::env::var("FINUPDATE_TEST_MOCK_RUNNER") {
        let mut cmd = Command::new(mock_path);
        // Ensure the test runner inherits the parent's environment (especially PATH)
        // so it can find mock binaries.
        for (key, value) in std::env::vars() {
            cmd.env(&key, &value);
        }
        if system_only {
            cmd.env("FINUPDATE_SYSTEM_ONLY", "1");
        }
        return cmd;
    }

    // When system_only is true, the runner script skips flatpak/brew/distrobox
    // and only refreshes the bootc image. pkexec strips most env vars by
    // default, so we pass FINUPDATE_SYSTEM_ONLY *through* pkexec via the
    // `env KEY=VAL CMD` idiom rather than relying on env inheritance.
    let env_prefix = if system_only {
        "env FINUPDATE_SYSTEM_ONLY=1 "
    } else {
        ""
    };

    if is_flatpak() {
        // `flatpak-spawn --host pkexec /app/bin/finupdate-runner` fails with
        // exit 127: `/app/` is a sandbox-internal path invisible to the
        // host's pkexec. A prior fix worked around that by staging the
        // bundled script body to a fresh /tmp file and pkexec-ing *that* --
        // but the shipped polkit rule authorizes only the exact paths
        // `/usr/bin/finupdate-runner` and `/usr/libexec/finupdate-runner`
        // (build-aux/49-finupdate.polkit.rules), so a generated /tmp path
        // falls outside the allowlist, and the file stays owned and
        // writable by the unprivileged caller between creation and
        // pkexec's exec -- a TOCTOU window an unprivileged process sharing
        // that user session could race (tuna-os/finupdate#124).
        //
        // Fix: invoke one of the two allowlisted host-owned paths directly,
        // matching the native (non-Flatpak) branch below, which already
        // relies on a host-installed `finupdate-runner`. The meson install
        // (data/meson.build) puts it in `bindir`, i.e. `/usr/bin` on a
        // standard prefix; `/usr/libexec` is kept as an allowlisted
        // fallback for a libexec-convention packaging. The wrapping shell
        // only *selects* which fixed path to hand to pkexec -- it never
        // constructs or runs anything itself as root, and polkit's exact-
        // path check applies to whichever literal path `exec pkexec` is
        // called with, so the authorized surface is still exactly the two
        // allowlisted paths.
        let mut cmd = Command::new("flatpak-spawn");
        cmd.arg("--host");
        cmd.arg("sh")
            .arg("-c")
            .arg(flatpak_runner_driver(env_prefix));
        cmd
    } else {
        // Native build / dev: PATH lookup. `cargo install --path .` or the
        // meson install both put `finupdate-runner` on PATH.
        let mut cmd = Command::new("pkexec");
        if system_only {
            cmd.arg("env").arg("FINUPDATE_SYSTEM_ONLY=1");
        }
        cmd.arg("finupdate-runner");
        cmd
    }
}

/// Result of parsing a stdout line from finupdate-runner.
enum ParsedLine {
    /// A structured event to forward to the UI.
    Event(UpdateEvent),
    /// A marker line we consumed but doesn't map to a UI event (e.g. ===DONE===).
    Consumed,
    /// An ordinary log line; forward as Output.
    Plain,
}

fn parse_line(line: &str) -> ParsedLine {
    let Some(inner) = line.strip_prefix("===").and_then(|s| s.strip_suffix("===")) else {
        return ParsedLine::Plain;
    };

    if inner == "DONE" {
        return ParsedLine::Consumed;
    }

    let parts: Vec<&str> = inner.split(':').collect();
    match parts.as_slice() {
        ["MODULE", key] => match Module::from_key(key) {
            Some(m) => ParsedLine::Event(UpdateEvent::ModuleStarted(m)),
            None => ParsedLine::Plain,
        },
        ["MODULE", key, "done", code_str] => match Module::from_key(key) {
            Some(m) => {
                let code: i32 = code_str.parse().unwrap_or(-1);
                let status = match code {
                    0 => ModuleStatus::Success,
                    77 => ModuleStatus::UpToDate,
                    _ => ModuleStatus::Failed(code),
                };
                ParsedLine::Event(UpdateEvent::ModuleFinished(m, status))
            }
            None => ParsedLine::Plain,
        },
        ["MODULE", key, "skipped"] => match Module::from_key(key) {
            Some(m) => ParsedLine::Event(UpdateEvent::ModuleFinished(m, ModuleStatus::Skipped)),
            None => ParsedLine::Plain,
        },
        _ => ParsedLine::Plain,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Shared with uupd_compat's tests: both mutate the process-global PATH to
    // install mock binaries, so a module-local mutex cannot serialise them
    // against each other. Using two separate mutexes made
    // `test_is_uupd_installed` flake about one run in three.
    use crate::test_support::env_lock;

    fn expect_event(line: &str) -> UpdateEvent {
        match parse_line(line) {
            ParsedLine::Event(e) => e,
            ParsedLine::Consumed => panic!("expected Event, got Consumed for {line:?}"),
            ParsedLine::Plain => panic!("expected Event, got Plain for {line:?}"),
        }
    }

    #[test]
    fn module_keys_round_trip() {
        for m in [
            Module::System,
            Module::Flatpak,
            Module::Brew,
            Module::Distrobox,
        ] {
            assert_eq!(Module::from_key(m.key()), Some(m));
        }
    }

    #[test]
    fn module_from_unknown_key_is_none() {
        assert_eq!(Module::from_key("nothing"), None);
        assert_eq!(Module::from_key(""), None);
        assert_eq!(Module::from_key("SYSTEM"), None);
    }

    #[test]
    fn flatpak_driver_never_mentions_tmp() {
        // Regression for tuna-os/finupdate#124: the driver must only ever
        // name the two polkit-allowlisted host paths, never stage anything
        // under /tmp (which is outside the exact-path allowlist and would
        // be writable by the caller between creation and pkexec's exec).
        let driver = flatpak_runner_driver("");
        assert!(
            !driver.contains("/tmp"),
            "driver must not reference /tmp: {driver}"
        );
        assert!(
            !driver.contains("mktemp"),
            "driver must not create a temp file: {driver}"
        );
        assert!(driver.contains("/usr/bin/finupdate-runner"));
        assert!(driver.contains("/usr/libexec/finupdate-runner"));
        // Exactly the fixed path is handed to pkexec, not a variable script
        // body -- "$p" is one of the two loop-bound literals above, never
        // caller-controlled content.
        assert!(driver.contains(r#"exec pkexec "$p""#));
    }

    #[test]
    fn flatpak_driver_carries_system_only_env_prefix() {
        let driver = flatpak_runner_driver("env FINUPDATE_SYSTEM_ONLY=1 ");
        assert!(driver.contains(r#"exec pkexec env FINUPDATE_SYSTEM_ONLY=1 "$p""#));
    }

    #[tokio::test]
    async fn flatpak_driver_execs_first_existing_path() {
        // End-to-end proof the shell driver actually selects and execs a
        // real allowlisted-shaped path rather than falling back to /tmp:
        // run it under `sh -c` with PATH-mocked `pkexec`, honoring only the
        // second of the two candidate paths existing (as if only the
        // libexec-convention package were installed).
        let _lock = env_lock().lock().await;
        let env = MockEnv::new();
        let pkexec = env.create_mock_bin("pkexec", 0);

        let runner_dir = tempfile::tempdir().unwrap();
        let runner_path = runner_dir.path().join("finupdate-runner");
        std::fs::write(&runner_path, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&runner_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        // Substitute the driver's fixed candidates with our tempdir path so
        // the test never needs real root-owned /usr paths.
        let driver = flatpak_runner_driver("")
            .replace("/usr/bin/finupdate-runner", "/nonexistent/finupdate-runner")
            .replace(
                "/usr/libexec/finupdate-runner",
                runner_path.to_str().unwrap(),
            );

        let original_path = std::env::var("PATH").unwrap_or_default();
        let new_path = format!("{}:{}", env.bin_dir.display(), original_path);
        unsafe {
            std::env::set_var("PATH", &new_path);
        }

        let output = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(&driver)
            .output()
            .await
            .unwrap();

        unsafe {
            std::env::set_var("PATH", &original_path);
        }

        assert!(output.status.success(), "driver did not exit 0: {output:?}");
        let invocations = env.read_invocations();
        assert!(
            invocations.contains(&format!(
                "pkexec called with args: {}",
                runner_path.display()
            )),
            "pkexec was not invoked with the existing candidate path: {invocations}"
        );
        let _ = pkexec;
    }

    #[test]
    fn parses_module_started_for_each_module() {
        let cases = [
            ("===MODULE:system===", Module::System),
            ("===MODULE:flatpak===", Module::Flatpak),
            ("===MODULE:brew===", Module::Brew),
            ("===MODULE:distrobox===", Module::Distrobox),
        ];
        for (line, expected) in cases {
            match expect_event(line) {
                UpdateEvent::ModuleStarted(m) => assert_eq!(m, expected),
                other => panic!("expected ModuleStarted({expected:?}) got {other:?}"),
            }
        }
    }

    #[test]
    fn parses_done_zero_as_success() {
        match expect_event("===MODULE:system:done:0===") {
            UpdateEvent::ModuleFinished(Module::System, ModuleStatus::Success) => {}
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn parses_done_seventyseven_as_uptodate() {
        match expect_event("===MODULE:flatpak:done:77===") {
            UpdateEvent::ModuleFinished(Module::Flatpak, ModuleStatus::UpToDate) => {}
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn parses_done_nonzero_as_failed() {
        match expect_event("===MODULE:brew:done:1===") {
            UpdateEvent::ModuleFinished(Module::Brew, ModuleStatus::Failed(1)) => {}
            other => panic!("got {other:?}"),
        }
        match expect_event("===MODULE:brew:done:127===") {
            UpdateEvent::ModuleFinished(Module::Brew, ModuleStatus::Failed(127)) => {}
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn parses_skipped_marker() {
        match expect_event("===MODULE:distrobox:skipped===") {
            UpdateEvent::ModuleFinished(Module::Distrobox, ModuleStatus::Skipped) => {}
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn done_marker_is_consumed_silently() {
        assert!(matches!(parse_line("===DONE==="), ParsedLine::Consumed));
    }

    #[test]
    fn plain_lines_are_passed_through() {
        for line in [
            "regular log output",
            "",
            "===not a real marker",
            "===MODULE:===",                // empty key
            "===MODULE:unknown===",         // unknown module
            "===MODULE:system:done:===",    // missing code
            "===MODULE:system:done:abc===", // non-numeric code (we parse_or(-1) → Failed, but spelling matches the shape so it actually becomes Failed(-1))
        ] {
            // Lines that don't match the marker shape at all are Plain.
            // The "non-numeric code" line is intentionally ambiguous — it matches
            // the shape, parses to -1 via unwrap_or, and is treated as Failed(-1).
            // That's acceptable behavior; only the explicit shape mismatches
            // should round-trip as Plain.
            let _ = parse_line(line);
        }

        assert!(matches!(
            parse_line("regular log output"),
            ParsedLine::Plain
        ));
        assert!(matches!(parse_line(""), ParsedLine::Plain));
        assert!(matches!(
            parse_line("===MODULE:unknown==="),
            ParsedLine::Plain
        ));
    }

    #[test]
    fn unparseable_done_code_falls_through_to_failed_minus_one() {
        // Defensive: the runner should always emit a numeric code, but if it
        // doesn't, we still mark the module finished rather than dropping the event.
        match expect_event("===MODULE:system:done:garbage===") {
            UpdateEvent::ModuleFinished(Module::System, ModuleStatus::Failed(-1)) => {}
            other => panic!("got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_orchestrator_integration_with_mock_process() {
        let _lock = env_lock().lock().await;
        use std::io::Write;
        let mut mock_script = tempfile::NamedTempFile::new().unwrap();
        writeln!(
            mock_script,
            "#!/bin/sh\n\
             echo '===MODULE:system==='\n\
             echo 'System output line 1'\n\
             echo '===MODULE:system:done:0==='\n\
             echo '===MODULE:flatpak==='\n\
             echo '===MODULE:flatpak:done:77==='\n\
             echo '===DONE==='\n\
             exit 0"
        )
        .unwrap();

        let temp_path = mock_script.into_temp_path();
        let path = temp_path.to_path_buf();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        unsafe {
            std::env::set_var("FINUPDATE_TEST_MOCK_RUNNER", &path);
        }

        let (_cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
        let mut rx = run(cancel_rx).await;

        let mut events = vec![];
        while let Some(ev) = rx.recv().await {
            events.push(ev);
        }

        unsafe {
            std::env::remove_var("FINUPDATE_TEST_MOCK_RUNNER");
        }

        assert!(!events.is_empty());

        let mut has_started = false;
        let mut has_output = false;
        let mut has_finished = false;
        let mut has_complete = false;

        for ev in events {
            match ev {
                UpdateEvent::ModuleStarted(Module::System) => has_started = true,
                UpdateEvent::Output(ref line) if line == "System output line 1" => {
                    has_output = true
                }
                UpdateEvent::ModuleFinished(Module::System, ModuleStatus::Success) => {
                    has_finished = true
                }
                UpdateEvent::Complete => has_complete = true,
                _ => {}
            }
        }

        assert!(has_started, "Missing ModuleStarted(System)");
        assert!(has_output, "Missing System output line 1");
        assert!(has_finished, "Missing ModuleFinished(System, Success)");
        assert!(has_complete, "Missing Complete");
    }

    #[tokio::test]
    async fn test_orchestrator_integration_cancellation() {
        let _lock = env_lock().lock().await;
        use std::io::Write;
        let mut mock_script = tempfile::NamedTempFile::new().unwrap();
        writeln!(
            mock_script,
            "#!/bin/sh\n\
             echo '===MODULE:system==='\n\
             sleep 10\n\
             exit 0"
        )
        .unwrap();

        let temp_path = mock_script.into_temp_path();
        let path = temp_path.to_path_buf();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        unsafe {
            std::env::set_var("FINUPDATE_TEST_MOCK_RUNNER", &path);
        }

        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
        let mut rx = run(cancel_rx).await;

        if let Some(UpdateEvent::ModuleStarted(Module::System)) = rx.recv().await {
            let _ = cancel_tx.send(());
        } else {
            panic!("Expected ModuleStarted(System)");
        }

        let mut got_error = false;
        while let Some(ev) = rx.recv().await {
            if let UpdateEvent::Error(msg) = ev {
                assert!(msg.contains("cancelled"));
                got_error = true;
            }
        }

        unsafe {
            std::env::remove_var("FINUPDATE_TEST_MOCK_RUNNER");
        }
        assert!(got_error);
    }

    #[tokio::test]
    async fn test_orchestrator_integration_exit_77_uptodate() {
        let _lock = env_lock().lock().await;
        use std::io::Write;
        let mut mock_script = tempfile::NamedTempFile::new().unwrap();
        writeln!(
            mock_script,
            "#!/bin/sh\n\
             exit 77"
        )
        .unwrap();

        let temp_path = mock_script.into_temp_path();
        let path = temp_path.to_path_buf();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        unsafe {
            std::env::set_var("FINUPDATE_TEST_MOCK_RUNNER", &path);
        }

        let (_cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
        let mut rx = run(cancel_rx).await;

        let mut got_uptodate = false;
        while let Some(ev) = rx.recv().await {
            if let UpdateEvent::UpToDate = ev {
                got_uptodate = true;
            }
        }

        unsafe {
            std::env::remove_var("FINUPDATE_TEST_MOCK_RUNNER");
        }
        assert!(got_uptodate);
    }

    #[tokio::test]
    async fn test_orchestrator_integration_exit_error() {
        let _lock = env_lock().lock().await;
        use std::io::Write;
        let mut mock_script = tempfile::NamedTempFile::new().unwrap();
        writeln!(
            mock_script,
            "#!/bin/sh\n\
             exit 5"
        )
        .unwrap();

        let temp_path = mock_script.into_temp_path();
        let path = temp_path.to_path_buf();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        unsafe {
            std::env::set_var("FINUPDATE_TEST_MOCK_RUNNER", &path);
        }

        let (_cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
        let mut rx = run(cancel_rx).await;

        let mut got_error = false;
        while let Some(ev) = rx.recv().await {
            if let UpdateEvent::Error(msg) = ev {
                assert!(msg.contains("exit code 5") || msg.contains("exited with code 5"));
                got_error = true;
            }
        }

        unsafe {
            std::env::remove_var("FINUPDATE_TEST_MOCK_RUNNER");
        }
        assert!(got_error);
    }

    struct MockEnv {
        _temp_dir: tempfile::TempDir,
        bin_dir: std::path::PathBuf,
        log_path: std::path::PathBuf,
    }

    impl MockEnv {
        fn new() -> Self {
            let temp_dir = tempfile::tempdir().unwrap();
            let bin_dir = temp_dir.path().join("bin");
            std::fs::create_dir_all(&bin_dir).unwrap();
            let log_path = temp_dir.path().join("invocations.log");
            Self {
                _temp_dir: temp_dir,
                bin_dir,
                log_path,
            }
        }

        fn create_mock_bin(&self, name: &str, exit_code: i32) -> std::path::PathBuf {
            let path = self.bin_dir.join(name);
            let content = format!(
                "#!/bin/sh\n\
                 echo \"{name} called with args: $@\" >> \"{log}\"\n\
                 exit {exit_code}\n",
                name = name,
                log = self.log_path.display(),
                exit_code = exit_code
            );
            std::fs::write(&path, content).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            path
        }

        fn read_invocations(&self) -> String {
            if self.log_path.exists() {
                std::fs::read_to_string(&self.log_path).unwrap()
            } else {
                String::new()
            }
        }
    }

    #[tokio::test]
    async fn test_real_runner_full_success_with_mocks() {
        let _lock = env_lock().lock().await;

        let env = MockEnv::new();
        env.create_mock_bin("bootc", 0);
        env.create_mock_bin("flatpak", 0);
        env.create_mock_bin("su", 0);
        env.create_mock_bin("distrobox", 0);

        let brew_path = env.create_mock_bin("mock-brew", 0);

        let original_path = std::env::var("PATH").unwrap_or_default();
        let new_path = format!("{}:{}", env.bin_dir.display(), original_path);

        // CARGO_MANIFEST_DIR is this crate, but data/ lives at the workspace
        // root — hence the `..`.
        let runner_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("data")
            .join("finupdate-runner");
        assert!(
            runner_path.exists(),
            "finupdate-runner script not found in data/"
        );

        // Guarantee include_app_updates is true via isolated config home
        let temp_config = tempfile::tempdir().unwrap();
        let original_xdg = std::env::var("XDG_CONFIG_HOME").ok();

        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", temp_config.path());
        }

        let mut settings = crate::settings::Settings::default();
        settings.include_app_updates = true;
        settings.save();

        unsafe {
            std::env::set_var("PATH", &new_path);
            std::env::set_var("FINUPDATE_TEST_MOCK_RUNNER", &runner_path);
            std::env::set_var("FINUPDATE_TEST_BREW_BIN", &brew_path);
            std::env::set_var("PKEXEC_USER", "mocked-human-user");
        }

        let (_cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
        let mut rx = run(cancel_rx).await;

        let mut events = vec![];
        while let Some(ev) = rx.recv().await {
            events.push(ev);
        }

        unsafe {
            std::env::set_var("PATH", &original_path);
            std::env::remove_var("FINUPDATE_TEST_MOCK_RUNNER");
            std::env::remove_var("FINUPDATE_TEST_BREW_BIN");
            std::env::remove_var("PKEXEC_USER");
            if let Some(ref val) = original_xdg {
                std::env::set_var("XDG_CONFIG_HOME", val);
            } else {
                std::env::remove_var("XDG_CONFIG_HOME");
            }
        }

        assert!(!events.is_empty(), "Events list should not be empty");
        let expected = vec![
            UpdateEvent::ModuleStarted(Module::System),
            UpdateEvent::ModuleFinished(Module::System, ModuleStatus::Success),
            UpdateEvent::ModuleStarted(Module::Flatpak),
            UpdateEvent::ModuleFinished(Module::Flatpak, ModuleStatus::Success),
            UpdateEvent::ModuleStarted(Module::Brew),
            UpdateEvent::ModuleFinished(Module::Brew, ModuleStatus::Success),
            UpdateEvent::ModuleStarted(Module::Distrobox),
            UpdateEvent::ModuleFinished(Module::Distrobox, ModuleStatus::Success),
            UpdateEvent::Complete,
        ];

        for item in &expected {
            assert!(
                events.contains(item),
                "Missing expected event: {:?}. Full events list: {:?}",
                item,
                events
            );
        }

        let invocations = env.read_invocations();
        assert!(
            invocations.contains("bootc called with args: upgrade"),
            "bootc call invalid"
        );
        assert!(
            invocations.contains("flatpak called with args: update -y --noninteractive"),
            "flatpak call invalid"
        );
        assert!(
            invocations.contains("su called with args: - mocked-human-user -c"),
            "su call invalid"
        );
        assert!(
            invocations.contains("distrobox called with args: upgrade --all"),
            "distrobox call invalid"
        );
    }

    #[tokio::test]
    #[ignore = "requires real bootc/rpm-ostree on PATH"]
    async fn test_real_runner_system_only_skips_others() {
        let _lock = env_lock().lock().await;

        let env = MockEnv::new();
        env.create_mock_bin("bootc", 0);

        let original_path = std::env::var("PATH").unwrap_or_default();
        let new_path = format!("{}:{}", env.bin_dir.display(), original_path);

        // CARGO_MANIFEST_DIR is this crate, but data/ lives at the workspace
        // root — hence the `..`.
        let runner_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("data")
            .join("finupdate-runner");

        let temp_config = tempfile::tempdir().unwrap();
        let original_xdg = std::env::var("XDG_CONFIG_HOME").ok();

        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", temp_config.path());
        }

        let mut settings = crate::settings::Settings::default();
        settings.include_app_updates = false;
        settings.save();

        unsafe {
            std::env::set_var("PATH", &new_path);
            std::env::set_var("FINUPDATE_TEST_MOCK_RUNNER", &runner_path);
        }

        let (_cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
        let mut rx = run(cancel_rx).await;

        let mut events = vec![];
        while let Some(ev) = rx.recv().await {
            events.push(ev);
        }

        unsafe {
            std::env::set_var("PATH", &original_path);
            std::env::remove_var("FINUPDATE_TEST_MOCK_RUNNER");
            if let Some(ref val) = original_xdg {
                std::env::set_var("XDG_CONFIG_HOME", val);
            } else {
                std::env::remove_var("XDG_CONFIG_HOME");
            }
        }

        assert!(!events.is_empty());
        let expected = vec![
            UpdateEvent::ModuleStarted(Module::System),
            UpdateEvent::ModuleFinished(Module::System, ModuleStatus::Success),
            UpdateEvent::ModuleStarted(Module::Flatpak),
            UpdateEvent::ModuleFinished(Module::Flatpak, ModuleStatus::Skipped),
            UpdateEvent::ModuleStarted(Module::Brew),
            UpdateEvent::ModuleFinished(Module::Brew, ModuleStatus::Skipped),
            UpdateEvent::ModuleStarted(Module::Distrobox),
            UpdateEvent::ModuleFinished(Module::Distrobox, ModuleStatus::Skipped),
            UpdateEvent::Complete,
        ];

        for item in &expected {
            assert!(
                events.contains(item),
                "Missing expected event: {:?}. Full events list: {:?}",
                item,
                events
            );
        }

        let invocations = env.read_invocations();
        assert!(invocations.contains("bootc called with args: upgrade"));
        assert!(!invocations.contains("flatpak"));
        assert!(!invocations.contains("su"));
        assert!(!invocations.contains("distrobox"));
    }
}
