//! The one piece of `bootc_probe` that needs a GDK display: resolving the
//! distro logo through `gtk::IconTheme`.
//!
//! Everything else that used to live in this file — reading `bootc status`
//! and `/etc/os-release`, matching the booted image against the registry
//! version list, building the changelog Stack rows, and the pure logo-file
//! search — has moved to `finupdate_core::bootc_probe` (finupdate#111): none
//! of it touched GTK, and it belongs on the side of the crate boundary that
//! `cargo test -p finupdate-core` and `finupdate-cli` can actually reach.
//!
//! `crate::bootc_probe` re-exports that module (see `src/lib.rs`), so the
//! rest of `src/ui/` calls `crate::bootc_probe::…` for everything that moved;
//! only this file's two functions stayed behind, because they are the only
//! ones that call `gtk::gdk::Display::default()`.

use relm4::gtk;

use crate::bootc_probe::{HeroLogo, LOGO_SEARCH_ROOTS, read_logo_names, resolve_logo_file};

pub(super) fn read_logo_icon_name() -> HeroLogo {
    let branded = read_logo_names();

    // Tier 1: the icon theme, for distros that install their logo properly.
    if let Some(display) = gtk::gdk::Display::default() {
        let theme = gtk::IconTheme::for_display(&display);
        for name in &branded {
            if theme.has_icon(name) {
                return HeroLogo::Themed(name.clone());
            }
        }
    }

    // Tier 2: the file the theme can't see. This is the arm that fires on
    // Bluefin/Dakota.
    for name in &branded {
        if let Some(path) = resolve_logo_file(LOGO_SEARCH_ROOTS, name) {
            return HeroLogo::File(path);
        }
    }

    // Fallbacks. `distributor-logo-symbolic` is the freedesktop spec name;
    // `computer-symbolic` is guaranteed by Adwaita, so the row is never blank.
    if let Some(display) = gtk::gdk::Display::default() {
        let theme = gtk::IconTheme::for_display(&display);
        if theme.has_icon("distributor-logo-symbolic") {
            return HeroLogo::Themed("distributor-logo-symbolic".to_string());
        }
    }
    HeroLogo::Themed("computer-symbolic".to_string())
}
