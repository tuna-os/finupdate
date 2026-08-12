//! Pure tag parsing for GHCR dated-image tags. No I/O, no state.
//!
//! Extracted from the former single-file `registry_client.rs`; the RegistryClient
//! HTTP layer calls these via `use tags::{...}`.

use chrono::NaiveDate;

pub(crate) fn is_sha_only_tag(tag: &str) -> bool {
    tag.len() == 40
        && tag
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

/// Probe a single tag's config blob for its `created` timestamp.
///
/// Returns the date in UTC (truncated to a NaiveDate to match the rest of
/// the version-list flow) or None on any failure: network error, an OCI
/// index (multi-arch) where we can't single-out a platform manifest, a
/// missing config digest, missing `created` field, or an unparseable
/// RFC3339 timestamp. The caller treats None as "don't include this tag."
///
/// Used by `RegistryClient::probe_sha_tag_dates` to surface history for
/// images that publish via sha-tagged manifests without dated tag names.
/// Extract a `NaiveDate` from a dated image tag, accepting the four conventions
/// observed across the bootc image families we support:
///
/// 1. **Stream-suffixed** (Bluefin, Aurora):
///    `stable-daily-43.20260222`, `lts-hwe-20260224`, `latest.20260527`
///    → accepted for stream `"stable"`, `"lts"`, `"latest"` respectively (via
///      prefix match — see the prefix rule below).
///
/// 2. **Sub-revisioned** (Bazzite):
///    `testing-43.20260308.1`, `stable-43.20260301.2`
///    → trailing `.N` (1–4 digits) is treated as a build sub-revision and
///      stripped before the date extraction.
///
/// 3. **Stream-prefix match** (Bluefin, Aurora, Bazzite):
///    A tag like `stable-daily-43.20260527` is accepted when the caller asks
///    for stream `"stable"` — the prefix begins with `"stable-"`. This lets
///    callers ask for the broad channel ("stable") and get back any tagged
///    build in that family, regardless of the fully-qualified stream
///    (e.g. `stable-daily-43`, `stable-gts-42`).
///
/// 4. **Bare date** (Dakota):
///    `20260114` — 8 digits, no prefix. Accepted only when the caller asks
///    for stream `"latest"` or `""` (the implicit / pointer-tag streams).
///
/// Returns the parsed calendar date, or `None` if the tag doesn't match any
/// of these patterns or fails calendar validation (e.g. month 13).
pub(crate) fn parse_dated_tag(tag: &str, stream: &str) -> Option<NaiveDate> {
    // (4) Bare YYYYMMDD with no separator — accepted only for the implicit
    //     streams that don't qualify their dates.
    if (stream == "latest" || stream.is_empty())
        && tag.len() == 8
        && tag.chars().all(|c| c.is_ascii_digit())
    {
        return NaiveDate::parse_from_str(tag, "%Y%m%d").ok();
    }

    // (2) Strip an optional trailing build sub-revision `.N` (1-4 digits) so
    //     `testing-43.20260308.1` reduces to `testing-43.20260308`.
    let base = if let Some(idx) = tag.rfind('.') {
        let suffix = &tag[idx + 1..];
        if (1..=4).contains(&suffix.len()) && suffix.chars().all(|c| c.is_ascii_digit()) {
            // Only strip if doing so leaves a date-shaped tail. Otherwise
            // we'd corrupt something like `stable.20260527` (where the `.`
            // is the date separator, not a sub-revision).
            let candidate = &tag[..idx];
            if candidate.len() >= 8
                && candidate[candidate.len() - 8..]
                    .chars()
                    .all(|c| c.is_ascii_digit())
            {
                candidate
            } else {
                tag
            }
        } else {
            tag
        }
    } else {
        tag
    };

    // (1)/(3) Find a trailing `-YYYYMMDD` or `.YYYYMMDD` on `base`, then
    //         check the prefix matches the requested stream.
    for sep in ['.', '-'] {
        if let Some(idx) = base.rfind(sep) {
            let date_str = &base[idx + 1..];
            if date_str.len() != 8 || !date_str.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            let prefix = &base[..idx];

            // Stream match rule: prefix is exactly `stream`, or begins with
            // `stream.` / `stream-` (qualified channel: stable-daily-43 etc.).
            let stream_matches = prefix == stream
                || prefix.starts_with(&format!("{}.", stream))
                || prefix.starts_with(&format!("{}-", stream));
            if !stream_matches {
                continue;
            }

            if let Some(date) = NaiveDate::parse_from_str(date_str, "%Y%m%d").ok() {
                return Some(date);
            }
        }
    }

    None
}

/// Remove the trailing `.YYYYMMDD[.N]` or `-YYYYMMDD[.N]` from a tag to get the
/// fully-qualified stream prefix.
///
/// Examples:
///   `stable-daily-43.20260527`     → `Some("stable-daily-43")`
///   `testing-43.20260308.1`        → `Some("testing-43")`   (sub-revision stripped)
///   `lts-hwe-20260224`             → `Some("lts-hwe")`
///   `latest`                       → `None`                  (no date)
///   `20260114`                     → `None`                  (no stream embedded)
pub fn strip_date_suffix(tag: &str) -> Option<String> {
    // Strip optional trailing sub-revision `.N` (1-4 digits) before looking
    // for the date — matches the Bazzite convention.
    let base = if let Some(idx) = tag.rfind('.') {
        let suffix = &tag[idx + 1..];
        if (1..=4).contains(&suffix.len())
            && suffix.chars().all(|c| c.is_ascii_digit())
            // Only strip when what's left ends in 8 digits — otherwise we'd
            // turn `stable.20260527` into `stable.20260527` again incorrectly.
            && idx >= 8
            && tag[..idx].as_bytes()[idx - 8..idx].iter().all(|b| b.is_ascii_digit())
        {
            &tag[..idx]
        } else {
            tag
        }
    } else {
        tag
    };

    for sep in ['.', '-'] {
        if let Some(pos) = base.rfind(sep) {
            let suffix = &base[pos + 1..];
            if suffix.len() == 8 && suffix.chars().all(|c| c.is_ascii_digit()) {
                return Some(base[..pos].to_string());
            }
        }
    }
    None
}
