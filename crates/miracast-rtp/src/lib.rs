#![forbid(unsafe_code)]
//! Miracast media receive: RTP (RFC 3550) carrying MPEG-TS (ISO 13818-1) with H.264 video and
//! audio PES, delivered as access units (step 5; `docs/agents/miracast.md` section 4).
//!
//! Sans-IO: the caller owns the UDP socket and hands each datagram to [`Receiver::push`], so
//! everything tests from synthetic streams. Layers: [`rtp`] (header, reorder, loss), [`ts`]
//! (PAT/PMT, PES, continuity), [`Receiver`] (glue). Loss makes the sink drop video until the
//! next IDR and calls [`MediaSink::need_keyframe`] once per loss episode.

pub mod diag;
pub mod h264;
pub mod rtp;
pub mod ts;

use std::time::{Duration, Instant};

/// Longest a packet waits for a missing earlier one (the jitter budget). Wi-Fi reordering is
/// a few ms; 40 ms keeps latency low and still repairs normal reordering.
pub const JITTER_WAIT: Duration = Duration::from_millis(40);

/// Receive counters for the trace and the GUI.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RxStats {
    /// RTP packets accepted (before reordering).
    pub packets: u64,
    pub lost_packets: u64,
    /// Loss episodes (gaps given up on).
    pub gaps: u64,
    /// Out-of-order packets that were repaired.
    pub reordered: u64,
    /// Late or duplicate packets dropped.
    pub late: u64,
    pub bad_datagrams: u64,
}

/// Network/port settings the RTSP layer negotiated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RtpConfig {
    /// UDP port the source sends to (`wfd_client_rtp_ports`, default 19000).
    pub rtp_port: u16,
}

impl Default for RtpConfig {
    fn default() -> Self {
        Self { rtp_port: 19000 }
    }
}

/// One H.264 access unit in Annex B form (start codes), ready for a decoder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoAccessUnit {
    /// Presentation timestamp, 90 kHz ticks.
    pub pts_90khz: u64,
    pub keyframe: bool,
    pub data: Vec<u8>,
}

/// One audio PES payload (LPCM or AAC ADTS); demuxed but not played (non-goal).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioFrame {
    pub pts_90khz: u64,
    pub data: Vec<u8>,
}

/// Consumer of demuxed media. MirrorWire's decoder path implements this later.
pub trait MediaSink {
    fn video(&mut self, unit: VideoAccessUnit);
    fn audio(&mut self, frame: AudioFrame);
    /// Loss or a continuity error made the next video unit undecodable: the RTSP layer should
    /// send `wfd_idr_request` (M13).
    fn need_keyframe(&mut self);
}

/// RTP datagrams in, media out.
#[derive(Debug)]
pub struct Receiver {
    reorder: rtp::Reorder,
    demux: ts::Demux,
    ssrc: Option<u32>,
    /// Datagrams dropped as malformed or of the wrong payload type.
    pub bad_datagrams: u64,
    /// RTP packets given up on (gaps).
    pub lost_packets: u64,
    packets: u64,
}

impl Default for Receiver {
    fn default() -> Self {
        Self::new()
    }
}

impl Receiver {
    /// Reorder depth 8 packets (about 56 TS packets) or [`JITTER_WAIT`], whichever is first.
    pub fn new() -> Self {
        Self {
            reorder: rtp::Reorder::new(8, JITTER_WAIT),
            demux: ts::Demux::new(),
            ssrc: None,
            bad_datagrams: 0,
            lost_packets: 0,
            packets: 0,
        }
    }

    pub fn stats(&self) -> RxStats {
        RxStats {
            packets: self.packets,
            lost_packets: self.lost_packets,
            gaps: self.reorder.gaps,
            reordered: self.reorder.reordered,
            late: self.reorder.dropped_late,
            bad_datagrams: self.bad_datagrams,
        }
    }

    /// Releases packets held too long behind a gap; call regularly, also when idle.
    pub fn poll(&mut self, now: Instant, sink: &mut dyn MediaSink) {
        let out = self.reorder.expire(now);
        self.deliver(out, sink);
    }

    fn deliver(&mut self, out: rtp::Released, sink: &mut dyn MediaSink) {
        if out.lost > 0 {
            self.lost_packets += u64::from(out.lost);
            self.demux.discontinuity(sink);
        }
        for p in &out.payloads {
            self.demux.push(p, sink);
        }
    }

    pub fn ts_stats(&self) -> ts::TsStats {
        self.demux.stats
    }

    /// Process one UDP datagram.
    pub fn push(&mut self, datagram: &[u8], now: Instant, sink: &mut dyn MediaSink) {
        let pkt = match rtp::parse(datagram) {
            Ok(p) if p.payload_type == rtp::PT_MP2T && p.payload.len() % ts::TS_PACKET == 0 => p,
            _ => {
                self.bad_datagrams += 1;
                return;
            }
        };
        if self.ssrc.is_some_and(|s| s != pkt.ssrc) {
            // New source stream: restart sequence tracking.
            self.reorder.reset();
            self.demux.discontinuity(sink);
        }
        self.ssrc = Some(pkt.ssrc);
        self.packets += 1;
        let out = self.reorder.push(pkt.seq, pkt.payload, now);
        self.deliver(out, sink);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ts::synth::*;

    #[derive(Default)]
    struct Rec {
        video: Vec<VideoAccessUnit>,
        audio: Vec<AudioFrame>,
        idr_requests: u32,
    }
    impl MediaSink for Rec {
        fn video(&mut self, u: VideoAccessUnit) {
            self.video.push(u);
        }
        fn audio(&mut self, f: AudioFrame) {
            self.audio.push(f);
        }
        fn need_keyframe(&mut self) {
            self.idr_requests += 1;
        }
    }

    /// Annex B access unit: SPS, PPS, then an IDR (type 5) or non-IDR (type 1) slice.
    fn au(idr: bool, len: usize) -> Vec<u8> {
        let mut v = vec![0, 0, 0, 1, 0x67, 1, 2, 0, 0, 0, 1, 0x68, 3];
        v.extend_from_slice(&[0, 0, 0, 1, if idr { 0x65 } else { 0x41 }]);
        v.extend((0..len).map(|i| 0x10 + (i % 7) as u8));
        v
    }

    /// PSI + `n` frames (every 5th an IDR) as RTP datagrams, plus the AUs sent.
    fn stream(n: usize) -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
        let (mut seq, mut vcc, mut acc, mut pcc) = (0u16, 0u8, 0u8, 0u8);
        let (mut dgrams, mut aus) = (Vec::new(), Vec::new());
        for i in 0..n {
            let mut tsp = psi_packets(pcc);
            pcc = (pcc + 1) & 0x0F;
            let a = au(i % 5 == 0, 100 + i * 150);
            let pts = 90_000 + i as u64 * 3000;
            tsp.extend(packetize(PID_VIDEO, &mut vcc, &pes(0xE0, pts, &a, false)));
            tsp.extend(packetize(
                PID_AUDIO,
                &mut acc,
                &pes(0xC0, pts, &[0xFF, 0xF1, i as u8], true),
            ));
            dgrams.extend(rtp_packets(&mut seq, pts as u32, &tsp));
            aus.push(a);
        }
        (dgrams, aus)
    }

    #[test]
    fn clean_stream_yields_every_access_unit() {
        let (d, aus) = stream(12);
        let (mut rx, mut sink) = (Receiver::new(), Rec::default());
        for p in &d {
            rx.push(p, Instant::now(), &mut sink);
        }
        // The last video PES is unbounded and only flushes on the next start.
        assert_eq!(sink.video.len(), 11);
        for (i, u) in sink.video.iter().enumerate() {
            assert_eq!(u.data, aus[i]);
            assert_eq!(u.pts_90khz, 90_000 + i as u64 * 3000);
            assert_eq!(u.keyframe, i % 5 == 0);
        }
        assert_eq!(sink.audio.len(), 12);
        assert_eq!(sink.audio[3].data, vec![0xFF, 0xF1, 3]);
        assert_eq!(sink.idr_requests, 0);
        assert_eq!(rx.ts_stats().cc_errors, 0);
    }

    #[test]
    fn reordered_packets_are_repaired() {
        let (mut d, aus) = stream(12);
        d.swap(4, 5);
        d.swap(10, 12);
        let (mut rx, mut sink) = (Receiver::new(), Rec::default());
        for p in &d {
            rx.push(p, Instant::now(), &mut sink);
        }
        assert_eq!(sink.video.len(), 11);
        assert_eq!(sink.video[4].data, aus[4]);
        assert_eq!(sink.idr_requests, 0);
        assert_eq!(rx.lost_packets, 0);
    }

    #[test]
    fn loss_requests_idr_and_resumes_on_the_next_one() {
        let (mut d, aus) = stream(14);
        d.drain(12..14); // lose two datagrams mid-stream
        let (mut rx, mut sink) = (Receiver::new(), Rec::default());
        for p in &d {
            rx.push(p, Instant::now(), &mut sink);
        }
        assert_eq!(rx.lost_packets, 2);
        assert_eq!(sink.idr_requests, 1, "one request per loss episode");
        // Nothing after the gap is delivered until an IDR (frame 10 follows the gap or is
        // pre-gap; every unit delivered after the loss must be a keyframe first).
        let first_after = sink.video.iter().position(|u| u.data == aus[10]);
        for u in &sink.video {
            assert!(
                aus.contains(&u.data),
                "no corrupted unit may reach the decoder"
            );
        }
        assert!(first_after.is_some() || sink.video.last().is_some_and(|u| u.keyframe));
    }

    #[test]
    fn gap_in_video_continuity_drops_until_idr() {
        let (mut d, aus) = stream(14);
        // Remove one datagram that is in the middle of a P-frame run (inside frames 6-9).
        let (mut rx, mut sink) = (Receiver::new(), Rec::default());
        let mid = d.len() / 2;
        d.remove(mid);
        for p in &d {
            rx.push(p, Instant::now(), &mut sink);
        }
        assert!(sink.idr_requests >= 1);
        for u in &sink.video {
            assert!(aus.contains(&u.data));
        }
        // After the gap the first unit delivered is an IDR.
        let gap_pts = sink
            .video
            .windows(2)
            .find(|w| w[1].pts_90khz - w[0].pts_90khz > 3000);
        if let Some(w) = gap_pts {
            assert!(w[1].keyframe);
        }
    }

    #[test]
    fn jitter_buffer_releases_after_the_wait_and_counts() {
        let (dgrams, _) = stream(10);
        assert!(dgrams.len() > 5);
        let mut sink = Rec::default();
        let mut rx = Receiver::new();
        let t0 = Instant::now();
        // Drop the second datagram; nothing after the gap may wait longer than JITTER_WAIT.
        rx.push(&dgrams[0], t0, &mut sink);
        for d in dgrams.iter().skip(2).take(3) {
            rx.push(d, t0, &mut sink);
        }
        let held = rx.stats();
        assert_eq!((held.packets, held.lost_packets), (4, 0));
        rx.poll(t0 + JITTER_WAIT, &mut sink);
        let s = rx.stats();
        assert_eq!((s.lost_packets, s.gaps), (1, 1));
        assert!(sink.idr_requests >= 1, "loss asks for a keyframe");
    }

    #[test]
    fn garbage_datagrams_are_counted_not_fatal() {
        let (mut rx, mut sink) = (Receiver::new(), Rec::default());
        rx.push(&[1, 2, 3], Instant::now(), &mut sink);
        let mut wrong_pt = vec![0x80, 96, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0];
        wrong_pt.extend_from_slice(&[0; 188]);
        rx.push(&wrong_pt, Instant::now(), &mut sink);
        let mut short = vec![0x80, 33, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0];
        short.extend_from_slice(&[0x47; 100]);
        rx.push(&short, Instant::now(), &mut sink);
        assert_eq!(rx.bad_datagrams, 3);
        // 188 bytes of noise inside a valid RTP packet: no panic, no output.
        let mut noise = vec![0x80, 33, 0, 3, 0, 0, 0, 0, 0, 0, 0, 0];
        noise.extend((0..188u32).map(|i| (i * 31 % 251) as u8));
        rx.push(&noise, Instant::now(), &mut sink);
        assert!(sink.video.is_empty());
    }

    #[test]
    fn pes_survives_truncated_and_oversized_fields() {
        let mut d = ts::Demux::new();
        let mut sink = Rec::default();
        for p in psi_packets(0) {
            d.push(&p, &mut sink);
        }
        // A PES start whose header length overruns the packet.
        let mut bad = pes(0xE0, 1, &au(true, 10), false);
        bad[8] = 250;
        let mut cc = 0;
        for p in packetize(PID_VIDEO, &mut cc, &bad) {
            d.push(&p, &mut sink);
        }
        let mut cc2 = cc;
        for p in packetize(PID_VIDEO, &mut cc2, &pes(0xE0, 2, &au(true, 10), false)) {
            d.push(&p, &mut sink);
        }
        assert!(sink.video.is_empty()); // bad one dropped, good one still waiting for flush
        assert_eq!(d.stats.dropped_units, 1);
    }

    #[test]
    fn real_ffmpeg_output_demuxes() {
        // Optional: skipped when ffmpeg is not installed.
        let Ok(out) = std::process::Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=320x240:rate=30",
                "-t",
                "1",
                "-c:v",
                "libx264",
                "-bf",
                "0",
                "-g",
                "15",
                "-bsf:v",
                "h264_mp4toannexb",
                "-f",
                "mpegts",
                "-",
            ])
            .output()
        else {
            return;
        };
        if !out.status.success() || out.stdout.len() < 188 * 10 {
            return; // no libx264 here
        }
        let (mut d, mut sink) = (ts::Demux::new(), Rec::default());
        d.push(&out.stdout[..out.stdout.len() / 188 * 188], &mut sink);
        assert!(sink.video.len() >= 25, "got {}", sink.video.len());
        assert!(sink.video[0].keyframe);
        assert!(sink.video.iter().filter(|u| u.keyframe).count() >= 2);
        assert_eq!(d.stats.cc_errors, 0);
    }

    #[test]
    fn default_port_is_the_wfd_default() {
        assert_eq!(RtpConfig::default().rtp_port, 19000);
    }
}
