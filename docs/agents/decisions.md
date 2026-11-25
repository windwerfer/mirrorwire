# Fixed decisions

These were decided with the owner (windwerfer). Agents do not change them on their own. To
propose a change, write down the problem, the alternative and its cost, and ask. When the owner
agrees, update the entry (status, date, reason) in the same commit as the change.

Every new or changed decision also records **Alternatives considered**: what else was on the
table and why it lost (cost, risk, owner preference). The table stays short; put the details in
the decision log below the table. The point is that a different agent, months later, can see
not only what was chosen but what was already rejected and why, so it isn't proposed again
without new information.

| # | Decision | Why |
|---|---|---|
| D1 | **No special privileges.** Capture only via xdg-desktop-portal ScreenCast + PipeWire. No root, `setcap`, KMS/DRM capture, kernel modules, uinput. **Amended by D26 (2026-11-22):** `/dev/uinput` is allowed only for the opt-in pen pressure, off by default; everything else here is unchanged. | The owner refuses tools that need elevated rights (the reason Sunshine was ruled out). Portals are the sanctioned Wayland path and work in a Flatpak. |
| D2 | **Flatpak-friendly, minimal permissions.** `finish-args`: `--socket=wayland`, `--socket=fallback-x11`, `--share=ipc`, `--share=network`, `--device=dri`, `--system-talk-name=org.fedoraproject.FirewallD1`, `--system-talk-name=org.freedesktop.NetworkManager` (D24), `--system-talk-name=org.freedesktop.hostname1` (D25). Nothing else. | Sandboxed install without extra rights. Screen capture needs no permission because the portal asks the user. The firewalld name was added with the owner's approval (2026-11) so the presenter can detect a blocked port and offer a polkit-authenticated Open-port button; the app never runs as root. The NetworkManager name was added with the owner's approval (2026-11-22, D24) for Miracast-compatible receiving; the hostname1 name (read-only properties) with the owner's approval (2026-11-22, D25) for the "Name shown on Android" line. |
| D3 | **Transport: QUIC (`quinn`).** Video as unreliable datagrams with per-frame fragmentation; one reliable bidirectional control stream. | Low latency without TCP head-of-line blocking; TLS 1.3 built in; one UDP port. |
| D4 | **Security: pinned self-signed certs + pairing code.** Each install generates a cert once. The viewer pins the presenter's SHA-256 fingerprint (from QR/URI or mDNS). The presenter streams only to a viewer that sent the 6-digit pairing code once, then remembers it by client-cert fingerprint. No plaintext or skip-verify mode. | "Secure and easy": no CA, no accounts, keys generated once. |
| D5 | **Identifiers:** app id `io.github.windwerfer.MirrorWire`; mDNS service `_mirrorwire._udp`; default UDP port `47800`; ALPN `mirrorwire/1`; URI scheme `mirrorwire://`; config dir name `mirrorwire`. | Users' firewall rules (firewalld `47800/udp`), pairings and launchers depend on them. |
| D6 | **No backwards compatibility until version 1.x (or until the owner says otherwise).** No migrations, no old-version support, no deprecated aliases, flags or env vars, no capability flags or optional-for-old-peers fields. Clean and lean; breaking changes are fine. One `PROTOCOL_VERSION`; presenter and viewer must match exactly (checked in `hello` before the rest is parsed; a mismatch gets a readable "update both devices" rejection). Every incompatible change bumps it, updates [protocol.md](protocol.md) (which describes only the current format) and is noted in `CHANGELOG.md`. Real runtime fallbacks unrelated to old versions (VAAPI to x264, combined portal session to plain ScreenCast, low_power retry) stay. | The owner (2026-11-16, reinforced 2026-11-19): "remove all backwards compatibility … go for code clarity and the better design in any case." Alternatives rejected: additive changes with `hello.caps` (each feature left an old-peer branch behind); a new ALPN per version (an ALPN mismatch fails inside TLS with no readable message). |
| D7 | **Codec: H.264, low-latency profile** (no B-frames, no lookahead, IDR on demand, optional intra refresh). Hardware encode first (VAAPI on Linux, VideoToolbox on macOS), x264 as fallback. | Universal hardware decode, including the 2011 iMac's CPU path. HEVC/AV1 decode is not available in hardware on the target viewer. |
| D8 | **Licence:** source `MIT OR Apache-2.0`. **Linux does not ship or bundle FFmpeg or x264 (owner 2026-11-17):** the Flatpak uses the `org.freedesktop.Platform.ffmpeg-full` runtime extension, the AppImage links the system FFmpeg (the user installs it with the package manager). macOS builds keep a static FFmpeg with only its LGPL parts + VideoToolbox. Never copy GPL code into the source tree. A missing H.264 encoder (e.g. Fedora `ffmpeg-free` has no libx264) is detected at runtime: Auto tries VA-API, then x264, and a total failure shows a GUI message with the install command; never a crash. | Permissive source, no GPL binaries from us. Rejected: keep bundling x264 (GPL binaries). |
| D9 | **macOS targets:** x86_64 minimum macOS 10.13 (`MACOSX_DEPLOYMENT_TARGET`), arm64 minimum 11.0. Since 2026-11-16 one universal DMG (both slices merged with `lipo`, each keeps its own minimum; alternative rejected: two separate DMGs, users picked the wrong one), ad-hoc signed, not notarized. | The target viewer is an old Intel iMac. Notarization needs a paid Apple account (owner decision). |
| D10 | **Renderer default:** OpenGL (`glow`) on macOS x86_64, wgpu elsewhere, with automatic fallback to glow if wgpu fails. | Old Intel Macs (Radeon HD 6770M) have no Metal; OCLP-patched macOS runs OpenGL more reliably. |
| D11 | **Target hardware.** Presenter: Lenovo ThinkPad T480s, Fedora, KDE Plasma (Wayland), Intel UHD 620 (VAAPI via RPM Fusion `intel-media-driver`). Viewer: iMac 27" Mid 2011 (i5, Radeon HD 6770M, 2560×1440) on macOS 15 Sequoia via OpenCore Legacy Patcher. | Every performance decision is judged on these two machines, not on a fast dev box. |
| D13 | **Linux is Wayland-first.** Native Wayland is the target for presenter and viewer. X11 that comes for free (winit's X11 backend, Flatpak `--socket=fallback-x11`) stays, but no X11-specific code paths or workarounds are written (no Xlib/XCB, X11 clipboard, or X11 capture). The app only *reports* which window system it runs on. | The owner's decision (2026-11). Fedora KDE is Wayland; X11 branches cost testing time and drift. Example: the clipboard uses egui/winit (smithay-clipboard on Wayland), not an X11 clipboard. |
| D14 | **Never ask for elevated privileges without an explicit user click after an explanation.** Nothing on the startup, reconnect or refresh path may cause a polkit/sudo/password dialog. A privileged action runs only from a button whose text says it will ask for the admin password; one click causes at most one dialog, a cancel or denial stops it with no retry, and the app does not ask again until the next click. If the needed information is only available with authorization, show "unknown" instead. | The owner (2026-11, after a polkit prompt opened at app start on the T480s): "we do NOT want a random app asking for root/user password, nobody will trust us." |
| D15 | **No `unsafe` Rust if at all possible; the owner is told about every new `unsafe`.** Safe APIs or another design come first. Unavoidable FFI `unsafe` (FFmpeg, VA-API, GL, JNI) is kept minimal, documented with `// SAFETY:`, never exposed in a public API, and never placed in proto or net. Every agent that adds or widens an `unsafe` block reports it to the owner with the file, the reason, and the safe alternative it rejected. | The owner (2026-11-14): "i would prefer no unsafe rust code, if at all possible. and being notified when you decide to do so." Alternatives considered: allowing `unsafe` freely in media (status quo, rejected as unreviewed risk); forbidding it entirely (rejected for now, because FFmpeg hardware encode/decode has no fully safe Rust binding). |
| D12 | **Push policy:** direct pushes to `main` are allowed (no PR required); never force-push or rewrite history; commit identity `Thomas Grok Bot Barth <windwerfer@gmail.com>`. | Small single-owner project; the owner wants fast iteration but an intact history. |
| D16 | **Windows builds (owner 2026-11-17): viewer first, MSVC, CI on `windows-latest`.** Bundling FFmpeg DLLs is allowed on Windows (no system FFmpeg there), preferring LGPL shared builds (no x264/GPL parts) with the licence notes shipped next to them. Presenter capture on Windows is a later, separate step (Windows Graphics Capture / DXGI). D7 (H.264) and D15 (no new unsafe) apply unchanged. | Windows has no package-manager FFmpeg, so a user cannot install it as on Linux (D8). The CI check job links a BtbN GPL shared build only so the tests find libx264; nothing from it is distributed. Rejected: building FFmpeg from source in CI (very slow); GPL builds in releases (licence, D8). |
| D18 | **Nightly Rust only for the sanitizer/fuzz CI job (owner 2026-11-18).** `.github/workflows/fuzz.yml` (nightly schedule + manual) runs the media and film tests under AddressSanitizer and three cargo-fuzz targets (`film_decode`, `proto_parse`, `reassembler`; 60 s each) from `fuzz/`, a crate outside the workspace. Normal builds, releases and all other CI stay on stable. | Catches memory errors in the FFmpeg FFI and panics in the pure-Rust H.264 decoder and wire parsers on hostile input (rule 1.9). `libfuzzer-sys` is `(MIT OR Apache-2.0) AND NCSA` (NCSA is permissive but not on the rule 1.11 list); acceptable because it lives only in the unshipped `fuzz/` crate, which the root `Cargo.lock` and `cargo deny` never see. Rejected: proptest (heavy tree, no coverage guidance), nightly for normal CI. |
| D19 | **Viewer display size in `hello` (protocol 8, owner 2026-11-18).** `hello.display = {width, height, scale_milli}` is required (physical pixels, scale ×1000); both viewers fill it from the egui screen rect × pixels-per-point. The presenter sanitises it (1..=16384 px, scale 0.25–8) and exposes the last viewer's value as `Presenter::viewer_display`. Rejected: a separate `display` control message after `hello` (an extra round trip and a state where Extend has no size yet); optional field (D6). Not wired into capture yet: `org.freedesktop.portal.ScreenCast` `SelectSources` has no size option, so a matching virtual monitor needs KDE/GNOME-specific work (open). Breaks: `PROTOCOL_VERSION` 7 → 8; older viewers get the version-mismatch rejection. | The owner (2026-11-18). |
| D20 | **Clean-room only (owner rule, permanent, 2026-11-21).** Implement from public specs, standards and official API docs. Never read, copy, translate or paraphrase code from other projects whose licence is not compatible (GPL/LGPL/AGPL or unknown), e.g. MiracleCast, gnome-network-displays. Permissive (MIT/Apache) dependencies via Cargo are fine if `deny.toml` allows them. Strict copyright compliance; if unsure, stop and report `NEED_MANAGER`. | The owner (2026-11-21). Keeps the `MIT OR Apache-2.0` source (D8, rule 1.11) free of derivative-work claims. |
| D21 | **Start screen (owner UI decision, 2026-11-21).** Three buttons: a large primary "Presenter" button, a small "Viewer" button, and "View your Android device (Miracast-compatible)" (label per D22; only in builds with the `miracast` feature). Advanced settings (UDP port) stay out of the way in a collapsed "Advanced" section. The Miracast button starts Wi-Fi Direct discovery right away, so the laptop is visible and waiting before the user taps it on the Android device (findings R1/R2, [miracast.md](miracast.md) §11), and shows one plain instruction plus a waiting state with Cancel; the stream then opens in the normal viewer. | The owner wants it as simple as possible for the user: one obvious main action, Miracast one click away. Rejected: a Miracast button inside the existing viewer dialog (one more step and mixed with MirrorWire presenters, although Miracast needs its own waiting flow and the user must act on the tablet first). |
| D22 | **Miracast naming (owner, 2026-11-21).** The button reads exactly "View your Android device (Miracast-compatible)". "Miracast" is used only descriptively, to say what the feature is compatible with: never as our product or feature name, never with the Wi-Fi Alliance logo, never with "certified" wording. README, store/Flathub (AppStream) descriptions and the docs say: "Supports the Miracast™ protocol (not Wi-Fi Alliance certified). Miracast is a trademark of Wi-Fi Alliance." Code identifiers and crate names (`miracast-*`, feature `miracast`, `--miracast`) stay. The feature `miracast` is a default feature with effect on Linux only. | MirrorWire is not Wi-Fi Alliance certified, and Miracast is their trademark; descriptive use tells users what works without claiming certification. Rejected: "Wi-Fi Display only" (the technically neutral spec name, but users and Android menus do not know it, so they would not find the feature). |
| D23 | **FilmCraft encoder from craft-libs (owner info, 2026-11-21).** The optional FilmCraft H.264 encoder (cargo feature `filmcraft`, `--encoder filmcraft`) comes from `craft-video` in `storytold/craft-libs` with features `h264-encode` and `threads` (slice-parallel encoding; without it 1080p encodes take about twice as long), at the same pinned craft-libs rev as the decoder in `mirrorwire-film`, so one craft-libs copy is built. `deny.toml` allows only the craft-libs git source. The encoder name `filmcraft-h264enc` reported in `StreamInfo` and the GUIs stays. | FilmCraft moved its encoder into craft-libs and removed its local `h264enc` crate; one source and one rev for encoder and decoder keeps upgrades to one bump. Rejected: keep depending on the full FilmCraft repo (`filmcraft-h264enc` at an old filmcraft rev: no longer maintained there, a second git source, and a second craft-libs pin to keep in step). |
| D24 | **Flatpak talks to NetworkManager (owner, 2026-11-22).** The Flatpak `finish-args` get `--system-talk-name=org.freedesktop.NetworkManager`, so "View your Android device (Miracast-compatible)" (D21/D22) works right after install. Only this one system-bus name; NM's own Polkit rules still decide what each call may do, and MirrorWire only creates a volatile `wifi-p2p` profile and removes it again. The in-app "not available" page stays for a missing NM/P2P device and, inside the Flatpak, adds the `flatpak override` command for users who removed the permission; a Polkit refusal has its own "not allowed" page. Flathub justification: [packaging/flatpak/README.md](../../packaging/flatpak/README.md). | Flatpak has no runtime permission dialog and no portal for NetworkManager or Wi-Fi P2P, so without the name the feature cannot work in the sandbox at all. Rejected: leave it out and show a `flatpak override` / Flatseal hint (every Flatpak user would hit a broken button first and need a terminal or an extra app to fix it). |
| D25 | **Name shown on Android is a hostname estimate (2026-11-22).** The Miracast screen shows "Name shown on Android: …" from systemd hostname1 (`PrettyHostname`, then `StaticHostname`; readable without root), with "none" plus an ℹ explanation (`hostnamectl` commands) when both are empty. The Flatpak has `--system-talk-name=org.freedesktop.hostname1` (owner-approved 2026-11-22; only the two properties are read), so it shows the same line as native builds. When hostname1 cannot be reached anyway (no systemd-hostnamed, or the name removed with `flatpak override`), NetworkManager's `Settings.Hostname` over the D24 name is used; if that is empty the line is hidden, never a guessed "none". Code and docs call it an estimate. [miracast.md](miracast.md) §16. | The Wi-Fi Direct name is set by NetworkManager/wpa_supplicant; NM's documented API has no P2P device-name property, and wpa_supplicant's `P2PDeviceConfig.DeviceName` is not readable without root. Rejected: Flatpak without the hostname1 name and only the NM fallback (first version; the owner approved the name 2026-11-22 because a Flatpak user with no static hostname, the T480s case, saw no line and no ℹ); reading wpa_supplicant (root-only); the transient hostname (set on the T480s while Android showed no name). |
| D26 | **Pen pressure through uinput, opt-in only (owner, 2026-11-22; amends D1).** On the Linux presenter, "Enable pen pressure" (off by default, saved as `pen_pressure` in `settings.json`) sends pen input to a virtual uinput pen (`MirrorWire virtual pen`: `BTN_TOOL_PEN`, `BTN_TOUCH`, `ABS_X`/`ABS_Y` 0..65535, `ABS_PRESSURE` 0..4095, `INPUT_PROP_DIRECT`) instead of the RemoteDesktop portal pointer. The checkbox unlocks only after "Check pen pressure support on my system" passes (opens `/dev/uinput` for writing, no root, no prompt). The app never changes the system: when the check fails it shows the commands for the user to run (udev rule `KERNEL=="uinput", SUBSYSTEM=="misc", TAG+="uaccess"`, inside the Flatpak also `flatpak override --user --device=all`), with an honest warning and the undo commands. No new manifest permission. Keys, and pen input while the setting is off or uinput fails, stay on the portal (D1). | The portal's RemoteDesktop interface (v2) has only KEYBOARD, POINTER and TOUCHSCREEN devices and no pressure; libei has an `ei_stylus` interface with pressure, but no KWin/Mutter support for it and no `ei_stylus` in the `reis` crate could be confirmed (2026-11-22), so there is no portal path yet. Rejected: always-on uinput (every user would need the udev rule, and the permission lets any program of the user create input devices); no pressure at all (the owner wants drawing apps such as Krita to see pressure); portal-only (cannot carry pressure today; preferred again once a compositor offers `ei_stylus` through the portal). |

## Pending owner decision (not decided, no D number)

**AirPlay FairPlay component vs D20, needs an owner exception.** Owner 2026-11-21: PARKED
roadmap item, AirPlay receiver (audio + video + screen mirroring), DO NOT START. The preferred
design (option B) keeps FairPlay as a small separate LGPL component, which conflicts with D20
(clean room, no reading or using GPL/LGPL code) and the permissive-only `deny.toml` list. It
needs an explicit, documented exception scoped to the FairPlay component only; until then
nobody reads LGPL FairPlay code, and FairPlay is never described as clean-room. Details:
[roadmap.md](roadmap.md), "AirPlay receiver". The "Not goals" line below (AirPlay
receiver/sender) would need revisiting if adopted.

## Not goals (decided)

- WebAssembly *presenter*: capture is native. A browser *viewer* (WebTransport + WebCodecs) is
  planned, see [roadmap.md](roadmap.md); not built yet.
- AirPlay receiver/sender: proprietary, and old Macs can't receive it anyway.
- Uncompressed video (~2.9 Gbit/s at 2000×1000@60): infeasible over Wi-Fi/USB networking.
- Root-based capture (KMS grab) for lower latency: conflicts with D1.

## Decision log

Newest first. Add an entry when the owner decides something non-obvious.

- **2026-11-22 (D26, opt-in pen pressure via uinput, amends D1):** owner decision (Thomas): uinput may be used only as an explicit opt-in that is off by default. Checked first against the official docs whether pressure can go through the portal: the xdg-desktop-portal RemoteDesktop page (version 2) defines device types KEYBOARD=1, POINTER=2, TOUCHSCREEN=4 and Notify* methods without pressure, tilt or distance; `ConnectToEIS` hands over a libei connection, and the libei protocol docs list an `ei_stylus` interface (pressure, distance, tilt, tip/proximity), but no KDE/GNOME release note or doc read here says the compositor offers it through the portal, and `reis` 0.7.1 (MIT, our only pure-Rust libei option) has no stylus module. So uinput (kernel docs: uinput.rst, event-codes.rst) with the `evdev` crate (Apache-2.0 OR MIT; its `VirtualDevice` keeps our code free of `unsafe`, D15). Fix text: udev rule with `TAG+="uaccess"` (systemd sd-login: access tied to the active seat session; the same pattern systemd's own `70-uaccess.rules` uses for e.g. `rfkill`), file `/etc/udev/rules.d/70-mirrorwire-uinput.rules` so it sorts before `73-seat-late.rules`, which applies the tag; `OPTIONS+="static_node=uinput"` so the tag also reaches the static node that loads the module on first access (udev(7)); `udevadm control --reload` + `udevadm trigger --action=change --sysname-match=uinput` (udevadm(8): a reload alone does not touch existing devices). Rejected for the rule: `GROUP="input"` (membership also allows reading every keyboard and mouse, and needs a re-login), `MODE="0666"` (every user and service). Inside the Flatpak `/dev/uinput` is only visible with `--device=all`: Flatpak's device list is dri, input (`/dev/input` only), usb, kvm, shm, all, so nothing narrower exists; it is a user override, never in the manifest (D2 unchanged). Rejected alternatives for the feature: always-on uinput, no pressure, portal-only (see the table). Not tested: no uinput access, pen or KDE session on the build box; the device, KWin's mapping of the virtual pen to a screen, and Android's pressure values are unverified.
- **2026-11-22 (D25, Flatpak hostname1 access):** owner approved
  `--system-talk-name=org.freedesktop.hostname1` for the Flatpak, so the "Name shown on
  Android" line (with "none ℹ" when no hostname is set) also works there. Only the read-only
  `PrettyHostname` and `StaticHostname` properties are used. AGENTS.md §1.2 and D2 list it.
  The NM `Settings.Hostname` fallback stays for hosts where hostname1 is not reachable.

- **2026-11-22 (D25, name shown on Android):** the Miracast screen shows the name Android most
  likely lists, estimated from hostname1 (pretty, then static), with an ℹ and `hostnamectl`
  commands when it is empty. In the Flatpak, NM's `Settings.Hostname` (existing D24 name)
  is the fallback and an empty answer hides the line. Rejected: a new Flatpak permission for
  `org.freedesktop.hostname1` (owner's call), wpa_supplicant's `DeviceName` (root-only).

- **2026-11-22 (D24, Flatpak NetworkManager access):** owner added
  `--system-talk-name=org.freedesktop.NetworkManager` to the Flatpak so the Miracast-compatible
  button works after install. AGENTS.md §1.2 and D2 list it as part of the ceiling. Rejected:
  no permission plus a `flatpak override`/Flatseal hint (no runtime dialog or portal exists, so
  the button would be broken by default). Kept: the "not available" page, now with the
  override command as a fallback hint inside the Flatpak; Polkit refusals got their own page
  (NM's `org.freedesktop.NetworkManager.PermissionDenied` used to be treated as a group
  formation failure and retried).

- **2026-11-18 (Windows viewer zip, D16 step 2):** build.yml builds the viewer (`gui,ffmpeg`, no
  portal) on windows-latest and zips `mirrorwire.exe` with the DLLs of the BtbN LGPL shared FFmpeg
  build, the FFmpeg licence text and our licences; the release job attaches it. Rejected: an
  FFmpeg-free viewer zip now (FilmCraft decode speed on Windows not measured; it can follow);
  building FFmpeg from source (slow). BtbN `latest` asset names rotate, so a rename breaks the job.

- **2026-11-17 (frame-ack throughput as a second ABR signal, protocol 6):** there is no
  per-frame ack message; the viewer's `link_report` carries `rx_kbps` and `window_ms` instead
  (arrival rate of first-transmission video packets), and the presenter aligns its own sent
  rate to that window from a 4 s per-frame history (`SentHistory`). Two uses, both only when the
  signals are usable (≥ 1 Mbit/s sent): a queue cut aims at 90 % of what arrived (floor: half of
  what was sent) rather than of what was sent, and growth pauses while the shortfall exceeds the
  reported loss and learned random loss by 8 points. Rejected: a growth gate on the raw
  shortfall (tried first: it froze the budget just above a shallow-buffer link, because loss
  there is treated as random and the climb to heavy loss is what produced the cut; sim:
  16 Mbit/s / 20 ms buffer 7 cuts → 0 cuts, 0.85 → 12.9 % lost, and 5.5 instead of 3.0 % lost on
  20 → 8 Mbit/s); counting the shortfall as loss (duplicates the loss rules). "No raise on a
  still screen" was already enforced (`USED_SHARE`, sent < 80 % of the budget); a test now pins
  it. Measured effect: the 40 closed-loop sim rows are unchanged (their cuts already land below
  90 % of sent); the capacity cut and the gate are shown by unit tests only, not on real Wi-Fi.
  Breaks: `PROTOCOL_VERSION` 5 → 6, `link_report` has two more required fields.
- **2026-11-17 (frame checksum, protocol 5):** every video packet header carries the CRC-32
  of the whole encoded frame (offset 44, header 44 → 48 bytes, `WIRE_VERSION` 2 → 3); the
  viewer checks it when a frame completes and never decodes a mismatching one (counted as a
  lost frame, shown in the HUD). CRC-32 rather than Glass's Fletcher-16: 32 bits are cheap
  (1.6 GB/s here, ~0.2 ms for a 300 KB keyframe, nothing measurable in the loopback) and a
  16-bit sum misses one error in 65 000, which is the whole point of the check. Hand-written
  slicing-by-8 table (no `crc32fast`: a 60-line module vs a dependency; swap if profiling ever
  says so). Per frame, not per packet: the packet is already authenticated by QUIC, what can
  be wrong is the frame we rebuilt from them. Breaks: `PROTOCOL_VERSION` 4 → 5, `WIRE_VERSION`
  2 → 3, 4 more bytes per packet.
- **2026-11-17 (NACK resend, protocol 4):** lost fragments that FEC cannot rebuild are
  requested again (protocol.md §3d). Design choices: NACK as an unreliable **datagram**
  (`0xC2`), not a control message: a lost NACK is repeated after RTT + 2 ms anyway, and the
  control stream is ordered JSON behind pongs and link reports. The viewer asks for the
  *deficit* per FEC block (missing data shards minus parity that arrived), not for every hole,
  so FEC keeps doing the cheap work and resends only fill the rest; it waits until the frame
  is settled (newer frame started / last packet in / quiet 15 ms). Delivery stays strictly in
  order, so a frame waiting for a resend holds newer ones for 2 × RTT + 4 ms (6-100 ms); the
  keyframe request (with the 250 ms to 1 s backoff) comes only after that. Resent packets
  carry header flag bit 1 and do not count as received packets or queueing delay, so loss
  reports and the bitrate/FEC control still see the on-air loss. Presenter window: 3 s /
  8 MiB, budget 128 KiB/s (burst 64 KiB), cleared per viewer. Numbers are Glass's (reference
  only, nothing copied; no MIT notice needed). Rejected: NACK on the control stream; asking for
  parity shards (no better than data shards, more decode work); resending on every hole
  immediately (duplicates what parity rebuilds anyway); an unbounded resend budget (resends
  would compete with video on a congested link). Breaks: `PROTOCOL_VERSION` 3 → 4, a viewer or
  presenter of protocol 3 is rejected with the readable version message.
- **2026-11-17 (resend budget scales with the bitrate):** the NACK resend rate is 15 % of
  the current ABR target (`LinkControl::budget_kbps`, the total wire budget), floor 128 KiB/s
  (the old fixed value), cap 2 MiB/s; the burst is half a second of the rate, at least 64 KiB.
  Why: a fixed 128 KiB/s is 3 % of a 40 Mbit/s stream, too little for a big IDR hit by loss,
  while 15 % of a 3 Mbit/s stream is below the floor anyway. Resends still compete with video
  on the same link, hence a share and a cap, not an unbounded budget. No wire change, no
  protocol bump. Breaks: none.
- **2026-11-17 (keyframe backoff and acks):** the viewer's keyframe requests back off
  250 ms, 500 ms, 1 s (was every 100 ms); the presenter's gate is 2 × RTT clamped to
  250 ms–1 s (was 500 ms–2 s); the encoder drops a request that follows an IDR directly
  (`Controls::take_keyframe_request`). Glass's "keep acking frames while waiting" is
  interpreted for our protocol, which has no per-frame ack: frames that arrive while the
  viewer waits for a keyframe still count in `link_report`, so the wait is not read as
  silence or queueing.
- **2026-11-17 (Fuzzing without cargo-fuzz):** proto/net parsers get deterministic
  fuzz-style tests (`crates/mirrorwire-proto/tests/hostile_input.rs`, a 15-line xorshift
  generator, mutation of valid packets, runs in `cargo test`) and a crate-level
  `deny(unwrap_used, expect_used, indexing_slicing, panic)` outside tests. Rejected:
  cargo-fuzz (needs nightly, which this box lacks, so it could not be run or verified;
  `libfuzzer-sys` carries an NCSA licence outside the allowed list), proptest (a heavy
  dev-dependency tree for what a small PRNG does here). Revisit cargo-fuzz in a separate
  `fuzz/` crate outside the workspace if a nightly CI job appears. `fragment()` now returns
  no packets for a frame over `MAX_FRAME_LEN` instead of panicking (the presenter logs it and
  asks for a keyframe). Not a wire change.
- **2026-11-17 (Lints in `[workspace.lints.clippy]`):** unwrap/expect/panic/indexing_slicing
  are `deny` for every product crate through the workspace table (each crate has
  `[lints] workspace = true`), replacing per-crate attributes. Tests are exempt via
  `clippy.toml`; integration test files carry an explicit `#![allow]` because clippy.toml does
  not cover them. xtask has an empty `[lints.clippy]` (dev tool, no inheritance). Rejected:
  keeping per-crate attributes (drift: media/film/android lacked indexing_slicing).

- **2026-11-16 (No backwards compatibility, D6):** the owner asked whether keeping old peers
  working was worth its maintenance cost and decided: remove all of it, and from now on
  prefer the clearer code and better design over compatibility; agents notify him of breaks
  instead of asking. Protocol 2 dropped `hello.caps` (`cursor-metadata`, `fec-rs1`), the
  presenter's embedded-cursor reopen for old viewers, plain version-1 video fragments, the
  optional `stream_info`/`link_report` fields and the adaptive bitrate's RTT fallback. The
  ALPN stays `mirrorwire/1` (a D5 identifier); the version lives in `hello.protocol` only.

- **2026-11-15 (Hardware decode by default):** the owner wants hardware decoding wherever it
  exists; software is only the fallback. The desktop viewer's `--hwdec` now defaults to on on
  Linux and Windows too (it was macOS only); `--software-decode` or the View checkbox turns it
  off. Android already tries hardware MediaCodec first.
- **2026-11-15 (FilmCraft decoder as the software fallback):** the owner wants FilmCraft's
  pure-Rust H.264 decoder (`craft-video`, feature `h264-decode`, same pinned craft-libs commit
  as the encoder) as the automatic software decoder everywhere except macOS, which keeps
  FFmpeg's software decoder for now. This replaces the earlier plan of an opt-in-only option.
  Order: hardware first (VAAPI/VideoToolbox via FFmpeg on the desktop, hardware MediaCodec
  on Android), then FilmCraft. Android no longer uses the platform's software MediaCodec
  decoders (`c2.android.avc.decoder` froze on old content after presenter encoder reopens).
  Escape hatch: `mirrorwire view --software-decoder ffmpeg`; on Android a home-screen switch
  forces FilmCraft for comparison. Evidence: bit-exact with FFmpeg on a 2804-frame VAAPI
  recording with encoder reopens and on two simulated-loss variants; tablet (MT8755,
  Cortex-A76) 14.8 ms per 1080p P-frame single-threaded vs 80–155 ms for
  `c2.android.avc.decoder`. Known cost: on the build box FilmCraft takes ~14 ms per 1080p60
  frame vs FFmpeg's ~4 ms (no SIMD yet), +~13 ms capture-to-decoded in the loopback.
- **2026-11 (Adaptive bitrate):** after a Wi-Fi test with 22–24 % loss, ~2000 unrecoverable
  frames and 9 fps at the viewer (auto FEC had climbed to +60 %/+100 %: more parity, more
  load, more loss), the owner approved: FEC parity is paid from the bitrate budget, auto FEC is
  capped at 35 % (delta) / 60 % (keyframes) and its boost does not grow under congestion, and an
  AIMD controller (floor 3 Mbit/s, ceiling = the bitrate setting) follows the viewer's loss
  reports and the RTT. On by default; `--adaptive-bitrate`, `MIRRORWIRE_ADAPTIVE_BITRATE`,
  `settings.json`, Quality panel. x264 dropped `nal-hrd=cbr` (filler padded every frame to the
  target and blocks live rate changes).

- **2026-11 (No unprompted password dialogs, D14):** on Fedora KDE (Flatpak) a polkit
  dialog opened as soon as presenting started, although 47800/udp was already open: the
  firewall probe called firewalld's `queryPort`/`queryService`, which need
  `org.fedoraproject.FirewallD1.config.info` (`auth_admin_keep` in firewalld's default
  server policy; firewalld always lets polkit interact). Cancelling the Open-port
  dialog showed it again for each following call. Now the probe uses only
  `.info` calls (rules only after a non-interactive polkit check), a viewer reaching
  the presenter counts as OK, and the opt-in button stops at the first cancelled
  authorization. The presenter window also scrolls, with diagnostics under "Details".

- **2026-11 (Presenter port and firewall):** the owner does not want a "magic always-open
  port" (none exists across distros). Keep default UDP 47800 (D5). The port is configurable
  (`--port`, `MIRRORWIRE_PORT`, a saved field on the presenter start screen). mDNS and the
  QR/URI already carry the port, so the viewer finds a changed port automatically. If a
  firewall is detected, the presenter shows a prompt: an **Open port for me…** button
  when firewalld is reachable over D-Bus (polkit password prompt, runtime + permanent; see D14),
  otherwise a short general guide (Fedora `firewall-cmd`, one Ubuntu `ufw` line, and "look
  up your own distro if different") — not a list of every OS. The same guide is in the
  README troubleshooting section. A `mirrorwire.xml` firewalld service (UDP 47800) is
  shipped in the Flatpak/AppImage for packagers; it only works once installed on the host.
- **2026-11 (FilmCraft encoder built; source superseded by D23):** `filmcraft-h264enc` is an optional presenter encoder
  behind cargo feature `filmcraft` (`--encoder filmcraft`, or the Encoder selector in the
  presenter's Quality panel). FFmpeg stays the default; `auto` never picks FilmCraft. The git
  dependency is pinned to rev `cb9783ba` of `storytold/filmcraft` (it pulls `craft-media` from
  `storytold/craft-libs` at a pinned rev); `deny.toml` allows exactly those two git sources.
  Input is the same swscale I420 picture x264 gets (shared-memory capture; a DMA-BUF stream is
  switched to shared memory). Settings: Constrained Baseline, `speed` preset, no B-frames,
  no scenecut, CBR/VBR/QP from the usual RC options, one slice per core (2–8,
  `MIRRORWIRE_FILMCRAFT_SLICES` overrides). FilmCraft has no force-IDR call and no intra
  refresh, so a keyframe request re-creates the encoder (rate-control history restarts).
  Enabled in the Flatpak and AppImage (+0.8 MB stripped, a few seconds of build time); not in
  the DMG (the Mac is a viewer) or the APK (viewer only). Box numbers are in
  [testing.md](testing.md) §3.1; T480s numbers are still untested on target.
- **2026-11 (Wayland-first, D13):** after "Copy log" failed on the T480s (arboard tried an X11
  connection in the Flatpak), the owner fixed Linux as Wayland-first: keep free X11 fallback,
  never write X11-specific code. Clipboard now goes through egui/winit (smithay-clipboard).
- **2026-11 (Android viewer):** the owner wants an installable APK (thin Kotlin/Java shell that
  is never touched, loading the Rust viewer, the neurofeed pattern), and later a browser
  viewer for people who don't want to install anything. Built as: one `NativeActivity`
  subclass in Java (multicast lock, link hand-off), all logic in `crates/mirrorwire-android`
  (egui/glow, MediaCodec, no FFmpeg), APK built without Gradle, debug-signed arm64-v8a,
  minSdk 26. The browser viewer (WebTransport + WebCodecs) is a roadmap sketch, not built.

- **2026-11 (GitHub CodeQL):** the owner wants CodeQL as a second, separate
  security check next to SonarCloud, on every push to `main`, on pull
  requests, and as part of tag/manual Builds, plus a weekly schedule.
  Rust and GitHub Actions are analyzed with `build-mode: none`. Default
  setup must stay off so the advanced workflow can upload results.
- **2026-11 (SonarCloud CI analysis):** the owner added a `SONAR_TOKEN` repository
  secret and wants scans to run from GitHub Actions (Clippy report + lcov)
  instead of Automatic Analysis. Automatic Analysis must be turned off under
  Administration › Analysis method, or the CI upload is rejected.
- **2026-11-17 (CI scan switch):** the CI SonarCloud scan failed with "Automatic Analysis is
  enabled" and only the owner can change that in the SonarCloud web UI. The scan stays (it
  carries clippy + coverage) but runs only when the repository variable `SONAR_CI_ANALYSIS` is
  `true`; otherwise it skips with a notice and Automatic Analysis covers the repo. To get the CI
  scan: turn Automatic Analysis off, then set the variable. Both at once is never valid.
- **2026-11-17 (CI build fixes):** the AppImage job restored a static-FFmpeg cache whose `.pc`
  files held the old repo path (rename), so headers were not found; the cache key now includes
  the repo name and a check step fails early on a stale prefix. The Flatpak job removes and
  reinstalls the image's runtimes first, because in-place updates fail on the container overlay.
- **2026-11 (FilmCraft encoder):** the owner wants the FFmpeg version finished first, then the
  pure-Rust `filmcraft-h264enc` added as a switchable option for a side-by-side comparison. x264
  stays until then. Optional "use the system FFmpeg" mode accepted as a plan.
- **2026-11 (DMG packaging):** macOS artifacts are single DMG files uploaded with
  `archive: false`, no zip inside a zip.
- **2026-11 (renderer):** OpenGL default on macOS x86_64 (D10) after the target iMac turned
  out to be non-Metal.
- **2026-11 (name):** "MirrorWire" (ScreenBeam, MirrorCast, QuickMirror and "Wi-Fi Display"
  were taken or trademarked).
- **2026-11-15 (rename):** the owner renamed the app to **mirrorwire** (repo
  `github.com/windwerfer/mirrorwire`), before any release. Full rename, clean break: every D5
  identifier moved from `mirrorwire`/`MirrorWire` to `mirrorwire`/`mirrorwire` (app id, mDNS
  service, ALPN, URI scheme, config dir, binary, crates, `mirrorwire_*` env vars, firewalld
  service, Android package). Port 47800 is unchanged. No migration of the old `mirrorwire`
  config dir: viewers pair again once. MirrorWire builds and mirrorwire builds cannot see or
  talk to each other.
- **2026-11-16 (rename back):** the owner changed the name back to **MirrorWire**. Every D5
  identifier moved from `mirrorwire`/`mirrorwire` back to `mirrorwire`/`MirrorWire` (same list as
  above, `MIRRORWIRE_*` env vars). The GitHub repo and the SonarCloud project key stay
  `windwerfer/mirrorwire` / `windwerfer_mirrorwire` until the owner renames them. Port 47800 is
  unchanged. No migration of the `mirrorwire` config dir: viewers pair again once. mirrorwire
  builds and MirrorWire builds cannot see or talk to each other.

(2026-11-17, D15 follow-up) `unsafe_code` is enforced by the compiler per crate: `forbid` in proto, net, film, xtask; `deny` in app, media and android, with `#[allow(unsafe_code)]` only on the modules listed in HANDOFF/roadmap 0c (yuv_glow, decode, encode, hw, source::portal, android gpu). A new `unsafe` elsewhere fails to compile; widening the allow list needs the owner (D15).

(2026-11-17, bottleneck classifier) The classifier moved from the app crate to `mirrorwire-proto/src/bottleneck.rs` (pure std, next to `rate.rs`, `fec.rs`) so the desktop and Android viewers use one implementation and send identical verdicts. Rejected: a copy in the Android crate (two sets of thresholds), and a new crate (too small to justify). Android decoder kind comes from the label prefix `SOFTWARE`, as the HUD already shows it.

(2026-11-17, extend mode, roadmap 6 first part) `CaptureMode` {mirror, extend} lives in `mirrorwire-media::source` (always compiled; the app prefs and the portal both use it). Mirror asks the portal for source type MONITOR only; Extend asks for VIRTUAL after reading AvailableSourceTypes and falls back to MONITOR with a note (shown next to the cursor note in the Quality panel). Each mode has its own restore token file (`portal_restore_token`, `portal_restore_token_extend`) because the portal ties a token to the granted source type. Setting: `--mode`, `MIRRORWIRE_MODE`, `settings.json` `mode`, Mode box in the Quality panel (re-opens the capture like cursor/fps). Breaks: Mirror no longer offers virtual monitors in the portal dialog (they were selectable by accident before); old `settings.json` files without `mode` load as mirror, nothing is lost. Rejected: one shared token (the portal would hand back the wrong source type); sending the viewer's window size in hello (needs a protocol bump, left open).
- **2026-11-17 (Linux packaging, D8):** Flatpak manifest drops the x264 and FFmpeg modules and adds the `ffmpeg-full` extension (`lib/ffmpeg`, `add-ld-path`); the AppImage job builds against Ubuntu's dev packages, links dynamically and excludes `libav*`/`libsw*` from the bundle. Rejected: a custom LGPL-only static FFmpeg on Linux (no VA-API/x264 gain, still our binaries to maintain). Unverified: SDK headers matching the extension's soname; FFmpeg 6.1 (Ubuntu 24.04) with ffmpeg-next 9.
- **2026-11-18 (optional FFmpeg, D17, step 1 of 2):** goal is an FFmpeg-free (pure MIT/Apache) build later without removing FFmpeg. Step 1 done: `mirrorwire-media/src/convert.rs`, a safe scalar BT.601 limited-range BGRA/BGRx/RGBA/RGBx → I420 converter (bounds-checked, odd sizes), unit-tested against swscale (luma ±3, chroma ±20 at hard edges: swscale sites chroma differently). Not wired in yet; swscale stays the default converter where FFmpeg is on. Step 2 (open): cargo feature `ffmpeg` (default on) in media/app, FFmpeg types only behind the `Encoder`/`VideoDecoder` seams, `Encoder` split so the FilmCraft path has no `ffmpeg_next::frame::Video` in its signature (`FcEncoder::encode` takes one today), CI builds `--no-default-features --features filmcraft`. Rejected for step 1: a SIMD converter (scalar is enough for the FilmCraft path, which is slower than the conversion anyway; measure first, rule 1.7).
- **2026-11-18 (optional FFmpeg, D17, step 2 of 2):** cargo feature `ffmpeg` (default on; `static-ffmpeg` implies it) in `mirrorwire-media` and `mirrorwire-app`; `ffmpeg-next` is optional. `--no-default-features --features filmcraft` builds with no libav* (CI and `cargo xtask ci` build, clippy and test it). `encode/mod.rs` is now config + `path` + `DmaBufError` + an `Encoder` enum (`Ffmpeg(FfmpegEncoder)` in `encode/ffmpeg.rs`, `Film(FilmEncoder)` in `filmcraft.rs`) that forwards each method; nothing outside `encode/` changed. Without FFmpeg: `--encoder x264|vaapi|videotoolbox` fails with a readable message, the viewer uses FilmCraft's decoder and `convert::yuv_to_rgba` (pure Rust, BT.601) instead of swscale. **FilmCraft encoder now always uses `convert.rs`** (swscale is no longer used for it, even with FFmpeg on): the optimised scalar converter measured faster than swscale on loopback 1080p (capture_to_encoded 13 ms vs 16 ms; the first naive version was 20 ms, hence the rewrite). **Breaks:** `--encoder filmcraft` no longer scales: a capture size different from `--size` is an error ("does not scale", visible in the GUI); omit `--size`. Rejected: keeping a swscale path inside `FilmEncoder` (re-couples the type to FFmpeg for a rarely used option), SIMD converter (not needed, measure first). Not done: a direct VAAPI backend without FFmpeg; macOS keeps FFmpeg decode/encode.
- **2026-11-18 (loopback budget):** `capture_to_decoded_p95_ms_max` tightened from 60 to 40 ms (owner decision). Measured p95: 11-15 ms on the dev VM, 15-29 ms on CI runners; 40 ms leaves about 1.4x headroom over the worst CI run and still catches a 2-3x regression. Rejected: 30 ms (a single noisy CI run at 29 ms would flake).
- **2026-11-18 (nightly ASan + cargo-fuzz, D18):** added `fuzz/` (libfuzzer-sys 0.4; targets `film_decode` = FilmCraft H.264 decoder on access units split at 0xFF, `proto_parse` = PacketHeader/Nack/PointerUpdate/CursorImage/Control parsers, `reassembler` = datagram stream into `Reassembler`) and `fuzz.yml`. Ran locally 20 s per target (no findings), but the first CI run found a real bug: craft-libs h264 `decoder.rs:532` panics with "attempt to subtract with overflow" on corrupt input (release builds wrap, and `FilmDecoder` catches panics, so no crash in the app; it is upstream, to be reported to FilmCraft). The `film_decode` step is therefore `continue-on-error` until fixed and the media tests under ASan (21 passed, ffmpeg feature). The fuzz crate does not cover the FFmpeg decoder (needs libav); the ASan job exercises it through the corrupt-H.264 unit test. Not verified: the GitHub workflow itself (first manual run). Supersedes the 2026-11-17 note that cargo-fuzz was impossible.
- **2026-11-18 (native size and limits, roadmap 1):** the encoded size is the capture size by default; `resolved_size` rounds to even, derives the missing edge from the capture aspect, and caps at 4096 px edge / 4096x2304 area (H.264 level 5.1, VA-API hardware limit), scaling down keeping the aspect (5K -> 4096x2304). Alternatives rejected: no cap (5K/8K captures fail encoder open with an unreadable error), cap at 1080p (blurs desktop text). Roadmap 0.6 (live VAAPI bitrate) left blocked: FFmpeg cannot do it and a direct libva path needs new `unsafe` plus a rejected dependency; owner must choose.
- **2026-11-18 (presenter remote, protocol 9):** the viewer sends `remote_key` (`next`/`previous`/`space`/`escape`) on the existing reliable control stream; the presenter rate-limits it (10 taps/s per viewer) and taps the key through the xdg-desktop-portal **RemoteDesktop** portal (`notify_keyboard_keysym`, keyboard device only; `media/src/source/remote.rs`; ashpd feature `remote_desktop`, no new crate, no new Flatpak permission, no uinput, D1). The set of keys is a closed enum, never raw key codes, so a paired but hostile viewer can flip slides but not type. The portal session opens lazily on the first tap (a second portal dialog, kept with a restore token in `remote-desktop.token`); its state, or why it failed, shows in the presenter window. Viewer UI: desktop viewer buttons over the video (R toggles), Android buttons in the tap overlay. **Breaks:** PROTOCOL_VERSION 8 -> 9. Rejected: reusing the cursor datagram path (a key tap must not be lost: reliable stream instead); combining it into the ScreenCast session (the portal then asks about screen and keyboard together and a keyboard denial would kill the video); sending raw keycodes (see above); pointer clicks (the "click" button is Space; mouse injection belongs to the later touchpad mode). Not tested: no portal or real PowerPoint on the build box.
- **2026-11-18 (Extend sizing, owner option 2):** the VIRTUAL source keeps the compositor's default size; the presenter window (Extend mode, viewer connected) shows the viewer's display size from hello (`CaptureStatus::viewer_display`, fed by `Presenter::viewer_display`) and tells the user to set that resolution in System Settings → Display. Rejected: calling KWin over D-Bus (`org.kde.KWin` needs a new Flatpak `--talk-name`, rule 1.2) and `zkde_screencast stream_virtual_output` (reserved for the portal backend). May be revisited. Not tried on a real KDE desktop.
- **2026-11-18 (hardware findings: VAAPI low-power, reassembler livelock):** (1) `h264_vaapi` is opened with `low_power=0` first and retried with `low_power=1` when that fails (Intel iHD on the T480s offers H.264 only as `EncSliceLP`, CQP-only on the normal entry); the combined error names `intel-media-va-driver-non-free`. Rejected: probing entrypoints with libva (new unsafe). (2) Cause of the x264 run's permanent freeze (`in 0.0 fps`, unrecoverable +50/s, loss 0 %): `Reassembler::poll` gave up one missing frame id per `hold_us` and restarted the timer for each, so after an outage of N frames it consumed ids slower (1/hold ≈ 55/s) than the presenter produced new ones (60/s) while `evict_stale` removed the very frames it was heading for. Now the hold is measured from the first packet of the oldest newer frame and a run of missing ids is skipped at once. Regression test `missing_run_is_skipped_at_once`. Not verified on hardware. Breaks: none (viewer-local behaviour, no wire change).
- **2026-11-18 (Extend size dropdown):** in Extend mode the Quality panel has a "Size" dropdown: "Auto" (the viewer's display from hello) or a common size (1280x800 … 2800x1752, `EXTEND_SIZES`). It is a session choice (`CapturePrefs::extend_size`, not saved, no restart) and drives the hint "set WxH in System Settings → Display" everywhere. It does not yet move the virtual monitor or the encode size: the KWin route (option 1) needs a D-Bus call or a new Flatpak `--talk-name` and is not done; non-Flatpak-only KWin sizing and an encode-size target remain open. Unit test `extend_size_resolves_auto_to_the_viewer`. Breaks: none. Not seen on a real desktop.
- **2026-11-18 (camera QR scan, step 1: decoder):** `mirrorwire_net::pairing::scan_qr_luma(w, h, luma)` finds a `mirrorwire://` QR in a grayscale image (a camera frame's luma plane) and returns the parsed `ConnectInfo`; other QR codes, noise and bad sizes give `None`, input is capped at 16 Mpixel, no panics (unit tests render our own QR codes). Dependency: `rqrr` 0.11 (MIT, pure safe Rust, deps `g2p` + `lru`; ~1 small crate tree) in net, next to the QR renderer, because the image is untrusted input. Rejected: `rxing` (large ZXing port, many deps), `quircs` (C-derived, bindgen/unsafe-adjacent), `image` crate (not needed for luma). NOT done: the camera source itself (xdg-desktop-portal Camera + a PipeWire video-input stream in `mirrorwire-media`, then a "Scan" button in the viewer) — needs real camera formats (YUY2/MJPEG/NV12) and a device to test, so it is left for a hardware session. Breaks: none.
- **2026-11-18 (roadmap 0.6 closed: live VAAPI bitrate via reopen):** manager decision after hardware data (T480s, iHD non-free, low-power path, TB336FU over Wi-Fi: reopen ~27 ms, ~3 cuts/min, ~50 fps, 0 unrecoverable, no visible stutter). The reopen path stays; no direct libva backend, no private-struct offsets, no cros-codecs, so no new `unsafe` was added (the earlier approval of an isolated libva module is not used). Supersedes the "left blocked" note in the native-size entry. Also confirmed by the owner on hardware: the presenter remote (KDE RemoteDesktop portal, Android buttons, a real document). Details in testing.md §3.10. Breaks: none.
- **2026-11-18 (Extend size as encode target):** `EncoderConfig::max_size` caps the encoded picture: a larger capture is scaled down keeping the aspect ratio (even edges, level 5.1 limits still apply), a smaller one is never upscaled. In Extend mode the presenter window sets it every frame to the resolved Extend size (Auto = the viewer's display from hello, or a dropdown size); Mirror mode clears it. A change goes through the existing size check and reopens the encoder (`set_max_size` is a no-op for the same value). Unit test `max_size_caps_keeping_aspect_and_never_upscales`. KWin sizing on non-Flatpak builds is NOT done: it needs a D-Bus API on `org.kde.KWin`/KScreen whose virtual-output sizing cannot be checked without a KDE desktop and has no clean, safe, permission-free route; the dropdown + hint stay the way to size the monitor. Rejected: stretching to the chosen size (distorts when the aspect differs). Breaks: none. Not tried on hardware.
- **2026-11-18 (pen/touch as an absolute pointer, protocol 10):** the viewer sends `pointer {phase: down|move|up, x, y}` (x/y = `u16`, 0..65535 inside the picture) on the **reliable control stream**; the presenter rate-limits it (`PointerGate`: 120 moves/s, 40 downs/s, ups never refused so a button cannot stick) and injects it through the RemoteDesktop portal (`notify_pointer_motion_absolute` + `notify_pointer_button` BTN_LEFT; no uinput, D1; no new crate, no new Flatpak permission, no `unsafe`). Position is normalised to the picture rectangle, not the window (`pointer_in_rect`, clamped, NaN rejected) and mapped to the stream size with `pointer_to_stream` (always inside the monitor); both unit-tested. Viewer: "Pen" toggle (desktop: button next to the remote buttons; Android: under Disconnect), off by default so a normal tap does not click on the presenter; moves coalesced to ~60/s; leaving pen mode releases a pressed button. No pressure/tilt (that needs uinput or a libei tablet tool: roadmap wishlist). Absolute motion is addressed to a screen-cast stream of the same portal session, so the pointer session selects a MONITOR source too: the remote session opens key-only on a key tap and is **closed and reopened with POINTER (one more dialog: "choose the screen to control")** on the first pointer `down`; the restore token (`remote-desktop.token`) is reused, so the dialog returns once for owners of an older keyboard-only token. **Mirror only:** in Extend the second session would not see the virtual monitor (the VIRTUAL source belongs to the capture session), so the presenter shows "pen/touch input works in Mirror mode only"; fixing that means running capture and input in one RemoteDesktop session, a rework of the working capture path that cannot be tested without a desktop (open). Rejected: datagrams for moves (a lost down/up sticks the button; the control stream's head-of-line cost at 60 moves/s is negligible on a LAN); sharing the capture's ScreenCast session now (see above); `RemoteInput` carrying raw button codes (closed enum). **Breaks:** PROTOCOL_VERSION 9 -> 10. Not tested: no portal, pen or touch screen on the build box; only wire/mapping/rate-limit/loopback unit tests.

- **2026-11-18 (roadmap 0c closed, unsafe stays where it is):** manager decision (option 2). All `unsafe` lives in `mirrorwire-media` (decode, encode, hw, source::portal) plus the glow/EGL FFI in the viewers (yuv_glow, android gpu); proto, net, film and xtask `forbid` it, app/media/android `deny` it with per-module allows; every site has a `// SAFETY:` comment, `clippy::undocumented_unsafe_blocks` is on, and the nightly ASan job (D18) covers the media tests. Rejected: moving the VAAPI/PipeWire FFI into one module (move-only refactor of paths that cannot be tested without hardware, risk without gain). Revisit only if the parked own VA-API encoder replaces FFmpeg.
- **2026-11-18 (capture and input in ONE RemoteDesktop session; pen works in Extend):** owner order 20:13. On Linux the presenter now opens a single RemoteDesktop session: `CreateSession`, `SelectDevices` KEYBOARD|POINTER, ScreenCast `SelectSources` (MONITOR or VIRTUAL by mode) on that same session handle, one `Start`, `OpenPipeWireRemote` on it (`portal.rs::start_combined`). The stream we capture is therefore the stream `NotifyPointerMotionAbsolute` is addressed to, in Mirror and in Extend; keys use the same session. The injection code is shared (`remote.rs::inject`) and runs on the capture's portal thread (D-Bus calls stay on one runtime); the presenter sends input through a stable route (`RemoteRoute` in `presenter/mod.rs`) that follows source reopens. Restore tokens: one per mode and per device set (`portal_restore_token[_extend]_input`, separate from the capture-only tokens), so a changed device set simply gets a fresh dialog. **Fallback:** if the combined session is unavailable (no RemoteDesktop portal, no keyboard/pointer offered, denied, or no stream size) the capture retries on a plain ScreenCast session (Mirror capture cannot regress) and the old separate lazy injector (`remote.rs`, Mirror-only pen) is used; the presenter window says why. Cost: the first share dialog now also covers keyboard/pointer permission (one dialog instead of capture + a later input dialog), and a user who cancels the combined dialog sees a second, capture-only one. The pointer maps on the stream's own size, not the encode size (`EncoderConfig::max_size` rescales the picture; positions are normalised), unit-tested. Rejected: sharing the Screencast session across two portal interfaces (input needs the RemoteDesktop session handle); keeping the lazy key-only session and upgrading it (the old two-session design is exactly what could not see the virtual monitor). No new unsafe, crate or Flatpak permission. Not tested: no portal on the build box; only token/mapping unit tests.
- **2026-11-19 (camera QR scan, step 2: camera source + Scan button):** `mirrorwire-media/src/source/camera.rs`: `extract_luma` (pure, bounds-checked, unit-tested: YUY2, UYVY, NV12/I420 Y plane, GRAY8, BGRx, RGBx with stride padding) and, on Linux with feature `portal`, `CameraScanner<T>`: Camera portal (`request_access` + `open_pipe_wire_remote`, ashpd feature `camera`, no new crate), a PipeWire capture stream with role Camera and no node id (the portal's remote exposes only cameras), frames throttled to ~7/s, newest luma handed to a worker thread that runs the caller's decoder (the app passes `pairing::scan_qr_luma`); events `Status/Found/Failed` show in the connect screen ("Scan QR with camera" button next to Connect, `gui/scan.rs`; hidden on other platforms). Raw formats only: an MJPEG-only camera reports "this camera sends ..., which the scanner cannot read" in the window. No new `unsafe`, no new Flatpak permission (the Camera portal needs none). Rejected: a JPEG decoder dependency for MJPEG (most UVC cameras also offer YUY2/NV12; add only if hardware shows otherwise); a registry walk to pick the node (the portal remote is already filtered to cameras); decoding on the PipeWire thread (would stall the loop). Breaks: none. NOT tested on a camera (none on the build box): only the luma extraction has unit tests; the portal/PipeWire path compiles and is clippy-clean only.

- **2026-11-25 (input never stalls video; fallback injector keys only; owner report t316u):** after pressing Pen on the Android viewer the Linux presenter kept running but sent no more video. Found in the code: the presenter's control reader `await`ed room in the remote-input channel for every pointer down/up, and that reader is the only thing that marks the viewer alive; a stuck input path therefore made the viewer look silent after `VIEWER_SILENT` (1.5 s) and the send loop paused the video for good. Now: (1) the control reader hands input over with `try_send` only (`InputForward`: a full channel drops moves/presses, a release that does not fit is held and retried on each later control message); (2) `RemoteRoute` (incl. the blocking uinput open/write, D26) runs on its own `remote-input` thread, not on the two-worker network runtime; (3) every portal input call is bounded (`remote::INPUT_TIMEOUT`, 2 s), and in the combined session a stop wins over an in-flight call, so `PortalSource::drop`/a capture reopen never waits on input; (4) the separate fallback injector (`remote.rs`) is **keys only**: it no longer opens a second RemoteDesktop+ScreenCast session (`SelectSources` + `Start`, a "choose the screen" dialog) for pen/touch while the capture runs, and pen/touch there shows "pen/touch is not available: capture and input could not share one portal session …". This supersedes the Mirror-only pen fallback of the 2026-11-18 entries (amends, does not reverse, the combined-session design; the virtual uinput pen still works in the fallback). Rejected: keeping the second screen-cast (the portal docs do not promise it leaves the running cast alone, it needs a dialog at the laptop while the user is at the tablet, and it retried a dialog on every pen down after a failure); unbounded input channels (hostile input, §1.9). Breaks: none on the wire (no protocol bump); fallback users lose pen/touch through the portal. Tested: unit/e2e only (the net e2e test fails before the fix: viewer silent 2.2 s); not hardware-tested.
