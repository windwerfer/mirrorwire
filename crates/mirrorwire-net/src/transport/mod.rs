//! QUIC transport. One reliable bidirectional stream carries control messages
//! (hello, pairing, keyframe requests, clock pings); video goes out as
//! unreliable datagrams so a lost packet never stalls later frames.

mod link;
mod presenter;
mod viewer;

pub use link::{rate_summary, LinkControl, LinkState};
pub use presenter::{
    should_drop_for_backlog, KeyframeGate, OutgoingFrame, Presenter, PresenterConfig,
};
pub use viewer::ViewerSession;

use anyhow::Result;
use mirrorwire_proto::control::Control;
use mirrorwire_proto::PROTOCOL_VERSION;
use quinn::{RecvStream, SendStream, TransportConfig};
use std::sync::Arc;
use std::time::Duration;

/// Presenter datagram queue. At Wi-Fi rates 2 MiB is ~100–150 ms of video.
pub const DATAGRAM_SEND_BUFFER: usize = 2 << 20;

fn transport_config() -> Arc<TransportConfig> {
    let mut t = TransportConfig::default();
    t.datagram_receive_buffer_size(Some(32 << 20));
    // Bounded so frames can't pile up inside QUIC: the send loop drops a
    // delta frame (and asks for a keyframe) instead of queueing behind this.
    t.datagram_send_buffer_size(DATAGRAM_SEND_BUFFER);
    t.send_window(8 * 1024 * 1024);
    t.receive_window(quinn::VarInt::from_u32(16 * 1024 * 1024));
    t.stream_receive_window(quinn::VarInt::from_u32(4 * 1024 * 1024));
    // Default initial RTT is 333 ms, which paces a small window down to
    // tens of kilobits. A LAN RTT is a couple of milliseconds.
    t.initial_rtt(Duration::from_millis(2));
    t.keep_alive_interval(Some(Duration::from_secs(1)));
    t.max_idle_timeout(Some(quinn::IdleTimeout::from(quinn::VarInt::from_u32(
        10_000,
    ))));
    t.congestion_controller_factory(crate::lan_cc::factory());
    Arc::new(t)
}

async fn write_msg(send: &mut SendStream, msg: &Control) -> Result<()> {
    send.write_all(&msg.encode()).await?;
    Ok(())
}

async fn read_body(recv: &mut RecvStream) -> Result<Vec<u8>> {
    let mut len = [0u8; 4];
    recv.read_exact(&mut len).await?;
    let len = Control::check_len(u32::from_be_bytes(len))?;
    let mut body = vec![0u8; len];
    recv.read_exact(&mut body).await?;
    Ok(body)
}

async fn read_msg(recv: &mut RecvStream) -> Result<Control> {
    Control::decode_body(&read_body(recv).await?)
}

/// What a viewer of `protocol` is told (and the presenter window lists).
fn version_mismatch(protocol: u32) -> String {
    let side = if protocol < PROTOCOL_VERSION {
        "viewer"
    } else {
        "presenter"
    };
    format!(
        "version mismatch: the presenter speaks protocol {PROTOCOL_VERSION}, the viewer {protocol}; update the {side} to the same MirrorWire release"
    )
}

pub fn ms(later_us: u64, earlier_us: u64) -> f64 {
    (later_us as i64).saturating_sub(earlier_us as i64) as f64 / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::{no_answer_message, ConnectTrace};
    use crate::identity::Identity;
    use crate::pairing::ConnectInfo;
    use bytes::Bytes;
    use mirrorwire_proto::control::{CursorMode, DisplayInfo, QueueDelay, StreamInfo};
    use mirrorwire_proto::cursor::{CursorEvent, CursorImage, PointerUpdate};
    use mirrorwire_proto::fec::{FecMode, LinkStats};
    use std::net::SocketAddr;
    use std::net::{IpAddr, Ipv4Addr};
    use std::time::Instant;
    use tokio::sync::mpsc;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cursor_video_and_link_reports_reach_the_other_side() {
        use mirrorwire_proto::cursor::PointerTool;
        let dir = std::env::temp_dir().join(format!("mirrorwire-cursor-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let presenter_id = Identity::generate().unwrap();
        let (cursor_tx, cursor_rx) = mpsc::channel(16);
        let presenter = Presenter::start(
            &presenter_id,
            PresenterConfig {
                bind: "127.0.0.1:0".parse().unwrap(),
                stream_info: StreamInfo {
                    width: 64,
                    height: 64,
                    fps: 60,
                    codec: "h264".into(),
                    encoder: "test".into(),
                    bitrate_kbps: 1,
                    intra_refresh: false,
                    cursor: CursorMode::Separate,
                    fec: String::new(),
                },
                pairing_code: None,
                config_dir: dir.clone(),
                attempts: Default::default(),
                cursor_events: Some(cursor_rx),
                remote_keys: None,
                fec: LinkControl::new(FecMode::Auto),
            },
            Arc::new(|| {}),
        )
        .unwrap();
        // Sent before anyone connects: cached and replayed on connect.
        let image = CursorImage {
            id: 5,
            width: 2,
            height: 2,
            hotspot_x: 1,
            hotspot_y: 1,
            rgba: vec![255; 16],
        };
        cursor_tx
            .send(CursorEvent::Image(image.clone()))
            .await
            .unwrap();
        let pointer = PointerUpdate {
            seq: 0,
            tool: PointerTool::Cursor,
            visible: true,
            x: 10,
            y: 20,
            frame_w: 64,
            frame_h: 64,
            image_id: 5,
            capture_us: 1,
        };
        cursor_tx.send(CursorEvent::Pointer(pointer)).await.unwrap();

        let target = ConnectInfo {
            addr: presenter.local_addr,
            addrs: Vec::new(),
            fingerprint: presenter_id.fingerprint(),
            pairing_code: None,
            name: None,
        };
        let viewer = ViewerSession::connect(&Identity::generate().unwrap(), &target, "test")
            .await
            .unwrap();
        assert_eq!(viewer.cursor_mode(), CursorMode::Separate);
        let deadline = Instant::now() + Duration::from_secs(5);
        while viewer.cursor.lock().unwrap().current().is_none() {
            assert!(Instant::now() < deadline, "cursor never arrived");
            if let Ok(Ok(d)) =
                tokio::time::timeout(Duration::from_millis(50), viewer.conn.read_datagram()).await
            {
                assert!(viewer.handle_pointer_datagram(&d));
            }
        }
        let (p, img) = viewer.cursor.lock().unwrap().current().unwrap();
        assert_eq!((p.x, p.y, p.image_id), (10, 20, 5));
        assert_eq!(*img, image);

        // Video goes out as FEC packets, and the viewer's link report
        // reaches the presenter's FEC state.
        assert_eq!(viewer.stream_info.fec, "auto");
        let payload = Bytes::from(vec![7u8; 20_000]);
        presenter
            .frames
            .send(OutgoingFrame {
                frame_id: 1,
                keyframe: true,
                data: payload.clone(),
                capture_us: 1,
                encoded_us: 2,
            })
            .await
            .unwrap();
        let mut reassembler = mirrorwire_proto::fragment::Reassembler::new(u64::MAX);
        let mut first_dropped = false;
        let frame = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let d = viewer.conn.read_datagram().await.unwrap();
                if viewer.handle_pointer_datagram(&d) {
                    continue;
                }
                assert_eq!(d[0], mirrorwire_proto::fragment::WIRE_VERSION);
                if !first_dropped {
                    first_dropped = true; // lose one packet: parity must cover it
                    continue;
                }
                if let Some((f, ev)) = reassembler.push(d, 0) {
                    return (f, ev);
                }
            }
        })
        .await
        .expect("FEC frame");
        assert_eq!(frame.0.data, payload);
        assert_eq!(frame.1.recovered, 1);
        viewer.report_link(
            &LinkStats {
                expected: 1000,
                received: 950,
                recovered: 40,
                ..Default::default()
            },
            None,
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while presenter.fec.snapshot().reports == 0 {
            assert!(Instant::now() < deadline, "link report never arrived");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let fec = presenter.fec.snapshot();
        assert!((fec.loss_pct - 5.0).abs() < 0.01, "{}", fec.loss_pct);
        assert_eq!(fec.overheads(), Some((0.25, 0.5)));
        // Lost frames on a link that is not congested: more parity.
        // With the viewer's queueing delay (a small, falling queue).
        let delay = QueueDelay {
            mean_us: 5_000,
            max_us: 12_000,
            trend_us_per_s: -1_500,
        };
        viewer.report_link(
            &LinkStats {
                expected: 1000,
                received: 990,
                unrecoverable_frames: 1,
                ..Default::default()
            },
            Some(delay),
        );
        while presenter.fec.snapshot().reports < 2 {
            assert!(
                Instant::now() < deadline,
                "second link report never arrived"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let fec = presenter.fec.snapshot();
        assert_eq!((fec.boost_pct, fec.unrecoverable), (5.0, 1));
        assert_eq!(fec.queue, Some(delay));
        assert!(
            rate_summary(&fec).ends_with("viewer queue 5 ms (max 12, -2 ms/s)"),
            "{}",
            rate_summary(&fec)
        );
        assert!(!fec.rate.congested());
        // Sustained heavy loss is congestion: the budget is cut and parity
        // is not raised further (it would add load).
        for n in 3..=4 {
            viewer.report_link(
                &LinkStats {
                    expected: 1000,
                    received: 800,
                    unrecoverable_frames: 2,
                    ..Default::default()
                },
                None,
            );
            while presenter.fec.snapshot().reports < n {
                assert!(Instant::now() < deadline, "link report {n} never arrived");
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
        let fec = presenter.fec.snapshot();
        assert!(fec.rate.congested());
        assert!(
            fec.boost_pct <= 5.0,
            "boost grew under congestion: {}",
            fec.boost_pct
        );
        assert!(fec.budget_kbps() < 40_000, "{}", fec.budget_kbps());
        let (delta, _) = fec.overheads().unwrap();
        assert!(delta <= 0.35);
        assert!(
            fec.encoder_kbps() < fec.budget_kbps(),
            "parity is paid from the budget"
        );
        assert!(
            rate_summary(&fec).contains("(adaptive)"),
            "{}",
            rate_summary(&fec)
        );
        let line = fec.bitrate_line();
        assert!(
            line.starts_with("Bitrate: target ") && line.contains("of max 40.0 Mbit/s (adaptive)"),
            "{line}"
        );
        assert!(
            line.contains("FEC parity") && line.contains("congested"),
            "{line}"
        );
        assert!(
            line.contains("last cut: heavy loss 20.0 % (background 0.0 %)"),
            "{line}"
        );
        presenter.fec.set_adaptive(false);
        assert_eq!(presenter.fec.snapshot().budget_kbps(), 40_000);
        assert!(presenter
            .fec
            .snapshot()
            .bitrate_line()
            .contains("target 40.0 of max 40.0 Mbit/s (fixed)"));
        presenter.fec.set_ceiling(8_000);
        assert_eq!(presenter.fec.snapshot().rate.ceiling_kbps(), 8_000);
        assert!(presenter.fec.encoder_kbps() < 8_000);
        presenter.set_fec_mode(FecMode::Off);
        assert_eq!(presenter.fec.snapshot().overheads(), None);

        // A changed cursor mode reaches the connected viewer.
        presenter.update_stream_info(|i| i.cursor = CursorMode::Embedded);
        let mut updates = viewer.stream_updates.clone();
        tokio::time::timeout(Duration::from_secs(5), async {
            while updates.borrow_and_update().cursor != CursorMode::Embedded
                || updates.borrow().fec != "off"
            {
                updates.changed().await.unwrap();
            }
        })
        .await
        .expect("stream_info update");
        viewer.close();
        presenter.close();
        std::fs::remove_dir_all(&dir).ok();
    }

    fn test_presenter(dir: &std::path::Path, fec: FecMode) -> (Presenter, Identity) {
        test_presenter_with(dir, fec, None)
    }

    fn test_presenter_with(
        dir: &std::path::Path,
        fec: FecMode,
        remote_keys: Option<mpsc::Sender<mirrorwire_proto::control::RemoteInput>>,
    ) -> (Presenter, Identity) {
        std::fs::create_dir_all(dir).unwrap();
        let id = Identity::generate().unwrap();
        let p = Presenter::start(
            &id,
            PresenterConfig {
                bind: "127.0.0.1:0".parse().unwrap(),
                stream_info: StreamInfo {
                    width: 64,
                    height: 64,
                    fps: 60,
                    codec: "h264".into(),
                    encoder: "test".into(),
                    bitrate_kbps: 1,
                    intra_refresh: false,
                    cursor: CursorMode::Embedded,
                    fec: String::new(),
                },
                pairing_code: None,
                config_dir: dir.to_path_buf(),
                attempts: Default::default(),
                cursor_events: None,
                remote_keys,
                fec: LinkControl::new(fec),
            },
            Arc::new(|| {}),
        )
        .unwrap();
        (p, id)
    }

    fn target_of(p: &Presenter, id: &Identity) -> ConnectInfo {
        ConnectInfo {
            addr: p.local_addr,
            addrs: Vec::new(),
            fingerprint: id.fingerprint(),
            pairing_code: None,
            name: None,
        }
    }

    fn test_display() -> DisplayInfo {
        DisplayInfo {
            width: 2560,
            height: 1440,
            scale_milli: 2000,
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pointer_events_arrive_in_order() {
        use mirrorwire_proto::control::{PointerPhase, RemoteInput};
        let dir = std::env::temp_dir().join(format!("mirrorwire-ptr-{}", std::process::id()));
        let (tx, mut rx) = mpsc::channel(64);
        let (presenter, id) = test_presenter_with(&dir, FecMode::Off, Some(tx));
        let viewer = ViewerSession::connect_traced(
            &Identity::generate().unwrap(),
            &target_of(&presenter, &id),
            "t",
            test_display(),
            &ConnectTrace::new(),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        viewer.send_pointer(PointerPhase::Down, 10, 20, Some(1000));
        viewer.send_pointer(PointerPhase::Move, 30, 40, Some(65535));
        viewer.send_pointer(PointerPhase::Up, 30, 40, None);
        for want in [
            RemoteInput::Pointer {
                phase: PointerPhase::Down,
                x: 10,
                y: 20,
                pressure: Some(1000),
            },
            RemoteInput::Pointer {
                phase: PointerPhase::Move,
                x: 30,
                y: 40,
                pressure: Some(65535),
            },
            RemoteInput::Pointer {
                phase: PointerPhase::Up,
                x: 30,
                y: 40,
                pressure: None,
            },
        ] {
            let got = tokio::time::timeout(Duration::from_secs(5), rx.recv())
                .await
                .unwrap();
            assert_eq!(got, Some(want));
        }
    }

    /// Owner report t316u: pressing Pen on the tablet stopped the video until the presenter
    /// was restarted. Input that nobody drains (a portal dialog waiting, a hung injector, a
    /// blocking device) must never stop the control reader, because that reader is what tells
    /// the send loop the viewer is still there: a silent viewer pauses the video.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn undrained_pen_input_does_not_stop_the_video() {
        use mirrorwire_proto::control::PointerPhase;
        let dir = std::env::temp_dir().join(format!("mirrorwire-pen-stall-{}", std::process::id()));
        // Room for one input, and nobody ever reads it.
        let (tx, _never_drained) = mpsc::channel(1);
        let (presenter, id) = test_presenter_with(&dir, FecMode::Off, Some(tx));
        let viewer = ViewerSession::connect_traced(
            &Identity::generate().unwrap(),
            &target_of(&presenter, &id),
            "t",
            test_display(),
            &ConnectTrace::new(),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        for _ in 0..5 {
            viewer.send_pointer(PointerPhase::Down, 10, 20, None);
            viewer.send_pointer(PointerPhase::Up, 10, 20, None);
        }
        // Longer than the viewer-silent threshold: the viewer keeps pinging meanwhile.
        tokio::time::sleep(link::VIEWER_SILENT + Duration::from_millis(700)).await;
        assert_eq!(
            presenter.fec.snapshot().viewer_silent_for(),
            None,
            "the control reader stopped reading behind the input channel"
        );
        let got_video = tokio::time::timeout(Duration::from_secs(3), async {
            let mut frame_id = 1;
            loop {
                presenter
                    .frames
                    .send(OutgoingFrame {
                        frame_id,
                        keyframe: true,
                        data: Bytes::from(vec![7u8; 1_000]),
                        capture_us: 1,
                        encoded_us: 2,
                    })
                    .await
                    .unwrap();
                frame_id += 1;
                if let Ok(Ok(d)) =
                    tokio::time::timeout(Duration::from_millis(50), viewer.conn.read_datagram())
                        .await
                {
                    if d.first() == Some(&mirrorwire_proto::fragment::WIRE_VERSION) {
                        return;
                    }
                }
            }
        })
        .await;
        assert!(got_video.is_ok(), "video stopped after undrained pen input");
        viewer.close();
        presenter.close();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pen_mode_reaches_the_presenter_and_is_rate_limited() {
        use mirrorwire_proto::control::RemoteInput;
        let dir = std::env::temp_dir().join(format!("mirrorwire-penmode-{}", std::process::id()));
        let (tx, mut rx) = mpsc::channel(64);
        let (presenter, id) = test_presenter_with(&dir, FecMode::Off, Some(tx));
        let viewer = ViewerSession::connect_traced(
            &Identity::generate().unwrap(),
            &target_of(&presenter, &id),
            "t",
            test_display(),
            &ConnectTrace::new(),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        viewer.send_pen_mode(true);
        viewer.send_pen_mode(false);
        for want in [RemoteInput::PenMode(true), RemoteInput::PenMode(false)] {
            let got = tokio::time::timeout(Duration::from_secs(5), rx.recv())
                .await
                .unwrap();
            assert_eq!(got, Some(want));
        }
        // A toggle flood is cut to the per-second limit.
        for i in 0..40 {
            viewer.send_pen_mode(i % 2 == 0);
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        let mut n = 0;
        while rx.try_recv().is_ok() {
            n += 1;
        }
        assert!(n <= 10, "{n} pen toggles got through");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn video_pause_follows_the_viewer_and_resets_when_it_leaves() {
        let dir = std::env::temp_dir().join(format!("mirrorwire-pause-{}", std::process::id()));
        let (presenter, id) = test_presenter_with(&dir, FecMode::Off, None);
        let mut paused = presenter.video_paused.clone();
        let viewer = ViewerSession::connect_traced(
            &Identity::generate().unwrap(),
            &target_of(&presenter, &id),
            "t",
            test_display(),
            &ConnectTrace::new(),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        assert!(!*paused.borrow());
        let wait_for = |rx: &mut tokio::sync::watch::Receiver<bool>, want: bool| {
            let mut rx = rx.clone();
            async move {
                tokio::time::timeout(Duration::from_secs(5), rx.wait_for(|v| *v == want))
                    .await
                    .expect("pause state did not arrive")
                    .unwrap();
            }
        };
        viewer.send_video_paused(true);
        wait_for(&mut paused, true).await;
        viewer.send_video_paused(false);
        wait_for(&mut paused, false).await;
        viewer.send_video_paused(true);
        wait_for(&mut paused, true).await;
        // The viewer leaves while paused: the next one must get video.
        viewer.close();
        wait_for(&mut paused, false).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn remote_taps_reach_the_presenter_and_are_rate_limited() {
        use mirrorwire_proto::control::{RemoteInput, RemoteKey};
        let dir = std::env::temp_dir().join(format!("mirrorwire-remote-{}", std::process::id()));
        let (tx, mut rx) = mpsc::channel(64);
        let (presenter, id) = test_presenter_with(&dir, FecMode::Off, Some(tx));
        let viewer = ViewerSession::connect_traced(
            &Identity::generate().unwrap(),
            &target_of(&presenter, &id),
            "t",
            test_display(),
            &ConnectTrace::new(),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        for _ in 0..40 {
            viewer.send_remote_key(RemoteKey::Next);
        }
        let first = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap();
        assert_eq!(first, Some(RemoteInput::Key(RemoteKey::Next)));
        // Let the rest arrive, then count: a flood is cut to the per-second limit.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let mut n = 1;
        while rx.try_recv().is_ok() {
            n += 1;
        }
        assert!((1..=10).contains(&n), "{n} taps got through");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_presenter_learns_the_viewer_display_from_hello() {
        let dir = std::env::temp_dir().join(format!("mirrorwire-disp-{}", std::process::id()));
        let (presenter, id) = test_presenter(&dir, FecMode::Off);
        assert_eq!(*presenter.viewer_display.borrow(), None);
        let _viewer = ViewerSession::connect_traced(
            &Identity::generate().unwrap(),
            &target_of(&presenter, &id),
            "t",
            test_display(),
            &ConnectTrace::new(),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        assert_eq!(*presenter.viewer_display.borrow(), Some(test_display()));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_viewer_of_another_protocol_gets_a_readable_rejection() {
        let dir = std::env::temp_dir().join(format!("mirrorwire-proto-{}", std::process::id()));
        let (presenter, id) = test_presenter(&dir, FecMode::Off);
        for other in [PROTOCOL_VERSION - 1, PROTOCOL_VERSION + 1] {
            let err = viewer::connect_one(
                &Identity::generate().unwrap(),
                &target_of(&presenter, &id),
                "old",
                test_display(),
                presenter.local_addr,
                &ConnectTrace::new(),
                Duration::from_secs(5),
                other,
            )
            .await
            .err()
            .expect("rejected")
            .to_string();
            assert!(err.contains("version mismatch"), "{err}");
            assert!(err.contains("update the"), "{err}");
        }
        presenter.close();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn nack_makes_the_presenter_resend_a_lost_fragment() {
        let dir = std::env::temp_dir().join(format!("mirrorwire-nack-{}", std::process::id()));
        let (presenter, id) = test_presenter(&dir, FecMode::Off);
        let viewer = ViewerSession::connect(
            &Identity::generate().unwrap(),
            &target_of(&presenter, &id),
            "t",
        )
        .await
        .unwrap();
        let payload = Bytes::from((0..30_000u32).map(|i| (i % 251) as u8).collect::<Vec<u8>>());
        presenter
            .frames
            .send(OutgoingFrame {
                frame_id: 1,
                keyframe: true,
                data: payload.clone(),
                capture_us: 1,
                encoded_us: 2,
            })
            .await
            .unwrap();
        let mut r = mirrorwire_proto::fragment::Reassembler::new(u64::MAX);
        let (mut seen, mut nacked) = (0, false);
        let started = Instant::now();
        let frame = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let d = match tokio::time::timeout(
                    Duration::from_millis(5),
                    viewer.conn.read_datagram(),
                )
                .await
                {
                    Ok(d) => Some(d.unwrap()),
                    Err(_) => None,
                };
                let now = started.elapsed().as_micros() as u64;
                if let Some(d) = d {
                    seen += 1;
                    if seen == 3 {
                        continue; // lost on the wire; no parity (FEC off)
                    }
                    if let Some((f, _)) = r.push(d, now) {
                        return f;
                    }
                }
                if let Some((f, _)) = r.poll(now) {
                    return f;
                }
                if let Some(n) = r.take_nacks(now) {
                    nacked = true;
                    viewer.send_nack(&n);
                }
            }
        })
        .await
        .expect("frame completed by a resend");
        assert!(nacked);
        assert_eq!(frame.data, payload);
        let s = r.nack_stats();
        assert_eq!(
            (s.resent_received >= 1, s.resent_useful),
            (true, 1),
            "{s:?}"
        );
        viewer.close();
        presenter.close();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn version_mismatch_says_which_side_to_update() {
        let old_viewer = version_mismatch(PROTOCOL_VERSION - 1);
        assert!(old_viewer.contains("update the viewer"), "{old_viewer}");
        assert!(
            old_viewer.contains(&format!("presenter speaks protocol {PROTOCOL_VERSION}")),
            "{old_viewer}"
        );
        assert!(version_mismatch(PROTOCOL_VERSION + 1).contains("update the presenter"));
    }

    #[test]
    fn no_answer_message_is_stable() {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 201, 132)), 47_800);
        let msg = no_answer_message(&[addr], None);
        assert!(msg.contains("192.168.201.132:47800 (UDP)"));
        assert!(msg.contains("sudo firewall-cmd --add-port=47800/udp"));
    }
}
