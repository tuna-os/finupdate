//! Changelog / What's-New page widget construction for StatusView.

use adw::prelude::*;
use relm4::prelude::*;

use crate::app::PreflightStatus;
use crate::registry_client::ImageVersion;
use crate::ui::bootc_probe::{
    build_stack_items, find_booted_match, get_host_kernel, read_booted_tag_suffix,
    read_selected_tag,
};
use crate::ui::changelog::SbomStatus;
use crate::ui::version_parse::parse_org_repo;

use super::helpers::{VERSION_MAX_CHARS, version_diff_box};
use super::{StatusView, StatusViewOutput};

impl StatusView {
    pub(super) fn rebuild_changelog_page(&self, sender: &ComponentSender<StatusView>) {
        while let Some(child) = self.changelog_box.first_child() {
            self.changelog_box.remove(&child);
        }

        let version = self.changelog_version.as_str();

        // Show a loading indicator until versions arrive
        if self.registry_versions.is_empty() {
            let spinner = gtk::Spinner::new();
            spinner.set_spinning(true);
            spinner.set_size_request(24, 24);
            let load_label = gtk::Label::new(Some("Loading versions…"));
            load_label.add_css_class("dim-label");
            load_label.add_css_class("caption");
            let load_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            load_box.set_halign(gtk::Align::Center);
            load_box.append(&spinner);
            load_box.append(&load_label);
            self.changelog_box.append(&load_box);
        }

        let mut real_version: Option<&ImageVersion> = None;
        if !self.registry_versions.is_empty() {
            real_version = self
                .registry_versions
                .iter()
                .find(|v| v.version == self.changelog_version);
        }

        // Booted version — pulled from bootc-status's booted image ref (with
        // os-release fallback for Dakota where bootc-status fails). Anchors
        // the "from" side of the Stack diff. None when the booted tag can't
        // be matched against the recently-fetched registry_versions window.
        let booted_version: Option<&ImageVersion> = read_booted_tag_suffix()
            .as_deref()
            .and_then(|t| find_booted_match(&self.registry_versions, t));

        let header_box = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        header_box.set_margin_top(12);

        let info_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
        info_box.set_hexpand(true);

        let tag_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);

        let tag_code = gtk::Label::builder()
            .label(&format!("{}:{}", self.registry_uri, version))
            .halign(gtk::Align::Start)
            .build();
        tag_code.add_css_class("monospace");
        tag_box.append(&tag_code);

        // Pills in header
        let is_update = if let Some(v) = real_version {
            let booted_tag = read_selected_tag();
            v.version != booted_tag
                && !self.reboot_pending
                && matches!(self.preflight_status, PreflightStatus::UpdateAvailable)
        } else {
            false
        };

        if is_update {
            let update_pill = gtk::Label::new(Some("Update"));
            update_pill.add_css_class("accent");
            update_pill.add_css_class("caption");
            tag_box.append(&update_pill);
        } else {
            let is_booted = if let Some(v) = real_version {
                let booted_tag = read_selected_tag();
                v.version == booted_tag
            } else {
                false
            };
            if is_booted {
                let booted_pill = gtk::Label::new(Some("✓ Booted"));
                booted_pill.add_css_class("success");
                booted_pill.add_css_class("caption");
                tag_box.append(&booted_pill);
            }
        }
        info_box.append(&tag_box);

        let stable_str = if let Some(v) = real_version {
            v.version.clone()
        } else {
            version.to_string()
        };
        let date_str = if let Some(v) = real_version {
            v.created.format("%B %-d, %Y").to_string()
        } else {
            "".to_string()
        };
        let meta_label = gtk::Label::builder()
            .label(&format!("{}  ·  {}", stable_str, date_str))
            .halign(gtk::Align::Start)
            .build();
        meta_label.add_css_class("caption");
        meta_label.add_css_class("dim-label");
        info_box.append(&meta_label);

        let summary_str = if let Some(v) = real_version {
            let booted_tag = read_selected_tag();
            if v.version == booted_tag {
                format!(
                    "Currently booted. Kernel {} · stable point release.",
                    v.kernel
                )
            } else {
                format!(
                    "Image build. Kernel {} · git commit {}.",
                    v.kernel,
                    if v.revision.len() >= 7 {
                        &v.revision[0..7]
                    } else {
                        &v.revision
                    }
                )
            }
        } else {
            "".to_string()
        };
        let summary_label = gtk::Label::builder()
            .label(&summary_str)
            .halign(gtk::Align::Start)
            .wrap(true)
            .max_width_chars(60)
            .build();
        summary_label.add_css_class("body");
        info_box.append(&summary_label);

        header_box.append(&info_box);

        if is_update {
            let install_btn = gtk::Button::builder()
                .label("Install")
                .icon_name("object-select-symbolic")
                .build();
            install_btn.add_css_class("suggested-action");
            install_btn.set_valign(gtk::Align::Center);
            let out_sender = sender.output_sender().clone();
            install_btn.connect_clicked(move |_| {
                let _ = out_sender.send(StatusViewOutput::StartUpdate);
            });
            header_box.append(&install_btn);
        }

        self.changelog_box.append(&header_box);

        let stack_title = gtk::Label::builder()
            .label("Stack")
            .halign(gtk::Align::Start)
            .margin_top(12)
            .build();
        stack_title.add_css_class("caption");
        stack_title.add_css_class("dim-label");
        self.changelog_box.append(&stack_title);

        // Build "from → to" rows for the Stack. The user is comparing the
        // booted build (left) against the selected target (right), so each
        // component renders as `current → target` with the target highlighted
        // when it differs. Booted info is missing when bootc-status couldn't
        // be read or when the booted tag is outside the registry window — in
        // that case we degrade to "—" for the current side so the row still
        // makes sense. `host_kernel` (uname -r) backstops the Kernel row for
        // Dakota, whose registry-side kernel is empty.
        let host_kernel = get_host_kernel();
        let mut stack_items = build_stack_items(
            booted_version,
            real_version,
            if host_kernel == "—" {
                None
            } else {
                Some(host_kernel.as_str())
            },
        );

        if self.sbom_diff.is_some() {
            stack_items.retain(|item| item.label != "Kernel");
        }

        if !stack_items.is_empty() {
            let stack_list = gtk::ListBox::builder()
                .selection_mode(gtk::SelectionMode::None)
                .build();
            stack_list.add_css_class("card");

            for item in &stack_items {
                let row = adw::ActionRow::builder().title(item.label).build();
                row.add_suffix(&version_diff_box(
                    item.current.as_deref().unwrap_or("—"),
                    &item.target,
                    item.bumped,
                ));
                stack_list.append(&row);
            }

            if let Some(ref diff) = self.sbom_diff {
                let targets = [
                    ("Kernel", vec!["Kernel"]),
                    ("Gnome", vec!["GNOME", "Gnome"]),
                    ("Mesa", vec!["Mesa"]),
                    ("Podman", vec!["Podman"]),
                    ("Nvidia", vec!["Nvidia", "NVIDIA"]),
                    ("bootc", vec!["bootc"]),
                    ("systemd", vec!["systemd"]),
                    ("pipewire", vec!["pipewire"]),
                    ("flatpak", vec!["Flatpak", "flatpak"]),
                ];
                for &(label, ref keys) in &targets {
                    let mut found = None;
                    for key in keys {
                        if let Some(versions) = diff.stack_info.get(*key) {
                            found = Some(versions);
                            break;
                        }
                    }
                    if let Some((booted_ver, target_ver)) = found {
                        let row = adw::ActionRow::builder().title(label).build();
                        let current = if booted_ver.is_empty() {
                            "—"
                        } else {
                            booted_ver.as_str()
                        };
                        row.add_suffix(&version_diff_box(
                            current,
                            target_ver.as_str(),
                            booted_ver != target_ver,
                        ));
                        stack_list.append(&row);
                    }
                }
            }

            self.changelog_box.append(&stack_list);
        }

        let mut upgrades_list: Vec<(String, String, String)> = Vec::new();
        let mut downgrades_list: Vec<(String, String, String)> = Vec::new();
        let mut added_list: Vec<(String, String)> = Vec::new();
        let mut removals_list: Vec<String> = Vec::new();

        if let Some(ref diff) = self.sbom_diff {
            for pkg in &diff.upgraded {
                upgrades_list.push((
                    pkg.name.clone(),
                    pkg.old_version.clone(),
                    pkg.new_version.clone(),
                ));
            }
            for pkg in &diff.downgraded {
                downgrades_list.push((
                    pkg.name.clone(),
                    pkg.old_version.clone(),
                    pkg.new_version.clone(),
                ));
            }
            for pkg in &diff.added {
                added_list.push((pkg.name.clone(), pkg.new_version.clone()));
            }
            for pkg in &diff.removed {
                removals_list.push(pkg.clone());
            }
        }

        // SBOM placeholder: surface the in-flight state so the user knows the
        // package diff is loading. Without this the Stack section is silently
        // blank for 30+ seconds on a slow connection. Skipped once the diff
        // lands and we have real upgrades/removals to render below.
        match self.sbom_status {
            SbomStatus::Loading => {
                let title = gtk::Label::builder()
                    .label("Package changes")
                    .halign(gtk::Align::Start)
                    .margin_top(12)
                    .build();
                title.add_css_class("caption");
                title.add_css_class("dim-label");
                self.changelog_box.append(&title);

                let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                row.set_margin_start(12);
                row.set_margin_end(12);
                row.set_margin_top(8);
                row.set_margin_bottom(8);
                let spinner = gtk::Spinner::new();
                spinner.set_spinning(true);
                spinner.set_size_request(16, 16);
                let lbl = gtk::Label::new(Some("Comparing packages…"));
                lbl.add_css_class("dim-label");
                lbl.add_css_class("caption");
                row.append(&spinner);
                row.append(&lbl);

                let placeholder = gtk::Box::new(gtk::Orientation::Vertical, 0);
                placeholder.add_css_class("card");
                placeholder.append(&row);
                self.changelog_box.append(&placeholder);
            }
            SbomStatus::NotAvailable => {
                let title = gtk::Label::builder()
                    .label("Package changes")
                    .halign(gtk::Align::Start)
                    .margin_top(12)
                    .build();
                title.add_css_class("caption");
                title.add_css_class("dim-label");
                self.changelog_box.append(&title);

                let lbl = gtk::Label::builder()
                    .label("Package diff not available — registry didn't publish an SPDX SBOM for one of the images.")
                    .halign(gtk::Align::Start)
                    .wrap(true)
                    .max_width_chars(70)
                    .margin_start(12)
                    .margin_end(12)
                    .margin_top(8)
                    .margin_bottom(8)
                    .build();
                lbl.add_css_class("dim-label");
                lbl.add_css_class("caption");
                let placeholder = gtk::Box::new(gtk::Orientation::Vertical, 0);
                placeholder.add_css_class("card");
                placeholder.append(&lbl);
                self.changelog_box.append(&placeholder);
            }
            SbomStatus::Pending | SbomStatus::Loaded => {}
        }

        if !upgrades_list.is_empty() {
            let upgrades_title = gtk::Label::builder()
                .label(&format!("Updated  ·  {}", upgrades_list.len()))
                .halign(gtk::Align::Start)
                .margin_top(12)
                .build();
            upgrades_title.add_css_class("caption");
            upgrades_title.add_css_class("dim-label");
            self.changelog_box.append(&upgrades_title);

            let list_upgrades = gtk::ListBox::builder()
                .selection_mode(gtk::SelectionMode::None)
                .build();
            list_upgrades.add_css_class("card");

            for (pkg, from, to) in upgrades_list {
                let row = adw::ActionRow::builder().title(&pkg).build();
                // version_diff_box re-derives the direction, so a package that
                // landed here with unorderable versions still renders neutral
                // rather than claiming an upgrade.
                row.add_suffix(&version_diff_box(&from, &to, true));
                list_upgrades.append(&row);
            }
            self.changelog_box.append(&list_upgrades);
        }

        // Downgrades get their own section rather than being folded into
        // "Updated". Rolling back, or moving to an older stream, is a normal
        // thing to do — but it needs saying out loud, not burying in a list
        // whose heading claims everything moved forward.
        if !downgrades_list.is_empty() {
            let downgrades_title = gtk::Label::builder()
                .label(&format!("Downgraded  ·  {}", downgrades_list.len()))
                .halign(gtk::Align::Start)
                .margin_top(12)
                .build();
            downgrades_title.add_css_class("caption");
            downgrades_title.add_css_class("dim-label");
            self.changelog_box.append(&downgrades_title);

            let list_downgrades = gtk::ListBox::builder()
                .selection_mode(gtk::SelectionMode::None)
                .build();
            list_downgrades.add_css_class("card");

            for (pkg, from, to) in downgrades_list {
                let row = adw::ActionRow::builder().title(&pkg).build();
                row.add_suffix(&version_diff_box(&from, &to, true));
                list_downgrades.append(&row);
            }
            self.changelog_box.append(&list_downgrades);
        }

        if !added_list.is_empty() {
            let added_title = gtk::Label::builder()
                .label(&format!("Added  ·  {}", added_list.len()))
                .halign(gtk::Align::Start)
                .margin_top(12)
                .build();
            added_title.add_css_class("caption");
            added_title.add_css_class("dim-label");
            self.changelog_box.append(&added_title);

            let list_added = gtk::ListBox::builder()
                .selection_mode(gtk::SelectionMode::None)
                .build();
            list_added.add_css_class("card");

            for (pkg, ver) in added_list {
                let row = adw::ActionRow::builder().title(&pkg).build();

                // Prefix with a green `+` so additions stand out from
                // upgrades. ActionRow doesn't natively style the prefix
                // glyph — a Label with the `success` class does the job.
                let plus_lbl = gtk::Label::new(Some("+"));
                plus_lbl.add_css_class("success");
                plus_lbl.add_css_class("monospace");
                row.add_prefix(&plus_lbl);

                let ver_lbl = gtk::Label::new(Some(&ver));
                ver_lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
                ver_lbl.set_max_width_chars(VERSION_MAX_CHARS);
                ver_lbl.set_tooltip_text(Some(&ver));
                ver_lbl.add_css_class("monospace");
                ver_lbl.add_css_class("caption");
                ver_lbl.add_css_class("success");
                row.add_suffix(&ver_lbl);

                list_added.append(&row);
            }
            self.changelog_box.append(&list_added);
        }

        if !removals_list.is_empty() {
            let removals_title = gtk::Label::builder()
                .label(&format!("Removed  ·  {}", removals_list.len()))
                .halign(gtk::Align::Start)
                .margin_top(12)
                .build();
            removals_title.add_css_class("caption");
            removals_title.add_css_class("dim-label");
            self.changelog_box.append(&removals_title);

            let list_removals = gtk::ListBox::builder()
                .selection_mode(gtk::SelectionMode::None)
                .build();
            list_removals.add_css_class("card");

            for pkg in removals_list {
                let row = adw::ActionRow::builder().title(&pkg).build();
                let dash_lbl = gtk::Label::new(Some("−"));
                dash_lbl.add_css_class("error");
                row.add_prefix(&dash_lbl);
                list_removals.append(&row);
            }
            self.changelog_box.append(&list_removals);
        }

        let commits_list: Vec<(String, String, String, String)> = self.github_commits.clone();

        if !commits_list.is_empty() {
            let commits_title = gtk::Label::builder()
                .label("Commits")
                .halign(gtk::Align::Start)
                .margin_top(12)
                .build();
            commits_title.add_css_class("caption");
            commits_title.add_css_class("dim-label");
            self.changelog_box.append(&commits_title);

            let list_commits = gtk::ListBox::builder()
                .selection_mode(gtk::SelectionMode::None)
                .build();
            list_commits.add_css_class("card");

            // Build GitHub URL from registry URI org/repo
            let github_url = parse_org_repo(&self.registry_uri)
                .map(|(org, repo)| format!("https://github.com/{}/{}", org, repo));

            for (sha, msg, author, date) in commits_list {
                let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 12);
                row_box.set_margin_start(16);
                row_box.set_margin_end(16);
                row_box.set_margin_top(8);
                row_box.set_margin_bottom(8);

                let sha_short = if sha.len() >= 7 { &sha[0..7] } else { &sha };
                let sha_lbl = gtk::Label::new(Some(sha_short));
                sha_lbl.add_css_class("monospace");
                sha_lbl.add_css_class("caption");
                sha_lbl.add_css_class("dim-label");
                sha_lbl.set_valign(gtk::Align::Start);
                row_box.append(&sha_lbl);

                let msg_box = gtk::Box::new(gtk::Orientation::Vertical, 2);
                msg_box.set_hexpand(true);

                let msg_lbl = gtk::Label::builder()
                    .label(&msg)
                    .halign(gtk::Align::Start)
                    .wrap(true)
                    .build();
                msg_lbl.add_css_class("body");
                msg_box.append(&msg_lbl);

                // Parse ISO8601 date and format as "MMM D, YYYY"
                let date_str = if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(&date) {
                    parsed.format("%b %d, %Y").to_string()
                } else {
                    date.clone()
                };

                let auth_lbl = gtk::Label::builder()
                    .label(&format!("{} · {}", author, date_str))
                    .halign(gtk::Align::Start)
                    .build();
                auth_lbl.add_css_class("caption");
                auth_lbl.add_css_class("dim-label");
                msg_box.append(&auth_lbl);

                row_box.append(&msg_box);

                // Whole-row click opens the GitHub commit page in the user's
                // default browser.
                if let Some(ref base_url) = github_url {
                    let commit_url = format!("{}/commit/{}", base_url, sha);
                    let gesture = gtk::GestureClick::new();
                    let row_for_gesture = row_box.clone();
                    gesture.connect_pressed(move |_, _, _, _| {
                        let launcher = gtk::UriLauncher::new(&commit_url);
                        let parent = row_for_gesture
                            .root()
                            .and_then(|r| r.downcast::<gtk::Window>().ok());
                        launcher.launch(parent.as_ref(), gtk::gio::Cancellable::NONE, |result| {
                            if let Err(e) = result {
                                tracing::warn!("Couldn't open commit URL: {}", e);
                            }
                        });
                    });
                    row_box.add_controller(gesture);
                    row_box.set_cursor_from_name(Some("pointer"));
                }

                list_commits.append(&row_box);
            }
            self.changelog_box.append(&list_commits);
        }

        if is_update {
            self.changelog_install_bar.set_visible(true);
        } else {
            self.changelog_install_bar.set_visible(false);
        }
    }
}
