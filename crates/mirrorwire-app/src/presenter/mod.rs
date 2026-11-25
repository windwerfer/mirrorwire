mod args;
mod capture;
#[cfg(feature = "gui")]
mod status;
pub use args::{PresentArgs, SourceKind};
pub use capture::{CaptureCtl, CaptureStatus};
#[cfg(feature = "gui")]
pub use status::{EncodeHud, PresentPhase, PresentStatus};

use crate::capture_prefs::CapturePrefs;
use crate::sync::LockExt;
use anyhow::{bail, Result};
use mirrorwire_media::encode::{parse_size, Encoder, EncoderConfig};
use mirrorwire_media::frame::EncodedFrame;
use mirrorwire_media::pipeline::{
    continue_capture_encode, open_encoder_for, wait_first_frame, CaptureExit, Controls, Tick,
    VideoSink,
};
use mirrorwire_media::source::testpattern::TestPattern;
use mirrorwire_media::source::{CursorCapture, CursorSink, FrameSource};
use mirrorwire_net::diag::FirewallHint;
use mirrorwire_net::diag::IncomingAttempt;
use mirrorwire_net::discovery::Advertisement;
use mirrorwire_net::identity::{short_fingerprint, Identity};
use mirrorwire_net::pairing::{
    lan_ipv4s, primary_ipv4, qr_terminal, random_pairing_code, ConnectInfo,
};
use mirrorwire_net::transport::{OutgoingFrame, Presenter, PresenterConfig};
use mirrorwire_proto::control::StreamInfo;
use mirrorwire_proto::cursor::CursorEvent;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
#[cfg(feature = "gui")]
use std::thread::JoinHandle;
use tokio::sync::{mpsc, watch};

#[cfg(feature = "gui")]
pub struct PresenterHandle {
    status: Arc<Mutex<PresentStatus>>,
    connected: Arc<Mutex<Option<watch::Receiver<bool>>>>,
    controls: Controls,
    settings: Arc<Mutex<EncoderConfig>>,
    attempts: Arc<Mutex<Vec<IncomingAttempt>>>,
    hud: Arc<Mutex<EncodeHudShim>>,
    capture: Arc<Mutex<CaptureCtl>>,
    config_dir: PathBuf,
    thread: Option<JoinHandle<()>>,
}

#[cfg(feature = "gui")]
impl PresenterHandle {
    pub fn stop(&self) {
        self.controls.stop();
    }

    pub fn apply_settings(&self, cfg: EncoderConfig) {
        *self.settings.lock_ok() = cfg;
        self.controls.request_keyframe();
    }

    /// Cap the encoded size (Extend mode: the chosen size). A change reopens the encoder
    /// through the usual size check; the same value is a no-op.
    pub fn set_max_size(&self, max: Option<(u32, u32)>) {
        let mut s = self.settings.lock_ok();
        if s.max_size != max {
            s.max_size = max;
        }
    }

    /// Change the capture mode, cursor mode or frame-rate cap: saved for the next start,
    /// and the capture is reopened (silently, with the portal restore token)
    /// when it changes what is asked of the compositor.
    pub fn apply_capture(&self, prefs: CapturePrefs) {
        if let Err(e) = mirrorwire_net::settings::save_capture(
            &self.config_dir,
            prefs.mode.as_str(),
            prefs.cursor.as_str(),
            &prefs.fps.label(),
            &prefs.fec.as_string(),
            prefs.adaptive,
        ) {
            tracing::warn!("could not save capture settings: {e:#}");
        }
        let restart = {
            let mut c = self.capture.lock_ok();
            if c.prefs.fec != prefs.fec {
                tracing::info!("FEC set to {}", prefs.fec.as_string());
            }
            if c.prefs.adaptive != prefs.adaptive {
                tracing::info!(
                    "adaptive bitrate {}",
                    if prefs.adaptive { "on" } else { "off" }
                );
            }
            // FEC and adaptive bitrate apply to the next frame; no restart needed.
            c.fec.set_mode(prefs.fec);
            c.fec.set_adaptive(prefs.adaptive);
            c.prefs = prefs;
            c.needs_restart()
        };
        if restart {
            tracing::info!(
                "capture settings changed (mode {}, cursor {}, fps {}); reopening the capture",
                prefs.mode.as_str(),
                prefs.cursor.as_str(),
                prefs.fps.label()
            );
            self.controls.request_source_restart();
        }
    }

    /// Switch the virtual uinput pen on or off for this session (D26). The caller passes
    /// `true` only when the system check passed; the saved setting is written separately.
    #[cfg(target_os = "linux")]
    pub fn set_pen_pressure(&self, on: bool) {
        self.capture.lock_ok().pen_pressure = on;
    }

    pub fn refresh_firewall(&self) {
        let port = self.status.lock_ok().port;
        let hint = FirewallHint::for_port(port);
        self.status.lock_ok().firewall = Some(hint);
    }

    pub fn poll(&self) -> PresentStatus {
        let mut s = self.status.lock_ok().clone();
        if let Some(rx) = self.connected.lock_ok().as_ref() {
            s.viewer_connected = *rx.borrow();
        }
        s.attempts = self.attempts.lock_ok().clone();
        s.hud = self.hud.lock_ok().to_hud();
        s.settings = self.settings.lock_ok().clone();
        let c = self.capture.lock_ok();
        s.capture = c.status();
        if !c.source_desc.is_empty() {
            s.source = Some(c.source_desc.clone());
        }
        s
    }
}

#[cfg(feature = "gui")]
impl Drop for PresenterHandle {
    fn drop(&mut self) {
        self.controls.stop();
        let _ = self.thread.take();
    }
}

struct QuicSink {
    frames: mpsc::Sender<OutgoingFrame>,
    connected: watch::Receiver<bool>,
    /// The viewer shows no video (`video_paused`): skip encoding like with no viewer.
    paused: watch::Receiver<bool>,
    controls: Controls,
    next_id: u32,
}

impl VideoSink for QuicSink {
    fn send(&mut self, frame: EncodedFrame) -> Result<()> {
        let frame_id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        let out = OutgoingFrame {
            frame_id,
            keyframe: frame.keyframe,
            data: frame.data,
            capture_us: frame.capture_us,
            encoded_us: frame.encoded_us,
        };
        match self.frames.try_send(out) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => {
                tracing::debug!("network backlog, dropped frame {frame_id}");
                self.controls.request_keyframe();
                Ok(())
            }
            Err(mpsc::error::TrySendError::Closed(_)) => bail!("network task stopped"),
        }
    }

    fn wants_frames(&self) -> bool {
        *self.connected.borrow() && !*self.paused.borrow()
    }
}

/// Open the capture source for the current wanted cursor mode and frame
/// rate, and record what it delivers.
fn open_source(
    args: &PresentArgs,
    config_dir: &Path,
    ctl: &Arc<Mutex<CaptureCtl>>,
    sink: &CursorSink,
) -> Result<Box<dyn FrameSource>> {
    let (cursor, fps, mode, want_input) = {
        let c = ctl.lock_ok();
        (
            c.wanted_cursor(),
            c.prefs.fps.request(),
            c.prefs.mode,
            c.want_input,
        )
    };
    // Input (pen, remote keys in the capture session) only when the viewer switched its pen on,
    // or when the desktop already remembers that permission (then no dialog appears). Otherwise
    // the start asks for the screen only.
    let with_input = want_input || input_remembered(config_dir, mode);
    let source: Box<dyn FrameSource> = match args.source {
        SourceKind::Test => {
            let (w, h) = if let Some(s) = &args.size {
                parse_size(s).unwrap_or((args.width, args.height))
            } else {
                (args.width, args.height)
            };
            let pattern = TestPattern::new(w, h, fps.min(60), args.frames);
            if cursor == CursorCapture::Metadata {
                Box::new(pattern.with_cursor(sink.clone()))
            } else {
                Box::new(pattern)
            }
        }
        SourceKind::Portal => {
            let ctl_status = ctl.clone();
            let status: Arc<dyn Fn(String) + Send + Sync> =
                Arc::new(move |line| ctl_status.lock_ok().remote_note = Some(line));
            open_portal(
                args, config_dir, mode, cursor, fps, sink, with_input, &status,
            )?
        }
    };
    let mut c = ctl.lock_ok();
    // The test pattern has no input to ask for; never reopen it for the pen.
    c.asked_input = with_input || matches!(args.source, SourceKind::Test);
    c.asked_cursor = cursor;
    c.asked_mode = mode;
    c.asked_fps = fps;
    c.active_cursor = source.cursor_capture();
    c.cursor_note = source.cursor_note();
    c.negotiated_fps = None;
    c.source_desc = source.describe();
    tracing::info!(
        "capture: cursor {} (asked {}){}, frame rate up to {fps}",
        c.active_cursor.as_str(),
        cursor.as_str(),
        c.cursor_note
            .as_deref()
            .map(|n| format!(" [{n}]"))
            .unwrap_or_default()
    );
    Ok(source)
}

/// After the first frame the compositor's rate is known: run the encoder at
/// min(asked, negotiated) and keep a custom keyframe interval in seconds.
fn apply_negotiated_fps(
    source: &dyn FrameSource,
    ctl: &Mutex<CaptureCtl>,
    settings: &Mutex<EncoderConfig>,
) -> u32 {
    let mut c = ctl.lock_ok();
    c.negotiated_fps = source.negotiated_fps();
    c.source_desc = source.describe();
    let fps = effective_fps(c.asked_fps, c.negotiated_fps);
    let mut s = settings.lock_ok();
    if s.fps != fps {
        if let Some(k) = s.keyint {
            s.keyint = Some((k as u64 * fps as u64 / s.fps.max(1) as u64).max(1) as u32);
        }
        tracing::info!(
            "encoder frame rate {} -> {fps} fps (compositor allows {:?})",
            s.fps,
            c.negotiated_fps
        );
        s.fps = fps;
    }
    fps
}

/// The encoder's nominal rate: what we asked for, capped by what the
/// compositor agreed to; 60 when "max" is asked and nothing is known.
fn effective_fps(asked: u32, negotiated: Option<u32>) -> u32 {
    let asked = asked.max(1);
    match negotiated {
        Some(n) => asked.min(n).max(1),
        None => asked.min(60),
    }
}

/// The desktop remembers the pen/keys permission for this mode's combined session.
#[cfg(all(target_os = "linux", feature = "portal"))]
fn input_remembered(config_dir: &Path, mode: mirrorwire_media::source::CaptureMode) -> bool {
    crate::remote_rights::combined_saved(config_dir, mode)
}

#[cfg(not(all(target_os = "linux", feature = "portal")))]
fn input_remembered(_config_dir: &Path, _mode: mirrorwire_media::source::CaptureMode) -> bool {
    false
}

#[cfg(all(target_os = "linux", feature = "portal"))]
#[allow(clippy::too_many_arguments)]
fn open_portal(
    args: &PresentArgs,
    config_dir: &Path,
    mode: mirrorwire_media::source::CaptureMode,
    cursor: CursorCapture,
    fps: u32,
    sink: &CursorSink,
    with_input: bool,
    remote_status: &Arc<dyn Fn(String) + Send + Sync>,
) -> Result<Box<dyn FrameSource>> {
    use mirrorwire_media::source::portal::{PortalOptions, PortalSource};
    Ok(Box::new(PortalSource::open(PortalOptions {
        restore_token_path: config_dir.join(mode.restore_token_file()),
        cursor,
        cursor_sink: Some(sink.clone()),
        mode,
        fps,
        input: with_input.then(|| mirrorwire_media::source::portal::InputOptions {
            restore_token_path: config_dir
                .join(mirrorwire_media::source::portal::input_token_file(mode)),
            status: remote_status.clone(),
        }),
        // Only VAAPI can import DMA-BUFs. With `auto` the encoder may still
        // turn out to be x264; the pipeline then asks the source to switch
        // to shared memory.
        dmabuf: !args.no_dmabuf
            && matches!(
                args.encoder,
                mirrorwire_media::encode::EncoderBackend::Auto
                    | mirrorwire_media::encode::EncoderBackend::Vaapi
            )
            && std::env::var_os("MIRRORWIRE_NO_DMABUF").is_none(),
    })?))
}

#[cfg(not(all(target_os = "linux", feature = "portal")))]
#[allow(clippy::too_many_arguments)]
fn open_portal(
    _args: &PresentArgs,
    _config_dir: &Path,
    _mode: mirrorwire_media::source::CaptureMode,
    _cursor: CursorCapture,
    _fps: u32,
    _sink: &CursorSink,
    _with_input: bool,
    _remote_status: &Arc<dyn Fn(String) + Send + Sync>,
) -> Result<Box<dyn FrameSource>> {
    bail!("portal capture is only available on Linux builds with the `portal` feature; use --source test")
}

struct ReadyInfo {
    name: String,
    uri: String,
    pairing_code: Option<String>,
    fingerprint: String,
    encoder: String,
    source: String,
    listen_addr: SocketAddr,
    advertised: Vec<SocketAddr>,
    rc_summary: String,
    conversion: String,
}

fn stream_info_from(encoder: &Encoder, fps: u32, cursor: CursorCapture) -> StreamInfo {
    let (w, h) = encoder.output_size();
    StreamInfo {
        width: w,
        height: h,
        fps,
        codec: "h264".into(),
        encoder: encoder.name().into(),
        bitrate_kbps: encoder.config().bitrate_kbps,
        intra_refresh: encoder.config().intra_refresh,
        cursor: cursor.mode(),
        // Filled in by `Presenter::start` from the FEC setting.
        fec: String::new(),
    }
}

/// The presenter remote: taps from the viewer become key presses through the RemoteDesktop
/// portal (Linux with the portal feature); elsewhere the taps are dropped.
#[cfg(all(target_os = "linux", feature = "portal"))]
fn start_remote(
    config_dir: &std::path::Path,
    ctl: &Arc<Mutex<CaptureCtl>>,
) -> Option<mpsc::Sender<mirrorwire_proto::control::RemoteInput>> {
    let ctl_mode = ctl.clone();
    let ctl_asking = ctl.clone();
    let ctl = ctl.clone();
    Some(mirrorwire_media::source::remote::spawn_remote_injector(
        config_dir.join(crate::remote_rights::SEPARATE_TOKEN_FILE),
        Arc::new(move |line| ctl.lock_ok().remote_note = Some(line)),
        Arc::new(move |asking| ctl_asking.lock_ok().remote_asking = asking),
        Arc::new(move || {
            ctl_mode.lock_ok().asked_mode == mirrorwire_media::source::CaptureMode::Mirror
        }),
    ))
}

#[cfg(not(all(target_os = "linux", feature = "portal")))]
fn start_remote(
    _config_dir: &std::path::Path,
    _ctl: &Arc<Mutex<CaptureCtl>>,
) -> Option<mpsc::Sender<mirrorwire_proto::control::RemoteInput>> {
    None
}

/// Routes remote input (keys, pen/touch) from the viewer to the current capture session when
/// that session carries input (combined portal session), otherwise to the separate injector.
/// The network side holds one stable sender; the capture source can be reopened underneath.
struct RemoteRoute {
    tx: mpsc::Sender<mirrorwire_proto::control::RemoteInput>,
    separate: Option<mpsc::Sender<mirrorwire_proto::control::RemoteInput>>,
    target: Arc<Mutex<Option<mpsc::Sender<mirrorwire_proto::control::RemoteInput>>>>,
}

impl RemoteRoute {
    /// On Linux, pen input first passes the opt-in uinput pen (D26): while
    /// `CaptureCtl::pen_pressure` is on it goes to the virtual pen, everything else (and pen
    /// input when the pen cannot be created) to the portal.
    ///
    /// The viewer's Pen button (`PenMode`, and a stroke from a viewer whose button was already
    /// on) reopens the capture together with input when it runs without; strokes are dropped
    /// until that reopened session is in place, so no second dialog opens meanwhile.
    fn start(
        separate: Option<mpsc::Sender<mirrorwire_proto::control::RemoteInput>>,
        runtime: &tokio::runtime::Handle,
        ctl: &Arc<Mutex<CaptureCtl>>,
        controls: &Controls,
    ) -> Self {
        use mirrorwire_proto::control::RemoteInput;
        let (tx, mut rx) = mpsc::channel(64);
        let target: Arc<Mutex<Option<mpsc::Sender<_>>>> = Arc::new(Mutex::new(separate.clone()));
        let t = target.clone();
        let ctl = ctl.clone();
        let controls = controls.clone();
        runtime.spawn(async move {
            #[cfg(target_os = "linux")]
            let mut pen = crate::pen::PenRouter::new(crate::pen::uinput_opener());
            while let Some(input) = rx.recv().await {
                match input {
                    RemoteInput::PenMode(on) => {
                        if on {
                            request_input(&ctl, &controls);
                        }
                        continue;
                    }
                    RemoteInput::Pointer { .. } => {
                        // Pen already on in the viewer (e.g. after a reconnect): same as
                        // switching it on now.
                        if !ctl.lock_ok().want_input {
                            request_input(&ctl, &controls);
                        }
                        if input_pending(&ctl.lock_ok()) {
                            continue;
                        }
                    }
                    RemoteInput::Key(_) => {}
                }
                #[cfg(target_os = "linux")]
                let input = {
                    let enabled = ctl.lock_ok().pen_pressure;
                    let routed = pen.route(input, enabled);
                    if let Some(note) = routed.note {
                        ctl.lock_ok().pen_note = Some(note);
                    }
                    let Some(input) = routed.portal else {
                        continue;
                    };
                    input
                };
                let dest = t.lock_ok().clone();
                if let Some(dest) = dest {
                    // Full channel: drop (moves are replaced by newer ones).
                    let _ = dest.try_send(input);
                }
            }
        });
        Self {
            tx,
            separate,
            target,
        }
    }

    fn sender(&self) -> mpsc::Sender<mirrorwire_proto::control::RemoteInput> {
        self.tx.clone()
    }

    /// Point the route at a freshly opened source.
    fn set_source(&self, source: &dyn FrameSource, ctl: &Mutex<CaptureCtl>) {
        let own = source.remote_input();
        *self.target.lock_ok() = own.or_else(|| self.separate.clone());
        if let Some(note) = source.input_note() {
            ctl.lock_ok().remote_note = Some(note);
        }
    }
}

/// The viewer wants the pen: remember it for this run and, if the running capture has no input,
/// reopen it together with pointer/keyboard input (the desktop's dialog shows then, with ours).
fn request_input(ctl: &Mutex<CaptureCtl>, controls: &Controls) {
    let restart = {
        let mut c = ctl.lock_ok();
        c.want_input = true;
        let restart = c.needs_input_restart();
        if restart {
            c.remote_asking = true;
        }
        restart
    };
    if restart {
        tracing::info!(
            "viewer switched the pen on: reopening the capture with pointer and keyboard input"
        );
        controls.request_source_restart();
    }
}

/// The capture is being reopened for input: pen strokes wait (are dropped) until it is in place.
fn input_pending(c: &CaptureCtl) -> bool {
    c.needs_input_restart() || (c.want_input && c.remote_asking)
}

/// Shared capture/encode/announce loop used by the CLI and the GUI.
#[allow(clippy::too_many_arguments)]
fn present_session(
    args: PresentArgs,
    config_dir: PathBuf,
    identity: Identity,
    runtime: &tokio::runtime::Handle,
    controls: Controls,
    name: String,
    settings: Arc<Mutex<EncoderConfig>>,
    attempts: Arc<Mutex<Vec<IncomingAttempt>>>,
    hud: Arc<Mutex<EncodeHudShim>>,
    ctl: Arc<Mutex<CaptureCtl>>,
    mut on_ready: impl FnMut(ReadyInfo, watch::Receiver<bool>) -> Result<()>,
) -> Result<()> {
    // Cursor changes go straight from the capture thread to the network,
    // independent of video frames. Full channel = drop (a newer one follows).
    let (cursor_tx, cursor_rx) = mpsc::channel::<CursorEvent>(256);
    let cursor_sink: CursorSink = Arc::new(move |ev| {
        let _ = cursor_tx.try_send(ev);
    });
    let mut source = open_source(&args, &config_dir, &ctl, &cursor_sink)?;
    let Some(first) = wait_first_frame(source.as_mut(), &controls)? else {
        bail!("capture source ended before the first frame");
    };
    let fps = apply_negotiated_fps(source.as_ref(), &ctl, &settings);
    let cfg = settings.lock_ok().clone();
    let encoder = open_encoder_for(&first, &cfg)?;
    let (active_cursor, fec) = {
        let c = ctl.lock_ok();
        (c.active_cursor, c.fec.clone())
    };

    let _guard = runtime.enter();
    let pairing_code = if args.no_pairing {
        None
    } else {
        Some(random_pairing_code()?)
    };
    let keyframe_controls = controls.clone();
    #[cfg(target_os = "linux")]
    {
        let on = crate::pen::enabled_at_start(&config_dir);
        ctl.lock_ok().pen_pressure = on;
        if on {
            tracing::info!("pen pressure on (virtual uinput pen on the first stroke)");
        }
    }
    let route = RemoteRoute::start(start_remote(&config_dir, &ctl), runtime, &ctl, &controls);
    route.set_source(source.as_ref(), &ctl);
    let remote_keys = Some(route.sender());
    let presenter = Presenter::start(
        &identity,
        PresenterConfig {
            bind: SocketAddr::new(
                args.bind.unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED)),
                args.port,
            ),
            stream_info: stream_info_from(&encoder, fps, active_cursor),
            pairing_code: pairing_code.clone(),
            config_dir: config_dir.clone(),
            attempts: attempts.clone(),
            cursor_events: Some(cursor_rx),
            fec: fec.clone(),
            remote_keys,
        },
        Arc::new(move || keyframe_controls.request_keyframe()),
    )?;

    let advertised_ips = if let Some(ip) = args.advertise_ip {
        vec![ip]
    } else {
        let mut ips = lan_ipv4s();
        if ips.is_empty() {
            ips.extend(primary_ipv4());
        }
        if ips.is_empty() {
            ips.push(IpAddr::V4(Ipv4Addr::LOCALHOST));
        }
        ips
    };
    let advertised: Vec<SocketAddr> = advertised_ips
        .into_iter()
        .map(|ip| SocketAddr::new(ip, presenter.local_addr.port()))
        .collect();
    // `advertised_ips` is never empty (localhost is the last resort above).
    let (first_addr, other_addrs) = advertised
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("no address to advertise"))?;
    let info = ConnectInfo {
        addr: *first_addr,
        addrs: other_addrs.to_vec(),
        fingerprint: identity.fingerprint(),
        pairing_code: pairing_code.clone(),
        name: Some(name.clone()),
    };
    let uri = info.to_uri();
    if let Some(path) = &args.uri_file {
        std::fs::write(path, &uri)?;
    }

    on_ready(
        ReadyInfo {
            name: name.clone(),
            uri,
            pairing_code,
            fingerprint: identity.fingerprint(),
            encoder: encoder.name().to_owned(),
            source: source.describe(),
            listen_addr: presenter.local_addr,
            advertised: advertised.clone(),
            rc_summary: encoder.rc_summary().to_owned(),
            conversion: encoder.conversion_path().to_owned(),
        },
        presenter.viewer_connected.clone(),
    )?;

    {
        let mut c = ctl.lock_ok();
        c.viewer_display = Some(presenter.viewer_display.clone());
        c.video_paused = Some(presenter.video_paused.clone());
    }

    let _advert = if args.no_mdns {
        None
    } else {
        match Advertisement::start(
            &name,
            presenter.local_addr.port(),
            &identity.fingerprint(),
            &format!("{}x{}", encoder.output_size().0, encoder.output_size().1),
        ) {
            Ok(a) => Some(a),
            Err(e) => {
                tracing::warn!("mDNS announcement failed: {e:#}");
                None
            }
        }
    };

    let mut sink = QuicSink {
        frames: presenter.frames.clone(),
        connected: presenter.viewer_connected.clone(),
        paused: presenter.video_paused.clone(),
        controls: controls.clone(),
        next_id: 0,
    };
    let mut held = Some(first);
    let mut encoder = encoder;
    let result = loop {
        let hud_reopen = hud.clone();
        let hud_tick = hud.clone();
        let (rate_fec, rate_controls, rate_settings) =
            (fec.clone(), controls.clone(), settings.clone());
        let exit = continue_capture_encode(
            source.as_mut(),
            held.take(),
            encoder,
            &mut sink,
            &controls,
            settings.clone(),
            move |enc| {
                if let Ok(mut h) = hud_reopen.lock() {
                    let (cw, ch) = (h.capture_w, h.capture_h);
                    h.note_frame(enc, cw, ch);
                }
                Ok(())
            },
            move |enc, cw, ch, tick| {
                // Adaptive bitrate: the setting is the ceiling, the link
                // feedback decides the budget, FEC parity comes out of it.
                let ceiling = rate_settings.lock_ok().bitrate_kbps;
                rate_fec.set_ceiling(ceiling);
                rate_controls.set_bitrate_cap(rate_fec.encoder_kbps());
                if let Ok(mut h) = hud_tick.lock() {
                    match tick {
                        Tick::Captured => h.note_frame(enc, cw, ch),
                        Tick::Repeated => h.note_repeat(),
                    }
                }
            },
        );
        match exit {
            Ok(CaptureExit::Finished) => break Ok(()),
            Err(e) => break Err(e),
            Ok(CaptureExit::RestartSource(enc)) => {
                encoder = *enc;
                // Close the old portal session before opening the new one.
                drop(source);
                source = match open_source(&args, &config_dir, &ctl, &cursor_sink) {
                    Ok(s) => s,
                    Err(e) => break Err(e.context("reopening the capture with new settings")),
                };
                match wait_first_frame(source.as_mut(), &controls) {
                    Ok(Some(f)) => held = Some(f),
                    Ok(None) => break Ok(()),
                    Err(e) => break Err(e),
                }
                route.set_source(source.as_ref(), &ctl);
                // The reopened session (with input, when the pen asked for it) is in place.
                ctl.lock_ok().remote_asking = false;
                let fps = apply_negotiated_fps(source.as_ref(), &ctl, &settings);
                let cursor = ctl.lock_ok().active_cursor;
                presenter.update_stream_info(|info| {
                    info.fps = fps;
                    info.cursor = cursor.mode();
                });
            }
        }
    };
    presenter.close();
    drop(sink);
    result
}

/// Timings the encode loop updates; the GUI polls them.
#[derive(Debug, Clone, Default)]
struct EncodeHudShim {
    encoder: String,
    encode_sum_ms: f64,
    encode_avg_ms: f64,
    pack_ms: f64,
    convert_ms: f64,
    upload_ms: f64,
    encode_ms: f64,
    path: String,
    output_w: u32,
    output_h: u32,
    capture_w: u32,
    capture_h: u32,
    rc_summary: String,
    frames_window: u32,
    repeats_window: u32,
    window_start: Option<std::time::Instant>,
    source_fps: f64,
    repeat_fps: f64,
    fallback_note: Option<String>,
}

impl EncodeHudShim {
    fn note_frame(
        &mut self,
        enc: &mirrorwire_media::encode::Encoder,
        capture_w: u32,
        capture_h: u32,
    ) {
        let (p, c, u, e) = enc.last_breakdown_ms();
        self.pack_ms = p;
        self.convert_ms = c;
        self.upload_ms = u;
        self.encode_ms = e;
        self.encode_sum_ms += e;
        if self.encoder != enc.name() {
            self.encoder = enc.name().to_owned();
        }
        self.path = enc.conversion_path().to_owned();
        let (w, h) = enc.output_size();
        self.output_w = w;
        self.output_h = h;
        self.capture_w = capture_w;
        self.capture_h = capture_h;
        self.rc_summary = enc.rc_summary().to_owned();
        self.fallback_note = enc.fallback_note().map(str::to_owned);
        self.frames_window += 1;
        self.roll_window();
    }

    /// The last frame was re-sent because the compositor had no new one.
    /// Counted apart from real frames so "source fps" stays honest.
    fn note_repeat(&mut self) {
        self.repeats_window += 1;
        self.roll_window();
    }

    fn roll_window(&mut self) {
        let now = std::time::Instant::now();
        let start = *self.window_start.get_or_insert(now);
        let elapsed = now.saturating_duration_since(start);
        if elapsed >= std::time::Duration::from_secs(1) {
            let secs = elapsed.as_secs_f64();
            self.source_fps = self.frames_window as f64 / secs;
            self.repeat_fps = self.repeats_window as f64 / secs;
            if self.frames_window > 0 {
                self.encode_avg_ms = self.encode_sum_ms / self.frames_window as f64;
            }
            tracing::info!(
                "encoder {}  capture path {}  {}x{}  source {:.1} fps (+{:.1} repeats/s)  pack/map {:.2} ms  convert {:.2} ms  upload {:.2} ms  encode {:.2} ms (avg {:.2} ms){}",
                self.encoder,
                self.path,
                self.capture_w,
                self.capture_h,
                self.source_fps,
                self.repeat_fps,
                self.pack_ms,
                self.convert_ms,
                self.upload_ms,
                self.encode_ms,
                self.encode_avg_ms,
                self.fallback_note.as_deref().map(|n| format!("  [{n}]")).unwrap_or_default()
            );
            self.encode_sum_ms = 0.0;
            self.frames_window = 0;
            self.repeats_window = 0;
            self.window_start = Some(now);
        }
    }

    #[cfg(feature = "gui")]
    fn to_hud(&self) -> EncodeHud {
        EncodeHud {
            encoder: self.encoder.clone(),
            encode_avg_ms: self.encode_avg_ms,
            pack_ms: self.pack_ms,
            convert_ms: self.convert_ms,
            upload_ms: self.upload_ms,
            encode_ms: self.encode_ms,
            path: self.path.clone(),
            capture_w: self.capture_w,
            capture_h: self.capture_h,
            source_fps: self.source_fps,
            repeat_fps: self.repeat_fps,
            fallback_note: self.fallback_note.clone(),
        }
    }
}

pub fn run(args: PresentArgs, config_dir: &Path) -> Result<()> {
    let identity = Identity::load_or_create(config_dir)?;
    let name = args.name.clone().unwrap_or_else(crate::default_name);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let controls = Controls::default();
    let stop = controls.clone();
    ctrlc_like(move || stop.stop());
    let settings = Arc::new(Mutex::new(args.encoder_config()?));
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let hud = Arc::new(Mutex::new(EncodeHudShim::default()));
    let ctl = Arc::new(Mutex::new(CaptureCtl::new(args.capture)));
    let ctl_print = ctl.clone();
    #[cfg(target_os = "linux")]
    let pen_config_dir = config_dir.to_path_buf();

    let result = present_session(
        args,
        config_dir.to_path_buf(),
        identity,
        runtime.handle(),
        controls,
        name,
        settings,
        attempts,
        hud,
        ctl,
        |ready, _connected| {
            println!("{}", qr_terminal(&ready.uri)?);
            println!(
                "presenter \"{}\" listening on {} (UDP {})",
                ready.name,
                ready.listen_addr,
                ready.listen_addr.port()
            );
            if !ready.advertised.is_empty() {
                println!(
                    "advertised {}",
                    ready
                        .advertised
                        .iter()
                        .map(|a| a.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            println!("encoder {}  source {}", ready.encoder, ready.source);
            {
                let c = ctl_print.lock_ok();
                println!(
                    "cursor {}{}  frame rate asked {} fps, compositor allows {}",
                    c.active_cursor.as_str(),
                    c.cursor_note
                        .as_deref()
                        .map(|n| format!(" ({n})"))
                        .unwrap_or_default(),
                    c.asked_fps,
                    c.negotiated_fps
                        .map(|f| format!("{f} fps"))
                        .unwrap_or_else(|| "?".into())
                );
                println!("forward error correction {}", c.prefs.fec.as_string());
            }
            println!("{}", ready.rc_summary);
            println!("colour conversion {}", ready.conversion);
            println!("fingerprint {}", short_fingerprint(&ready.fingerprint));
            if let Some(code) = &ready.pairing_code {
                println!("pairing code {code}");
            }
            println!("{}", ready.uri);
            #[cfg(target_os = "linux")]
            if let Some(text) = crate::pen::cli_report(&pen_config_dir) {
                println!("{text}");
            }
            let hint = FirewallHint::for_port(ready.listen_addr.port());
            if hint.show {
                println!("{}", hint.message);
                if !hint.guide.is_empty() {
                    println!("{}", hint.guide);
                }
            }
            Ok(())
        },
    );
    runtime.shutdown_timeout(std::time::Duration::from_millis(500));
    result
}

/// Start presenting with the GUI defaults (portal capture, pairing, mDNS).
#[cfg(feature = "gui")]
pub fn spawn_for_gui(
    runtime: tokio::runtime::Handle,
    config_dir: PathBuf,
    identity: Identity,
    name: String,
    port: u16,
    encoder: mirrorwire_media::encode::EncoderBackend,
    on_update: Option<Arc<dyn Fn() + Send + Sync>>,
) -> PresenterHandle {
    let mut args = PresentArgs::gui_defaults();
    args.name = Some(name);
    args.port = port;
    args.encoder = encoder;
    args.capture = crate::capture_prefs::resolve(None, None, None, None, None, &config_dir);
    spawn(runtime, args, config_dir, identity, on_update)
}

/// Start presenting on a background thread and report status for the GUI.
#[cfg(feature = "gui")]
pub fn spawn(
    runtime: tokio::runtime::Handle,
    args: PresentArgs,
    config_dir: PathBuf,
    identity: Identity,
    on_update: Option<Arc<dyn Fn() + Send + Sync>>,
) -> PresenterHandle {
    let name = args.name.clone().unwrap_or_else(crate::default_name);
    let port = args.port;
    let settings = Arc::new(Mutex::new(
        args.encoder_config()
            .unwrap_or_else(|_| EncoderConfig::default()),
    ));
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let hud = Arc::new(Mutex::new(EncodeHudShim::default()));
    let capture = Arc::new(Mutex::new(CaptureCtl::new(args.capture)));
    let handle_capture = capture.clone();
    let handle_config_dir = config_dir.clone();
    let initial_settings = settings.lock_ok().clone();
    let status = Arc::new(Mutex::new(PresentStatus::new(
        name.clone(),
        initial_settings,
        port,
    )));
    {
        let mut s = status.lock_ok();
        s.firewall = Some(FirewallHint::for_port(port));
    }
    let connected = Arc::new(Mutex::new(None::<watch::Receiver<bool>>));
    let controls = Controls::default();
    let handle_status = status.clone();
    let handle_connected = connected.clone();
    let handle_controls = controls.clone();
    let handle_settings = settings.clone();
    let handle_attempts = attempts.clone();
    let handle_hud = hud.clone();
    let ping = on_update.clone();
    let notify = move || {
        if let Some(cb) = &ping {
            cb();
        }
    };

    let thread = std::thread::Builder::new()
        .name("present".into())
        .spawn({
            let connected = connected.clone();
            let hud_status = hud.clone();
            move || {
                {
                    let mut s = status.lock_ok();
                    s.phase = PresentPhase::WaitingForCapture;
                }
                notify();

                let status_ready = status.clone();
                let connected_slot = connected.clone();
                let ping_ready = on_update.clone();
                let result = present_session(
                    args,
                    config_dir,
                    identity,
                    &runtime,
                    controls.clone(),
                    name,
                    settings,
                    attempts,
                    hud,
                    capture,
                    move |ready, rx| {
                        {
                            let mut s = status_ready.lock_ok();
                            s.phase = PresentPhase::Streaming;
                            s.pairing_code = ready.pairing_code;
                            s.uri = Some(ready.uri);
                            s.fingerprint = Some(ready.fingerprint);
                            s.encoder = Some(ready.encoder);
                            s.source = Some(ready.source);
                            s.listen_addr = Some(ready.listen_addr.to_string());
                            s.advertised = ready.advertised.iter().map(|a| a.to_string()).collect();
                            s.port = ready.listen_addr.port();
                            s.rc_summary = Some(ready.rc_summary);
                            s.conversion = Some(ready.conversion);
                        }
                        *connected_slot.lock_ok() = Some(rx);
                        if let Some(cb) = &ping_ready {
                            cb();
                        }
                        Ok(())
                    },
                );

                let mut s = status.lock_ok();
                match result {
                    Ok(()) => {
                        if s.phase != PresentPhase::Failed {
                            s.phase = PresentPhase::Stopped;
                        }
                    }
                    Err(e) => {
                        tracing::error!("presenter: {e:#}");
                        s.phase = PresentPhase::Failed;
                        s.error = Some(format!("{e:#}"));
                    }
                }
                drop(s);
                let _ = hud_status;
                notify();
            }
        })
        .map_err(|e| {
            tracing::error!("presenter: cannot spawn thread: {e}");
            let mut s = handle_status.lock_ok();
            s.phase = PresentPhase::Failed;
            s.error = Some(format!("cannot start the presenter thread: {e}"));
        })
        .ok();

    PresenterHandle {
        status: handle_status,
        connected: handle_connected,
        controls: handle_controls,
        settings: handle_settings,
        attempts: handle_attempts,
        hud: handle_hud,
        capture: handle_capture,
        config_dir: handle_config_dir,
        thread,
    }
}

fn ctrlc_like(on_signal: impl Fn() + Send + 'static) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build();
        if let Ok(rt) = rt {
            rt.block_on(async {
                let _ = tokio::signal::ctrl_c().await;
            });
            on_signal();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture_prefs::{CursorPref, FpsPref};
    use mirrorwire_proto::fec::FecMode;

    #[test]
    fn encoder_rate_follows_compositor() {
        assert_eq!(effective_fps(60, Some(60)), 60);
        assert_eq!(effective_fps(60, Some(30)), 30);
        assert_eq!(effective_fps(240, Some(144)), 144);
        assert_eq!(effective_fps(240, None), 60);
        assert_eq!(effective_fps(30, None), 30);
    }

    #[test]
    fn pen_on_reopens_the_capture_once_with_input() {
        use std::sync::atomic::Ordering;
        let ctl = Mutex::new(CaptureCtl::new(CapturePrefs::default()));
        {
            let mut c = ctl.lock_ok();
            c.asked_cursor = CursorCapture::Metadata;
            c.asked_fps = 60;
        }
        let controls = Controls::default();
        assert!(!ctl.lock_ok().needs_restart());
        // Started without input: switching the pen on reopens, and strokes wait meanwhile.
        request_input(&ctl, &controls);
        assert!(controls.restart_source.swap(false, Ordering::AcqRel));
        {
            let c = ctl.lock_ok();
            assert!(c.want_input && c.remote_asking && c.needs_restart());
            assert!(input_pending(&c));
        }
        // The reopened session carries input: done, strokes pass.
        {
            let mut c = ctl.lock_ok();
            c.asked_input = true;
            c.remote_asking = false;
            assert!(!c.needs_restart());
            assert!(!input_pending(&c));
        }
        // Pen on again (or a later stroke): no second reopen.
        request_input(&ctl, &controls);
        assert!(!controls.restart_source.load(Ordering::Acquire));
        // Started with input already (permission remembered): pen on reopens nothing.
        let ctl = Mutex::new(CaptureCtl::new(CapturePrefs::default()));
        ctl.lock_ok().asked_input = true;
        request_input(&ctl, &controls);
        assert!(!controls.restart_source.load(Ordering::Acquire));
        assert!(!input_pending(&ctl.lock_ok()));
    }

    #[test]
    fn capture_restarts_only_for_changed_settings() {
        let mut c = CaptureCtl::new(CapturePrefs::default());
        c.asked_cursor = CursorCapture::Metadata;
        c.asked_fps = 60;
        assert!(!c.needs_restart());
        c.prefs.mode = mirrorwire_media::source::CaptureMode::Extend;
        assert!(c.needs_restart());
        c.asked_mode = mirrorwire_media::source::CaptureMode::Extend;
        assert!(!c.needs_restart());
        c.prefs.cursor = CursorPref::Embedded;
        assert_eq!(c.wanted_cursor(), CursorCapture::Embedded);
        assert!(c.needs_restart());
        c.asked_cursor = CursorCapture::Embedded;
        assert!(!c.needs_restart());
        c.prefs.fps = FpsPref::Max;
        assert!(c.needs_restart());
    }

    /// End to end in one process: test-pattern presenter (x264) -> QUIC ->
    /// viewer (software decode, RGBA) with 1 % simulated loss, so FEC,
    /// keyframe recovery, live settings and both status paths all run.
    #[cfg(feature = "gui")]
    #[test]
    fn test_source_streams_to_an_in_process_viewer() {
        use crate::viewer::{self, ViewerOptions};
        use std::time::{Duration, Instant};
        let dir = std::env::temp_dir().join(format!("mirrorwire-e2e-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let localhost = Some(IpAddr::V4(Ipv4Addr::LOCALHOST));
        let mut args = PresentArgs::gui_defaults();
        args.source = SourceKind::Test;
        (args.width, args.height) = (640, 360);
        args.encoder = mirrorwire_media::encode::EncoderBackend::X264;
        args.bitrate_kbps = 4000;
        (args.port, args.bind, args.advertise_ip) = (0, localhost, localhost);
        args.no_mdns = true;
        args.name = Some("e2e".into());
        let presenter = spawn(
            runtime.handle().clone(),
            args,
            dir.clone(),
            Identity::generate().unwrap(),
            None,
        );

        let deadline = Instant::now() + Duration::from_secs(60);
        let wait = |what: &str, done: &mut dyn FnMut() -> bool| {
            while !done() {
                let s = presenter.poll();
                assert!(
                    s.phase != PresentPhase::Failed,
                    "presenter failed: {:?}",
                    s.error
                );
                assert!(Instant::now() < deadline, "timed out waiting for {what}");
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        let mut uri = None;
        wait("the presenter URI", &mut || {
            uri = presenter.poll().uri;
            uri.is_some()
        });
        let target = ConnectInfo::parse(&uri.unwrap()).unwrap();
        assert!(target.pairing_code.is_some(), "pairing is on by default");
        let view = viewer::spawn(
            runtime.handle(),
            Arc::new(Identity::generate().unwrap()),
            target,
            ViewerOptions {
                hwdec: false,
                software_decoder: mirrorwire_media::decode::SoftwareDecoder::platform_default(),
                simulate_loss: 0.01,
                simulate_burst: 1.0,
                simulate_bandwidth_kbps: 0,
                rgba: true,
                name: "e2e viewer".into(),
                display: mirrorwire_proto::control::DisplayInfo::from_points(1920.0, 1080.0, 1.0),
            },
            None,
        );
        // Waits for `n` more frames to reach the display queue.
        let count = |n: usize| {
            let mut shown = 0;
            wait("decoded frames", &mut || {
                while view.frames.try_recv().is_ok() {
                    shown += 1;
                }
                assert!(
                    view.status.lock().unwrap().error.is_none(),
                    "{:?}",
                    view.status.lock().unwrap().error
                );
                shown >= n
            })
        };
        count(20);

        let status = presenter.poll();
        assert!(status.viewer_connected);
        assert_eq!(status.phase, PresentPhase::Streaming);
        assert_eq!(
            status.capture.cursor_mode,
            mirrorwire_proto::control::CursorMode::Separate
        );
        assert!(
            status.attempts.iter().any(|a| a.ok),
            "{:?}",
            status.attempts
        );
        // Live changes: fixed FEC (no restart), a lower bitrate (encoder
        // reopens with an IDR), embedded cursor (capture restarts).
        let mut prefs = status.capture.prefs;
        prefs.fec = FecMode::Fixed(30);
        prefs.cursor = CursorPref::Embedded;
        presenter.apply_capture(prefs);
        let mut cfg = status.settings.clone();
        cfg.bitrate_kbps = 3000;
        presenter.apply_settings(cfg);
        count(30);
        let status = presenter.poll();
        assert_eq!(status.capture.fec.mode, FecMode::Fixed(30));
        assert_eq!(status.settings.bitrate_kbps, 3000);
        assert_eq!(
            mirrorwire_net::settings::Settings::load(&dir)
                .fec
                .as_deref(),
            Some("30%")
        );

        let viewer_status = view.status.lock().unwrap().clone();
        assert!(viewer_status.connected && viewer_status.first_frame);
        view.stop();
        let (totals, _, _) = view.done.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(totals.frames >= 50, "{} frames", totals.frames);
        assert_eq!(totals.marker_mismatch, 0);
        assert!(totals.link.parity_expected > 0, "FEC packets were sent");
        // (Whether the 1 % loss hit any counted packet is left to chance.)
        assert!(totals.link.received <= totals.link.expected);
        presenter.stop();
        wait("the presenter to stop", &mut || {
            matches!(presenter.poll().phase, PresentPhase::Stopped)
        });
        std::fs::remove_dir_all(&dir).ok();
    }
}
