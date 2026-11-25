use anyhow::{anyhow, bail, Result};

/// Default target bitrate for desktop content. Per-frame bits are based on
/// wall-clock PTS, so a static screen (few portal damage frames) still gets
/// a sharp picture instead of `bitrate / configured_fps` scraps.
pub const DEFAULT_BITRATE_KBPS: u32 = 40_000;
pub const DEFAULT_QP: u32 = 18;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncoderBackend {
    Auto,
    Vaapi,
    X264,
    VideoToolbox,
    /// Pure-Rust FilmCraft encoder (cargo feature `filmcraft`), never picked by `Auto`.
    Filmcraft,
}

impl std::str::FromStr for EncoderBackend {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        Ok(match s.to_ascii_lowercase().as_str() {
            "auto" => Self::Auto,
            "vaapi" => Self::Vaapi,
            "x264" | "software" => Self::X264,
            "videotoolbox" => Self::VideoToolbox,
            "filmcraft" => Self::Filmcraft,
            _ => {
                bail!("unknown encoder backend {s:?} (auto, vaapi, x264, videotoolbox, filmcraft)")
            }
        })
    }
}

impl EncoderBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Vaapi => "vaapi",
            Self::X264 => "x264",
            Self::VideoToolbox => "videotoolbox",
            Self::Filmcraft => "filmcraft",
        }
    }

    /// Whether this build can open the backend at all (FilmCraft is a cargo feature).
    pub fn compiled_in(self) -> bool {
        !matches!(self, Self::Filmcraft) || cfg!(feature = "filmcraft")
    }

    /// Backends worth offering on this platform in this build (for the GUI selector).
    pub fn selectable() -> Vec<Self> {
        let mut v = vec![Self::Auto];
        if cfg!(target_os = "linux") {
            v.push(Self::Vaapi);
        }
        if cfg!(target_os = "macos") {
            v.push(Self::VideoToolbox);
        }
        v.push(Self::X264);
        if Self::Filmcraft.compiled_in() {
            v.push(Self::Filmcraft);
        }
        v
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateControl {
    /// CBR for VAAPI/VideoToolbox, bitrate mode for x264.
    Auto,
    Cbr,
    Vbr,
    Cqp,
    Icq,
}

impl std::str::FromStr for RateControl {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        Ok(match s.to_ascii_lowercase().as_str() {
            "auto" => Self::Auto,
            "cbr" => Self::Cbr,
            "vbr" => Self::Vbr,
            "cqp" | "qp" => Self::Cqp,
            "icq" => Self::Icq,
            _ => bail!("unknown rate control {s:?} (auto, cbr, vbr, cqp, icq)"),
        })
    }
}

impl RateControl {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Cbr => "cbr",
            Self::Vbr => "vbr",
            Self::Cqp => "cqp",
            Self::Icq => "icq",
        }
    }

    pub fn all() -> &'static [Self] {
        &[Self::Auto, Self::Cbr, Self::Vbr, Self::Cqp, Self::Icq]
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EncoderConfig {
    /// `0` means "use the capture size" (resolved by the pipeline).
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub backend: EncoderBackend,
    pub rate_control: RateControl,
    pub qp: u32,
    /// GOP length in frames. `None` = 10 seconds at `fps`.
    pub keyint: Option<u32>,
    pub intra_refresh: bool,
    pub vaapi_device: String,
    /// Upper bound for the encoded picture (Extend mode: the chosen size). A larger result
    /// is scaled down keeping the aspect ratio; a smaller one is never scaled up.
    pub max_size: Option<(u32, u32)>,
}

impl Default for EncoderConfig {
    fn default() -> Self {
        Self {
            width: 0,
            height: 0,
            fps: 60,
            bitrate_kbps: DEFAULT_BITRATE_KBPS,
            backend: EncoderBackend::Auto,
            rate_control: RateControl::Auto,
            qp: DEFAULT_QP,
            keyint: None,
            intra_refresh: false,
            vaapi_device: "/dev/dri/renderD128".into(),
            max_size: None,
        }
    }
}

impl EncoderConfig {
    /// The encoded size for a capture of `src_w`x`src_h`.
    ///
    /// Default (0x0): the native size. One dimension given: the other follows the
    /// capture's aspect ratio. Both given: used as asked. The result is always
    /// even (4:2:0 chroma) and within [`MAX_ENCODE_PIXELS`] / [`MAX_ENCODE_EDGE`]
    /// (H.264 level 5.1, what hardware encoders accept): a larger request or capture is
    /// scaled down keeping its aspect ratio (5K -> 4096x2304). Never below 2x2.
    pub fn resolved_size(&self, src_w: u32, src_h: u32) -> (u32, u32) {
        let (src_w, src_h) = (src_w.max(1), src_h.max(1));
        let (w, h) = match (self.width, self.height) {
            (0, 0) => (src_w, src_h),
            (0, h) => (scale_dim(h, src_w, src_h), h),
            (w, 0) => (w, scale_dim(w, src_h, src_w)),
            (w, h) => (w, h),
        };
        let (w, h) = match self.max_size {
            Some((mw, mh)) if mw > 0 && mh > 0 && (w > mw || h > mh) => {
                let f = (f64::from(mw) / f64::from(w)).min(f64::from(mh) / f64::from(h));
                let fit = |v: u32| (f64::from(v) * f).round() as u32;
                (fit(w), fit(h))
            }
            _ => (w, h),
        };
        limit_size(w, h)
    }

    pub fn gop_frames(&self) -> u32 {
        self.keyint
            .filter(|k| *k > 0)
            .unwrap_or_else(|| self.fps.saturating_mul(10).max(1))
    }

    /// Settings that require the encoder to be re-opened (not just an IDR).
    pub fn reopen_needed(&self, other: &Self, out_w: u32, out_h: u32) -> bool {
        let (ew, eh) = self.resolved_size(out_w, out_h);
        ew != out_w
            || eh != out_h
            || self.bitrate_kbps != other.bitrate_kbps
            || self.backend != other.backend
            || self.rate_control != other.rate_control
            || self.qp != other.qp
            || self.keyint != other.keyint
            || self.intra_refresh != other.intra_refresh
            || self.vaapi_device != other.vaapi_device
            || self.fps != other.fps
    }
}

/// Longest edge the encoders are asked for (H.264 level 5.1 allows 4096 on VA-API hardware).
pub const MAX_ENCODE_EDGE: u32 = 4096;
/// Largest picture area (level 5.1: 36 864 macroblocks = 9 437 184 px; 4096x2304 fits).
pub const MAX_ENCODE_PIXELS: u64 = 4096 * 2304;

/// `num * other / den`, rounded.
fn scale_dim(num: u32, other: u32, den: u32) -> u32 {
    let v = (u64::from(num) * u64::from(other) + u64::from(den) / 2) / u64::from(den);
    u32::try_from(v).unwrap_or(u32::MAX)
}

/// Fits a size into the encoder limits, keeping the aspect ratio, and makes both edges even.
fn limit_size(w: u32, h: u32) -> (u32, u32) {
    let (w, h) = (f64::from(w.max(2)), f64::from(h.max(2)));
    let mut f = 1.0f64;
    let edge = w.max(h);
    if edge > f64::from(MAX_ENCODE_EDGE) {
        f = f64::from(MAX_ENCODE_EDGE) / edge;
    }
    let area = w * h * f * f;
    if area > MAX_ENCODE_PIXELS as f64 {
        f *= (MAX_ENCODE_PIXELS as f64 / area).sqrt();
    }
    let even = |v: f64| ((v * f) as u32 & !1).clamp(2, MAX_ENCODE_EDGE);
    (even(w), even(h))
}

/// Parses `1920x1080` or `1920×1080`.
pub fn parse_size(s: &str) -> Result<(u32, u32)> {
    let s = s.trim().replace('×', "x");
    let (w, h) = s
        .split_once('x')
        .ok_or_else(|| anyhow!("size must be WIDTHxHEIGHT, got {s:?}"))?;
    Ok((w.trim().parse()?, h.trim().parse()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_names_round_trip_and_junk_is_rejected() {
        for b in [
            EncoderBackend::Auto,
            EncoderBackend::Vaapi,
            EncoderBackend::X264,
            EncoderBackend::VideoToolbox,
            EncoderBackend::Filmcraft,
        ] {
            assert_eq!(b.as_str().parse::<EncoderBackend>().unwrap(), b);
        }
        assert_eq!(
            "SoftWare".parse::<EncoderBackend>().unwrap(),
            EncoderBackend::X264
        );
        let err = "nvenc".parse::<EncoderBackend>().unwrap_err().to_string();
        assert!(err.contains("nvenc") && err.contains("filmcraft"), "{err}");
    }

    #[test]
    fn selectable_backends_start_with_auto_and_are_compiled_in() {
        let v = EncoderBackend::selectable();
        assert_eq!(v.first(), Some(&EncoderBackend::Auto));
        assert!(v.contains(&EncoderBackend::X264));
        assert!(v.iter().all(|b| b.compiled_in()));
        assert_eq!(
            v.contains(&EncoderBackend::Filmcraft),
            cfg!(feature = "filmcraft")
        );
    }

    #[test]
    fn rate_control_names_round_trip() {
        for rc in RateControl::all() {
            assert_eq!(rc.as_str().parse::<RateControl>().unwrap(), *rc);
        }
        assert_eq!("QP".parse::<RateControl>().unwrap(), RateControl::Cqp);
        assert!("abr".parse::<RateControl>().is_err());
    }

    #[test]
    fn size_defaults_to_the_capture_and_follows_the_aspect_ratio() {
        let mut c = EncoderConfig::default();
        assert_eq!(c.resolved_size(1920, 1080), (1920, 1080));
        // Odd capture sizes are made even for 4:2:0.
        assert_eq!(c.resolved_size(1921, 1081), (1920, 1080));
        c.width = 1280;
        assert_eq!(c.resolved_size(1920, 1080), (1280, 720));
        c.width = 0;
        c.height = 720;
        assert_eq!(c.resolved_size(1920, 1080), (1280, 720));
        c.width = 800;
        assert_eq!(c.resolved_size(1920, 1080), (800, 720));
        // A zero-sized capture never divides by zero.
        assert_eq!(EncoderConfig::default().resolved_size(0, 0), (2, 2));
    }

    #[test]
    fn size_is_limited_to_the_encoder_maximum_and_max_size() {
        let c = EncoderConfig::default();
        let (w, h) = c.resolved_size(5120, 2880);
        assert_eq!((w, h), (4096, 2304));
        let (w, h) = c.resolved_size(8192, 8192);
        assert!(u64::from(w) * u64::from(h) <= MAX_ENCODE_PIXELS);
        assert!(w <= MAX_ENCODE_EDGE && h <= MAX_ENCODE_EDGE && w % 2 == 0 && h % 2 == 0);
        let c = EncoderConfig {
            max_size: Some((1280, 720)),
            ..Default::default()
        };
        assert_eq!(c.resolved_size(2560, 1440), (1280, 720));
        // Smaller than the limit: never scaled up.
        assert_eq!(c.resolved_size(640, 360), (640, 360));
        // A zero limit means "no limit".
        let c = EncoderConfig {
            max_size: Some((0, 720)),
            ..Default::default()
        };
        assert_eq!(c.resolved_size(2560, 1440), (2560, 1440));
    }

    #[test]
    fn gop_is_ten_seconds_unless_set() {
        let mut c = EncoderConfig::default();
        assert_eq!(c.gop_frames(), 600);
        c.keyint = Some(0);
        assert_eq!(c.gop_frames(), 600);
        c.keyint = Some(30);
        assert_eq!(c.gop_frames(), 30);
        c.keyint = None;
        c.fps = 0;
        assert_eq!(c.gop_frames(), 1);
    }

    #[test]
    fn reopen_is_needed_only_for_settings_the_encoder_cannot_change_live() {
        let a = EncoderConfig::default();
        assert!(!a.reopen_needed(&a.clone(), 1920, 1080));
        // The running output size no longer matches what the config resolves to.
        let sized = EncoderConfig {
            width: 1280,
            ..a.clone()
        };
        assert!(sized.reopen_needed(&sized, 1920, 1080));
        assert!(!sized.reopen_needed(&sized, 1280, 720));
        for b in [
            EncoderConfig {
                bitrate_kbps: 1000,
                ..a.clone()
            },
            EncoderConfig {
                backend: EncoderBackend::X264,
                ..a.clone()
            },
            EncoderConfig {
                rate_control: RateControl::Cqp,
                ..a.clone()
            },
            EncoderConfig {
                qp: 30,
                ..a.clone()
            },
            EncoderConfig {
                keyint: Some(5),
                ..a.clone()
            },
            EncoderConfig {
                intra_refresh: true,
                ..a.clone()
            },
            EncoderConfig {
                vaapi_device: "/dev/dri/renderD129".into(),
                ..a.clone()
            },
            EncoderConfig {
                fps: 30,
                ..a.clone()
            },
        ] {
            assert!(a.reopen_needed(&b, 1920, 1080), "{b:?}");
        }
    }

    #[test]
    fn parse_size_accepts_both_separators_and_rejects_junk() {
        assert_eq!(parse_size("1920x1080").unwrap(), (1920, 1080));
        assert_eq!(parse_size(" 1280 × 720 ").unwrap(), (1280, 720));
        assert!(parse_size("1920").is_err());
        assert!(parse_size("axb").is_err());
        assert!(parse_size("-1x5").is_err());
    }
}
