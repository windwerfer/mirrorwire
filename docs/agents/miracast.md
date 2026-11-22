# Miracast sink spike (branch `miracast-spike`)

Supports the Miracast™ protocol (not Wi-Fi Alliance certified). Miracast is a trademark of Wi-Fi Alliance. Naming rules: decision D22 (descriptive use only, never as a product name, no logo, no "certified").

Status: **research only (step 1 of 6)**. Nothing here is implemented or hardware-tested.
Everything below is written from the specs and general knowledge, clean-room (no MiracleCast,
gnome-network-displays or other GPL code was read). Items marked **[verify]** are unconfirmed
facts that the later steps or the owner's hardware run must check. The research box (a VM
container) has no Wi-Fi radio, no `iw`, no NetworkManager, so nothing about the T480s was
measured here; commands to measure it are in section 9.

Specs to work from: Wi-Fi Display Technical Specification (Wi-Fi Alliance, v1.1; message and
parameter grammar), Wi-Fi Peer-to-Peer (P2P) Technical Specification (GO negotiation, IEs),
RFC 2326 (RTSP 1.0), RFC 3550 (RTP), RFC 2250 (MPEG-TS over RTP), ISO/IEC 13818-1 (MPEG-TS),
ITU-T H.264, NetworkManager D-Bus API reference (`org.freedesktop.NetworkManager.*`).

Goal: **sink** role first. An Android tablet (TB336FU, "Enable wireless display" in Cast
settings) is the source; the Fedora KDE laptop is the sink and shows the stream (later through
MirrorWire's decoder and viewer path). Rootless: NetworkManager D-Bus + Polkit only.

## 1. Fedora packages and permissions

| Need | Package | Notes |
|---|---|---|
| NM with Wi-Fi P2P | `NetworkManager`, `NetworkManager-wifi` | P2P device support exists since NM 1.16 (`NMDeviceWifiP2P`, `NMWifiP2PPeer`, connection type `wifi-p2p`). Current Fedora ships far newer. **[verify]** `rpm -q NetworkManager` |
| P2P in the supplicant | `wpa_supplicant` | Must be built with `CONFIG_P2P` (+ `CONFIG_WIFI_DISPLAY` for WFD IEs). **[verify]** that Fedora's build has both: `wpa_cli` is not needed; check that NM exposes a `p2p-dev-wlp…` device in `nmcli device`. |
| Driver/firmware | `kernel`, `linux-firmware` | Intel 8265: `iwlwifi` + `iwlmvm`, firmware `iwlwifi-8265-*.ucode`. |
| RTSP/RTP/TS | none | Written in Rust in the new crates; no GStreamer or FFmpeg demuxer needed for the spike (FFmpeg only as a test-stream generator). |
| D-Bus client | crate `zbus` (MIT) | Pure Rust, check licence and size at step 3. |

**Polkit.** All calls go through NetworkManager's own actions, no custom rule file should be
needed for an active local session **[verify with `pkaction --verbose`]**:
`org.freedesktop.NetworkManager.network-control` (activate/deactivate connections, default
`yes` for active sessions), `…settings.modify.system` or `…settings.modify.own` (adding a
connection profile; use an in-memory/volatile profile to avoid storing anything),
`…wifi.scan` (start P2P find). If a prompt appears on a given setup, document a minimal rule in
`/etc/polkit-1/rules.d/` as an option for the user, never as a requirement we install.
Direct access to `fi.w1.wpa_supplicant1` is **rejected**: its D-Bus policy is root-only and it
would fight NM for the interface.

**Firewall (firewalld, Fedora KDE default zone `FedoraWorkstation`).** The sink needs inbound
TCP **7236** (RTSP, the sink listens) and inbound UDP on the RTP port the sink announces
(default **19000**; RTCP port+1 if used). `FedoraWorkstation` already allows 1025-65535 TCP/UDP
so this works on a stock install. A P2P group interface that lands in a stricter zone needs
those two ports; if the sink is the GO and NM runs DHCP for it, UDP 67 must be reachable too
(NM's `shared` method uses the `nm-shared` zone, which allows DHCP/DNS). The MirrorWire
Flatpak has `--system-talk-name=org.fedoraproject.FirewallD1` and, since 2026-11-22 (owner
decision D24), `--system-talk-name=org.freedesktop.NetworkManager` for the NM Wi-Fi P2P API,
and `--system-talk-name=org.freedesktop.hostname1` (owner, 2026-11-22, D25) to read the
hostname for the "Name shown on Android" line.
(The spike itself added no permission and ran as a native binary.)

## 2. Network structure

1. **P2P discovery.** Both sides run P2P find (social channels 1/6/11 on 2.4 GHz). The sink
   publishes a **WFD IE** in its P2P probe responses/beacons; the source sees "a WFD sink named
   X". NM: `Device.WifiP2P.StartFind` → signals `PeerAdded/PeerRemoved`, peers appear as
   `NMWifiP2PPeer` objects (name, address, `WfdIEs`, strength).
2. **Group formation.** Either P2P GO Negotiation (both sides send GO intent 0-15, the higher
   wins, tie-break bit) or, for an already-existing group, invitation. Provisioning is WPS
   (PBC is normal for Miracast; PIN for some devices). NM: `ActivateConnection` with a
   `wifi-p2p` profile naming the `peer` MAC and `wps-method`; NM brings up a `p2p-wlp…-N`
   group interface. **Open risk R1:** NM drives *outgoing* connects well; whether it accepts an
   *incoming* GO-negotiation request from the tablet when the user taps our sink in the Cast
   list is **[verify on hardware]**. Fallback: the sink finds the source and connects itself
   (works when the tablet is sitting in its wireless-display screen and is discoverable).
3. **Who is GO.** The spec allows both. Android sources historically set a high GO intent and
   become GO themselves **[verify on the TB336FU]**; the sink then is a P2P client and gets its
   address by DHCP from the source (commonly `192.168.49.0/24`, GO `192.168.49.1`). If the sink
   is GO, NM (`ipv4.method=shared`) serves DHCP. The design must handle both: the sink learns
   the source IP from the group interface (GO address, or the DHCP lease/ARP), never hardcodes
   it.
4. **IP + sessions.** After group formation the source opens a TCP connection to the sink's
   RTSP port **7236** (advertised in the WFD IE "session management control port"). The sink
   listens before the group comes up (bind on `0.0.0.0` or the group interface).
5. **Media.** RTP/UDP, source → sink: the source sends to the sink's `wfd_client_rtp_ports`
   (default 19000), MPEG-TS payload (RTP payload type 33, 90 kHz clock, 7 TS packets =
   1316 bytes per datagram). TCP interleaved transport is optional in the spec; Android uses UDP.
6. **Teardown.** RTSP TEARDOWN (M8), or the group is removed; the sink returns to P2P find.

### WFD information element (what the sink publishes)

A vendor-specific P2P attribute (Wi-Fi Alliance OUI `50:6F:9A`, type `0x0A`) carrying WFD
subelements, each `id(1) len(2) payload`. The sink needs **subelement 0, WFD Device
Information**, length 6: device info bitmap (2 bytes), session management control port
(2 bytes, 7236), maximum throughput (2 bytes, Mbit/s). Bitmap fields, from the spec
**[verify the exact bit positions against the spec text in step 3]**: bits 1:0 device type
(0 source, 1 primary sink, 2 secondary sink, 3 dual role), bits 3:2 session availability, a
service-discovery-support bit, a preferred-connectivity bit (P2P vs TDLS), content-protection
and time-sync support bits, and the audio-only/audio-unsupported bits for sinks. Our value:
primary sink, session available, no HDCP, no time sync, P2P connectivity. Other subelements
(coupled sink info, associated BSSID, extended capability) are optional and left out.
NM carries these bytes in the `wfd-ies` property of the `wifi-p2p` setting (and exposes the
peer's on `NMWifiP2PPeer.WfdIEs`). Missing WFD IEs mean the source will not list us at all, so
this is the first thing to check on hardware.

## 3. RTSP message flow (sink view)

RTSP 1.0 (RFC 2326), source = RTSP client for requests M1/M3/M4/M5/…, sink = client for
M2 and M6/M7. TCP port 7236, `CSeq` per direction. Required feature tag
`org.wfa.wfd1.0`.

| Msg | Direction | Request | Sink does |
|---|---|---|---|
| M1 | source → sink | `OPTIONS * RTSP/1.0`, `Require: org.wfa.wfd1.0` | Reply 200 with `Public: org.wfa.wfd1.0, GET_PARAMETER, SET_PARAMETER` |
| M2 | sink → source | `OPTIONS * RTSP/1.0`, `Require: org.wfa.wfd1.0` | Send; expect 200 with the source's `Public:` list |
| M3 | source → sink | `GET_PARAMETER rtsp://localhost/wfd1.0`, body lists parameter names | Reply with the capability values (below) |
| M4 | source → sink | `SET_PARAMETER`, body = chosen `wfd_video_formats`, `wfd_audio_codecs`, `wfd_client_rtp_ports`, `wfd_presentation_URL` (`rtsp://<src-ip>/wfd1.0/streamid=0 none`), optionally `wfd_uibc_*`, `wfd_content_protection` | Validate the selection is a subset of what we offered, reply 200 (or 4xx and abort) |
| M5 | source → sink | `SET_PARAMETER`, `wfd_trigger_method: SETUP` | Reply 200, then send M6 |
| M6 | sink → source | `SETUP rtsp://<src-ip>/wfd1.0/streamid=0`, `Transport: RTP/AVP/UDP;unicast;client_port=19000` | Parse 200: `Session: <id>;timeout=<s>`, `Transport … server_port=…` |
| M7 | sink → source | `PLAY rtsp://<src-ip>/wfd1.0/streamid=0`, `Session: <id>` | On 200, start RTP receive; source starts sending |
| M8 | either | `TEARDOWN` | Stop media, close, go back to find |
| M9/M10 | sink → source | `PAUSE`/`PLAY` | optional |
| M11/M12 | | standby / `wfd_standby` | optional, reply 200 |
| M13 | sink → source | `SET_PARAMETER wfd_idr_request` | Request a keyframe after loss |
| M16 | source → sink | empty `GET_PARAMETER` | Keepalive; reply 200 within the session timeout (often 30 s; the source sends it about every timeout/2) |

The sink must answer M3 with, at least (`wfd_*` values are text, CRLF separated):

- `wfd_video_formats`: `native cea_mask vesa_mask hh_mask latency min_slice_size slice_enc_params
  frame_rate_control max_hres max_vres` then per-format `profile level …`. Profile bitmask
  `01` = Constrained Baseline (CBP), `02` = Constrained High (CHP); level bitmask
  `01` 3.1, `02` 3.2, `04` 4.1, `08` 4.2. CEA bit positions **[verify against the spec table]**:
  0 640x480p60, 1 720x480p60, 2 720x480i60, 3 720x576p50, 4 720x576i50, 5 1280x720p30,
  6 1280x720p60, 7 1920x1080p30, 8 1920x1080p60, 9 1920x1080i60, 10 1280x720p25,
  11 1280x720p50, 12 1920x1080p25, 13 1920x1080p50, 14 1920x1080i50, 15 1280x720p24,
  16 1920x1080p24. VESA and handheld (HH) masks list further sizes. 640x480p60 CBP 3.1 is the
  mandatory baseline every sink must accept.
- `wfd_audio_codecs`: e.g. `LPCM 00000003 00, AAC 00000001 00` (mode bitmasks per codec; LPCM
  44.1/48 kHz 16 bit 2 ch is the mandatory format, AAC-LC 2 ch optional; `none` if the sink
  declines audio, which sources accept) **[verify masks]**.
- `wfd_client_rtp_ports`: `RTP/AVP/UDP;unicast 19000 0 mode=play`.
- `wfd_content_protection: none` (no HDCP, ever; see decisions D8/security stance).
- `wfd_display_edid: none`, `wfd_coupled_sink: none`, `wfd_uibc_capability: none`,
  `wfd_standby_resume_capability: none`, `wfd_connector_type: 05` (or per spec) as needed.
  Unknown/unsupported parameters are answered `none` or with `GET_PARAMETER` status 415-style
  per spec; the state machine must tolerate parameters it does not know and never panic on
  hostile text (AGENTS.md §1.9).

## 4. Media format

- **Video:** H.264/AVC in **MPEG-2 TS**, stream type `0x1B`, PES stream id `0xE0`. Profiles
  CBP (mandatory) and optionally CHP; levels 3.1 to 4.2. Annex B byte stream inside PES; SPS/PPS
  repeated at IDR frames. Frame rates 24-60; resolution from the table above (1080p30/p60 on
  4.1/4.2). Sources do not use B-frames in low-latency mode, but the decoder must tolerate them.
- **Audio:** LPCM (private stream 1, PES stream id `0xBD` with a 4-byte LPCM header, 48/44.1 kHz,
  16 bit, 2 ch) and AAC-LC (stream type `0x0F`, ADTS/LATM). The spike decodes/plays none in
  steps 4-5 beyond demuxing PES; audio is a non-goal for MirrorWire (AGENTS.md), so the
  sink may advertise `none` and ignore audio. Default decision: **advertise LPCM only if the
  tablet refuses a stream without audio**, else `none`.
- **TS layer:** 188-byte packets, PAT/PMT repeated about every 100 ms (the sink must not assume
  one PAT per stream), PCR on the video PID, continuity-counter checking per PID, 33-bit PTS/DTS
  at 90 kHz with wraparound.
- **RTP layer:** header per RFC 3550, PT 33, SSRC fixed per session, sequence gaps → loss; the
  jitter buffer is tiny (target ~20-50 ms, reorder window a few packets) because latency matters
  more than perfection; on loss in a video PID send M13 (IDR request) instead of waiting.

## 5. What Android sources expect

From the AOSP wireless-display design (behaviour to confirm on the TB336FU, **[verify]** each):

- The sink must show in the tablet's "Wireless display" list, which needs a valid WFD IE
  (primary sink, available) in P2P probe responses and **a stable device name**.
- After the user taps it, the *source* chooses the topology and usually tries to be GO; the sink
  must accept the P2P invitation/GO-negotiation (risk R1 above) and join without user input on
  the laptop side (PBC auto-accept).
- The source waits for the sink's IP, then connects TCP to 7236 within a few seconds. If nothing
  listens, the session fails with a generic "can't connect". So **start listening on 7236 before
  forming the group**.
- It runs M1-M7 strictly in order, answers M2 at once, picks one video format out of ours, and
  keeps M16 keepalives; a missed keepalive or a malformed reply makes it tear down after the
  session timeout. Replies must carry the right `CSeq` and `Content-Length`.
- Some vendor builds (Lenovo's "Wireless display" is a Mediatek-based AOSP feature) expect
  `wfd_content_protection` absent or `none`, a mandatory-subset `wfd_video_formats`, and may
  request 1080p30 from CBP level 3.1/4.1. They tolerate `wfd_audio_codecs: none` only
  sometimes **[verify]**; keep an LPCM fallback ready.
- IDR on demand (`wfd_idr_request`) is honoured by AOSP sources; the first frame after PLAY is
  an IDR with SPS/PPS.
- Screen rotation changes resolution mid-stream: expect new SPS and different frame size
  without a new RTSP exchange (M4 may be sent again with new `wfd_video_formats`).

## 6. Intel 8265 on the ThinkPad T480s

Intel Dual Band Wireless-AC 8265 ("Windstorm Peak"/Stone Peak), driver `iwlwifi` + `iwlmvm`.
That driver family advertises the **P2P-client, P2P-GO and P2P-device** interface types, so
NM can create the `p2p-dev-*` device. Expected, **[verify with `iw list` on the T480s]**:
interface combinations allow one managed station plus one P2P-client/GO plus the P2P-device,
but only on **a single channel at a time** (or time-shared with a throughput hit); so the group
is best on the channel the laptop's Wi-Fi is already on, or the laptop's normal Wi-Fi drops/
stutters while the group is up. 5 GHz P2P may be restricted by regulatory flags (no-IR/DFS
channels cannot host a GO). Firmware (`iwlwifi-8265-*.ucode`) comes from `linux-firmware`.
Not measured here. Commands: section 9.

## 7. Architecture for the spike

Crates (new, workspace members, no change to existing crates):

- `crates/miracast-nm`: `P2pProvider` trait (find, peers, connect/accept, group info with
  interface + local/peer IP, WFD IE set) + `MockP2pProvider` + NM D-Bus backend (zbus), calls
  tested against a mock bus/object.
- `crates/miracast-rtsp`: sink-side M1-M7 state machine, text parsers for the `wfd_*`
  parameters, keepalive and teardown timers; pure sans-IO state machine (feed bytes, get
  actions) so it tests from message traces.
- `crates/miracast-rtp`: RTP receive, TS demux, H.264 access-unit assembly, audio PES, loss/
  jitter handling; output = access units for a sink trait (MirrorWire's `VideoSink` later).

Layering: core crates depend on nothing platform-specific; only the NM backend knows D-Bus.
No `unsafe` is expected anywhere. Wire parsers follow AGENTS.md §1.9 (bounded lengths, no panics).
Decisions made for the spike (all reversible, spike-local):

1. Sink first; Source later. Sink listens on 7236 before the group is formed.
2. Sans-IO RTSP/TS parsers, so tests need no sockets and fuzzing is possible later.
3. No HDCP, no content protection, no UIBC.
4. Audio is demuxed but not played (non-goal); advertise it only if hardware demands it.
5. Rejected: root helpers or stopping NM (MiracleCast style); direct `wpa_supplicant` D-Bus
   (root-only policy); shipping GStreamer/FFmpeg demuxers (size, we need only TS+H.264);
   copying code from GPL projects (clean-room rule).

### Crate skeleton (step 3, done)

- `miracast-nm`: `P2pProvider` (object-safe, boxed futures), `MockP2pProvider`, `NmApi` +
  `NmP2pProvider` (maps the trait onto NM calls, tested with the recording `MockNm`),
  `zbus_backend::ZbusNm` (Linux only; real calls for device list, StartFind/StopFind, peers,
  AddAndActivateConnection, DeactivateConnection; `connection_settings` is a pure function
  with tests), `wfd` (device-info IE encode/parse, hostile-input safe), `Sink` (advertise,
  wait for group, stop) and `SinkConfig` (name, RTSP port 7236, RTP port 19000).
- `miracast-rtsp`: filled in step 4, see "RTSP sink (step 4)" below.
- `miracast-rtp`: filled in step 5, see "RTP/TS receive (step 5)" below.
- All three are in `xtask` LAYERS as standalone crates (no mirrorwire-* dependencies), no
  `unsafe`. Rejected for the trait: `async fn` in traits (not object-safe, clippy
  `async_fn_in_trait`), the `async-trait` crate (a new dependency for no gain).

### RTSP sink (step 4, done)

- `message`: RFC 2326 framing (`parse_message` returns `Ok(None)` until a whole message is
  buffered, bounded: head 8 KiB, body 32 KiB, 64 headers), `Request`/`Response` builders that
  add `Content-Length`. Bare-LF line ends are accepted; header names are case-insensitive.
- `params`: `wfd_video_formats` (spec field order: native, preferred-display-mode, then per
  H.264 entry profile, level, CEA/VESA/HH masks, latency, min-slice, slice-enc, frame-rate
  control, max h/v res; entries comma separated), `wfd_audio_codecs`,
  `wfd_client_rtp_ports`, `wfd_presentation_URL`, subset checks (`VideoFormats::accepts`) and
  the CEA table. Bit positions remain `[verify]`.
- `SinkSession` (sans-IO): `handle_input(bytes, now)`, `poll(now)` (keepalive timeout from the
  `Session: id;timeout=N` of the M6 reply, default 60 s), `request_idr()` (M13),
  `teardown()` (M8). Actions: `Send`, `Negotiated` (M4 accepted, also on mid-stream
  renegotiation), `StartMedia`, `StopMedia`, `Close{reason}`.
- Behaviour choices: M2 is sent right after answering M1; the M2 reply is tolerated in any
  order relative to M3. M3 answers known parameters (unsupported ones with `none`) and leaves
  unknown names out. M4 outside the offered set gets 406 and closes. A different RTP port in
  M4 is refused (we already listen on ours). Requests before M1 get 455; unknown methods 405;
  an unknown `Require:` feature tag 551. The default offer is CBP+CHP level 4.1, CEA mask
  0x1E1 (480p60, 720p30/60, 1080p30/60), no audio.
- 21 unit tests: full M1-M7 trace, byte-at-a-time input, keepalive/IDR/teardown, timeout,
  rejection paths, hostile input. No unsafe, no dependencies, workspace `indexing_slicing`
  lint satisfied.

## 8. Test plan

1. **Unit (no hardware):** RTSP traces for M1-M7, M13, M16, TEARDOWN written from the spec
   grammar; malformed and oversized messages; `wfd_video_formats` / audio parameter
   round-trips; TS packets with continuity errors, PAT/PMT repeats, PTS wrap; RTP reorder/loss.
2. **Loopback (no Wi-Fi):** a small test *source* speaking M1-M7 and sending TS over RTP to
   `127.0.0.1`; media from `ffmpeg -f lavfi -i testsrc2 -c:v libx264 -profile:v baseline
   -bsf:v h264_mp4toannexb -f mpegts` pushed through RTP framing; decode through the existing
   decoder path in an example binary.
3. **Virtual radios:** not usable without root, see section 8a; skipped.
4. **Hardware (owner):** T480s + TB336FU: (a) the tablet lists the sink; (b) group forms and
   which side is GO; (c) RTSP M1-M7 completes; (d) video appears; (e) loss/IDR behaviour;
   (f) idle teardown. Checklist with exact commands goes into HANDOFF.md at step 6; logs to
   return: the RTSP trace (both directions, timestamps), `nmcli -f all device show`
   output for the p2p devices, `journalctl -u NetworkManager --since`, and a `tcpdump`
   summary if allowed.

## 8a. Virtual radios without root (step 2 result, 2026-11-20)

Checked on the research box (Linux 6.12.113+, uid 1000, no sudo, VM without Wi-Fi):

- No `mac80211_hwsim`: the kernel has no loadable-module tree (`/lib/modules` missing, no
  `modprobe`/`modinfo`), no `/sys/class/ieee80211`, no `/proc/net/wireless`, no `iw`, `nmcli` or
  `wpa_supplicant`. `/proc/config.gz` has `CONFIG_MODULES=y` and `CONFIG_USER_NS=y` but no `CFG80211`, `MAC80211` or `MAC80211_HWSIM` at all, so the kernel cannot do Wi-Fi even with root.
- Unprivileged user namespaces work (`unshare -Urn` succeeds and gives a private network
  namespace), but a namespace cannot load kernel modules or create radios: `hwsim` radios are
  created by the module in the init namespace (needs `CAP_SYS_MODULE`/root) and only *moved*
  into namespaces afterwards with `iw phy ... set netns`.
- No `/dev/rfkill`, `/dev/vhci`, `/dev/uhid`.

**Result: virtual Wi-Fi radios are not usable here without root.** Not planned: P2P tests
on virtual radios on this box. Where it could work later (owner's T480s or CI with root):
`sudo modprobe mac80211_hwsim radios=2`, then one phy per netns (`iw phy phyN set netns
name NS`) with a `wpa_supplicant` P2P instance each. That tests wpa_supplicant, not our code,
and NetworkManager cannot easily be run per netns without a private system bus; it is
therefore a low-value, optional test. The spike relies on mocks (`P2pProvider`), loopback
RTSP/RTP tests (step 6) and the real hardware run instead.

## 9. Commands to measure on the T480s (for the owner or step 6)

```sh
rpm -q NetworkManager NetworkManager-wifi wpa_supplicant linux-firmware
nmcli device                       # expect wlp…, and p2p-dev-wlp… 
iw list | grep -A12 'Supported interface modes'     # P2P-client / P2P-GO / P2P-device
iw list | grep -B2 -A10 'valid interface combinations'
pkaction --verbose --action-id org.freedesktop.NetworkManager.network-control
sudo -n true 2>/dev/null && echo "root-less check only; the spike itself never uses sudo"
```

## 10. Open questions

- R1: does NM accept an incoming GO negotiation (tablet taps the sink), or must the sink
  initiate? Decides the discovery UX. (Hardware.)
- Exact WFD IE bit layout and `wfd_*` masks: confirm against the spec text (step 3/4).
- Is `wfd-ies` on the `wifi-p2p` setting enough to be listed, or does the IE need to be set on
  the P2P device itself? (Hardware; NM docs say per-connection.)
- Audio mandatory or not for the TB336FU. (Hardware.)
- zbus: already a workspace dependency (mirrorwire-net, MIT, same feature set
  `tokio`/`proxy`/`tracing`), so no new licence or size cost. Done in step 3.
- R2 (step 3): the zbus backend's `set_wfd_ies`, group address read (Ip4Config after
  activation) and signal subscription are `Unsupported` stubs; how NM takes sink-side WFD IEs
  must be checked on hardware (see the open question above).

### RTP/TS receive (step 5, done)

`miracast-rtp` is sans-IO: the caller owns the UDP socket and calls `Receiver::push(datagram,
&mut impl MediaSink)`. No dependencies, no unsafe, workspace `indexing_slicing = deny` holds.

- `rtp`: RFC 3550 header parse (CSRC, extension, padding skipped; errors, never panics) and a
  `Reorder` buffer (depth 8 packets, wrap-safe seq compare, duplicates/late packets dropped and
  counted; when full it skips the gap and reports `lost`).
- `ts`: 188-byte packet demux. PAT -> first non-zero program's PMT -> streams by type (0x1B video;
  0x0F/0x11/0x81/0x83/0x06 audio). PES reassembly per PID, PTS (33 bit, 90 kHz), a PES with
  length 0 (video) flushes on the next start, a bounded one flushes when complete. Per-PID
  continuity check (duplicate tolerated, discontinuity flag honoured). PSI CRC32 is not verified
  (single-packet sections only; hostile data is bounded anyway). `MAX_PES` 8 MiB.
- Loss policy: an RTP gap or video CC error drops the partial PES, discards video until the next
  IDR (NAL type 5) and calls `MediaSink::need_keyframe()` once per episode; the caller maps that
  to `SinkSession::request_idr()` (M13). `TsStats` and `Receiver::{bad_datagrams,lost_packets}`
  are meant for the GUI diagnostics (AGENTS.md 1.8).
- Audio PES is delivered as `AudioFrame`, not decoded or played (non-goal).
- `ts::synth` (public, indexing allowed, test-data only) builds PAT/PMT/PES/TS/RTP; step 6 reuses
  it as the loopback source.
- Tests (16): header edge cases, reorder/wrap/dup/gap, clean stream, reordered stream, RTP loss,
  CC gap, garbage datagrams, hostile PES header, and a real `ffmpeg -f mpegts` (libx264) stream
  (skipped when ffmpeg or libx264 is missing; it ran on this box).
- Not done: feeding MirrorWire's decoder from an example binary (left to step 6, which needs a
  source anyway); AAC/LPCM parsing beyond PES; PCR-based clock recovery (decode ASAP, as a
  low-latency sink should).

### Loopback end-to-end (step 6, done)

Crate `miracast-loopback` (test support, `publish = false`, no deps beyond the two spike crates,
no unsafe; test code may panic, the crate allows unwrap/indexing locally).

- `run_sink` / `run_sink_logged`: drives `SinkSession` + `Receiver` over real sockets (TCP
  accept, UDP RTP). `Action::Send` -> TCP write, `StartMedia` -> start draining UDP,
  `need_keyframe` -> `SinkSession::request_idr()` (M13). Datagrams are left in the socket buffer
  until PLAY is acknowledged (a source may send right after the M7 reply: found by the test,
  not a spec rule). `run_sink_logged` calls back with the RTSP trace in both directions.
- `TestSource`: RTSP client side M1-M7 (answers the sink's M2, picks 1080p30 CBP in M4 with the
  RTP port the sink announced in M3, answers M6 SETUP/M7 PLAY), builds TS/RTP frames with
  `ts::synth` (PSI + H.264 AU + audio PES, every 5th frame IDR), sends them on localhost.
- Tests (`tests/e2e.rs`): all access units arrive byte-identical, no loss/bad datagrams;
  one dropped RTP packet -> `need_keyframe` -> M13 `wfd_idr_request` on the wire -> decoding
  resumes at the next IDR; garbage datagrams are counted, not fatal. 3 passes in a row, 0.3 s.
- Binary `miracast-sink-demo` for the hardware test: listens on TCP 7236 / UDP 19000, prints the
  RTSP trace and per-second media stats, writes the H.264 Annex B stream to a file.
- NOT done: feeding the access units to MirrorWire's decoder (mirrorwire-media) in an example.
  The spike crates stay free of mirrorwire-* dependencies; use `ffplay stream.h264` instead.
  The sink/NM glue (P2P group -> listen on 7236) is also not wired: group setup is manual in
  the hardware checklist until R1/R2 are settled.

## 11. After the hardware runs (2026-11-21, T480s + TB336FU)

Measured facts (runs 1-3, `~/tmp/mc-hw/` on the owner's box): the tablet is a WFD **source**
(IE `00 0006 0010 1c44 0032`), becomes **group owner** at `192.168.49.1` and serves DHCP
(we got `192.168.49.74/24` on `p2p-wlp58s0-N`); the tablet is the **RTSP server** on
`192.168.49.1:7236` and the sink connects to it (not the other way round, as section 2
assumed); it refuses `wfd_audio_codecs: none`; it negotiated CHP level 3.2 but sent an SPS
with level 4.0; the P2P interface lands in firewalld zone `public`, which drops RTP on 19000.

### R1/R2 flow in `miracast-nm` (settled)

- **R2:** NM has no device-level WFD IE property. The IE is published only while a
  `wifi-p2p` profile carrying `wifi-p2p.wfd-ies` is activating or active.
  `NmP2pProvider::set_wfd_ies` therefore only stores the IEs; `connect` puts them in the profile.
- **R1:** NM does not answer the source's own connection request without a pending profile.
  `Sink::connect_source(peer)` activates a volatile profile (`AddAndActivateConnection2`,
  options `persist: volatile`, `ipv4.method auto`, WPS PBC), the provider polls the active
  connection's `State` (250 ms) until `ACTIVATED` (default timeout 120 s: the user taps us in
  the tablet's Cast list; tapping "Connect" on the tablet's prompt forms a group without RTSP),
  then reads `Ip4Config.AddressData`/`Gateway` and the device's `IpInterface`.
  `group_from_ip4`: gateway != own address -> we are client and the gateway is the source
  (`peer_ip`); no gateway -> we are GO, source address unknown. A vanished/deactivated
  connection or a timeout gives a readable `P2pError` and the connection is deactivated.
- `wfd_source_port(peer)` picks Miracast sources from the peer list (device type source/dual,
  session available) and returns their RTSP port.
- Tests: `MockNm` scripts the state sequence and IPv4 delay (no hardware). The zbus calls
  themselves are untested here (no NM in the container); value parsing is unit tested.
- [verify] `bind-activation: dbus-client` (tear down if our process dies) is not used: the
  hardware runs used busctl, so it was never tried.

### Sink connects to the source (replaces the socat bridge)

`run_sink(source, udp, ..)` connects to the source's RTSP server (`connect_to_source`: 1 s
connect timeout, retry every 500 ms until the deadline, because the source may not listen yet
right after the group forms). An unreachable source ends with a readable `close_reason`.
`TestSource` is now the RTSP server too (`TestSource::listen()` -> `SourceListener::accept()`),
like the tablet. `miracast-sink-demo --source 192.168.49.1:7236` (default) replaces
`--rtsp-port` and the socat relay; the source address comes from `GroupInfo::peer_ip`
(group owner) once the demo is wired to `miracast-nm`.

### Decoder size from the SPS (`miracast_rtp::h264`)

`Sps::parse(nal)` / `find_sps(access_unit)` read profile, level and the cropped width/height
(ITU-T H.264 7.3.2.1.1, incl. scaling lists, POC types, interlaced and crop units); hostile
input gives `None`. Size the decoder from this, never from M4: in run 3 the TB336FU's M4 said
CHP level 3.2 and every one of its 34 SPS said High (100) level 4.0, 1920x1080. Rotation kept
1920x1080 in run 3; a future source that changes size mid-stream shows up as a new SPS.
`miracast-sink-demo` logs `stream SPS: WxH profile P level L` when the SPS changes.

### "RTSP ok, no RTP" diagnostic (`miracast_rtp::diag`)

`NoRtpWatchdog` (sans-IO) starts at PLAY; if no datagram (even a malformed one) arrives within
`DEFAULT_NO_RTP_AFTER` (5 s) it yields `NoRtp { rtp_port, waited, interface, zone }` once.
`Display` is the GUI text: it names the port, the zone (`zone` if the caller looked it up via
firewalld, else the likely `public`, measured in run 1) and the runtime-only fix
`firewall-cmd --zone=public --add-port=19000/udp`; `title()` is a headline. MirrorWire never
runs the command itself. `run_sink_logged` logs it and keeps it in `SinkReport::no_rtp`; the
session stays up. Not wired into the GUI yet (no Miracast GUI exists); the caller that owns the
`GroupInfo` fills in `interface` (and `zone`, through the Flatpak's existing FirewallD1 access).

### `--nm` mode and hardware run 4 (2026-11-21)

`miracast-sink-demo --nm [--peer NAME] [--find-secs 60] [--attempts 3]` runs the whole chain:
`ZbusNm` find -> `Sink::sources` -> `connect_source` (retried up to `attempts` on a formation
failure) -> `run_sink(peer_ip:port-from-WFD-IE)` with the group interface in `RunOptions`
-> leave the group. `nm_flow::join_source` is mock-tested.

Run 4 on the T480s + TB336FU (logs `~/tmp/mc-hw/*run4*`), no sudo, firewall untouched:
- The zbus calls work as written: discovery found the tablet in 7-13 s, NM activated the
  volatile profile, `group_ip4` gave `p2p-wlp58s0-N`, 192.168.49.74, gateway 192.168.49.1;
  the sink connected to 192.168.49.1:7236 itself (no socat); the profile was gone afterwards.
- Group formation failed in 2 of 5 activations on the source side (GO negotiation OK then
  `P2P-GROUP-FORMATION-FAILURE`; once `P2P-GO-NEG-FAILURE status=10`), with the same taps as
  the good ones. Hence the retry. NM/wpa_supplicant journals are not readable from the
  container, so the cause on our side is unknown.
- `stream SPS: 1920x1080 profile 100 level 4.0` within 0.5 s of PLAY (M4: CHP level 3.2).
  UDP 19000 was reachable (RTP flowed), so `NoRtp` did not fire; firewalld's default zone is
  `public` here, and querying its ports needs polkit `config.info` (refused, no prompt).
- One attempt lost the link after 37 s (tablet: `AP-STA-DISCONNECTED`; the laptop's Wi-Fi is
  on 5300 MHz while the group is on 2462 MHz, so the 8265 time-shares channels, which fits
  but is not proven). That gap led to `RtpStalled`.
- Teardown from the tablet's DISCONNECT: `wfd_trigger_method: TEARDOWN` -> our M8 -> "stopped
  by the user" -> group left.

## 12. P2P operating channel and GO role (stability research, run 5)

Question: can the sink steer the group onto the laptop's STA channel (one radio, so two
channels mean time-sharing, see run 4), or become GO itself?

Sources: Wi-Fi P2P Technical Specification (GO Negotiation, Operating Channel and Channel List
attributes, Invitation), the live NetworkManager D-Bus API on the T480s (introspection), the
tablet's logcat. The official NM settings reference and wpa_supplicant D-Bus docs could not be
fetched from the research box (fetch permission denied); the points marked [docs] need that
check.

- **Who picks the channel.** In GO Negotiation each side sends a GO Intent (0-15) and its
  Channel List; the winner (GO) picks the Operating Channel from the intersection. In
  Invitation the inviting GO names the Operating Channel. So the GO decides.
- **Who is GO.** The TB336FU asks with `groupOwnerIntent: 14` (logcat, MtkWifiDisplayController);
  our NM/wpa_supplicant sends `go_intent=7` (tablet: `P2P-GO-NEG-REQUEST ... go_intent=7`). To
  become GO we would need intent 15. NM's API on this machine offers no GO intent: the
  `Device.WifiP2P` interface has only `StartFind(a{sv})`, `StopFind`, `Peers`, `HwAddress`;
  the `wifi-p2p` setting is known to carry `peer`, `wps-method`, `wfd-ies` [docs: confirm no
  intent/frequency property]. wpa_supplicant's own D-Bus API (which has GO intent and frequency
  on its P2P Connect [docs]) is root-only by policy and fights NM (rejected, section 7). Being
  GO would also mean `ipv4.method shared` and learning the source's address from the DHCP lease.
  **Result: not possible rootlessly through NM; not pursued.**
- **Which channel the tablet picks.** Every group so far was on 2462 MHz, which is the tablet's
  own STA channel (`cmd wifi status`: Frequency 2462MHz, AP 24:65:11:66:7e:69). A GO that is
  also a station prefers its STA channel to avoid its own channel switching.
- **What we can influence.** Nothing in the P2P request through NM. What makes the channels
  match is the laptop's STA being on the same channel as the tablet's STA (same AP, same
  band). In run 4 the laptop STA was on 5300 MHz (DFS) while the group was on 2462; at the start
  of run 5 it was on 2462 (same AP as the tablet), with no change by us. Note: a GO generally
  cannot start a group on a DFS channel such as 5300 MHz (no initiating radiation without
  radar detection), so even as GO we could not have followed a 5300 MHz STA.
- **Optional later (roadmap TODO, idea/optional):** a root prompt, only on an explicit user
  click and never automatic, to choose the P2P/Wi-Fi channel so the group channel matches the
  STA channel; possibly offered only when statistics show frequent disconnects that line up
  with a channel mismatch (group freq != STA freq). Any privilege needs the owner (AGENTS.md
  §1.1, §5); nothing of this is implemented.
- **What we do instead (rootless):** log the STA frequency next to every session (NM
  `AccessPoint.Frequency`); NM exposes no group frequency, so the hardware runs take it from
  the source's log. Retry formation, close a session quickly when the link is gone and
  reconnect, rate-limit IDR requests, and bound the reorder delay (sections below).

### Stability work and hardware run 5 (2026-11-21)

Code (all rootless, spike crates):
- IDR requests on loss (already there) are rate-limited to one per second (`MIN_IDR_INTERVAL`,
  `idr_suppressed`).
- The reorder buffer is also time-bounded (`JITTER_WAIT` 40 ms, `Receiver::poll`) and counts
  gaps, reordered and late packets (`RxStats`).
- `RunOptions::close_on_silence` ends a session whose media stopped (`link_lost`, best-effort
  TEARDOWN); `--nm` uses 8 s and `--reconnect N` joins again when `wants_reconnect()`. Group
  formation is retried (`--attempts`, default 3). A deadline ends with a TEARDOWN.
- Trace: STA band/channel before and after each session (`ZbusNm::sta_frequencies`,
  `freq::describe`), a stats line every 5 s.

Run 5 (5 `--nm` attempts, T480s + TB336FU, logs `~/tmp/mc-hw/*run5*`, laptop STA untouched):

| # | retries | group up | media | lost (%) | gaps | IDR sent/suppr. | end |
|---|---|---|---|---|---|---|---|
| 1 | 0 | 21.6 s | 398 s | 84 (0.07) | 16 | 14/1 | deadline (no TEARDOWN then, fixed since) |
| 2 | 0 | 13.5 s | 406 s | 500 (0.43) | 15 | 10/0 | deadline (same) |
| 3 | 1 | 60.9 s | 306 s | 4 (0.00) | 1 | 1/0 | DISCONNECT on the tablet, clean TEARDOWN |
| 4 | 1 | 61.8 s | 313 s | 87 (0.10) | 16 | 13/1 | same |
| 5 | 0 | 13.0 s | 315 s | 40 (0.04) | 10 | 7/0 | same |

- 5/5 attempts streamed at least 5 minutes; 0 link losses; ~290 packets/s (1080p30).
- Group formation failed once in attempts 3 and 4 (tablet: `P2P-GROUP-FORMATION-FAILURE`);
  the automatic retry formed the group each time (7 activations, 5 groups).
- Group frequency (tablet log `P2P-GROUP-STARTED`): 2462 MHz in all 5; laptop STA: 2462 MHz
  (ch 11) before and after every session. Same channel, which fits the run-4 link loss having
  been caused by the 5300/2462 MHz split (run 4: one loss in 4 sessions; run 5: none in 5).
- No polkit or password prompt appeared; no firewall change (UDP 19000 reachable).

## 13. Viewer integration (step 8, 2026-11-21)

`mirrorwire view --miracast [--miracast-peer NAME]` (app feature `miracast`, not default;
the flags exist in every build and say so when the feature is missing).

- Module `crates/mirrorwire-app/src/miracast.rs` returns the same `ViewerHandle` as a QUIC
  viewer, so the GUI's video view, HUD, Back/Esc and renderers work unchanged.
- Session thread: `ZbusNm` -> `nm_flow::join_source` (progress text in the new
  `ViewerStatus::note`, shown on the connecting screen instead of the QUIC steps) ->
  `run_sink_logged` to the group owner (8 s close-on-silence, stats every 5 s, `stop` flag =
  Back button -> TEARDOWN) -> leave the group. Driver log lines map to the status:
  `start media` = connected, `diagnostic: ...` = red error before the first picture (the
  firewall-command copy button applies) or HUD text after it, `stats: ...` = HUD line.
- Decode thread: access units through a 6-deep channel (full: drop until the next keyframe
  and request an IDR via `RunOptions::idr_request`, rate-limited 1/s);
  the first keyframe's SPS (`h264::find_sps`) opens `decode::open_decoder` and sets the native
  size; a keyframe with a different SPS size (rotation) reopens it. Units before the first
  SPS are skipped. Only the newest picture goes to the GUI. Audio is ignored.
- Not done: the NM join can't be cancelled while it waits (up to 3 x 120 s); Back takes
  effect when it returns, and the group is left then. No Miracast entry on the home screen
  (CLI only). No HUD latency (the source has no clock sync with us).
- Build here: the full app needs libpipewire and FFmpeg; `--no-default-features --features
  gui,filmcraft,miracast` builds without both (FilmCraft decoder). Tests: 5 in
  `miracast::tests`, including real FilmCraft H.264 -> `TestSource` RTP/TS on localhost ->
  this module -> a 320x240 picture with the SPS size.

### Hardware run 6: Miracast in the viewer (2026-11-21, logs `~/tmp/mc-hw/*run6*`)

T480s (STA 2462 MHz), TB336FU, `mirrorwire view --miracast` built here with
`--no-default-features --features gui,filmcraft,miracast` (FilmCraft software decode, wgpu
window on the KDE Wayland session; no FFmpeg, so no VAAPI). Taps on the tablet through adb.

| run | build change | result |
|---|---|---|
| 6a | first build | group formed after the tablet's prompt was answered without the Cast-list tap: no RTSP server, the viewer retried forever -> fixed (`connect_within` 20 s, readable reason) |
| 6b | | media 72 s, 0.10 % loss, clean TEARDOWN; the log had no proof of decoded pictures -> first-picture and per-second decode lines |
| 6c | logs | first picture 1920x1080 0.3 s after PLAY, but ~0.7 pictures/s |
| 6d | IDR request on queue overflow | ~1.7 pictures/s (19 IDR requests) |
| 6e | decode queue 2 -> 6 | **~30 pictures/s** (1995 in 71 s), decode 10-12 ms, loss 0.12 %, 6 IDR requests, clean TEARDOWN |

Cause of 6c/6d: an IDR picture decodes several times slower than a P picture; with a 2-deep
queue every IDR overflowed it and the drop-to-keyframe rule fed the next IDR. Offline, on
`stream-run5-5.h264`: FilmCraft 8.4 ms per unit + RGBA conversion 13.7 ms per 1080p picture.

Not done: a screenshot of the laptop screen. The only rootless way is the xdg Screenshot
portal; one non-interactive request (13:17:58) got no Response within 15 s and its request
object was gone afterwards, outcome unknown; not retried, to avoid a dialog on the owner's
screen. Evidence is the viewer log (first picture, per-second picture counts) and the
tablet screenshots `t27`-`t30` (casting). Group formation needed the automatic retry in 3 of
5 runs.

## 14. Start screen flow (D21, 2026-11-21)

Start screen: large "Presenter", small "Viewer", "View your Android device
(Miracast-compatible)" (label per D22; feature `miracast`, default and effective on Linux only), "Advanced" (UDP port) collapsed. The Miracast button opens
`Screen::Miracast` (`gui/miracast.rs`), driven by the pure state machine
`mirrorwire-app/src/miracast_flow.rs`:

| phase | from | shown |
|---|---|---|
| Starting / Searching | button, `FlowEvent::Discovering` | "On your Android device open Settings > Cast (Wireless display) and turn wireless display on" + spinner + Cancel |
| Joining {peer, attempt} | `SourceFound`, `AttemptFailed` | "On <peer>, tap "<name shown on Android>" (or "the entry without a name" / "this computer", §16; may be listed as "Wireless display"); if it asks to connect, choose Cancel and tap it in the list", plus "try n of 3" |
| Connecting | `Joined` | "Connected to <peer>. Starting the stream…" |
| Streaming {warning} | `SinkEvent::MediaStarted`, `NoRtp`, `Stalled` | the normal video view once a picture exists; before that the firewall text (port, zone, `firewall-cmd` with a Copy button) |
| Failed(issue) / Ended | join error, `SinkReport` | result page with Try again / Back: no device found, group did not form, "did not start casting" (tap hint), "Video stopped" (link lost), casting stopped on the device |

R1/R2 order: discovery starts at the click; the laptop becomes visible on the device when the
profile activates after the device was found, and only then the user is told to tap it.
The sink driver and the join report typed `SinkEvent`/`FlowEvent` (Display = the old trace
text). Cancel, Back, Esc and closing the window set the stop flag: the NM join is cancelled
(`tokio::select!` on the flag), `Sink::stop` leaves the group and removes the volatile
profile, and only then the final phase is published; a new attempt and the window's `Drop`
wait (up to 5 s) for `MiracastSession::finished`. Tests: 7 state-machine tests, the
localhost end-to-end test now also checks Video and Ended after Cancel. Not run on the
desktop this round: the container cannot read the user journal for the required KWin check,
so no GUI was started on the owner's machine.

## 15. Clean-room audit and [verify] resolution (step 10, 2026-11-21)

Method: provenance review of each file (who wrote it, from which public source or our own
trace), a search for literals that do not come from the spec grammar or our hardware traces,
and a check for copyright headers, foreign identifiers or comments pointing at other
projects. No GPL code (MiracleCast, gnome-network-displays, ...) was read for this or any
earlier step, so no side-by-side comparison was made (D20).

| scope | origin | result |
|---|---|---|
| `crates/miracast-loopback/src/lib.rs` (sink driver, `TestSource`, `SinkEvent`) | written in steps 6-11 of this branch from RFC 2326 framing, the WFD M1-M7 order as documented in §3, and our own hardware traces | own work. Test literals `6B8B4567` (session id), `192.168.173.x` (presentation URL) and `server_port=15550` had no documented source (not code, but of unclear origin): **replaced** with values from our run-3 trace (session `448846178`, `192.168.49.1`, `server_port=7016`), also in `miracast-rtsp` `session.rs`/`params.rs` tests (commit 50431e2) |
| `crates/miracast-loopback/src/nm_flow.rs` | written in steps 8-11 on our `P2pProvider` API and the R1/R2 findings (§11) | own work |
| `crates/miracast-loopback/tests/e2e.rs` | written in steps 6-11 against our own `TestSource` | own work; RTSP bodies follow the spec grammar of §3 (`wfd_video_formats` field order, `wfd_client_rtp_ports`, `wfd_trigger_method`) |
| `crates/miracast-loopback/src/bin/miracast-sink-demo.rs` | written in steps 6-9 (CLI, stats, `--nm` loop) | own work |
| `crates/miracast-rtsp/src/message.rs` from line 200 (`parse_message`, tests) | written in step 4 from RFC 2326 section 4 (request/status line, headers, `Content-Length` body) | generic framing parser in idiomatic Rust, bounded per AGENTS.md 1.9; tests use spec strings (`org.wfa.wfd1.0`, `wfd_trigger_method: SETUP`) only. Nothing distinctive |

Result: **nothing looks derived**; the only finding (test values of unclear origin) is fixed.

[verify] items, resolved only against our hardware traces (`~/tmp/mc-hw`, runs 1-6) or left
open (the Wi-Fi Display spec text is not available on the research box):

| item | status | evidence |
|---|---|---|
| WFD IE device info: type bits 1:0, session availability bits 5:4 | **confirmed** | tablet IE `00 0006 0010 1c44 0032` = source, available, port 7236; tablet logcat `WFD DeviceInfo: 17` for our `0x0011` and listed us as a sink |
| other device-info bits (HDCP, time sync, coupled sink, ...) | open, unused | we set them to 0 and the tablet accepted us |
| CEA bit 7 = 1920x1080p30 | **confirmed** | M4 `02 02 00000080`, SPS 1920x1080, ~30 fps measured |
| other CEA bits | open | never picked by the tablet |
| profile bit 1 = CHP | **confirmed** | M4 profile `02`, SPS profile_idc 100 (High) |
| level bits (0x01 3.1, 0x02 3.2, 0x04 4.1, 0x08 4.2) | open | M4 level `02` with an SPS level 4.0: fits no reading of the table; the decoder is sized from the SPS (step 4), so it does not matter at runtime |
| audio `LPCM 00000002 00` = 48 kHz 16 bit stereo | partly | the tablet requires audio and picks exactly this offer (runs 1-6); the sample-rate meaning of bit 1 is not checked (audio is not decoded) |
| Fedora wpa_supplicant built with P2P + WFD | **confirmed** | `p2p-dev-wlp58s0` exists; the tablet received our WFD IE |
| polkit: no prompt for an active session | **confirmed** | NM `GetPermissions`: `network-control`, `settings.modify.own/system`, `wifi.scan` = yes (baseline); no prompt in runs 1-6 |
| R1 (incoming negotiation), who is GO, AOSP behaviours of §5 | **settled** | §11: sink must activate first; tablet is GO (intent 14); M1-M7 in order, M16 keepalives, IDR on request, first unit has SPS, rotation without new RTSP, `audio none` refused. Correction to §5: the source is the RTSP server and the sink connects (not the other way round) |
| Intel 8265 interface combinations (`iw list`) | partly | `iw` was not available; the owner confirmed one shared radio (station + P2P), consistent with run 4 (5300 vs 2462 MHz, link loss) and run 5 (same channel, no loss) |
| `bind-activation: dbus-client` | open | never tried; the app removes the profile itself on Cancel/close |

Flatpak note (D24, 2026-11-22): the manifest has
`--system-talk-name=org.freedesktop.NetworkManager`, so the Miracast-compatible button works
in the Flatpak as in native builds (not yet tried in an installed Flatpak bundle). Error pages:
NM missing or unreachable (also a Flatpak whose NM permission was removed with an override:
the bus proxy answers `ServiceUnknown`) -> "Receiving from Android is not available", inside
the Flatpak with the `flatpak override --user --system-talk-name=org.freedesktop.NetworkManager
io.github.windwerfer.MirrorWire` command; no Wi-Fi P2P device -> the same page with the
wpa_supplicant hint; Polkit refusal (`NotAuthorized`, `AccessDenied` or NM's
`org.freedesktop.NetworkManager.PermissionDenied`) -> "Receiving from Android was not allowed",
no retry. Flathub justification text: `packaging/flatpak/README.md`.

## 16. Name shown on Android (D25, step 16, 2026-11-22)

Problem: on the T480s, Android listed the laptop with a blank name. On that host hostname1's
`StaticHostname` and `PrettyHostname` were both `""`; only the transient `Hostname`
(`fedora.fritz.box`, from DHCP) was set. The Wi-Fi Direct device name is set by the system
(NetworkManager / wpa_supplicant), not by MirrorWire.

Sources checked (official docs only, read 2026-11-22, D20):

| source | result |
|---|---|
| NM `org.freedesktop.NetworkManager.Device.WifiP2P` | properties `HwAddress` (deprecated), `Peers`; methods `StartFind`, `StopFind`. **No device-name property.** |
| NM `wifi-p2p` setting | `peer`, `wfd-ies`, `wps-method`. **No device-name setting.** |
| NM `org.freedesktop.NetworkManager.Settings.Hostname` | "The machine hostname stored in persistent configuration" (the static hostname), readable through the Flatpak's existing NM name (D24). |
| NM docs (above pages, NetworkManager.conf) | say nothing about where the P2P device name comes from. |
| wpa_supplicant `fi.w1.wpa_supplicant1.Interface.P2PDevice` `P2PDeviceConfig` `DeviceName` (s) | the actual value, but undocumented beyond the type, and the wpa_supplicant D-Bus API is access-denied for unprivileged users (§12). |
| systemd `org.freedesktop.hostname1` | `Hostname` (transient, never empty), `StaticHostname` (`/etc/hostname`, `""` if unset), `PrettyHostname` (`/etc/machine-info`, `""` if unset); readable without root (busctl from the container, 2026-11-22), changes go through Polkit. |

Decision (D25): MirrorWire shows a **best estimate**, not the advertised value: hostname1
`PrettyHostname`, else `StaticHostname`, else "none" (`miracast_nm::name`,
`zbus_backend::sink_name`). The Flatpak has `--system-talk-name=org.freedesktop.hostname1`
(owner-approved 2026-11-22; read-only, only these two properties), so it behaves like a native
build. When hostname1 cannot be reached anyway (no systemd-hostnamed, or the name removed with
`flatpak override`), the fallback is NM `Settings.Hostname`: a non-empty value is
shown, an empty one says nothing about the pretty name, so the line is hidden (`Unknown`)
rather than showing a possibly wrong "none". Every read is bounded to 3 s and runs on its own
task, so it never delays discovery; names are trimmed, stripped of control characters and
capped at 64 characters. The transient hostname is not used (on the T480s it was set while
Android showed no name). Nobody has confirmed that the P2P name follows the pretty name
rather than the static one; `hostnamectl hostname --pretty` plus `--static` sets both.

GUI (`Screen::Miracast`, every page except the video): "Name shown on Android: <name>"; with
no name "Name shown on Android: none ℹ"; clicking or tapping the line opens a short
explanation with `hostnamectl hostname --pretty "My Laptop"`, `hostnamectl hostname --static
my-laptop`, then turn Wi-Fi off and on. The tap instruction uses the same estimate ("tap
"<name>"", "tap the entry without a name", or "tap this computer" when unknown) instead of
MirrorWire's own viewer name. Live read in the container against the host bus (2026-11-22):
hostname1 `""`/`""`, NM `Settings.Hostname` `""` -> `Empty`. Not run in a GUI, not in an
installed Flatpak, and not checked on the tablet that a newly set name shows up after the
Wi-Fi toggle.
