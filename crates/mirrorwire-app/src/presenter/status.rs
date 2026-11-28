//! Presenter status types shown in the GUI window (GUI builds only).

use super::CaptureStatus;
use mirrorwire_media::encode::EncoderConfig;
use mirrorwire_net::diag::{FirewallHint, IncomingAttempt};

#[cfg(feature = "gui")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentPhase {
    Starting,
    WaitingForCapture,
    Streaming,
    Stopped,
    Failed,
}

#[cfg(feature = "gui")]
#[derive(Debug, Clone)]
pub struct EncodeHud {
    /// Encoder actually running (e.g. "h264_vaapi", "filmcraft-h264enc").
    pub encoder: String,
    /// Mean encode time over the last ~1 s window (steadier than `encode_ms`).
    pub encode_avg_ms: f64,
    pub pack_ms: f64,
    pub convert_ms: f64,
    pub upload_ms: f64,
    pub encode_ms: f64,
    pub path: String,
    pub capture_w: u32,
    pub capture_h: u32,
    pub source_fps: f64,
    /// Re-sent frames per second while the screen was static.
    pub repeat_fps: f64,
    pub fallback_note: Option<String>,
}

#[cfg(feature = "gui")]
impl Default for EncodeHud {
    fn default() -> Self {
        Self {
            encoder: String::new(),
            encode_avg_ms: 0.0,
            pack_ms: 0.0,
            convert_ms: 0.0,
            upload_ms: 0.0,
            encode_ms: 0.0,
            path: String::new(),
            capture_w: 0,
            capture_h: 0,
            source_fps: 0.0,
            repeat_fps: 0.0,
            fallback_note: None,
        }
    }
}

#[cfg(feature = "gui")]
#[derive(Debug, Clone)]
pub struct PresentStatus {
    pub phase: PresentPhase,
    pub name: String,
    pub pairing_code: Option<String>,
    pub uri: Option<String>,
    pub fingerprint: Option<String>,
    pub encoder: Option<String>,
    pub source: Option<String>,
    pub listen_addr: Option<String>,
    pub advertised: Vec<String>,
    pub port: u16,
    pub viewer_connected: bool,
    pub error: Option<String>,
    /// Why the start is taking a while (an earlier session still closing), shown in the window.
    pub note: Option<String>,
    pub rc_summary: Option<String>,
    pub conversion: Option<String>,
    pub firewall: Option<FirewallHint>,
    pub attempts: Vec<IncomingAttempt>,
    pub hud: EncodeHud,
    pub settings: EncoderConfig,
    pub capture: CaptureStatus,
}

#[cfg(feature = "gui")]
impl PresentStatus {
    pub(super) fn new(name: String, settings: EncoderConfig, port: u16) -> Self {
        Self {
            phase: PresentPhase::Starting,
            name,
            pairing_code: None,
            uri: None,
            fingerprint: None,
            encoder: None,
            source: None,
            listen_addr: None,
            advertised: Vec::new(),
            port,
            viewer_connected: false,
            error: None,
            note: None,
            rc_summary: None,
            conversion: None,
            firewall: None,
            attempts: Vec::new(),
            hud: EncodeHud::default(),
            settings,
            capture: CaptureStatus::default(),
        }
    }
}
