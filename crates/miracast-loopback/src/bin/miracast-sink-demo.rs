//! Hardware test sink: connects to one Miracast source's RTSP server (retrying until the
//! source listens), prints the RTSP trace and media stats, and writes the received H.264
//! (Annex B) to a file playable with `ffplay`/`mpv`. Without `--nm`, bring the Wi-Fi
//! Direct link up first (see HANDOFF.md). The source is the group owner, on Android
//! `192.168.49.1`.
//!
//! With `--nm` it does the Wi-Fi Direct part itself through NetworkManager (no root): find a
//! source, join its group with our WFD IEs (the user taps this device in the source's Cast
//! list), connect to the group owner's RTSP port, and leave the group at the end. In `--nm`
//! mode a session whose media stops for 8 s ends as "link lost"; `--reconnect N` then joins
//! again (the source asks the user again). The STA frequency is logged around each session.
//!
//! Usage: `miracast-sink-demo [--nm [--peer NAME] [--find-secs 60] [--attempts 3] [--reconnect 0]]
//!         [--source 192.168.49.1:7236] [--rtp-port 19000] [--out stream.h264] [--secs 600]`
#![forbid(unsafe_code)]
#![allow(clippy::print_stderr)]

use std::fs::File;
use std::io::Write;
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use miracast_loopback::{run_sink_logged, RunOptions};
use miracast_rtp::h264::{find_sps, Sps};
use miracast_rtp::{AudioFrame, MediaSink, VideoAccessUnit};

struct Out {
    file: File,
    start: Instant,
    video: u64,
    audio: u64,
    bytes: u64,
    idr: u64,
    last_report: Instant,
    /// Size the decoder from this, not from M4 (run 3: M4 level 3.2, SPS level 4.0).
    sps: Option<Sps>,
}

impl Out {
    fn maybe_report(&mut self) {
        if self.last_report.elapsed() >= Duration::from_secs(1) {
            self.last_report = Instant::now();
            eprintln!(
                "[{:6.1}s] video AUs {} ({} B), audio frames {}, loss-driven IDR requests {}",
                self.start.elapsed().as_secs_f32(),
                self.video,
                self.bytes,
                self.audio,
                self.idr
            );
        }
    }
}

impl MediaSink for Out {
    fn video(&mut self, u: VideoAccessUnit) {
        self.video += 1;
        self.bytes += u.data.len() as u64;
        if let Some(sps) = u.keyframe.then(|| find_sps(&u.data)).flatten() {
            if self.sps != Some(sps) {
                let (major, minor) = sps.level();
                eprintln!(
                    "[{:6.1}s] stream SPS: {}x{} profile {} level {major}.{minor}",
                    self.start.elapsed().as_secs_f32(),
                    sps.width,
                    sps.height,
                    sps.profile_idc
                );
                self.sps = Some(sps);
            }
        }
        if let Err(e) = self.file.write_all(&u.data) {
            eprintln!("cannot write output: {e}");
        }
        self.maybe_report();
    }
    fn audio(&mut self, _: AudioFrame) {
        self.audio += 1;
    }
    fn need_keyframe(&mut self) {
        self.idr += 1;
    }
}

fn arg(name: &str, default: &str) -> String {
    let a: Vec<String> = std::env::args().collect();
    a.iter()
        .position(|x| x == name)
        .and_then(|i| a.get(i + 1))
        .cloned()
        .unwrap_or_else(|| default.to_string())
}

fn has_flag(name: &str) -> bool {
    std::env::args().any(|a| a == name)
}

fn log_line(start: Instant, l: &str) {
    eprintln!("[{:6.1}s] {l}", start.elapsed().as_secs_f32());
}

#[cfg(target_os = "linux")]
type NmSink = miracast_nm::Sink<miracast_nm::NmP2pProvider<miracast_nm::zbus_backend::ZbusNm>>;

/// Logs the STA frequency (the radio the group shares) for the trace.
#[cfg(target_os = "linux")]
fn log_sta(rt: &tokio::runtime::Runtime, sink: &NmSink, start: Instant, when: &str) {
    match rt.block_on(sink.provider().api().sta_frequencies()) {
        Ok(v) if v.is_empty() => log_line(start, &format!("{when}: no Wi-Fi station connected")),
        Ok(v) => {
            for (iface, mhz) in v {
                log_line(
                    start,
                    &format!(
                        "{when}: STA {iface} on {} (the group's channel is chosen by the \
                         source and not exposed by NetworkManager)",
                        miracast_nm::freq::describe(mhz)
                    ),
                );
            }
        }
        Err(e) => log_line(start, &format!("{when}: STA frequency unknown: {e}")),
    }
}

/// `--nm`: join, stream, leave; again after a lost link, up to `--reconnect` times.
#[cfg(target_os = "linux")]
fn run_nm(out: &mut Out, rtp: &str, secs: u64, start: Instant) {
    use miracast_loopback::nm_flow::{join_source, FlowOptions};
    use miracast_nm::zbus_backend::ZbusNm;
    use miracast_nm::{NmP2pProvider, Sink, SinkConfig};

    let flow = FlowOptions {
        peer_name: has_flag("--peer").then(|| arg("--peer", "")),
        find_timeout: Duration::from_secs(arg("--find-secs", "60").parse().unwrap_or(60)),
        attempts: arg("--attempts", "3").parse().unwrap_or(3),
        ..FlowOptions::default()
    };
    let reconnects: u32 = arg("--reconnect", "0").parse().unwrap_or(0);
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => return log_line(start, &format!("cannot start the async runtime: {e}")),
    };
    let sink = match rt.block_on(ZbusNm::connect_system()) {
        Ok(api) => Sink::new(NmP2pProvider::new(api), SinkConfig::default()),
        Err(e) => return log_line(start, &format!("--nm failed: {e}")),
    };
    let deadline = start + Duration::from_secs(secs);
    for session in 0..=reconnects {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        log_line(start, &format!("session {}: joining", session + 1));
        log_sta(&rt, &sink, start, "before join");
        let joined = rt.block_on(join_source(&sink, &flow, &mut |e| {
            log_line(start, &e.to_string())
        }));
        let j = match joined {
            Ok(j) => j,
            Err(e) => {
                log_line(start, &format!("--nm failed: {e}"));
                break;
            }
        };
        let udp = match UdpSocket::bind(format!("0.0.0.0:{rtp}")) {
            Ok(u) => u,
            Err(e) => {
                log_line(start, &format!("cannot bind UDP {rtp}: {e}"));
                let _ = rt.block_on(sink.stop());
                break;
            }
        };
        let opts = RunOptions {
            interface: Some(j.group.interface.clone()),
            close_on_silence: Some(Duration::from_secs(8)),
            stats_every: Some(Duration::from_secs(5)),
            ..RunOptions::new(deadline.saturating_duration_since(Instant::now()))
        };
        log_line(start, &format!("connecting to the source at {}", j.rtsp));
        let report = run_sink_logged(j.rtsp, udp, out, &opts, &mut |e| {
            log_line(start, &e.to_string())
        });
        log_sta(&rt, &sink, start, "after session");
        log_line(
            start,
            &format!(
                "session {} ended after {} s of media: {:?}; link lost {}",
                session + 1,
                report.media_secs,
                report.close_reason,
                report.link_lost
            ),
        );
        eprintln!("done: {report:?}");
        match rt.block_on(sink.stop()) {
            Ok(()) => log_line(start, "Wi-Fi Direct group left"),
            Err(e) => log_line(
                start,
                &format!("leaving the Wi-Fi Direct group failed: {e}"),
            ),
        }
        if !report.wants_reconnect() {
            break;
        }
    }
}

fn main() {
    let rtp = arg("--rtp-port", "19000");
    let out_path = arg("--out", "stream.h264");
    let secs: u64 = arg("--secs", "600").parse().unwrap_or(600);
    let file = match File::create(&out_path) {
        Ok(f) => f,
        Err(e) => return eprintln!("cannot create {out_path}: {e}"),
    };
    let now = Instant::now();
    let mut out = Out {
        file,
        start: now,
        video: 0,
        audio: 0,
        bytes: 0,
        idr: 0,
        last_report: now,
        sps: None,
    };
    if has_flag("--nm") {
        #[cfg(target_os = "linux")]
        run_nm(&mut out, &rtp, secs, now);
        #[cfg(not(target_os = "linux"))]
        eprintln!("--nm needs NetworkManager (Linux)");
        return;
    }
    let source = arg("--source", "192.168.49.1:7236");
    let source: SocketAddr = match source.parse() {
        Ok(a) => a,
        Err(e) => return eprintln!("--source {source}: not an IP:port ({e})"),
    };
    let udp = match UdpSocket::bind(format!("0.0.0.0:{rtp}")) {
        Ok(u) => u,
        Err(e) => return eprintln!("cannot bind UDP {rtp}: {e}"),
    };
    let opts = RunOptions {
        stats_every: Some(Duration::from_secs(5)),
        ..RunOptions::new(Duration::from_secs(secs))
    };
    eprintln!("connecting to the source at {source}, RTP on UDP {rtp}, writing {out_path}");
    let report = run_sink_logged(source, udp, &mut out, &opts, &mut |e| {
        log_line(now, &e.to_string())
    });
    eprintln!("done: {report:?}");
}
