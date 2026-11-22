# HANDOFF (branch flatpak-nm, based on main a5344da)

## Goal
Current (step 17, 2026-11-22): owner approved `--system-talk-name=org.freedesktop.hostname1` for the Flatpak so the "Name shown on Android" line (D25) also works there.

Step 16 (2026-11-22): show the name Android lists for this computer on the Miracast screen, with an ℹ hint (hostnamectl) when it is empty (D25).

Step 14 (2026-11-22): owner decision D24, add `--system-talk-name=org.freedesktop.NetworkManager` to the Flatpak so the Miracast-compatible button works after install; accurate "not available"/"not allowed" messages; Flathub justification text.

Step 13 (2026-11-21, 2026-11-21): make `cargo xtask ci` green again (clippy dead_code) and take the FilmCraft encoder from craft-libs (`craft-video`, `h264-encode`) with one craft-libs pin for the workspace; then full CI on the box and merge into main.

Original: Miracast sink spike (Android tablet TB336FU = source, Fedora KDE laptop = sink), rootless via NetworkManager D-Bus WifiP2P + Polkit. Clean-room, new crates only, never touch main. Steps 1-6 one per session.

## Done
- Step 1: `docs/agents/miracast.md` (packages, Polkit, firewall, P2P network structure, WFD IE, M1-M7 flow, media formats, Android expectations, Intel 8265 notes, spike architecture, test plan, commands to measure the T480s).

- Step 2: miracast.md section 8a: mac80211_hwsim/virtual radios NOT usable without root here (no module tree, no Wi-Fi stack; userns works but cannot create radios). Skipped; tests use mocks + loopback.

- Step 3: crates `miracast-nm` (P2pProvider trait, MockP2pProvider, NmApi/NmP2pProvider + MockNm, zbus backend `ZbusNm`, wfd IE encode/parse, Sink API), `miracast-rtsp` and `miracast-rtp` (type skeletons). In xtask LAYERS. 17 tests, clippy -D warnings and `cargo xtask ci --no-loopback` green. No unsafe, no new dependency (zbus already in the workspace). Details: miracast.md "Crate skeleton".

- Step 4: `miracast-rtsp` sink M1-M7 state machine (`message`, `params`, `SinkSession`), 21 tests, clippy -D warnings and `cargo xtask ci --no-loopback` green. No unsafe, no dependencies. Details: miracast.md "RTSP sink (step 4)".

- Step 5: `miracast-rtp` RTP parse + reorder, TS demux (PAT/PMT/PES/CC), `Receiver`, `MediaSink` loss -> `need_keyframe`, `ts::synth` stream builders, 16 tests incl. a real ffmpeg mpegts stream; clippy -D warnings green, no unsafe, no deps. Details: miracast.md "RTP/TS receive (step 5)". Decoder-feeding example binary NOT done (moved to step 6).

- Step 6: crate `miracast-loopback` (real-socket sink driver, `TestSource` speaking M1-M7 + RTP/TS, 3 e2e tests incl. loss -> M13 IDR request -> recovery, hardware test binary `miracast-sink-demo`). `cargo xtask ci --no-loopback` green, clippy -D warnings green, no unsafe, no new dependency. Decoder example NOT done (kept spike crates free of mirrorwire-* deps; `ffplay` on the dumped stream instead). Details: miracast.md "Loopback end-to-end (step 6)".

- Step 7 (after hardware runs 1-3, 2026-11-21; details in miracast.md section 11):
  - `468a665` LPCM audio offer (`sink_config`, `LPCM 00000002 00`) + accept lower H.264 levels (`c.level <= o.level`), both hardware-confirmed, with unit tests.
  - `1e2651f` miracast-nm R1/R2 flow: `Sink::connect_source` -> `NmP2pProvider::connect` = `AddAndActivateConnection2` (volatile profile with our WFD IEs, PBC, ipv4 auto), poll `State` until ACTIVATED (120 s for the user's tap), read `IpInterface` + `Ip4Config` -> `GroupInfo` (gateway = GO = source = `peer_ip`), deactivate on failure. `wfd_source_port` picks sources. Mock tests only.
  - `ecad42c` sink connects to the source's RTSP server (`run_sink(source, ..)`, retry 500 ms); `miracast-sink-demo --source 192.168.49.1:7236` replaces the socat bridge; `TestSource` listens like the tablet.
  - `d3d2899` `miracast_rtp::h264` SPS parser (profile, level, cropped size), test with the first SPS of `stream-run3.h264` (High 4.0 1920x1080); all 34 SPS of that dump parse the same. Demo logs the stream SPS.
  - `1d3873a` `miracast_rtp::diag::NoRtp` ("RTSP ok, no RTP in 5 s": port, zone `public` or the looked-up one, runtime-only `firewall-cmd` fix text); in `SinkReport::no_rtp`. Not wired to a GUI.

- Step 8 (hardware run 4, 2026-11-21, `~/tmp/mc-hw/*run4*`):
  - `e4309af` `miracast-sink-demo --nm`: find -> sources -> connect_source -> run_sink(peer_ip:7236) -> leave group; `nm_flow::join_source` (mock-tested), `RunOptions` (deadline, no-RTP timeout, interface for the NoRtp text).
  - `651f0f6` hardware fix: group formation failed in 2 of 5 activations on the tablet side; `join_source` now re-activates (default 3 attempts, `--attempts`) and the error text no longer blames the connect prompt.
  - `658cb8c` hardware fix: `RtpStalled` diagnostic when media stops for 5 s after flowing (attempt c lost the link after 37 s and sat silent 57 s until the keepalive timeout); `SinkReport::stalls`.
  - Hardware results: `--nm` works without socat. Attempts c, run4 and d connected; the RTSP connect, SPS line `1920x1080 profile 100 level 4.0`, ~30 fps video, M13 answered, and a clean TEARDOWN all happened (run4: 10 SPS/10 PPS/10 IDR/826 non-IDR, 144 lost packets, 3 IDR requests in 30 s; 4d: 1 lost packet). UDP 19000 reachable, so NoRtp did not fire (not verified on hardware). Cleanup was verified: no profile, no p2p interface, wlp58s0 activated, wireless display off, rotation 1/1 unchanged.

- Step 9 (stability, hardware run 5, 2026-11-21, `~/tmp/mc-hw/*run5*`):
  - `1942535` research (miracast.md section 12): the GO (tablet, intent 14 vs our 7) picks the channel and uses its own STA channel (2462); NM offers no GO-intent or frequency knob (live API), wpa_supplicant D-Bus is root-only, so steering or becoming GO is not possible rootlessly. Official NM and wpa_supplicant doc pages could not be fetched (permission denied), so those points are marked [docs].
  - `6ad2e1c` IDR requests rate-limited (1/s); `e66a624` time-bounded jitter buffer (40 ms) + RxStats; `96c0e13` close on silence (link lost), stats lines, no panic on a dead TCP write; `8ab16e0` demo `--reconnect N`, STA channel logged around each session; `03258f8` TEARDOWN at the deadline (found in run 5).
  - Run 5 result: 5/5 attempts streamed at least 5 min (306-406 s), 0 link losses, loss 0.00-0.43 %, groups on 2462 MHz = STA 2462 MHz every time. Group formation failed once in 2 attempts and the retry fixed it. The table is in miracast.md.

- Step 10 (viewer integration, hardware run 6, 2026-11-21, `~/tmp/mc-hw/*run6*`):
  - `a4f80f8` roadmap TODO: optional root prompt for the channel (idea/optional), noted in miracast.md section 12.
  - `38f6d43` sink driver usable from the app (stop flag, no unwraps); `90929b0` TestSource::datagrams_for.
  - `e5f3426` `mirrorwire view --miracast [--miracast-peer NAME]` (app feature `miracast`, module `mirrorwire-app/src/miracast.rs`): NM join -> RTSP/RTP -> the app's `open_decoder`, sized from the SPS, into the existing `ViewerHandle`/video view; `ViewerStatus::note` for progress.
  - Fixes from the hardware runs: `194d4fc` bounded RTSP connect (`connect_within`, "the source did not start the stream"), `bccd794` decode log lines, `97e5b74` IDR request on queue overflow, `953735e` decode queue 6.
  - Result (run 6e): TB336FU 1080p shown at ~30 fps (1995 pictures/71 s), decode 10-12 ms (FilmCraft, wgpu), loss 0.12 %, clean TEARDOWN. No laptop screenshot (see open questions).

- Step 11 (start screen D21 + guided Miracast flow, 2026-11-21):
  - `124c903` decision D21 (Presenter large, Viewer small, "View your Android device with Miracast"; rejected: Miracast inside the viewer dialog).
  - `d2d761a` typed `SinkEvent` (sink driver) and `FlowEvent` (NM join) instead of log strings.
  - `595f8fe` start screen per D21; `Screen::Miracast` driven by the pure state machine `miracast_flow.rs` (instruction, waiting with Cancel, video, result pages for no device / group failed / source never started / no RTP with port+zone+firewall-cmd / video stopped); cancellable join; NM profile removed on Cancel, Back, Esc and window close (Drop waits up to 5 s).
  - **Only unit-tested, not hardware-tested:** the start-screen flow (D21) and the guided Miracast screen ran in no GUI and with no tablet. Manager decision 2026-11-21: skip the GUI/tablet check (no GUI start on the owner's desktop; the KWin journal cannot be read from the container, and mounting the host journal is the owner's call).

- Step 12 (release preparation, 2026-11-21):
  - `50431e2` clean-room audit fix: RTSP test values of unclear origin (`6B8B4567`, `192.168.173.x`, `server_port=15550`) replaced with our run-3 trace values; [verify] comments in code updated.
  - `6777d3d` decision D22 (naming: button "View your Android device (Miracast-compatible)", Miracast only descriptively, trademark sentence in README, AppStream metainfo and miracast.md; rejected "Wi-Fi Display only"); feature `miracast` default with effect on Linux only (Linux target deps + `cfg(all(feature, target_os = "linux"))`); miracast.md §15 audit report (nothing derived) and [verify] resolution table.
  - Built here with `--no-default-features --features gui,filmcraft,miracast` (clippy, 11 app tests, release). Default build: manager's box. macOS/Windows with the new default: CI (no cross toolchain here).

- Step 13 (CI fix + craft-libs encoder, 2026-11-21):
  - `0fe552d` `spawn_direct` gated `#[cfg(all(test, feature = "filmcraft"))]` (its only caller is the FilmCraft end-to-end test); the default build's test target no longer has dead code. No `allow`.
  - `8a6e5b7` encoder = `craft-video` (craft-libs `068e079c`, features `h264-encode` + `threads`, default-features off); decoder pin in mirrorwire-film bumped from `09dcbc22` to the same rev; `fuzz/Cargo.lock` updated; `deny.toml` drops `storytold/filmcraft`. One craft-video/craft-media in Cargo.lock. Feature `filmcraft`, `--encoder filmcraft` and the reported name `filmcraft-h264enc` unchanged.
  - Measured old vs new (container, 4 vCPU): encoder 1080p ~63 ms both (122 ms without `threads`), same bitrate; decoder on `stream-run4d.h264` byte-identical I420, same timings. Numbers in filmcraft.md §3.1a.
  - `d86f0db` D23 + filmcraft.md + architecture.md.

- Step 17 (Flatpak hostname1 permission, 2026-11-22, branch `flatpak-nm`, local commits only):
  - `383edeb` manifest finish-arg `--system-talk-name=org.freedesktop.hostname1` with comment; D2, D25 (rejected alternative is now "Flatpak with NM fallback only") + decision-log entry, AGENTS.md §1.2, README permissions, miracast.md §1/§16, packaging/flatpak/README.md (table row, Flathub justification: one name, read-only `PrettyHostname`/`StaticHostname`, no method calls, removal via override).
  - `c7ac0a2` comment-only: the NM fallback is for hosts without hostname1 (no systemd-hostnamed, or the name overridden away), not "the Flatpak". No behaviour change; the code already tries hostname1 first.

- Step 16 (name shown on Android, 2026-11-22, branch `flatpak-nm`, local commits only):
  - `07b676d` `miracast_nm::name::SinkName` (Named/Empty/Unknown) + pure selection (hostname1 Pretty -> Static -> Empty; NM `Settings.Hostname` fallback, empty -> Unknown; trimmed, no control chars, max 64 chars), `zbus_backend::sink_name()` (3 s bound, any error -> Unknown). 5 unit tests. Live read here against the host bus: `Empty` (hostname1 ""/"", NM Hostname "").
  - `67a8567` GUI: "Name shown on Android: <name>" on every Miracast page except the video; "none ℹ" when empty, click/tap opens the hostnamectl explanation (`miracast_flow::NAME_HELP`); hidden when Unknown. Read on its own tokio task at flow start. Tap instructions use the estimate instead of MirrorWire's viewer name. D25, miracast.md §16, README, Flathub call list (`Settings.Hostname` read).

- Step 14 (Flatpak NetworkManager permission, 2026-11-22, branch `flatpak-nm`, local commits only):
  - `fd744d7` miracast-nm `map_err`: NM's `org.freedesktop.NetworkManager.PermissionDenied` (Polkit refusal) was mapped to `Backend`, so it was retried and shown as "connection did not form"; now `PermissionDenied`. The test accepted `Backend`; now strict (failed before the fix). `ServiceUnknown` text: "not running or cannot be reached".
  - `e651d3f` app: `Issue::NotAllowed` ("Receiving from Android was not allowed", Polkit text); `Issue::NotAvailable` adds, inside a Flatpak (`/.flatpak-info`), the `flatpak override --user --system-talk-name=org.freedesktop.NetworkManager io.github.windwerfer.MirrorWire` hint for users who removed the permission.
  - `9ce3598` manifest finish-arg with comment; D24 (table + log, rejected alternative), D2, AGENTS.md §1.2, README permissions, miracast.md §1 and the §15 Flatpak note; new `packaging/flatpak/README.md` (permission table, Flathub review justification with the exact D-Bus calls, GNOME Network Displays comparison, how to remove the permission).

## Decisions
- D25: name shown on Android = hostname1 PrettyHostname, then StaticHostname (estimate; NM has no documented P2P name property, wpa_supplicant DeviceName is root-only). Step 17: the Flatpak has `--system-talk-name=org.freedesktop.hostname1` (owner 2026-11-22). Fallback when hostname1 is unreachable: NM `Settings.Hostname`; empty -> line hidden.
- D24: Flatpak gets `--system-talk-name=org.freedesktop.NetworkManager` (owner 2026-11-22). Rejected: no permission + `flatpak override`/Flatseal hint.
- The "not available" page stays (NM missing, no P2P device, permission removed by override); the override hint is shown only inside the Flatpak. Polkit refusals are not retried and get their own page.
- D23: FilmCraft encoder from craft-libs at the decoder's rev; `threads` feature on (slice parallelism; the decoder stays single-threaded via `with_threads(1)`). Rejected: keep depending on the full FilmCraft repo.
- Encoder display name stays `filmcraft-h264enc` (it is sent in `StreamInfo`; renaming is not needed for the switch).
- D22: Miracast only as a descriptive term; feature `miracast` default on Linux only.
- Miracast reuses `ViewerHandle` and the app's decoders; it is behind the non-default app feature `miracast`. The flags exist in every build and fail with a clear message without it.
- Decode queue 6 for Miracast (like Android), with drop-to-keyframe plus an IDR request on overflow.
- Do not try to steer the P2P channel; log the STA channel and rely on the source picking its STA channel (same AP, same band as the laptop works).
- Retry only `P2pError::Backend` (formation) failures; permission/availability/peer errors stop at once.
- ipv4.method `auto` (tablet is GO and serves DHCP), `persist: volatile`; no `bind-activation` yet (untried on hardware).
- `NmApi` is fine-grained (activate/state/ip4/deactivate/sleep) so the wait logic is mock-tested; the provider polls instead of subscribing to signals.
- `tokio` became a direct Linux dependency of miracast-nm (already in the tree via zbus; sleep only).
- Diagnostic suggests a command, never changes the firewall itself.
- See also miracast.md section 7 (sink first, sans-IO parsers, no HDCP/UIBC, audio demuxed not played, rejected: root helpers, direct wpa_supplicant D-Bus, GStreamer/FFmpeg demux, GPL code).

## Open questions
- Step 17: hostname1 permission added; not built with flatpak-builder, not run as an installed Flatpak, so whether the bus proxy then forwards hostname1's properties (expected: yes) is unverified. Check `flatpak info --show-permissions io.github.windwerfer.MirrorWire` and that the Miracast screen shows "none ℹ" on the T480s Flatpak.
- Step 16: Not verified: that the P2P name follows the pretty rather than the static hostname, that toggling Wi-Fi applies a new name, the GUI look (no GUI run), the bus-proxy answer for hostname1 inside a Flatpak (any error or 3 s timeout -> NM fallback).
- Step 13: default-feature CI (`cargo xtask ci` with portal + ffmpeg), the x264-vs-FilmCraft bench, the FilmCraft loopback and `cargo ndk -t arm64-v8a clippy -p mirrorwire-android` were not run here (no libpipewire/FFmpeg/NDK). Decoder output was compared old rev vs new rev only (no `ffmpeg` CLI for the FFmpeg bit-exact check).
- Cargo.lock now lists rav1e and ~30 crates via craft-video's weak `rav1e?/threading` in `threads`; they are not compiled (`cargo tree -i rav1e`: none) but show in cargo-deny's duplicate warnings. Upstream could split `threads` per codec.
- Does craft-libs `068e079c` fix the fuzz finding (h264 `decoder.rs:532` subtract overflow)? If so, drop `continue-on-error` in `fuzz.yml`; not checked.
- Step 14: GNOME Network Displays comparison is NOT confirmed. Flathub's summary API (read 2026-11-22, v0.99.0) lists sockets wayland/fallback-x11/pulseaudio, shared ipc/network, devices dri and no system-bus name. Check `flatpak remote-info --show-metadata flathub org.gnome.NetworkDisplays` (metadata only, D20) before using it in a Flathub submission; otherwise drop the comparison (packaging/flatpak/README.md says so).
- Step 14: not built with flatpak-builder (not installed here), not run as an installed Flatpak; the Flatpak NM path, the exact error a bus proxy returns for a disallowed name (assumed `ServiceUnknown`), and the Polkit page are untested on hardware. Commit `e651d3f` mentions D24, which `9ce3598` adds right after.
- Remaining [verify] items (miracast.md §15): WFD level bits, other CEA bits, LPCM sample-rate bit, `bind-activation`. None affects the working path.
- KWin check: `journalctl --user -b` is not readable from the research container (no journal files, `/var/log/journal` empty). Either run the GUI checks where the journal is readable or mount the journal read-only; until then no GUI starts on the owner's desktop.
- The previous viewer runs (6a-6e, wgpu window) happened before the KWin rule; whether they caused the KWin DRM loss is unknown from here.
- Laptop screenshot of the viewer: the xdg Screenshot portal request at 13:17:58 had an unknown outcome (no Response, request gone); not retried, so no dialog piles up on the owner's screen. Ask the owner for a photo/screenshot, or allow a portal screenshot.
- Default app build (`portal` + `ffmpeg`) cannot link in this container (no libpipewire, no FFmpeg dev libs). Miracast was built and tested with `--no-default-features --features gui,filmcraft,miracast`. The default build with `--features miracast` (VAAPI decode) is untested: run it on the manager's box or after installing the dev packages.
- `presenter::tests::test_source_streams_to_an_in_process_viewer` fails in the FFmpeg-less build (needs x264); unrelated to Miracast.
- Confirm against the official nm-settings-dbus / wpa_supplicant D-Bus pages that `wifi-p2p` has no GO intent or frequency property (the box could not fetch them).
- When the laptop STA is on 5 GHz (run 4: 5300 MHz DFS) and the tablet on 2.4 GHz, a link loss is likely; the GUI could warn when the STA is on 5 GHz. Not measured beyond run 4's one loss.
- `--reconnect` has unit/e2e coverage only; no link loss happened in run 5 to exercise it.
- Why group formation fails now and then (GROUP-FORMATION-FAILURE / GO-NEG status 10). Without the NM and wpa_supplicant journals (not readable from the container) the cause on our side is unknown.
- Mid-stream link loss in attempt c: does the 5 GHz station (5300 MHz) vs the 2.4 GHz group (2462 MHz) on the single 8265 radio cause it? Test idea: the same run with the laptop's Wi-Fi on 2.4 GHz ch 11 (needs the owner, and it touches wlp58s0).
- NoRtp has still not been seen on hardware with the new code (the port was open).
- The new zbus calls (`AddAndActivateConnection2`, `State`, `Ip4Config`, `IpInterface`) are untested against a real NM (none in the container); the flow was proven with busctl only.
- Should `bind-activation: dbus-client` be used so a crashed sink leaves no active group? Needs one hardware try.
- Where the GUI shows `NoRtp` and whether it looks the zone up through the Flatpak's existing FirewallD1 permission (no new permission needed for a read).
- Tapping "Connect" on the tablet's prompt forms a group but no RTSP; only tapping us in the Cast list works. UX text for that is in the `P2pError` messages; a GUI hint is still open.
- The research box is a VM without Wi-Fi/iw/NM: nothing about the Intel 8265 was measured; the doc lists what to run on the T480s (section 9). Facts marked [verify] in miracast.md (WFD IE bits, wfd_* masks, NM incoming GO negotiation = risk R1) come from memory of the specs and are unconfirmed.

## Verify
Step 17: `git log --oneline 28705c7..flatpak-nm` (2 commits + this HANDOFF); manifest parse with the throwaway serde_yaml binary `/tmp/yamlcheck` (8 finish-args, last `--system-talk-name=org.freedesktop.hostname1`); `cargo fmt --all --check`; `cargo clippy -p miracast-nm -p mirrorwire-app --all-targets --no-default-features --features gui,filmcraft,miracast -- -D warnings`; `cargo test -p miracast-nm` (31).

Step 16: `git log --oneline 3d76ce4..flatpak-nm` (3 commits); `cargo fmt --all --check`; `cargo clippy --workspace --all-targets --no-default-features --features gui,filmcraft,miracast -- -D warnings` and the same with `gui,filmcraft`; `cargo test -p miracast-nm -p miracast-loopback` (31 + 14); `cargo test -p mirrorwire-app --no-default-features --features gui,filmcraft,miracast` (34 pass, the x264 test fails without FFmpeg). On the T480s: open the Miracast screen, expect "Name shown on Android: none ℹ"; run the two hostnamectl commands, toggle Wi-Fi, check the tablet's Cast list.

Step 14: `git log --oneline a5344da..flatpak-nm` (3 commits); `cargo fmt --all --check`; `cargo test -p miracast-nm -p miracast-loopback`; `cargo test -p mirrorwire-app --no-default-features --features gui,filmcraft,miracast` (32 pass, the x264 test fails without FFmpeg); `cargo clippy -p mirrorwire-app -p miracast-nm --all-targets --no-default-features --features gui,filmcraft,miracast -- -D warnings`; manifest parse: no PyYAML here, parsed with a throwaway serde_yaml binary in /tmp (finish-args list printed, 7 entries). On the box: `flatpak-builder --user --install build-dir packaging/flatpak/io.github.windwerfer.MirrorWire.yml`, `flatpak info --show-permissions io.github.windwerfer.MirrorWire`.

Step 13: `cargo fmt --all --check`; `cargo clippy --workspace --all-targets --no-default-features --features gui,filmcraft,miracast -- -D warnings` and the same with `gui,filmcraft`; `cargo test --no-fail-fast -p mirrorwire-media -p mirrorwire-film -p mirrorwire-app --no-default-features --features mirrorwire-app/gui,mirrorwire-app/filmcraft,mirrorwire-app/miracast` (22 + 2 + 31/32, the one failure needs x264); `cargo deny --all-features --locked check advisories bans licenses sources`; `(cd fuzz && cargo check --locked)`; `grep -A2 'name = "craft-' Cargo.lock` (one rev). On the box: `cargo xtask ci`.

`git log --oneline 767ed3c..miracast-spike`; `cargo test -p miracast-nm -p miracast-rtsp -p miracast-rtp -p miracast-loopback` (94 tests); `cargo clippy -p miracast-nm -p miracast-rtsp -p miracast-rtp -p miracast-loopback --all-targets -- -D warnings`; `cargo fmt --all --check`; `cargo xtask layers`. `cargo xtask ci` cannot build in the research container (no libpipewire-0.3 dev package), unrelated to the spike.

## Next step
Step 17: manager reviews and merges `flatpak-nm` (steps 14, 16, 17); on the T480s install the CI Flatpak bundle, check the permissions list and the "Name shown on Android: none ℹ" line.

Step 16: manager reviews `flatpak-nm` (steps 14 + 16) and merges; on the T480s check the name line and that the tablet shows the name after `hostnamectl` + Wi-Fi toggle; owner decides on the hostname1 Flatpak permission.

Step 14: manager reviews and merges `flatpak-nm` into main (and pushes; CI + Build must go green, the Flatpak job builds the new manifest). Then on the T480s: install the CI Flatpak bundle and try the Miracast-compatible button with the TB336FU; also `flatpak override --user --no-system-talk-name=org.freedesktop.NetworkManager io.github.windwerfer.MirrorWire` once to see the "not available" page with the hint, then restore. Confirm the GNOME Network Displays metadata before a Flathub submission.

Earlier (step 13): Step 13 done: run the full `cargo xtask ci` (default features), `cargo audit`/`cargo deny`, the FilmCraft loopback (`MIRRORWIRE_FEATURES=filmcraft scripts/loopback-test.sh 10 --encoder filmcraft`) and the Android clippy on the build box; if green, merge miracast-spike into main.

Earlier: Owner/manager: try the start screen button on the T480s (with the KWin journal check), then build the default app with `--features miracast` on a box with libpipewire and FFmpeg (VAAPI decode) and repeat run 6; then a home-screen entry ("Receive Miracast") and a cancellable NM join. Optionally `bind-activation: dbus-client` so a killed sink leaves no group (Ctrl-C today leaves the volatile profile active until NM drops it).

## HARDWARE RUN 5 (done 2026-11-21; driver script `~/tmp/mc-hw/driver-run5.sh`)
`./target/release/miracast-sink-demo --nm --out stream.h264 --secs 300 2> sink-trace.log`; on the tablet: Settings -> Cast -> Enable wireless display, cancel the "wants to connect" prompt, tap "Wireless display". Watch for `attempt n/3 failed` (retry) and `diagnostic:` lines.

## HARDWARE RUN 4 (done 2026-11-21, kept for reference)
`./target/release/miracast-sink-demo --source 192.168.49.1:7236 --out stream.h264 --secs 300 2> sink-trace.log` replaces socat (start it after the group is up; it retries). Expect `RTSP connected to 192.168.49.1:7236`, `stream SPS: 1920x1080 profile 100 level 4.0`, and with the firewall closed a `diagnostic: ... zone "public" ...` line after 5 s. The original checklist below is kept for reference; its step 1 (sink listens on 7236) is superseded.

## HARDWARE TEST CHECKLIST (T480s Fedora KDE Wayland + Lenovo TB336FU)
Facts marked [verify] come from memory of the specs/NM docs, not measured. Nothing here needs root for the spike code itself; stop at any step that asks for sudo and report it instead (the spike must stay rootless).

**0. Build and baseline (T480s)**
```sh
git fetch origin && git checkout miracast-spike
cargo build --release -p miracast-loopback --bin miracast-sink-demo
cargo test -p miracast-loopback            # 3 tests must pass on real hardware too
rpm -q NetworkManager NetworkManager-wifi wpa_supplicant linux-firmware
nmcli device                               # expect wlp.. and p2p-dev-wlp.. (type wifi-p2p)
iw list | grep -A12 'Supported interface modes'    # want P2P-client, P2P-GO, P2P-device
iw list | grep -B2 -A10 'valid interface combinations'
nmcli -f all device show | grep -i -B2 -A8 p2p
pkaction --verbose --action-id org.freedesktop.NetworkManager.network-control
```
Save all output to `miracast-baseline.txt`. If `nmcli device` shows no `p2p-dev-*`, stop: report it (Intel 8265 + this NM/wpa_supplicant do not expose P2P).

**1. Sink first, listening before any group exists (T480s)**
```sh
sudo -n true 2>/dev/null; echo "(no sudo used below)"
./target/release/miracast-sink-demo --out stream.h264 --secs 900 2> sink-trace.log
```
(firewall: the Wi-Fi Direct group interface may be in a restricted zone; see miracast.md section 1. Report what `firewall-cmd --get-active-zones` says; if the tablet cannot connect, ask the owner before opening 7236/tcp and 19000/udp, e.g. `firewall-cmd --add-port=7236/tcp --add-port=19000/udp` for the runtime config only.)

**2. Discovery and group (T480s, second terminal) [verify all]**
```sh
P2P=$(nmcli -t -f DEVICE,TYPE device | awk -F: '$2=="wifi-p2p"{print $1}')
busctl --system introspect org.freedesktop.NetworkManager $(nmcli -g GENERAL.UDI device show $P2P) | grep -i -E 'StartFind|Peers|WfdIEs'
nmcli connection add type wifi-p2p con-name mw-wfd ifname "$P2P" peer <TABLET_P2P_MAC> wifi-p2p.wfd-ies 00000600111c440032 wifi-p2p.wps-method 4
```
The WFD IE bytes `00 00 06 0011 1c44 0032` = device-info subelement: primary sink, session available, RTSP port 7236, 50 Mbit/s (`miracast_nm::wfd::sink_device_info`). Open question R2: whether NM publishes this IE to the tablet at all; R1: whether NM accepts the tablet's incoming connection request.

**3. On the TB336FU**
1. Settings -> Connected devices (or Connection preferences) -> Cast -> menu -> **Enable wireless display**.
2. Open Cast; wait up to 30 s. **Note: is a device named "MirrorWire" (or the laptop's name) listed? (yes/no)**
3. If listed: tap it. Accept any "connect" prompt. Note: does the tablet show "Connecting" then fail, or connect?
4. If not listed: on the T480s run `busctl --system call ... StartFind` / `nmcli device show $P2P` and check whether the *tablet* appears as a peer (`busctl ... get-property ... Peers`); then `nmcli connection up mw-wfd` to connect from the laptop side. Note which of the two directions worked.
5. After connecting, note: who became GO (`ip addr show` on the T480s: `192.168.49.1` on our side = we are GO, else the tablet is), the tablet's IP, whether video appears in `sink-trace.log`.
6. Rotate the tablet once (resolution change mid-stream) and note the log lines; lock the screen, then wake it (loss/IDR); finally tap Disconnect (teardown M8).

**4. Check the dump (T480s)**
```sh
ffplay -f h264 stream.h264        # or: mpv --demuxer-lavf-format=h264 stream.h264
ffprobe -hide_banner stream.h264
```

**5. Logs to send back**
`miracast-baseline.txt`, `sink-trace.log` (full RTSP trace both directions with timestamps plus per-second media stats; the `done: SinkReport {..}` line at the end shows close reason, loss and IDR counts), `journalctl -u NetworkManager --since "-30 min" > nm.log`, `journalctl -u wpa_supplicant --since "-30 min" > wpa.log`, `nmcli -f all device show $P2P`, `ip addr` and `ip route` during the session, and answers to the **bold** questions above (listed? which direction? who was GO? video yes/no, audio demanded?). Optional if allowed: `tcpdump -i <p2p-iface> -w mw.pcap 'tcp port 7236 or udp port 19000'` (needs root normally: skip if so and say so).

**What a good result looks like:** `sink-trace.log` shows M1..M7 in order, "start media on UDP 19000", rising video AU counts, no `close:` until you disconnect. Also useful: which `wfd_video_formats`/`wfd_audio_codecs` the tablet's M4 picked, and whether it rejected `wfd_audio_codecs: none` (then an LPCM offer is needed: miracast.md section 5).

## Commit
Step 16: 07b676d, 67a8567, plus this HANDOFF commit. Not pushed.
d86f0db Document the craft-libs encoder (D23) and one craft-libs pin
8a6e5b7 Take the FilmCraft encoder from craft-libs (craft-video h264-encode)
0fe552d Gate spawn_direct on its only user (test + filmcraft)
Step 7: 468a665, 1e2651f, ecad42c, d3d2899, 1d3873a, 91987c4. Step 8: e4309af, 651f0f6, 658cb8c, df3ab68. Step 9: 1942535, 6ad2e1c, e66a624, 96c0e13, 8ab16e0, 03258f8, 877a2b9. Step 10: a4f80f8, 38f6d43, 90929b0, e5f3426, 194d4fc, bccd794, 97e5b74, 953735e, 7050dd9. Step 11: 124c903, d2d761a, 595f8fe, 81082c2. Step 12: 50431e2, 6777d3d, plus this docs commit (see `git log -1`). Not pushed (container has no GitHub access).
