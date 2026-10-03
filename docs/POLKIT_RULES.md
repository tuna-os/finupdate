# Polkit Authorization Rules for Finupdate

## Overview

Finupdate requires elevated privileges to interact with bootc and system management operations. This document describes the Polkit rules configured to allow these operations without interactive password prompts during testing and routine use.

## Rule: `/etc/polkit-1/rules.d/49-finupdate.rules`

### Purpose
Allows members of the `wheel` group to run the allowlisted bootc and
finupdate-runner programs with no password prompt. Members can also reboot the
system and start/stop the `uupd.timer` unit without a prompt; enabling or
disabling that timer's unit file still asks for the admin password (see
Security Notes). Designed for:
- Automated testing in CI/CD environments
- Development/debug mode operations
- Non-destructive command verification (bootc status, upgrade checks)

### Configuration
```javascript
// The exec allowlist matches the FULL program path, exactly.
var FINUPDATE_ALLOWED_PROGRAMS = [
    "/usr/bin/bootc",
    "/usr/sbin/bootc",
    "/usr/bin/finupdate-runner",
    "/usr/libexec/finupdate-runner"
];

polkit.addRule(function(action, subject) {
    if (subject.isInGroup("wheel")) {
        if (action.id == "org.freedesktop.policykit.exec") {
            var program = action.lookup("program");
            if (program && FINUPDATE_ALLOWED_PROGRAMS.indexOf(program) >= 0) {
                return polkit.Result.YES;
            }
        }
        // Allow systemctl reboot for integration testing
        if (action.id == "org.freedesktop.login1.reboot") {
            return polkit.Result.YES;
        }
        // Scoped to the one unit finupdate actually starts/stops. systemd
        // supplies a per-call "unit" detail for this action.
        if (action.id == "org.freedesktop.systemd1.manage-units") {
            if (action.lookup("unit") == "uupd.timer") {
                return polkit.Result.YES;
            }
        }
        // manage-unit-files (enable/disable) is NOT granted: systemd does
        // not expose a per-unit detail for it, so it cannot be scoped, and
        // an unscoped grant is passwordless root. This falls through to
        // the system default (password prompt) instead.
    }
});
```

The full shipped rule is `build-aux/49-finupdate.polkit.rules`; keep the two
in step.

**Match the whole path, never a substring.** `program.indexOf("bootc") >= 0`
authorizes every executable whose path merely *contains* that text, so any
unprivileged process can drop a script at `/home/u/bootc/x.sh` or
`/tmp/finupdate-runner-1234.sh` and get it run as root with no password. A
path belongs on the allowlist only if root owns it and no unprivileged user
can write to it or to any directory leading to it.

### Operations Authorized

#### bootc commands (all variants)
- `bootc status --json` — Query current OS image metadata
- `bootc status` — Human-readable status output
- `bootc upgrade` — Stage image upgrades
- `bootc upgrade --check` — Check for available upgrades

Executed via:
- Direct: `pkexec bootc <command>`
- From Flatpak: `flatpak-spawn --host pkexec bootc <command>`

#### System reboot
- `systemctl reboot` — Initiate system restart
- Polkit action: `org.freedesktop.login1.reboot`

#### Systemd unit management
- `org.freedesktop.systemd1.manage-units` — start/stop the `uupd.timer` unit
  (`systemctl start|stop --now uupd.timer`). The rule checks
  `action.lookup("unit") == "uupd.timer"` and grants **only** that unit —
  not "otherwise manage the systemd units used by finupdate" in general.
- `org.freedesktop.systemd1.manage-unit-files` — would cover enabling or
  disabling the timer's unit file (the other half of
  `systemctl enable|disable --now uupd.timer`). The rule does **not** grant
  this: systemd does not pass a per-unit detail for this action on the
  systemd versions checked, so there's no way to scope a YES to
  `uupd.timer` specifically, and a blanket YES would authorize writing or
  linking any unit file as root. This falls through to polkit's default
  authority, i.e. it prompts for the admin password.

Treat any change to the finupdate code that selects or changes which
systemd unit is touched as a change to this privilege boundary — the
allowlist above is `uupd.timer` and nothing else.

### Security Notes

**Scope**: Limited to members of `wheel`, and — for `pkexec` — to the exact
program paths on the allowlist, and — for systemd unit management — to the
single `uupd.timer` unit via `manage-units`. It does not authorize arbitrary
root command execution or arbitrary unit-file writes *provided* the exec
allowlist stays exact-match with root-owned entries and the unit check stays
an exact-match on `action.lookup("unit")`. A substring match on the exec
path, an entry under a user-writable directory, or restoring an unscoped
`manage-units`/`manage-unit-files` grant each remove that guarantee.

**Assumptions**: This configuration assumes members of `wheel` are trusted
with system administration. Note what it still changes even so: `sudo`
normally re-authenticates, and this rule does not. On a machine where the
administrator's `sudo` requires a password, installing this rule means code
running in that user's session — a compromised app, not a person — reaches
the allowlisted programs as root with no prompt at all.

**Non-destructive intent**: The rule authorizes operations that are necessary for update checking and management, not arbitrary system modification. The finupdate application enforces additional safeguards:
- Dev mode prevents actual reboots
- Simulation scenarios allow safe testing without touching the real system

### Installation

The rule is deployed during system setup or when finupdate is initialized:

```bash
sudo install -m 0644 build-aux/49-finupdate.polkit.rules \
    /etc/polkit-1/rules.d/49-finupdate.rules
```

Installing the file from the repository rather than pasting a second copy of
the rule keeps one reviewed version of the allowlist.

### Verification

Test that rules are in effect:

```bash
# Should complete without password prompt
flatpak-spawn --host pkexec bootc status --json

# Should show current deployment info
pkexec bootc status
```

### Upstream Proposal

This rule is intended as a model for upstreaming into the Dakota OS layer or a finupdate system package. The specific actions (bootc status, reboot) are legitimate for any system update tool and could be generalized for broader use.

## Related Issues

- **AT-SPI testing dependencies**: See `docs/GUI_TESTING.md` for notes on `gnome-ponytail-daemon` requirement for automated GUI tests.

## References

- [Polkit Documentation](https://www.freedesktop.org/software/polkit/docs/latest/)
- [systemd-logind D-Bus Interface](https://dbus.freedesktop.org/doc/org.freedesktop.login1.html)
- [bootc Documentation](https://containers.github.io/bootc/)
