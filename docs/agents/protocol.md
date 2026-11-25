# Protocol

Everything here lives in `mirrorwire-proto` (constants, framing) and `mirrorwire-net`
(handshake, URI, mDNS). If you change any of it, follow §4.

## 1. Connection

- QUIC over UDP, default port **47800** (`DEFAULT_PORT`), ALPN **`mirrorwire/1`** (`ALPN`,
  fixed; the protocol version is `hello.protocol`, §4).
  The presenter port is configurable (`--port`, `MIRRORWIRE_PORT`, saved `settings.json`).
  A changed port is advertised in mDNS (`ServiceInfo` port) and in the URI (`mirrorwire://ip:port?…`);
  the viewer always uses the advertised port, never a hard-coded 47800.
- TLS 1.3 with self-signed certificates on both sides (`rcgen`, ring provider).
  - The viewer's verifier accepts the presenter's cert **only** if its SHA-256 fingerprint
    (64 lowercase hex chars) equals the pinned one from the URI/mDNS.
  - The presenter accepts any client cert at the TLS layer, then authorizes in the handshake:
    the fingerprint is in `paired_viewers.json`, or the `Hello` carries the right pairing code
    (then the fingerprint is added), or pairing is off (`--no-pairing`).
- Idle timeout 10 s. Datagram send buffer 2 MiB (`DATAGRAM_SEND_BUFFER`); when it is too full
  for a delta frame, the presenter drops that frame and forces a keyframe instead of queueing
  (keyframes are always queued). Pacing 500 Mbit/s (`lan_cc.rs`); on top, the send loop spreads
  a keyframe over time at 1.25 × the bitrate budget and the frames that waited behind it at
  1.5 ×, at most 100 ms per frame (`rate::pace_spread_us`), so a keyframe does not overflow a
  shallow bottleneck buffer. Not a wire change: the viewer sees the same packets, later.

## 2. Control stream

One bidirectional stream opened by the viewer. Each message is a 4-byte big-endian length
followed by a JSON body (`serde`, `tag = "type"`, `snake_case`). Bodies over
`MAX_CONTROL_MESSAGE` (64 KiB) are an error.

| `type` | Direction | Fields |
|---|---|---|
| `hello` | viewer → presenter | `protocol` (u32, must equal `PROTOCOL_VERSION`, currently **12**; read first, §4), `name`, `pairing_code` (string or null), `display` (`{width, height, scale_milli}`: the viewer's screen in physical pixels and its scale factor ×1000, protocol 8; the presenter keeps it only if both sides are 1..=16384 and the scale is 0.25–8, and Extend mode sizes its virtual monitor from it) |
| `rejected` | presenter → viewer | `reason` (shown to the user) |
| `stream_info` | presenter → viewer | `width`, `height`, `fps`, `codec`, `encoder`, `bitrate_kbps`, `intra_refresh`, `cursor` (`separate` = §3a, `embedded` = in the video, `hidden`), `fec` (the presenter's FEC setting `off`/`auto`/`20%`, for display). Sent after `hello`, and **again** whenever it changes |
| `request_keyframe` | viewer → presenter | `reason`. The presenter forces at most one keyframe per 2 × RTT (250 ms–1 s) per viewer and ignores requests in between. The viewer asks at once, then backs off 250 ms, 500 ms, 1 s, 1 s … until a keyframe arrives (`KeyframeBackoff`), so a lost keyframe is still replaced without a request storm; frames that arrive while it waits are still counted in `link_report`. The encoder drops a request that arrives when the previous frame sent was already an IDR (`Controls::take_keyframe_request`: no forced IDR directly after an IDR) |
| `link_report` | viewer → presenter | `loss_permille`, `packets`, `recovered`, `unrecoverable`, `rx_kbps` (u32) and `window_ms` (u32) over the last ~0.5 s (`rx_kbps` = wire rate of video packets that arrived, first transmissions only, parity and headers included; `window_ms` = the window's length; §3b), and `queue`: `{mean_us, max_us, trend_us_per_s}` (u32, u32, i32), the one-way queueing delay of the window's frames (mean, max) and its least-squares slope (§3c), or null when no frame arrived. Not sent for a window without packets |
| `bottleneck` | viewer → presenter | `stage` (`none`/`network`/`viewer`/`driver`), `rating` (`good`/`ok`/`weak`): the viewer's coarse bottleneck verdict (architecture.md, Bottleneck line), sent when it changes (protocol 7). The presenter window shows it as `viewer sees: …` after the bitrate line; it never feeds rate control |
| `remote_key` | viewer → presenter | `key` (`next`/`previous`/`space`/`escape`): one tap of a presenter-remote button (protocol 9). A closed set, never raw key codes. The presenter taps the key (Right, Left, Space, Escape) through the RemoteDesktop portal, at most 10 taps/s, and ignores the rest; no reply. A presenter without a granted portal session drops it and shows why in its window |
| `pointer` | viewer → presenter | `phase` (`down`/`move`/`up`), `x`, `y` (u16, 0..65535 inside the picture, 0 = left/top), `pressure` (u16 0..65535 from the viewer's pen/touch sensor, or `null` when the device reports none, e.g. a mouse; required, protocol 11): pen/finger as an absolute pointer (protocol 10). `down` presses and `up` releases the left button, `move` only moves. Reliable control stream; the viewer coalesces moves to ~60/s, the presenter allows 120 moves/s and 40 downs/s (ups always) and maps the position onto the captured monitor through the RemoteDesktop portal (which ignores `pressure`; the portal has no pen device), or, when the presenter's opt-in "Enable pen pressure" is on (Linux, D26), onto a virtual uinput pen with `ABS_PRESSURE` (`null` = full pressure). Out-of-range or unknown values and a missing `pressure` fail to parse and are dropped; no reply |
| `pen_mode` | viewer → presenter | `on` (bool, required; protocol 12): the viewer's Pen button changed (also its "Wacom pen" mode). On `true` a presenter whose capture runs without input reopens it as one RemoteDesktop session with keyboard + pointer, so the desktop's permission dialog appears now, not at presenter start; strokes are dropped until that session is in place. A `pointer` from a viewer that never sent `pen_mode` (button already on before a reconnect) counts as `true`. `false` changes nothing on the presenter (the permission stays for the run). Rate limited to 10/s with its own budget; no reply |
| `video_paused` | viewer → presenter | `paused` (bool, required; protocol 12): the viewer shows no video ("Wacom pen" or "PowerPoint remote only" mode). While `true` the presenter skips encoding and sending (as with no viewer); `false` resumes with a keyframe (keyframe gate). A state, not an event: only changes count, no rate limit. Every new connection starts with `false`; the viewer sends its state again after connecting. No reply |
| `ping` / `pong` | viewer ↔ presenter | `t_viewer_us`, `t_presenter_us` (clock offset + RTT) |
| `bye` | either | — |

Every field is required (a missing or mistyped one is a parse error that closes the session);
unknown fields are ignored. Any change to a message bumps `PROTOCOL_VERSION` (§4).

## 3. Video datagrams

One packet format, with or without FEC (`mirrorwire-proto/src/fragment.rs`). Each encoded frame
(H.264 Annex B access unit) is cut into `data_count` equal data shards of `shard_len` bytes
(even) that fit `conn.max_datagram_size()`; the last data shard goes out unpadded. While the
presenter's FEC setting isn't `off`, `parity_count` parity shards follow (§3b); with `off` it is
0. Data packets go out first, then parity (paced by the congestion controller like everything
else). Every packet starts with a **48-byte big-endian header** (`HEADER_LEN`):

| Offset | Size | Field |
|---|---|---|
| 0 | 1 | wire version (`WIRE_VERSION` = 3); other values are dropped |
| 1 | 1 | flags: bit 0 = keyframe; bit 1 = resent (a retransmission after a NACK, §3d); other bits reserved, send 0 |
| 2 | 4 | `frame_id` (u32, increments per frame) |
| 6 | 2 | `index`: `0..data_count` data, then `data_count..data_count+parity_count` parity |
| 8 | 2 | `data_count` (≥ 1) |
| 10 | 2 | `parity_count` |
| 12 | 4 | `frame_len` (bytes of the encoded frame, ≤ 64 MiB) |
| 16 | 2 | `shard_len` |
| 18 | 1 | `blocks` (§3b; 1..=255, ≤ `data_count`) |
| 19 | 1 | reserved, 0 |
| 20 | 8 | `capture_us` (presenter clock) |
| 28 | 8 | `encoded_us` |
| 36 | 8 | `sent_us` |
| 44 | 4 | `frame_crc`: CRC-32 (IEEE, `crc::crc32`) of the whole encoded frame, the same in every packet of it |

Receivers drop packets whose header is inconsistent (zero blocks, `frame_len` not matching the
counts, payload length not what the index implies) or whose layout differs from the first packet
seen for that frame, and keep at most 64 partial frames. The receiver reassembles by
`frame_id` and delivers in order; an incomplete frame is first asked for again (§3d), and when
that fails it is dropped, reported as lost, and the viewer asks for a keyframe unless the
stream uses intra refresh.

The header costs 14 bytes more per packet than the old version-1 fragments (34 bytes), about
1 % of a 1400-byte packet, in exchange for a single receive path and a frame checksum.

**Frame integrity.** When a frame completes (all data shards present, some possibly rebuilt
from parity or filled by a resend), the viewer computes the CRC-32 of the reassembled bytes and
compares it with `frame_crc`. A mismatch means our reassembly or FEC produced a wrong frame
(QUIC authenticates every packet, so the wire cannot corrupt one): the frame is never given
to the decoder, counts as lost (keyframe request, §2), and is counted separately; the viewer
HUD shows `CORRUPT FRAMES n (checksum mismatch)` and the loopback summary prints
`checksum failures`. It should stay 0. Packets of one frame that disagree on `frame_crc` are
treated as a layout mismatch and dropped.

## 3a. Cursor as coordinates (`stream_info.cursor` = `separate`)

All in `mirrorwire-proto/src/cursor.rs`.

**Pointer datagram** (unreliable, one per change, not tied to video frames). The first byte
`0xC1` can't collide with a video packet (whose first byte is `WIRE_VERSION` = 3).
Exactly 32 bytes, big endian (other lengths are dropped; a later tool such as a pen with
pressure/tilt changes the layout and bumps `PROTOCOL_VERSION`):

| Offset | Size | Field |
|---|---|---|
| 0 | 1 | kind `0xC1` |
| 1 | 1 | version 1 (other values dropped) |
| 2 | 1 | flags: bit 0 visible |
| 3 | 1 | tool: 0 = cursor (future: pen, touch) |
| 4 | 4 | `seq` (u32, wrapping; the viewer drops older/duplicate ones) |
| 8 | 4 | `x` (i32, hotspot position in captured-frame pixels) |
| 12 | 4 | `y` (i32) |
| 16 | 2 | `frame_w` (captured frame width, before encoder scaling) |
| 18 | 2 | `frame_h` |
| 20 | 4 | `image_id` (0 = none yet) |
| 24 | 8 | `capture_us` (presenter clock) |

**Cursor image**: one unidirectional QUIC stream per shape (presenter → viewer), at most
`MAX_IMAGE_STREAM` bytes: magic `MWCI`, version 1, 3 reserved, `id` u32, `width` u16, `height`
u16 (each ≤ 512), `hotspot_x` i32, `hotspot_y` i32, 4 reserved, then `width*height*4` bytes of
premultiplied RGBA. 0×0 = invisible cursor. The viewer caches the last 32 images by id; a new
viewer gets every cached image and the last position on connect.

**Drawing**: `cursor_rect()` maps `(x - hotspot, y - hotspot)` and the image size through the
rectangle the video is drawn in (`video.w / frame_w`), so fit, letterbox and 1:1 modes all
line up. The same message is meant to carry the future pen/Wacom mode.

## 3b. Forward error correction

Parity shards in the packets of §3, while the presenter's FEC setting isn't `off`. Code in
`mirrorwire-proto/src/fec.rs` (erasure code: `reed-solomon-simd`, MIT AND BSD-3-Clause).

Data shard `i` belongs to block `i % blocks`, parity shard `j` to block `j % blocks`
(`blocks = ceil(data_count / 64)`, at most 255), so consecutive packets hit different blocks and
a burst costs each block one or two packets. Each block is a Reed-Solomon code over its data
shards (zero-padded to `shard_len` for coding) and parity shards: **any** `k_b` of its
`k_b + m_b` shards rebuild it, immediately, without a round trip.

**Overhead** (`FecMode::overhead`): `auto` gives delta frames 10 % + 3 × loss + boost (10–35 %)
parity and keyframes 25 % + 5 × loss + 1.5 × boost (25–60 %), from the loss the viewer reports in
`link_report` (smoothed: rises at once, decays slowly). The boost (0–15 points) grows by 5 for
every report with unrecoverable frames and shrinks by 1 per clean report, because bursty loss
needs more parity than its average suggests; it does not grow while the adaptive bitrate sees
congestion (parity would only add load). A fixed setting `N%` gives delta frames N % and
keyframes 1.5 × N %. At least one parity packet per block (two for keyframes). Parity is paid
from the bitrate budget: the encoder target is the video budget / (1 + delta overhead); while
the adaptive bitrate probes (below), delta frames carry extra parity so that video + parity
fill the whole budget (`rate::probe_overhead`, at most 60 %; keyframes keep theirs).

**Delivery** (second bitrate signal, protocol 6): the presenter keeps what it sent per frame
for 4 s (`rate::SentHistory`) and, when a `link_report` arrives, compares `rx_kbps` with its own
wire rate over the same `window_ms` (ending RTT/2 ago). It is only used when at least 1 Mbit/s
was sent. A queue cut aims at 90 % of what the link carried (`rx_kbps`, but not below half of
what was sent in the window, and not above what was sent) instead of 90 % of what was sent, so
a saturated bottleneck is hit in one step; and the budget does not grow while 8 points or more
of what was sent are missing beyond both the reported packet loss and the learned random loss
(frames lost whole, which are not in the packet counts, show up here). A still screen never
raises the budget: it sends less than 80 % of it, which already blocked growth (`USED_SHARE`).

**Adaptive bitrate** (presenter; `mirrorwire-proto/src/rate.rs`): the same `link_report`
(loss and the queueing delay of §3c) and the presenter's backlog drops drive an AIMD
controller (the QUIC RTT is shown in the status line but not used). Its ceiling is the bitrate setting, its floor
3 Mbit/s. Congestion cuts the budget ×0.85 (×0.75 at ≥ 10 points excess loss, ×0.6 at ≥ 20),
at most once per second. Congestion is: a backlog drop; loss ≥ 3 points above the learned
background loss in two reports in a row, or ≥ 10 points once; or a queue:
one report with a queue building (mean ≥ 25 ms or max ≥ 50 ms, and rising ≥ 30 ms/s) or standing
(mean ≥ 60 ms, not within 2 s of a cut while it drains) is enough, and that cut also goes to
≤ 90 % of the wire rate sent in the last second. Loss with no frame waiting ≥ 40 ms in this or
the previous report counts as random (no cut, FEC covers it) unless it is ≥ 20 points. A clean link (< 1.5 points excess, no queue) adds 4 % of the ceiling
per second from 4 s after the last cut. Each cut remembers what the link did not carry (the
budget, or the wire rate sent if less); from 90 % of it up, loss above the background counts
even with a flat queue, and up to 115 % of it the budget grows by a parity probe: a quarter of
the normal step, up to 3 % above the video budget, filled with extra FEC parity; after 3 s
of clean reports (any report with loss ≥ 1.5 points excess or a queue restarts the 3 s) the
video budget follows. A cut lowers both. Keyframes are paced on the video budget. A new viewer starts at 10 Mbit/s (or the ceiling if
lower) in slow start: ×1.25 per second until the first cut, through loss below the cut
threshold. A viewer whose link failed before (same certificate fingerprint and address, within
10 min, presenter memory only) instead starts at min(the budget it left with, 60 % of the
budget that failed last), keeps that failure for the hysteresis and slow-starts only to 90 % of
it (`AdaptiveBitrate::resume`). Presenter-side only; no wire change. The budget only grows while the presenter sends ≥ 80 % of it (wire rate, video +
parity, per 1 s send window), so it stays near what has actually been sent. Loss that stays at ≥ 70 % of its level for 3 s after a
cut is learned as background loss (random Wi-Fi loss that FEC covers). x264 takes the new rate
live (no keyframe); VAAPI/FilmCraft reopen (keyframe): down by ≥ 10 % at most every 1 s, up by
≥ 15 % at most every 5 s.

**Loss accounting**: the viewer counts, per frame, packets sent (`data_count + parity_count`)
against packets received, once the frame is 30 frames old (so late packets still count). Frames
that never arrive at all are not in the packet count; they show up as unrecoverable frames.

## 3c. One-way queueing delay (`mirrorwire-proto/src/delay.rs`)

The viewer measures, for the **first packet that arrives of each new frame**, `arrival − sent_us`
(its clock minus the presenter's; the unknown clock offset is constant). The lowest value of the
last 30 s (per-second minima) is the empty-queue baseline; `delay − baseline` is the time the
packet waited in a queue. The last packet is not used: it also waits for the frame's own packets
to cross the link, which is not a queue left by earlier frames. A value above 2 s is taken as a
clock step and restarts the baseline; clock drift (ppm) is absorbed by the 30 s window. Per
`link_report` window: mean and max of the frames' queueing delay, and the least-squares slope over
arrival time (µs per s). The viewer overlay shows it as `queue N ms (max N, +N ms/s)`. No wire
change for video packets; `sent_us` was always there.

## 3d. Retransmission on NACK (`mirrorwire-proto/src/nack.rs`, protocol 4)

FEC (§3b) covers random loss at once; what it cannot rebuild is asked for again.

**Receiver** (`Reassembler::take_nacks`). A frame is *settled* when a newer frame has started,
its last packet (the final parity shard) arrived, or it has been quiet for 15 ms (lost tail on
a static screen). For a settled, incomplete frame the viewer asks, per unfinished FEC block, for
as many missing **data** shards as the block still lacks (parity that arrived counts), so the
fewest packets that complete it. A frame of which nothing arrived is asked for whole. Each fragment is asked for again
after RTT + 2 ms, three rounds at most. Frames are still delivered strictly in order: an
incomplete frame holds the newer ones back for `hold_us` = 2 × RTT + 4 ms (6–100 ms); only
when that expires (measured from the first packet of the oldest newer frame, so a run of missing frames is given up at once, not one `hold_us` each) is the frame **lost** (`unrecoverable`, and the viewer asks for a keyframe,
§2). So a keyframe is requested only if the resend missed about one round trip. Resent packets
are not counted in `link_report` loss or in the queueing-delay measurement (they are not first
transmissions); the HUD shows `resent N`.

**NACK datagram** (viewer → presenter, unreliable, at most 1000 bytes, big endian):
`0xC2`, version `1`, `n` (u8, 1..=255 entries), then per entry `frame_id` u32, `count` u8
(0 = the whole frame, i.e. every data shard), `count` × packet index u16. Anything else
(other length, trailing bytes, zero entries) is dropped.

**Sender** (`ResendWindow`). The presenter keeps the packets of every frame it sent for 3 s,
at most 8 MiB, and clears them when a viewer connects. A NACK is answered with copies of the
asked packets, flag bit 1 set, otherwise byte for byte the original (the original timestamps
stay). Resends have their own budget: 15 % of the current ABR target (total wire budget), at least
128 KiB/s and at most 2 MiB/s (token bucket, burst half a second of the rate, at least 64 KiB;
the presenter updates the rate before answering each NACK; no wire change); what does not
fit, and frames already out of the window, are counted (`resent X of Y asked (…over budget…)`
in the presenter log) and left to the viewer's keyframe request. A frame the presenter never
sent (dropped for backlog) is simply unknown to it.

Why datagrams and not the control stream: a NACK that is itself lost is repeated by the
receiver after a round trip anyway, and a datagram does not queue behind control traffic.

## 4. Versioning rules

There is **no backwards compatibility** (decisions.md D6): presenter and viewer must speak
the same `PROTOCOL_VERSION` (`lib.rs`), and any incompatible change (a control message or
field, a datagram layout, handshake or auth semantics, mDNS TXT keys, URI parameters) bumps
it. Don't add optional fields, capabilities or fallbacks to keep an older peer working:
change the format and delete the old path.

The presenter reads `hello.protocol` before parsing the rest of the hello
(`control::hello_protocol`), so a viewer of any version gets a `rejected` message the viewer
GUI shows ("version mismatch: the presenter speaks protocol X, the viewer Y; update the
viewer …"), not a timeout or a parse error. That check is the one thing that must keep working
across versions: `hello` stays a JSON object with `"type":"hello"` and a numeric `protocol`.
The ALPN stays `mirrorwire/1` (D5); an ALPN mismatch would fail inside TLS with no readable
message.

In the same commit: update this file, the README if users see it, add a test in
`mirrorwire-proto` / `mirrorwire-net` that pins the new layout (encode → bytes → decode), and note
the change in `CHANGELOG.md`.

## 5. Discovery and pairing

- **mDNS:** service `_mirrorwire._udp.local.`, instance name = presenter name (hostname by
  default), TXT `fp` (fingerprint), `v` (the presenter's `PROTOCOL_VERSION` in decimal), `res` (`WxH`). The viewer's list (desktop, Android and
  `mirrorwire discover`) still shows a presenter whose `v` differs or is missing, marked "older/
  newer MirrorWire …: update the presenter/this viewer"; connecting to it gets the `hello`
  rejection (§4).
- **URI / QR:** `mirrorwire://<ip>:<port>?fp=<64 hex>&code=<6 digits>&name=<name>`. `fp` is
  required; `code` and `name` are optional. IPv6 hosts are bracketed.
- **Pairing code:** 6 decimal digits from `getrandom`, new on each presenter start, shown in the
  presenter window and terminal. Only needed on the first connection of a viewer.
