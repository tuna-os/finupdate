#!/usr/bin/env bash
#
# Visual-audit screenshot capture for the finupdate ↔ gnome-control-center
# comparison. Drives both apps from this host's GNOME session via D-Bus.
#
# Why D-Bus instead of grim / gnome-screenshot:
#   - Bluefin ships GNOME Shell's Screenshot service on the session bus; no
#     extra packages required.
#   - ScreenshotWindow captures only the active window (no panel chrome from
#     other apps, no desktop background bleed-through).
#
# Usage:
#   tests/visual-audit/capture.sh reference <gcc-panel>     # control-center
#   tests/visual-audit/capture.sh finupdate <slug>          # finupdate
#
# Examples:
#   tests/visual-audit/capture.sh reference system
#   tests/visual-audit/capture.sh reference applications
#   tests/visual-audit/capture.sh finupdate idle
#   tests/visual-audit/capture.sh finupdate rebase     (Ctrl+Shift+R first)
#
# Output: tests/visual-audit/screenshots/<which>/<slug>.png

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT_ROOT="$SCRIPT_DIR/screenshots"

which="${1:-}"
slug="${2:-}"

if [[ -z "$which" || -z "$slug" ]]; then
    echo "Usage: $0 {reference|finupdate} <slug>" >&2
    exit 2
fi

case "$which" in
    reference|finupdate) : ;;
    *) echo "first arg must be 'reference' or 'finupdate', got: $which" >&2; exit 2 ;;
esac

mkdir -p "$OUT_ROOT/$which"
out_path="$OUT_ROOT/$which/$slug.png"

# Helper: run on host (this script may be invoked from inside a toolbox / flatpak)
host_run() {
    if [[ -n "${FLATPAK_ID:-}" ]] || [[ -n "${TOOLBOX_PATH:-}" ]] || ! command -v gnome-control-center >/dev/null 2>&1; then
        flatpak-spawn --host "$@"
    else
        "$@"
    fi
}

launch_reference() {
    # `gnome-control-center <panel>` opens straight to that panel.
    # Background it so we can capture; pid tracked so cleanup is precise.
    host_run pkill -f "gnome-control-center" 2>/dev/null || true
    sleep 0.5
    host_run gnome-control-center "$slug" >/dev/null 2>&1 &
    # Give the window time to render + settle. Cold-start is ~1-2s; we kill
    # between runs so always treat it as a cold start.
    sleep 2.5
}

launch_finupdate() {
    # Assumes the Devel Flatpak is installed (just flatpak / just flatpak-run).
    # Don't restart if it's already running — the user may be staging a
    # specific subpage state (e.g. rebase dialog open).
    if ! host_run pgrep -f "org.tunaos.finupdate.Devel" >/dev/null 2>&1; then
        host_run flatpak run org.tunaos.finupdate.Devel &
        sleep 4
    fi
}

capture_active_window() {
    # ScreenshotWindow(include_frame: bool, include_cursor: bool, flash: bool, filename: string)
    # → (success: bool, filename: string)
    #
    # include_frame=true  → captures the GTK CSD frame (shadows, headerbar) so
    #                       the screenshot matches what the user sees.
    # include_cursor=false → no mouse pointer overlay.
    # flash=false         → no screen-flash effect (we're scripted).
    host_run gdbus call --session \
        --dest org.gnome.Shell.Screenshot \
        --object-path /org/gnome/Shell/Screenshot \
        --method org.gnome.Shell.Screenshot.ScreenshotWindow \
        true false false "$out_path" >/dev/null
}

case "$which" in
    reference)
        launch_reference
        capture_active_window
        # Leave the panel open so the human can inspect — kill manually
        # between runs or invoke the script with a new slug (it'll pkill).
        ;;
    finupdate)
        launch_finupdate
        capture_active_window
        ;;
esac

echo "✓ $out_path"
