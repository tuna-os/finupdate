//! Auto-update settings I/O for the status view.
//!
//! Extracted from `status_view.rs` (finupdate#44): reading the uupd.timer
//! enabled-state and applying the auto-update preference shell out to
//! systemd via the privileged chokepoint. That side-effect code does not
//! belong in a widget file — the view should only observe and render. The
//! `bootc_probe` / `log_view` / `update_list` modules already follow this
//! separation; settings I/O now does too.

fn read_auto_updates_enabled() -> bool {
    let output = if crate::update_worker::is_flatpak() {
        Command::new("flatpak-spawn")
            .args(["--host", "systemctl", "is-enabled", "uupd.timer"])
            .output()
    } else {
        Command::new("systemctl")
            .args(["is-enabled", "uupd.timer"])
            .output()
    };

    match output {
        Ok(output) => match String::from_utf8_lossy(&output.stdout).trim() {
            "enabled" => true,
            "disabled" => false,
            _ => Settings::load().auto_updates,
        },
        Err(_) => Settings::load().auto_updates,
    }
}

fn apply_auto_updates_setting(active: bool) {
    let mut settings = Settings::load();
    settings.auto_updates = active;
    settings.save();

    let suppressed =
        crate::action_journal::Suppressed::from_flags(settings.dev_mode, settings.dry_run);
    let verb = if active { "enable" } else { "disable" };

    // Same command as uupd_compat::set_uupd_timer — this is the synchronous
    // twin used from the switch row. Both now share the chokepoint, so the
    // journal sees an identical `set_uupd_timer` entry whichever path fires.
    let mut cmd = match crate::privileged::privileged(
        "set_uupd_timer",
        serde_json::json!({ "enable": active }),
        &["systemctl", verb, "--now", "uupd.timer"],
        crate::privileged::Privilege::Pkexec,
        suppressed,
    ) {
        // The preference is already persisted above; suppressing only withholds
        // the host-side effect, which is exactly what dry-run promises.
        crate::privileged::Exec::Suppressed => return,
        crate::privileged::Exec::Run(cmd) => cmd,
    };

    std::thread::spawn(move || {
        let status = cmd.status();

        match status {
            Ok(status) if status.success() => {}
            Ok(status) => tracing::warn!("Failed to toggle uupd.timer: {}", status),
            Err(err) => tracing::warn!("Failed to toggle uupd.timer: {}", err),
        }
    });
}
