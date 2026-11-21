//! `wfd_*` parameter text: parse and encode (Wi-Fi Display spec, section on RTSP parameters).
//!
//! Field order follows the spec grammar. Hardware-confirmed (miracast.md §15): profile bit 1 =
//! CHP and CEA bit 7 = 1920x1080p30 (the TB336FU picked `02 02 00000080` and sent High-profile
//! 1920x1080 at ~30 fps). Still `[verify]`: the level bits and the other CEA bits.

use crate::message::ParseError;

const MAX_CODECS: usize = 16;
const MAX_URL: usize = 256;

fn perr<T>(msg: impl Into<String>) -> Result<T, ParseError> {
    Err(ParseError(msg.into()))
}

fn hex(tok: &str, what: &str) -> Result<u32, ParseError> {
    if tok.is_empty() || tok.len() > 8 {
        return perr(format!("bad hex field {what}"));
    }
    u32::from_str_radix(tok, 16).or_else(|_| perr(format!("bad hex field {what}")))
}

/// Splits `name: value` lines of a `text/parameters` body. Lines without a colon are names
/// only (GET_PARAMETER bodies).
pub fn parse_params(body: &str) -> Vec<(&str, &str)> {
    body.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| match l.split_once(':') {
            Some((n, v)) => (n.trim(), v.trim()),
            None => (l, ""),
        })
        .collect()
}

/// One H.264 entry of `wfd_video_formats`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct H264Codec {
    /// Bit 0 CBP, bit 1 CHP.
    pub profile: u8,
    /// Bit 0 3.1, 1 3.2, 2 4.1, 3 4.2 [verify: the TB336FU picked 0x02 but its SPS says 4.0,
    /// which fits no reading of this table; size the decoder from the SPS]. In an offer this is
    /// the highest level supported; a source may pick any lower one (seen on hardware: the
    /// TB336FU picked 3.2 against our 4.1 offer).
    pub level: u8,
    pub cea: u32,
    pub vesa: u32,
    pub hh: u32,
    pub latency: u8,
    pub min_slice_size: u16,
    pub slice_enc_params: u16,
    pub frame_rate_control: u8,
    pub max_hres: Option<u16>,
    pub max_vres: Option<u16>,
}

/// `wfd_video_formats`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoFormats {
    /// Bits 0-2: table (0 CEA, 1 VESA, 2 HH); bits 3-7: index of the native resolution.
    pub native: u8,
    pub preferred_display_mode: u8,
    pub codecs: Vec<H264Codec>,
}

impl VideoFormats {
    pub fn encode(&self) -> String {
        let mut out = format!("{:02X} {:02X}", self.native, self.preferred_display_mode);
        for (i, c) in self.codecs.iter().enumerate() {
            out.push_str(if i == 0 { " " } else { ", " });
            let dim = |v: Option<u16>| v.map_or("none".to_string(), |v| format!("{v:04X}"));
            out.push_str(&format!(
                "{:02X} {:02X} {:08X} {:08X} {:08X} {:02X} {:04X} {:04X} {:02X} {} {}",
                c.profile,
                c.level,
                c.cea,
                c.vesa,
                c.hh,
                c.latency,
                c.min_slice_size,
                c.slice_enc_params,
                c.frame_rate_control,
                dim(c.max_hres),
                dim(c.max_vres)
            ));
        }
        out
    }

    /// Parses a value; `none` gives `Ok(None)`.
    pub fn parse(s: &str) -> Result<Option<Self>, ParseError> {
        let s = s.trim();
        if s.eq_ignore_ascii_case("none") {
            return Ok(None);
        }
        let toks: Vec<&str> = s
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|t| !t.is_empty())
            .collect();
        let [native, preferred, entries @ ..] = toks.as_slice() else {
            return perr("wfd_video_formats: wrong field count");
        };
        if entries.len() % 11 != 0 {
            return perr("wfd_video_formats: wrong field count");
        }
        if entries.len() / 11 > MAX_CODECS {
            return perr("wfd_video_formats: too many codec entries");
        }
        let dim = |t: &str| -> Result<Option<u16>, ParseError> {
            if t.eq_ignore_ascii_case("none") {
                Ok(None)
            } else {
                Ok(Some(hex(t, "max resolution")? as u16))
            }
        };
        let mut codecs = Vec::new();
        for e in entries.as_chunks::<11>().0 {
            let [pr, lv, cea, vesa, hh, lat, mss, sep, frc, hres, vres] = e;
            codecs.push(H264Codec {
                profile: hex(pr, "profile")? as u8,
                level: hex(lv, "level")? as u8,
                cea: hex(cea, "cea")?,
                vesa: hex(vesa, "vesa")?,
                hh: hex(hh, "hh")?,
                latency: hex(lat, "latency")? as u8,
                min_slice_size: hex(mss, "min-slice-size")? as u16,
                slice_enc_params: hex(sep, "slice-enc-params")? as u16,
                frame_rate_control: hex(frc, "frame-rate-control")? as u8,
                max_hres: dim(hres)?,
                max_vres: dim(vres)?,
            });
        }
        Ok(Some(VideoFormats {
            native: hex(native, "native")? as u8,
            preferred_display_mode: hex(preferred, "preferred-display-mode")? as u8,
            codecs,
        }))
    }

    /// True if `chosen` (the source's M4 pick) fits inside what we offered.
    pub fn accepts(&self, chosen: &VideoFormats) -> bool {
        !chosen.codecs.is_empty()
            && chosen.codecs.iter().all(|c| {
                c.profile != 0
                    && c.level != 0
                    && self.codecs.iter().any(|o| {
                        c.profile & !o.profile == 0
                            // The offered level is a maximum: lower levels fit.
                            && c.level <= o.level
                            && c.cea & !o.cea == 0
                            && c.vesa & !o.vesa == 0
                            && c.hh & !o.hh == 0
                    })
            })
    }
}

/// A CEA resolution/frame-rate entry (bit 7 = 1080p30 confirmed on hardware; the other bit
/// positions are `[verify]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolution {
    pub width: u16,
    pub height: u16,
    pub fps: u8,
    pub interlaced: bool,
}

/// CEA bit position to resolution.
pub fn cea_resolution(bit: u32) -> Option<Resolution> {
    let (w, h, fps, i) = match bit {
        0 => (640, 480, 60, false),
        1 => (720, 480, 60, false),
        2 => (720, 480, 60, true),
        3 => (720, 576, 50, false),
        4 => (720, 576, 50, true),
        5 => (1280, 720, 30, false),
        6 => (1280, 720, 60, false),
        7 => (1920, 1080, 30, false),
        8 => (1920, 1080, 60, false),
        9 => (1920, 1080, 60, true),
        10 => (1280, 720, 25, false),
        11 => (1280, 720, 50, false),
        12 => (1920, 1080, 25, false),
        13 => (1920, 1080, 50, false),
        14 => (1920, 1080, 50, true),
        15 => (1280, 720, 24, false),
        16 => (1920, 1080, 24, false),
        _ => return None,
    };
    Some(Resolution {
        width: w,
        height: h,
        fps,
        interlaced: i,
    })
}

/// The resolution of a CEA mask with exactly one bit set.
pub fn single_cea(mask: u32) -> Option<Resolution> {
    (mask.count_ones() == 1)
        .then(|| cea_resolution(mask.trailing_zeros()))
        .flatten()
}

/// One `wfd_audio_codecs` entry, e.g. `LPCM 00000003 00`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioCodec {
    pub name: String,
    pub modes: u32,
    pub latency: u8,
}

/// `wfd_audio_codecs`; `none` is `Ok(None)` from [`parse_audio_codecs`].
pub fn parse_audio_codecs(s: &str) -> Result<Option<Vec<AudioCodec>>, ParseError> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("none") {
        return Ok(None);
    }
    let mut out = Vec::new();
    for entry in s.split(',') {
        let t: Vec<&str> = entry.split_whitespace().collect();
        let [name, modes, latency] = t.as_slice() else {
            return perr("wfd_audio_codecs: wrong field count");
        };
        if out.len() >= MAX_CODECS {
            return perr("wfd_audio_codecs: too many entries");
        }
        out.push(AudioCodec {
            name: name.to_ascii_uppercase(),
            modes: hex(modes, "audio modes")?,
            latency: hex(latency, "audio latency")? as u8,
        });
    }
    Ok(Some(out))
}

pub fn encode_audio_codecs(codecs: &Option<Vec<AudioCodec>>) -> String {
    match codecs {
        None => "none".to_string(),
        Some(v) => v
            .iter()
            .map(|c| format!("{} {:08X} {:02X}", c.name, c.modes, c.latency))
            .collect::<Vec<_>>()
            .join(", "),
    }
}

/// True if every chosen codec is offered with a subset of the modes. `None` (no audio) is
/// always acceptable.
pub fn audio_accepts(offered: &Option<Vec<AudioCodec>>, chosen: &Option<Vec<AudioCodec>>) -> bool {
    match (offered, chosen) {
        (_, None) => true,
        (None, Some(_)) => false,
        (Some(o), Some(c)) => c.iter().all(|c| {
            o.iter()
                .any(|o| o.name == c.name && c.modes & !o.modes == 0)
        }),
    }
}

/// `wfd_client_rtp_ports`: `RTP/AVP/UDP;unicast 19000 0 mode=play`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientRtpPorts {
    pub profile: String,
    pub port0: u16,
    pub port1: u16,
}

impl ClientRtpPorts {
    pub fn encode(&self) -> String {
        format!("{} {} {} mode=play", self.profile, self.port0, self.port1)
    }

    pub fn parse(s: &str) -> Result<Self, ParseError> {
        let t: Vec<&str> = s.split_whitespace().collect();
        let [profile, port0, port1, ..] = t.as_slice() else {
            return perr("wfd_client_rtp_ports: wrong field count");
        };
        let num = |x: &str| {
            x.parse::<u16>()
                .or_else(|_| perr("wfd_client_rtp_ports: bad port"))
        };
        Ok(ClientRtpPorts {
            profile: profile.to_string(),
            port0: num(port0)?,
            port1: num(port1)?,
        })
    }
}

/// `wfd_presentation_URL`: `rtsp://192.168.49.1/wfd1.0/streamid=0 none`. Returns the first
/// URL (the second item is the optional second stream, ignored).
pub fn parse_presentation_url(s: &str) -> Result<String, ParseError> {
    let url = s.split_whitespace().next().unwrap_or("");
    if !url.starts_with("rtsp://") || url.len() > MAX_URL || url.chars().any(char::is_control) {
        return perr("wfd_presentation_URL is not a valid rtsp:// URL");
    }
    Ok(url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "00 00 01 04 000001E1 00000000 00000000 00 0000 0000 00 none none, \
                          02 04 000001E1 00000000 00000000 00 0000 0000 00 none none";

    #[test]
    fn video_formats_roundtrip() {
        let v = VideoFormats::parse(SAMPLE).unwrap().unwrap();
        assert_eq!(v.codecs.len(), 2);
        // SAMPLE has 01 04 as first codec fields after "00 00": profile 01, level 04.
        assert_eq!(v.codecs[0].profile, 1);
        assert_eq!(v.codecs[0].cea, 0x1E1);
        assert_eq!(v.codecs[1].profile, 2);
        let again = VideoFormats::parse(&v.encode()).unwrap().unwrap();
        assert_eq!(v, again);
        assert_eq!(VideoFormats::parse("none").unwrap(), None);
    }

    #[test]
    fn video_formats_rejects_garbage() {
        for bad in [
            "",
            "00",
            "00 00 01",
            "zz 00 01 04 1 1 1 00 0 0 00 none none",
            "00 00 01 04 123456789 0 0 0 0 0 0 none none",
        ] {
            assert!(VideoFormats::parse(bad).is_err(), "{bad}");
        }
        let many = format!("00 00{}", " 01 04 1 0 0 00 0 0 00 none none".repeat(17));
        assert!(VideoFormats::parse(&many).is_err());
    }

    #[test]
    fn subset_check() {
        let ours = VideoFormats::parse(SAMPLE).unwrap().unwrap();
        let pick = |s: &str| VideoFormats::parse(s).unwrap().unwrap();
        // 1080p30 (bit 7 = 0x80) CBP level 4.1.
        assert!(ours.accepts(&pick(
            "00 00 01 04 00000080 00000000 00000000 00 0000 0000 00 none none"
        )));
        // 1080p60 (0x100) is in 0x1E1? 0x1E1 = bits 0,5,6,7,8: yes.
        assert!(ours.accepts(&pick(
            "00 00 02 04 00000100 00000000 00000000 00 0000 0000 00 none none"
        )));
        // bit 9 (1080i60) was not offered.
        assert!(!ours.accepts(&pick(
            "00 00 01 04 00000200 00000000 00000000 00 0000 0000 00 none none"
        )));
        // level 4.2 not offered.
        assert!(!ours.accepts(&pick(
            "00 00 01 08 00000080 00000000 00000000 00 0000 0000 00 none none"
        )));
        // A lower level than offered fits (hardware: TB336FU picked CHP 3.2 against 4.1).
        assert!(ours.accepts(&pick(
            "00 00 02 02 00000080 00000000 00000000 00 0000 0000 00 none none"
        )));
        assert!(ours.accepts(&pick(
            "00 00 01 01 00000080 00000000 00000000 00 0000 0000 00 none none"
        )));
        // VESA not offered.
        assert!(!ours.accepts(&pick(
            "00 00 01 04 00000080 00000001 00000000 00 0000 0000 00 none none"
        )));
    }

    #[test]
    fn cea_table() {
        assert_eq!(single_cea(0x80).unwrap().height, 1080);
        assert_eq!(single_cea(0x80).unwrap().fps, 30);
        assert_eq!(single_cea(0x81), None);
        assert_eq!(single_cea(1 << 31), None);
    }

    #[test]
    fn audio_and_ports() {
        let a = parse_audio_codecs("LPCM 00000003 00, AAC 00000001 00").unwrap();
        assert_eq!(a.as_ref().unwrap().len(), 2);
        assert_eq!(parse_audio_codecs(&encode_audio_codecs(&a)).unwrap(), a);
        assert_eq!(parse_audio_codecs("none").unwrap(), None);
        assert!(parse_audio_codecs("LPCM 3").is_err());
        let offered = parse_audio_codecs("LPCM 00000003 00").unwrap();
        assert!(audio_accepts(
            &offered,
            &parse_audio_codecs("LPCM 00000002 00").unwrap()
        ));
        assert!(!audio_accepts(
            &offered,
            &parse_audio_codecs("LPCM 00000004 00").unwrap()
        ));
        assert!(!audio_accepts(&None, &offered));
        assert!(audio_accepts(&None, &None));

        let p = ClientRtpPorts::parse("RTP/AVP/UDP;unicast 19000 0 mode=play").unwrap();
        assert_eq!(p.port0, 19000);
        assert_eq!(ClientRtpPorts::parse(&p.encode()).unwrap(), p);
        assert!(ClientRtpPorts::parse("RTP/AVP/UDP;unicast 99999 0").is_err());
    }

    #[test]
    fn url_and_params() {
        assert_eq!(
            parse_presentation_url("rtsp://192.168.49.1/wfd1.0/streamid=0 none").unwrap(),
            "rtsp://192.168.49.1/wfd1.0/streamid=0"
        );
        assert!(parse_presentation_url("http://x").is_err());
        assert!(parse_presentation_url("").is_err());
        let p = parse_params("wfd_video_formats: 00 00\r\nwfd_audio_codecs\r\n\r\nwfd_x:y:z\r\n");
        assert_eq!(p[0], ("wfd_video_formats", "00 00"));
        assert_eq!(p[1], ("wfd_audio_codecs", ""));
        assert_eq!(p[2], ("wfd_x", "y:z"));
    }
}
