//! Wi-Fi Display information elements. The device-type bits 1:0 and the session-availability
//! bits 5:4 are confirmed on hardware (miracast.md §15): the TB336FU advertises 0x0010
//! (source, available) and logged our 0x0011 as "WFD DeviceInfo: 17" and listed us as a sink.
//! The other bitmap bits are not used.
//!
//! A WFD subelement is `id(1) length(2, big endian) payload`.

/// Subelement id of "WFD Device Information".
pub const SUBELEMENT_DEVICE_INFO: u8 = 0;
/// Default RTSP port of the sink (the "session management control port").
pub const DEFAULT_RTSP_PORT: u16 = 7236;

/// Device type, bits 1:0 of the device info bitmap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceType {
    Source = 0,
    PrimarySink = 1,
    SecondarySink = 2,
    DualRole = 3,
}

/// Session availability, bits 5:4.
const SESSION_AVAILABLE: u16 = 1 << 4;

/// WFD Device Information payload (6 bytes on the wire).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceInfo {
    pub device_type: DeviceType,
    pub session_available: bool,
    pub rtsp_port: u16,
    /// Maximum throughput in Mbit/s.
    pub max_throughput_mbps: u16,
}

impl DeviceInfo {
    /// Primary sink, available, no HDCP, no time sync, P2P connectivity.
    pub fn primary_sink(rtsp_port: u16, max_throughput_mbps: u16) -> Self {
        Self {
            device_type: DeviceType::PrimarySink,
            session_available: true,
            rtsp_port,
            max_throughput_mbps,
        }
    }

    fn bitmap(&self) -> u16 {
        let mut b = self.device_type as u16;
        if self.session_available {
            b |= SESSION_AVAILABLE;
        }
        b
    }

    /// Encode as a complete subelement (9 bytes).
    pub fn to_subelement(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(9);
        out.push(SUBELEMENT_DEVICE_INFO);
        out.extend_from_slice(&6u16.to_be_bytes());
        out.extend_from_slice(&self.bitmap().to_be_bytes());
        out.extend_from_slice(&self.rtsp_port.to_be_bytes());
        out.extend_from_slice(&self.max_throughput_mbps.to_be_bytes());
        out
    }
}

/// The subelements a plain sink publishes (only device information).
pub fn sink_device_info(rtsp_port: u16, max_throughput_mbps: u16) -> Vec<u8> {
    DeviceInfo::primary_sink(rtsp_port, max_throughput_mbps).to_subelement()
}

/// Split a peer's WFD IE bytes into `(id, payload)` subelements. Malformed or truncated
/// input ends the walk (hostile input from the air, AGENTS.md 1.9); it never panics.
pub fn subelements(mut ies: &[u8]) -> Vec<(u8, &[u8])> {
    let mut out = Vec::new();
    while let Some((&id, rest)) = ies.split_first() {
        let (Some(&hi), Some(&lo)) = (rest.first(), rest.get(1)) else {
            break;
        };
        let len = usize::from(u16::from_be_bytes([hi, lo]));
        let Some(payload) = rest.get(2..2 + len) else {
            break;
        };
        out.push((id, payload));
        ies = rest.get(2 + len..).unwrap_or(&[]);
    }
    out
}

/// Parse the device information subelement out of a peer's WFD IEs.
pub fn parse_device_info(ies: &[u8]) -> Option<DeviceInfo> {
    let (_, p) = subelements(ies)
        .into_iter()
        .find(|(id, p)| *id == SUBELEMENT_DEVICE_INFO && p.len() >= 6)?;
    let bitmap = u16::from_be_bytes([*p.first()?, *p.get(1)?]);
    let device_type = match bitmap & 0b11 {
        0 => DeviceType::Source,
        1 => DeviceType::PrimarySink,
        2 => DeviceType::SecondarySink,
        _ => DeviceType::DualRole,
    };
    Some(DeviceInfo {
        device_type,
        session_available: bitmap & (0b11 << 4) == SESSION_AVAILABLE,
        rtsp_port: u16::from_be_bytes([*p.get(2)?, *p.get(3)?]),
        max_throughput_mbps: u16::from_be_bytes([*p.get(4)?, *p.get(5)?]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sink_ie_matches_the_common_on_air_value() {
        // 00 0006 0011 1c44 0032 is what many sinks publish (port 7236 = 0x1c44, 50 Mbit/s).
        assert_eq!(
            sink_device_info(7236, 50),
            [0x00, 0x00, 0x06, 0x00, 0x11, 0x1c, 0x44, 0x00, 0x32]
        );
    }

    #[test]
    fn round_trip() {
        let info = DeviceInfo::primary_sink(7236, 50);
        assert_eq!(parse_device_info(&info.to_subelement()), Some(info));
    }

    #[test]
    fn source_is_recognised() {
        let ies = [0x00, 0x00, 0x06, 0x00, 0x10, 0x1c, 0x44, 0x00, 0x32];
        let info = parse_device_info(&ies).unwrap();
        assert_eq!(info.device_type, DeviceType::Source);
        assert!(info.session_available);
    }

    #[test]
    fn hostile_input_is_dropped_not_panicked() {
        assert!(parse_device_info(&[]).is_none());
        assert!(parse_device_info(&[0, 0xff, 0xff, 1]).is_none());
        assert!(parse_device_info(&[0, 0, 6, 1, 2]).is_none());
        assert!(subelements(&[1, 0]).is_empty());
        // Unknown subelement before ours is skipped.
        let mut ies = vec![7, 0, 1, 0xaa];
        ies.extend(sink_device_info(7236, 50));
        assert!(parse_device_info(&ies).is_some());
    }
}
