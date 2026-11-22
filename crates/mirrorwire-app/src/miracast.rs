//! Miracast sink as a viewer source (cargo feature `miracast`, spike; docs/agents/miracast.md
//! "Viewer integration"). An Android tablet that casts with "Wireless display" shows up in
//! the normal video view: Wi-Fi Direct join through NetworkManager (`miracast-nm`), RTSP and
//! RTP/TS (`miracast-loopback`'s sink driver on `miracast-rtsp`/`miracast-rtp`), then the
//! app's own H.264 decoders (`mirrorwire_media::decode`) sized from the stream's SPS
//! (`miracast_rtp::h264::find_sps`), never from the RTSP M4 pick (run 3: M4 said level 3.2,
//! the SPS 4.0). The result is a [`ViewerHandle`], so the GUI draws it like a QUIC stream.
//!
//! Threads: a session thread (NM join, then the blocking RTSP/RTP loop) feeds access units
//! through a 6-deep channel into a decode thread, which keeps only the newest picture for the
//! GUI (latency beats smoothness). A full channel drops units until the next keyframe.

use crate::miracast_flow::Phase;
use crate::sync::LockExt;
use crate::viewer::{DisplayFrame, Picture, Totals, ViewerHandle, ViewerStatus};
use anyhow::Result;
use crossbeam_channel::{bounded, Receiver, Sender, TrySendError};
use miracast_loopback::{run_sink_logged, RunOptions, SinkEvent, SinkReport};
use miracast_rtp::h264::{find_sps, Sps};
use miracast_rtp::{AudioFrame, MediaSink, VideoAccessUnit};
use mirrorwire_media::decode::{self, HwDecode, RgbaConverter, SoftwareDecoder, VideoDecoder};
use mirrorwire_proto::clock::now_us;
use mirrorwire_proto::stats::StageStats;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Access units waiting for the decoder (bounded: a growing queue is a latency bug). Six
/// like the Android viewer: an IDR picture decodes several times slower than a P picture, and
/// with 2 (hardware run 6d) every IDR overflowed the queue, which forced a skip to the next
/// keyframe, whose IDR overflowed it again: ~1.7 pictures/s although FilmCraft + RGBA
/// conversion average ~22 ms per 1080p picture (offline, stream-run5-5). The queue is empty
/// while the decoder keeps up, so latency only grows during a burst.
const DECODE_QUEUE: usize = 6;
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

/// The Miracast side of a running flow, next to its [`ViewerHandle`].
pub struct MiracastSession {
    /// Where the flow is (see [`crate::miracast_flow`]); the GUI renders it.
    pub phase: Arc<Mutex<Phase>>,
    /// Receives `()` once the session thread has finished, including leaving the Wi-Fi Direct
    /// group and removing our NetworkManager profile.
    pub finished: Receiver<()>,
}

impl MiracastSession {
    pub fn phase(&self) -> Phase {
        self.phase.lock_ok().clone()
    }
}

/// Starts discovery, the join and the stream (the "View your Android device with Miracast"
/// button, D21); the viewer handle works like a QUIC viewer's. `stop()` on it cancels at any
/// point, also while waiting for the user on the Android device, and cleans up NM.
pub fn spawn(
    runtime: &tokio::runtime::Handle,
    opts: MiracastOptions,
    on_frame: Option<Arc<dyn Fn() + Send + Sync>>,
) -> (ViewerHandle, MiracastSession) {
    let runtime = runtime.clone();
    start(opts, on_frame, move |ctx| session_nm(&runtime, ctx))
}

/// Like [`spawn`] but without Wi-Fi Direct: the source's RTSP server is already reachable at
/// `source` (an existing group, or a test source on localhost). Only the end-to-end test
/// uses it, and that test needs the FilmCraft encoder.
#[cfg(all(test, feature = "filmcraft"))]
pub fn spawn_direct(
    source: SocketAddr,
    opts: MiracastOptions,
    on_frame: Option<Arc<dyn Fn() + Send + Sync>>,
) -> (ViewerHandle, MiracastSession) {
    start(opts, on_frame, move |ctx| {
        let end = ctx.stream(source, None);
        ctx.set_phase(|p| p.on_end(&end.0, end.1));
    })
}

/// What the session thread needs.
struct Ctx {
    status: Arc<Mutex<ViewerStatus>>,
    phase: Arc<Mutex<Phase>>,
    stop: Arc<AtomicBool>,
    units: Sender<VideoAccessUnit>,
    rtp_port: u16,
    peer: Option<String>,
}

fn start(
    opts: MiracastOptions,
    on_frame: Option<Arc<dyn Fn() + Send + Sync>>,
    session: impl FnOnce(&Ctx) + Send + 'static,
) -> (ViewerHandle, MiracastSession) {
    let status = Arc::new(Mutex::new(ViewerStatus {
        decoder: "…".into(),
        ..ViewerStatus::default()
    }));
    let (frames_tx, frames) = bounded::<DisplayFrame>(1);
    let (done_tx, done) = bounded(1);
    let (units_tx, units_rx) = bounded::<VideoAccessUnit>(DECODE_QUEUE);
    let stop = Arc::new(AtomicBool::new(false));
    let phase = Arc::new(Mutex::new(Phase::Starting));
    let (finished_tx, finished) = bounded(1);
    let ctx = Ctx {
        status: status.clone(),
        phase: phase.clone(),
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
                    Err(e) => fail(&status, format!("decoder for the Android stream: {e:#}")),
                }
            });
        if let Err(e) = spawned {
            fail(&status, format!("cannot start the decode thread: {e}"));
        }
    }
    let spawned = std::thread::Builder::new()
        .name("miracast-session".into())
        .spawn(move || {
            session(&ctx);
            let _ = finished_tx.send(());
        });
    if let Err(e) = spawned {
        fail(
            &status,
            format!("cannot start the receiver for the Android device: {e}"),
        );
        *phase.lock_ok() = Phase::Failed(crate::miracast_flow::Issue::Other(e.to_string()));
    }
    (
        ViewerHandle {
            status,
            frames,
            stop,
            done,
        },
        MiracastSession { phase, finished },
    )
}

fn fail(status: &Mutex<ViewerStatus>, text: String) {
    tracing::warn!("miracast: {text}");
    let mut s = status.lock_ok();
    s.connected = false;
    s.error = Some(text);
}

/// NM flow: discovery, group join (the user taps this laptop on the Android device), then the
/// stream to the group owner. Cancel (`stop`) works in every step; the group is always left and
/// our volatile NM profile removed before the final phase is published, so a new attempt
/// cannot race with the old cleanup.
fn session_nm(runtime: &tokio::runtime::Handle, ctx: &Ctx) {
    use miracast_loopback::nm_flow::{join_source, FlowOptions};
    use miracast_nm::zbus_backend::ZbusNm;
    use miracast_nm::{NmP2pProvider, Sink, SinkConfig};

    let api = match runtime.block_on(ZbusNm::connect_system()) {
        Ok(a) => a,
        Err(e) => return ctx.set_phase(|p| p.on_join_error(&e)),
    };
    let sink = Sink::new(NmP2pProvider::new(api), SinkConfig::default());
    let flow = FlowOptions {
        peer_name: ctx.peer.clone(),
        ..FlowOptions::default()
    };
    let mut on_flow = |e: miracast_loopback::nm_flow::FlowEvent| {
        tracing::info!("miracast: {e}");
        ctx.set_phase(|p| p.on_flow(&e));
    };
    let joined = runtime.block_on(async {
        tokio::select! {
            r = join_source(&sink, &flow, &mut on_flow) => Some(r),
            () = wait_for(&ctx.stop) => None,
        }
    });
    let end = match joined {
        Some(Ok(j)) if !ctx.stop.load(Ordering::Acquire) => {
            let (report, cancelled) = ctx.stream(j.rtsp, Some(j.group.interface));
            Some(Phase::Streaming { warning: None }.on_end(&report, cancelled))
        }
        Some(Err(e)) => Some(ctx.phase.lock_ok().on_join_error(&e)),
        // Cancelled while joining (also right after the join finished).
        _ => Some(Phase::Ended { by_source: false }),
    };
    if let Err(e) = runtime.block_on(sink.stop()) {
        tracing::warn!("miracast: leaving the Wi-Fi Direct group failed: {e}");
    }
    if let Some(end) = end {
        tracing::info!("miracast: flow ended: {end:?}");
        ctx.set_phase(|_| end);
    }
}

/// Resolves once `flag` is set (polled; the flag is a plain atomic shared with the GUI).
async fn wait_for(flag: &AtomicBool) {
    while !flag.load(Ordering::Acquire) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

impl Ctx {
    fn set_phase(&self, f: impl FnOnce(&Phase) -> Phase) {
        let mut p = self.phase.lock_ok();
        *p = f(&p);
    }

    /// RTSP + RTP until the session ends; returns the report and whether the user cancelled.
    fn stream(&self, source: SocketAddr, interface: Option<String>) -> (SinkReport, bool) {
        let udp = match UdpSocket::bind(("0.0.0.0", self.rtp_port)) {
            Ok(u) => u,
            Err(e) => {
                let reason = format!("cannot use UDP port {} for the stream: {e}", self.rtp_port);
                let report = SinkReport {
                    close_reason: Some(reason),
                    ..SinkReport::default()
                };
                return (report, false);
            }
        };
        let want_idr = Arc::new(AtomicBool::new(false));
        let opts = RunOptions {
            idr_request: Some(want_idr.clone()),
            interface,
            close_on_silence: Some(Duration::from_secs(8)),
            stats_every: Some(Duration::from_secs(5)),
            stop: Some(self.stop.clone()),
            ..RunOptions::new(Duration::from_secs(24 * 3600))
        };
        let mut forward = Forward::new(self.units.clone(), want_idr);
        let report = run_sink_logged(source, udp, &mut forward, &opts, &mut |e| {
            apply_event(&self.status, &e);
            self.set_phase(|p| p.on_sink(&e));
        });
        self.status.lock_ok().connected = false;
        (report, self.stop.load(Ordering::Acquire))
    }
}

/// Mirrors the sink driver's events into the viewer status (HUD) and the log. RTSP message
/// dumps go to the debug log only.
fn apply_event(status: &Mutex<ViewerStatus>, e: &SinkEvent) {
    if matches!(e, SinkEvent::Rtsp { .. }) {
        tracing::debug!("miracast: {e}");
        return;
    }
    tracing::info!("miracast: {e}");
    let mut s = status.lock_ok();
    match e {
        SinkEvent::MediaStarted { .. } => s.connected = true,
        SinkEvent::NoRtp(d) => s.last_second = d.to_string(),
        SinkEvent::Stalled(d) => s.last_second = d.to_string(),
        SinkEvent::Stats(line) => line.clone_into(&mut s.bottleneck),
        _ => {}
    }
}

/// `MediaSink` that hands video to the decode thread. When the decoder falls behind, units are
/// dropped until the next keyframe (a decoder fed a broken reference chain shows garbage).
struct Forward {
    tx: Sender<VideoAccessUnit>,
    skipping: bool,
    dropped: u64,
    /// Asks the sink driver for an IDR when units had to be dropped (run 6c: with only the
    /// source's periodic keyframes, a slow software decoder showed under 1 picture/s).
    want_idr: Arc<AtomicBool>,
}

impl Forward {
    fn new(tx: Sender<VideoAccessUnit>, want_idr: Arc<AtomicBool>) -> Self {
        Self {
            tx,
            skipping: false,
            dropped: 0,
            want_idr,
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
                self.want_idr.store(true, Ordering::Release);
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
            tracing::info!(
                "miracast: first picture {width}x{height} via {}",
                dec.name()
            );
            let mut s = status.lock_ok();
            s.first_frame = true;
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
                "Android stream {:.1} fps, {:.2} Mbit/s, decode {:.2} ms, {} pictures",
                s.source_fps, s.bitrate_mbps, s.decode_ms, totals.frames
            );
            tracing::info!("miracast: {}", s.last_second);
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
        let want_idr = Arc::new(AtomicBool::new(false));
        let mut f = Forward::new(tx, want_idr.clone());
        f.video(unit(true, true));
        assert!(!want_idr.load(Ordering::Acquire));
        f.video(unit(false, false)); // queue full: dropped, now skipping, IDR wanted
        assert!(want_idr.load(Ordering::Acquire));
        let _ = rx.try_recv();
        f.video(unit(false, false)); // still skipping (no keyframe yet)
        assert!(rx.try_recv().is_err());
        f.video(unit(true, true));
        assert!(rx.try_recv().unwrap().keyframe);
        assert_eq!(f.dropped, 2);
    }

    #[test]
    fn sink_events_drive_the_hud_status() {
        let s = Mutex::new(ViewerStatus::default());
        apply_event(
            &s,
            &SinkEvent::Rtsp {
                outgoing: true,
                text: "OPTIONS * RTSP/1.0".into(),
            },
        );
        assert!(!s.lock_ok().connected);
        apply_event(&s, &SinkEvent::MediaStarted { rtp_port: 19000 });
        assert!(s.lock_ok().connected);
        apply_event(
            &s,
            &SinkEvent::Stats("stats: 5s packets 10 lost 0 (0.00%)".into()),
        );
        assert!(s.lock_ok().bottleneck.starts_with("stats: 5s"));
        apply_event(
            &s,
            &SinkEvent::Stalled(miracast_rtp::diag::RtpStalled {
                rtp_port: 19000,
                silent_for: Duration::from_secs(5),
            }),
        );
        assert!(s.lock_ok().last_second.contains("Video stopped"));
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
        let (handle, session) = spawn_direct(
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
        assert_eq!(session.phase(), Phase::Streaming { warning: None });
        assert_eq!(
            crate::miracast_flow::view(&session.phase(), "t480s", true),
            crate::miracast_flow::View::Video
        );
        handle.stop();
        // A slow (debug-build) decoder may overflow the queue: IDR requests may come first.
        let idr_requests = src.await_teardown();
        assert!(idr_requests <= 6, "{idr_requests}");
        session
            .finished
            .recv_timeout(Duration::from_secs(5))
            .expect("the session thread finishes after Cancel");
        assert_eq!(session.phase(), Phase::Ended { by_source: false });
    }
}
