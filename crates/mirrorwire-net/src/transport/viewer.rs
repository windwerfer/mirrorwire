//! Viewer side: connect to a presenter, pair, receive video and cursor.

use super::{read_msg, transport_config, write_msg};
use crate::diag::{
    format_candidates, is_logical_reject, is_unreachable, no_answer_message, ConnectStepId,
    ConnectTrace, HANDSHAKE_TIMEOUT,
};
use crate::identity::{short_fingerprint, Identity};
use crate::pairing::ConnectInfo;
use crate::tls;
use anyhow::{anyhow, bail, Context, Result};
use mirrorwire_proto::clock::{now_us, ClockSync};
use mirrorwire_proto::control::{Control, CursorMode, DisplayInfo, QueueDelay, StreamInfo};
use mirrorwire_proto::cursor::{
    is_pointer_datagram, CursorImage, PointerUpdate, ViewerCursor, MAX_IMAGE_STREAM,
};
use mirrorwire_proto::fec::LinkStats;
use mirrorwire_proto::PROTOCOL_VERSION;
use quinn::crypto::rustls::QuicClientConfig;
use quinn::{Connection, Endpoint, RecvStream, SendStream};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, watch};

pub struct ViewerSession {
    pub conn: Connection,
    pub stream_info: StreamInfo,
    /// Newest `stream_info` (the presenter re-sends it when e.g. the cursor
    /// mode changes).
    pub stream_updates: watch::Receiver<StreamInfo>,
    /// Cursor sent as coordinates: latest position and cached images.
    pub cursor: Arc<Mutex<ViewerCursor>>,
    pub clock: Arc<Mutex<ClockSync>>,
    control: mpsc::UnboundedSender<Control>,
    _endpoint: Endpoint,
}

impl ViewerSession {
    pub async fn connect(
        identity: &Identity,
        target: &ConnectInfo,
        viewer_name: &str,
    ) -> Result<Self> {
        Self::connect_traced(
            identity,
            target,
            viewer_name,
            DisplayInfo {
                width: 1920,
                height: 1080,
                scale_milli: 1000,
            },
            &ConnectTrace::new(),
            HANDSHAKE_TIMEOUT,
        )
        .await
    }

    pub async fn connect_traced(
        identity: &Identity,
        target: &ConnectInfo,
        viewer_name: &str,
        display: DisplayInfo,
        trace: &ConnectTrace,
        handshake_timeout: Duration,
    ) -> Result<Self> {
        let addrs = target.candidates();
        trace.start(
            ConnectStepId::Resolve,
            format!("trying {} (UDP)", format_candidates(target)),
        );
        if addrs.is_empty() {
            let msg = "no addresses to try".to_owned();
            trace.fail(ConnectStepId::Resolve, msg.clone());
            bail!("{msg}");
        }
        trace.ok(
            ConnectStepId::Resolve,
            format!("{} address(es): {}", addrs.len(), format_candidates(target)),
        );

        let mut last_err: Option<anyhow::Error> = None;
        for (i, addr) in addrs.iter().enumerate() {
            trace.start(
                ConnectStepId::Handshake,
                format!(
                    "QUIC handshake to {addr} (UDP), try {}/{}",
                    i + 1,
                    addrs.len()
                ),
            );
            match connect_one(
                identity,
                target,
                viewer_name,
                display,
                *addr,
                trace,
                handshake_timeout,
                PROTOCOL_VERSION,
            )
            .await
            {
                Ok(session) => return Ok(session),
                Err(e) => {
                    tracing::warn!("connect to {addr} failed: {e:#}");
                    if is_logical_reject(&e) {
                        return Err(e);
                    }
                    last_err = Some(e);
                }
            }
        }
        let cause = last_err
            .as_ref()
            .map(|e| format!("{e:#}"))
            .unwrap_or_default();
        let msg = if last_err.as_ref().is_some_and(is_unreachable) || last_err.is_some() {
            no_answer_message(&addrs, Some(cause.trim()))
        } else {
            format!(
                "failed to connect to {}: {cause}",
                format_candidates(target)
            )
        };
        trace.fail(ConnectStepId::Handshake, msg.clone());
        trace.set_error(msg.clone(), Some(msg.clone()));
        bail!("{msg}")
    }

    pub fn request_keyframe(&self, reason: &str) {
        let _ = self.control.send(Control::RequestKeyframe {
            reason: reason.to_owned(),
        });
    }

    /// Asks the presenter to send fragments again. A lost or refused NACK costs nothing: the
    /// caller asks again after a round trip, and gives up to a keyframe request in the end.
    pub fn send_nack(&self, nack: &mirrorwire_proto::nack::Nack) {
        let _ = self.conn.send_datagram(bytes::Bytes::from(nack.encode()));
    }

    /// If `datagram` is a pointer update, apply it and return true (the
    /// caller then skips video reassembly). Malformed ones are dropped.
    pub fn handle_pointer_datagram(&self, datagram: &[u8]) -> bool {
        if !is_pointer_datagram(datagram) {
            return false;
        }
        if let Some(p) = PointerUpdate::decode(datagram) {
            self.cursor
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .apply(p);
        }
        true
    }

    /// How the presenter currently sends the cursor.
    pub fn cursor_mode(&self) -> CursorMode {
        self.stream_updates.borrow().cursor
    }

    /// Tell the presenter how the link did over `window` (a difference of
    /// [`LinkStats`] snapshots) and the queueing delay its frames saw.
    pub fn report_link(&self, window: &LinkStats, queue: Option<QueueDelay>) {
        if window.expected == 0 {
            return;
        }
        let loss = window.expected.saturating_sub(window.received);
        let _ = self.control.send(Control::LinkReport {
            loss_permille: (loss * 1000 / window.expected).min(1000) as u32,
            packets: window.expected.min(u32::MAX as u64) as u32,
            recovered: window.recovered.min(u32::MAX as u64) as u32,
            unrecoverable: window.unrecoverable_frames.min(u32::MAX as u64) as u32,
            queue,
            rx_kbps: window.rx_kbps(),
            window_ms: (window.window_us / 1000).min(u32::MAX as u64) as u32,
        });
    }

    /// Tap a presenter-remote button (`remote_key`, protocol 9).
    pub fn send_remote_key(&self, key: mirrorwire_proto::control::RemoteKey) {
        let _ = self.control.send(Control::RemoteKey { key });
    }

    /// Pen/finger as an absolute pointer on the presenter (`pointer`, protocol 11); `x`/`y`
    /// are 0..=65535 inside the picture, `pressure` 0..=65535 or `None` without a sensor.
    pub fn send_pointer(
        &self,
        phase: mirrorwire_proto::control::PointerPhase,
        x: u16,
        y: u16,
        pressure: Option<u16>,
    ) {
        let _ = self.control.send(Control::Pointer {
            phase,
            x,
            y,
            pressure,
        });
    }

    pub fn control_sender(&self) -> mpsc::UnboundedSender<Control> {
        self.control.clone()
    }

    pub fn close(&self) {
        let _ = self.control.send(Control::Bye);
        self.conn.close(0u32.into(), b"viewer closed");
    }
}

/// One connection attempt. `protocol` is what the `hello` claims (always [`PROTOCOL_VERSION`]
/// outside tests, which use another one to check the presenter's rejection).
#[allow(clippy::too_many_arguments)]
pub(super) async fn connect_one(
    identity: &Identity,
    target: &ConnectInfo,
    viewer_name: &str,
    display: DisplayInfo,
    addr: SocketAddr,
    trace: &ConnectTrace,
    handshake_timeout: Duration,
    protocol: u32,
) -> Result<ViewerSession> {
    let tls = tls::client_config(identity, &target.fingerprint)?;
    let mut client = quinn::ClientConfig::new(Arc::new(QuicClientConfig::try_from(tls)?));
    client.transport_config(transport_config());
    let bind: SocketAddr = if addr.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    }
    .parse()?;
    let mut endpoint = viewer_endpoint(bind)?;
    endpoint.set_default_client_config(client);

    let connecting = endpoint
        .connect(addr, crate::identity::CERT_SERVER_NAME)
        .with_context(|| format!("connecting to {addr}"))?;
    let conn = match tokio::time::timeout(handshake_timeout, connecting).await {
        Ok(Ok(c)) => c,
        Ok(Err(e)) => {
            let err = anyhow!(e).context(format!("connecting to {addr}"));
            if is_unreachable(&err) {
                let msg = no_answer_message(&[addr], Some(&format!("{err:#}")));
                trace.fail(ConnectStepId::Handshake, msg.clone());
                bail!("{msg}");
            }
            trace.fail(ConnectStepId::Handshake, format!("{err:#}"));
            return Err(err);
        }
        Err(_) => {
            let msg = no_answer_message(
                &[addr],
                Some(&format!("timed out after {}s", handshake_timeout.as_secs())),
            );
            trace.fail(ConnectStepId::Handshake, msg.clone());
            bail!("{msg}");
        }
    };
    trace.ok(ConnectStepId::Handshake, format!("connected to {addr}"));

    match crate::tls::peer_fingerprint(&conn) {
        Some(fp) if fp.eq_ignore_ascii_case(&target.fingerprint) => {
            trace.ok(
                ConnectStepId::Cert,
                format!("fingerprint matches ({})", short_fingerprint(&fp)),
            );
        }
        Some(fp) => {
            let msg = format!(
                "presenter fingerprint mismatch: expected {}, got {}",
                short_fingerprint(&target.fingerprint),
                short_fingerprint(&fp)
            );
            trace.fail(ConnectStepId::Cert, msg.clone());
            bail!("{msg}");
        }
        None => {
            let msg = "presenter sent no certificate".to_owned();
            trace.fail(ConnectStepId::Cert, msg.clone());
            bail!("{msg}");
        }
    }

    trace.start(ConnectStepId::Control, "opening control stream");
    let (mut send, mut recv) = conn.open_bi().await?;
    trace.ok(ConnectStepId::Control, "control stream open");

    let pairing_note = if target.pairing_code.as_ref().is_some_and(|c| !c.is_empty()) {
        "sending pairing code"
    } else {
        "no pairing code (already paired, or pairing is off)"
    };
    trace.start(ConnectStepId::Pairing, pairing_note);
    write_msg(
        &mut send,
        &Control::Hello {
            protocol,
            name: viewer_name.to_owned(),
            pairing_code: target.pairing_code.clone(),
            display,
        },
    )
    .await?;
    let stream_info = match tokio::time::timeout(handshake_timeout, read_msg(&mut recv)).await {
        Ok(Ok(Control::StreamInfo(info))) => {
            trace.ok(ConnectStepId::Pairing, "accepted");
            info
        }
        Ok(Ok(Control::Rejected { reason })) => {
            let msg = format!("presenter rejected connection: {reason}");
            trace.fail(ConnectStepId::Pairing, reason);
            bail!("{msg}");
        }
        Ok(Ok(other)) => {
            let msg = format!("unexpected reply {other:?}");
            trace.fail(ConnectStepId::Pairing, msg.clone());
            bail!("{msg}");
        }
        Ok(Err(e)) => {
            trace.fail(ConnectStepId::Pairing, format!("{e:#}"));
            return Err(e);
        }
        Err(_) => {
            let msg =
                no_answer_message(&[addr], Some("timed out waiting for pairing / stream info"));
            trace.fail(ConnectStepId::Pairing, msg.clone());
            bail!("{msg}");
        }
    };

    let clock = Arc::new(Mutex::new(ClockSync::default()));
    let (control, control_rx) = mpsc::unbounded_channel();
    let (info_tx, stream_updates) = watch::channel(stream_info.clone());
    tokio::spawn(control_loop(send, recv, control_rx, clock.clone(), info_tx));
    let cursor = Arc::new(Mutex::new(ViewerCursor::default()));
    tokio::spawn(accept_cursor_images(conn.clone(), cursor.clone()));
    Ok(ViewerSession {
        conn,
        stream_info,
        stream_updates,
        cursor,
        clock,
        control,
        _endpoint: endpoint,
    })
}

/// UDP receive buffer the viewer asks for. On USB or Ethernet a keyframe and its parity
/// (up to ~200 KB) arrive within a few milliseconds; Android's default 224 KB overflowed
/// when the receive task paused briefly (tablet over USB tethering: ~2 % of datagrams
/// dropped as `RcvbufErrors`, bursts of 13–18 %, budget pinned at the 3 Mbit/s floor).
/// The kernel caps it at `net.core.rmem_max` (8 MB on Android, often 208 KB on desktop
/// Linux) without needing any privilege.
pub const VIEWER_RECV_BUFFER: usize = 4 << 20;

/// Like `Endpoint::client`, but with a larger UDP receive buffer.
fn viewer_endpoint(bind: SocketAddr) -> Result<Endpoint> {
    let (socket, got) = viewer_socket(bind)?;
    match got {
        // Linux reports twice the usable size (it counts bookkeeping overhead).
        Ok(got) => tracing::info!(
            "UDP receive buffer {} KiB (asked {} KiB)",
            got / 1024,
            VIEWER_RECV_BUFFER / 1024
        ),
        Err(e) => tracing::warn!("could not read the UDP receive buffer size: {e}"),
    }
    let runtime = quinn::default_runtime().ok_or_else(|| anyhow!("no async runtime found"))?;
    Ok(Endpoint::new(
        quinn::EndpointConfig::default(),
        None,
        socket,
        runtime,
    )?)
}

/// The viewer's UDP socket with [`VIEWER_RECV_BUFFER`] requested, and the size the
/// kernel granted. Failing to enlarge the buffer is logged, not fatal.
fn viewer_socket(bind: SocketAddr) -> Result<(std::net::UdpSocket, std::io::Result<usize>)> {
    let socket = std::net::UdpSocket::bind(bind)?;
    let state = quinn::udp::UdpSocketState::new((&socket).into())?;
    if let Err(e) = state.set_recv_buffer_size((&socket).into(), VIEWER_RECV_BUFFER) {
        tracing::warn!("could not enlarge the UDP receive buffer: {e}");
    }
    let got = state.recv_buffer_size((&socket).into());
    Ok((socket, got))
}

/// Cursor images arrive one per unidirectional stream (bounded size).
async fn accept_cursor_images(conn: Connection, cursor: Arc<Mutex<ViewerCursor>>) {
    while let Ok(mut stream) = conn.accept_uni().await {
        let cursor = cursor.clone();
        tokio::spawn(async move {
            match stream.read_to_end(MAX_IMAGE_STREAM).await {
                Ok(bytes) => match CursorImage::decode(&bytes) {
                    Ok(img) => cursor
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .add_image(img),
                    Err(e) => tracing::debug!("dropping cursor image: {e:#}"),
                },
                Err(e) => tracing::debug!("cursor image stream failed: {e}"),
            }
        });
    }
}

async fn control_loop(
    mut send: SendStream,
    mut recv: RecvStream,
    mut outgoing: mpsc::UnboundedReceiver<Control>,
    clock: Arc<Mutex<ClockSync>>,
    stream_info: watch::Sender<StreamInfo>,
) {
    let reader = async {
        loop {
            match read_msg(&mut recv).await {
                Ok(Control::Pong {
                    t_viewer_us,
                    t_presenter_us,
                }) => {
                    clock.lock().unwrap_or_else(|e| e.into_inner()).add_sample(
                        t_viewer_us,
                        t_presenter_us,
                        now_us(),
                    );
                }
                Ok(Control::StreamInfo(info)) => {
                    tracing::info!(
                        "presenter updated stream info: cursor {:?}, {} fps",
                        info.cursor,
                        info.fps
                    );
                    stream_info.send_replace(info);
                }
                Ok(other) => tracing::debug!("ignoring {other:?}"),
                Err(e) => {
                    tracing::debug!("control stream closed: {e:#}");
                    return;
                }
            }
        }
    };
    let writer = async {
        let mut ping = tokio::time::interval(Duration::from_millis(250));
        loop {
            let msg = tokio::select! {
                _ = ping.tick() => Control::Ping { t_viewer_us: now_us() },
                msg = outgoing.recv() => match msg {
                    Some(m) => m,
                    None => return,
                },
            };
            if write_msg(&mut send, &msg).await.is_err() {
                return;
            }
        }
    };
    tokio::select! {
        _ = reader => {}
        _ = writer => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;
    use crate::pairing::ConnectInfo;

    /// The viewer socket gets a bigger receive buffer than a plain socket whenever the
    /// kernel allows one (`rmem_max` above the default).
    #[test]
    fn viewer_socket_enlarges_the_receive_buffer() {
        let bind: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let plain = std::net::UdpSocket::bind(bind).unwrap();
        let state = quinn::udp::UdpSocketState::new((&plain).into()).unwrap();
        let default = state.recv_buffer_size((&plain).into()).unwrap();

        let (_socket, got) = viewer_socket(bind).unwrap();
        let got = got.unwrap();
        let rmem_max: Option<usize> = std::fs::read_to_string("/proc/sys/net/core/rmem_max")
            .ok()
            .and_then(|s| s.trim().parse().ok());
        // Linux reports double the requested size, capped at rmem_max.
        let possible = rmem_max.map_or(VIEWER_RECV_BUFFER, |m| m.min(VIEWER_RECV_BUFFER)) * 2;
        if possible > default {
            assert!(
                got >= possible.min(VIEWER_RECV_BUFFER),
                "receive buffer {got} B, plain socket {default} B, rmem_max {rmem_max:?}"
            );
        } else {
            assert!(got >= default);
        }
    }

    #[tokio::test]
    async fn connect_timeout_reports_firewall_hint() {
        let identity = Identity::generate().unwrap();
        let sock = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr = sock.local_addr().unwrap();
        drop(sock);
        let target = ConnectInfo {
            addr,
            addrs: Vec::new(),
            fingerprint: identity.fingerprint(),
            pairing_code: None,
            name: Some("timeout-test".into()),
        };
        let started = std::time::Instant::now();
        let result = ViewerSession::connect_traced(
            &identity,
            &target,
            "tester",
            DisplayInfo {
                width: 1920,
                height: 1080,
                scale_milli: 1000,
            },
            &ConnectTrace::new(),
            Duration::from_secs(2),
        )
        .await;
        let err = match result {
            Ok(_) => panic!("expected connect to time out"),
            Err(e) => e,
        };
        let msg = format!("{err:#}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "timeout too long: {:?}",
            started.elapsed()
        );
        assert!(msg.contains(&format!("{addr} (UDP)")), "{msg}");
        assert!(msg.contains("firewall-cmd"), "{msg}");
        assert!(msg.contains("QUIC"), "{msg}");
    }
}
