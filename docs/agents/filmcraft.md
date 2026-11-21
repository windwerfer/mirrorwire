# FilmCraft in MirrorWire

FilmCraft (storytold/filmcraft and storytold/craft-libs, the owner's project) changes quickly.
This file says where MirrorWire touches it, how to upgrade, and what we measured, so an upgrade
can be judged against known numbers.

## 1. Where FilmCraft is used

| Use | Crate | Pin | Notes |
|---|---|---|---|
| H.264 **software decoder** (fallback when no hardware decoder works; macOS keeps FFmpeg) | `mirrorwire-film` (all API calls), used by `mirrorwire-media/src/film_decode.rs` (desktop, maps to `YuvImage`) and `mirrorwire-android` (re-export `film`) | `craft-video` (`h264-decode`) @ craft-libs `068e079c` | Decision log 2026-11-15 |
| H.264 **encoder** (optional, `--features filmcraft`, `--encoder filmcraft`) | `mirrorwire-media/src/filmcraft.rs` | `craft-video` (`h264-encode`, `threads`) @ craft-libs `068e079c`, pinned in `mirrorwire-media/Cargo.toml` | D23 (was `filmcraft-h264enc` @ filmcraft `cb9783ba` until 2026-11-21) |
| H.264 **hardware decoder**: planned, not built | will go into `mirrorwire-film` | — | §4 |

`mirrorwire-film` exposes only `FilmDecoder::decode(&[u8]) -> Result<Option<Planes>>` (tight
I420) and `Planes`. Nothing else in the workspace names a `craft_video` type, so an API change
upstream is fixed in that one crate.

## 2. Upgrading

1. Bump the craft-libs rev in `crates/mirrorwire-film/Cargo.toml` (decoder) **and**
   `crates/mirrorwire-media/Cargo.toml` (encoder) to the same commit, then
   `cargo update -p craft-video` in `fuzz/` (own lock file). Cargo.lock must list one
   `craft-video`/`craft-media` (`grep -A2 'name = "craft-' Cargo.lock`).
2. Fix `crates/mirrorwire-film/src/lib.rs` if the API moved. Keep: one worker thread (no output
   delay), panics caught, tight planes.
3. Gates (AGENTS.md §3), including `cargo ndk -t arm64-v8a clippy -p mirrorwire-android`.
4. Re-measure with the same recording and compare with §3:
   ```sh
   MIRRORWIRE_FILM_BENCH=rec.h264 MIRRORWIRE_FILM_YUV=out.yuv \
     cargo test --release -p mirrorwire-film --test bench -- --ignored --nocapture
   ffmpeg -v error -i rec.h264 -fps_mode passthrough -f rawvideo -pix_fmt yuv420p - | md5sum
   md5sum out.yuv     # must match: bit-exact with FFmpeg
   ```
   Expect: every access unit gives one picture ("0 calls without a picture"), 0 errors, and a P
   time no worse than §3. Then the loopback with `--software-decode` and, for Android, the
   tablet with the home-screen **Software decoder (FilmCraft)** switch.
5. Encoder: `cargo test -p mirrorwire-media --no-default-features --features filmcraft`
   (pure-Rust round trip) and, with FFmpeg, the `bench_filmcraft_vs_x264` bench
   ([testing.md](testing.md) §3.1).
6. Record the new numbers in §3 (date, rev) in the same commit.

The test recording is not in the repository (130 MB). Make one by teeing the presenter's
Annex B output to a file; the 2026-11-15 one was `h264_vaapi` 1080p60, 2804 access units,
63 IDRs, including back-to-back IDRs with `idr_pic_id 0` after encoder reopens (the case
that froze `c2.android.avc.decoder`).

## 3. Measurements

### 3.1 Software decoder, craft-libs `09dcbc22` (2026-11-15)

Same 2804-AU recording, one access unit per call, single thread.

| Machine | Result |
|---|---|
| Build box (x86-64), `mirrorwire-film` bench | 2804/2804 pictures, 0 errors, no output delay, **bit-exact with FFmpeg** (also on two simulated-loss variants). IDR 49.4 ms mean (p95 69.0), P 15.7 ms mean (p50 13.6, p95 26.3) |
| Build box, loopback 1080p60 (x264) | decode 15.9 ms, capture→decoded 36 ms. FFmpeg software: 4.2 ms / 19.8 ms. FFmpeg VAAPI: 8.9 ms / 27 ms. FilmCraft has no SIMD yet |
| Lenovo TB336FU (MT8755, Cortex-A76), app switch forced | decode 14–16 ms, 38–48 fps, live through 24 presenter encoder reopens, 0 errors. 14.8 ms per P-frame in the offline bench vs 80–155 ms for `c2.android.avc.decoder` |

### 3.1a Encoder and decoder move to craft-libs `068e079c` (2026-11-21, D23)

Research container (4 vCPU VM, no GPU, no FFmpeg), release builds, same inputs old vs new.

- Decoder, `mirrorwire-film` bench on the TB336FU Miracast recording `stream-run4d.h264`
  (1080p, 565 AUs, 5 IDR): 565/565 pictures, 0 errors, no output delay with both revs; the I420
  output is **byte-identical** between `09dcbc22` and `068e079c` (md5 `fa89e711…`; no `ffmpeg`
  CLI here for the FFmpeg comparison). IDR 38.3 → 40.5 ms mean, P 8.80 → 8.82 ms mean (noise).
- Encoder, 1080p synthetic moving pattern, 120 frames, CBR 15 Mbit/s, `Encoder::encode` incl.
  BGRx→I420 conversion: `filmcraft-h264enc` @ `cb9783ba` 63.2 / 64.0 ms mean (two runs),
  `craft-video` @ `068e079c` with `threads` 64.2 / 62.6 ms, same output size (15.04 Mbit/s at
  60 fps); without `threads` 121.7 ms (slices encoded on one thread), hence the feature.
  The x264 comparison bench and the loopback need FFmpeg: not run here.

### 3.2 Hardware decoder via VA-API, filmcraft `cb9783ba` (2026-11-15, prototype only)

`filmcraft-platform::vaapi::VaDecoder` (libva loaded at run time) set up with
`NalStreamInfo::from_avcc`, samples converted from Annex B to 4-byte length prefixes, one
access unit per call; build box, Intel iHD driver in the dev container.

- 2804/2804 pictures, 0 errors, **bit-exact with FFmpeg's software decoder**.
- 6.8 ms per access unit (p95 9.2) including the read-back to planar YUV; no output delay.
- For comparison: our FFmpeg VAAPI path measured 8.9 ms in the loopback (different setup).
  The `ffmpeg` CLI with `-hwaccel vaapi` + `hwdownload` reaches 3.3 ms per frame, but that is
  pipelined throughput, not per-call latency.
- Cost: `filmcraft-platform` depends on FilmCraft's editor stack and adds ~50 crates (symphonia
  audio, captions, project, render, ProRes, DNG, Matroska, …); cold release build 6 min 40 s.
  The VA-API code itself is ~1.6k lines and needs only `craft-video`'s parsers.

## 4. Planned: FilmCraft hardware decoders (roadmap item)

Goal (owner, 2026-11-15): hardware decoding where possible, pure Rust except the OS driver
FFI, no FFmpeg in the viewer. Steps:

1. Upstream (owner): move the hardware decoders (VA-API, VideoToolbox, Media Foundation, NVDEC)
   out of `filmcraft-platform` into a small crate without the editor dependencies, e.g.
   `craft-video` features or a `craft-hwdec` crate in craft-libs. Tracked upstream in
   [filmcraft #2759](https://github.com/storytold/filmcraft/issues/2759) (split
   `filmcraft-platform`; the owner commented with our VA-API numbers) and a craft-libs
   feature request for a hardware-decoder crate (owner opened it 2026-11-15). Check both
   before starting; until one lands there is no light crate to pull.
2. Here: add it to `mirrorwire-film` as `HwDecoder` with the same `Planes` output (later a GPU
   surface to skip the read-back), plus a `VideoDecoder` impl in `mirrorwire-media`.
   `decode::open_decoder` already picks hardware first and falls back to FilmCraft software,
   so only that function and the `--software-decoder`/hwdec options change. Android keeps
   MediaCodec (already hardware, no FFmpeg).
3. Re-run §2 step 4 with the hardware decoder and compare with §3.2; then drop FFmpeg from the
   viewer (macOS last, after a VideoToolbox test on the Mac).

The presenter keeps FFmpeg for encoding: FilmCraft has no VA-API encoder (VideoToolbox and
NVENC only), and the zero-copy VPP path and x264 go through FFmpeg too.
