use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

pub const MAX_CONTROL_MESSAGE: usize = 64 * 1024;

/// The viewer's display, sent in `hello` (protocol 8).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct DisplayInfo {
    /// Physical pixels.
    pub width: u32,
    pub height: u32,
    /// Device scale factor in 1/1000 (2000 = 2x).
    pub scale_milli: u32,
}

impl DisplayInfo {
    pub const MAX_SIDE: u32 = 16_384;

    /// Clamp a size from the outside into a sane one; `None` if it is empty
    /// or absurd (hostile or broken viewers must not size a monitor).
    pub fn sanitized(self) -> Option<Self> {
        let ok = |v: u32| (1..=Self::MAX_SIDE).contains(&v);
        (ok(self.width) && ok(self.height) && (250..=8000).contains(&self.scale_milli))
            .then_some(self)
    }

    /// From a size in points and the points-to-pixels factor (egui).
    pub fn from_points(width: f32, height: f32, pixels_per_point: f32) -> Self {
        let px = |v: f32| {
            (v * pixels_per_point)
                .round()
                .clamp(1.0, Self::MAX_SIDE as f32) as u32
        };
        Self {
            width: px(width),
            height: px(height),
            scale_milli: (pixels_per_point * 1000.0).round().clamp(250.0, 8000.0) as u32,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Control {
    Hello {
        /// Must equal [`crate::PROTOCOL_VERSION`]; checked (with
        /// [`hello_protocol`]) before the rest of the message is parsed.
        protocol: u32,
        name: String,
        pairing_code: Option<String>,
        /// The viewer's screen (what Extend mode sizes a virtual monitor to).
        display: DisplayInfo,
    },
    Rejected {
        reason: String,
    },
    StreamInfo(StreamInfo),
    RequestKeyframe {
        reason: String,
    },
    Ping {
        t_viewer_us: u64,
    },
    Pong {
        t_viewer_us: u64,
        t_presenter_us: u64,
    },
    /// Viewer -> presenter, about twice a second: how the video packets of
    /// the last window arrived. Feeds the FEC overhead and the adaptive
    /// bitrate.
    LinkReport {
        /// Packet loss over the window, in 1/1000.
        loss_permille: u32,
        /// Packets the presenter sent for frames in the window.
        packets: u32,
        /// Data packets rebuilt from parity.
        recovered: u32,
        /// Frames that could not be rebuilt.
        unrecoverable: u32,
        /// What arrived over the window, in kbit/s on the wire (first transmissions of video
        /// packets, parity included, headers included; resends do not count): the viewer's
        /// acknowledgement of what the presenter sent, a second signal for the adaptive bitrate.
        rx_kbps: u32,
        /// Length of the window the report covers, in ms (so the presenter can compare `rx_kbps`
        /// with what it sent in the same period).
        window_ms: u32,
        /// One-way queueing delay of the window's frames; `None` when no
        /// frame arrived in the window.
        queue: Option<QueueDelay>,
    },
    /// Viewer -> presenter, when the viewer's bottleneck verdict changes (and once on connect
    /// once it has one): the same coarse verdict the viewer shows on its HUD, so the presenter
    /// window can show it too. Both fields are required.
    Bottleneck {
        stage: BottleneckStage,
        rating: BottleneckRating,
    },
    /// Viewer -> presenter: the user pressed a presenter-remote button (protocol 9). The
    /// presenter taps the matching key through the RemoteDesktop portal. One message is one tap.
    RemoteKey {
        key: RemoteKey,
    },
    /// Viewer -> presenter: pen or finger as an absolute pointer (protocol 10). `x`/`y` are
    /// the position inside the *picture* (not the window), 0 = left/top, 65535 = right/bottom.
    /// Sent on the reliable control stream so down/up are never lost or reordered; the viewer
    /// coalesces `move`s (about 60 per second) and the presenter rate limits them.
    /// `pressure` (protocol 11): 0..=65535 from the viewer's pen or touch sensor, `null` when
    /// the device reports none (mouse, finger without force). Always present on the wire.
    Pointer {
        phase: PointerPhase,
        x: u16,
        y: u16,
        #[serde(deserialize_with = "Option::deserialize")]
        pressure: Option<u16>,
    },
    /// Viewer -> presenter: the user switched the viewer's Pen button on or off (protocol 12),
    /// sent at once and again after a reconnect. On `true` the presenter asks the desktop for
    /// pointer permission (reopening the capture together with input) before the first
    /// stroke, so the permission dialog appears only when the pen is really wanted.
    PenMode {
        on: bool,
    },
    /// Viewer -> presenter: the viewer shows no video right now (protocol 12; its "Wacom pen"
    /// or "PowerPoint remote only" mode). While `true` the presenter skips encoding and
    /// sending; `false` resumes with a keyframe. A new connection starts with video on.
    VideoPaused {
        paused: bool,
    },
    Bye,
}

/// A presenter-remote button. Fixed set on purpose: the viewer never sends raw key codes, so a
/// paired-but-hostile viewer can only flip slides, not type.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RemoteKey {
    /// Next slide (Right arrow).
    Next,
    /// Previous slide (Left arrow).
    Previous,
    /// The "click" button (Space).
    Space,
    /// Leave the slideshow (Escape).
    Escape,
}

impl RemoteKey {
    pub const ALL: [RemoteKey; 4] = [Self::Previous, Self::Next, Self::Space, Self::Escape];

    /// X11 keysym of the key (layout independent; what the portal's keysym call takes).
    pub fn keysym(self) -> i32 {
        match self {
            Self::Next => 0xff53,
            Self::Previous => 0xff51,
            Self::Space => 0x20,
            Self::Escape => 0xff1b,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Next => "Next",
            Self::Previous => "Previous",
            Self::Space => "Click",
            Self::Escape => "Escape",
        }
    }
}

/// Input a viewer may inject on the presenter, after parsing and rate limiting (the presenter
/// side's channel item).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteInput {
    Key(RemoteKey),
    Pointer {
        phase: PointerPhase,
        x: u16,
        y: u16,
        pressure: Option<u16>,
    },
    /// The viewer's Pen button changed ([`Control::PenMode`]).
    PenMode(bool),
}

/// Phase of a [`Control::Pointer`] event. Down = left button pressed, up = released.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PointerPhase {
    Down,
    Move,
    Up,
}

/// The largest rectangle with the aspect ratio `content_w : content_h` centred in `area`
/// `(left, top, width, height)`: the pen surface of the viewers' "Wacom pen" mode (the whole
/// rectangle stands for the presenter's screen). `None` for an empty area or content size, or
/// any input that is not finite.
pub fn fit_rect(
    area: (f32, f32, f32, f32),
    content_w: u32,
    content_h: u32,
) -> Option<(f32, f32, f32, f32)> {
    let (l, t, w, h) = area;
    if ![l, t, w, h].iter().all(|v| v.is_finite()) || w <= 0.0 || h <= 0.0 {
        return None;
    }
    if content_w == 0 || content_h == 0 {
        return None;
    }
    let scale = (w / content_w as f32).min(h / content_h as f32);
    let (fw, fh) = (content_w as f32 * scale, content_h as f32 * scale);
    Some((l + (w - fw) / 2.0, t + (h - fh) / 2.0, fw, fh))
}

/// Wire position for a point `(px, py)` inside the picture rectangle `(left, top, width, height)`
/// (any unit, same for all). Clamped to the picture; `None` if the rectangle is empty or any
/// input is not finite. Used by both viewers.
pub fn pointer_in_rect(px: f32, py: f32, rect: (f32, f32, f32, f32)) -> Option<(u16, u16)> {
    let (l, t, w, h) = rect;
    if ![px, py, l, t, w, h].iter().all(|v| v.is_finite()) || w <= 0.0 || h <= 0.0 {
        return None;
    }
    let f = |v: f32| (v.clamp(0.0, 1.0) * 65535.0).round() as u16;
    Some((f((px - l) / w), f((py - t) / h)))
}

/// What the viewers' pen mode turns the local pointer into: one copy for the desktop and the
/// Android viewer. A press sends `Down`, a release `Up`, and every other position sends `Move`,
/// **also while nothing is pressed** (hover: the presenter's cursor follows the pen in
/// proximity; `Move` never presses a button). Moves are coalesced to about 60 per second, and
/// a hover that did not change position is not sent again.
#[derive(Debug, Default)]
pub struct PenSampler {
    down: bool,
    last_move: Option<std::time::Instant>,
    last_hover: Option<(u16, u16)>,
}

impl PenSampler {
    const MIN_MOVE_GAP: std::time::Duration = std::time::Duration::from_millis(16);

    /// Is a stroke (a pressed button on the presenter) in progress?
    pub fn is_down(&self) -> bool {
        self.down
    }

    /// Forget a stroke without sending anything (pen mode left). Returns whether a release
    /// must be sent so the presenter's button is not stuck.
    pub fn cancel(&mut self) -> bool {
        self.last_hover = None;
        std::mem::take(&mut self.down)
    }

    /// One frame. `pos` is the wire position of the local pointer inside the picture (`None`:
    /// outside or unknown), `over` whether the pointer is over the picture, `pressed` a press
    /// edge this frame, `released` that the button is not held. Returns the phase to send.
    pub fn step(
        &mut self,
        pos: Option<(u16, u16)>,
        over: bool,
        pressed: bool,
        released: bool,
        now: std::time::Instant,
    ) -> Option<(PointerPhase, u16, u16)> {
        let (x, y) = pos?;
        if pressed && over && !self.down {
            self.down = true;
            self.last_hover = None;
            return Some((PointerPhase::Down, x, y));
        }
        if self.down && released {
            self.down = false;
            self.last_hover = Some((x, y));
            return Some((PointerPhase::Up, x, y));
        }
        if !self.down && (!over || self.last_hover == Some((x, y))) {
            return None;
        }
        if self
            .last_move
            .is_some_and(|t| now.duration_since(t) < Self::MIN_MOVE_GAP)
        {
            return None;
        }
        self.last_move = Some(now);
        if !self.down {
            self.last_hover = Some((x, y));
        }
        Some((PointerPhase::Move, x, y))
    }
}

/// Wire pressure for a sensor reading `force` (0.0 = none, 1.0 = full, as egui reports
/// touch force). `None` for no reading or a value that is not finite; clamped otherwise.
pub fn pressure_from_force(force: Option<f32>) -> Option<u16> {
    let f = force.filter(|f| f.is_finite())?;
    Some((f.clamp(0.0, 1.0) * 65535.0).round() as u16)
}

/// Map a normalised 0..=65535 position onto a stream of `size` pixels. `None` if the size
/// is empty or absurd. The result is always inside `0..=size-1`, so a hostile value cannot
/// point outside the captured monitor.
pub fn pointer_to_stream(x: u16, y: u16, size: (i32, i32)) -> Option<(f64, f64)> {
    let ok = |v: i32| (1..=DisplayInfo::MAX_SIDE as i32).contains(&v);
    if !ok(size.0) || !ok(size.1) {
        return None;
    }
    let map = |v: u16, n: i32| f64::from(v) / 65535.0 * f64::from(n - 1);
    Some((map(x, size.0), map(y, size.1)))
}

/// Which stage limits the stream, as judged by the viewer.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BottleneckStage {
    None,
    Network,
    Viewer,
    Driver,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BottleneckRating {
    Good,
    Ok,
    Weak,
}

/// One-way queueing delay over a report window (µs above the lowest delay
/// of the last 30 s; [`crate::delay`]).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct QueueDelay {
    pub mean_us: u32,
    pub max_us: u32,
    /// Least-squares slope over the window (µs per second; positive = a
    /// queue is building up).
    pub trend_us_per_s: i32,
}

impl QueueDelay {
    pub fn mean_ms(&self) -> f32 {
        self.mean_us as f32 / 1000.0
    }

    pub fn max_ms(&self) -> f32 {
        self.max_us as f32 / 1000.0
    }

    pub fn trend_ms_per_s(&self) -> f32 {
        self.trend_us_per_s as f32 / 1000.0
    }
}

/// `queue 5 ms (max 12, -2 ms/s)`, as in the logs and the HUD.
impl std::fmt::Display for QueueDelay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "queue {:.0} ms (max {:.0}, {:+.0} ms/s)",
            self.mean_ms(),
            self.max_ms(),
            self.trend_ms_per_s()
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StreamInfo {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub codec: String,
    pub encoder: String,
    pub bitrate_kbps: u32,
    /// The encoder heals losses by itself within about a second, so the viewer
    /// keeps decoding through a loss instead of waiting for a keyframe.
    pub intra_refresh: bool,
    /// How the cursor reaches the viewer. The presenter sends `stream_info`
    /// again when this changes.
    pub cursor: CursorMode,
    /// The presenter's forward error correction setting (`off`, `auto`,
    /// `20%`), for display.
    pub fec: String,
}

/// How the presenter delivers the mouse cursor.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum CursorMode {
    /// As pointer datagrams and image streams, drawn by the viewer
    /// ([`crate::cursor`]).
    Separate,
    /// Drawn into the video by the compositor.
    #[default]
    Embedded,
    /// Not shown.
    Hidden,
}

impl CursorMode {
    /// The name on the wire, in the status and in the HUD.
    pub fn as_str(self) -> &'static str {
        match self {
            CursorMode::Separate => "separate",
            CursorMode::Embedded => "embedded",
            CursorMode::Hidden => "hidden",
        }
    }
}

/// The `protocol` field of a `hello` body, read without parsing the rest, so
/// a peer of another version is rejected with a readable message even when
/// the message shapes differ. `None` if the body is not a `hello`.
pub fn hello_protocol(body: &[u8]) -> Option<u32> {
    #[derive(Deserialize)]
    struct Peek {
        #[serde(rename = "type")]
        kind: String,
        protocol: u32,
    }
    let p: Peek = serde_json::from_slice(body).ok()?;
    (p.kind == "hello").then_some(p.protocol)
}

impl Control {
    pub fn encode(&self) -> Vec<u8> {
        // Cannot fail for these plain types; the fallback keeps encode infallible.
        let body = serde_json::to_vec(self).unwrap_or_else(|_| br#"{"type":"bye"}"#.to_vec());
        let mut out = Vec::with_capacity(body.len() + 4);
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(&body);
        out
    }

    pub fn decode_body(body: &[u8]) -> Result<Self> {
        Ok(serde_json::from_slice(body)?)
    }

    pub fn check_len(len: u32) -> Result<usize> {
        let len = len as usize;
        if len > MAX_CONTROL_MESSAGE {
            bail!("control message too large: {len} bytes");
        }
        Ok(len)
    }
}

#[cfg(test)]
mod tests {
    // --- PenSampler: hover (pen in proximity, tip up) must reach the presenter ---

    fn at(t0: std::time::Instant, ms: u64) -> std::time::Instant {
        t0 + std::time::Duration::from_millis(ms)
    }

    /// Before the fix both viewers sent nothing unless the tip was down: a hovering pen never
    /// moved the presenter's cursor.
    #[test]
    fn hover_without_contact_sends_moves() {
        let t0 = std::time::Instant::now();
        let mut s = PenSampler::default();
        assert_eq!(
            s.step(Some((10, 20)), true, false, false, t0),
            Some((PointerPhase::Move, 10, 20))
        );
        assert!(!s.is_down());
        assert_eq!(
            s.step(Some((30, 40)), true, false, false, at(t0, 20)),
            Some((PointerPhase::Move, 30, 40))
        );
        assert!(!s.is_down());
    }

    #[test]
    fn hover_is_coalesced_and_not_repeated_when_still() {
        let t0 = std::time::Instant::now();
        let mut s = PenSampler::default();
        assert!(s.step(Some((1, 1)), true, false, false, t0).is_some());
        // Too soon, and then the same place again.
        assert_eq!(s.step(Some((2, 2)), true, false, false, at(t0, 5)), None);
        assert!(s
            .step(Some((2, 2)), true, false, false, at(t0, 20))
            .is_some());
        assert_eq!(s.step(Some((2, 2)), true, false, false, at(t0, 40)), None);
    }

    #[test]
    fn no_hover_outside_the_picture() {
        let t0 = std::time::Instant::now();
        let mut s = PenSampler::default();
        assert_eq!(s.step(Some((1, 1)), false, false, false, t0), None);
        assert_eq!(s.step(None, true, false, false, t0), None);
    }

    /// Hover, touch down, stroke, lift, hover again: contact still sends Down/Move/Up and the
    /// hover around it never presses (only `Down` does) or leaves the button pressed.
    #[test]
    fn contact_still_works_between_hovers_and_up_always_follows_down() {
        let t0 = std::time::Instant::now();
        let mut s = PenSampler::default();
        let mut sent = Vec::new();
        let mut run = |s: &mut PenSampler, p, over, pressed, released, ms| {
            if let Some((ph, _, _)) = s.step(Some(p), over, pressed, released, at(t0, ms)) {
                sent.push(ph);
            }
        };
        run(&mut s, (1, 1), true, false, true, 0); // hover
        run(&mut s, (2, 2), true, true, false, 20); // tip down
        assert!(s.is_down());
        run(&mut s, (3, 3), true, false, false, 40); // stroke
        run(&mut s, (4, 4), true, false, true, 60); // lift
        assert!(!s.is_down());
        run(&mut s, (5, 5), true, false, true, 80); // hover after the lift
        assert_eq!(
            sent,
            [
                PointerPhase::Move,
                PointerPhase::Down,
                PointerPhase::Move,
                PointerPhase::Up,
                PointerPhase::Move
            ]
        );
    }

    /// Leaving the picture mid-stroke still ends the stroke; hover exit (pointer leaves while
    /// not down) sends nothing that could press.
    #[test]
    fn stroke_ends_outside_and_cancel_releases_once() {
        let t0 = std::time::Instant::now();
        let mut s = PenSampler::default();
        s.step(Some((1, 1)), true, true, false, t0);
        assert_eq!(
            s.step(Some((9, 9)), false, false, true, at(t0, 20)),
            Some((PointerPhase::Up, 9, 9))
        );
        s.step(Some((1, 1)), true, true, false, at(t0, 40));
        assert!(s.cancel());
        assert!(!s.cancel());
    }

    use super::*;

    #[test]
    fn roundtrip() {
        let msg = Control::Ping { t_viewer_us: 42 };
        let wire = msg.encode();
        let len = u32::from_be_bytes(wire[..4].try_into().unwrap());
        assert_eq!(Control::check_len(len).unwrap(), wire.len() - 4);
        assert_eq!(Control::decode_body(&wire[4..]).unwrap(), msg);
    }

    #[test]
    fn version_is_read_before_the_shape() {
        // A version-1 hello (with its `caps` list) still shows its version.
        let v1 = br#"{"type":"hello","protocol":1,"name":"imac","pairing_code":null,"caps":["fec-rs1"]}"#;
        assert_eq!(hello_protocol(v1), Some(1));
        // A future hello with a different shape, too.
        assert_eq!(
            hello_protocol(br#"{"type":"hello","protocol":9,"other":[1]}"#),
            Some(9)
        );
        assert_eq!(hello_protocol(br#"{"type":"bye"}"#), None);
        assert_eq!(hello_protocol(b"\xff garbage"), None);
        let hello = Control::Hello {
            protocol: crate::PROTOCOL_VERSION,
            name: "imac".into(),
            pairing_code: None,
            display: DisplayInfo {
                width: 1920,
                height: 1080,
                scale_milli: 1000,
            },
        };
        assert_eq!(
            hello_protocol(&hello.encode()[4..]),
            Some(crate::PROTOCOL_VERSION)
        );
    }

    #[test]
    fn remote_key_wire_and_bounds() {
        let msg = Control::RemoteKey {
            key: RemoteKey::Next,
        };
        let wire = msg.encode();
        assert_eq!(
            std::str::from_utf8(&wire[4..]).unwrap(),
            r#"{"type":"remote_key","key":"next"}"#
        );
        assert_eq!(Control::decode_body(&wire[4..]).unwrap(), msg);
        // Raw key codes and unknown names are errors, never keys.
        assert!(Control::decode_body(br#"{"type":"remote_key","key":"ctrl_alt_del"}"#).is_err());
        assert!(Control::decode_body(br#"{"type":"remote_key","key":65}"#).is_err());
        let syms: std::collections::HashSet<i32> =
            RemoteKey::ALL.iter().map(|k| k.keysym()).collect();
        assert_eq!(syms.len(), RemoteKey::ALL.len());
    }

    #[test]
    fn pen_mode_wire() {
        let msg = Control::PenMode { on: true };
        let wire = msg.encode();
        assert_eq!(
            std::str::from_utf8(&wire[4..]).unwrap(),
            r#"{"type":"pen_mode","on":true}"#
        );
        assert_eq!(Control::decode_body(&wire[4..]).unwrap(), msg);
        assert_eq!(
            Control::decode_body(br#"{"type":"pen_mode","on":false}"#).unwrap(),
            Control::PenMode { on: false }
        );
        // Missing, null, number or string: errors.
        for bad in [
            r#"{"type":"pen_mode"}"#,
            r#"{"type":"pen_mode","on":null}"#,
            r#"{"type":"pen_mode","on":1}"#,
            r#"{"type":"pen_mode","on":"yes"}"#,
        ] {
            assert!(Control::decode_body(bad.as_bytes()).is_err(), "{bad}");
        }
    }

    #[test]
    fn fit_rect_keeps_the_aspect_and_centres() {
        // 16:9 content in a 4:3 area: full width, letterboxed top and bottom.
        assert_eq!(
            fit_rect((0.0, 0.0, 1600.0, 1200.0), 1920, 1080),
            Some((0.0, 150.0, 1600.0, 900.0))
        );
        // Tall area offset from the origin: full height, pillarboxed.
        assert_eq!(
            fit_rect((10.0, 20.0, 1000.0, 300.0), 4, 3),
            Some((310.0, 20.0, 400.0, 300.0))
        );
        assert_eq!(fit_rect((0.0, 0.0, 0.0, 100.0), 16, 9), None);
        assert_eq!(fit_rect((0.0, 0.0, 100.0, 100.0), 0, 9), None);
        assert_eq!(fit_rect((f32::NAN, 0.0, 100.0, 100.0), 16, 9), None);
    }

    #[test]
    fn video_paused_wire() {
        let msg = Control::VideoPaused { paused: true };
        let wire = msg.encode();
        assert_eq!(
            std::str::from_utf8(&wire[4..]).unwrap(),
            r#"{"type":"video_paused","paused":true}"#
        );
        assert_eq!(Control::decode_body(&wire[4..]).unwrap(), msg);
        for bad in [
            r#"{"type":"video_paused"}"#,
            r#"{"type":"video_paused","paused":0}"#,
            r#"{"type":"video_paused","paused":"no"}"#,
        ] {
            assert!(Control::decode_body(bad.as_bytes()).is_err(), "{bad}");
        }
    }

    #[test]
    fn pointer_wire_and_mapping() {
        let msg = Control::Pointer {
            phase: PointerPhase::Down,
            x: 100,
            y: 65535,
            pressure: Some(4000),
        };
        let wire = msg.encode();
        assert_eq!(
            std::str::from_utf8(&wire[4..]).unwrap(),
            r#"{"type":"pointer","phase":"down","x":100,"y":65535,"pressure":4000}"#
        );
        assert_eq!(Control::decode_body(&wire[4..]).unwrap(), msg);
        let none = Control::Pointer {
            phase: PointerPhase::Move,
            x: 0,
            y: 0,
            pressure: None,
        };
        let wire = none.encode();
        assert_eq!(
            std::str::from_utf8(&wire[4..]).unwrap(),
            r#"{"type":"pointer","phase":"move","x":0,"y":0,"pressure":null}"#
        );
        assert_eq!(Control::decode_body(&wire[4..]).unwrap(), none);
        // Out of range, negative, float, unknown phase, missing pressure: errors.
        for bad in [
            r#"{"type":"pointer","phase":"down","x":65536,"y":0,"pressure":null}"#,
            r#"{"type":"pointer","phase":"down","x":-1,"y":0,"pressure":null}"#,
            r#"{"type":"pointer","phase":"down","x":0.5,"y":0,"pressure":null}"#,
            r#"{"type":"pointer","phase":"drag","x":0,"y":0,"pressure":null}"#,
            r#"{"type":"pointer","phase":"up","x":0,"pressure":null}"#,
            r#"{"type":"pointer","phase":"down","x":0,"y":0}"#,
            r#"{"type":"pointer","phase":"down","x":0,"y":0,"pressure":65536}"#,
            r#"{"type":"pointer","phase":"down","x":0,"y":0,"pressure":-1}"#,
            r#"{"type":"pointer","phase":"down","x":0,"y":0,"pressure":0.5}"#,
        ] {
            assert!(Control::decode_body(bad.as_bytes()).is_err(), "{bad}");
        }
        assert_eq!(pointer_to_stream(0, 0, (1920, 1080)), Some((0.0, 0.0)));
        assert_eq!(
            pointer_to_stream(65535, 65535, (1920, 1080)),
            Some((1919.0, 1079.0))
        );
        let (x, y) = pointer_to_stream(32768, 32768, (1001, 501)).unwrap();
        assert!((x - 500.0).abs() < 0.1 && (y - 250.0).abs() < 0.1);
        let r = (10.0, 20.0, 100.0, 50.0);
        assert_eq!(pointer_in_rect(10.0, 20.0, r), Some((0, 0)));
        assert_eq!(pointer_in_rect(110.0, 70.0, r), Some((65535, 65535)));
        assert_eq!(pointer_in_rect(-500.0, 9999.0, r), Some((0, 65535)));
        assert_eq!(pointer_in_rect(f32::NAN, 0.0, r), None);
        assert_eq!(pointer_in_rect(1.0, 1.0, (0.0, 0.0, 0.0, 5.0)), None);
        assert_eq!(pointer_to_stream(1, 1, (0, 100)), None);
        assert_eq!(pointer_to_stream(1, 1, (100, -5)), None);
        assert_eq!(pointer_to_stream(1, 1, (i32::MAX, 5)), None);
    }

    #[test]
    fn pressure_from_force_is_clamped() {
        assert_eq!(pressure_from_force(None), None);
        assert_eq!(pressure_from_force(Some(f32::NAN)), None);
        assert_eq!(pressure_from_force(Some(f32::INFINITY)), None);
        assert_eq!(pressure_from_force(Some(0.0)), Some(0));
        assert_eq!(pressure_from_force(Some(0.5)), Some(32768));
        assert_eq!(pressure_from_force(Some(1.0)), Some(65535));
        assert_eq!(pressure_from_force(Some(3.0)), Some(65535));
        assert_eq!(pressure_from_force(Some(-1.0)), Some(0));
    }

    #[test]
    fn display_info_is_bounded() {
        let d = |w, h, s| DisplayInfo {
            width: w,
            height: h,
            scale_milli: s,
        };
        assert!(d(1920, 1080, 1000).sanitized().is_some());
        assert!(d(0, 1080, 1000).sanitized().is_none());
        assert!(d(1920, 99_999, 1000).sanitized().is_none());
        assert!(d(1920, 1080, 0).sanitized().is_none());
        assert_eq!(
            DisplayInfo::from_points(1280.0, 720.0, 1.5),
            d(1920, 1080, 1500)
        );
        assert!(DisplayInfo::from_points(0.0, f32::NAN, 0.0)
            .sanitized()
            .is_none());
    }

    #[test]
    fn wire_names_are_pinned() {
        let info = Control::StreamInfo(StreamInfo {
            width: 1,
            height: 1,
            fps: 60,
            codec: "h264".into(),
            encoder: "x".into(),
            bitrate_kbps: 1,
            intra_refresh: false,
            cursor: CursorMode::Separate,
            fec: "auto".into(),
        });
        let body = info.encode();
        let text = std::str::from_utf8(&body[4..]).unwrap();
        assert!(
            text.contains(r#""cursor":"separate""#) && text.contains(r#""fec":"auto""#),
            "{text}"
        );
        assert_eq!(Control::decode_body(&body[4..]).unwrap(), info);

        let report = Control::LinkReport {
            loss_permille: 5,
            packets: 900,
            recovered: 2,
            unrecoverable: 0,
            rx_kbps: 21_500,
            window_ms: 503,
            queue: Some(QueueDelay {
                mean_us: 12_000,
                max_us: 30_000,
                trend_us_per_s: -4_500,
            }),
        };
        let body = report.encode();
        let text = std::str::from_utf8(&body[4..]).unwrap();
        assert!(
            text.contains(r#""queue":{"mean_us":12000,"max_us":30000,"trend_us_per_s":-4500}"#),
            "{text}"
        );
        assert!(
            text.contains(r#""rx_kbps":21500,"window_ms":503"#),
            "{text}"
        );
        assert_eq!(Control::decode_body(&body[4..]).unwrap(), report);

        let verdict = Control::Bottleneck {
            stage: BottleneckStage::Network,
            rating: BottleneckRating::Weak,
        };
        let body = verdict.encode();
        assert_eq!(
            std::str::from_utf8(&body[4..]).unwrap(),
            r#"{"type":"bottleneck","stage":"network","rating":"weak"}"#
        );
        assert_eq!(Control::decode_body(&body[4..]).unwrap(), verdict);
        // Unknown stages and missing fields are errors, not panics.
        assert!(
            Control::decode_body(br#"{"type":"bottleneck","stage":"gpu","rating":"weak"}"#)
                .is_err()
        );
        assert!(Control::decode_body(br#"{"type":"bottleneck","stage":"none"}"#).is_err());
        // Every field is required: a report of another protocol version does not parse.
        let old = br#"{"type":"link_report","loss_permille":5,"packets":9,"recovered":0,"unrecoverable":0,"queue":null}"#;
        assert!(Control::decode_body(old).is_err());
    }
}
