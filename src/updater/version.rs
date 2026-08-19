use crate::settings::UpdateChannel;

/// Strip a leading `v` / `V` and surrounding whitespace.
pub fn normalize_version(raw: &str) -> String {
    let s = raw.trim();
    let s = s
        .strip_prefix('v')
        .or_else(|| s.strip_prefix('V'))
        .unwrap_or(s);
    s.trim().to_string()
}

/// True when `raw` is a nightly stamp (`X.Y.Z-nightly.*`, optional `v` prefix).
pub fn is_nightly_version(raw: &str) -> bool {
    let version = normalize_version(raw);
    version
        .split_once('-')
        .map(|(_, pre)| {
            pre.split('.')
                .next()
                .unwrap_or("")
                .eq_ignore_ascii_case("nightly")
        })
        .unwrap_or(false)
}

/// True when `latest` is a greater semver-like triple than `current`.
///
/// Accepts optional pre-release suffix (`1.2.3-beta`); pre-release of the same
/// core version is treated as older than the plain release. Distinct pre-release
/// identifiers are compared (so `0.3.1-nightly.2` > `0.3.1-nightly.1`).
pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_semverish(latest), parse_semverish(current)) {
        (Some(l), Some(c)) => l > c,
        _ => latest != current && !latest.is_empty(),
    }
}

/// Whether the in-app updater should offer `latest` for `channel`.
///
/// The channel is the source of truth, not semver order:
/// - **Switching to Nightly** offers that channel’s current nightly even when
///   its core version is lower than the installed Stable.
/// - **Switching back to Stable** offers `/releases/latest` even when the
///   installed Nightly has a higher version (otherwise users get stuck).
/// - **Staying on a channel** still requires `latest` to be newer, so a local
///   or newer install is not treated as an update.
pub fn should_offer_on_channel(latest: &str, current: &str, channel: UpdateChannel) -> bool {
    let latest = normalize_version(latest);
    let current = normalize_version(current);
    if latest.is_empty() || latest == current {
        return false;
    }
    let current_is_nightly = is_nightly_version(&current);
    match channel {
        UpdateChannel::Stable => {
            if is_nightly_version(&latest) {
                return false;
            }
            if current_is_nightly {
                true
            } else {
                is_newer(&latest, &current)
            }
        }
        UpdateChannel::Nightly => {
            if !is_nightly_version(&latest) {
                return false;
            }
            if current_is_nightly {
                is_newer(&latest, &current)
            } else {
                true
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Semverish {
    major: u64,
    minor: u64,
    patch: u64,
    /// `None` = release (newer than any pre-release of the same core).
    pre: Option<Vec<PreIdent>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PreIdent {
    Num(u64),
    Text(String),
}

impl PartialOrd for PreIdent {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PreIdent {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match (self, other) {
            (Self::Num(a), Self::Num(b)) => a.cmp(b),
            (Self::Text(a), Self::Text(b)) => a.cmp(b),
            (Self::Num(_), Self::Text(_)) => std::cmp::Ordering::Less,
            (Self::Text(_), Self::Num(_)) => std::cmp::Ordering::Greater,
        }
    }
}

impl PartialOrd for Semverish {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Semverish {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (&self.pre, &other.pre) {
                (None, None) => std::cmp::Ordering::Equal,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (Some(_), None) => std::cmp::Ordering::Less,
                (Some(a), Some(b)) => a.cmp(b),
            })
    }
}

fn parse_semverish(s: &str) -> Option<Semverish> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    // Build metadata (`+…`) is ignored for precedence.
    let s = s.split_once('+').map(|(core, _)| core).unwrap_or(s);
    let (core, pre) = match s.split_once('-') {
        Some((core, rest)) if !rest.is_empty() => (core, Some(rest)),
        _ => (s, None),
    };
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    let pre = pre.map(|p| {
        p.split('.')
            .filter(|part| !part.is_empty())
            .map(|part| match part.parse::<u64>() {
                Ok(n) => PreIdent::Num(n),
                Err(_) => PreIdent::Text(part.to_string()),
            })
            .collect::<Vec<_>>()
    });
    let pre = match pre {
        Some(parts) if parts.is_empty() => None,
        other => other,
    };
    Some(Semverish {
        major,
        minor,
        patch,
        pre,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_v_prefix() {
        assert_eq!(normalize_version("v0.1.1"), "0.1.1");
        assert_eq!(normalize_version("V1.2.3"), "1.2.3");
        assert_eq!(normalize_version(" 0.2.0 "), "0.2.0");
    }

    #[test]
    fn is_newer_compares_triples() {
        assert!(is_newer("0.2.0", "0.1.1"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(!is_newer("0.1.1", "0.1.1"));
        assert!(!is_newer("0.1.0", "0.1.1"));
        assert!(is_newer("0.1.1", "0.1.1-beta"));
        assert!(!is_newer("0.1.1-beta", "0.1.1"));
        assert!(is_newer(
            "0.3.1-nightly.20260814120000",
            "0.3.1-nightly.20260813120000"
        ));
        assert!(!is_newer(
            "0.3.1-nightly.20260813120000",
            "0.3.1-nightly.20260814120000"
        ));
        assert!(!is_newer("0.3.1-nightly.20260813120000", "0.3.1"));
    }

    #[test]
    fn channel_switch_offers_that_stream_regardless_of_semver() {
        // Stable → Nightly: take nightly even when its core version is lower.
        assert!(should_offer_on_channel(
            "0.3.1-nightly.20260813155600",
            "0.3.2",
            UpdateChannel::Nightly
        ));
        // Nightly → Stable: take stable even when the nightly is “ahead”.
        assert!(should_offer_on_channel(
            "0.3.1",
            "0.3.2-nightly.20260813155600",
            UpdateChannel::Stable
        ));
        assert!(should_offer_on_channel(
            "0.3.2",
            "0.3.2-nightly.20260813155600",
            UpdateChannel::Stable
        ));
        // Same-core Nightly over the matching Stable (toggle to Nightly).
        assert!(should_offer_on_channel(
            "0.3.1-nightly.20260813155600",
            "0.3.1",
            UpdateChannel::Nightly
        ));
        // Nightly must not be offered on the Stable channel.
        assert!(!should_offer_on_channel(
            "0.3.1-nightly.20260813155600",
            "0.3.1",
            UpdateChannel::Stable
        ));
        // Already on that exact nightly.
        assert!(!should_offer_on_channel(
            "0.3.1-nightly.20260813155600",
            "0.3.1-nightly.20260813155600",
            UpdateChannel::Nightly
        ));
    }

    #[test]
    fn same_channel_still_requires_a_newer_build() {
        assert!(should_offer_on_channel(
            "0.3.2",
            "0.3.1",
            UpdateChannel::Stable
        ));
        assert!(!should_offer_on_channel(
            "0.3.1",
            "0.3.2",
            UpdateChannel::Stable
        ));
        assert!(!should_offer_on_channel(
            "0.3.2",
            "0.3.2",
            UpdateChannel::Stable
        ));
        // Newer local/dev Stable is not downgraded while staying on Stable.
        assert!(!should_offer_on_channel(
            "0.3.2",
            "0.3.3",
            UpdateChannel::Stable
        ));
        assert!(should_offer_on_channel(
            "0.3.2-nightly.20260813155600",
            "0.3.1-nightly.20260812000000",
            UpdateChannel::Nightly
        ));
        // Already on a newer nightly: do not roll back while staying on Nightly.
        assert!(!should_offer_on_channel(
            "0.3.1-nightly.20260813155600",
            "0.3.2-nightly.20260813155600",
            UpdateChannel::Nightly
        ));
        // Stable must not be offered as a Nightly target.
        assert!(!should_offer_on_channel(
            "0.3.2",
            "0.3.1-nightly.20260813155600",
            UpdateChannel::Nightly
        ));
    }

    #[test]
    fn nightly_tag_detection() {
        assert!(is_nightly_version("v0.3.1-nightly.20260813155600"));
        assert!(is_nightly_version("0.3.1-nightly.1"));
        assert!(!is_nightly_version("0.3.1"));
        assert!(!is_nightly_version("0.3.1-beta.1"));
        assert!(!is_nightly_version("nightly"));
    }
}
