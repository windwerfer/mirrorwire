//! Loopback end-to-end: test source -> TCP RTSP + UDP RTP/TS -> sink -> access units.
//! No Wi-Fi, no root, localhost only.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::net::{SocketAddr, TcpListener, UdpSocket};
use std::time::{Duration, Instant};

use miracast_loopback::{run_sink, run_sink_logged, RunOptions, SinkReport, TestSource};
use miracast_rtp::{AudioFrame, MediaSink, VideoAccessUnit};

#[derive(Default)]
struct Rec {
    video: Vec<VideoAccessUnit>,
    audio: Vec<AudioFrame>,
    idr_wanted: u32,
}

impl MediaSink for Rec {
    fn video(&mut self, u: VideoAccessUnit) {
        self.video.push(u);
    }
    fn audio(&mut self, f: AudioFrame) {
        self.audio.push(f);
    }
    fn need_keyframe(&mut self) {
        self.idr_wanted += 1;
    }
}

/// Starts the sink thread connecting to the source's RTSP server at `source`.
fn spawn_sink(
    source: SocketAddr,
    deadline: Duration,
) -> std::thread::JoinHandle<(SinkReport, Rec)> {
    let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
    std::thread::spawn(move || {
        let mut rec = Rec::default();
        let report = run_sink(source, udp, &mut rec, deadline);
        (report, rec)
    })
}

/// A listening test source and a sink connected to it.
fn start() -> (TestSource, std::thread::JoinHandle<(SinkReport, Rec)>) {
    let listener = TestSource::listen();
    let sink = spawn_sink(listener.addr(), Duration::from_secs(20));
    (listener.accept(), sink)
}

#[test]
fn negotiates_and_streams_every_access_unit() {
    let (mut src, sink) = start();
    let caps = src.negotiate();
    assert!(caps.contains("wfd_video_formats"), "M3 answer: {caps}");
    assert!(
        caps.contains("wfd_audio_codecs: LPCM 00000002 00"),
        "M3 answer: {caps}"
    );

    let mut sent = Vec::new();
    for _ in 0..20 {
        let (d, au) = src.next_frame();
        src.send_datagrams(&d);
        sent.push(au);
    }
    std::thread::sleep(Duration::from_millis(200));
    src.teardown();

    let (report, rec) = sink.join().unwrap();
    let n = report.negotiated.as_ref().expect("negotiated");
    assert_eq!(n.resolution.unwrap().height, 1080);
    // The last unbounded video PES only flushes on the next one.
    assert_eq!(rec.video.len(), 19, "report: {report:?}");
    for (i, u) in rec.video.iter().enumerate() {
        assert_eq!(u.data, sent[i]);
        assert_eq!(u.keyframe, i.is_multiple_of(5));
    }
    assert_eq!(rec.audio.len(), 20);
    assert_eq!(report.lost_packets, 0);
    assert_eq!(report.bad_datagrams, 0);
    assert_eq!(report.idr_requests_sent, 0);
}

#[test]
fn loss_triggers_idr_request_and_recovery() {
    let (mut src, sink) = start();
    src.negotiate();

    let mut sent = Vec::new();
    for f in 0..16 {
        let (mut d, au) = src.next_frame();
        if f == 6 && d.len() > 1 {
            d.remove(1); // lose one RTP packet in frame 6
        }
        src.send_datagrams(&d);
        sent.push(au);
    }
    // Reorder window needs a few later packets to declare the gap lost; the stream above
    // supplies them. The sink must now ask for an IDR over RTSP (M13).
    src.expect_idr_request();
    std::thread::sleep(Duration::from_millis(100));
    src.teardown();

    let (report, rec) = sink.join().unwrap();
    assert!(report.lost_packets >= 1, "{report:?}");
    assert!(rec.idr_wanted >= 1);
    assert_eq!(
        u64::from(report.idr_requests_sent) + report.idr_suppressed,
        u64::from(rec.idr_wanted)
    );
    // Frames before the loss arrive intact; decoding resumes at the next IDR (frame 10, 15).
    assert_eq!(rec.video[0].data, sent[0]);
    assert!(rec.video.iter().any(|u| u.keyframe && u.data == sent[10]));
    assert!(rec.video.iter().all(|u| u.data != sent[7] || u.keyframe));
}

#[test]
fn bad_datagrams_are_counted_not_fatal() {
    let (mut src, sink) = start();
    src.negotiate();
    src.send_datagrams(&[vec![1, 2, 3], vec![0x80, 33, 0, 0], vec![0xFF; 300]]);
    let (d, _) = src.next_frame();
    src.send_datagrams(&d);
    std::thread::sleep(Duration::from_millis(100));
    src.teardown();
    let (report, _) = sink.join().unwrap();
    assert_eq!(report.bad_datagrams, 3);
}

#[test]
fn sink_retries_until_the_source_listens() {
    // Reserve a free port, release it, and only listen on it after the sink started trying.
    let addr = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let sink = spawn_sink(addr, Duration::from_secs(20));
    std::thread::sleep(Duration::from_millis(700));
    let mut src = TestSource::listen_on(addr).accept();
    src.negotiate();
    src.teardown();
    let (report, _) = sink.join().unwrap();
    assert!(report.negotiated.is_some(), "{report:?}");
}

#[test]
fn unreachable_source_is_a_readable_close_reason() {
    let addr = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let t = Instant::now();
    let (report, _) = spawn_sink(addr, Duration::from_millis(1200))
        .join()
        .unwrap();
    assert!(t.elapsed() < Duration::from_secs(5));
    assert!(report.no_rtsp && report.wants_reconnect(), "{report:?}");
    let reason = report.close_reason.expect("close reason");
    assert!(reason.contains("did not start the stream"), "{reason}");
    assert!(reason.contains(&addr.to_string()), "{reason}");
}

#[test]
fn rtsp_ok_but_no_rtp_gives_the_firewall_diagnostic() {
    let listener = TestSource::listen();
    let addr = listener.addr();
    let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
    let port = udp.local_addr().unwrap().port();
    let sink = std::thread::spawn(move || {
        let mut rec = Rec::default();
        run_sink_logged(
            addr,
            udp,
            &mut rec,
            &RunOptions {
                no_rtp_after: Duration::from_millis(300),
                interface: Some("p2p-wlp58s0-3".into()),
                ..RunOptions::new(Duration::from_secs(20))
            },
            &mut |_| {},
        )
    });
    let mut src = listener.accept();
    src.negotiate();
    // Like a firewall dropping everything: send nothing.
    std::thread::sleep(Duration::from_millis(700));
    src.teardown();
    let report = sink.join().unwrap();
    let d = report.no_rtp.expect("diagnostic");
    assert_eq!(d.rtp_port, port);
    let m = d.to_string();
    assert!(
        m.contains("public") && m.contains(&format!("UDP port {port}")),
        "{m}"
    );
}

#[test]
fn streaming_session_has_no_firewall_diagnostic() {
    let (mut src, sink) = start();
    src.negotiate();
    let (d, _) = src.next_frame();
    src.send_datagrams(&d);
    src.teardown();
    let (report, _) = sink.join().unwrap();
    assert_eq!(report.no_rtp, None);
}

#[test]
fn media_that_stops_is_reported_as_a_stall() {
    let listener = TestSource::listen();
    let addr = listener.addr();
    let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
    let sink = std::thread::spawn(move || {
        let mut rec = Rec::default();
        run_sink_logged(
            addr,
            udp,
            &mut rec,
            &RunOptions {
                no_rtp_after: Duration::from_millis(300),
                ..RunOptions::new(Duration::from_secs(20))
            },
            &mut |_| {},
        )
    });
    let mut src = listener.accept();
    src.negotiate();
    let (d, _) = src.next_frame();
    src.send_datagrams(&d);
    // Like a dropped Wi-Fi Direct link: the stream just stops.
    std::thread::sleep(Duration::from_millis(700));
    src.teardown();
    let report = sink.join().unwrap();
    assert_eq!(report.stalls, 1, "{report:?}");
    assert_eq!(report.no_rtp, None);
}

#[test]
fn silence_after_media_ends_the_session_as_link_lost() {
    let listener = TestSource::listen();
    let addr = listener.addr();
    let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
    let mut lines = Vec::new();
    let sink = std::thread::spawn(move || {
        let mut rec = Rec::default();
        let r = run_sink_logged(
            addr,
            udp,
            &mut rec,
            &RunOptions {
                no_rtp_after: Duration::from_millis(200),
                close_on_silence: Some(Duration::from_millis(400)),
                stats_every: Some(Duration::from_millis(100)),
                ..RunOptions::new(Duration::from_secs(20))
            },
            &mut |l| lines.push(l),
        );
        (r, lines)
    });
    let mut src = listener.accept();
    src.negotiate();
    let (d, _) = src.next_frame();
    src.send_datagrams(&d);
    // The sink gives up on the silent link and tears down by itself.
    let td = src.read_message();
    let (report, lines) = sink.join().unwrap();
    assert!(
        matches!(td, miracast_rtsp::message::Message::Request(ref r) if r.method == "TEARDOWN"),
        "{td:?}"
    );
    assert!(report.link_lost && report.wants_reconnect(), "{report:?}");
    assert!(report.rx.packets >= 1);
    assert!(lines.iter().any(|l| l.starts_with("stats: ")), "{lines:?}");
    assert!(
        lines.iter().any(|l| l.starts_with("link lost")),
        "{lines:?}"
    );
}

#[test]
fn a_teardown_by_the_source_does_not_ask_for_a_reconnect() {
    let (mut src, sink) = start();
    src.negotiate();
    src.teardown();
    let (report, _) = sink.join().unwrap();
    assert!(!report.wants_reconnect(), "{report:?}");
}

#[test]
fn deadline_ends_the_session_with_a_teardown() {
    let listener = TestSource::listen();
    let addr = listener.addr();
    let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
    let sink = std::thread::spawn(move || {
        let mut rec = Rec::default();
        run_sink(addr, udp, &mut rec, Duration::from_millis(800))
    });
    let mut src = listener.accept();
    src.negotiate();
    let td = src.read_message();
    let report = sink.join().unwrap();
    assert!(
        matches!(td, miracast_rtsp::message::Message::Request(ref r) if r.method == "TEARDOWN"),
        "{td:?}"
    );
    assert_eq!(report.close_reason.as_deref(), Some("deadline reached"));
    assert!(!report.wants_reconnect());
}

#[test]
fn stop_flag_ends_the_session_with_a_teardown() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    let listener = TestSource::listen();
    let addr = listener.addr();
    let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let sink = std::thread::spawn(move || {
        let mut rec = Rec::default();
        run_sink_logged(
            addr,
            udp,
            &mut rec,
            &RunOptions {
                stop: Some(flag),
                ..RunOptions::new(Duration::from_secs(20))
            },
            &mut |_| {},
        )
    });
    let mut src = listener.accept();
    src.negotiate();
    stop.store(true, Ordering::Release);
    let td = src.read_message();
    let report = sink.join().unwrap();
    assert!(
        matches!(td, miracast_rtsp::message::Message::Request(ref r) if r.method == "TEARDOWN"),
        "{td:?}"
    );
    assert_eq!(report.close_reason.as_deref(), Some("stopped by the user"));
}

#[test]
fn rtsp_connect_is_bounded_even_with_a_long_session_deadline() {
    let addr = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
    let t = Instant::now();
    let report = run_sink_logged(
        addr,
        udp,
        &mut Rec::default(),
        &RunOptions {
            connect_within: Duration::from_millis(700),
            ..RunOptions::new(Duration::from_secs(3600))
        },
        &mut |_| {},
    );
    assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
    assert!(report.no_rtsp, "{report:?}");
    assert!(report.close_reason.unwrap().contains("tap this device"));
}

#[test]
fn a_consumer_idr_request_reaches_the_source() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    let listener = TestSource::listen();
    let addr = listener.addr();
    let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
    let flag = Arc::new(AtomicBool::new(false));
    let req = flag.clone();
    let sink = std::thread::spawn(move || {
        run_sink_logged(
            addr,
            udp,
            &mut Rec::default(),
            &RunOptions {
                idr_request: Some(req),
                ..RunOptions::new(Duration::from_secs(20))
            },
            &mut |_| {},
        )
    });
    let mut src = listener.accept();
    src.negotiate();
    flag.store(true, Ordering::Release);
    src.expect_idr_request();
    src.teardown();
    let report = sink.join().unwrap();
    assert_eq!(report.idr_requests_sent, 1);
    assert!(!flag.load(Ordering::Acquire), "consumed");
}
