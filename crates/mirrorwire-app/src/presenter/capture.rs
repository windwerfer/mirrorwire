//! Live capture settings shared by the encode thread, transport and GUI.

use crate::capture_prefs::{CapturePrefs, CursorPref};
use mirrorwire_media::source::CaptureMode;
use mirrorwire_media::source::CursorCapture;
use mirrorwire_net::transport::{LinkControl, LinkState};
use mirrorwire_proto::control::{CursorMode, DisplayInfo};
use std::sync::Arc;
use tokio::sync::watch;

/// Live capture settings and what the running source actually does. Shared
/// between the encode thread, the transport's viewer callback and the GUI.
#[derive(Debug, Clone)]
pub struct CaptureCtl {
    /// What the user asked for.
    pub prefs: CapturePrefs,
    /// What the current source was opened with (before portal fallback).
    pub(super) asked_cursor: CursorCapture,
    pub(super) asked_mode: CaptureMode,
    pub(super) asked_fps: u32,
    /// What the current source delivers.
    pub(super) active_cursor: CursorCapture,
    pub(super) cursor_note: Option<String>,
    /// State of the presenter remote (keyboard permission), for the window.
    pub(super) remote_note: Option<String>,
    /// Pen input goes to the virtual uinput pen (setting on and the check passed, D26).
    #[cfg(target_os = "linux")]
    pub(super) pen_pressure: bool,
    /// State of the virtual pen, for the window.
    #[cfg(target_os = "linux")]
    pub(super) pen_note: Option<String>,
    /// Display size the viewer reported in hello (for the Extend hint).
    pub(super) viewer_display: Option<watch::Receiver<Option<DisplayInfo>>>,
    pub(super) negotiated_fps: Option<u32>,
    pub(super) source_desc: String,
    /// FEC setting and link feedback, shared with the transport.
    pub(super) fec: Arc<LinkControl>,
}

impl CaptureCtl {
    pub(super) fn new(prefs: CapturePrefs) -> Self {
        let fec = LinkControl::new(prefs.fec);
        fec.set_adaptive(prefs.adaptive);
        Self {
            fec,
            prefs,
            asked_cursor: CursorCapture::Embedded,
            asked_mode: prefs.mode,
            asked_fps: 0,
            active_cursor: CursorCapture::Embedded,
            cursor_note: None,
            remote_note: None,
            #[cfg(target_os = "linux")]
            pen_pressure: false,
            #[cfg(target_os = "linux")]
            pen_note: None,
            viewer_display: None,
            negotiated_fps: None,
            source_desc: String::new(),
        }
    }

    /// The cursor mode to ask the capture source for right now.
    pub(super) fn wanted_cursor(&self) -> CursorCapture {
        match self.prefs.cursor {
            CursorPref::Separate => CursorCapture::Metadata,
            CursorPref::Embedded => CursorCapture::Embedded,
            CursorPref::Hidden => CursorCapture::Hidden,
        }
    }

    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub(super) fn needs_restart(&self) -> bool {
        self.wanted_cursor() != self.asked_cursor
            || self.prefs.mode != self.asked_mode
            || self.prefs.fps.request() != self.asked_fps
    }

    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub(super) fn status(&self) -> CaptureStatus {
        CaptureStatus {
            prefs: self.prefs,
            cursor_mode: self.active_cursor.mode(),
            cursor_note: self.cursor_note.clone(),
            remote_note: self.remote_note.clone(),
            #[cfg(target_os = "linux")]
            pen_pressure: self.pen_pressure,
            #[cfg(target_os = "linux")]
            pen_note: self.pen_note.clone(),
            viewer_display: self.viewer_display.as_ref().and_then(|rx| *rx.borrow()),
            fps_requested: self.asked_fps,
            fps_negotiated: self.negotiated_fps,
            fec: self.fec.snapshot(),
        }
    }
}

/// Capture settings as shown in the presenter window.
#[derive(Debug, Clone)]
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
pub struct CaptureStatus {
    pub prefs: CapturePrefs,
    /// What the viewer actually gets.
    pub cursor_mode: CursorMode,
    pub cursor_note: Option<String>,
    pub remote_note: Option<String>,
    /// Pen input uses the virtual uinput pen with pressure (D26).
    #[cfg(target_os = "linux")]
    pub pen_pressure: bool,
    #[cfg(target_os = "linux")]
    pub pen_note: Option<String>,
    /// The viewer's display size, once it has connected.
    pub viewer_display: Option<DisplayInfo>,
    pub fps_requested: u32,
    pub fps_negotiated: Option<u32>,
    /// FEC setting, applied overhead and what the viewer reports.
    pub fec: LinkState,
}

impl Default for CaptureStatus {
    fn default() -> Self {
        CaptureCtl::new(CapturePrefs::default()).status()
    }
}
