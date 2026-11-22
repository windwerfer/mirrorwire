//! "RTSP worked but no RTP arrived" diagnostic (AGENTS.md 1.8: the GUI must show it).
//!
//! Hardware run 1: M1-M7 completed, the tablet streamed to UDP 19000, but firewalld dropped
//! every datagram because NetworkManager put the Wi-Fi Direct interface in zone `public`.
//! Without this message the user only sees a black window. Sans-IO: the caller reports
//! datagrams and polls with the current time.

use std::fmt;
use std::time::{Duration, Instant};

/// Default wait after PLAY before reporting [`NoRtp`].
pub const DEFAULT_NO_RTP_AFTER: Duration = Duration::from_secs(5);

/// The zone firewalld uses for the Wi-Fi Direct interface on a stock Fedora (measured in
/// hardware run 1), named when the real zone is unknown.
pub const LIKELY_ZONE: &str = "public";

/// Structured error for the GUI: the session is up but no media datagram reached us.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoRtp {
    /// UDP port we announced in `wfd_client_rtp_ports`.
    pub rtp_port: u16,
    pub waited: Duration,
    /// The Wi-Fi Direct group interface, if the caller knows it.
    pub interface: Option<String>,
    /// The firewalld zone of that interface, if the caller looked it up; else
    /// [`LIKELY_ZONE`] is named as the likely one.
    pub zone: Option<String>,
}

impl NoRtp {
    /// Fills in what the caller knows about the interface and its zone.
    pub fn with_interface(mut self, interface: Option<String>, zone: Option<String>) -> Self {
        self.interface = interface;
        self.zone = zone;
        self
    }

    /// The zone to name: the known one, else the likely one.
    pub fn zone_name(&self) -> &str {
        self.zone.as_deref().unwrap_or(LIKELY_ZONE)
    }

    /// Short headline for the window.
    pub fn title(&self) -> String {
        format!("No video: UDP port {} receives nothing", self.rtp_port)
    }

    /// Command the user can run to allow the port for this session only (nothing here runs
    /// it; MirrorWire never changes the firewall by itself).
    pub fn fix_command(&self) -> String {
        format!(
            "firewall-cmd --zone={} --add-port={}/udp",
            self.zone_name(),
            self.rtp_port
        )
    }
}

impl fmt::Display for NoRtp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let iface = self
            .interface
            .as_deref()
            .map_or_else(|| "the Wi-Fi Direct interface".to_string(), str::to_string);
        let zone = if self.zone.is_some() {
            format!("is in firewalld zone \"{}\"", self.zone_name())
        } else {
            format!("is most likely in firewalld zone \"{}\"", self.zone_name())
        };
        write!(
            f,
            "The source accepted the stream (RTSP OK) but no video arrived on UDP port {} in \
             {} s. The firewall probably drops it: {iface} {zone}, which does not allow UDP {}. \
             To allow it until the next reboot, run: {}",
            self.rtp_port,
            self.waited.as_secs(),
            self.rtp_port,
            self.fix_command()
        )
    }
}

impl std::error::Error for NoRtp {}

/// Media flowed and then stopped: the usual sign of a lost Wi-Fi Direct link (hardware run 4:
/// the laptop's station dropped out of the source's group mid-stream and the sink sat silent
/// until the RTSP keepalive timeout, about a minute later).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtpStalled {
    pub rtp_port: u16,
    /// Time since the last datagram when reported.
    pub silent_for: Duration,
}

impl fmt::Display for RtpStalled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Video stopped: nothing arrived on UDP port {} for {} s. The Wi-Fi Direct link to \
             the source was probably lost (weak signal, or the laptop's Wi-Fi sharing the radio \
             on another channel); the session ends if the source does not come back.",
            self.rtp_port,
            self.silent_for.as_secs()
        )
    }
}

impl std::error::Error for RtpStalled {}

/// Fires [`NoRtp`] once if no datagram arrives within the timeout after media started, and
/// [`RtpStalled`] once per silence of the same length after datagrams had arrived.
#[derive(Debug, Clone)]
pub struct NoRtpWatchdog {
    timeout: Duration,
    rtp_port: u16,
    started: Option<Instant>,
    got_datagram: bool,
    fired: bool,
    last_datagram: Option<Instant>,
    stall_reported: bool,
}

impl NoRtpWatchdog {
    pub fn new(rtp_port: u16, timeout: Duration) -> Self {
        Self {
            timeout,
            rtp_port,
            started: None,
            got_datagram: false,
            fired: false,
            last_datagram: None,
            stall_reported: false,
        }
    }

    /// PLAY was acknowledged; the source should send now.
    pub fn start(&mut self, now: Instant) {
        self.started.get_or_insert(now);
    }

    /// Any datagram counts (even a malformed one proves the firewall lets UDP through).
    pub fn on_datagram(&mut self) {
        self.got_datagram = true;
    }

    /// Like [`on_datagram`](Self::on_datagram), and remembers the time for stall detection.
    pub fn on_datagram_at(&mut self, now: Instant) {
        self.got_datagram = true;
        self.last_datagram = Some(now);
        self.stall_reported = false;
    }

    /// Time since the last datagram, once media has flowed.
    pub fn silence(&self, now: Instant) -> Option<Duration> {
        self.last_datagram.map(|t| now.saturating_duration_since(t))
    }

    /// `Some` once per silence of at least the timeout after datagrams had arrived (re-armed
    /// by the next datagram).
    pub fn poll_stall(&mut self, now: Instant) -> Option<RtpStalled> {
        let last = self.last_datagram?;
        let silent_for = now.saturating_duration_since(last);
        if self.stall_reported || silent_for < self.timeout {
            return None;
        }
        self.stall_reported = true;
        Some(RtpStalled {
            rtp_port: self.rtp_port,
            silent_for,
        })
    }

    /// `Some` exactly once, when the timeout passed without a datagram.
    pub fn poll(&mut self, now: Instant) -> Option<NoRtp> {
        let started = self.started?;
        let waited = now.saturating_duration_since(started);
        if self.fired || self.got_datagram || waited < self.timeout {
            return None;
        }
        self.fired = true;
        Some(NoRtp {
            rtp_port: self.rtp_port,
            waited,
            interface: None,
            zone: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fires_once_after_timeout_without_datagrams() {
        let t0 = Instant::now();
        let mut w = NoRtpWatchdog::new(19000, Duration::from_secs(5));
        assert_eq!(w.poll(t0 + Duration::from_secs(60)), None, "not started");
        w.start(t0);
        assert_eq!(w.poll(t0 + Duration::from_secs(4)), None);
        let e = w.poll(t0 + Duration::from_secs(5)).unwrap();
        assert_eq!(e.rtp_port, 19000);
        assert_eq!(e.waited, Duration::from_secs(5));
        assert_eq!(w.poll(t0 + Duration::from_secs(9)), None, "only once");
    }

    #[test]
    fn a_datagram_disarms_it() {
        let t0 = Instant::now();
        let mut w = NoRtpWatchdog::new(19000, Duration::from_secs(5));
        w.start(t0);
        w.on_datagram();
        assert_eq!(w.poll(t0 + Duration::from_secs(30)), None);
    }

    #[test]
    fn stall_after_flowing_media_fires_once_per_silence() {
        let t0 = Instant::now();
        let s = |n| t0 + Duration::from_secs(n);
        let mut w = NoRtpWatchdog::new(19000, Duration::from_secs(5));
        w.start(t0);
        assert_eq!(
            w.poll_stall(s(30)),
            None,
            "never flowed: that is NoRtp's case"
        );
        w.on_datagram_at(s(1));
        assert_eq!(w.poll_stall(s(5)), None);
        let e = w.poll_stall(s(6)).unwrap();
        assert_eq!((e.rtp_port, e.silent_for), (19000, Duration::from_secs(5)));
        assert!(e.to_string().contains("UDP port 19000"), "{e}");
        assert_eq!(w.poll_stall(s(20)), None, "once per silence");
        assert_eq!(w.poll(s(20)), None, "not a firewall case");
        w.on_datagram_at(s(21));
        assert_eq!(w.poll_stall(s(25)), None);
        assert!(w.poll_stall(s(26)).is_some(), "re-armed");
        assert_eq!(w.silence(s(26)), Some(Duration::from_secs(5)));
        assert_eq!(NoRtpWatchdog::new(1, Duration::ZERO).silence(s(1)), None);
    }

    #[test]
    fn message_names_zone_port_and_fix() {
        let e = NoRtp {
            rtp_port: 19000,
            waited: Duration::from_secs(5),
            interface: None,
            zone: None,
        };
        let m = e.to_string();
        assert!(m.contains("UDP port 19000"), "{m}");
        assert!(
            m.contains("most likely in firewalld zone \"public\""),
            "{m}"
        );
        assert_eq!(
            e.fix_command(),
            "firewall-cmd --zone=public --add-port=19000/udp"
        );
        assert!(e.title().contains("19000"));

        let e = e.with_interface(Some("p2p-wlp58s0-3".into()), Some("home".into()));
        let m = e.to_string();
        assert!(
            m.contains("p2p-wlp58s0-3 is in firewalld zone \"home\""),
            "{m}"
        );
        assert!(m.contains("--zone=home"), "{m}");
    }
}
