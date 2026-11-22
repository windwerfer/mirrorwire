//! The name the Android device most likely lists this computer under (decision D25,
//! miracast.md §16).
//!
//! The Wi-Fi Direct device name is set by the system (NetworkManager / wpa_supplicant), not by
//! MirrorWire. NetworkManager's documented D-Bus API has no property for it (neither
//! `Device.WifiP2P` nor the `wifi-p2p` setting), and wpa_supplicant's `P2PDeviceConfig`
//! `DeviceName` is on a D-Bus API that unprivileged users cannot read. So this is a **best
//! estimate** from the configured hostnames, not the advertised value itself: hostname1's
//! `PrettyHostname`, then `StaticHostname`. The transient hostname (DHCP) is not used: on the
//! T480s it was `fedora.fritz.box` while both configured names were empty and Android listed
//! the laptop without a name (2026-11-22).

/// Longest name we show; hostnames come from outside the app (AGENTS.md 1.9).
pub const MAX_NAME_CHARS: usize = 64;

/// What we know about the name the Android device shows for this computer.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SinkName {
    /// Most likely listed under this name (best estimate, see the module docs).
    Named(String),
    /// Neither a pretty nor a static hostname is set: Android most likely lists this computer
    /// without a name.
    Empty,
    /// Not known (hostname1 not reachable, e.g. inside the Flatpak, and no other answer):
    /// show nothing rather than a wrong "none".
    #[default]
    Unknown,
}

/// From hostname1's `PrettyHostname` and `StaticHostname` (both readable without root;
/// empty when unset). The pretty name wins.
pub fn from_hostname1(pretty: &str, static_name: &str) -> SinkName {
    clean(pretty)
        .or_else(|| clean(static_name))
        .map_or(SinkName::Empty, SinkName::Named)
}

/// From NetworkManager's `Settings.Hostname` ("the machine hostname stored in persistent
/// configuration", i.e. the static hostname), the fallback when hostname1 is not reachable.
/// An empty value says nothing about the pretty name, so it gives [`SinkName::Unknown`].
pub fn from_nm_hostname(hostname: &str) -> SinkName {
    clean(hostname).map_or(SinkName::Unknown, SinkName::Named)
}

/// Trimmed, without control characters, at most [`MAX_NAME_CHARS`]; `None` when nothing is
/// left.
fn clean(s: &str) -> Option<String> {
    let s: String = s
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_NAME_CHARS)
        .collect();
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pretty_name_wins_then_static() {
        assert_eq!(
            from_hostname1("Thomas' Laptop", "t480s"),
            SinkName::Named("Thomas' Laptop".into())
        );
        assert_eq!(from_hostname1("", "t480s"), SinkName::Named("t480s".into()));
        assert_eq!(
            from_hostname1("  \t ", "t480s"),
            SinkName::Named("t480s".into())
        );
    }

    #[test]
    fn both_empty_is_empty_not_unknown() {
        // The T480s case: Android shows a blank name.
        assert_eq!(from_hostname1("", ""), SinkName::Empty);
        assert_eq!(from_hostname1(" ", "\n"), SinkName::Empty);
    }

    #[test]
    fn nm_fallback_never_claims_empty() {
        assert_eq!(from_nm_hostname("t480s"), SinkName::Named("t480s".into()));
        assert_eq!(from_nm_hostname(""), SinkName::Unknown);
        assert_eq!(from_nm_hostname("  "), SinkName::Unknown);
    }

    #[test]
    fn hostile_names_are_bounded_and_cleaned() {
        let long = "x".repeat(10_000);
        let SinkName::Named(n) = from_hostname1(&long, "") else {
            panic!("expected a name");
        };
        assert_eq!(n.chars().count(), MAX_NAME_CHARS);
        assert_eq!(
            from_hostname1("a\u{1b}[31mb\0c", ""),
            SinkName::Named("a[31mbc".into())
        );
        assert_eq!(from_hostname1("\u{7}\u{0}", "\r"), SinkName::Empty);
    }

    #[test]
    fn default_is_unknown() {
        assert_eq!(SinkName::default(), SinkName::Unknown);
    }
}
