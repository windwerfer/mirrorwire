//! MPEG-TS (ISO/IEC 13818-1) demux: PAT/PMT, PES reassembly, continuity checks. Sans-IO; input
//! is hostile, so every length is bounded and nothing indexes unchecked.

use crate::{AudioFrame, MediaSink, VideoAccessUnit};

pub const TS_PACKET: usize = 188;
const SYNC: u8 = 0x47;
const PID_PAT: u16 = 0;
/// Upper bound for one reassembled PES (a 4K IDR is well below this); beyond it, drop.
const MAX_PES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Video,
    Audio,
}

#[derive(Debug, Default)]
struct Pes {
    buf: Vec<u8>,
    started: bool,
    /// Bytes expected in total (header included) when the PES length field is non-zero.
    expected: Option<usize>,
}

#[derive(Debug)]
struct Stream {
    kind: Kind,
    last_cc: Option<u8>,
    pes: Pes,
}

/// Counters, for the GUI/diagnostics (AGENTS.md §1.8).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TsStats {
    pub packets: u64,
    pub bad_packets: u64,
    pub cc_errors: u64,
    pub video_units: u64,
    pub audio_frames: u64,
    pub dropped_units: u64,
}

/// Transport-stream demuxer.
#[derive(Debug, Default)]
pub struct Demux {
    pmt_pid: Option<u16>,
    streams: Vec<(u16, Stream)>,
    /// After loss, video is dropped until the next IDR.
    need_idr: bool,
    pub stats: TsStats,
}

fn mark_loss(need_idr: &mut bool, sink: &mut dyn MediaSink) {
    if !*need_idr {
        *need_idr = true;
        sink.need_keyframe();
    }
}

impl Demux {
    pub fn new() -> Self {
        Self::default()
    }

    /// A gap upstream (RTP loss): drop partial PES data and wait for an IDR.
    pub fn discontinuity(&mut self, sink: &mut dyn MediaSink) {
        for (_, s) in &mut self.streams {
            s.pes = Pes::default();
            s.last_cc = None;
        }
        mark_loss(&mut self.need_idr, sink);
    }

    /// Feed a payload made of whole 188-byte packets (an RTP payload, 7 packets typically).
    /// Malformed packets are counted in `stats` and skipped.
    pub fn push(&mut self, data: &[u8], sink: &mut dyn MediaSink) {
        for p in data.chunks(TS_PACKET) {
            match <&[u8; TS_PACKET]>::try_from(p) {
                Ok(p) => self.packet(p, sink),
                Err(_) => self.stats.bad_packets += 1,
            }
        }
    }

    fn packet(&mut self, p: &[u8; TS_PACKET], sink: &mut dyn MediaSink) {
        self.stats.packets += 1;
        let [sync, b1, b2, b3, rest @ ..] = p;
        if *sync != SYNC || b1 & 0x80 != 0 {
            self.stats.bad_packets += 1; // sync lost or transport error indicator
            if b1 & 0x80 != 0 {
                self.discontinuity(sink);
            }
            return;
        }
        let pusi = b1 & 0x40 != 0;
        let pid = u16::from_be_bytes([b1 & 0x1F, *b2]);
        let afc = (b3 >> 4) & 3;
        let cc = b3 & 0x0F;
        let mut discontinuity_flag = false;
        let mut payload: &[u8] = rest;
        if afc & 2 != 0 {
            let (len, after) = rest.split_first().unwrap_or((&0, &[]));
            let len = usize::from(*len);
            discontinuity_flag = after
                .get(..len)
                .and_then(<[u8]>::first)
                .is_some_and(|f| f & 0x80 != 0);
            payload = after.get(len..).unwrap_or(&[]);
        }
        let has_payload = afc & 1 != 0;

        if pid == PID_PAT {
            if pusi && has_payload {
                self.parse_pat(payload);
            }
            return;
        }
        if Some(pid) == self.pmt_pid {
            if pusi && has_payload {
                self.parse_pmt(payload);
            }
            return;
        }
        let Self {
            streams,
            stats,
            need_idr,
            ..
        } = self;
        let Some(stream) = streams.iter_mut().find(|(q, _)| *q == pid).map(|(_, s)| s) else {
            return;
        };
        if !has_payload {
            return;
        }
        // Continuity: the counter increments only on packets carrying payload; a single repeat
        // is a legal duplicate.
        let mut loss = false;
        if let Some(last) = stream.last_cc {
            if cc == last && !pusi {
                return; // duplicate
            }
            loss = cc != (last + 1) & 0x0F && !discontinuity_flag;
        }
        stream.last_cc = Some(cc);
        if loss {
            stats.cc_errors += 1;
            stream.pes = Pes::default();
            if stream.kind == Kind::Video {
                mark_loss(need_idr, sink);
            }
        }
        if pusi {
            flush(stream, stats, need_idr, sink);
            stream.pes.started = true;
            stream.pes.expected = payload
                .get(4..6)
                .map(|b| {
                    usize::from(u16::from_be_bytes([
                        b.first().copied().unwrap_or(0),
                        b.get(1).copied().unwrap_or(0),
                    ]))
                })
                .filter(|&n| n > 0)
                .map(|n| n + 6);
        }
        if !stream.pes.started {
            return; // mid-PES data after loss/start: wait for the next start
        }
        if stream.pes.buf.len() + payload.len() > MAX_PES {
            stream.pes = Pes::default();
            stats.dropped_units += 1;
            return;
        }
        stream.pes.buf.extend_from_slice(payload);
        if stream
            .pes
            .expected
            .is_some_and(|n| stream.pes.buf.len() >= n)
        {
            flush(stream, stats, need_idr, sink);
        }
    }

    fn parse_pat(&mut self, payload: &[u8]) {
        let Some(sec) = section(payload) else { return };
        if sec.first() != Some(&0x00) {
            return;
        }
        // table_id(1) len(2) tsid(2) ver(1) sec(1) last(1) | programs(4 each) | crc(4)
        let Some(mut body) = sec.get(8..sec.len().saturating_sub(4)) else {
            return;
        };
        while let Some((&[p0, p1, m0, m1], tail)) = body.split_first_chunk::<4>() {
            if u16::from_be_bytes([p0, p1]) != 0 {
                self.pmt_pid = Some(u16::from_be_bytes([m0 & 0x1F, m1]));
                return;
            }
            body = tail;
        }
    }

    fn parse_pmt(&mut self, payload: &[u8]) {
        let Some(sec) = section(payload) else { return };
        if sec.first() != Some(&0x02) {
            return;
        }
        // 8 header bytes, PCR pid(2), program_info_length(2), descriptors, streams, crc(4)
        let Some(&[i0, i1]) = sec.get(10..).and_then(|s| s.first_chunk::<2>()) else {
            return;
        };
        let pil = usize::from(u16::from_be_bytes([i0, i1]) & 0x0FFF);
        let Some(mut rest) = sec.get(12 + pil..sec.len().saturating_sub(4)) else {
            return;
        };
        while let Some((&[st, p0, p1, l0, l1], tail)) = rest.split_first_chunk::<5>() {
            let pid = u16::from_be_bytes([p0 & 0x1F, p1]);
            let es_len = usize::from(u16::from_be_bytes([l0, l1]) & 0x0FFF);
            let kind = match st {
                0x1B => Some(Kind::Video),
                // AAC (ADTS), AC-3 in WFD, private LPCM, PES-private data
                0x0F | 0x11 | 0x81 | 0x83 | 0x06 => Some(Kind::Audio),
                _ => None,
            };
            if let Some(kind) = kind {
                if !self.streams.iter().any(|(q, _)| *q == pid) {
                    self.streams.push((
                        pid,
                        Stream {
                            kind,
                            last_cc: None,
                            pes: Pes::default(),
                        },
                    ));
                }
            }
            let Some(next) = tail.get(es_len..) else {
                return;
            };
            rest = next;
        }
    }
}

fn flush(s: &mut Stream, stats: &mut TsStats, need_idr: &mut bool, sink: &mut dyn MediaSink) {
    let pes = std::mem::take(&mut s.pes);
    if !pes.started {
        return;
    }
    let Some((pts, es)) = parse_pes(&pes.buf, pes.expected) else {
        stats.dropped_units += 1;
        return;
    };
    match s.kind {
        Kind::Audio => {
            stats.audio_frames += 1;
            sink.audio(AudioFrame {
                pts_90khz: pts,
                data: es.to_vec(),
            });
        }
        Kind::Video => {
            let keyframe = has_idr(es);
            if *need_idr {
                if !keyframe {
                    stats.dropped_units += 1;
                    return;
                }
                *need_idr = false;
            }
            stats.video_units += 1;
            sink.video(VideoAccessUnit {
                pts_90khz: pts,
                keyframe,
                data: es.to_vec(),
            });
        }
    }
}

/// Section bytes (table_id..CRC) from a PSI payload, single-packet sections only.
fn section(payload: &[u8]) -> Option<&[u8]> {
    let ptr = usize::from(*payload.first()?);
    let s = payload.get(1 + ptr..)?;
    let len = usize::from(u16::from_be_bytes([*s.get(1)?, *s.get(2)?]) & 0x0FFF);
    s.get(..3 + len)
}

/// Returns (pts in 90 kHz or 0 when absent, elementary payload).
fn parse_pes(buf: &[u8], expected: Option<usize>) -> Option<(u64, &[u8])> {
    let &[0, 0, 1, _, _, _, _, flags, hdr_len] = buf.first_chunk::<9>()? else {
        return None;
    };
    let pts = if flags & 0x80 != 0 {
        let &[b0, b1, b2, b3, b4] = buf.get(9..)?.first_chunk::<5>()?;
        (u64::from(b0 >> 1 & 7) << 30)
            | (u64::from(b1) << 22)
            | (u64::from(b2 >> 1) << 15)
            | (u64::from(b3) << 7)
            | u64::from(b4 >> 1)
    } else {
        0
    };
    let end = expected.map_or(buf.len(), |n| n.min(buf.len()));
    Some((pts, buf.get(9 + usize::from(hdr_len)..end)?))
}

/// True when the Annex B stream carries an IDR slice (NAL type 5).
pub fn has_idr(es: &[u8]) -> bool {
    es.windows(4)
        .any(|w| matches!(w, [0, 0, 1, n] if n & 0x1F == 5))
}

/// Synthetic TS/RTP builders: used by tests and by the loopback test source (step 6).
#[allow(clippy::indexing_slicing)] // builders over fixed-size test data, never fed network input
pub mod synth {
    use super::*;

    pub const PID_PMT: u16 = 0x100;
    pub const PID_VIDEO: u16 = 0x1011;
    pub const PID_AUDIO: u16 = 0x1100;

    fn ts(pid: u16, pusi: bool, cc: u8, payload: &[u8]) -> [u8; TS_PACKET] {
        let mut p = [0xFFu8; TS_PACKET];
        p[0] = SYNC;
        p[1] = (u8::from(pusi) << 6) | (pid >> 8) as u8;
        p[2] = pid as u8;
        let room = TS_PACKET - 4;
        // Pad with an adaptation field when the payload does not fill the packet.
        let stuffing = room - payload.len();
        if stuffing == 0 {
            p[3] = 0x10 | cc;
            p[4..].copy_from_slice(payload);
        } else {
            let af_len = stuffing.max(1) - 1;
            p[3] = 0x30 | cc;
            p[4] = af_len as u8;
            if af_len > 0 {
                p[5] = 0;
            }
            p[5 + af_len..].copy_from_slice(payload);
        }
        p
    }

    fn psi(table: &[u8]) -> Vec<u8> {
        let mut v = vec![0]; // pointer field
        v.extend_from_slice(table);
        v.extend_from_slice(&[0; 4]); // CRC is not checked by the sink
        v
    }

    /// PAT then PMT (H.264 video + AAC audio) packets; `cc` is the shared PSI counter.
    pub fn psi_packets(cc: u8) -> Vec<[u8; TS_PACKET]> {
        let pat = [
            0x00,
            0xB0,
            13,
            0,
            1,
            0xC1,
            0,
            0,
            0,
            1,
            0xE0 | (PID_PMT >> 8) as u8,
            PID_PMT as u8,
        ];
        let pmt = [
            0x02,
            0xB0,
            23,
            0,
            1,
            0xC1,
            0,
            0,
            0xE0 | (PID_VIDEO >> 8) as u8,
            PID_VIDEO as u8,
            0xF0,
            0,
            0x1B,
            0xE0 | (PID_VIDEO >> 8) as u8,
            PID_VIDEO as u8,
            0xF0,
            0,
            0x0F,
            0xE0 | (PID_AUDIO >> 8) as u8,
            PID_AUDIO as u8,
            0xF0,
            0,
        ];
        vec![
            ts(PID_PAT, true, cc, &psi(&pat)),
            ts(PID_PMT, true, cc, &psi(&pmt)),
        ]
    }

    /// PES with a PTS (`stream_id` 0xE0 video / 0xC0 audio); video uses length 0 (unbounded).
    pub fn pes(stream_id: u8, pts: u64, es: &[u8], bounded: bool) -> Vec<u8> {
        let mut v = vec![0, 0, 1, stream_id];
        let len = if bounded { es.len() + 8 } else { 0 };
        v.extend_from_slice(&(len as u16).to_be_bytes());
        v.extend_from_slice(&[0x80, 0x80, 5]);
        v.extend_from_slice(&[
            0x21 | ((pts >> 29) & 0x0E) as u8,
            (pts >> 22) as u8,
            0x01 | ((pts >> 14) & 0xFE) as u8,
            (pts >> 7) as u8,
            0x01 | ((pts << 1) & 0xFE) as u8,
        ]);
        v.extend_from_slice(es);
        v
    }

    /// Split a PES into TS packets on `pid`, advancing `cc`.
    pub fn packetize(pid: u16, cc: &mut u8, pes: &[u8]) -> Vec<[u8; TS_PACKET]> {
        let mut out = Vec::new();
        for (i, chunk) in pes.chunks(TS_PACKET - 4).enumerate() {
            out.push(ts(pid, i == 0, *cc, chunk));
            *cc = (*cc + 1) & 0x0F;
        }
        out
    }

    /// Wrap up to 7 TS packets per RTP packet (payload type 33).
    pub fn rtp_packets(
        seq: &mut u16,
        timestamp: u32,
        ts_packets: &[[u8; TS_PACKET]],
    ) -> Vec<Vec<u8>> {
        ts_packets
            .chunks(7)
            .map(|c| {
                let mut v = vec![0x80, crate::rtp::PT_MP2T];
                v.extend_from_slice(&seq.to_be_bytes());
                v.extend_from_slice(&timestamp.to_be_bytes());
                v.extend_from_slice(&0xABCD_0001u32.to_be_bytes());
                for p in c {
                    v.extend_from_slice(p);
                }
                *seq = seq.wrapping_add(1);
                v
            })
            .collect()
    }
}
