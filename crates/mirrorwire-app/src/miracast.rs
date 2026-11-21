//! Miracast sink as a viewer source (cargo feature `miracast`, spike; docs/agents/miracast.md
//! "Viewer integration"). An Android tablet that casts with "Wireless display" shows up in
//! the normal video view: Wi-Fi Direct join through NetworkManager (`miracast-nm`), RTSP and
//! RTP/TS (`miracast-loopback`'s sink driver on `miracast-rtsp`/`miracast-rtp`), then the
//! app's own H.264 decoders (`mirrorwire_media::decode`) sized from the stream's SPS
//! (`miracast_rtp::h264::find_sps`), never from the RTSP M4 pick (run 3: M4 said level 3.2,
//! the SPS 4.0). The result is a [`ViewerHandle`], so the GUI draws it like a QUIC stream.
//!
//! Threads: a session thread (NM join, then the blocking RTSP/RTP loop) feeds access units
//! through a 2-deep channel into a decode thread, which keeps only the newest picture for the
//! GUI (latency beats smoothness). A full channel drops units until the next keyframe.

use crate::sync::LockExt;
use crate::viewer::{DisplayFrame, Picture, Totals, ViewerHandle, ViewerStatus};
use anyhow::Result;
use crossbeam_channel::{bounded, Receiver, Sender, TrySendError};
use miracast_loopback::{run_sink_logged, RunOptions, SinkReport};
use miracast_rtp::h264::{find_sps, Sps};
use miracast_rtp::{AudioFrame, MediaSink, VideoAccessUnit};
use mirrorwire_media::decode::{self, HwDecode, RgbaConverter, SoftwareDecoder, VideoDecoder};
use mirrorwire_proto::clock::now_us;
use mirrorwire_proto::stats::StageStats;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Access units waiting for the decoder (bounded: a growing queue is a latency bug).
const DECODE_QUEUE: usize = 2;
/// The only stage the Miracast path can time (no sender clock).
const STAGES: &[&str] = &["decode"];

pub struct MiracastOptions {
    pub hwdec: bool,
    pub software_decoder: SoftwareDecoder,
    /// Convert to RGBA (wgpu) instead of handing YUV to the GL renderer.
    pub rgba: bool,
    /// Only sources whose Wi-Fi Direct name contains this.
    pub peer: Option<String>,
    /// UDP port announced for RTP.
    pub rtp_port: u16,
}

/// Starts the Wi-Fi Direct join and the stream; the handle works like a QUIC viewer's.
pub fn spawn(
    runtime: &tokio::runtime::Handle,
    opts: MiracastOptions,
    on_frame: Option<Arc<dyn Fn() + Send + Sync>>,
) -> ViewerHandle {
    let runtime = runtime.clone();
    start(opts, on_frame, move |ctx| session_nm(&runtime, ctx))
}

/// Like [`spawn`] but without Wi-Fi Direct: the source's RTSP server is already reachable at
/// `source` (an existing group, or a test source on localhost).
#[cfg_attr(not(test), allow(dead_code))]
pub fn spawn_direct(
    source: SocketAddr,
    opts: MiracastOptions,
    on_frame: Option<Arc<dyn Fn() + Send + Sync>>,
) -> ViewerHandle {
    start(opts, on_frame, move |ctx| {
        ctx.stream(source, None);
    })
}

/// What the session thread needs.
struct Ctx {
    status: Arc<Mutex<ViewerStatus>>,
    stop: Arc<AtomicBool>,
    units: Sender<VideoAccessUnit>,
    rtp_port: u16,
    peer: Option<String>,
}

fn start(
    opts: MiracastOptions,
    on_frame: Option<Arc<dyn Fn() + Send + Sync>>,
    session: impl FnOnce(&Ctx) + Send + 'static,
) -> ViewerHandle {
    let status = Arc::new(Mutex::new(ViewerStatus {
        decoder: "…".into(),
        note: Some("Miracast: starting".into()),
        ..ViewerStatus::default()
    }));
    let (frames_tx, frames) = bounded::<DisplayFrame>(1);
    let (done_tx, done) = bounded(1);
    let (units_tx, units_rx) = bounded::<VideoAccessUnit>(DECODE_QUEUE);
    let stop = Arc::new(AtomicBool::new(false));
    let ctx = Ctx {
        status: status.clone(),
        stop: stop.clone(),
        units: units_tx,
        rtp_port: opts.rtp_port,
        peer: opts.peer.clone(),
    };
    {
        let thread_status = status.clone();
        let drain = frames.clone();
        let spawned = std::thread::Builder::new()
            .name("miracast-decode".into())
            .spawn(move || {
                let status = thread_status;
                let started = Instant::now();
                let result = decode_loop(units_rx, &opts, &status, &frames_tx, &drain, on_frame);
                match result {
                    Ok((totals, stats)) => {
                        let _ = done_tx.send((totals, stats, started.elapsed().as_secs_f64()));
                    }
                    Err(e) => fail(&status, format!("Miracast decoder: {e:#}")),
                }
            });
        if let Err(e) = spawned {
            fail(&status, format!("cannot start the decode thread: {e}"));
        }
    }
    let spawned = std::thread::Builder::new()
        .name("miracast-session".into())
        .spawn(move || session(&ctx));
    if let Err(e) = spawned {
        fail(&status, format!("cannot start the Miracast thread: {e}"));
    }
    ViewerHandle {
        status,
        frames,
        stop,
        done,
    }
}

fn fail(status: &Mutex<ViewerStatus>, text: String) {
    tracing::warn!("miracast: {text}");
    let mut s = status.lock_ok();
    s.connected = false;
    s.error = Some(text);
}

fn note(status: &Mutex<ViewerStatus>, text: impl Into<String>) {
    let text = text.into();
    tracing::info!("miracast: {text}");
    status.lock_ok().note = Some(text);
}

/// NM flow: discovery, group join (the user taps this laptop in the tablet's Cast list), then
/// the stream to the group owner; the group is left at the end.
#[cfg(target_os = "linux")]
fn session_nm(runtime: &tokio::runtime::Handle, ctx: &Ctx) {
    use miracast_loopback::nm_flow::{join_source, FlowOptions};
    use miracast_nm::zbus_backend::ZbusNm;
    use miracast_nm::{NmP2pProvider, Sink, SinkConfig};

    note(
        &ctx.status,
        "Looking for a Miracast source: on the tablet open Settings > Cast and enable \
         wireless display",
    );
    let api = match runtime.block_on(ZbusNm::connect_system()) {
        Ok(a) => a,
        Err(e) => return fail(&ctx.status, e.to_string()),
    };
    let sink = Sink::new(NmP2pProvider::new(api), SinkConfig::default());
    let flow = FlowOptions {
        peer_name: ctx.peer.clone(),
        ..FlowOptions::default()
    };
    let joined = runtime.block_on(join_source(&sink, &flow, &mut |l| note(&ctx.status, l)));
    match joined {
        Ok(j) if !ctx.stop.load(Ordering::Acquire) => {
            note(
                &ctx.status,
                format!(
                    "Joined {} on {}; starting the stream",
                    j.peer.name, j.group.interface
                ),
            );
            ctx.stream(j.rtsp, Some(j.group.interface));
        }
        Ok(_) => {}
        Err(e) => fail(&ctx.status, e.to_string()),
    }
    if let Err(e) = runtime.block_on(sink.stop()) {
        tracing::warn!("miracast: leaving the Wi-Fi Direct group failed: {e}");
    }
}

#[cfg(not(target_os = "linux"))]
fn session_nm(_runtime: &tokio::runtime::Handle, ctx: &Ctx) {
    fail(
        &ctx.status,
        "Miracast needs NetworkManager Wi-Fi Direct (Linux only)".into(),
    );
}

impl Ctx {
    /// RTSP + RTP until the session ends; status updates from the driver's log lines.
    fn stream(&self, source: SocketAddr, interface: Option<String>) {
        let udp = match UdpSocket::bind(("0.0.0.0", self.rtp_port)) {
            Ok(u) => u,
            Err(e) => {
                return fail(
                    &self.status,
                    format!("cannot use UDP port {} for the stream: {e}", self.rtp_port),
                )
            }
        };
        let opts = RunOptions {
            interface,
            close_on_silence: Some(Duration::from_secs(8)),
            stats_every: Some(Duration::from_secs(5)),
            stop: Some(self.stop.clone()),
            ..RunOptions::new(Duration::from_secs(24 * 3600))
        };
        let mut forward = Forward::new(self.units.clone());
        let status = self.status.clone();
        let report = run_sink_logged(source, udp, &mut forward, &opts, &mut |l| {
            apply_log_line(&status, &l)
        });
        finish(&self.status, &report, self.stop.load(Ordering::Acquire));
    }
}

/// Maps the sink driver's trace lines onto the GUI status (AGENTS.md 1.8: failures show in
/// the window). RTSP message dumps stay in the log only.
fn apply_log_line(status: &Mutex<ViewerStatus>, line: &str) {
    if line.starts_with("<<< ") || line.starts_with(">>> ") {
        tracing::debug!("miracast: {line}");
        return;
    }
    tracing::info!("miracast: {line}");
    let mut s = status.lock_ok();
    if line.starts_with("start media") {
        s.connected = true;
        s.note = None;
    } else if let Some(d) = line.strip_prefix("diagnostic: ") {
        if s.first_frame {
            s.last_second = d.to_owned();
        } else {
            // Shown in red on the connecting screen (with the firewall-command copy button).
            s.error = Some(d.to_owned());
        }
    } else if line.starts_with("stats: ") {
        line.clone_into(&mut s.bottleneck);
    } else if !s.connected {
        s.note = Some(line.to_owned());
    }
}

fn finish(status: &Mutex<ViewerStatus>, report: &SinkReport, stopped: bool) {
    let mut s = status.lock_ok();
    s.connected = false;
    if !stopped && s.error.is_none() {
        let reason = report.close_reason.as_deref().unwrap_or("the stream ended");
        s.error = Some(if report.link_lost {
            format!(
                "Miracast: {reason}. The Wi-Fi Direct link was lost (see the log for the \
                 laptop's Wi-Fi channel)"
            )
        } else {
            format!("Miracast: {reason}")
        });
    }
}

/// `MediaSink` that hands video to the decode thread. When the decoder falls behind, units are
/// dropped until the next keyframe (a decoder fed a broken reference chain shows garbage).
struct Forward {
    tx: Sender<VideoAccessUnit>,
    skipping: bool,
    dropped: u64,
}

impl Forward {
    fn new(tx: Sender<VideoAccessUnit>) -> Self {
        Self {
            tx,
            skipping: false,
            dropped: 0,
        }
    }
}

impl MediaSink for Forward {
    fn video(&mut self, unit: VideoAccessUnit) {
        if self.skipping && !unit.keyframe {
            self.dropped += 1;
            return;
        }
        match self.tx.try_send(unit) {
            Ok(()) => self.skipping = false,
            Err(TrySendError::Full(_)) => {
                self.dropped += 1;
                self.skipping = true;
            }
            Err(TrySendError::Disconnected(_)) => {}
        }
    }

    fn audio(&mut self, _frame: AudioFrame) {
        // Audio is a non-goal (AGENTS.md); LPCM is offered only because the source insists.
    }

    fn need_keyframe(&mut self) {
        // The sink driver sends the IDR request (M13) itself.
    }
}

/// The decoder size the stream asks for: `Some(sps)` when a keyframe's SPS has a different
/// picture size than `current` (also the first one), i.e. when the decoder must be (re)opened.
fn resize_needed(current: Option<(u32, u32)>, unit: &VideoAccessUnit) -> Option<Sps> {
    if !unit.keyframe {
        return None;
    }
    let sps = find_sps(&unit.data)?;
    (current != Some((sps.width, sps.height))).then_some(sps)
}

fn open(opts: &MiracastOptions) -> Result<Box<dyn VideoDecoder>> {
    decode::open_decoder(
        if opts.hwdec {
            HwDecode::Auto
        } else {
            HwDecode::None
        },
        opts.software_decoder,
    )
}

fn decode_loop(
    rx: Receiver<VideoAccessUnit>,
    opts: &MiracastOptions,
    status: &Mutex<ViewerStatus>,
    frames_tx: &Sender<DisplayFrame>,
    frames_drain: &Receiver<DisplayFrame>,
    on_frame: Option<Arc<dyn Fn() + Send + Sync>>,
) -> Result<(Totals, StageStats)> {
    let mut decoder: Option<Box<dyn VideoDecoder>> = None;
    let mut size: Option<(u32, u32)> = None;
    let mut converter = RgbaConverter::default();
    let mut totals = Totals::default();
    let mut all = StageStats::new(STAGES);
    let mut window = StageStats::new(STAGES);
    let (mut w_start, mut w_frames, mut w_bytes) = (Instant::now(), 0u64, 0u64);

    while let Ok(unit) = rx.recv() {
        totals.bytes += unit.data.len() as u64;
        w_bytes += unit.data.len() as u64;
        if let Some(sps) = resize_needed(size, &unit) {
            // Size from the stream, not from M4; a new size (rotation) gets a fresh decoder.
            let (major, minor) = sps.level();
            tracing::info!(
                "miracast: stream SPS {}x{} profile {} level {major}.{minor}{}",
                sps.width,
                sps.height,
                sps.profile_idc,
                if size.is_some() {
                    " (new size: decoder reopened)"
                } else {
                    ""
                }
            );
            let d = open(opts)?;
            let mut s = status.lock_ok();
            s.decoder = d.name().to_owned();
            s.native_width = sps.width;
            s.native_height = sps.height;
            drop(s);
            decoder = Some(d);
            size = Some((sps.width, sps.height));
        }
        // Nothing decodes before the first keyframe with an SPS.
        let Some(dec) = decoder.as_mut() else {
            totals.skipped_waiting_keyframe += 1;
            continue;
        };
        totals.keyframes += u64::from(unit.keyframe);
        let t0 = now_us();
        let img = match dec.decode_yuv(&unit.data) {
            Ok(Some(img)) => img,
            Ok(None) => continue,
            Err(e) => {
                totals.decode_errors += 1;
                tracing::warn!("miracast: decode error: {e}");
                continue;
            }
        };
        let decode_ms = (now_us().saturating_sub(t0)) as f64 / 1000.0;
        all.record("decode", decode_ms);
        window.record("decode", decode_ms);
        totals.frames += 1;
        w_frames += 1;
        let (width, height) = (img.width, img.height);
        let picture = if opts.rgba {
            let mut rgba = Vec::new();
            converter.convert_yuv(&img, &mut rgba)?;
            Picture::Rgba(rgba)
        } else {
            Picture::Yuv(img)
        };
        if totals.frames == 1 {
            let mut s = status.lock_ok();
            s.first_frame = true;
            s.note = None;
        }
        let display = DisplayFrame {
            width,
            height,
            picture,
            capture_us_viewer: t0,
        };
        if let Err(TrySendError::Full(display)) = frames_tx.try_send(display) {
            let _ = frames_drain.try_recv();
            let _ = frames_tx.try_send(display);
        }
        if let Some(cb) = &on_frame {
            cb();
        }
        let elapsed = w_start.elapsed();
        if elapsed >= Duration::from_secs(1) {
            let secs = elapsed.as_secs_f64();
            let mut s = status.lock_ok();
            s.source_fps = w_frames as f64 / secs;
            s.bitrate_mbps = w_bytes as f64 * 8.0 / secs / 1e6;
            s.decode_ms = window
                .summaries()
                .into_iter()
                .find(|(n, _)| *n == "decode")
                .map(|(_, v)| v.mean)
                .unwrap_or(0.0);
            s.last_second = format!(
                "Miracast {:.1} fps, {:.2} Mbit/s, decode {:.2} ms",
                s.source_fps, s.bitrate_mbps, s.decode_ms
            );
            drop(s);
            window.clear();
            (w_start, w_frames, w_bytes) = (Instant::now(), 0, 0);
        }
    }
    Ok((totals, all))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// First SPS of the TB336FU's stream (run 3): 1920x1080.
    const SPS_1080P: [u8; 20] = [
        0x67, 0x64, 0x00, 0x28, 0xac, 0x1a, 0xd0, 0x0f, 0x00, 0x44, 0xfc, 0xb3, 0x50, 0x20, 0x20,
        0x20, 0x78, 0x44, 0x23, 0x50,
    ];

    fn unit(keyframe: bool, sps: bool) -> VideoAccessUnit {
        let mut data = Vec::new();
        if sps {
            data.extend_from_slice(&[0, 0, 0, 1]);
            data.extend_from_slice(&SPS_1080P);
        }
        data.extend_from_slice(&[0, 0, 0, 1, if keyframe { 0x65 } else { 0x41 }, 0x88]);
        VideoAccessUnit {
            pts_90khz: 0,
            keyframe,
            data,
        }
    }

    #[test]
    fn decoder_is_sized_from_the_sps_of_a_keyframe() {
        let sps = resize_needed(None, &unit(true, true)).unwrap();
        assert_eq!((sps.width, sps.height), (1920, 1080));
        assert!(resize_needed(Some((1920, 1080)), &unit(true, true)).is_none());
        assert!(resize_needed(Some((1080, 1920)), &unit(true, true)).is_some());
        assert!(resize_needed(None, &unit(false, false)).is_none());
        assert!(resize_needed(None, &unit(true, false)).is_none());
    }

    #[test]
    fn a_full_queue_drops_until_the_next_keyframe() {
        let (tx, rx) = bounded(1);
        let mut f = Forward::new(tx);
        f.video(unit(true, true));
        f.video(unit(false, false)); // queue full: dropped, now skipping
        let _ = rx.try_recv();
        f.video(unit(false, false)); // still skipping (no keyframe yet)
        assert!(rx.try_recv().is_err());
        f.video(unit(true, true));
        assert!(rx.try_recv().unwrap().keyframe);
        assert_eq!(f.dropped, 2);
    }

    #[test]
    fn log_lines_drive_the_gui_status() {
        let s = Mutex::new(ViewerStatus::default());
        apply_log_line(&s, "RTSP connected to 192.168.49.1:7236");
        assert_eq!(
            s.lock_ok().note.as_deref(),
            Some("RTSP connected to 192.168.49.1:7236")
        );
        apply_log_line(&s, ">>> sink\nOPTIONS * RTSP/1.0");
        apply_log_line(&s, "start media on UDP 19000");
        assert!(s.lock_ok().connected);
        assert!(s.lock_ok().note.is_none());
        apply_log_line(
            &s,
            "diagnostic: No video arrived; firewall-cmd --add-port=19000/udp",
        );
        assert!(s
            .lock_ok()
            .error
            .as_deref()
            .unwrap()
            .contains("firewall-cmd"));
        apply_log_line(&s, "stats: 5s packets 10 lost 0 (0.00%)");
        assert!(s.lock_ok().bottleneck.starts_with("stats: 5s"));
    }

    #[test]
    fn a_lost_link_is_a_readable_error_and_a_user_stop_is_none() {
        let s = Mutex::new(ViewerStatus::default());
        let report = SinkReport {
            close_reason: Some("link lost".into()),
            link_lost: true,
            ..SinkReport::default()
        };
        finish(&s, &report, false);
        assert!(s
            .lock_ok()
            .error
            .as_deref()
            .unwrap()
            .contains("Wi-Fi Direct link"));
        let s = Mutex::new(ViewerStatus::default());
        finish(&s, &report, true);
        assert!(s.lock_ok().error.is_none());
    }

    /// End to end on localhost: real H.264 from the FilmCraft encoder -> RTP/TS from the loopback
    /// test source -> this module -> the app's decoder -> a picture of the SPS size.
    #[cfg(feature = "filmcraft")]
    #[test]
    fn real_h264_over_rtp_reaches_the_viewer_at_the_sps_size() {
        use miracast_loopback::TestSource;
        use mirrorwire_media::encode::{EncoderBackend, EncoderConfig};
        use mirrorwire_media::filmcraft::FilmEncoder;
        use mirrorwire_media::frame::{PixelFormat, RawFrame};

        let (w, h) = (320u32, 240u32);
        let cfg = EncoderConfig {
            width: w,
            height: h,
            fps: 30,
            backend: EncoderBackend::Filmcraft,
            ..EncoderConfig::default()
        };
        let mut enc = FilmEncoder::new(&cfg).unwrap();
        let mut units = Vec::new();
        for i in 0..6u64 {
            let px = (0..w * h).flat_map(|p| [(p % 255) as u8, (i * 40) as u8, 90, 255]);
            let raw = RawFrame {
                seq: i,
                width: w,
                height: h,
                stride: w as usize * 4,
                format: PixelFormat::Bgrx,
                data: px.collect(),
                capture_us: i * 33_333,
                dmabuf: None,
            };
            for f in enc.encode(&raw, i == 0).unwrap() {
                units.push(f.data.to_vec());
            }
        }
        assert!(!units.is_empty());

        let listener = TestSource::listen();
        let udp_port = UdpSocket::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let handle = spawn_direct(
            listener.addr(),
            MiracastOptions {
                hwdec: false,
                software_decoder: SoftwareDecoder::FilmCraft,
                rgba: true,
                peer: None,
                rtp_port: udp_port,
            },
            None,
        );
        let mut src = listener.accept();
        src.negotiate();
        for au in &units {
            let d = src.datagrams_for(au);
            src.send_datagrams(&d);
        }
        let frame = handle
            .frames
            .recv_timeout(Duration::from_secs(10))
            .expect("a decoded picture");
        assert_eq!((frame.width, frame.height), (w, h));
        let s = handle.status.lock_ok().clone();
        assert_eq!(
            (s.native_width, s.native_height),
            (w, h),
            "sized from the SPS"
        );
        assert!(s.connected && s.first_frame);
        handle.stop();
        let td = src.read_message();
        assert!(
            matches!(td, miracast_rtsp::message::Message::Request(ref r) if r.method == "TEARDOWN"),
            "{td:?}"
        );
    }
}
