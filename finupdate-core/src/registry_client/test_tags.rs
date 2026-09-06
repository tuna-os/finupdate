use super::*;

#[test]
fn sha_only_tag_true_for_40_lowercase_hex() {
    let sha = "a".repeat(40);
    assert!(is_sha_only_tag(&sha));
    let mixed = "0123456789abcdef".repeat(2) + "01234567";
    assert_eq!(mixed.len(), 40);
    assert!(is_sha_only_tag(&mixed));
}

#[test]
fn sha_only_tag_false_for_wrong_length() {
    assert!(!is_sha_only_tag(&"a".repeat(39)));
    assert!(!is_sha_only_tag(&"a".repeat(41)));
    assert!(!is_sha_only_tag(""));
}

#[test]
fn sha_only_tag_false_for_uppercase_hex() {
    assert!(!is_sha_only_tag(&"A".repeat(40)));
}

#[test]
fn sha_only_tag_false_for_non_hex_chars() {
    let mut s = "a".repeat(39);
    s.push('g');
    assert!(!is_sha_only_tag(&s));
}

#[test]
fn parse_dated_tag_bare_date_for_latest_or_empty_stream() {
    assert_eq!(
        parse_dated_tag("20260114", "latest"),
        NaiveDate::from_ymd_opt(2026, 1, 14)
    );
    assert_eq!(
        parse_dated_tag("20260114", ""),
        NaiveDate::from_ymd_opt(2026, 1, 14)
    );
}

#[test]
fn parse_dated_tag_bare_date_rejected_for_other_streams() {
    assert_eq!(parse_dated_tag("20260114", "stable"), None);
}

#[test]
fn parse_dated_tag_stream_suffixed_prefix_match() {
    assert_eq!(
        parse_dated_tag("stable-daily-43.20260222", "stable"),
        NaiveDate::from_ymd_opt(2026, 2, 22)
    );
    assert_eq!(
        parse_dated_tag("lts-hwe-20260224", "lts"),
        NaiveDate::from_ymd_opt(2026, 2, 24)
    );
    assert_eq!(
        parse_dated_tag("latest.20260527", "latest"),
        NaiveDate::from_ymd_opt(2026, 5, 27)
    );
}

#[test]
fn parse_dated_tag_sub_revision_stripped() {
    assert_eq!(
        parse_dated_tag("testing-43.20260308.1", "testing"),
        NaiveDate::from_ymd_opt(2026, 3, 8)
    );
    assert_eq!(
        parse_dated_tag("stable-43.20260301.2", "stable"),
        NaiveDate::from_ymd_opt(2026, 3, 1)
    );
}

#[test]
fn parse_dated_tag_exact_stream_match() {
    assert_eq!(
        parse_dated_tag("stable-20260301", "stable"),
        NaiveDate::from_ymd_opt(2026, 3, 1)
    );
}

#[test]
fn parse_dated_tag_dot_separator_not_confused_with_sub_revision() {
    // "stable.20260527" — the 8-digit suffix after the dot must NOT be
    // treated as a stripped sub-revision, or the date gets corrupted.
    assert_eq!(
        parse_dated_tag("stable.20260527", "stable"),
        NaiveDate::from_ymd_opt(2026, 5, 27)
    );
}

#[test]
fn parse_dated_tag_rejects_non_matching_stream() {
    assert_eq!(parse_dated_tag("stable-daily-43.20260222", "testing"), None);
}

#[test]
fn parse_dated_tag_rejects_invalid_calendar_date() {
    assert_eq!(parse_dated_tag("stable-20261301", "stable"), None); // month 13
}

#[test]
fn parse_dated_tag_none_when_no_date_present() {
    assert_eq!(parse_dated_tag("latest", "latest"), None);
    assert_eq!(parse_dated_tag("", "latest"), None);
}

#[test]
fn strip_date_suffix_stream_suffixed() {
    assert_eq!(
        strip_date_suffix("stable-daily-43.20260527"),
        Some("stable-daily-43".to_string())
    );
    assert_eq!(
        strip_date_suffix("lts-hwe-20260224"),
        Some("lts-hwe".to_string())
    );
}

#[test]
fn strip_date_suffix_sub_revision_stripped() {
    assert_eq!(
        strip_date_suffix("testing-43.20260308.1"),
        Some("testing-43".to_string())
    );
}

#[test]
fn strip_date_suffix_none_for_undated_or_bare_tags() {
    assert_eq!(strip_date_suffix("latest"), None);
    assert_eq!(strip_date_suffix("20260114"), None);
}
