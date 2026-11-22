//! Optional pure-Rust H.264 encoder (FilmCraft's, `craft_video::h264` from craft-libs, feature
//! `filmcraft`), for comparison with the FFmpeg backends.
//!
//! Input is a CPU I420 picture made by the pure-Rust converter (`convert.rs`) from the
//! shared-memory capture; the capture size must equal the encoder size (no scaling).
//! Settings: Constrained Baseline (CAVLC), `speed` preset, no B-frames, no
//! lookahead/scenecut, one packet out per frame in. FilmCraft has no
//! "force IDR" call and no intra refresh, so a keyframe request re-creates the
//! encoder (well under a millisecond; rate-control history restarts).

use crate::convert::{packed_to_i420, I420Planes};
use crate::encode::{path, EncoderConfig, RateControl};
use crate::frame::{EncodedFrame, RawFrame};
use anyhow::{bail, Result};
use bytes::Bytes;
use craft_video::h264 as fc;
use mirrorwire_proto::clock::now_us;

pub const NAME: &str = "filmcraft-h264enc";

/// Slices = parallel work units (FilmCraft encodes slices on separate threads).
/// One per core up to 8, at least 2, never more than macroblock rows / 4.
pub fn default_slices(height: u32) -> usize {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let mb_rows = (height as usize).div_ceil(16);
    cores.clamp(2, 8).min((mb_rows / 4).max(1))
}

pub struct FcEncoder {
    enc: fc::Encoder,
    cfg: fc::EncoderConfig,
    frames: u64,
    pub summary: String,
}

impl FcEncoder {
    pub fn new(config: &EncoderConfig) -> Result<Self> {
        let (w, h) = (config.width, config.height);
        if w % 2 != 0 || h % 2 != 0 {
            bail!("{NAME} needs an even size, got {w}x{h} (use --size)");
        }
        let mut cfg = fc::EncoderConfig::new(w, h, config.fps.max(1), 1);
        cfg.profile = fc::Profile::Baseline;
        cfg.preset = fc::Preset::Speed;
        cfg.bframes = 0;
        cfg.scenecut = false;
        cfg.keyint = config.gop_frames();
        cfg.format = fc::PacketFormat::AnnexB;
        cfg.aud = false;
        cfg.threads = 0;
        cfg.slices = std::env::var("MIRRORWIRE_FILMCRAFT_SLICES")
            .ok()
            .and_then(|s| s.parse().ok())
            .filter(|&n: &usize| n > 0)
            .unwrap_or_else(|| default_slices(h));
        // Same picture as the x264 path (swscale defaults); leave the VUI
        // colour description unspecified like x264 does.
        cfg.color = fc::ColorConfig {
            primaries: 2,
            transfer: 2,
            matrix: 2,
            full_range: false,
        };
        let kbps = config.bitrate_kbps.max(100);
        let rc_name;
        cfg.rate = match config.rate_control {
            RateControl::Cqp | RateControl::Icq => {
                rc_name = format!("qp {}", config.qp);
                fc::RateControl::Qp(config.qp.min(51) as u8)
            }
            RateControl::Vbr => {
                rc_name = format!(
                    "vbr {:.1}/{:.1} Mbit/s",
                    kbps as f64 / 1000.0,
                    kbps as f64 * 1.25 / 1000.0
                );
                fc::RateControl::Vbr {
                    target_kbps: kbps,
                    max_kbps: kbps + kbps / 4,
                }
            }
            RateControl::Auto | RateControl::Cbr => {
                rc_name = format!("cbr {:.1} Mbit/s", kbps as f64 / 1000.0);
                fc::RateControl::Cbr { kbps }
            }
        };
        let enc = fc::Encoder::new(cfg.clone())?;
        let summary = format!(
            "encoder {NAME} {w}x{h}  rc={rc_name}  profile=baseline  preset=speed  slices={}  gop={}  bframes=0  convert=shm (cpu)  (no intra refresh)",
            cfg.slices, cfg.keyint
        );
        if config.intra_refresh {
            tracing::info!("{NAME} has no intra refresh; using keyframes on request instead");
        }
        Ok(Self {
            enc,
            cfg,
            frames: 0,
            summary,
        })
    }

    /// Encode one I420 (YUV420P) picture. Returns (Annex-B data, keyframe) access units.
    pub fn encode(
        &mut self,
        pic: &fc::YuvFrame<'_>,
        pts: i64,
        force_keyframe: bool,
    ) -> Result<Vec<(Vec<u8>, bool)>> {
        if force_keyframe && self.frames > 0 {
            // No public force-IDR: a fresh encoder starts with an IDR.
            self.enc = fc::Encoder::new(self.cfg.clone())?;
        }
        let packets = self.enc.encode(pic, pts)?;
        self.frames += 1;
        Ok(packets.into_iter().map(|p| (p.data, p.keyframe)).collect())
    }
}

/// FilmCraft as an `Encoder` backend: shared-memory frame -> I420 (pure Rust) -> H.264.
pub struct FilmEncoder {
    fc: FcEncoder,
    config: EncoderConfig,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
    start_us: Option<u64>,
    last_pts: Option<i64>,
    have_frame: bool,
    last_convert_us: u64,
    last_encode_us: u64,
}

impl FilmEncoder {
    pub fn new(config: &EncoderConfig) -> Result<Self> {
        let fc = FcEncoder::new(config)?;
        tracing::info!("{}", fc.summary);
        let (w, h) = (config.width as usize, config.height as usize);
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        Ok(Self {
            fc,
            config: config.clone(),
            y: vec![0; w * h],
            u: vec![0; cw * ch],
            v: vec![0; cw * ch],
            start_us: None,
            last_pts: None,
            have_frame: false,
            last_convert_us: 0,
            last_encode_us: 0,
        })
    }

    pub fn name(&self) -> &'static str {
        NAME
    }

    pub fn conversion_path(&self) -> &'static str {
        path::SHM_CPU
    }

    pub fn rc_summary(&self) -> &str {
        &self.fc.summary
    }

    pub fn output_size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    pub fn config(&self) -> &EncoderConfig {
        &self.config
    }

    pub fn last_timings_ms(&self) -> (f64, f64) {
        (
            self.last_convert_us as f64 / 1000.0,
            self.last_encode_us as f64 / 1000.0,
        )
    }

    pub fn last_breakdown_ms(&self) -> (f64, f64, f64, f64) {
        (
            0.0,
            self.last_convert_us as f64 / 1000.0,
            0.0,
            self.last_encode_us as f64 / 1000.0,
        )
    }

    pub fn encode(&mut self, raw: &RawFrame, force_keyframe: bool) -> Result<Vec<EncodedFrame>> {
        if raw.dmabuf.is_some() {
            bail!("{NAME} cannot take DMA-BUF frames");
        }
        let (w, h) = (self.config.width, self.config.height);
        if (raw.width, raw.height) != (w, h) {
            bail!(
                "{NAME} does not scale: capture is {}x{} but the encoder size is {w}x{h} (omit --size)",
                raw.width,
                raw.height
            );
        }
        let t0 = now_us();
        let mut planes = I420Planes {
            y: &mut self.y,
            u: &mut self.u,
            v: &mut self.v,
            y_stride: w as usize,
            uv_stride: (w as usize).div_ceil(2),
        };
        packed_to_i420(
            &raw.data,
            raw.stride,
            w as usize,
            h as usize,
            raw.format,
            &mut planes,
        )?;
        self.last_convert_us = now_us().saturating_sub(t0);
        self.have_frame = true;
        self.submit(raw.seq, raw.capture_us, force_keyframe)
    }

    pub fn encode_repeat(
        &mut self,
        seq: u64,
        capture_us: u64,
        force_keyframe: bool,
    ) -> Result<Vec<EncodedFrame>> {
        if !self.have_frame {
            return Ok(Vec::new());
        }
        self.last_convert_us = 0;
        self.submit(seq, capture_us, force_keyframe)
    }

    fn submit(
        &mut self,
        seq: u64,
        capture_us: u64,
        force_keyframe: bool,
    ) -> Result<Vec<EncodedFrame>> {
        let start = *self.start_us.get_or_insert(capture_us);
        let mut pts = capture_us.saturating_sub(start) as i64;
        if let Some(last) = self.last_pts {
            if pts <= last {
                pts = last + 1;
            }
        }
        self.last_pts = Some(pts);
        let t0 = now_us();
        let frame = fc::YuvFrame {
            y: &self.y,
            u: &self.u,
            v: &self.v,
            y_stride: self.config.width as usize,
            uv_stride: (self.config.width as usize).div_ceil(2),
        };
        let aus = self.fc.encode(&frame, pts, force_keyframe)?;
        let out = aus
            .into_iter()
            .map(|(data, keyframe)| EncodedFrame {
                seq,
                keyframe,
                data: Bytes::from(data),
                capture_us,
                encoded_us: now_us(),
            })
            .collect();
        self.last_encode_us = now_us().saturating_sub(t0);
        Ok(out)
    }
}

#[cfg(test)]
#[cfg(feature = "ffmpeg")]
mod tests {
    use crate::decode::{luma, Decoder, HwDecode};
    use crate::encode::{Encoder, EncoderBackend, EncoderConfig};
    use crate::frame::{PixelFormat, RawFrame};
    use crate::pattern::{read_marker, stamp_marker};

    fn frame(seq: u32, w: u32, h: u32) -> RawFrame {
        let stride = w as usize * 4;
        let mut data = vec![0u8; stride * h as usize];
        for (i, px) in data.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let (x, y) = (i % w as usize, i / w as usize);
            px[0] = (x * 2 + seq as usize) as u8;
            px[1] = (y * 3) as u8;
            px[2] = ((x ^ y) & 0xff) as u8;
            px[3] = 255;
        }
        stamp_marker(&mut data, stride, w as usize, h as usize, seq);
        RawFrame {
            seq: seq as u64,
            width: w,
            height: h,
            stride,
            format: PixelFormat::Bgrx,
            data,
            capture_us: 1_000 + seq as u64 * 16_667,
            dmabuf: None,
        }
    }

    /// FilmCraft output through the real `Encoder` path decodes with FFmpeg,
    /// keeps the frame marker, and honours a keyframe request.
    #[test]
    fn filmcraft_roundtrip_with_ffmpeg_decoder() {
        crate::init().unwrap();
        let (w, h) = (960, 544);
        let mut enc = Encoder::new(EncoderConfig {
            width: w,
            height: h,
            backend: EncoderBackend::Filmcraft,
            bitrate_kbps: 10_000,
            ..EncoderConfig::default()
        })
        .unwrap();
        assert_eq!(enc.name(), super::NAME);
        assert!(!enc.accepts_dmabuf());
        let mut dec = Decoder::new(HwDecode::None).unwrap();
        let mut decoded = 0;
        for seq in 0..12u32 {
            let force = seq == 7;
            let out = enc.encode(&frame(seq, w, h), force).unwrap();
            assert_eq!(out.len(), 1, "one access unit per frame");
            if seq == 0 || force {
                assert!(out[0].keyframe, "frame {seq} should be a keyframe");
            } else {
                assert!(!out[0].keyframe, "frame {seq} should not be a keyframe");
            }
            if let Some(pic) = dec.decode(&out[0].data).unwrap() {
                let (y, stride, pw, ph) = luma(pic);
                assert_eq!((pw, ph), (w as usize, h as usize));
                assert_eq!(read_marker(y, stride, pw, ph), Some(seq));
                decoded += 1;
            }
        }
        assert!(decoded >= 10, "decoded only {decoded} of 12 frames");
        // Repeats re-encode the last converted picture.
        let rep = enc.encode_repeat(12, 1_000 + 12 * 16_667, false).unwrap();
        assert_eq!(rep.len(), 1);
    }

    #[test]
    fn slices_are_bounded() {
        assert_eq!(super::default_slices(16), 1);
        let s = super::default_slices(1080);
        assert!((1..=8).contains(&s));
    }

    fn y_psnr(raw: &RawFrame, y: &[u8], stride: usize) -> f64 {
        // BT.601 limited range, as swscale's default BGRx→YUV420P.
        let (w, h) = (raw.width as usize, raw.height as usize);
        let mut se = 0f64;
        for row in 0..h {
            for col in 0..w {
                let p = &raw.data[row * raw.stride + col * 4..];
                let (b, g, r) = (p[0] as f64, p[1] as f64, p[2] as f64);
                let ry = 16.0 + (65.481 * r + 128.553 * g + 24.966 * b) / 255.0;
                let d = ry - y[row * stride + col] as f64;
                se += d * d;
            }
        }
        let mse = (se / (w * h) as f64).max(1e-9);
        10.0 * (255.0f64 * 255.0 / mse).log10()
    }

    /// Side-by-side encode benchmark (release build, not run by default):
    /// `cargo test --release -p mirrorwire-media --features filmcraft -- --ignored bench_ --nocapture`
    /// Size via MIRRORWIRE_BENCH_SIZE=WxH (default 1920x1080), bitrate via
    /// MIRRORWIRE_BENCH_KBPS (default 15000).
    #[test]
    #[ignore]
    fn bench_filmcraft_vs_x264() {
        crate::init().unwrap();
        let (w, h) = std::env::var("MIRRORWIRE_BENCH_SIZE")
            .ok()
            .and_then(|s| crate::encode::parse_size(&s).ok())
            .unwrap_or((1920, 1080));
        let kbps = std::env::var("MIRRORWIRE_BENCH_KBPS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(15_000);
        let frames: Vec<RawFrame> = (0..120).map(|i| frame(i, w, h)).collect();
        for backend in [EncoderBackend::X264, EncoderBackend::Filmcraft] {
            let mut enc = Encoder::new(EncoderConfig {
                width: w,
                height: h,
                backend,
                bitrate_kbps: kbps,
                ..EncoderConfig::default()
            })
            .unwrap();
            let mut dec = Decoder::new(HwDecode::None).unwrap();
            let (mut enc_ms, mut bytes, mut psnr, mut n) = (Vec::new(), 0usize, 0f64, 0usize);
            for (i, f) in frames.iter().enumerate() {
                let out = enc.encode(f, false).unwrap();
                enc_ms.push(enc.last_breakdown_ms().3);
                for au in &out {
                    bytes += au.data.len();
                    if let Some(pic) = dec.decode(&au.data).unwrap() {
                        if i % 10 == 0 {
                            let (y, stride, _, _) = luma(pic);
                            psnr += y_psnr(f, y, stride);
                            n += 1;
                        }
                    }
                }
            }
            enc_ms.sort_by(f64::total_cmp);
            let mean = enc_ms.iter().sum::<f64>() / enc_ms.len() as f64;
            let p95 = enc_ms[enc_ms.len() * 95 / 100];
            println!(
                "BENCH {:<18} {w}x{h} {kbps} kbit/s: encode mean {mean:.2} ms p95 {p95:.2} ms ({:.0} fps max)  {:.2} Mbit/s at 60 fps  Y-PSNR {:.2} dB",
                enc.name(),
                1000.0 / mean,
                bytes as f64 * 8.0 * 60.0 / frames.len() as f64 / 1e6,
                psnr / n.max(1) as f64
            );
        }
    }
}

/// Pure-Rust round trip (no FFmpeg anywhere): convert.rs -> FilmCraft encoder -> FilmCraft decoder.
#[cfg(test)]
mod pure_tests {
    use crate::decode::VideoDecoder;
    use crate::encode::{Encoder, EncoderBackend, EncoderConfig};
    use crate::film_decode::FilmDecoder;
    use crate::frame::{PixelFormat, RawFrame};
    use crate::pattern::{read_marker, stamp_marker};

    fn frame(seq: u32, w: u32, h: u32) -> RawFrame {
        let stride = w as usize * 4;
        let mut data = vec![90u8; stride * h as usize];
        stamp_marker(&mut data, stride, w as usize, h as usize, seq);
        RawFrame {
            seq: seq as u64,
            width: w,
            height: h,
            stride,
            format: PixelFormat::Bgrx,
            data,
            capture_us: 1_000 + seq as u64 * 16_667,
            dmabuf: None,
        }
    }

    #[test]
    fn roundtrip_without_ffmpeg() {
        let (w, h) = (960, 544);
        let mut enc = Encoder::new(EncoderConfig {
            width: w,
            height: h,
            backend: EncoderBackend::Filmcraft,
            bitrate_kbps: 8_000,
            ..EncoderConfig::default()
        })
        .unwrap();
        let mut dec = FilmDecoder::new("test");
        let mut seen = 0;
        for seq in 0..8u32 {
            for au in enc.encode(&frame(seq, w, h), false).unwrap() {
                if let Some(img) = dec.decode_yuv(&au.data).unwrap() {
                    assert_eq!((img.width, img.height), (w, h));
                    assert_eq!(
                        read_marker(&img.y, img.y_stride, w as usize, h as usize),
                        Some(seq)
                    );
                    seen += 1;
                }
            }
        }
        assert!(seen >= 6, "decoded only {seen} of 8");
        // A different capture size is a readable error, not a panic.
        let err = enc.encode(&frame(9, 480, 272), false).unwrap_err();
        assert!(format!("{err}").contains("does not scale"));
    }
}
