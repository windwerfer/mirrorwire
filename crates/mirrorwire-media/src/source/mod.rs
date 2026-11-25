use crate::frame::RawFrame;
use anyhow::Result;
use mirrorwire_proto::control::CursorMode;
use mirrorwire_proto::cursor::CursorEvent;
use std::sync::Arc;
use std::time::Duration;

/// How the capture source delivers the mouse cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorCapture {
    /// Not captured at all.
    Hidden,
    /// Drawn into the video frames by the compositor (the classic way).
    Embedded,
    /// Left out of the video; position and image reported separately
    /// through a [`CursorSink`] ("cursor as coordinates").
    Metadata,
}

impl CursorCapture {
    /// How the viewer gets the cursor (`stream_info.cursor`).
    pub fn mode(self) -> CursorMode {
        match self {
            CursorCapture::Hidden => CursorMode::Hidden,
            CursorCapture::Embedded => CursorMode::Embedded,
            CursorCapture::Metadata => CursorMode::Separate,
        }
    }

    /// The name used in the status and the HUD.
    pub fn as_str(self) -> &'static str {
        self.mode().as_str()
    }
}

/// What the presenter captures: the existing screen, or a new virtual
/// monitor the viewer shows as an extra display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CaptureMode {
    /// Share an existing monitor (portal source type MONITOR).
    #[default]
    Mirror,
    /// Ask the compositor for a virtual monitor (portal source type VIRTUAL).
    Extend,
}

impl CaptureMode {
    pub fn as_str(self) -> &'static str {
        match self {
            CaptureMode::Mirror => "mirror",
            CaptureMode::Extend => "extend",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "mirror" | "monitor" => Some(CaptureMode::Mirror),
            "extend" | "virtual" => Some(CaptureMode::Extend),
            _ => None,
        }
    }

    pub fn all() -> [CaptureMode; 2] {
        [CaptureMode::Mirror, CaptureMode::Extend]
    }

    /// File name of the portal restore token for this mode. The portal ties
    /// a token to the source type it was granted for, so each mode keeps its own.
    pub fn restore_token_file(self) -> &'static str {
        match self {
            CaptureMode::Mirror => "portal_restore_token",
            CaptureMode::Extend => "portal_restore_token_extend",
        }
    }
}

/// Receives cursor changes from the capture thread. Must not block.
pub type CursorSink = Arc<dyn Fn(CursorEvent) + Send + Sync>;

pub mod camera;
#[cfg(all(target_os = "linux", feature = "portal"))]
#[allow(unsafe_code)]
pub mod portal;
#[cfg(all(target_os = "linux", feature = "portal"))]
pub mod portal_rt;
#[cfg(all(target_os = "linux", feature = "portal"))]
pub mod remote;
pub mod testpattern;

/// A pull-based source of raw frames. `next_frame` blocks until a frame is
/// available and returns `None` when the source has ended.
pub trait FrameSource: Send {
    fn next_frame(&mut self) -> Result<Option<RawFrame>>;

    /// Wait up to `timeout` for a frame. `Ok(None)` means no frame arrived in
    /// time but the source is still running. Override this for damage-driven
    /// sources (portal) so a held frame can be sent when a viewer connects.
    fn poll_frame(&mut self, _timeout: Duration) -> Result<Option<RawFrame>> {
        self.next_frame()
    }

    fn is_finished(&self) -> bool {
        false
    }

    /// Hands a frame buffer back so the source can reuse the allocation.
    fn recycle(&mut self, _buffer: Vec<u8>) {}

    /// The encoder cannot use DMA-BUF frames (no VAAPI, or the import
    /// failed): switch the capture to shared memory. No-op for sources that
    /// never produce DMA-BUFs.
    fn disable_dmabuf(&mut self, _reason: &str) {}

    fn describe(&self) -> String;

    /// How the cursor is delivered by this source (after any fallback).
    fn cursor_capture(&self) -> CursorCapture {
        CursorCapture::Embedded
    }

    /// Why the requested cursor mode could not be used, if it could not.
    fn cursor_note(&self) -> Option<String> {
        None
    }

    /// Where remote input (keys, pen/touch) goes when the capture session itself carries it
    /// (the portal's combined RemoteDesktop + ScreenCast session). `None`: the caller starts
    /// a separate injector.
    fn remote_input(
        &self,
    ) -> Option<tokio::sync::mpsc::Sender<mirrorwire_proto::control::RemoteInput>> {
        None
    }

    /// One line about the input side of the session, for the presenter window.
    fn input_note(&self) -> Option<String> {
        None
    }

    /// Maximum frame rate agreed with the compositor, once known.
    fn negotiated_fps(&self) -> Option<u32> {
        None
    }
}
