# Roadmap, open issues, known debt

Status words: **done** (measured on target hardware), **built** (compiles/CI, untested on
target), **planned**, **idea**. Update this file when an item moves.

## State (2026-11)

| Area | State |
|---|---|
| Loopback pipeline (test pattern → x264 → QUIC → decode) | done on a VM (≈9 ms capture→decoded mean at 2000×1000@60) |
| Portal + PipeWire capture on Fedora KDE | done on the T480s (first real stream works) |
| VAAPI encode on UHD 620 | works on the T480s; was on `cpu+vaapi-upload` at 6–10 fps (photos 2026-11-12) |
| DMA-BUF zero-copy capture → VAAPI | built (2026-11-13), untested on the T480s; shm fallback built |
| Shared-memory VPP path | built: the VPP graph got pixel format "bgrz" (invalid for FFmpeg) so every BGRx frame fell back to the CPU; fixed, untested on hardware |
| Viewer on the iMac (OpenGL, DMG x86_64) | works on the target iMac |
| Pairing, pinned TLS, mDNS | works between the target machines (after opening 47800/udp) |
| Flatpak, AppImage, DMGs in CI | built; Flatpak and x86_64 DMG run on target |
| Android viewer APK (arm64-v8a, MediaCodec) | built (2026-11-13); first device test 2026-11-14: connects, cursor fine, picture froze after the first frame. Fixed 2026-11-14 (decode-queue flush spiral, late pictures, hardware decoder choice, pipeline HUD), tested on the x86_64 emulator only; device retest pending |
| Browser (WebAssembly) viewer | design sketch in Planned and Future, not started |
| Camera QR scan in the desktop viewer | decoder (`pairing::scan_qr_luma`) + camera source (portal Camera + PipeWire, raw formats) + "Scan QR with camera" button built 2026-11-19, never run on a real camera (hardware check open; MJPEG-only cameras unsupported); Android: the phone's camera app opens the `mirrorwire://` link |
| Cursor sent as coordinates (portal METADATA cursor, pointer datagrams, viewer-drawn) | built (2026-11-13), loopback-tested with a synthetic cursor; untested with KWin on the T480s |
| 60 fps / max frame-rate setting, negotiated rate shown | built (2026-11-13); KWin's reported max untested |
| Click-freeze in the viewer (egui nested input lock) | fixed (2026-11-13), untested on the iMac |
| Forward error correction for video (Reed-Solomon per block, interleaved, adaptive from viewer loss reports; `--fec auto|off|N`, Quality panel) | built (2026-11-13), loopback-tested with simulated random and bursty loss (protocol.md §3b); untested on real Wi-Fi and on Android hardware |
| Adaptive bitrate (AIMD on viewer loss + one-way queueing delay, FEC parity inside the budget, auto FEC capped at 35 %/60 %; `--adaptive-bitrate`, Quality panel) | built (2026-11-13), loopback-tested with a simulated bottleneck (testing.md §3.2); queue delay tested over real Wi-Fi to the Android tablet (§3.5), not yet T480s → iMac |
| Latency spikes on scene changes (2 s VBV, unbounded QUIC queue, 4-frame decode backlog, no pacing) | fixed (2026-11-13): 100 ms VBV, 2 MiB datagram queue with delta-frame drop, 2-frame decode queue flushed on overflow, 500 Mbit/s pacing; untested on Wi-Fi hardware |

## Next (owner-agreed order)

- **Android MTK tablet: fixed on the device by GPU output (2026-11-15, debug APK, not yet an
  owner build).** The MTK hardware decoder now runs: `hardware decoder c2.mtk.avc.decoder, GPU
  output`, `decode 23.3 ms`, `to RGBA 0.0 ms`, picture age 23–42 ms (software decoder before:
  80–155 ms decode, 100–300 ms age), live content through 24 VAAPI encoder reopens in 2.5 min
  (presenter: container build, h264_vaapi, adaptive bitrate). The plane-1 error was the CPU
  path: the MTK buffers can't be CPU-read; the GPU samples them directly. Findings on the way:
  - With `c2.android.avc.decoder` the picture froze on old content at an encoder reopen while
    `decoded/shown` and a small picture age kept going (decoder output, not the UI). The dumped
    stream decoded fine in FFmpeg. Not fixed there; the software fallback is FilmCraft since
    2026-11-15 (bit-exact on that recording).
  - h264_vaapi writes `idr_pic_id 0` on every IDR, and the stream has IDRs directly after IDRs
    (reopen + keyframe request; 25 in 19 846 pictures), which H.264 7.4.3 forbids. Flushing
    the Android decoder before such an IDR did not cure the software-decoder freeze (13 flushes,
    still frozen), so it was dropped. Fixed 2026-11-17 in the presenter: a keyframe request that
    arrives right after an IDR is dropped (`Controls::take_keyframe_request`); the viewer's
    retry backs off 250 ms → 1 s. Not yet seen on the tablet.
  - Open: `lost`/`unrecoverable frames` climb (433 in ~2.5 min) at 0 % reported loss, with
    `waiting key` stretches of up to 5 s; not looked into yet.
- **Earlier notes: Android MTK tablet, no picture (owner test 2026-11-14, Build 43542749952).**
  HUD: `hardware decoder c2.mtk.avc.decoder (error: plane 1: CodecErrorReclaimed)`,
  `in 12.0 decoded 0.0 shown 0.0`, `key req 212 errors 190`, black screen, the red stall line
  comes and goes every 20–30 s. The previous APK (software `c2.android.avc.decoder`) showed
  the first frame. Found so far:
  - The 3-failures fallback never fired: errors from `Decoder::poll()` (late pictures) were not
    counted, and every successful input call reset the count. Fixed in the commit after this
    one: failures from `decode` and `poll` count together, and only a delivered picture clears
    them, so the next decoder (in the end `c2.android.avc.decoder`) takes over with the reason
    in the HUD. Unverified on the tablet.
  - The text comes from `codec.rs` `convert()`: `AImage_getPlaneData(1)` (plane 0 succeeded)
    returned status 1101, which the `ndk` crate names `CodecErrorReclaimed`
    (`AMEDIACODEC_ERROR_RECLAIMED`). In AOSP's `NdkImage.cpp` neither `lockImage` nor
    `getPlaneData` returns 1101, so it is probably a vendor (MTK) value or a mislabelled code,
    not a real resource-manager reclaim. Unknown: the AImage format/usage the MTK decoder
    delivers to a YUV_420_888 reader (maybe a private/compressed layout that cannot be CPU-read).
  - Leak check (code reading only): a skipped decoder's `Active` (codec, reader, window) is
    dropped once before the next opens; reopen happens only after a skip; images are dropped
    after conversion. No leak found, not measured on a device.
  - Owner test 2026-11-14, Build 43570349073 (release-signed, fallback fix): the fallback
    works. HUD: `SOFTWARE decoder c2.android.avc.decoder (OMX.MTK.VIDEO.DECODER.AVC failed:
    plane 1: CodecErrorReclaimed)` (so the OMX MTK decoder hits the same plane-1 error),
    1920x1080, `11.9 fps`, `decode 155.4 ms`, `to RGBA 14.5 ms`, `lost 17`, `in 11.9 decoded 11.9
    shown 11.9`, `key req 11 flushes 0 input full 0 errors 6`, `fec +12% loss 0.0%
    unrecoverable frames 17`, presenter `h264_vaapi` 6.46 Mbit/s. Picture: the first frame shows,
    then it is a still image; with a video behind it a few tiles flip colour back and forth;
    the cursor (separate overlay) moves but with huge latency. Leads: 155 ms software decode
    caps the tablet at ~6 fps of real work, so the queue drops frames; 17 unrecoverable frames
    with 0 % network loss point at frames dropped *before* decode (lost references), so P-frames
    decode against a stale picture (flipping tiles) until a keyframe that apparently never
    lands cleanly. Check whether queue drops skip frames the decoder needs, and request a
    keyframe instead of feeding P-frames after a drop.
  - Owner note: the first Android APK decoded in ~10–40 ms (~33 ms typical), now 155 ms.
    Unclear whether that build really used hardware decode (it may have, despite the name), or
    whether the software decoder got slower (queue/conversion changes since `d4e9856`/`7a8d2e8`).
    Compare: put the first APK's decoder name + decode time next to the current one.
  - Owner hypothesis: the 155 ms may not be real decode work at all; the HUD says
    `decoded 11.9 shown 11.9` but nothing on screen changes. Candidates: (a) frames dropped or
    decoded incompletely (lost references); (b) output pictures never reach the texture (the
    draw/upload path keeps showing the first picture, stale buffer, wrong stride/plane);
    (c) the timing measures waiting, not decoding. Check first: hash or sample a few pixels of
    each converted RGBA frame; if they change, the bug is in drawing, if not, in decode/queue.
  - Next steps: on the tablet, read `adb logcat` around the error (format, usage, plane strides);
    if hardware decode keeps failing CPU reads, render the decoder to a `SurfaceTexture`/
    `AHardwareBuffer` → GL texture (roadmap "Android viewer polish") instead of the CPU path;
    add a "no picture within N s" timeout per decoder, not only the error count.

0. **TOP PRIORITY: reliable, likable transport (owner, 2026-11-14).** The prototype only
   continues if streaming over real Wi-Fi is stable and pleasant. Do these first, in order:
   1. **Explain the adaptive-bitrate cuts: done (2026-11-15).** Every cut is logged with its
      trigger and values, and the presenter status shows `last cut: <reason>`. The closed-loop
      simulation is in `rate.rs` (testing.md §3.3). The "~17 of 40 Mbit" was never a cut: the
      adaptive commit also removed x264's HRD filler, so x264 sends what the test pattern needs
      (~15 Mbit/s, the same with adaptive off). Re-measured with 0 cuts (testing.md §3.2).
      Found along the way: 5 % random loss costs three cuts before the probe learns it (cuts
      continue during the probe); a bottleneck is found only by overrunning it every ~8 s.
      **Tablet over real Wi-Fi (2026-11-15, testing.md §3.4):** the climbing `unrecoverable
      frames` were the presenter overrunning a ~20–25 Mbit/s link (at connect, and after the
      budget drifted far above what was sent), amplified by 4–8 forced IDRs a second. Fixed:
      slow start from 10 Mbit/s, the budget grows only while ≥ 80 % used, at most one forced
      keyframe per 2 × RTT. 66–198 → 3–41 unrecoverable frames per minute. The rest is item 2.
   2. **Delay signal: done (2026-11-15).** The viewer measures each frame's one-way queueing
      delay (first packet, `sent_us` vs arrival, minus the 30 s minimum; protocol.md §3c) and
      sends mean, max and trend in `link_report` (a required `queue` object since protocol 2,
      2026-11-16, when the RTT fallback for older viewers was removed). The presenter cuts on one report of a building or standing queue, to ≤ 90 %
      of what was sent, and treats loss with a flat queue as random. Tablet over Wi-Fi
      (testing.md §3.5): queue p95 −35 to −58 % in 4 of 4 runs for ~7 % less video; lost
      frames not separable from noise in 4 min. Simulation: 5 % random loss 3 → 0 cuts.
      Found on the way, still open:
      - **Viewer silent = congestion?** One run lost 121 frames in a Wi-Fi outage: no report for
        2.4 s, QUIC lost 2606 packets. Nothing reacted until the link came back. Partly done
        (2026-11-16): after 1.5 s without any message (the viewer pings every 250 ms) the
        presenter pauses the video, the window says the viewer is not responding, and the
        next message gets a keyframe. Found on the tablet: a force-stopped Android app sends no
        close, so before this the presenter streamed into the void until QUIC's 10 s idle
        timeout. Tablet (USB): paused 1.5 s after the force-stop, closed 8.6 s later, the next
        viewer got video at once. Not seen on hardware: a viewer that comes back within the
        10 s (needs a real outage). Still open: whether the rate should also cut then.
      - The remaining overruns come from the budget sitting far above what is sent (27.5 vs
        10–15 Mbit/s), so a content burst hits the link at once: item 4.
      - **Shallow bottleneck buffers: done (2026-11-16).** With ≤ 50 ms (a router or USB link)
        the queue never reached the 40 ms that marks loss as congestion, so the budget climbed
        past the link until loss was heavy. Now near the budget that failed last, loss counts
        and the climb is slow (item 4). Sim: 4.9–5.5 → 0.7–1.2 % lost (testing.md §3.3).
   3. **Keyframes, VAAPI reopens, keyframe pacing: done in the simulation (2026-11-16).** The
      closed-loop sim has keyframes (5×, measured), VAAPI-style reopens and a "frozen frames"
      metric (testing.md §3.3, means of 10 runs). Ignoring loss after a keyframe was rejected
      (an upward reopen's keyframe hid its overrun). Built: a decrease reopens VAAPI after 1 s
      (≥ 10 %; increases still every 5 s), and the send loop paces a keyframe at 1.25 × the
      budget and the frames behind it at 1.5 ×, ≤ 100 ms each. Simulated VAAPI frozen frames,
      burst → paced: 16 Mbit/s with a 300 ms buffer 100 → 0, 50 ms 3123 → 291, 20 ms 4799 → 759,
      20 Mbit/s / 150 ms 294 → 74; x264 behind a 20 ms buffer never got a picture, now 0. Cost:
      shallow-buffer lag p95 34 → 82 ms. Real-hardware results: testing.md §3.6. Still open:
      VAAPI behind a 20–50 ms buffer still freezes 6–16 % of the time (item 6 removes the reopen
      keyframes); parity is spread with the keyframe, not over later frames.
   4. **Hysteresis and probing:** remember the last good budget, climb slowly near it, probe with
      extra FEC before more video, start from the last good value per peer. ~100–150 LOC.
      (Done 2026-11-15: slow start from 10 Mbit/s and no growth while unused. Done 2026-11-16:
      each cut remembers what the link did not carry; from 90 % to 115 % of it the climb is a
      quarter as fast and flat-queue loss counts as congestion (testing.md §3.3). Done
      2026-11-16: a reconnecting viewer (fingerprint + address, 10 min, in memory, not on disk:
      a stale capacity would overrun the link at connect) starts at min(budget it left with,
      60 % of its last failure) and slow-starts to 90 % of it; sim: shallow-buffer and < 10
      Mbit/s links freeze far less in the first 20 s, VAAPI 20 Mbit/s / 150 ms and a link that
      got worse a little more (testing.md §3.3). Not yet seen on hardware: the USB test link
      never cut, so no resume fired. Done 2026-11-17: probing with FEC. From 90 % to 115 % of
      the last failure the budget grows by extra parity first (up to 3 % above the video), and
      the video follows after 3 s clean; keyframes are paced on the video budget. Sim: 2889 →
      2154 frozen frames and 199 → 117 cuts over all paced/reconnect cases, same video;
      VAAPI 16/50 291 → 107, 16/20 759 → 624; cost: VAAPI 16/300 6 % less video. About a
      third of the gain is the parity, the rest the held climb (testing.md §3.3). Tablet over
      weak 2.4 GHz Wi-Fi (testing.md §3.8): 15 probes, 8 clean, 6 ended by a queue cut, none
      by loss; A/B totals better but within the link's own swings. Closer to the router
      (−60 dBm): 14 probes, 13 clean; QUIC lost 662 → 3 packets, lost frames 28 → 0 on the
      two pairs that cut, 4 % less video. The window says
      `probing: …` while one runs, the log has one line per probe start and end. Note for VAAPI: slow start changes the rate each
      report, but VAAPI reopens at most every 5 s, so it reaches 40 Mbit/s in steps; untested
      on the T480s.)
   5. **Don't cut when FEC recovered everything; smooth the loss: tried and rejected in the
      simulation (2026-11-16, testing.md §3.3).** Skipping loss cuts while every frame was rebuilt
      lets the budget sit above a shallow-buffer link (16/50: loss 0.72 → 5.42 %, VAAPI frozen
      frames 291 → 401); limited to outside the hysteresis zone it changes almost nothing, since
      flat-queue loss already counts as random. Feeding rate control the smoothed FEC loss keeps
      cutting after the loss ends (x264 16/50: 0 → 25 frozen). Fewer VAAPI reopens have to come
      from item 6 instead. Open: real-hardware cut logs could still show recovered-loss cuts the
      sim does not have (the sim pools parity per frame, real FEC is per block).
   6. **Live VAAPI bitrate without a reopen**, removing the reopen limits (up 5 s, down 1 s since
      2026-11-16) and their keyframes. **Waits for FilmCraft's codec in its own crate** (owner,
      2026-11-16). Not possible through FFmpeg: FFmpeg 7.1 `vaapi_encode.c` sends the rate-control
      and HRD parameters (incl. qmin/qmax, so no QP clamp either) only with an IDR (lines 349–354),
      from a private context computed once at open. Glass is no reference here: its GStreamer VA
      encoder also reconfigures with a keyframe. Other VA-API encoder crates (cros-codecs) were
      rejected by the owner: encoding stays on FFmpeg or FilmCraft. Cost until then (sim, §3.3):
      VAAPI behind a 20–50 ms buffer freezes 6–16 % of the time; deep buffers and x264 are fine.
   7. Small fixes (**done 2026-11-17**): `lan_cc.rs` window grows back after persistent
      congestion (acked bytes, up to 4 MiB; unit test); the presenter window says under the
      encoder row that CQP/ICQ ignore the adaptive bitrate.
   Alongside (**done 2026-11-17**): the proto parsers (fragment, cursor, control, NACK, URI) are
   covered by deterministic hostile-input tests (no cargo-fuzz, see decisions.md) and the
   panicking index/unwrap sites in proto/net are gone, enforced by a crate-level `deny`.
   **Ideas from Glass (owner, 2026-11-16; no code yet).** Glass
   (https://github.com/Ntarasiuk/glass, MIT, reviewed at 4ba9757 / v0.15.0) is a browser-first
   remote desktop over Tailscale with the same video core as ours (H.264, QUIC datagrams, no
   jitter buffer, adaptive bitrate). Not a replacement: no viewer for the 2011 iMac (VideoToolbox
   or browser only), no Android APK, Linux pen/extend/overlay missing, needs Tailscale for
   encryption and sudo for uinput. Worth adopting, by re-implementing in our code (their code is
   bound to their protocol and JS client; if a small piece is copied, keep their MIT notice and
   note it in decisions.md):
   - **NACK resend of lost fragments (done 2026-11-17, protocol 4, protocol.md §3d; loopback
     3 % loss over 20 s: 2 → 0 unrecoverable frames, 2 → 0 keyframe requests; 10 %: 10 → 0 lost
     frames, 12 → 0 requests; not yet on real Wi-Fi).** Original note
     (`crates/glass-host/src/retransmit.rs`, viewer side in
     `web/video-stream.js`): the viewer asks for missing fragments of a frame before giving up,
     the presenter keeps a short send window (Glass: 3 s / 8 MiB, repair budget 128 KiB/s).
     Complements FEC, mainly for large IDRs; ask for a keyframe only if the resend misses its
     deadline (~1 RTT). Needs a protocol bump. Fits item 0 between 3 and 5.
   - **Keyframe request backoff (done 2026-11-17)** 250 ms doubling to 1 s, and keep acking frames while waiting
     for a keyframe so the wait is not read as queueing (`web/video-stream.js`). Extends the
     current one-per-2×RTT limit. Here: `KeyframeBackoff` in the viewer loops, gate 250 ms–1 s on
     the presenter, no forced IDR right after an IDR; "acking" = frames that arrive while
     waiting still count in `link_report` (there is no per-frame ack message).
   - **Frame integrity check before decode (done 2026-11-17, protocol 5, protocol.md §3).** Original note (Glass: Fletcher-16 over the reassembled frame in
     `glass-proto`). QUIC already authenticates each packet, so this only catches our own
     reassembly/FEC bugs; cheap, never feed a mismatching frame to the decoder. Low priority.
   - **Bitrate: frame-ack throughput as a second signal and no raise on a still screen (done
     2026-11-17, protocol 6, protocol.md §3b; sims unchanged, unit-tested, not on hardware)**
     (`crates/glass-host/src/rate.rs`). Choke-point memory is already done (item 4).
   - **Live capture-to-display latency** from the clock offset plus a capture timestamp on each
     frame (`web/worker.js`): feeds the bottleneck line (item 5).
   - **In-place VAAPI bitrate with step/hold limits** (`crates/glass-host/src/linux/va_encode.rs`,
     via GStreamer): reference for item 6.
   - **Decoder ladder** hw-lowlatency, hw, sw (`web/video-stream.js`): matches the Android
     fallback chain; check ours has an explicit low-latency step.
   - Optional: **OpenH264** (BSD) as the Linux software encoder instead of x264, for GPL-free
     builds (D8, item 4 of Next).
   Not adopted: Tailscale/accounts (D4: LAN pairing, no accounts), uinput (D1: portal first).

0b. **Structure improvements (owner, 2026-11-14): after item 0, before widening the scope**
   (bottleneck line, Extend mode, phone input, Mac presenter). Borrowed from PhotoCraft:
   1. **Lints enforced by tooling** (**unsafe_code lint per crate done 2026-11-17**: `forbid` in proto, net, film, xtask; `deny` in app, media, android with `allow` only on the unsafe modules; `clippy.toml` allows unwrap/expect/panic/indexing in tests; unwrap/expect/panic `deny` outside tests also in media, film, android and app 2026-11-17 (lock poisoning via `PoisonError::into_inner`, app uses `sync::LockExt::lock_ok`); indexing_slicing now also in app, media, film and android (2026-11-17; `clippy::undocumented_unsafe_blocks` is a workspace `deny` too since 2026-11-18, so every `unsafe` block in app, android and media has a `// SAFETY:` comment; the four lints now live in `[workspace.lints.clippy]`, each product crate has `[lints] workspace = true`; xtask opts out)) (proto/net `deny(unwrap_used, expect_used, indexing_slicing,
      panic)` outside tests done 2026-11-17; the rest open): `[workspace.lints]` plus `clippy.toml` (unwrap/expect/panic
      allowed in tests only); `#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic,
      clippy::indexing_slicing)]` in proto and net; `forbid(unsafe_code)` everywhere except media
      and the glow renderer; `clippy::undocumented_unsafe_blocks` with a `// SAFETY:` comment on
      all remaining unsafe sites; first try to remove the `unsafe` in
      portal.rs (reviewed 2026-11-17: `BorrowedFd::borrow_raw` on PipeWire's raw fd has no safe
      alternative, stays, documented; media SAFETY comments + deny lint done; net/diag.rs done: tz-rs, net forbids unsafe; app/applog.rs done 2026-11-17:
      `IsTerminal`, app is `deny(unsafe_code)` except yuv_glow.rs) via safe APIs (D15); lock poisoning via `unwrap_or_else(PoisonError::into_inner)`. ~1 day.
   2. **cargo-fuzz targets** for fragment, fec, cursor and control decode, plus a CI job (if not
      already done alongside item 0). ~½ day. Done 2026-11-18: `fuzz/` crate + `.github/workflows/fuzz.yml` (nightly, D18); the
      2026-11-17 `cargo test` hostile-input tests stay as the stable check.
   3. **A `cargo xtask ci` gate: done 2026-11-17** (`cargo xtask ci [--no-loopback]`, `cargo xtask layers`; std-only `xtask/` crate; the layer check reads each crate's `mirrorwire-*` deps against architecture.md and limits proto's external deps; CI runs `cargo xtask layers`; audit/deny stay manual) (the formatting-only commit and the CI fmt check are done,
      rustfmt defaults, no `rustfmt.toml`): fmt, clippy, tests, a crate-layer check
      (architecture.md made mechanical), loopback test.
   4. **Machine-checked perf budgets: loopback half done 2026-11-17** (`perf-budgets.toml`, `cargo xtask loopback [secs]`, used by `xtask ci` and CI; loose p95 limit plus zero integrity counters; the ABR simulation rows were already asserted by the `sim_*` unit tests in rate.rs, which fail on regression; the 16 Mbit/s FIFO rows stay prose): loopback p95 capture→decoded and the adaptive-bitrate
      simulation rows as TOML budgets checked in CI, instead of prose tables in testing.md.
   5. **Split the big files: first part done 2026-11-17** (the duplicated encoder-config comparison in `maybe_reopen` / `maybe_replace_encoder` is now one `needs_new_encoder`; the two functions themselves stay, their bitrate rules differ; encode: config types moved to `encode/config.rs` and the VAAPI VPP graph + DMA-BUF import to `encode/vaapi.rs` 2026-11-17, rate-control helpers to `encode/rc.rs` 2026-11-17; app: `presenter/` is a module dir, GUI status types in `presenter/status.rs` 2026-11-17, CaptureCtl/CaptureStatus in `presenter/capture.rs` 2026-11-17, PresentArgs/SourceKind in `presenter/args.rs` 2026-11-17; gui: stateless widgets/panels in `gui/widgets.rs`, present screen + quality panel in `gui/present.rs`, video screen in `gui/video.rs` 2026-11-17) (encode.rs, presenter.rs; net's transport is done: `transport/`
      `presenter.rs`, `viewer.rs`, `link.rs` for the FEC/bitrate state) and merge the duplicated encoder-config comparison in
      pipeline.rs (`maybe_reopen` / `maybe_replace_encoder`).
   Already in force: decision records with alternatives (decisions.md) and small commits (AGENTS.md).

0c. **Remove `unsafe` Rust where possible (owner, 2026-11-14; after 0 and 0b, D15). DONE 2026-11-18: realistic goal reached (manager decision, option 2): all unsafe is in mirrorwire-media (plus glow/EGL FFI in the viewers), never in proto or net, every site has a SAFETY comment, the lint is enforced, the ASan job runs in CI. No move-only refactor of the untested VAAPI/PipeWire paths; revisit only if the parked own VA-API encoder replaces FFmpeg.** Go through
   every `unsafe` site (encode.rs, decode.rs, hw.rs, source/portal.rs, yuv_glow.rs, applog.rs;
   net/diag.rs is done) and check for a safe replacement: safe methods of `ffmpeg-next`/`ffmpeg-sys`
   wrappers, `glow`/egui-safe GL paths or a wgpu-only path, safe crates for the libc/socket calls,
   dropping the `unsafe impl Send` by restructuring threads. Remove what can go; for what must
   stay, write down why (per site) and report the list to the owner. Goal: no `unsafe` outside
   `mirrorwire-media`, then `#![forbid(unsafe_code)]` in proto, net and app.
   **Realistic goal (owner-agreed):** not zero `unsafe`, but FFmpeg behind one small,
   well-documented module with a safe API; `unsafe` removed from the GUI's OpenGL code
   (yuv_glow.rs), logging (applog.rs) and network diagnostics (net/diag.rs). Reason: the iMac
   has no hardware H.264 decoder, and its ~11 ms software decode at 2560×1440 is FFmpeg's
   hand-optimised C/asm decoder, reachable from Rust only through FFI (hardware decode would
   need FFI too). No known pure-Rust H.264 decoder is fast enough; one written from scratch
   would likely be several times slower and break the latency budget. Best practice for that FFI layer: one small module with a safe API,
   a `// SAFETY:` invariant on every block, `clippy::undocumented_unsafe_blocks`, a CI job running
   the media tests under AddressSanitizer (done 2026-11-18, fuzz.yml, D18), and fuzzing the decode path with corrupt H.264 input
   (a `cargo test` version is done 2026-11-17: `decode::tests::corrupt_h264_never_panics_and_decoder_recovers`; the `VideoDecoder` trait with our own `YuvImage` also exists).
   Part of the same safe API: a `Decoder` trait with our own frame type (today `decode.rs`
   returns `ffmpeg_next::frame::Video`, and there is only one FFmpeg-based decoder), so other
   decoders (VideoToolbox direct, FilmCraft) can be plugged in like the encoder backends. ~1 day.
   (A FilmCraft decoder option is its own item under Planned.)

1. **Quality and latency fix (native size + limits done 2026-11-18: default is the capture's native size; `EncoderConfig::resolved_size` makes it even, keeps the aspect when one of --width/--height is given, and scales anything above 4096 px edge / 4096x2304 area (H.264 level 5.1) down, e.g. 5K -> 4096x2304; unit-tested, not run on a 4K/5K display; the GPU scaling part is open):** stream at the screen's native resolution (no fixed
   2000×1000 scaling), higher default bitrate for desktop text, move colour conversion and
   scaling to the GPU (VAAPI `scale_vaapi`/hw upload) so `capture_to_encoded` drops.
2. **Connection diagnostics in the GUI (done; verified in code 2026-11-18: `net::diag` ConnectTrace with eight steps resolve → first frame, rendered by `step_row` in the desktop viewer and as ✓/✗ lines with advice on Android, unit-tested in diag.rs; photo-based check on hardware still the owner's):** per-step status (resolve, handshake, cert, pairing,
   stream) with actionable advice, for photo-based debugging.
3. **FilmCraft encoder comparison:** built (feature `filmcraft`, `--encoder filmcraft`, GUI
   selector; see decisions.md log and testing.md §3.1). Still open: the owner's side-by-side on
   the T480s (photos of the presenter status with each encoder), a real force-IDR / intra
   refresh upstream instead of re-creating the encoder, and switching back to DMA-BUF capture
   when the user returns to VAAPI without restarting.
4. **Optional system FFmpeg (owner decision 2026-11-17, D8; runtime part done: Auto falls back VA-API -> x264, a total failure shows a message with the install command; packaging switched 2026-11-17: Flatpak builds no FFmpeg/x264 and declares the ffmpeg-full extension, AppImage CI links the runner's system FFmpeg dynamically and excludes libav*; neither built on a Flatpak/Ubuntu runner locally, CI result pending; macOS keeps the static build):** ship without x264 and use an installed FFmpeg for VAAPI when the
   user wants it (Flatpak: `org.freedesktop.Platform.ffmpeg-full` extension). Would make Linux
   builds GPL-free (D8).
5. **Bottleneck diagnosis line (viewer HUD half done 2026-11-17: `bottleneck.rs`, classifier + 3-window hysteresis, unit-tested, shown on the HUD from loss, queue delay, decode time, source fps and encoder; thresholds in architecture.md. Presenter status line done 2026-11-17 (`bottleneck` control message, protocol 7; both viewers send it; Android since 2026-11-17, classifier moved to `mirrorwire-proto/src/bottleneck.rs`, untested on a device; the Android HUD shows the hint line since 2026-11-17). Open: render stage; hw/sw encoder hint needs real hardware):** a "Bottleneck: <stage> – <reason> + hint" line on
   the viewer HUD and the presenter status, from the per-stage stats (capture fps vs requested,
   encode time and hw/sw encoder, network loss/RTT/jitter, reassembly, decode time and hw/sw
   decoder, render). Examples: "network – 22 % loss, likely Wi-Fi; cable or 5 GHz helps",
   "presenter – desktop sends only 11 fps", "driver – CPU encoding, no hardware H.264 encoder",
   "viewer – software decode 11 ms, OK". A coarse rating (good / OK / weak), no percentages;
   documented thresholds with hysteresis so it does not flicker. The viewer reports its stats to
   the presenter so both show the same verdict (capability-negotiated, protocol.md update).
   Unit-test the classifier. LOW priority follow-up: "percent of achievable quality/speed, Wi-Fi
   vs cable detection" – unreliable, because the achievable maximum depends on the encoder,
   content and the other traffic on the link, and the OS does not tell us reliably whether the
   path crosses Wi-Fi.
6. **Extend mode / virtual monitor (setting, portal source type, fallback, per-mode token, Quality-panel box and unit tests done 2026-11-17; not tried on a real KDE/GNOME desktop; viewer size in hello done 2026-11-18 (protocol 8, `Presenter::viewer_display`; shown in the presenter window as a "set this resolution" hint 2026-11-18, no automatic sizing; the encode size is capped at the chosen Extend size 2026-11-18: the ScreenCast portal has no size parameter, see D19), Flatpak portal-version check open):** presenter setting Mirror/Extend (CLI, env,
   settings.json, start screen, Quality panel). Extend asks the ScreenCast portal for source
   type VIRTUAL (4) after checking AvailableSourceTypes (KDE Plasma 6 and GNOME mutter support
   it); Mirror asks for MONITOR. Switching mode re-opens the portal dialog; one restore token per
   mode. If VIRTUAL is unavailable, fall back to Mirror with a clear note. Ideally the viewer
   sends its window/display size in hello; otherwise the UI says resolution and position are set
   in the system display settings. Must work in the Flatpak (check the portal version needed).
   Tests, docs.

7a. **Optional FFmpeg (owner, 2026-11-17, D17):** done 2026-11-18. Cargo feature `ffmpeg` (default on); `--no-default-features --features filmcraft` builds a libav-free binary (FilmCraft encode + decode, pure-Rust converters), checked by CI and `cargo xtask ci`. Later: direct VAAPI encode/decode backend (no FFmpeg), Windows/macOS encoders without FFmpeg.

7. **Windows build (owner request 2026-11-17, D16).** Step 1: CI job `Check (Windows)` on
   `windows-latest` (MSVC) builds, clippy-checks and tests the workspace with the viewer
   (`--no-default-features --features gui`; no portal, no PipeWire), FFmpeg from a shared LGPL
   build (BtbN) via `FFMPEG_DIR`. Not yet run on a real Windows machine. Step 2 (**built 2026-11-18**, `Windows viewer` job in build.yml: zip with mirrorwire.exe, LGPL
   FFmpeg DLLs, licence files and a README; `--version` smoke test only, never run on a real
   Windows machine; an FFmpeg-free zip is a later option, decode speed unchecked). Step 3 (later, separate): presenter
   capture via Windows Graphics Capture / DXGI, no admin prompts; encoder via FFmpeg
   (h264_mf/NVENC/QSV/AMF or x264 is not available in LGPL builds, so check). Firewall on
   Windows: `net/firewall.rs` is a no-op there; a message must tell the user to allow the
   port.

## Planned

- **Adaptive bitrate follow-ups:** tune the thresholds on the T480s → iMac Wi-Fi; VAAPI bitrate
  changes **done via encoder reopen (item 0.6 closed 2026-11-18)**: FFmpeg's h264_vaapi only reads the
  rate when opened, so it reopens with a keyframe, rate-limited; a live libva change is not worth its size (see decisions.md); per-frame pacing to the budget for every frame (keyframes and the frames behind them are paced since 2026-11-16; pacing everything at exactly the budget built up seconds of lag in the sim).
- **FEC follow-ups:** measure on real Wi-Fi and tune the auto curve. (Keyframes, parity
  included, are paced over up to 100 ms since 2026-11-16.)
- **Zero-copy (viewer):** VideoToolbox → texture and a YUV→RGB shader on the wgpu path too
  (glow already has the shader). The presenter side (DMA-BUF → VAAPI) is built.
- **DMA-BUF modifiers from VAAPI:** today a fixed list (Intel Y/X-tiled, linear, implicit) is
  offered; query the driver's importable modifiers instead (AMD needs this).
- **Camera QR scanning** in the viewer (the URI is already in the QR). Decoder, camera source and Scan button done 2026-11-19; only the real-camera hardware check remains.
- **Virtual monitor (extend) mode** via the portal's virtual source type.
- **GitHub Releases** for stable download links (release job exists for `v*` tags, never run;
  the owner decides when to tag).
- Flathub (vendored cargo sources), notarized Mac builds (needs a paid Apple account).
- Miracast sink in the viewer: **built** (spike, 2026-11-21, app feature `miracast`,
  `mirrorwire view --miracast`): a Miracast source arrives as a second producer of the
  viewer's `ViewerHandle`; hardware run 6 showed the TB336FU at ~30 fps 1080p through
  FilmCraft. Open: default build (VAAPI) on the owner's box, a home-screen entry, a cancellable
  NM join. See also the "Miracast support" entry below and [miracast.md](miracast.md) §13.
- **Miracast support (Wi-Fi Display, optional MS-MICE)** — status: idea/parked, owner decides.
  Goal: an Android tablet/phone mirrors to the Linux laptop via Miracast (tablet = source,
  laptop = sink), optionally the reverse, as a clean reusable Rust crate.
  - Rootless via NetworkManager D-Bus WifiP2P (no MiracleCast-style root or stopping NM);
    Polkit `allow_active` for the logged-in user. The owner's ThinkPad (Intel 8265) supports P2P.
  - Crate split: `miracast-nm` (NM P2P scan, peers, group formation, WFD IEs), `miracast-rtsp`
    (M1-M7 state machine), `miracast-rtp` (RTP/MPEG-TS, H.264 + audio); a `P2pProvider` trait
    keeps the core platform-agnostic. Sink API first, then Source.
  - Effort comparable to the existing protocol work; mostly doable with traces and mocks,
    hardware only for P2P timing and edge cases.
  - Platforms: Linux primary; Windows via the Wi-Fi Direct API (~1.5-2x work); macOS has no
    practical public Wi-Fi Direct API and AirPlay is not viable.
  - MS-MICE later: P2P discovery, then streaming over LAN (TCP 7250); limited Android
    support, mostly Windows.
  - Next steps if adopted: sink crate API, NM P2P backend with mocks, RTSP M1-M7 from specs and
    traces (clean-room: MiracleCast and gnome-network-displays are GPL, copy nothing), RTP
    receive path, hardware validation with the owner.
  - Open check: whether the TB336FU still offers a Miracast source ("Cast"/"Wireless
    display"); stock Android dropped it, only some vendors ship it.
  - **TODO (status: idea/optional):** Optional root prompt (explicit user click, never
    automatic) to choose the P2P/Wi-Fi channel so that the group channel matches the STA
    channel. Possibly behind statistics that detect frequent disconnects and attribute them to
    a channel mismatch (group freq != STA freq) before offering it. Needs the owner's OK
    (AGENTS.md §1.1 / §5: any privilege). Background: [miracast.md](miracast.md) §12.
- **AirPlay receiver (audio + video + screen mirroring)** — status: idea/parked, owner decides
  (owner 2026-11-21). **DO NOT START.**
  Goal: MirrorWire as an AirPlay receiver; a reusable crate; MIT/Apache where possible.
  - Hard part: FairPlay SAP (`/fp-setup` challenge-response) uses white-box crypto recovered
    from Apple binaries; all working implementations are LGPL/GPL because of that derivation.
    Black-box recovery is impractical; extracting it ourselves would still be a derivative
    work. Never claim clean-room for the FairPlay tables.
  - Clean-roomable from public notes and traces: mDNS discovery, RTSP, pairing (public
    primitives), RTP, video packet headers, framing.
  - Size: FairPlay tables + logic ~200-500 KB; the whole stack stays under ~1.5 MB stripped
    (typically 400-900 KB).
  - Existing work: the `shairplay` crate (LGPL-3.0-or-later, complete pure-Rust receiver);
    `tvpipe` keeps FairPlay (LGPL) separate; UxPlay and RPiPlay are mature C/C++ receivers.
  - Option A: use all of `shairplay` (fastest, but the whole AirPlay stack becomes LGPL).
  - Option B (preferred): FairPlay as a small separate LGPL component, dynamically linked or
    with a clear relink path, inside the same Flatpak if needed; everything else clean-room
    MIT/Apache. Decode H.264/AAC with the existing pipelines (FFmpeg), no own codecs. Scope
    comparable to the Miracast protocol work; mostly doable with traces and mocks.
  - Compliance: LGPL source offer, the user can replace the component, notices. Patents on
    FairPlay are a separate risk.
  - **OWNER DECISION NEEDED BEFORE ANY WORK:** option B conflicts with D20 (clean room, no
    reading or using GPL/LGPL code) and the `deny.toml` allow-list (permissive only). It needs
    an explicit, documented exception scoped to the FairPlay component only. Nobody reads LGPL
    FairPlay code until the owner grants that exception. See the pending note in
    [decisions.md](decisions.md).
  - Note: the earlier line "AirPlay is not viable" (Miracast entry) meant macOS as a Wi-Fi
    Direct platform, not an AirPlay receiver. (The "Not goals" line in decisions.md also covers
    AirPlay receiver/sender; it would need revisiting if this is adopted.)
- **Chromecast sender (mirror the desktop to a Chromecast / Google TV)** — status: idea/parked,
  owner decides (owner 2026-11-21). **DO NOT START** until the owner has a Cast test device;
  he considers it as unlikely as AirPlay.
  - Direction: MirrorWire sends only. Receiving from Android's Cast menu needs Google device
    certificates and is not feasible; Miracast already covers receiving from Android.
  - Easy part: mDNS discovery, TLS channel (port 8009), protobuf CastMessage framing, JSON
    control namespaces.
  - Hard part: live mirroring needs the Cast Streaming protocol (offer/answer, RTP, AES,
    bandwidth adaptation). No official public spec; Google's Open Screen library (Chromium,
    BSD) may be read as documentation under D20 since it is permissively licensed.
  - Effort: about the same as the Miracast work or more, not half. Needs a real Chromecast or
    Google TV for testing; the TB336FU tablet cannot be used.
  - Note: the "Not goals" line in decisions.md may need revisiting if this is adopted.
- **Android viewer polish:** zero-copy MediaCodec → `SurfaceTexture`/`AHardwareBuffer` → GL
  texture instead of the CPU YUV→RGBA copy; soft keyboard for the link field (today: QR via
  the camera app, or the keypad + mDNS list); a stable release keystore so updates install
  over each other (CI debug keys change per run).
- **FilmCraft pure-Rust decoder: built (2026-11-15) as the software fallback except on macOS**
  (decision log). Desktop: `VideoDecoder` trait in `mirrorwire-media/src/decode.rs`,
  `film_decode.rs`, `--software-decoder`. Android: `film.rs`, planes drawn by a GPU shader.
  Measured: tablet (MT8755) with the home-screen switch, VAAPI test-pattern presenter, 1080p:
  decode 14–16 ms, picture age 14–23 ms, 38–48 fps shown, live through 24 encoder reopens in
  ~2 min, 0 decode errors. Loopback on the build box (x264 1080p60): FilmCraft decode
  14–17 ms, capture_to_decoded ~32–35 ms vs FFmpeg 4 ms / ~19–20 ms. Open: FilmCraft has no
  SIMD (desktop cost); a small Cortex-A55 core does ~20 fps, so the Android decode thread
  should stay on a big core (not controlled yet); not tested on Windows or with a real screen
  on the tablet's software path; GUI selector on the desktop not built (CLI only).
- **FilmCraft hardware decoders, no FFmpeg in the viewer (owner 2026-11-15; later).**
  Measured, not built: FilmCraft's VA-API decoder is bit-exact with FFmpeg at 6.8 ms per
  access unit with no output delay, but `filmcraft-platform` adds ~50 crates. Needs the
  hardware decoders split into a small upstream crate first; then they go into
  `mirrorwire-film` behind the existing `VideoDecoder`/`open_decoder` path. Plan, numbers and
  the upgrade checklist: [filmcraft.md](filmcraft.md). The presenter keeps FFmpeg for encoding.
- **Name back to MirrorWire (owner 2026-11-16):** done in the code, packaging and docs (D5
  history in [decisions.md](decisions.md)). Still `mirrorwire`: the GitHub repo
  (`windwerfer/mirrorwire`), the SonarCloud project key (`windwerfer_mirrorwire`) and the dev
  container's workspace path and `mirrorwire_home` volume. They change only when the owner renames
  the repo and the SonarCloud project. Before friends install it: check the name is free
  (Flathub, app stores, trademarks).
- **Android presenter** later (MediaProjection + MediaCodec encode; needs Java/JNI glue).
- **Browser viewer (WebAssembly), sketch:** see also [Future: phone/tablet as input device](#future-phonetablet-as-input-device) for the agreed trust model and input-mode scope.
  - Presenter: add an HTTP/3 WebTransport endpoint on the presenter's own UDP port (any port, including 47800; not 443), e.g. `wtransport` or `h3` + `quinn`. Video goes as WebTransport datagrams with the existing fragment header; control messages on a bidirectional stream. Decode with **WebCodecs**.
  - Trust: browsers reject self-signed certs unless pinned via `serverCertificateHashes` (cert valid ≤ 14 days, ECDSA P-256). The presenter mints a short-lived cert and puts its hash in the QR/URL. The web page itself must come from a normal HTTPS origin (e.g. GitHub Pages); only the WebTransport session goes to the laptop on the LAN. Pairing is bound to the pairing identity, not to one certificate, so pairings survive automatic cert rotation.
  - Page: egui compiled to wasm (or plain JS) + WebCodecs `VideoDecoder` (`avc1.42E0xx`, `optimizeForLatency: true`), drawing `VideoFrame`s onto a canvas.
  - Wire: reuse `mirrorwire-proto` compiled to wasm (it has no I/O by design, see architecture.md §1).
  - The viewer and the input modes in the Future section can all run in the browser. The presenter stays a native app.


## Future: phone/tablet as input device

**Status: idea.** Parked until screen mirroring is solid and the owner sends a UI
sketch. Do not start this work until then.

The phone or tablet already shows the laptop screen as a MirrorWire viewer.
These modes add a reverse input channel so the same device can control the
presenter: a PowerPoint remote, a laser pointer, a drawing overlay, a
touchpad, and (later) a Wacom-style pen tablet. They all share one foundation.

### Foundation

Build the reverse input channel first, over the existing paired, encrypted QUIC
connection (do not switch to TCP).

- Unreliable datagrams for pointer and pen samples (high rate; loss is acceptable).
  **Reuse the cursor path:** the presenter→viewer pointer datagram
  (`mirrorwire-proto/src/cursor.rs`, protocol.md §3a) already has a `tool` byte and accepts
  trailing fields, so pen pressure/tilt/buttons are appended there (and the same message goes
  viewer→presenter for input), with `cursor_rect()` inverted to map tablet touches back to
  presenter pixels.
- Reliable stream for pen-down / pen-up, key events, and other edges, so a
  stroke or a key press never gets stuck.
- Virtual input on the Linux presenter: keys and the pointer go through the
  xdg-desktop-portal `RemoteDesktop` portal. Pen pressure uses `/dev/uinput`
  only as the opt-in of D26 (a udev `uaccess` rule the user adds, plus
  `--device=all` as a user override in the Flatpak); no privileged helper.

### Modes (rough sizes)

| Mode | Notes | Rough size |
|---|---|---|
| Presenter remote (**built 2026-11-18**, untested on hardware: protocol 9 `remote_key`, RemoteDesktop portal, buttons on desktop and Android viewers) | Next, previous, and click as arrow / space keys. PowerPoint, LibreOffice Impress, and PDF viewers already understand these. | ~300–500 LOC |
| Pen/touch as absolute pointer (**built 2026-11-18**, untested on hardware: protocol 10 `pointer`, RemoteDesktop pointer + MONITOR stream, "Pen" toggle on desktop and Android viewers; one RemoteDesktop session for capture + input since 2026-11-18, so Extend works; the separate fallback session (Mirror-only pointer, keys) runs on the portal runtime since 2026-11-25 and nothing waits for its permission dialog) | Pen or finger down = left button, drag = drag. No pressure/tilt. | ~400 LOC |
| Pen pressure (**built 2026-11-22**, untested on hardware: protocol 11 `pointer.pressure`, opt-in uinput pen per D26, "Pen pressure" section with system check + fix panel on the Linux presenter) | Pressure only; no tilt, hover or eraser yet. Open: does KWin map "MirrorWire virtual pen" onto the shared screen by default (several screens, Extend); real Android pressure values; switch to libei `ei_stylus` through the RemoteDesktop portal (no uinput, no udev rule) once KWin offers it and `reis` supports it. | ~900 LOC incl. tests |
| No-video viewer modes (**built 2026-11-25**, untested on hardware: protocol 12 `video_paused` + `pen_mode`, ☰ menu on desktop and Android viewers: "Wacom pen (no video)", "PowerPoint remote only (no video)") | Presenter skips encoding while paused, resumes with a keyframe; pen permission asked when the pen is switched on (capture reopens with input). Open: KDE run, Android tablet run, ABR behaviour after a long pause. | ~400 LOC |
| Pen tilt, eraser (**wishlist, do not start**; hover as a moving cursor shipped 2026-11-28, no proximity/distance on the wire) | Extends the D26 pen (`ABS_TILT_X/Y`, `ABS_DISTANCE`, `BTN_TOOL_RUBBER`) and the `pointer` message. | — |
| Laser pointer | Finger position drawn as a coloured dot in a transparent overlay on the presenter; colours selectable. The KDE Wayland overlay is the hard part. | ~300–600 LOC |
| Drawing overlay | The same overlay, plus strokes and colours, with an optional fade-out like old Linux screen-annotation / "flame" effects. | ~800–1500 LOC |
| Touchpad | One finger moves the cursor, a tap clicks, two fingers scroll or right-click. | ~500–800 LOC |
| Pen tablet (Wacom emulation) | Pressure, tilt, hover, and buttons. Two display modes on the tablet: screen-visible (the existing viewer picture) and grey (no picture). Linux apps such as Krita and GIMP see a virtual tablet. Photoshop would need a macOS presenter, which is a separate and harder job (macOS makes faking a tablet difficult). Total includes tablet-side capture, laptop-side uinput, protocol, UI, tests, and docs. | ~1500–2500 LOC total |

The "screen shines through" mode is essentially free, because the tablet is
already a viewer. The grey mode is just "don't show the picture".

### Web viewer (WASM)

The viewer and all of the input modes above can run in the browser. The
presenter stays a native app (screen capture, virtual input, and the overlay
need access a browser cannot give).

- Transport: WebTransport over HTTP/3 on the presenter's own port (any port,
  including the existing 47800; not 443).
- Trust: `serverCertificateHashes` with the certificate fingerprint already
  carried in the QR link, so the page can say "trust exactly this certificate".
  No public hostname or internet address is required for the video/input
  connection.
- The web page itself must be served from a normal HTTPS origin (for example
  a free GitHub Pages site). Only the WebTransport session then goes to the
  laptop on the LAN.
- Browsers accept these certificates for at most 14 days, and they must be
  ECDSA. The presenter rotates the short-lived cert automatically. Pairing
  must be bound to the pairing identity, not to one certificate, so existing
  pairings survive rotation.
- Video decode via WebCodecs.

### Build order

1. Reverse input channel and virtual mouse/keyboard on the Linux presenter.
2. Presenter remote and touchpad (they rest on that channel).
3. Overlay (laser pointer, then drawing).
4. Pen tablet.
5. Web viewer in parallel when ready.


## Open questions

- Default bitrate and whether to offer 4:4:4 where the encoder supports it.
- VideoToolbox decode on the HD 6770M under OCLP: the photos show it unavailable (no H.264
  decoder for that GPU); software decode takes ≈2.4 ms/frame there, so it is fine as default.

## Known debt and doc/code mismatches

Found while writing these docs (2026-11-12, commit `f7677c7`). Fix them in their own commits.

- **Viewer route through a VPN is silent (2026-11-16):** an Android always-on VPN routed the
  USB-tethering subnet into `tun0`; the viewer just never reached the presenter, with no hint in
  the window (AGENTS.md §1.8). The viewer could name the interface its route uses.
- **Hardcoded 2000×1000:** `--width/--height` default to 2000×1000 and the GUI presenter uses
  them, so a 1920×1080 screen is scaled and distorted. The docs describe native resolution as
  the goal. (Being fixed under Next 1.)
- **`unsafe` outside the FFmpeg module:** proto/net forbid it, app denies it except
  `yuv_glow.rs`. media has a `// SAFETY:` on every site, enforced by
  `clippy::undocumented_unsafe_blocks` (2026-11-17). Still open: gather FFmpeg `unsafe` into one
  small safe module.
- **Panicking shortcuts in production code:** `Mutex::lock().expect(..)` throughout
  `mirrorwire-app` and `mirrorwire-net` (a poisoned lock crashes instead of recovering), plus a
  few `expect`s (`Control::encode`, scaler init, URI building) and `unwrap()` in
  `stats.rs`/`fragment.rs` header parsing (safe today after the length check, but brittle). No
  clippy lints (`unwrap_used`, `expect_used`) enforce the rule.
- **No rate limit on pairing attempts:** a 6-digit code with unlimited `hello` retries per
  presenter run. Consider a per-address attempt limit or a lockout after N wrong codes.
- **ALPN vs protocol:** `ALPN` is `mirrorwire/1` and `PROTOCOL_VERSION` is 1; nothing ties them
  together in code. Bump both together (protocol.md §4).
- **macOS presenter:** the **Present** button is disabled on macOS (no capture source there
  yet); VideoToolbox encode is built but untested.
- **Desktop viewer decode queue is 2 deep** (`viewer.rs` `DECODE_QUEUE`). On Android the same
  depth made every slow keyframe overflow the queue, flush it, and request another keyframe:
  ~1 picture/s on the emulator, a frozen picture on the owner's phone. Android now uses 6 and
  skips the RGBA copy of superseded pictures. The iMac (11 ms software decode) has not shown
  it, but a slower Mac would; port the same change if it does.
