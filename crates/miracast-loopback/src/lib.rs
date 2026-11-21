#![forbid(unsafe_code)]
//! Test support for the Miracast spike: a real-socket sink driver ([`run_sink`]) and a minimal
//! test source ([`TestSource`]) that speaks RTSP M1-M7 and sends MPEG-TS over RTP, all on
//! localhost, no Wi-Fi. As on hardware, the source is the RTSP server (port 7236) and the sink
//! connects to it. This is test code: apart from the connect, which reports a readable
//! reason, it panics on I/O errors instead of reporting them.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use miracast_rtp::diag::{NoRtp, NoRtpWatchdog, RtpStalled, DEFAULT_NO_RTP_AFTER};
use miracast_rtp::ts::synth::{packetize, pes, psi_packets, rtp_packets, PID_AUDIO, PID_VIDEO};
use miracast_rtp::{MediaSink, Receiver, RxStats};
use miracast_rtsp::message::{parse_message, Message, Request, Response};
use miracast_rtsp::params::AudioCodec;
use miracast_rtsp::{Action, Negotiated, SinkConfig, SinkSession, State};

const FEATURE: &str = "org.wfa.wfd1.0";
const PUBLIC: &str = "org.wfa.wfd1.0, GET_PARAMETER, SET_PARAMETER";

/// What the sink side saw, returned by [`run_sink`].
#[derive(Debug, Default)]
pub struct SinkReport {
    pub negotiated: Option<Negotiated>,
    pub close_reason: Option<String>,
    pub idr_requests_sent: u32,
    /// Loss episodes whose IDR request was rate-limited away.
    pub idr_suppressed: u64,
    pub lost_packets: u64,
    pub bad_datagrams: u64,
    /// RTSP reached PLAY but no datagram arrived in time (likely the firewall).
    pub no_rtp: Option<NoRtp>,
    /// Times media stopped for at least the no-RTP timeout after it had flowed.
    pub stalls: u32,
    /// The session was ended because media stopped (see [`RunOptions::close_on_silence`]).
    pub link_lost: bool,
    /// The source's RTSP server never answered within [`RunOptions::connect_within`].
    pub no_rtsp: bool,
    /// Receive counters at the end.
    pub rx: RxStats,
    /// How long media flowed (PLAY to the end).
    pub media_secs: u64,
}

/// The sink's offer: the default video formats plus LPCM audio (48 kHz, 16 bit, stereo:
/// `LPCM 00000002 00`). Audio is offered but never played: the TB336FU (MtkWifiDisplaySource)
/// refuses `wfd_audio_codecs: none` with "Sink and source share no commonly supported audio
/// formats" (hardware run 2026-11-21).
pub fn sink_config(rtp_port: u16) -> SinkConfig {
    SinkConfig {
        rtp_port,
        audio: Some(vec![AudioCodec {
            name: "LPCM".into(),
            modes: 0x2,
            latency: 0,
        }]),
        ..SinkConfig::default()
    }
}

pub mod nm_flow;

/// Pause between connection attempts to the source's RTSP server.
const CONNECT_RETRY: Duration = Duration::from_millis(500);

/// Connects to the source's RTSP server, retrying until `within` has passed: right after the
/// P2P group forms the source may not listen yet (hardware runs needed a retry loop).
pub fn connect_to_source(addr: SocketAddr, within: Duration) -> std::io::Result<TcpStream> {
    let start = Instant::now();
    loop {
        let left = within.saturating_sub(start.elapsed());
        let attempt = TcpStream::connect_timeout(
            &addr,
            left.clamp(Duration::from_millis(1), Duration::from_secs(1)),
        );
        match attempt {
            Ok(s) => return Ok(s),
            Err(e) if start.elapsed() + CONNECT_RETRY >= within => return Err(e),
            Err(_) => std::thread::sleep(CONNECT_RETRY),
        }
    }
}

/// Drives a [`SinkSession`] and a [`Receiver`] over real sockets until the session closes
/// (or `deadline` passes). Connects to the source's RTSP server at `source` (on hardware
/// `192.168.49.1:7236`, the group owner). `udp` must be bound already; its port is announced
/// as the RTP port.
pub fn run_sink(
    source: SocketAddr,
    udp: UdpSocket,
    sink: &mut dyn MediaSink,
    deadline: Duration,
) -> SinkReport {
    run_sink_logged(source, udp, sink, &RunOptions::new(deadline), &mut |_| {})
}

/// What the sink driver reports while it runs. `Display` gives the trace line (the demo prints
/// it); the app reacts to the typed variants instead of parsing text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SinkEvent {
    /// The RTSP connection to the source is up.
    Connected(SocketAddr),
    /// One RTSP message; `outgoing` = sent by the sink.
    Rtsp {
        outgoing: bool,
        text: String,
    },
    Negotiated(Negotiated),
    MediaStarted {
        rtp_port: u16,
    },
    MediaStopped,
    /// RTSP worked but no RTP arrived (likely the firewall).
    NoRtp(NoRtp),
    /// Media flowed and then stopped.
    Stalled(RtpStalled),
    /// Media stayed away for [`RunOptions::close_on_silence`]; the session ends.
    LinkLost {
        silent: Duration,
    },
    Stats(String),
    StopRequested,
    /// The session ends with this reason.
    Closed(String),
}

impl std::fmt::Display for SinkEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connected(a) => write!(f, "RTSP connected to {a}"),
            Self::Rtsp {
                outgoing: true,
                text,
            } => write!(f, ">>> sink\n{text}"),
            Self::Rtsp {
                outgoing: false,
                text,
            } => write!(f, "<<< source\n{text}"),
            Self::Negotiated(n) => write!(f, "negotiated: {n:?}"),
            Self::MediaStarted { rtp_port } => write!(f, "start media on UDP {rtp_port}"),
            Self::MediaStopped => write!(f, "stop media"),
            Self::NoRtp(d) => write!(f, "diagnostic: {d}"),
            Self::Stalled(d) => write!(f, "diagnostic: {d}"),
            Self::LinkLost { silent } => write!(
                f,
                "link lost: no media for {} s, ending the session",
                silent.as_secs()
            ),
            Self::Stats(line) => f.write_str(line),
            Self::StopRequested => write!(f, "stop requested"),
            Self::Closed(reason) => write!(f, "close: {reason}"),
        }
    }
}

/// Default for [`RunOptions::connect_within`]: the TB336FU listens within a second or two of
/// the group forming (runs 3-5).
pub const DEFAULT_CONNECT_WITHIN: Duration = Duration::from_secs(20);

/// Settings of one sink run.
#[derive(Debug, Clone)]
pub struct RunOptions {
    /// Give up (connect and session) after this long.
    pub deadline: Duration,
    /// Give up connecting to the source's RTSP server after this long (hardware run 6: a
    /// group can form without the source ever starting its server).
    pub connect_within: Duration,
    /// Report [`NoRtp`] if no datagram arrives this long after PLAY.
    pub no_rtp_after: Duration,
    /// The Wi-Fi Direct group interface, named in the [`NoRtp`] text.
    pub interface: Option<String>,
    /// End the session (TEARDOWN, `link_lost`) when media that had flowed stops this long;
    /// `None` waits for the RTSP keepalive timeout (about a minute on the TB336FU).
    pub close_on_silence: Option<Duration>,
    /// Log a stats line this often (`None`: never).
    pub stats_every: Option<Duration>,
    /// Set from another thread to end the session with a TEARDOWN (the GUI's Back button).
    pub stop: Option<Arc<AtomicBool>>,
    /// Set by the consumer when it dropped video (e.g. its decoder fell behind): the driver
    /// sends an IDR request (M13, rate-limited) so decoding resumes without waiting for the
    /// source's next periodic keyframe (about 4 s on the TB336FU).
    pub idr_request: Option<Arc<AtomicBool>>,
}

impl SinkReport {
    /// True when the session ended because the link or the source went away (not a teardown
    /// by the user or the source): worth joining the group again.
    pub fn wants_reconnect(&self) -> bool {
        self.link_lost
            || self.no_rtsp
            || self
                .close_reason
                .as_deref()
                .is_some_and(|r| r.contains("keepalive timeout"))
    }
}

impl RunOptions {
    pub fn new(deadline: Duration) -> Self {
        Self {
            deadline,
            connect_within: DEFAULT_CONNECT_WITHIN,
            no_rtp_after: DEFAULT_NO_RTP_AFTER,
            interface: None,
            close_on_silence: None,
            stats_every: None,
            stop: None,
            idr_request: None,
        }
    }
}

/// One stats line for the trace: totals since the session started.
pub fn stats_line(rx: &RxStats, idr_sent: u32, idr_suppressed: u64, elapsed: Duration) -> String {
    let lost_pct = if rx.packets + rx.lost_packets == 0 {
        0.0
    } else {
        rx.lost_packets as f64 * 100.0 / (rx.packets + rx.lost_packets) as f64
    };
    format!(
        "stats: {}s packets {} lost {} ({lost_pct:.2}%) gaps {} reordered {} late {} bad {} \
         idr sent {idr_sent} suppressed {idr_suppressed}",
        elapsed.as_secs(),
        rx.packets,
        rx.lost_packets,
        rx.gaps,
        rx.reordered,
        rx.late,
        rx.bad_datagrams
    )
}

/// Like [`run_sink`], and calls `log` with the RTSP trace (both directions) and state events.
/// If no datagram arrives within `opts.no_rtp_after` of PLAY, the [`NoRtp`] diagnostic is
/// logged and kept in [`SinkReport::no_rtp`]; the session stays up (the user may open the
/// port).
pub fn run_sink_logged(
    source: SocketAddr,
    udp: UdpSocket,
    sink: &mut dyn MediaSink,
    opts: &RunOptions,
    log: &mut dyn FnMut(SinkEvent),
) -> SinkReport {
    let deadline = opts.deadline;
    let mut report = SinkReport::default();
    let fail = |report: &mut SinkReport, log: &mut dyn FnMut(SinkEvent), reason: String| {
        log(SinkEvent::Closed(reason.clone()));
        report.close_reason = Some(reason);
    };
    let rtp_port = match udp.local_addr() {
        Ok(a) => a.port(),
        Err(e) => {
            fail(
                &mut report,
                log,
                format!("the RTP socket has no local port: {e}"),
            );
            return report;
        }
    };
    let mut session = SinkSession::new(sink_config(rtp_port));
    let mut watchdog = NoRtpWatchdog::new(rtp_port, opts.no_rtp_after);
    let mut rx = Receiver::new();
    let start = Instant::now();

    let mut tcp = match connect_to_source(source, opts.connect_within.min(deadline)) {
        Ok(t) => t,
        Err(e) => {
            let reason = format!(
                "the source did not start the stream: no RTSP server at {source} ({e}). On \
                 the source, open Cast and tap this device; answering its connect prompt \
                 forms the Wi-Fi Direct group but starts no stream"
            );
            report.no_rtsp = true;
            fail(&mut report, log, reason);
            return report;
        }
    };
    log(SinkEvent::Connected(source));
    if let Err(e) = tcp
        .set_read_timeout(Some(Duration::from_millis(2)))
        .and_then(|()| udp.set_nonblocking(true))
    {
        fail(&mut report, log, format!("cannot set up the sockets: {e}"));
        return report;
    }
    let mut media = false;
    let mut buf = [0u8; 4096];
    let mut dgram = vec![0u8; 2048];
    let mut want_idr = false;
    let mut media_since: Option<Instant> = None;
    let mut last_stats = Instant::now();
    let mut pending: Vec<Action> = Vec::new();

    'outer: while start.elapsed() < deadline {
        if opts
            .stop
            .as_ref()
            .is_some_and(|s| s.load(Ordering::Acquire))
        {
            log(SinkEvent::StopRequested);
            pending.extend(session.teardown());
        }
        match tcp.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                log(SinkEvent::Rtsp {
                    outgoing: false,
                    text: String::from_utf8_lossy(&buf[..n]).into_owned(),
                });
                pending.extend(session.handle_input(&buf[..n], Instant::now()));
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => break,
        }
        pending.extend(session.poll(Instant::now()));
        // Until PLAY is acknowledged leave datagrams in the socket buffer: a source may start
        // sending right after it receives the M7 reply, before the sink has processed it.
        while let Some(n) = media.then(|| udp.recv(&mut dgram).ok()).flatten() {
            watchdog.on_datagram_at(Instant::now());
            let mut flagged = FlagSink {
                inner: sink,
                need_idr: &mut want_idr,
            };
            rx.push(&dgram[..n], Instant::now(), &mut flagged);
        }
        if media {
            let mut flagged = FlagSink {
                inner: sink,
                need_idr: &mut want_idr,
            };
            rx.poll(Instant::now(), &mut flagged);
        }
        if let Some(d) = watchdog.poll(Instant::now()) {
            let d = d.with_interface(opts.interface.clone(), None);
            log(SinkEvent::NoRtp(d.clone()));
            report.no_rtp = Some(d);
        }
        if let Some(st) = watchdog.poll_stall(Instant::now()) {
            log(SinkEvent::Stalled(st));
            report.stalls += 1;
        }
        if let (Some(every), Some(since)) = (opts.stats_every, media_since) {
            if last_stats.elapsed() >= every {
                last_stats = Instant::now();
                log(SinkEvent::Stats(stats_line(
                    &rx.stats(),
                    report.idr_requests_sent,
                    session.idr_suppressed,
                    since.elapsed(),
                )));
            }
        }
        if let (Some(limit), Some(silent)) =
            (opts.close_on_silence, watchdog.silence(Instant::now()))
        {
            if media && silent >= limit && !report.link_lost {
                report.link_lost = true;
                log(SinkEvent::LinkLost { silent });
                // The source may be gone; the TEARDOWN is best effort.
                pending.extend(session.teardown());
            }
        }
        if opts
            .idr_request
            .as_ref()
            .is_some_and(|f| f.swap(false, Ordering::AcqRel))
        {
            want_idr = true;
        }
        if want_idr {
            want_idr = false;
            let req = session.request_idr(Instant::now());
            if !req.is_empty() {
                report.idr_requests_sent += 1;
            }
            pending.extend(req);
        }
        for a in pending.drain(..) {
            match a {
                Action::Send(b) => {
                    log(SinkEvent::Rtsp {
                        outgoing: true,
                        text: String::from_utf8_lossy(&b).into_owned(),
                    });
                    if let Err(e) = tcp.write_all(&b) {
                        // A lost link must end the session, not the program.
                        let reason = format!("RTSP connection to the source failed: {e}");
                        log(SinkEvent::Closed(reason.clone()));
                        report.close_reason.get_or_insert(reason);
                        break 'outer;
                    }
                }
                Action::Negotiated(n) => {
                    log(SinkEvent::Negotiated(n.clone()));
                    report.negotiated = Some(n);
                }
                Action::StartMedia { rtp_port } => {
                    log(SinkEvent::MediaStarted { rtp_port });
                    media = true;
                    media_since.get_or_insert_with(Instant::now);
                    watchdog.start(Instant::now());
                }
                Action::StopMedia => {
                    log(SinkEvent::MediaStopped);
                    media = false;
                }
                Action::Close { reason } => {
                    log(SinkEvent::Closed(reason.clone()));
                    report.close_reason = Some(reason);
                    break 'outer;
                }
            }
        }
        if session.state() == State::Closed {
            break;
        }
    }
    if session.state() != State::Closed && report.close_reason.is_none() {
        // Deadline reached: end the session properly (hardware run 5 left it open, the source
        // only noticed when the group went away).
        let reason = "deadline reached".to_string();
        log(SinkEvent::Closed(reason.clone()));
        for a in session.teardown() {
            if let Action::Send(b) = a {
                log(SinkEvent::Rtsp {
                    outgoing: true,
                    text: String::from_utf8_lossy(&b).into_owned(),
                });
                let _ = tcp.write_all(&b);
            }
        }
        report.close_reason = Some(reason);
    }
    report.idr_suppressed = session.idr_suppressed;
    report.rx = rx.stats();
    report.media_secs = media_since.map_or(0, |t| t.elapsed().as_secs());
    if opts.stats_every.is_some() && media_since.is_some() {
        log(SinkEvent::Stats(stats_line(
            &report.rx,
            report.idr_requests_sent,
            report.idr_suppressed,
            Duration::from_secs(report.media_secs),
        )));
    }
    report.lost_packets = rx.lost_packets;
    report.bad_datagrams = rx.bad_datagrams;
    report
}

/// Forwards media and remembers `need_keyframe`.
struct FlagSink<'a> {
    inner: &'a mut dyn MediaSink,
    need_idr: &'a mut bool,
}

impl MediaSink for FlagSink<'_> {
    fn video(&mut self, unit: miracast_rtp::VideoAccessUnit) {
        self.inner.video(unit);
    }
    fn audio(&mut self, frame: miracast_rtp::AudioFrame) {
        self.inner.audio(frame);
    }
    fn need_keyframe(&mut self) {
        *self.need_idr = true;
        self.inner.need_keyframe();
    }
}

/// Annex B access unit: SPS, PPS, then an IDR (type 5) or non-IDR (type 1) slice.
pub fn access_unit(idr: bool, len: usize) -> Vec<u8> {
    let mut v = vec![0, 0, 0, 1, 0x67, 1, 2, 0, 0, 0, 1, 0x68, 3];
    v.extend_from_slice(&[0, 0, 0, 1, if idr { 0x65 } else { 0x41 }]);
    v.extend((0..len).map(|i| 0x10 + (i % 7) as u8));
    v
}

/// The listening side of a [`TestSource`] (the source is the RTSP server, like the tablet).
pub struct SourceListener {
    listener: TcpListener,
}

impl SourceListener {
    pub fn addr(&self) -> SocketAddr {
        self.listener.local_addr().unwrap()
    }

    /// Waits for the sink to connect.
    pub fn accept(self) -> TestSource {
        let (tcp, _) = self.listener.accept().unwrap();
        TestSource::from_stream(tcp)
    }
}

/// Minimal Miracast source: RTSP server on TCP that runs M1-M7 (it sends M1/M3/M4/M5 and
/// answers M2/M6/M7) plus an RTP/TS sender.
pub struct TestSource {
    tcp: TcpStream,
    buf: Vec<u8>,
    cseq: u32,
    udp: UdpSocket,
    rtp_target: Option<SocketAddr>,
    seq: u16,
    vcc: u8,
    acc: u8,
    pcc: u8,
    frame: usize,
    session: &'static str,
}

impl TestSource {
    /// Listens on `127.0.0.1` at an ephemeral port.
    pub fn listen() -> SourceListener {
        Self::listen_on(SocketAddr::from(([127, 0, 0, 1], 0)))
    }

    pub fn listen_on(addr: SocketAddr) -> SourceListener {
        SourceListener {
            listener: TcpListener::bind(addr).unwrap(),
        }
    }

    fn from_stream(tcp: TcpStream) -> Self {
        tcp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        Self {
            tcp,
            buf: Vec::new(),
            cseq: 0,
            udp: UdpSocket::bind("127.0.0.1:0").unwrap(),
            rtp_target: None,
            seq: 0,
            vcc: 0,
            acc: 0,
            pcc: 0,
            frame: 0,
            session: "6B8B4567",
        }
    }

    /// Blocks until one full RTSP message arrived.
    pub fn read_message(&mut self) -> Message {
        loop {
            if let Some((m, used)) = parse_message(&self.buf).unwrap() {
                self.buf.drain(..used);
                return m;
            }
            let mut b = [0u8; 2048];
            let n = self.tcp.read(&mut b).expect("RTSP read");
            assert!(n > 0, "sink closed the RTSP connection");
            self.buf.extend_from_slice(&b[..n]);
        }
    }

    fn next_cseq(&mut self) -> u32 {
        self.cseq += 1;
        self.cseq
    }

    fn send_request(&mut self, method: &str, uri: &str, body: &str) -> u32 {
        let cseq = self.next_cseq();
        let r = Request::new(method, uri, cseq)
            .with_header("Require", FEATURE)
            .with_params_body(body.to_string());
        self.tcp.write_all(&r.encode()).unwrap();
        cseq
    }

    fn expect_response(&mut self, cseq: u32) -> Response {
        match self.read_message() {
            Message::Response(r) => {
                assert_eq!(r.headers.cseq(), Some(cseq));
                r
            }
            m => panic!("expected response, got {m:?}"),
        }
    }

    fn expect_request(&mut self, method: &str) -> Request {
        match self.read_message() {
            Message::Request(r) => {
                assert_eq!(r.method, method);
                r
            }
            m => panic!("expected {method} request, got {m:?}"),
        }
    }

    /// Runs M1..M7 as the source; returns the sink's M3 answer body.
    pub fn negotiate(&mut self) -> String {
        let url = "rtsp://127.0.0.1/wfd1.0";
        // M1 source -> sink, then the sink's M2 to us.
        let c = self.send_request("OPTIONS", "*", "");
        let r = self.expect_response(c);
        assert_eq!(r.status, 200);
        assert_eq!(r.headers.get("Public"), Some(PUBLIC));
        let m2 = self.expect_request("OPTIONS");
        let ok = Response::new(200, m2.headers.cseq().unwrap()).with_header("Public", PUBLIC);
        self.tcp.write_all(&ok.encode()).unwrap();
        // M3 get capabilities.
        let c = self.send_request(
            "GET_PARAMETER",
            url,
            "wfd_client_rtp_ports\r\nwfd_audio_codecs\r\nwfd_video_formats\r\nwfd_content_protection\r\n",
        );
        let caps = self.expect_response(c);
        assert_eq!(caps.status, 200);
        // M4 choose 1080p30 CBP, no audio, our RTP port.
        let announced: u16 = caps
            .body
            .lines()
            .find_map(|l| l.strip_prefix("wfd_client_rtp_ports: RTP/AVP/UDP;unicast "))
            .and_then(|l| l.split(' ').next())
            .and_then(|p| p.parse().ok())
            .expect("sink announces an RTP port in M3");
        let body = format!("wfd_video_formats: 00 00 01 04 00000080 00000000 00000000 00 0000 0000 00 none none\r\nwfd_audio_codecs: none\r\nwfd_presentation_URL: rtsp://127.0.0.1/wfd1.0/streamid=0 none\r\nwfd_client_rtp_ports: RTP/AVP/UDP;unicast {announced} 0 mode=play\r\n");
        let c = self.send_request("SET_PARAMETER", url, &body);
        assert_eq!(self.expect_response(c).status, 200);
        // M5 trigger SETUP, then the sink's M6.
        let c = self.send_request("SET_PARAMETER", url, "wfd_trigger_method: SETUP\r\n");
        assert_eq!(self.expect_response(c).status, 200);
        let setup = self.expect_request("SETUP");
        let transport = setup
            .headers
            .get("Transport")
            .expect("Transport")
            .to_string();
        let port: u16 = transport
            .split("client_port=")
            .nth(1)
            .and_then(|s| s.split([';', '-']).next())
            .and_then(|s| s.parse().ok())
            .expect("client_port in Transport");
        self.rtp_target = Some(SocketAddr::from(([127, 0, 0, 1], port)));
        let ok = Response::new(200, setup.headers.cseq().unwrap())
            .with_header("Session", format!("{};timeout=30", self.session))
            .with_header(
                "Transport",
                format!("RTP/AVP/UDP;unicast;client_port={port};server_port=15550"),
            );
        self.tcp.write_all(&ok.encode()).unwrap();
        // M7.
        let play = self.expect_request("PLAY");
        assert_eq!(play.headers.get("Session"), Some(self.session));
        let ok = Response::new(200, play.headers.cseq().unwrap());
        self.tcp.write_all(&ok.encode()).unwrap();
        caps.body
    }

    /// Builds the RTP datagrams for the next video frame (and one audio PES).
    /// Every 5th frame is an IDR. Returns the access unit sent.
    pub fn next_frame(&mut self) -> (Vec<Vec<u8>>, Vec<u8>) {
        let i = self.frame;
        let au = access_unit(i.is_multiple_of(5), 200 + i * 90);
        (self.datagrams_for(&au), au)
    }

    /// RTP datagrams carrying `au` (an Annex B access unit) as the next frame, plus PSI and
    /// one audio PES, like [`next_frame`](Self::next_frame) does for its synthetic units.
    pub fn datagrams_for(&mut self, au: &[u8]) -> Vec<Vec<u8>> {
        let i = self.frame;
        self.frame += 1;
        let pts = 90_000 + i as u64 * 3000;
        let mut tsp = psi_packets(self.pcc);
        self.pcc = (self.pcc + 1) & 0x0F;
        tsp.extend(packetize(
            PID_VIDEO,
            &mut self.vcc,
            &pes(0xE0, pts, au, false),
        ));
        tsp.extend(packetize(
            PID_AUDIO,
            &mut self.acc,
            &pes(0xC0, pts, &[0xFF, 0xF1, i as u8], true),
        ));
        rtp_packets(&mut self.seq, pts as u32, &tsp)
    }

    pub fn send_datagrams(&self, d: &[Vec<u8>]) {
        let to = self.rtp_target.expect("negotiate first");
        for p in d {
            self.udp.send_to(p, to).unwrap();
            // Localhost UDP drops when the receive buffer fills: pace a little.
            std::thread::sleep(Duration::from_micros(200));
        }
    }

    /// Waits for the sink's M13 `wfd_idr_request`, answers it.
    pub fn expect_idr_request(&mut self) {
        let r = self.expect_request("SET_PARAMETER");
        assert!(r.body.contains("wfd_idr_request"), "body: {}", r.body);
        let ok = Response::new(200, r.headers.cseq().unwrap());
        self.tcp.write_all(&ok.encode()).unwrap();
    }

    /// Answers the sink's IDR requests until its TEARDOWN arrives (answered too); returns how
    /// many IDR requests came first.
    pub fn await_teardown(&mut self) -> u32 {
        let mut idr = 0;
        loop {
            let r = match self.read_message() {
                Message::Request(r) => r,
                m => panic!("expected a request, got {m:?}"),
            };
            let ok = Response::new(200, r.headers.cseq().unwrap());
            self.tcp.write_all(&ok.encode()).unwrap();
            match r.method.as_str() {
                "TEARDOWN" => return idr,
                "SET_PARAMETER" if r.body.contains("wfd_idr_request") => idr += 1,
                m => panic!("unexpected {m} before TEARDOWN"),
            }
        }
    }

    /// M8: source-initiated teardown; returns after the sink's reply.
    pub fn teardown(&mut self) {
        let c = self.send_request("TEARDOWN", "rtsp://127.0.0.1/wfd1.0/streamid=0", "");
        let _ = self.expect_response(c);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use miracast_rtsp::params::{audio_accepts, encode_audio_codecs, parse_audio_codecs};

    #[test]
    fn sink_offers_lpcm_48k_stereo() {
        let cfg = sink_config(19000);
        assert_eq!(cfg.rtp_port, 19000);
        assert_eq!(encode_audio_codecs(&cfg.audio), "LPCM 00000002 00");
        // The TB336FU's M4 pick (run 3) must be accepted, and so must "no audio".
        let pick = parse_audio_codecs("LPCM 00000002 00").unwrap();
        assert!(audio_accepts(&cfg.audio, &pick));
        assert!(audio_accepts(&cfg.audio, &None));
    }
}
