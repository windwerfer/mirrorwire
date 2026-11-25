# Testing and verification

## 1. Gates

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
scripts/loopback-test.sh 10 --encoder x264
# or all of it at once (fmt, layer check, clippy, tests, loopback): cargo xtask ci
# loopback only, checked against perf-budgets.toml (limit 40 ms p95, see perf-budgets.toml; CI runs this): cargo xtask loopback 5
cargo audit
cargo deny --all-features --locked check advisories bans licenses sources
```

`cargo-audit` and `cargo-deny` are not part of the Rust toolchain; install them
with `cargo install cargo-audit cargo-deny` (or `taiki-e/install-action` in CI).

Build dependencies (Fedora and Ubuntu package lists) are in the README and in
`.github/workflows/ci.yml`. A headless box without a GPU or a portal can run all of these gates.

**Formatting note.** The workspace uses the rustfmt defaults (`max_width` 100); there is no
`rustfmt.toml`. `main` is `cargo fmt`-clean and CI fails on unformatted code
(`cargo fmt --all -- --check`, before clippy), so run `cargo fmt --all` before every commit.

## 2. Test layers

| Kind | Where | What it proves |
|---|---|---|
| Unit | `#[cfg(test)]` in `mirrorwire-proto` | header encode/parse, fragmentation and reassembly (loss, reordering, late fragments), control framing, clock offset, stats |
| Unit (delivery) | `mirrorwire-proto/src/rate.rs`, `fec.rs`, `control.rs` | arrival rate vs sent rate: growth pause on an unexplained shortfall, queue cut at the arrival rate (27 → 18 Mbit/s case), garbage ignored, still screen never raises the budget, `SentHistory` windows, `rx_kbps` of a report window, `link_report` required fields |
| Unit (checksum) | `mirrorwire-proto/src/crc.rs`, `fragment.rs` (`nack_tests`) | CRC-32 known vectors and slicing = bytewise, a rebuilt frame with a flipped byte is never delivered and counted, packets disagreeing on the CRC are dropped |
| Unit (NACK) | `mirrorwire-proto/src/nack.rs`, `fragment.rs` (`nack_tests`), `mirrorwire-net/src/transport/mod.rs` | NACK datagram round trip and limits, resend window (3 s / 8 MiB; budget 15 % of the ABR target, 128 KiB/s to 2 MiB/s), a lost fragment resent and no frame lost, resends not counted as loss, give-up after the hold time, whole-frame requests, tail loss; end to end: a presenter resends a dropped packet on a viewer NACK, and a viewer of another protocol version gets a readable rejection |
| Hostile input | `mirrorwire-proto/tests/hostile_input.rs`, `mirrorwire-net/src/pairing.rs` | random and mutated bytes into the packet header, reassembler (incl. garbage bodies through FEC recovery), pointer/cursor-image decoders, control messages and the URI parser: never a panic. proto and net also `deny` unwrap/expect/indexing/panic outside tests |
| Unit | `#[cfg(test)]` in `mirrorwire-net` | identity load/create, URI round trip (including a non-default port) and rejection of bad fingerprints, QR generation, port settings precedence, firewall guide text, ufw rule parsing |
| Unit (FilmCraft) | `mirrorwire-media/src/filmcraft.rs`, feature `filmcraft` | FilmCraft output via the real `Encoder` decodes with FFmpeg, markers read back, keyframe on request; an `#[ignore]` x264-vs-FilmCraft benchmark; `pure_tests` round-trips FilmCraft to FilmCraft with no FFmpeg. CI and `cargo xtask ci` also clippy+test the FFmpeg-free build (`--no-default-features --features filmcraft`) |
| Unit (app) | `#[cfg(test)]` in `mirrorwire-app` | CLI parsing, capture/FEC preference precedence, process log, and `presenter::tests::test_source_streams_to_an_in_process_viewer`: test-pattern presenter (x264) → QUIC → viewer with 1 % loss in one process, live FEC/bitrate/cursor changes, markers read back (needs FFmpeg with libx264; part of the coverage report) |
| Unit (firewall) | `mirrorwire-net/src/firewall.rs` | hint and notice for every firewalld/ufw state, a fake `Firewalld` proving the startup probe makes no privileged call, one click = one authorization and cancel = no further prompts (D14), D-Bus error classification, zone merging, open-port messages (the D-Bus/process glue is `firewall/system.rs`) |
| Unit (rate) | `mirrorwire-proto/src/rate.rs`, `fec.rs`, `mirrorwire-media/src/pipeline.rs` | AIMD controller (cuts on queue/loss/backlog, floor 3 Mbit/s, ceiling = user setting, learned random-loss baseline), closed-loop sims with a FIFO bottleneck, auto FEC caps 35 %/60 %, token-bucket bandwidth simulator (viewer `--simulate-bandwidth-kbps`), x264 bitrate change live without a keyframe, VAAPI reopen rate-limited (down 1 s, up 5 s), keyframe pacing sim (frozen frames, send-loop lag) |
| Unit (pen pressure, Linux) | `mirrorwire-app/src/pen/` | system check with a mocked `/dev/uinput` open (ok, permission denied, missing, other error; Flatpak vs native), exact fix/undo command text (udev rule, module lines only when missing outside the sandbox, Flatpak override + reset), router (off = portal only, device opened on the first down, keys stay on the portal, open/write failure falls back with a note and no retry until toggled), uinput frame sequences and pressure scale (D26) |
| Loopback (end to end) | `scripts/loopback-test.sh` | test pattern → encoder → QUIC (pinned TLS, pairing) → reassembly → decoder, on localhost, with latency stats |
| Real Wi-Fi (Android) | `scripts/android-wifi-run.sh`, `scripts/run-stats.py` | test pattern → x264 → QUIC over real Wi-Fi → the Android viewer on an adb-attached device; per-run video, cuts, queue, loss (§3.4) |
| Real hardware | the owner's T480s + iMac | portal capture, VAAPI, Wi-Fi, OpenGL on a non-Metal Mac (only he can run this) |

### The oracle: the frame marker

The test pattern stamps its frame number into the image as black/white blocks
(`pattern::stamp_marker`). The headless viewer reads it back from the *decoded* picture
(`pattern::read_marker`) and counts mismatches. That proves the frame shown is the frame sent,
independent of our own bookkeeping. Keep this oracle working: any change to scaling, colour
conversion or the decoder must still read the markers back.

Use FFmpeg's `ffprobe`/`ffmpeg` only as an **external** check (for example, decoding a dumped
bitstream to prove an encoder's output is standard H.264). Never make the app depend on a
command-line ffmpeg at runtime.

## 3. Measuring latency and quality

The loopback and every viewer print one stats line per second, and the presenter prints its own:

- viewer: `fps, Mbit/s, rtt, ms mean/p95/max:` per stage
- presenter: `fps, Mbit/s, keyframes, mtu, ms mean/p95/max:`

Stages: `capture_to_encoded` (presenter: capture, colour conversion/scaling, encode),
`encoded_to_sent`, `network` (clock-offset corrected), `reassembly`, `decode`, `convert`
(YUV→RGBA, GUI only), `capture_to_decoded` (end to end). The GUI overlay adds
`capture->texture`.

Rules (adapted from FilmCraft's "report progress honestly"):

- **Before/after on the same machine, same build profile, same arguments,** at least 10 s. Quote
  mean and p95 of `capture_to_decoded`, plus fps and Mbit/s. State the machine (CPU, GPU or "VM,
  no GPU") and that it was loopback.
- **Name what was measured vs estimated.** A VM result says nothing about VAAPI on UHD 620 or
  decode on the iMac. Write "untested on target" when that's the truth.
- Use `--simulate-loss <percent>` on the viewer to test loss recovery (add
  `--simulate-burst <mean packets>` for Wi-Fi-like bursts); quote decoded frames, lost frames,
  keyframe requests and the `fec overhead … packet loss …` line. Compare `--fec auto` against
  `--fec off` (= the old behaviour) on the presenter.
- Quality: compare at the same bitrate and resolution. Native resolution with no scaling is the
  baseline; a sharpness claim needs a photo/screenshot of text at 100 % or a PSNR/SSIM number
  from a decoded dump.

### 3.0 Forward error correction under simulated loss (box, 2026-11-13)

Agent box (8 vCPU Xeon VM, no GPU), loopback, release build, x264 1920×1080@60 test pattern,
40 Mbit/s, 10 s per run, loss simulated on the viewer
(`scripts/loopback-test.sh 10 --encoder x264 --fec MODE -- --simulate-loss L [--simulate-burst B]`).
"main" is commit 1ea1523 (no FEC). "fec off" is this build with `--fec off` (same wire format as
main, used for the bursty runs, which main's simulator can't do). Latency is
`capture_to_decoded` mean / p95 in ms over decoded frames; the max is dominated by x264 stalls
on the VM (`capture_to_encoded`), not the network.

| Loss | main: frames (lost, kf req) | fec off: frames | fec auto: frames (lost, kf req) | fec auto overhead | fec auto latency mean / p95 |
|---|---|---|---|---|---|
| 0 % | 599 (0, 0), 8.45 / 10.45 ms | 594–600, 9.2–9.5 / 11.5–11.8 ms | 602–603 (0, 0) | 12 % | 8.80–8.90 / 11.3–11.5 |
| 2 % random | 1 (366, 74) | 9 (357, 75) | 600–601 (0, 0) | 16–17 % | 8.8–9.0 / 10.4–10.9 |
| 5 % random | 0 (517, 26) | 0 (503, 27) | 592–597 (0–1, 1–2) | 25 % | 9.5–9.6 / 11.0–12.6 |
| 2 % bursts of 4 | — | 172 (159, 77) | 570 (10, 9) | 31 % | 9.2 / 11.1 |
| 5 % bursts of 4 | — | 40 (279, 78) | 562 (16, 12) | 40 % | 9.4 / 11.7 |
| 5 % bursts of 8 | — | — | 554 (17, 15) | 49 % | 9.1 / 11.8 |

Without FEC, almost no frame of a 40 Mbit/s 1080p stream survives 2 % loss: every frame has
~60 packets and a keyframe ~400, so one loss per frame is likely and the keyframe asked for to
recover is lost too. With auto FEC random loss is fully covered; bursty loss still costs a
frame now and then (each lost delta frame waits for a keyframe, since there is no intra
refresh), and the auto boost raises the parity until that mostly stops. Untested on real Wi-Fi.

### 3.1 FilmCraft vs x264 (box, 2026-11-13)

Machine: the agent box, 8 vCPU Intel Xeon VM, no GPU, release build, so this says nothing about
the T480s (untested on target). Two measurements:

Encoder-only benchmark (synthetic gradient + moving pattern, 120 frames, CBR 15 Mbit/s, Y-PSNR
against a BT.601 reference, so the ceiling is ~50 dB):
`MIRRORWIRE_BENCH_SIZE=1920x1080 cargo test --release -p mirrorwire-media --features filmcraft -- --ignored bench_ --nocapture`

| Encoder | Size | encode mean / p95 | max fps | Mbit/s @60 | Y-PSNR |
|---|---|---|---|---|---|
| libx264 | 1920×1080 | 2.57 / 2.98 ms | ~389 | 14.4 | 42.10 dB |
| filmcraft-h264enc | 1920×1080 | 10.31 / 14.94 ms | ~97 | 14.6 | 41.71 dB |
| libx264 | 2000×1000 | 2.65 / 3.16 ms | ~377 | 14.3 | 41.87 dB |
| filmcraft-h264enc | 2000×1000 | 11.97 / 15.46 ms | ~84 | 14.7 | 41.54 dB |

End-to-end loopback, 15 s, test pattern at 60 fps, all frame markers read back, 0 mismatches
(`MIRRORWIRE_FEATURES=filmcraft scripts/loopback-test.sh 15 --encoder filmcraft --width W --height H --bitrate-kbps K`):

| Encoder | Size, bitrate | fps | Mbit/s | capture_to_encoded mean / p95 | capture_to_decoded mean / p95 |
|---|---|---|---|---|---|
| x264 | 1920×1080, 30 Mbit/s | 59.8 | 28.7 | 6.37 / 7.95 ms | 7.81 / 9.71 ms |
| filmcraft | 1920×1080, 30 Mbit/s | 59.9 | 21.0 | 14.67 / 19.23 ms | 15.74 / 20.82 ms |
| x264 | 2000×1000, 15 Mbit/s | 59.7 | 14.8 | 6.41 / 8.11 ms | 7.84 / 10.12 ms |
| filmcraft | 2000×1000, 15 Mbit/s | 59.9 | 15.0 | 15.61 / 20.13 ms | 17.13 / 22.70 ms |

Reading: FilmCraft keeps 60 fps at these sizes on 8 cores with similar quality per bit, but
costs ~8–9 ms more per frame than x264 (ultrafast/zerolatency) and much more CPU. At 30 Mbit/s
it undershot the target (21 Mbit/s) on this easy content. On the 4-core T480s expect it to be
slower still; VAAPI remains the default there.

Useful runs:

```sh
scripts/loopback-test.sh 30 --encoder x264 --width 1920 --height 1080 --bitrate-kbps 30000
MIRRORWIRE_FEATURES=filmcraft scripts/loopback-test.sh 15 --encoder filmcraft --width 1920 --height 1080 --bitrate-kbps 30000
scripts/loopback-test.sh 10 --intra-refresh -- --simulate-loss 0.5
```

### 3.2 Adaptive bitrate under a simulated bottleneck (box, 2026-11-13)

`scripts/loopback-test.sh 30 --encoder x264 --width 1920 --height 1080 --bitrate-kbps 40000 --
--simulate-bandwidth <Mbit/s> --simulate-loss <pct>`; the viewer drops datagrams that exceed a
token bucket (150 ms deep) after the random-loss simulator. "Before" is b0c90ad with only the
simulator added; "after" is the adaptive-bitrate commit (adaptive on, FEC auto). 1800 frames sent.

| Link | Before: frames, fps, packet loss, unrecoverable, FEC | After: frames, fps, Mbit/s, packet loss, unrecoverable, FEC |
|---|---|---|
| 20 Mbit/s + 2 % random | 6, 0.2 fps, 69.5 %, 596, 59 % | 1612, 53.7 fps, 12.1 Mbit/s, 7.9 %, 67, 34 % |
| 8 Mbit/s + 2 % random | 2, 0.1 fps, 87.8 %, —, 59 % | 1389, 46.3 fps, 3.2 Mbit/s, 25.7 %, 220, 39 % |
| no limit + 2 % random | 1800, 60 fps, 40.0 Mbit/s, 2.0 %, 0, 17 % | 1801, 60 fps, 17.4 Mbit/s, 2.1 %, 0, 18 % |

Before, the encoder kept sending 40 Mbit/s plus up to 59 % parity into the bottleneck, so
almost nothing got through. After, the controller backs off to what fits and the stream stays
watchable; the mean capture→decoded latency stayed at 8–9 ms.

**Correction (2026-11-15): the last row is not a controller cut.** The same commit removed
x264's `nal-hrd=cbr` filler. Before, every frame was padded to the target, hence "40.0 Mbit/s".
Since then x264 sends only what the test pattern needs. Re-measured on the box (current
`main`, 30 s, x264 1920×1080, 40 Mbit/s setting; ~45–52 fps, since the VM was slow that day),
with every cut logged (`adaptive bitrate: cut … : <reason>` in the presenter log; the presenter
status shows `last cut: <reason>`):

| Run | Cuts | Budget | Sent (mean of 1 s windows) |
|---|---|---|---|
| adaptive on, 2 % random | 0 | 40.0 throughout | 15.4 Mbit/s (7–27 per window) |
| adaptive on, 0 % | 0 | 40.0 | 15.0 Mbit/s |
| adaptive off, 0 % | 0 | 40.0 (fixed) | 13.1 Mbit/s |
| adaptive off, 2 % random | 0 | 40.0 (fixed) | 15.0 Mbit/s |

So the ~15–17 Mbit/s is the content, not the rate control. 2 % random loss can't trigger a cut
on its own: the excess-loss rule needs 3 points above the background loss.

### 3.3 Adaptive bitrate, closed-loop simulation (unit test)

`cargo test -p mirrorwire-proto sim_ -- --nocapture` runs the controller against an encoder
model (60 fps, sends min(target, content demand)), auto FEC parity, `LossSim`, a FIFO bottleneck
(150 ms buffer unless noted; overload shows as delay first, then as loss) and a `DelayMeter` as
the viewer has, with a 500 ms report. There is no QUIC, so the backlog trigger is not exercised.
Results with the controller as of 2026-11-16 (queue + loss, hysteresis around the budget that
failed last), 40 Mbit/s ceiling, 60–90 s; in brackets the controller before the hysteresis
("no hyst.", same day) and the 2026-11-15 controller ("old": loss + RTT, drop-only
token-bucket bottleneck) where they differ:

| Link | Cuts (triggers) | Budget at end | Video delivered | Packets lost | Queue mean / p95 |
|---|---|---|---|---|---|
| no limit, 0 % / 2 % random | 0 | 40.0 | 36.8 / 33.8 Mbit/s | 0 / 2.0 % | 0 |
| no limit, 5 % random | 0 (old: 3, loss, until learned as background) | 40.0 | 30.5 Mbit/s | 5.1 % | 0 |
| content needs 15 Mbit/s, 2 % random | 0 | 24.4 | 15.2 Mbit/s | 2.0 % | 0 |
| content 12 Mbit/s with 24 Mbit/s bursts, 20 Mbit/s bottleneck | 5 queue (no hyst. 6, old 4) | 16.9 (21.0) | 13.2 Mbit/s | 0 % (old 0.74 %) | 1.8 / 14.0 ms (5.3 / 47.7) |
| 20 Mbit/s bottleneck, 0 % | 11 queue (no hyst. 15, old 11 heavy loss) | 18.8 (17.7) | 16.6 Mbit/s (16.7, old 16.0) | 0 % (old 1.5 %) | 5.4 / 36.0 ms (10.5 / 58.9) |
| 20 Mbit/s bottleneck, 2 % | 9 (2 loss, then queue; no hyst. 15) | 19.8 | 15.8 Mbit/s (16.0) | 2.1 % | 9.2 / 47.7 ms (11.2 / 56.9) |
| 8 Mbit/s bottleneck, 2 % | 17 (queue, 1 heavy loss; no hyst. 22) | 8.1 (8.0) | 6.3 Mbit/s | 2.1 % (old 2.9 %) | 8.8 / 50.7 ms (23.3 / 123) |
| 16 Mbit/s, 300 ms FIFO (`WIFI`) | 13 queue (no hyst. 16, RTT rule: 19) | 14.4 (14.2) | 13.3 Mbit/s (13.8, RTT rule: 13.0) | 0 % | 7.3 / 41.4 ms (18.1 / 70.5, RTT rule: 25.9 / 129) |
| 16 Mbit/s, **50 ms** FIFO | 12 (1 queue, then loss at 4–7 %; no hyst. 10: 9 heavy loss) | 13.9 (18.4) | 13.3 Mbit/s (11.7) | **0.72 %** (4.9 %) | 7.7 / 34.2 ms (10.1 / 34.4) |
| 16 Mbit/s, **20 ms** FIFO | 12 (3 heavy loss, then loss at 4–7 %; no hyst. 10 heavy loss) | 13.4 (13.6) | 13.0 Mbit/s (11.3) | **1.19 %** (5.5 %) | 1.1 / 4.3 ms (1.5 / 4.4) |

**Hysteresis (2026-11-16).** Each cut remembers what the link did not carry (the budget, or
what was sent if less). From 90 % to 115 % of that value the budget climbs at a quarter of the
normal step, and from 90 % up loss above the background is congestion even with a flat queue.
With a **shallow** bottleneck buffer (≤ 50 ms) the queue never reaches the 40 ms that marks loss
as congestion, so before this, loss counted as random until it was heavy and each overrun lost
~23 % of a report's packets; now, after the first overrun, the next ones are cut at 4–7 % and
packet loss falls 4.9 → 0.7 % (50 ms) and 5.5 → 1.2 % (20 ms) with more video delivered
(`sim_shallow_buffer_is_found_without_heavy_loss`). The slow climb near the last failure also
lowers the queue p95 on the deep-buffer links (−16 to −71 %) with fewer cuts, for 0–4 % less
video there (`WIFI` 13.8 → 13.3 Mbit/s); the shallow links cut a little more often (10 → 12). Tried and not better overall: 0.95
or 0.85 instead of 90 %, a half or 35 % step instead of a quarter. Weak spot left (roadmap item
0): a bottleneck is still found by overrunning it, now every ~7–9 s (less often since the
parity probe, below).

**Keyframes, VAAPI reopens and keyframe pacing (2026-11-16,
`sim_keyframe_pacing_keeps_the_picture`).** The rows above have no keyframes. This test adds one
at connect and one every 500 ms while the viewer waits for one, 5× a delta frame (measured on
the build box, test pattern 1080p at 12 Mbit/s with the 100 ms VBV: x264 3.6–4.4×, h264_vaapi
2.0–5.0×, up to 122 KB). The rate is either live (x264) or changed only by a reopen that starts
with a keyframe (h264_vaapi). Each number is the mean of 10 runs (ceilings 36–44 Mbit/s, content
needing 60 or 18 Mbit/s); single runs with keyframes are chaotic (one parameter step moved lost
frames from 24 to 119). **Frozen** = frames the viewer cannot show: lost, or after a lost frame
until a keyframe arrives whole (4800 frames are measured per run). **Lag** = encoded → first
packet arrived, including the wait in the presenter's send loop (the viewer's queue metric
starts at the send and does not see that wait).

| Link (encoder) | Burst, 5 s reopen | Burst, 1 s down | **Paced, 1 s down** |
|---|---|---|---|
| 16 Mbit/s, 300 ms (VAAPI) | 301 frozen, 6.6 % lost, lag p95 284 ms, 9.3 Mbit/s | 100, 0.5 %, 128 ms, 12.6 | **0, 0.0 %, 60 ms, 12.4** |
| 16 Mbit/s, 50 ms (VAAPI) | 1801, 15.5 %, 34 ms, 7.3 | 3123, 7.0 %, 34 ms, 11.3 | **291, 1.5 %, 82 ms, 10.4** |
| 16 Mbit/s, 20 ms (VAAPI) | 4019, 20.8 %, 5 ms, 11.4 | 4799 (never a picture), 13.2 %, 3 ms, 9.9 | **759, 3.0 %, 54 ms, 10.1** |
| 20 Mbit/s, 150 ms (VAAPI) | 278, 5.9 %, 135 ms, 12.6 | 294, 1.3 %, 114 ms, 14.8 | **74, 0.2 %, 107 ms, 14.5** |
| 16 Mbit/s, 20 ms (x264) | 4799 (never a picture), 11.2 % | same | **0, 0.9 %, 4 ms, 13.0** |
| other links (x264), no limit | 0 | 0 | 0, unchanged |

Paced: a keyframe at 1.25 × the budget, frames that waited behind a paced one at 1.5 ×, at most
100 ms per frame (`rate::pace_spread_us`). Findings on the way:
- The 1 s down-reopen alone helps the deep buffers but makes shallow ones freeze more (more
  reopens, so more keyframes that do not fit). Pacing with the old 5 s rule is worse still
  (16/300: lag p95 1.4 s): a VAAPI encoder above the budget for 5 s backs up in the send loop.
  The two belong together.
- Pacing only the keyframe did not help: the 2–3 frames behind it left as one burst onto the
  buffer the keyframe had just filled.
- Pacing at exactly the budget never catches up: lag 0.3–4 s. 1.5 × for everything: more frozen
  frames on shallow buffers (16/50: 683, 16/20: 1206) for less lag (67 / 47 ms); 1.25 × for
  everything: fewer (157 / 205) but lag p95 ~150 ms. Keyframe 1.25 × / catch-up 1.5 × is in
  between and has the lowest lag on the deep buffers. Caps of 50 / 100 / 200 ms: 100 best.
- An earlier version of this sim (keyframes 10×, no repeated keyframe requests, single runs)
  made the 1 s down-reopen look better than it is; its numbers are superseded by this table.
- Rejected before pacing: ignoring loss/queue for 1 s after each keyframe (roadmap item 0.3).
  An upward reopen also starts with a keyframe, so the grace hid exactly that overrun.
- Left: VAAPI behind a 20–50 ms buffer still freezes 6–16 % of the time; a live VAAPI rate
  (roadmap item 0.6) would have removed the reopen keyframes; dropped 2026-11-18, see §3.10.

**Roadmap item 0.5 tried and rejected (2026-11-16).** The sim now counts lost frames (more
packets lost than parity, pooled per frame) in every mode and passes the count of each 500 ms
window to the controller as the viewer's `unrecoverable` would be. Three variants, each against
the table above (same sim, same seeds):
- *No loss cut while every frame was rebuilt* (except severe loss): on the shallow buffers the
  budget settles above the link with a standing loss FEC covers. 16 Mbit/s / 50 ms: 12 → 1 cuts,
  packet loss 0.72 → 5.42 %, video 13.3 → 12.9 Mbit/s, the test fails; VAAPI 16/50 paced
  291 → 401 frozen frames. The loss there is our own overrun, and FEC covering it now says
  nothing about the next keyframe (5× a delta frame).
- *The same, but not near the budget that failed last* (the hysteresis zone): the shallow rows
  are unchanged, but nothing improves either. 20 Mbit/s / 2 %: 9 → 11 cuts, video 15.8 → 15.6;
  VAAPI 16/50 paced still 291 → 401 frozen. Without the hysteresis zone, random loss already
  holds the rate (flat queue = random, learned background loss), so the rule has little left to
  act on.
- *Rate control on the smoothed loss FEC uses* (rise ×0.5, decay ×0.15): worse. x264 16/50
  paced 0 → 25 frozen frames and 0.82 → 3.81 % lost; shallow 20 ms 1.19 → 2.27 % lost (test
  fails). The slow decay keeps cutting after the loss is gone.

**Reconnect: resume from the last budget (2026-11-16, `sim_reconnect_resumes_the_last_budget`).**
A 90 s session, then the same viewer reconnects; the first 20 s after the reconnect, means of
the same 10 runs as above, paced. Fresh = slow start from 10 Mbit/s; resumed = start at
min(budget when it left, 60 % of the last bad budget), slow start to 90 % of it, keep the
failure for the hysteresis; without a failure, resumed = fresh. Frozen frames (lost frames):

| Link | x264 fresh → resumed | VAAPI fresh → resumed |
|---|---|---|
| no limit; no limit, then 16 Mbit/s / 50 ms | unchanged (no failure to resume from) | unchanged |
| 16 Mbit/s, 300 ms | 0 → 0 (lag p95 50 → 36 ms) | 0 → 0 (lag p95 257 → 47 ms) |
| 16 Mbit/s, 50 ms | 32 (13) → 0 (0) | 64 (19) → 50 (10) |
| 16 Mbit/s, 20 ms | 32 (16) → 0 (0) | 503 (108) → 160 (22) |
| 8 Mbit/s, 50 ms | 37 (33) → 0 (0) | 103 (66) → 47 (17) |
| 5 Mbit/s, 300 ms | 85 (51) → 0 (0) | 98 (58) → 30 (5) |
| 20 Mbit/s, 150 ms | 0 → 0 | **0 → 18.5 (11)** |
| 20 Mbit/s, then 8 Mbit/s / 50 ms (link got worse) | **37 (33) → 59 (31)** | **103 (66) → 115 (48)** |

Start share tried: 1.0 (the budget it left with) froze more than fresh on shallow buffers (x264
16/20: 32 → 119; the connect keyframe at 1.25 × a budget at the limit does not fit), 0.85 and
0.75 in between, 0.6 best. Resuming without a failure (an unlimited LAN remembered at 40 Mbit/s,
then a 16 Mbit/s / 50 ms path): 13 → 78 lost frames, so that case starts fresh. Remembering only
the failure (slow start from 10 Mbit/s, capped at 90 % of it): as good on 16 Mbit/s links, but no
help below 10 Mbit/s (5 Mbit/s: 85 frozen either way). Hardware (2026-11-16, USB tethering,
h264_vaapi test pattern, three 40 s sessions with a force-stop between): picture within ~0.5 s
of each reconnect, 0.0 % loss, 0 unrecoverable frames; the link never cut, so each reconnect
started fresh (resume not exercised on hardware).

**Comparing a change (tools).** Run the sims before and after, same machine:
`MIRRORWIRE_SIM_SOFT=1 cargo test --release -p mirrorwire-proto sim_ -- --nocapture
--test-threads=1 > F 2>&1`, then `scripts/sim-compare.py before.txt after.txt` prints totals
(frozen frames summed over the averaged rows, mean video, cuts, failures) and every row that
changed. `MIRRORWIRE_SIM_SOFT=1` turns the limits of the averaged tests into `SOFT FAIL` lines
so a sweep prints every row; `MIRRORWIRE_SIM_WIDE=1` averages over 20 ceilings (30–49 Mbit/s)
instead of 5, to tell a real change from the chaos of single runs. For a control, compare
against the same change with one part switched off (as the no-parity probe below).

**Probe with parity before more video (2026-11-17, roadmap item 0.4).** From 90 % to 115 % of
the last bad budget the budget grows by extra FEC parity only (a quarter of the normal step),
up to 3 % above the video budget; the video follows after 3 s of clean reports, and any report
that is not clean restarts the 3 s. Keyframes are paced on the video budget. The tables above
are from before this change. Same sim, same seeds, before → after:

| Link | Before | After |
|---|---|---|
| 20 Mbit/s bottleneck, 0 % / 2 % (no keyframes) | 11 / 9 cuts, 16.6 / 15.8 Mbit/s, queue p95 36 / 48 ms | 7 / 2 cuts, 16.8 / 16.3, 26 / 20 ms |
| 8 Mbit/s, 2 % (no keyframes) | 17 cuts, 6.3 Mbit/s, p95 51 ms | 13 cuts, 6.0, 45 ms |
| 16 Mbit/s, 300 / 50 / 20 ms (no keyframes) | 13 / 12 / 12 cuts, 0 / 0.72 / 1.19 % lost | 7 / 7 / 7 cuts, 0 / 0.39 / 0.85 % lost, video ±0.2 Mbit/s |
| 12 Mbit/s content with bursts, 20 Mbit/s | 5 cuts, p95 14 ms | 2 cuts, p95 0 ms |
| VAAPI paced 16/300 | 0 frozen, 12.4 Mbit/s, lag p95 60 ms | 0 frozen, **11.7**, 10 ms |
| VAAPI paced 16/50 | 291 frozen (59 lost), 10.4 Mbit/s | **107** (33), 11.0 |
| VAAPI paced 16/20 | 759 frozen (93 lost), 10.1 Mbit/s | **624** (61), 10.7 |
| VAAPI paced 20/150 | 74 frozen (16 lost), 14.5 Mbit/s | **32** (6), 14.9 |
| x264 paced, all links | 0 frozen | 0 frozen, video −0.2 to +0.2 Mbit/s |
| Reconnect, VAAPI resumed 16/50, 16/20, 5/300, 8/50, 20/150 | 50, 160, 30, 47, 18.5 frozen | 7, 110, 0, 0, 0 |
| Reconnect, VAAPI fresh 16/50, 16/20, 8/50 | 64, 503, 103 | 28, 499, 82 |
| Reconnect, 20 then 8 Mbit/s resumed (x264 / VAAPI) | 59 / 115 frozen | 58 / 91 (still more than fresh: 37 / 82) |

Summed over all paced and reconnect means: 2889 → 2154 frozen frames, 199 → 117 cuts, video
delivered 11.95 → 11.95 Mbit/s. A bottleneck is still found by overrunning it, but less often:
7 cuts in 90 s instead of 11–13 on the 16 and 20 Mbit/s links. Cost: VAAPI behind a deep buffer
(16/300) delivers 6 % less video, and a fresh VAAPI reconnect on 20/150 5 % less (14.0 →
13.3 Mbit/s). Tried (sums over 20 ceilings 30–49 Mbit/s instead of 5, to see past the chaos; before:
2983 frozen frames, 196 cuts; the sweep ran before a rounding fix in `probe_overhead`,
so its numbers compare with each other; the chosen 3 % / 3 s is 2012 on the committed code):
- Probe size and hold: 2 % / 2 s 2189, 3 % / 2 s 2217, **3 % / 3 s 1967**, 3 % / 4 s 2014,
  4 % / 2 s 2449, 4 % / 3 s 2261, 5 % / 2 s 2232 (8 % and 10 % with the 10 runs: worse, and
  reconnect asserts fail).
- Control, the same stepped climb with no extra parity (the video waits, the wire rate does
  not rise): 3 % / 3 s 2341, 5 % / 2 s 2954, 2 % / 2 s 2231. So about two thirds of the gain is
  the slower, held climb and one third the parity; with a 2 % probe the parity adds nothing.
- Pacing keyframes on the probed budget instead of the video budget: VAAPI 16/20 624 → 969
  frozen frames (all else unchanged).
- Extra probe parity on keyframes too (they already get ≥ 25 %): no change; not done.
With FEC off nothing carries a probe; the video still waits for the 3 s hold (the control
above). Real hardware: not run yet (the USB link never cut, so it never reached the probe zone).

### 3.4 Real Wi-Fi: presenter in the dev container, Android tablet viewer (2026-11-15)

Presenter: this repo's dev container on the Fedora laptop (host network, Wi-Fi), test pattern,
x264 1920×1080@60, 40 Mbit/s setting, adaptive bitrate and FEC auto. Viewer: the MTK tablet
(TB336FU, `c2.mtk.avc.decoder` GPU output) over the same Wi-Fi, debug APK built here. 60 s per
run, before and after alternated so Wi-Fi conditions are comparable. "before" is 38e3290.

**Reproduce** (also §3.5): install the APK of the same release (`packaging/android/build-apk.sh`,
`adb install -r dist/MirrorWire-arm64-v8a.apk`), keep the device on USB for adb and on the same
Wi-Fi, then alternate two presenter builds and summarise:

```sh
cp target/release/mirrorwire /tmp/bc-before        # build the "before" commit first
for i in 1 2; do for b in before after; do
  MIRRORWIRE_BIN=/tmp/bc-$b scripts/android-wifi-run.sh 60 /tmp/tab-$b-$i.log   # extra args go to `present`
done; done
scripts/run-stats.py /tmp/tab-*.log
```

`android-wifi-run.sh` starts the presenter (`--no-pairing --no-mdns`, test pattern, x264),
opens its `mirrorwire://` link on the device with `am start`, waits, stops both, and keeps the
device's `mirrorwire` logcat next to the log. `ADB_SERIAL` picks a device; `ADVERTISE_IP`
overrides the presenter address (default: the local address that routes to the device's
wlan0). The UDP port (47800) must be open in the host firewall. `run-stats.py` reads the
per-second `presenter:` lines and the `adaptive bitrate: cut` lines; its columns are the ones
in the tables below (unrecoverable frames, keyframes, cuts by trigger, video sent, queue mean /
p95 of the per-second viewer queue, QUIC packets lost).

| Run | Unrecoverable frames | Keyframes sent | Viewer requests honoured / ignored | Cuts | 1 s windows with RTT > 150 ms | Video sent (mean) |
|---|---|---|---|---|---|---|
| before | 66 | 28 | 21 / – | 12 | 5 | 15.1 Mbit/s |
| fixed | 3 | 9 | 3 / 5 | 7 | 0 | 14.5 Mbit/s |
| before | 198 | 46 | 34 / – | 10 | 6 | 13.8 Mbit/s |
| fixed | 41 | 9 | 4 / 9 | 5 | 2 | 12.6 Mbit/s |

Earlier runs of the old build: 64, 93, 136, 102, 85 unrecoverable frames per 40–75 s.

How the cause was found: the tablet's `/proc/net/snmp` `RcvbufErrors` did not move (no kernel
drops), the presenter dropped nothing, but QUIC itself lost up to 1369 of 2921 packets in a
second (the presenter log now shows `quic lost N of M packets, cwnd`). The link to the tablet
carries ~20–25 Mbit/s; every loss episode was the presenter overrunning it: at connect
(starting at 40 Mbit/s, ~28 Mbit/s sent, RTT 250+ ms, 20–47 % loss), and later after the
budget had drifted to ~26 Mbit/s while only ~12 were sent, so the next busy frames jumped to
~24. Each episode then drew a keyframe request every ~100 ms and 4–8 IDRs a second. Fixed by
slow start, growing the budget only while it is used, and at most one forced keyframe per
2 × RTT. Limiting keyframes alone (measured separately) halved the IDRs but not the lost frames
(119 / 92 vs 102 / 85). The remaining episodes (fixed run 2) are bursts that really used a
~21–27 Mbit/s budget; the RTT rose 1–1.5 s before the loss each time (roadmap 0.2).

### 3.5 Real Wi-Fi: one-way queueing delay (2026-11-15, same setup as §3.4)

Same presenter and tablet as §3.4; this Wi-Fi link was weaker (tablet RSSI −75 dBm, 11n,
39 Mbit/s link speed) and carried ~15–17 Mbit/s. The APK reports the queueing delay in both
columns; "before" is 79eb65b's controller (ignores it), "queue" cuts on it. 60 s per run,
alternated. Queue statistics are over the viewer's 0.5 s reports (mean of the window means;
95th percentile of the window means).

| Run | Unrecoverable frames | Keyframes | Cuts | 1 s windows RTT > 150 ms | Video sent | Queue mean / p95 |
|---|---|---|---|---|---|---|
| before | 13 | 12 | 7 | 5 | 15.8 Mbit/s | 24 / 102 ms |
| queue (no max rule) | 10 | 10 | 8 | 0 | 14.7 Mbit/s | 20 / 65 ms |
| before | 1 | 9 | 9 | 0 | 15.6 Mbit/s | 21 / 85 ms |
| queue (no max rule) | 1 | 9 | 7 | 1 | 14.2 Mbit/s | 13 / 40 ms |
| before | 0 | 9 | 10 | 0 | 15.1 Mbit/s | 19 / 63 ms |
| queue | 0 | 9 | 10 | 0 | 14.1 Mbit/s | 13 / 36 ms |
| before | 35 | 13 | 8 | 4 | 15.3 Mbit/s | 32 / 129 ms |
| queue | 126 (121 in one outage) | 10 | 9 | 4 | 13.4 Mbit/s | 15 / 71 ms |

The queue controller lowers queueing delay every time (p95 −35 to −58 %) for ~7 % less video.
Lost frames are dominated by rare events that four minutes can't separate: the 121 came from a
Wi-Fi outage (no viewer report for 2.4 s, QUIC lost 2606 packets, queue 3 ms right after); the
rest of that run and the 10 in the first "queue" run are bursts after the budget had climbed
far above the link (27.5 Mbit/s budget while 10–15 were sent, then queue 4 → 128 ms in one
report). The first observation run (old controller) showed the shape of every episode: the
queue rises for 0.5–1.5 s (e.g. 42 → 104 → 211 → 292 ms) before any loss, while the RTT rule
fired only at 110–180 ms. "No max rule" missed 24 ms (max 82, +193 ms/s) one report before a
236 ms queue, which is why one frame waiting ≥ 50 ms now counts.

**Protocol 2 (2026-11-16, 5a181fa, APK rebuilt):** same setup, one 60 s run: connects, 121
`link_report`s with the queue object, 0 unrecoverable frames, 9 queue cuts, 14.7 Mbit/s sent,
queue mean 12 / p95 44 ms (within the spread of the four "queue" runs above). A protocol-1
desktop viewer (e298736) against this presenter is rejected with `version mismatch: the
presenter speaks protocol 2, the viewer 1; update the viewer …` in its connect steps and the
presenter's attempt list.

**Hysteresis (2026-11-16, protocol 3 APK rebuilt):** same setup (tablet RSSI −71 dBm, 11n,
52–58 Mbit/s link speed), presenter before (328d63a) and after the hysteresis, alternated, 60 s
each. The Wi-Fi buffer is deep, so every cut came from the queue and no run lost a packet; the
change shows as fewer cuts and a lower queue for a little less video, as in the `WIFI` sim:

| Run | Unrecoverable frames | Keyframes | Cuts (all queue) | Video sent | Queue mean / p95 |
|---|---|---|---|---|---|
| before | 0 | 5 | 9 | 13.9 Mbit/s | 13 / 45 ms |
| after | 0 | 5 | 4 | 13.1 Mbit/s | 12 / 34 ms |
| before | 0 | 6 | 8 | 14.8 Mbit/s | 13 / 45 ms |
| after | 0 | 6 | 4 | 14.3 Mbit/s | 13 / 40 ms |

Loopback with the viewer's shallow simulated bottleneck (`--simulate-bandwidth 16`, 30 ms
tail-drop buffer), x264, 3 × 30 s each, alternated: packet loss 4.0–5.5 → 2.7–2.9 %,
unrecoverable frames 70–78 → 42–53, video 7.9–8.5 → 9.2–9.5 Mbit/s, capture→decoded
unchanged (24.6–25.8 / 30–32 ms mean / p95).

### 3.6 Real Wi-Fi: keyframe pacing (2026-11-16)

Presenter: the dev container (host network, Wi-Fi), test pattern 1920×1080@60, 40 Mbit/s
setting, adaptive bitrate, FEC auto; `ENCODER=x264` or `vaapi` (h264_vaapi on the laptop's
iGPU) with `scripts/android-wifi-run.sh`. Viewer: the MTK tablet, APK built from the same tree,
2.4 GHz, RSSI −78 to −79 dBm, link speed 26–72 Mbit/s; the link carried only 3.6–7 Mbit/s.
Before = 76fe82e (1 s down-reopen, no pacing), after = keyframe pacing. 60 s per run, four
rounds, before/after alternated (rounds 3–4 after first). `scripts/run-stats.py`:

| Encoder | Side | Keyframes (frames paced) | Unrecoverable | Waiting for a keyframe | Queue p95 | Video |
|---|---|---|---|---|---|---|
| VAAPI | before | 15, 13, 6, 8 | 1, 1, 0, 0 | 0, 8, 0, 0 frames | 146, 17, 10, 18 ms | 3.8–7.0 Mbit/s |
| VAAPI | after | 10 (12), 9 (10), 6 (8), 7 (11) | 0, 0, 0, 0 | 0, 0, 0, 0 | 12, 35, 16, 11 ms | 3.6–6.4 Mbit/s |
| x264 | before | 6, 5, 6, 5 | 0, 0, 0, 0 | 8, 0, 0, 0 | 26, 39, 46, 42 ms | 3.7–6.2 Mbit/s |
| x264 | after | 5 (35), 8 (56), 7 (54), 6 (44) | 0, **165**, 1, 0 | 0, **128**, 0, 0 | 38, **582**, 37, 61 ms | 4.0–6.0 Mbit/s |

This link buffers deeply (queue cuts only, 0.0 % QUIC loss), the case where the simulation
predicts little change; the shallow-buffer case pacing is for (router, USB) was not available.
The bold run was a Wi-Fi outage, not pacing: the queue went 17 → 456 ms within one second with
no keyframe or paced frame (the last keyframe 8 s earlier), then QUIC lost 819 of 820 packets.
Otherwise no difference beyond run-to-run noise: VAAPI a little better (no unrecoverable frames,
fewer keyframes), x264 queue p95 perhaps a few ms higher. Pacing fired as designed (each
keyframe plus 1–9 frames behind it). Loopback with the viewer's shallow simulated bottleneck
(`--simulate-bandwidth 16`, 30 ms tail drop), 3 × 30 s alternated: x264 packet loss 3.16 /
1.49 / 3.36 → 2.96 / 1.42 / 2.53 %, unrecoverable frames 62 / 40 / 55 → 73 / 28 / 55,
capture→decoded p95 35–69 → 44–52 ms (no clear change); h264_vaapi stayed at ~6 Mbit/s there
and never hit the limit.

**Caveat for every h264_vaapi test-pattern run above (found 2026-11-16):** the shm+VPP upload
allocated a new 8 MB frame per frame (`pack/map` 6.1 ms), so the presenter made ~49 fps, not 60
(the tablet showed `in 49`). Fixed in `encode.rs` (`vpp_input` is reused). Loopback, same box,
3 × 10 s alternated: 39.9 / 42.5 / 42.3 → 60.0 fps at steady state, capture→encoded 24 → 13 ms;
tablet over USB: 60.0 fps sent and decoded. Portal capture with DMA-BUF (zero-copy) never took
this path.

### 3.7 USB tethering: viewer receive buffer (2026-11-16)

Presenter: the GUI in the dev container, portal capture (dmabuf-zero-copy), h264_vaapi, 40 Mbit/s
setting, adaptive bitrate, a movie playing. Viewer: the MTK tablet over Android USB tethering
(`rndis0`, RTT 1 ms, tablet Wi-Fi still on). Before the fix the viewer socket had Android's
default 224 KB receive buffer (`rmem_default`); the link lost nothing (laptop tx drops 0, viewer
queue 0–1 ms) but the tablet's `/proc/net/snmp` `Udp RcvbufErrors` rose 33 per 10 s (~2 % of
datagrams, 13–18 % around keyframes), so rate control cut on "heavy loss" and sat at the 3 Mbit/s
floor. After (`VIEWER_RECV_BUFFER`, 4 MB asked; Android `rmem_max` 8 MB): 0 RcvbufErrors over
~106 000 datagrams, budget 17.6–31 Mbit/s, 9.2 Mbit/s sent (mean of 60 s, peak 19.6, the movie's
need), viewer loss 0.00 %, 0 unrecoverable frames. On Wi-Fi the same viewer ran at 3.4–5.5 Mbit/s
(the link's limit). Not tested: Ethernet, the iMac viewer (macOS buffer limits), desktop Linux
viewers (Fedora `rmem_max` 4 MB here; many distributions 208 KB, which caps the buffer).

Setup traps on the way (not MirrorWire bugs): an always-on VPN on the tablet (eduVPN, all apps)
routed `10.38.14.0/24` into `tun0`, so the viewer never reached the presenter; and Fedora gave the
tethering link a default route (metric 100, below Wi-Fi's 600), sending the laptop's internet
through the tablet (`nmcli connection modify … ipv4.never-default yes`). The presenter must be
started after tethering is up to advertise the new address.

### 3.9 NACK resend under simulated loss (box, 2026-11-17)

`scripts/loopback-test.sh 20 --encoder x264 -- --simulate-loss L` (the viewer drops L % of
datagrams, resends included), x264 test pattern, same machine; baseline = build 2aea0a3:

| Loss | Build | Lost frames | Keyframe requests | capture→decoded mean / p95 / max (ms) | NACK |
|---|---|---|---|---|---|
| 3 % | baseline | 2 | 2 | 13.2 / 15.7 / 100.9 | – |
| 3 % | NACK | 0 | 0 | 12.5 / 14.7 / 76.4 | 2 asked, 2 filled |
| 10 % | baseline | 10 (54 frames skipped waiting for a key) | 12 | 13.5 / 15.8 / 108.1 | – |
| 10 % | NACK | 0 | 0 | 12.4 / 14.7 / 77.4 | 18 asked, 17 filled |

(The 3 % baseline from the task brief was 2 unrecoverable / 4 requests / p95 22.8 ms; runs vary by a
frame or two.) On loopback the RTT is ~0, so the hold time is its 6 ms minimum; on a real link
a lost frame costs up to 2 × RTT + 4 ms of added latency for the frames behind it, instead of a
keyframe. Not measured on Wi-Fi.

Resend budget scaled to 15 % of the ABR target (build 0c9fcc7 vs 9ab6e5f with the fixed
128 KiB/s), 20 s, x264, loopback: 3 % loss: baseline asked 5 / resent 5, 0 lost frames, 0
keyframe requests; new asked 1 / resent 1, 0 lost frames, 1 keyframe request (3 frames skipped;
0 over budget, so not budget related, single-run noise). 10 %: baseline asked 13 / resent 13,
0 lost, 0 requests; new asked 17 / resent 17, 0 lost, 0 requests. Over budget 0 in all four
runs: on loopback at 40 Mbit/s the old 128 KiB/s already sufficed, so this shows no
regression, not a gain; the gain is expected only for big IDRs on lossy links (not measured).

Frame checksum (protocol 5), same setup, NACK build vs NACK + CRC-32, capture→decoded
mean / p95: 0 % loss 12.5 / 15.0 → 11.9 / 14.1 ms, 3 % 11.5 / 13.6 → 12.0 / 14.3 ms, 10 %
12.0 / 13.0 → 12.3 / 14.6 ms; checksum failures 0 in all runs (run-to-run spread is about
±1 ms; no measurable cost). CRC speed: 1.6 GB/s (`crc::tests::throughput`, release).

### 3.8 Real Wi-Fi: parity probe A/B (2026-11-17)

Presenter in the dev container (laptop Wi-Fi 192.168.201.132), TB336FU tablet on the same
2.4 GHz network (2462 MHz, 11n, RSSI −76/−77 dBm, PHY Tx rate 6–39 Mbit/s and changing
between runs; ping 15–220 ms, mean 91), adb over USB for control only, APK 179475888.
`scripts/android-wifi-run.sh 60 … ` with `ENCODER=vaapi`, test pattern, alternating
before (90f3ee3) / after (045ae3f + probe log lines), three pairs. To repeat it:
`scripts/android-ab.sh 90f3ee3 HEAD 3 60 <out-dir>` builds both revisions, alternates them,
retries a run once if the viewer never connected, logs RSSI / PHY rate per run to `wifi.txt`
and ends with `scripts/run-stats.py` (which also counts probes: started, clean, cut). Check
the RSSI first (`adb shell dumpsys wifi | grep -m1 mWifiInfo`): at −77 dBm the link swung more
than the builds differ. Per run (before | after):

| Pair | Video Mbit/s | Cuts | QUIC lost | Unrecoverable | Waiting for key (frames) | Queue p95 ms | Shown fps |
|---|---|---|---|---|---|---|---|
| 1 | 6.3 \| 8.5 | 11 \| 11 | 0.4 \| 0.0 % | 15 \| 0 | 134 \| 115 | 860 \| 1425 | 44.6 \| 44.1 |
| 2 | 9.7 \| 5.5 | 10 \| 16 | 1.8 \| 5.6 % | 110 \| 170 | 199 \| 427 | 260 \| 1162 | 44.3 \| 36.7 |
| 3 | 3.7 \| 6.5 | 21 \| 8 | 7.2 \| 0.6 % | 254 \| 22 | 566 \| 62 | 1143 \| 291 | 37.8 \| 47.1 |
| Sum / mean | 6.6 \| 6.8 | 42 \| 35 | 2.5 \| 1.8 % | 379 \| 192 | 899 \| 604 | | 42.2 \| 42.6 |

Probes (after only): 15 started, 8 clean, 6 ended by a cut, 1 still running at the end; all
6 cuts were queue cuts (max 58–331 ms), none loss. Probe size on this link 0.1–0.2 Mbit/s.
Reading: the probe works on hardware as designed (it runs, commits, and its failures are
caught by the queue before loss). Better totals after (half the unrecoverable frames, a third
fewer frames waiting for a keyframe), but the link itself changed more between runs than
the two builds differ (the worst run of each build was on a 6 Mbit/s PHY rate), so this is
not evidence that the probe is better, only that it does not break anything. A cleaner A/B
needs a stable link (5 GHz, or the T480s → iMac setup). An earlier 15 s check on the same link
dropped to the 3 Mbit/s floor with 24.5 % QUIC loss and an RTT of ~1 s before any probe:
this 2.4 GHz link at −77 dBm is at its limit.

**Closer to the router (same day, same setup):** tablet RSSI −59/−60 dBm, PHY 65 Mbit/s
(still 2.4 GHz), ping 3–202 ms. Same A/B, before | after:

| Pair | Video Mbit/s | Cuts | QUIC lost | Unrecoverable | Waiting for key | Queue p95 ms | Shown fps |
|---|---|---|---|---|---|---|---|
| 1 | 13.6 \| 15.8 | 6 \| 5 | 178 \| 1 of ~95k | 8 \| 0 | 7 \| 0 | 267 \| 152 | 50.0 \| 49.9 |
| 2 | 14.4 \| 11.0 | 6 \| 5 | 484 \| 2 of ~80k | 20 \| 0 | 8 \| 0 | 17 \| 24 | 50.2 \| 46.0 |
| 3 | 28.5 \| 28.5 | 0 \| 0 | 1 \| 0–1 | 0 \| 0 | 0 \| 0 | 7 \| 7–8 | 50.0 \| 49.9–51.8 |

All cuts were queue cuts. Probes (after, pairs 1–2): 14 started, 13 clean, 1 ended by a queue
cut (max 112 ms). In pair 3 the link carried everything the test pattern needs (28.5 Mbit/s)
for both builds: no cut, so no probe (two after runs, one before run; one before run lost
its handshake to the link once and was repeated). On the two congested pairs the probe build
lost 3 instead of 662 QUIC packets and 0 instead of 28 frames, with no frame waiting for a
keyframe (15 before), for −4 % video (13.4 vs 14.0 Mbit/s mean; pair 2 after sent 11.0).
Two pairs are few, but here the difference is larger than the run-to-run spread.
Shown fps stays at ~50 of 60 in every run, both builds (the open display-side item).

### 3.10 Hardware: VAAPI low-power, reopen cost, presenter remote (owner/manager, 2026-11-18)

Presenter: T480s dev container (Debian 13, Intel UHD 620, `intel-media-va-driver-non-free`,
H.264 only as `EncSliceLP`), KDE Plasma Wayland, portal capture, `--encoder vaapi`; viewer:
TB336FU tablet over Wi-Fi. Run: `scripts/android-wifi-run.sh 60` plus the owner's movie session.

- VAAPI opens on the low-power entrypoint (`low_power=1` retry). A bitrate change reopens the
  encoder in about 27 ms, about 3 cuts per minute; the viewer showed about 50 fps, 0 unrecoverable
  frames, and the owner saw no stutter. So the reopen path is accepted (roadmap 0.6 closed).
- On-the-fly quality changes work on hardware.
- Presenter remote works end to end: KDE RemoteDesktop portal, Android buttons, a real document.

## 4. Real hardware

The owner tests on the target machines (decisions D11) and reports with **photos of the
screen**, not logs. So:

- Every state and error must be readable in the window: presenter status panel (phase,
  listen address, encoder, source, fingerprint, pairing code, connected viewer) and viewer
  connection status and stats overlay (`S`).
- Error messages say what to do ("No answer from 192.168.1.20:47800 (UDP). … On Fedora:
  sudo firewall-cmd --add-port=47800/udp"), not just what failed.
- Ask for exactly what you need ("press S and photograph the top-left corner").
- Don't ask him to install a toolchain on the iMac for routine debugging; build artifacts in CI.
- Pen pressure (D26), not run yet: on the T480s click "Check pen pressure support on my
  system" before and after the udev rule (photo of both results), tick "Enable pen pressure",
  draw in Krita from the Android viewer with light and hard strokes, and check that "MirrorWire
  virtual pen" shows in System Settings → Drawing Tablet. Then run the undo commands and check
  again (it must fail, and pen mode must still work as a plain pointer).
- Pen must never stop the video (owner report t316u, 2026-11-25), not run yet: start sharing,
  connect the Android viewer, press **Pen** and draw for 30 s, with pen pressure off and then
  on. The tablet picture must keep moving (an on-screen clock in the shared screen shows it)
  and the presenter log must not say "viewer silent … video paused". With pen pressure off and
  the combined session refused, no second "choose the screen" dialog may appear; the window
  shows "pen/touch is not available: capture and input could not share one portal session".
  Unit guards: `undrained_pen_input_does_not_stop_the_video` (net, e2e),
  `input_forward_tests` (net), `a_blocking_pen_device_does_not_stall_the_network_runtime`
  (app), `the_fallback_injector_never_opens_a_session_for_the_pointer` (media, `portal`).

## 5. CI and artifacts

- `ci.yml` (push to `main`, PRs): `cargo fmt --all -- --check`, clippy `-D warnings`, `cargo test`, 5 s loopback with x264,
  then clippy/tests/5 s loopback again with feature `filmcraft` and `--encoder filmcraft`,
  the reusable audit workflow, and the reusable SonarCloud scan (`needs: audit`).
- `fuzz.yml` (nightly cron 03:17 UTC + manual): the only workflow on **nightly** Rust (D18). Media and film tests under AddressSanitizer, then 60 s each of the cargo-fuzz targets in `fuzz/` (`film_decode`, `proto_parse`, `reassembler`). Local: `cd fuzz && cargo +nightly fuzz run proto_parse -- -max_total_time=20`. Crashes upload as an artifact.
- `sonarcloud.yml` (called from CI; also from Build on `v*` tags and
  `workflow_dispatch`; can be dispatched on its own): if the repository variable
  `SONAR_CI_ANALYSIS` is not `true`, or `SONAR_TOKEN` is
  missing (fork PRs), the job succeeds and skips the upload. Otherwise it
  writes a Clippy JSON report (`cargo clippy --message-format=json`) and
  unit-test coverage (`cargo llvm-cov --lcov`), then runs
  `SonarSource/sonarqube-scan-action` pinned to a full SHA. Report paths
  live in `sonar-project.properties` (`sonar.rust.clippyReport.reportPaths`,
  `sonar.rust.lcov.reportPaths`). The loopback script is not included in
  coverage, but the in-process loopback unit test is. `sonar.coverage.exclusions`
  lists the few whole files that can't run headless (egui/GL window code,
  portal/PipeWire capture, VAAPI/VideoToolbox device glue, firewalld D-Bus
  glue, the Android shell), each explained in a comment; keep it file-level
  and don't add logic files to it. The project uses the built-in *Sonar way*
  gate (new code coverage ≥ 80 %); custom gates need a paid plan. **Automatic Analysis must
  be off** (and `SONAR_CI_ANALYSIS=true` set) for the CI scan; under SonarCloud Administration › Analysis method for
  `windwerfer_mirrorwire`; if it is still on, the upload fails and should
  not be worked around in the workflow.
- `codeql.yml` (push to `main`, pull requests, weekly Wednesday cron, and
  `workflow_dispatch`; also called from Build on `v*` tags and
  `workflow_dispatch`): GitHub CodeQL advanced setup for `rust` and
  `actions`, both with `build-mode: none` (no Linux media deps, no workspace
  compile). `github/codeql-action` init/analyze are pinned to a full SHA.
  Results upload to Security › Code scanning. **CodeQL default setup must
  be off** under Settings › Code security › Code scanning; if it is still
  on, the advanced workflow fails and should not be worked around.
  Branch-push Builds do not re-run this job, because the standalone
  workflow already ran on the same push.
- `audit.yml` (called from CI and Build; also `workflow_dispatch` and a weekly Monday cron):
  `cargo audit` against the RustSec advisory DB, and
  `cargo deny check advisories bans licenses sources` with `deny.toml`.
  `cargo audit` fails only on vulnerabilities; yanked and unmaintained crates are warnings.
  `cargo-deny` does the same for yanked crates, ignores unmaintained advisories (it has no
  warn-only level for them; `cargo audit` still prints those), fails on unsound crates and
  on any licence or source outside the allow-list, and warns on duplicate crate versions.
  Only crates.io is an allowed source. If an advisory hits a transitive dependency with no
  fix yet, add a justified `{ id = "RUSTSEC-…", reason = "…" }` ignore in `deny.toml` (and
  `--ignore` for `cargo audit` if needed) and report it; do not silently pass a vulnerability.
- `build.yml` (push to `main`, `v*` tags): the macOS, AppImage and Flatpak jobs all `needs:
  audit`, so a vulnerable or policy-violating lockfile never produces an artifact. Then:
  macOS universal DMG (arm64 slice built on `macos-15`, x86_64 slice on `macos-15-intel` with min 10.13, merged with `lipo`), Flatpak,
  AppImage (Ubuntu 24.04); a `v*` tag creates a GitHub Release with all four. The Intel Mac
  job is the slowest (static FFmpeg build on older runners); that is normal.
- Artifacts are uploaded with `archive: false`, so each download is the bare `.dmg` /
  `.flatpak` / `.AppImage`.
- **Re-runs hide artifacts:** if a job is re-run, the run page shows the latest attempt, which
  only has the re-run job's artifact. The others are on `…/actions/runs/<id>/attempts/1`. When
  you hand out links, check every artifact exists and link the attempt that has it (`gh api
  repos/windwerfer/mirrorwire/actions/runs/<id>/artifacts`).
- Do not change workflows as a side effect of a feature. A workflow change is its own commit.
