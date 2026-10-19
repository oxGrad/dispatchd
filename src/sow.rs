//! Scope-of-work deliverable references. The team's issues use `D.P.a.b`
//! (milestone `a`, deliverable `b`); older `/todo` rows carry the legacy
//! `M<a>D<b>` form (see migration 0004). Both parse to the same `SowRef`,
//! which always displays as `D.P.a.b`, so the two group together in reports
//! without rewriting any stored data.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SowRef {
    pub milestone: u32,
    /// `None` = the milestone as a whole (`D.P.1`, `M1`).
    pub deliverable: Option<u32>,
}

impl fmt::Display for SowRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.deliverable {
            Some(d) => write!(f, "D.P.{}.{d}", self.milestone),
            None => write!(f, "D.P.{}", self.milestone),
        }
    }
}

/// Parses a whole string as a SOW ref, case-insensitively: `D.P.1.2`,
/// `D.P.1`, `M1D2` or `M1`. Surrounding whitespace is ignored; anything
/// else (extra text, a trailing dot, `D.P.` with no number) is `None`.
pub fn parse(s: &str) -> Option<SowRef> {
    let s = s.trim().to_ascii_uppercase();
    if let Some(rest) = s.strip_prefix("D.P.") {
        let mut parts = rest.split('.');
        let milestone = number(parts.next()?)?;
        let deliverable = match parts.next() {
            Some(part) => Some(number(part)?),
            None => None,
        };
        if parts.next().is_some() {
            return None;
        }
        return Some(SowRef {
            milestone,
            deliverable,
        });
    }
    let rest = s.strip_prefix('M')?;
    let (milestone, deliverable) = match rest.split_once('D') {
        Some((m, d)) => (number(m)?, Some(number(d)?)),
        None => (number(rest)?, None),
    };
    Some(SowRef {
        milestone,
        deliverable,
    })
}

/// A non-empty run of ASCII digits, nothing else (no sign, no spaces).
fn number(s: &str) -> Option<u32> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// The SOW ref carried by an issue's labels: only labels named
/// `sow:<ref>` (prefix case-insensitive) count. When several parse, the
/// most specific wins (a deliverable over a milestone-only ref), then the
/// lowest. Returns warnings for unparseable `sow:` labels and for
/// ambiguity, for the caller to log.
pub fn from_labels<'a>(labels: impl IntoIterator<Item = &'a str>) -> (Option<SowRef>, Vec<String>) {
    let mut found = Vec::new();
    let mut warnings = Vec::new();
    for label in labels {
        let Some(value) = strip_prefix_ignore_case(label, "sow:") else {
            continue;
        };
        match parse(value) {
            Some(sow) => found.push(sow),
            None => warnings.push(format!("ignoring unparseable SOW label {label:?}")),
        }
    }
    found.sort_by_key(|sow| (sow.deliverable.is_none(), *sow));
    found.dedup();
    if found.len() > 1 {
        warnings.push(format!(
            "several SOW labels ({}), using {}",
            found
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", "),
            found[0]
        ));
    }
    (found.first().copied(), warnings)
}

fn strip_prefix_ignore_case<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &s[prefix.len()..])
}

/// The fallback when an issue has no `sow:` label: the first `D.P.`-form
/// token anywhere in the title (`"D.P.1.2 Payment gateway"`,
/// `"[D.P.1.2] ..."`). The legacy `M1D2` form is deliberately not searched
/// for in titles - words like "M2" would false-match.
pub fn from_title(title: &str) -> Option<SowRef> {
    title
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '.'))
        .map(|token| token.trim_end_matches('.'))
        .filter(|token| token.len() > 4 && token[..4].eq_ignore_ascii_case("D.P."))
        .find_map(parse)
}

/// How a free-text `entries.sow_ref` groups in reports: its normalised
/// `D.P.` form when it parses, the trimmed text as typed otherwise.
pub fn normalize(raw: &str) -> String {
    parse(raw).map_or_else(|| raw.trim().to_string(), |sow| sow.to_string())
}

/// `title` with its SOW ref token removed, for showing a deliverable's name
/// ("D.P.1.2 Payment gateway integration" -> "Payment gateway
/// integration"). Strips the first `D.P.` token plus any brackets, dashes
/// or colons left dangling around it; returns the title unchanged (trimmed)
/// when it has no such token.
pub fn strip_from_title(title: &str) -> String {
    let lower = title.to_ascii_lowercase();
    let Some(start) = lower.find("d.p.") else {
        return title.trim().to_string();
    };
    let end = title[start..]
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '.'))
        .map_or(title.len(), |offset| start + offset);
    if parse(title[start..end].trim_end_matches('.')).is_none() {
        return title.trim().to_string();
    }
    let junk = |c: char| c.is_whitespace() || "[]()-:\u{2013}\u{2014}".contains(c);
    let before = title[..start].trim_end_matches(junk);
    let after = title[end..].trim_start_matches(junk);
    match (before.is_empty(), after.is_empty()) {
        (true, _) => after.trim_end().to_string(),
        (false, true) => before.trim_start().to_string(),
        (false, false) => format!("{} {}", before.trim_start(), after.trim_end()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sow(milestone: u32, deliverable: Option<u32>) -> SowRef {
        SowRef {
            milestone,
            deliverable,
        }
    }

    #[test]
    fn parse_accepts_both_forms_case_insensitively() {
        assert_eq!(parse("D.P.1.2"), Some(sow(1, Some(2))));
        assert_eq!(parse("D.P.1"), Some(sow(1, None)));
        assert_eq!(parse("M1D2"), Some(sow(1, Some(2))));
        assert_eq!(parse("M1"), Some(sow(1, None)));
        assert_eq!(parse("d.p.12.30"), Some(sow(12, Some(30))));
        assert_eq!(parse("m1d2"), Some(sow(1, Some(2))));
        assert_eq!(parse("  D.P.1.2 "), Some(sow(1, Some(2))));
    }

    #[test]
    fn parse_rejects_anything_else() {
        for bad in [
            "",
            "Payment stuff",
            "D.P.",
            "D.P.1.",
            "D.P.1.2.3",
            "D.P.a.b",
            "D.P.-1",
            "M",
            "MD2",
            "M1D",
            "M1D2X",
            "M1 D2",
            "TBD",
        ] {
            assert_eq!(parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn display_is_always_the_dp_form() {
        assert_eq!(parse("M1D2").unwrap().to_string(), "D.P.1.2");
        assert_eq!(parse("m3").unwrap().to_string(), "D.P.3");
    }

    #[test]
    fn ordering_is_by_milestone_then_deliverable() {
        let mut refs = vec![
            sow(2, Some(1)),
            sow(1, Some(10)),
            sow(1, None),
            sow(1, Some(2)),
        ];
        refs.sort();
        assert_eq!(
            refs,
            vec![
                sow(1, None),
                sow(1, Some(2)),
                sow(1, Some(10)),
                sow(2, Some(1))
            ]
        );
    }

    #[test]
    fn from_labels_reads_only_sow_prefixed_labels() {
        let (got, warnings) = from_labels(["bug", "D.P.9.9", "SOW:D.P.1.2", "priority"]);
        assert_eq!(got, Some(sow(1, Some(2))));
        assert!(warnings.is_empty(), "{warnings:?}");

        let (got, _) = from_labels(["sow:M1D3"]);
        assert_eq!(got, Some(sow(1, Some(3))));

        assert_eq!(from_labels(["bug", "D.P.1.2"]).0, None);
    }

    #[test]
    fn from_labels_prefers_the_most_specific_then_the_lowest_and_warns() {
        let (got, warnings) = from_labels(["sow:D.P.2", "sow:D.P.1.5", "sow:D.P.1.3"]);
        assert_eq!(got, Some(sow(1, Some(3))));
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("several SOW labels"), "{warnings:?}");
    }

    #[test]
    fn from_labels_warns_about_an_unparseable_sow_label() {
        let (got, warnings) = from_labels(["sow:TBD"]);
        assert_eq!(got, None);
        assert!(warnings[0].contains("sow:TBD"), "{warnings:?}");
    }

    #[test]
    fn from_labels_ignores_a_duplicate_of_the_same_ref() {
        let (got, warnings) = from_labels(["sow:D.P.1.2", "sow:M1D2"]);
        assert_eq!(got, Some(sow(1, Some(2))));
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn from_title_finds_a_dp_token_anywhere() {
        assert_eq!(
            from_title("D.P.1.2 Payment gateway integration"),
            Some(sow(1, Some(2)))
        );
        assert_eq!(from_title("[D.P.1.2] Payment"), Some(sow(1, Some(2))));
        assert_eq!(from_title("Payment (d.p.3.1)"), Some(sow(3, Some(1))));
        assert_eq!(from_title("Ends with D.P.4.2."), Some(sow(4, Some(2))));
    }

    #[test]
    fn from_title_never_matches_the_legacy_form_or_noise() {
        assert_eq!(from_title("Upgrade to M2 Mac runners"), None);
        assert_eq!(from_title("M1D2 payment"), None);
        assert_eq!(from_title("Read the D.P. docs"), None);
        assert_eq!(from_title("No ref here"), None);
    }

    #[test]
    fn normalize_groups_both_forms_and_keeps_free_text() {
        assert_eq!(normalize("M1D2"), "D.P.1.2");
        assert_eq!(normalize("D.P.1.2"), "D.P.1.2");
        assert_eq!(normalize("  Payment stuff "), "Payment stuff");
    }

    #[test]
    fn strip_from_title_removes_the_ref_and_its_punctuation() {
        assert_eq!(
            strip_from_title("D.P.1.2 Payment gateway integration"),
            "Payment gateway integration"
        );
        assert_eq!(
            strip_from_title("[D.P.1.2] Payment gateway"),
            "Payment gateway"
        );
        assert_eq!(
            strip_from_title("D.P.1.2 - Payment gateway"),
            "Payment gateway"
        );
        assert_eq!(
            strip_from_title("Payment gateway (D.P.1.2)"),
            "Payment gateway"
        );
        assert_eq!(
            strip_from_title("Admin D.P.1.3: dashboard"),
            "Admin dashboard"
        );
        assert_eq!(strip_from_title("No ref here"), "No ref here");
        assert_eq!(strip_from_title("Read the D.P. docs"), "Read the D.P. docs");
    }
}
