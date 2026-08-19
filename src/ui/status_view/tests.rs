//! Unit tests for the status view (moved out of the component file).

use super::*;
use serde_json::{Value, json};

// ── parse_booted_image_summary ───────────────────────────────────────
// Pure JSON-shape tests for the hero-row subtitle helper. The bootc
// status JSON shape is `{ "status": { "booted": { "image": { ... } } } }`.

#[test]
fn booted_summary_with_image_digest_and_date() {
    let j = json!({
        "status": {
            "booted": {
                "image": {
                    "image": { "image": "ghcr.io/projectbluefin/dakota:latest" },
                    "imageDigest": "sha256:bc6d66c90d1e230b89f71a459fcd9f07fd72582b5a2a633f71885e7f6bf722ed",
                    "timestamp": "2026-05-30T02:20:28Z"
                }
            }
        }
    });
    // Two-line subtitle: ref on line 1, "shaDIGEST · YYYY-MM-DD" on line 2.
    assert_eq!(
        parse_booted_image_summary(&j),
        Some("ghcr.io/projectbluefin/dakota:latest\nbc6d66c9 · 2026-05-30".to_string())
    );
}

#[test]
fn booted_summary_with_image_and_digest_no_date() {
    let j = json!({
        "status": {
            "booted": {
                "image": {
                    "image": { "image": "ghcr.io/projectbluefin/dakota:latest" },
                    "imageDigest": "sha256:abcdef1234567890"
                }
            }
        }
    });
    assert_eq!(
        parse_booted_image_summary(&j),
        Some("ghcr.io/projectbluefin/dakota:latest\nabcdef12".to_string())
    );
}

#[test]
fn booted_summary_with_image_only() {
    let j = json!({
        "status": { "booted": { "image": { "image": { "image": "ghcr.io/projectbluefin/dakota:latest" } } } }
    });
    assert_eq!(
        parse_booted_image_summary(&j),
        Some("ghcr.io/projectbluefin/dakota:latest".to_string())
    );
}

#[test]
fn booted_summary_with_digest_only() {
    let j = json!({
        "status": {
            "booted": { "image": { "imageDigest": "sha256:cafe1234ffff5678" } }
        }
    });
    // Digest-only (no image ref): renders as just the second-line piece.
    assert_eq!(parse_booted_image_summary(&j), Some("cafe1234".to_string()));
}

#[test]
fn booted_summary_handles_unprefixed_digest() {
    // Some bootc versions emit the digest without the `sha256:` prefix.
    let j = json!({
        "status": {
            "booted": { "image": { "imageDigest": "00ff11ee22dd33cc" } }
        }
    });
    assert_eq!(parse_booted_image_summary(&j), Some("00ff11ee".to_string()));
}

#[test]
fn booted_summary_missing_booted_returns_none() {
    let j = json!({ "status": {} });
    assert_eq!(parse_booted_image_summary(&j), None);
}

#[test]
fn booted_summary_empty_image_returns_none() {
    let j = json!({ "status": { "booted": { "image": {} } } });
    assert_eq!(parse_booted_image_summary(&j), None);
}

// ── parse_booted_tag_suffix ──────────────────────────────────────────
// Pulls the tag suffix from the booted image ref so the changelog page
// can pair the booted build with its registry_versions entry.

#[test]
fn booted_tag_suffix_extracts_tag() {
    let j = json!({
        "status": {
            "booted": {
                "image": { "image": { "image": "ghcr.io/projectbluefin/dakota:stable-daily-43.20260530" } }
            }
        }
    });
    assert_eq!(
        parse_booted_tag_suffix(&j),
        Some("stable-daily-43.20260530".to_string())
    );
}

#[test]
fn booted_tag_suffix_missing_image_returns_none() {
    let j = json!({ "status": { "booted": {} } });
    assert_eq!(parse_booted_tag_suffix(&j), None);
}

#[test]
fn booted_tag_suffix_untagged_image_returns_none() {
    // No `:tag` separator → nothing to extract.
    let j = json!({
        "status": { "booted": { "image": { "image": { "image": "ghcr.io/projectbluefin/dakota" } } } }
    });
    assert_eq!(parse_booted_tag_suffix(&j), None);
}

// ── build_stack_items ────────────────────────────────────────────────
// Constructs the from→to rows the changelog Stack renders. Marks
// `bumped=true` only when the value actually moved so the renderer
// can highlight just the components that changed.

fn fake_image_version(
    version: &str,
    kernel: &str,
    revision: &str,
    created_iso: &str,
) -> ImageVersion {
    ImageVersion {
        date: chrono::NaiveDate::from_ymd_opt(2026, 5, 30).unwrap(),
        full_ref: format!("ghcr.io/example/image:{version}"),
        version: version.to_string(),
        kernel: kernel.to_string(),
        revision: revision.to_string(),
        created: chrono::DateTime::parse_from_rfc3339(created_iso)
            .unwrap()
            .with_timezone(&chrono::Utc),
    }
}

#[test]
fn stack_items_empty_without_target() {
    assert!(build_stack_items(None, None, None).is_empty());
}

#[test]
fn stack_items_marks_changed_components_as_bumped() {
    let booted = fake_image_version(
        "stable-daily-43.20260527",
        "6.13.4-200.fc41",
        "abc1234deadbeef",
        "2026-05-27T12:00:00Z",
    );
    let target = fake_image_version(
        "stable-daily-43.20260530",
        "6.13.5-201.fc41",
        "def5678feedface",
        "2026-05-30T12:00:00Z",
    );
    let items = build_stack_items(Some(&booted), Some(&target), None);
    let by_label: std::collections::HashMap<&str, &StackItem> =
        items.iter().map(|i| (i.label, i)).collect();
    assert!(by_label["Image"].bumped);
    assert!(by_label["Kernel"].bumped);
    assert!(by_label["Revision"].bumped);
    assert!(by_label["Built"].bumped);
    assert_eq!(
        by_label["Image"].current.as_deref(),
        Some("stable-daily-43.20260527")
    );
    assert_eq!(by_label["Image"].target, "stable-daily-43.20260530");
    assert_eq!(by_label["Revision"].target, "def5678");
}

#[test]
fn stack_items_marks_unchanged_components_not_bumped() {
    // Same booted as target — every row is bumped=false. Used when the
    // user is browsing the changelog for the version they're already on.
    let v = fake_image_version(
        "stable-daily-43.20260530",
        "6.13.5-201.fc41",
        "def5678feedface",
        "2026-05-30T12:00:00Z",
    );
    let items = build_stack_items(Some(&v), Some(&v), None);
    for item in &items {
        assert!(!item.bumped, "{} should not be bumped", item.label);
    }
}

#[test]
fn stack_items_without_booted_treat_target_as_bumped() {
    // bootc-status missing → every component is unknown on the "from"
    // side and should render as bumped so the user sees the values they
    // would land on.
    let target = fake_image_version(
        "stable-daily-43.20260530",
        "6.13.5-201.fc41",
        "def5678feedface",
        "2026-05-30T12:00:00Z",
    );
    let items = build_stack_items(None, Some(&target), None);
    for item in &items {
        assert!(item.bumped, "{} should be bumped", item.label);
        assert!(item.current.is_none());
    }
}

// ── Dakota scenarios ─────────────────────────────────────────────────
// Dakota's registry data is bare: no kernel annotation anywhere, an
// empty revision on some builds, and a `version` annotation that may
// be just a date ("20260530") rather than the Bluefin-style dated
// stream ("stable-daily-43.20260530"). These cases verify the Stack
// section degrades sensibly.

#[test]
fn stack_items_dakota_no_kernel_either_side_hides_kernel_row() {
    // Both sides empty kernel → row should be omitted entirely so the
    // user doesn't see "— → —".
    let booted = fake_image_version("20260527", "", "abc1234", "2026-05-27T12:00:00Z");
    let target = fake_image_version("20260530", "", "def5678", "2026-05-30T12:00:00Z");
    let items = build_stack_items(Some(&booted), Some(&target), None);
    assert!(
        !items.iter().any(|i| i.label == "Kernel"),
        "Kernel row should be hidden when both sides are empty"
    );
    // Image / Revision / Built still appear.
    assert!(items.iter().any(|i| i.label == "Image"));
    assert!(items.iter().any(|i| i.label == "Revision"));
    assert!(items.iter().any(|i| i.label == "Built"));
}

#[test]
fn stack_items_dakota_uses_host_kernel_as_fallback() {
    // Registry side has no kernel, but uname -r is known — show the
    // host kernel on the current side so the user can at least see what
    // they're actually running.
    let booted = fake_image_version("20260527", "", "abc1234", "2026-05-27T12:00:00Z");
    let target = fake_image_version("20260530", "", "def5678", "2026-05-30T12:00:00Z");
    let items = build_stack_items(Some(&booted), Some(&target), Some("7.0.7"));
    let kernel = items.iter().find(|i| i.label == "Kernel");
    assert!(
        kernel.is_some(),
        "Kernel row should be present when host_kernel is known"
    );
    let k = kernel.unwrap();
    assert_eq!(k.current.as_deref(), Some("7.0.7"));
    assert_eq!(k.target, "");
    // One-sided data → not flagged as bumped (we can't know if it
    // actually changed).
    assert!(!k.bumped);
}

#[test]
fn stack_items_dakota_omits_revision_when_target_missing() {
    let target = fake_image_version("20260530", "", "", "2026-05-30T12:00:00Z");
    let items = build_stack_items(None, Some(&target), None);
    assert!(
        !items.iter().any(|i| i.label == "Revision"),
        "Revision should be hidden when target revision is empty"
    );
}

// ── extract_yyyymmdd_date ────────────────────────────────────────────

#[test]
fn extract_date_from_bare_date() {
    assert_eq!(
        extract_yyyymmdd_date("20260530"),
        chrono::NaiveDate::from_ymd_opt(2026, 5, 30)
    );
}

#[test]
fn extract_date_from_dotted_stream_tag() {
    assert_eq!(
        extract_yyyymmdd_date("latest.20260530"),
        chrono::NaiveDate::from_ymd_opt(2026, 5, 30)
    );
}

#[test]
fn extract_date_from_dashed_bluefin_tag() {
    assert_eq!(
        extract_yyyymmdd_date("stable-daily-43.20260602"),
        chrono::NaiveDate::from_ymd_opt(2026, 6, 2)
    );
}

#[test]
fn extract_date_rejects_non_date_runs() {
    // 8-digit hex sha is not a date.
    assert_eq!(extract_yyyymmdd_date("abc12345"), None);
    // 12-digit run shouldn't be sliced into a date.
    assert_eq!(extract_yyyymmdd_date("000020260530"), None);
    assert_eq!(extract_yyyymmdd_date("latest"), None);
}

// ── find_booted_match ────────────────────────────────────────────────

#[test]
fn find_booted_match_exact_version() {
    let v1 = fake_image_version(
        "stable-daily-43.20260527",
        "6.13.4",
        "abc1234",
        "2026-05-27T12:00:00Z",
    );
    let v2 = fake_image_version(
        "stable-daily-43.20260530",
        "6.13.5",
        "def5678",
        "2026-05-30T12:00:00Z",
    );
    let versions = vec![v1, v2];
    let hit = find_booted_match(&versions, "stable-daily-43.20260530");
    assert!(hit.is_some());
    assert_eq!(hit.unwrap().version, "stable-daily-43.20260530");
}

#[test]
fn find_booted_match_substring_handles_dakota_anchor() {
    // Dakota's os-release IMAGE_VERSION="20260530", but the registry
    // entry's version annotation is "latest.20260530". Substring match
    // gets us there.
    let v = fake_image_version("latest.20260530", "", "abc1234", "2026-05-30T12:00:00Z");
    let versions = vec![v];
    let hit = find_booted_match(&versions, "20260530");
    assert!(hit.is_some());
}

#[test]
fn find_booted_match_date_fallback() {
    // Booted anchor is "latest" (no date), but we know the host's
    // booted date — wait, anchor would carry the date in the os-release
    // form. This guards the parse path: anchor "20260530" matches v.date
    // even if the version string is something unrelated.
    let v = fake_image_version("local-build", "", "abc1234", "2026-05-30T12:00:00Z");
    let mut v_dated = v.clone();
    v_dated.date = chrono::NaiveDate::from_ymd_opt(2026, 5, 30).unwrap();
    let versions = vec![v_dated];
    let hit = find_booted_match(&versions, "20260530");
    assert!(hit.is_some());
}

#[test]
fn find_booted_match_returns_none_for_unrelated_anchor() {
    let v = fake_image_version("latest.20260530", "", "abc1234", "2026-05-30T12:00:00Z");
    let versions = vec![v];
    assert!(find_booted_match(&versions, "foobar").is_none());
}

// ── parse_os_release_field ───────────────────────────────────────────

const SAMPLE_OS_RELEASE: &str = r#"NAME="Bluefin Dakota"
PRETTY_NAME="Bluefin Dakota"
ID=dakota
VERSION_ID="43"
IMAGE_ID=dakota
VARIANT_ID=dakota
LOGO=bluefin
"#;

#[test]
fn os_release_pretty_name_unquoted() {
    assert_eq!(
        parse_os_release_field(SAMPLE_OS_RELEASE, "PRETTY_NAME"),
        Some("Bluefin Dakota".to_string())
    );
}

#[test]
fn os_release_unquoted_value() {
    assert_eq!(
        parse_os_release_field(SAMPLE_OS_RELEASE, "ID"),
        Some("dakota".to_string())
    );
}

#[test]
fn os_release_missing_key_returns_none() {
    assert_eq!(parse_os_release_field(SAMPLE_OS_RELEASE, "BUILD_ID"), None);
}

#[test]
fn os_release_empty_value_skipped() {
    // VARIANT="" should NOT be returned — empty strings aren't useful.
    let content = "ID=fedora\nVARIANT=\"\"\nLOGO=fedora\n";
    assert_eq!(parse_os_release_field(content, "VARIANT"), None);
    // But ID still wins.
    assert_eq!(
        parse_os_release_field(content, "ID"),
        Some("fedora".to_string())
    );
}

#[test]
fn os_release_first_match_wins() {
    // os-release CAN have duplicate keys in pathological cases — first
    // occurrence wins (matches the read order).
    let content = "ID=first\nID=second\n";
    assert_eq!(
        parse_os_release_field(content, "ID"),
        Some("first".to_string())
    );
}

// ── strip_date_suffix ────────────────────────────────────────────────
// Mirror of the parser in registry_client::strip_date_suffix but a
// separate implementation lives here for the home page's tag parsing.
// Tests guard against the two diverging.

#[test]
fn strip_date_suffix_dot_form() {
    assert_eq!(
        strip_date_suffix("stable-daily-43.20260527"),
        Some("stable-daily-43".to_string())
    );
}

#[test]
fn strip_date_suffix_dash_form() {
    assert_eq!(
        strip_date_suffix("lts-hwe-20260224"),
        Some("lts-hwe".to_string())
    );
}

#[test]
fn strip_date_suffix_rejects_too_short() {
    assert_eq!(strip_date_suffix("stable-2026"), None);
}

#[test]
fn strip_date_suffix_rejects_non_digits() {
    assert_eq!(strip_date_suffix("stable-20260abc"), None);
}

#[test]
fn strip_date_suffix_rejects_no_separator() {
    assert_eq!(strip_date_suffix("stable20260527"), None);
}

#[test]
fn strip_date_suffix_bare_date_returns_none() {
    // 20260527 alone is 8 digits but has no separator — so strip can't
    // detect where to split. The bare-date case is owned by
    // parse_dated_tag with stream==""; strip_date_suffix only handles
    // prefixed forms.
    assert_eq!(strip_date_suffix("20260527"), None);
}

// ── parse_image_ref_fields ───────────────────────────────────────────

#[test]
fn parse_image_ref_fields_empty_returns_placeholders() {
    let (name, tag, org) = parse_image_ref_fields("");
    assert_eq!(name, "Unknown");
    assert_eq!(tag, "latest");
    assert_eq!(org, "unknown");
}

#[test]
fn parse_image_ref_fields_full_ref() {
    let (name, tag, org) = parse_image_ref_fields("ghcr.io/ublue-os/bluefin:stable");
    assert_eq!(name, "bluefin");
    assert_eq!(tag, "stable");
    assert_eq!(org, "ublue-os");
}

#[test]
fn parse_image_ref_fields_no_colon_defaults_to_latest() {
    let (name, tag, org) = parse_image_ref_fields("ghcr.io/projectbluefin/dakota");
    assert_eq!(name, "dakota");
    assert_eq!(tag, "latest");
    assert_eq!(org, "projectbluefin");
}

#[test]
fn parse_image_ref_fields_single_segment() {
    let (name, tag, org) = parse_image_ref_fields("standalone");
    assert_eq!(name, "standalone");
    assert_eq!(tag, "latest");
    assert_eq!(org, "unknown");
}

// ── get_real_deployments_from_json ───────────────────────────────────
// Validates the parsing that turns a bootc-status JSON blob into a
// list of MockDeployment rows for the history page.

#[test]
fn deployments_parses_booted_only() {
    // get_real_deployments_from_json uses the "current"/"previous"/
    // "staged" labels — matching the home-page UI's history row badges
    // — instead of the raw bootc terms. The mapping:
    //    status.booted   → state="current"  (the row badged "Active")
    //    status.rollback → state="previous"
    //    status.staged   → state="staged"
    let json: Value = serde_json::from_str(r#"{
            "status": {
                "booted": {
                    "image": {
                        "image": {"image": "ghcr.io/projectbluefin/dakota:latest"},
                        "timestamp": "2026-05-28T16:14:49Z",
                        "imageDigest": "sha256:baea47c64413bc61a6901e99ceb052bee843d05d406fe33513497863074d84ef"
                    }
                }
            }
        }"#).unwrap();
    let deps = get_real_deployments_from_json(&json).expect("parses");
    assert_eq!(deps.len(), 1);
    assert_eq!(deps[0].state, "current");
    assert_eq!(deps[0].title, "dakota");
    assert_eq!(deps[0].tag, "latest");
}

#[test]
fn deployments_parses_booted_and_rollback() {
    let json: Value = serde_json::from_str(
        r#"{
            "status": {
                "booted": {
                    "image": {
                        "image": {"image": "ghcr.io/projectbluefin/dakota:latest"},
                        "timestamp": "2026-05-28T16:14:49Z",
                        "imageDigest": "sha256:aaaa"
                    }
                },
                "rollback": {
                    "image": {
                        "image": {"image": "ghcr.io/projectbluefin/dakota:latest"},
                        "timestamp": "2026-05-27T14:21:59Z",
                        "imageDigest": "sha256:bbbb"
                    }
                }
            }
        }"#,
    )
    .unwrap();
    let deps = get_real_deployments_from_json(&json).expect("parses");
    let states: Vec<&str> = deps.iter().map(|d| d.state.as_str()).collect();
    assert!(states.contains(&"current"), "states: {states:?}");
    assert!(states.contains(&"previous"), "states: {states:?}");
    assert_eq!(deps.len(), 2);
}

#[test]
fn deployments_parses_staged_first() {
    // The function emits in fixed order: staged, current, previous. So
    // even though staged represents "the next boot", it appears first
    // in the result vector. Verify that ordering.
    let json: Value = serde_json::from_str(
        r#"{
            "status": {
                "staged": {
                    "image": {
                        "image": {"image": "ghcr.io/projectbluefin/dakota-nvidia:latest"},
                        "timestamp": "2026-05-30T02:20:28Z",
                        "imageDigest": "sha256:cccc"
                    }
                },
                "booted": {
                    "image": {
                        "image": {"image": "ghcr.io/projectbluefin/dakota:latest"},
                        "timestamp": "2026-05-28T16:14:49Z",
                        "imageDigest": "sha256:aaaa"
                    }
                }
            }
        }"#,
    )
    .unwrap();
    let deps = get_real_deployments_from_json(&json).expect("parses");
    assert_eq!(deps.len(), 2);
    assert_eq!(deps[0].state, "staged");
    assert_eq!(deps[0].title, "dakota-nvidia");
    assert_eq!(deps[1].state, "current");
}

#[test]
fn deployments_returns_none_for_empty_status() {
    let json: Value = serde_json::from_str(r#"{"status": {}}"#).unwrap();
    // No booted entry → can't surface anything useful.
    assert!(get_real_deployments_from_json(&json).is_none());
}

#[test]
fn deployments_returns_none_when_status_missing() {
    let json: Value = serde_json::from_str(r#"{"apiVersion": "v1"}"#).unwrap();
    assert!(get_real_deployments_from_json(&json).is_none());
}
