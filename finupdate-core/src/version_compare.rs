//! RPM version comparison, for telling an upgrade from a downgrade.
//!
//! The UI renders a target version in success-green to mean "this is being
//! upgraded". Until this module existed the decision was `booted != target`,
//! so switching to an older stream — a rollback, or Dakota's F44 → Bluefin's
//! F43 — painted every row green while every package was in fact going
//! backwards. Green on a downgrade tells the user the opposite of the truth
//! about what is about to happen to their system.
//!
//! [`rpmvercmp`] is a port of RPM's own algorithm (`lib/rpmvercmp.c`), which
//! is what actually decides ordering on these systems. Reimplementing it
//! approximately would produce a UI that disagrees with the package manager,
//! so the awkward parts — `~` sorting before everything, digits outranking
//! letters, leading zeros ignored — are kept rather than simplified away.

use std::cmp::Ordering;

/// Anything that isn't alphanumeric or `~`/`^` is a separator in RPM's
/// grammar, so `1.2.3` and `1_2_3` compare equal.
fn skip_separators(s: &[u8]) -> &[u8] {
    let mut i = 0;
    while i < s.len() && !s[i].is_ascii_alphanumeric() && s[i] != b'~' && s[i] != b'^' {
        i += 1;
    }
    &s[i..]
}

/// Split off a leading run of digits (or letters), returning it and the rest.
fn take_segment(s: &[u8], numeric: bool) -> (&[u8], &[u8]) {
    let mut i = 0;
    while i < s.len()
        && (if numeric {
            s[i].is_ascii_digit()
        } else {
            s[i].is_ascii_alphabetic()
        })
    {
        i += 1;
    }
    (&s[..i], &s[i..])
}

/// Leading zeros are not significant, so `007` and `7` compare equal.
fn trim_leading_zeros(s: &[u8]) -> &[u8] {
    let mut i = 0;
    while i + 1 < s.len() && s[i] == b'0' {
        i += 1;
    }
    &s[i..]
}

/// Compare two RPM version segments the way RPM does.
///
/// Handles only the segment grammar, not epoch/release splitting — see
/// [`compare_evr`] for full `epoch:version-release` strings.
pub fn rpmvercmp(a: &str, b: &str) -> Ordering {
    if a == b {
        return Ordering::Equal;
    }

    let (mut a, mut b) = (a.as_bytes(), b.as_bytes());

    loop {
        a = skip_separators(a);
        b = skip_separators(b);

        // `~` sorts *before* everything, including the empty string. This is
        // what makes 1.0~rc1 older than 1.0 — pre-release ordering.
        match (a.first(), b.first()) {
            (Some(b'~'), Some(b'~')) => {
                a = &a[1..];
                b = &b[1..];
                continue;
            }
            (Some(b'~'), _) => return Ordering::Less,
            (_, Some(b'~')) => return Ordering::Greater,
            _ => {}
        }

        // `^` sorts before the empty string but after everything else — it
        // marks a snapshot *after* a release.
        match (a.first(), b.first()) {
            (Some(b'^'), Some(b'^')) => {
                a = &a[1..];
                b = &b[1..];
                continue;
            }
            (Some(b'^'), None) => return Ordering::Greater,
            (None, Some(b'^')) => return Ordering::Less,
            (Some(b'^'), _) => return Ordering::Less,
            (_, Some(b'^')) => return Ordering::Greater,
            _ => {}
        }

        if a.is_empty() || b.is_empty() {
            break;
        }

        // Take a run of digits from one side and a run of the same class from
        // the other. A numeric segment always outranks an alphabetic one, so
        // 1.10 > 1.a.
        let numeric = a[0].is_ascii_digit();
        let (seg_a, rest_a) = take_segment(a, numeric);
        let (seg_b, rest_b) = take_segment(b, numeric);

        if seg_b.is_empty() {
            // b's segment is of the other class: numeric wins.
            return if numeric {
                Ordering::Greater
            } else {
                Ordering::Less
            };
        }

        let ord = if numeric {
            // Compare as numbers without parsing: strip leading zeros, then
            // longer-is-larger, then lexically. Avoids overflow on the absurd
            // release strings some builds carry.
            let (ta, tb) = (trim_leading_zeros(seg_a), trim_leading_zeros(seg_b));
            ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb))
        } else {
            seg_a.cmp(seg_b)
        };

        if ord != Ordering::Equal {
            return ord;
        }

        a = rest_a;
        b = rest_b;
    }

    // Whichever still has content is newer.
    match (a.is_empty(), b.is_empty()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        (false, false) => unreachable!("loop only exits when one side is empty"),
    }
}

/// Compare full `epoch:version-release` strings, e.g. `5:5.8.4-1.fc44`.
///
/// Epoch dominates when present — that is its whole purpose — then version,
/// then release. A missing epoch is 0.
pub fn compare_evr(a: &str, b: &str) -> Ordering {
    let split = |s: &str| -> (String, String, String) {
        let (epoch, rest) = match s.split_once(':') {
            Some((e, r)) if e.chars().all(|c| c.is_ascii_digit()) && !e.is_empty() => {
                (e.to_string(), r)
            }
            _ => ("0".to_string(), s),
        };
        let (ver, rel) = match rest.split_once('-') {
            Some((v, r)) => (v.to_string(), r.to_string()),
            None => (rest.to_string(), String::new()),
        };
        (epoch, ver, rel)
    };

    let (ea, va, ra) = split(a);
    let (eb, vb, rb) = split(b);

    rpmvercmp(&ea, &eb)
        .then_with(|| rpmvercmp(&va, &vb))
        .then_with(|| rpmvercmp(&ra, &rb))
}

/// How a target version relates to the currently-booted one.
///
/// `Unknown` exists so the UI can decline to claim a direction it cannot
/// establish — an empty or unparseable side renders neutral rather than
/// guessing. Green is a claim; make it only when it is earned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionChange {
    Upgrade,
    Downgrade,
    Same,
    Unknown,
}

pub fn classify(current: &str, target: &str) -> VersionChange {
    let (c, t) = (current.trim(), target.trim());
    if c.is_empty() || t.is_empty() || c == "—" || t == "—" {
        return VersionChange::Unknown;
    }
    match compare_evr(t, c) {
        Ordering::Greater => VersionChange::Upgrade,
        Ordering::Less => VersionChange::Downgrade,
        Ordering::Equal => VersionChange::Same,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orders_plain_numeric_segments() {
        assert_eq!(rpmvercmp("1.0", "1.1"), Ordering::Less);
        assert_eq!(rpmvercmp("1.10", "1.9"), Ordering::Greater);
        assert_eq!(rpmvercmp("1.0", "1.0"), Ordering::Equal);
    }

    /// Numeric beats alphabetic, and leading zeros are not significant.
    #[test]
    fn follows_rpm_segment_rules() {
        assert_eq!(rpmvercmp("1.1", "1.a"), Ordering::Greater);
        assert_eq!(rpmvercmp("1.007", "1.7"), Ordering::Equal);
        assert_eq!(rpmvercmp("1.2.3", "1_2_3"), Ordering::Equal);
    }

    /// `~` sorts before everything — this is what makes a release candidate
    /// older than its release, and getting it backwards would show a genuine
    /// upgrade as a downgrade.
    #[test]
    fn tilde_sorts_before_everything() {
        assert_eq!(rpmvercmp("1.0~rc1", "1.0"), Ordering::Less);
        assert_eq!(rpmvercmp("1.0", "1.0~rc1"), Ordering::Greater);
        assert_eq!(rpmvercmp("1.0~rc1", "1.0~rc2"), Ordering::Less);
    }

    #[test]
    fn epoch_dominates_version() {
        // Without epoch handling this reads as a downgrade.
        assert_eq!(
            compare_evr("5:1.0-1.fc44", "4:9.9-1.fc44"),
            Ordering::Greater
        );
        assert_eq!(compare_evr("1.0-1.fc44", "0:1.0-1.fc44"), Ordering::Equal);
    }

    /// The case that motivated this: Dakota's F44 packages against Bluefin's
    /// F43. Every one of these rendered success-green before.
    #[test]
    fn classifies_the_real_downgrades_we_were_painting_green() {
        for (booted, target) in [
            ("50.3-1.fc44", "49.7-1.fc43"),
            ("26.1.4-4.fc44", "25.3.6-6.fc43"),
            ("5:5.8.4-1.fc44", "5:5.8.2-1.fc43"),
            ("1.16.3-1.fc44", "1.15.1-1.fc43"),
            ("259.7-1.fc44", "258.7-1.fc43"),
        ] {
            assert_eq!(
                classify(booted, target),
                VersionChange::Downgrade,
                "{booted} -> {target} should be a downgrade"
            );
        }
    }

    #[test]
    fn classifies_upgrades_and_equals() {
        assert_eq!(
            classify("49.7-1.fc43", "50.3-1.fc44"),
            VersionChange::Upgrade
        );
        assert_eq!(classify("1.0-1", "1.0-1"), VersionChange::Same);
    }

    /// Never guess a direction from a missing side.
    #[test]
    fn unknown_when_a_side_is_missing() {
        assert_eq!(classify("", "1.0"), VersionChange::Unknown);
        assert_eq!(classify("—", "1.0"), VersionChange::Unknown);
        assert_eq!(classify("1.0", ""), VersionChange::Unknown);
    }
}
