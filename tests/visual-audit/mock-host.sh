#!/usr/bin/env bash
# Build a fake bootc host in a temp directory so finupdate renders a realistic,
# populated window on a machine that is not a bootc system at all (a CI
# container, say).
#
# Why not --dev-mode or --dry-run: both reveal the dev banner
# ("Developer Mode — updates are simulated" / "Dry run — ..."), which has no
# business appearing in a screenshot published to an app store. The mock
# runner hook and PATH stubs drive the *production* code path instead, so what
# gets photographed is the UI a real user sees.
#
# Usage:  eval "$(tests/visual-audit/mock-host.sh)"
# Prints the environment assignments to stdout; everything else goes to stderr.
set -euo pipefail

BIN="$(mktemp -d -t finupdate-mockbin-XXXXXX)"

# `bootc status --json` — finupdate reads .status.booted.image.image.image and
# parses it with parse_image_ref(), so the ref has to look like a real one.
cat > "$BIN/bootc" <<'STUB'
#!/bin/sh
[ "$1" = "status" ] || exit 0
cat <<'JSON'
{"status":{"booted":{"image":{"image":{"image":"ghcr.io/ublue-os/bluefin:stable","transport":"registry"},"version":"43.20260810","timestamp":"2026-08-10T00:00:00Z"}}}}
JSON
STUB

# uupd_compat probes `which uupd` and `systemctl is-enabled uupd.timer`.
# Reporting the timer as enabled is the ordinary configuration on Bluefin.
cat > "$BIN/uupd" <<'STUB'
#!/bin/sh
exit 0
STUB

cat > "$BIN/systemctl" <<'STUB'
#!/bin/sh
if [ "$1" = "is-enabled" ]; then echo enabled; exit 0; fi
exit 0
STUB

chmod +x "$BIN/bootc" "$BIN/uupd" "$BIN/systemctl"

# An isolated XDG_CONFIG_HOME. Without it the capture would read (and write)
# whatever settings.json the runner happens to have, which is how a stale
# dev_mode=true silently turns a "production" screenshot into a simulated one.
CFG="$(mktemp -d -t finupdate-mockcfg-XXXXXX)"

echo "PATH=$BIN:$PATH"
echo "XDG_CONFIG_HOME=$CFG"
echo "FINUPDATE_TEST_MOCK_RUNNER=$PWD/data/finupdate-runner"
