//! Wi-Fi frequency to band and channel (IEEE 802.11 channel numbering), for the trace.

/// `(band, channel)` for a centre frequency in MHz, e.g. 2462 -> `("2.4 GHz", 11)`.
pub fn channel(mhz: u32) -> Option<(&'static str, u32)> {
    match mhz {
        2484 => Some(("2.4 GHz", 14)),
        2412..=2472 if (mhz - 2407).is_multiple_of(5) => Some(("2.4 GHz", (mhz - 2407) / 5)),
        5160..=5885 if mhz.is_multiple_of(5) => Some(("5 GHz", (mhz - 5000) / 5)),
        5955..=7115 if (mhz - 5950).is_multiple_of(5) => Some(("6 GHz", (mhz - 5950) / 5)),
        _ => None,
    }
}

/// `"2462 MHz (2.4 GHz ch 11)"`, or just the MHz if unknown.
pub fn describe(mhz: u32) -> String {
    match channel(mhz) {
        Some((band, ch)) => format!("{mhz} MHz ({band} ch {ch})"),
        None => format!("{mhz} MHz"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channels_seen_on_hardware() {
        assert_eq!(channel(2462), Some(("2.4 GHz", 11)));
        assert_eq!(channel(5300), Some(("5 GHz", 60)));
        assert_eq!(channel(2412), Some(("2.4 GHz", 1)));
        assert_eq!(channel(2484), Some(("2.4 GHz", 14)));
        assert_eq!(channel(5955), Some(("6 GHz", 1)));
        assert_eq!(channel(0), None);
        assert_eq!(channel(2463), None);
        assert_eq!(describe(2462), "2462 MHz (2.4 GHz ch 11)");
        assert_eq!(describe(9999), "9999 MHz");
    }
}
