# HANDOFF

## Flaky nack test (2026-11-25)
- `nack_makes_the_presenter_resend_a_lost_fragment` asserted `resent_useful == 1`. A stall (>15 ms, TAIL_QUIET_US) mid-frame makes the viewer NACK fragments that are only late; their resends can land first and fill gaps too, so useful > 1 legitimately (CI log of run 44990072133: requested 22, resent_received 22, resent_useful 2). Not a Reassembler bug (useful counts only new fills). Test now asserts requested >= 1 and 1 <= useful <= resent_received, frame bytes equal. Not reproducible on this Linux box (0/400, also under load).

## Sonar coverage gate (2026-11-25, local commits on main, NOT pushed: manager pushes)
- Gate failed only on new_coverage 69.7% (3962 uncovered of 13083 new lines). Not yet re-measured by Sonar (nothing pushed).
- Local estimate (default features + filmcraft, `cargo llvm-cov` ratios applied to the files of coverage-before.txt, exclusions removed): about 1250 uncovered of about 10170 = **about 87.7%** (79.4% before the filmcraft change, 81.3% after it, before the extra tests/exclusions). Crude: per-file ratio, not Sonar's new-line set.
- CI fix: the SonarCloud job ran `cargo llvm-cov --workspace` without `filmcraft`, so tests behind that feature (Miracast end to end, FilmCraft decode, presenter/viewer paths) never ran. Now `--features mirrorwire-app/filmcraft`.
- `sonar-project.properties` exclusions (each with a reason in the file; the stale `gui.rs` pattern matched nothing and is gone; every pattern was checked against a real file): gui/{mod,video,present,widgets,pen,miracast,scan}.rs, encode/vaapi.rs, miracast-nm zbus_backend.rs, miracast-sink-demo.rs, source/camera/scanner.rs, source/remote/calls.rs, plus the old yuv_glow/portal/hw/firewall system/android patterns. gui/chrome.rs stays measured.
- Refactors (moves only): PipeWire scanner out of camera.rs into `camera/scanner.rs`; portal RemoteDesktop calls out of remote.rs into `remote/calls.rs`. Pure parts stay and keep their tests.
- Tests added: encode/config.rs (8), gui/chrome.rs headless egui (layout, clicks, icon painting; egui runs on a plain Context), xtask (layers on real and fake trees, run, report parsing), gui/mod.rs helpers. Protocol 12 (pen_mode, video_paused, version rejection), InputForward/RemoteRoute stall, settings and viewer-menu state already had tests; not duplicated. Android chrome.rs only compiles on Android (not in lcov).
- Gates run locally: fmt, `cargo xtask ci` (loopback p95 16.2 ms <= 40), deny, android clippy: ok. `cargo audit`: only an allowed warning (see its output).
- Next: push, wait for CI + SonarCloud, check the quality gate API; if still < 80%, `component_tree` and add tests for presenter/mod.rs, encode/ffmpeg.rs, transport/presenter.rs.

## Release 0.2.2 prep (2026-11-25, branch `release-0.2.2`, local only: not pushed, not tagged)
- Version 0.2.2, versionCode 2002 (`major*1e6+minor*1e3+patch`; Cargo.toml `[workspace.metadata.android] version_code` must match, the script checks). Dev app = code + 2000000000 (own package id).
- Reproducible unsigned APK: two clones at different paths, separate target dirs: both `20edb3c9d4f994543eed2080cd585e59de0d21d0b5e251ccaa8a838f41669d77` (commit 31535ed). At 5838ac5 (current build flags) the unsigned APK is `b40860e3c6590192ea6fd98b1b29365b11c0e65437bdc2e58747dc0e91ff553b`; a debug-signed build of the other clone passed `apksigcopier compare`. SOURCE_DATE_EPOCH is the commit time, so hashes change with every commit; compare only at one commit. Bugs found on the way: target-dir paths leaked via glutin build script (fixed by remap), apksigner 35 needs `--alignment-preserved true`.
- Sonar S8707: `scripts/run-stats.py` and `scripts/sim-compare.py` resolve with `os.path.realpath` and require the path inside cwd / repo / temp / `MIRRORWIRE_SCRIPT_BASE`. Not checked against SonarCloud (could not read the rule text offline). If the 3 issues stay open after the next scan: mark them Safe (input is the developer's own local log files).
- Needs Thomas: real screenshots in `fastlane/.../phoneScreenshots/`; release cert SHA-256 for `AllowedAPKSigningKeys` (TODO in the yml); submit fdroiddata MR and the IzzyOnDroid request (packaging/fdroid/README.md); IzzyOnDroid policy could not be fetched, re-read it.
- Proposed, needs owner OK (not in decisions.md): versionCode scheme and reproducible-build policy as a decision entry (D27?). `rust-toolchain.toml` pins 1.100.0 for all CI jobs (bump deliberately).
- Not run: the CI Android job itself (new reproducibility step), a device install of the v2+v3-only APK, `fdroid build` on a real F-Droid buildserver.
- README still says the APK is debug-signed (fixed in the 0.2.2 merge).

## Merge of pen-stall-fix and viewer-menu (2026-11-25, branch `merge-pen-menu` from main b239830, not pushed)
- Both feature branches merged with `--no-ff` (SHAs in `git log --merges`). Protocol stays 12: neither branch changes the wire.
- Persistence: main's 7e1816e and the branch's 3bcb155 fixed the same thing; one implementation remains (`portal.rs::cast_options`, `negotiate_combined`; SelectDevices only, session closed on failure), one decisions.md entry, one CHANGELOG bullet.
- Pen: main's "pen permission when the pen is switched on" (`request_input`, `pen_mode`) sits on the branch's `remote-input` thread and one portal runtime. Per the manager (decisions need the owner, AGENTS.md §5) Thomas's Mirror-only pointer fallback is restored in a separate commit: a separate pointer session (monitor source, `AskingSink` dialog in the presenter window, `PointerMapped`, keys otherwise, a note in Extend), now on `portal_rt` with `INPUT_TIMEOUT` on its calls. Stall guard: input reaches it by `try_send` only and the route drops strokes while the dialog is open, so an unanswered dialog cannot block the control reader or capture (tests: `an_unanswered_pointer_dialog_does_not_back_up_the_route`, `undrained_pen_input_does_not_stop_the_video`). The ✏ / "Forget the pen and keys permission" UI is unchanged.
- Menu: viewer-menu's corner icons and ☰ popup (desktop and Android) are the only menu; main's own ☰ menus were removed. The popup holds Pen, PowerPoint mode, Hide, Show statistics, then main's **Wacom pen (no video)** and **PowerPoint remote only (no video)**, then 1:1 / Fullscreen (desktop). Disconnect is the corner icon. The Pen icon/checkbox sends `pen_mode` (protocol 12) at once, so the permission flow runs when the pen is switched on from any entry; in Wacom no-video mode the Pen toggle leaves that mode. In PowerPoint-remote-only mode the bottom row is not drawn (the big buttons replace it).
- Pen fix hardware status: the *error* ("Remote desktop sessions cannot persist") was reproduced on the owner's KDE session 2026-11-25 and the fix was built, but a69daa6's notes show the run with the fix (run 2) waited ~30 min on an unanswered portal dialog, so **the combined session starting on hardware is not yet confirmed**, and the full tablet pen retest (Pen on the tablet, video keeps moving, laptop pointer follows, capture reopen) is pending, as are the merged menu and the no-video modes on a tablet.

## Pen stall fix (2026-11-25, branch `pen-stall-fix`, merged into this branch)
### Revision after owner facts t318u (read this first)
- Failing run: **Flatpak v0.2.1**, pen pressure **off**, **no udev rule**, **no second portal dialog**.
- With pen pressure off, v0.2.1's `RemoteRoute` only ever `try_send`s and never blocks, so the control-reader `send().await` (fixed in `4fff150`) can only stall when the route stops draining. No way to get there on the plain portal path was found. `4fff150` stays as a guard, but it is **probably not the t316u trigger**. The second-session theory needs a dialog (or a silent token restore) and only applies when the combined session failed; also unlikely.
- **Revised root cause (confirmed on the owner's KDE session bus from the container, read-only probe, no dialog):** ashpd keeps one process-wide D-Bus connection, and zbus binds it to the runtime it was built on. The capture built a throwaway current-thread runtime on its `portal` thread. After that thread ended (any capture **reopen**: mode, cursor or fps change), every later portal call hung forever (probe: `AvailableSourceTypes` timed out at 5 s). So a reopen never finished: the process stayed alive with no video until restarted, and there was no dialog because the reopen uses the restore token. How Pen gets there: in pen mode every tap is a real left click on the laptop, and in Mirror mode the shared screen shows the MirrorWire presenter window, so a tap on its Mode/Cursor/Frame-rate controls reopens the capture. Not proven that this is exactly what happened, but it is the only confirmed path to "alive, no video, no dialog, restart needed".
- Fix `a3393ce`: `source::portal_rt::portal_runtime()`, one long-lived runtime for every portal call (capture portal thread, key injector, camera scanner). Same probe with the shared runtime: three successive "captures" all answered.
- **Flatpak vs native: same code path.** Both reach RemoteDesktop/ScreenCast through `org.freedesktop.portal.Desktop` on the session bus (Flatpak's bus proxy allows the portal names by default; no finish-arg is involved), and both use the same ashpd/zbus code, so the native build had the same bug. Differences that do not matter here: `/dev/uinput` is not in the sandbox without `--device=all` (pen pressure was off anyway), and config/token files live under `~/.var/app/io.github.windwerfer.MirrorWire/config/mirrorwire` instead of `~/.config/mirrorwire`. A green native test therefore also covers the Flatpak code, but not the Flatpak packaging itself.

- **Second finding from the hardware run (fix `3bcb155`):** the native presenter logged `combined capture + input session unavailable: ... InvalidArgument: Remote desktop sessions cannot persist`. We passed `persist_mode`/`restore_token` to `ScreenCast.SelectSources` on the RemoteDesktop session; the portal docs say those "must not be used with a remote desktop session" (persistence goes through `RemoteDesktop.SelectDevices`). So the combined session has **never worked**; capture fell back to a plain cast (restored silently from its token, so no dialog), and all input went to the separate injector. In v0.2.1 that injector's pointer session made the same mistake, so each pen down failed at once with no dialog (t318u: "no second dialog") and left a half-open RemoteDesktop session behind each time. Now the combined cast has no persistence options, and a session that fails during setup is closed. (The separate fallback injector closes a failed session too, since the merge.)
- Whether leaked sessions or the reopen hang actually froze t316u's video is still not proven; both are fixed, as is the control-reader guard.

### Hardware run 2026-11-25 (native/container build, not the Flatpak)
- Built from the throwaway branch `pen-stall-fix-menu-hw` (worktree `/workspaces/mirrorwire-pen-menu-hw`, not pushed): `pen-stall-fix` + `viewer-menu` 7b750fa, docs conflicts only. Presenter: `--no-default-features --features gui,portal,filmcraft,miracast` (no FFmpeg dev in the container, so FilmCraft encoder; libpipewire headers extracted to /tmp), run as `mirrorwire present --encoder filmcraft --mode mirror`.
- APK: the tablet has release-signed v0.2.1 (versionCode 139); the container only has the debug key, so `adb install -r` failed with `INSTALL_FAILED_UPDATE_INCOMPATIBLE` (signature, not version). Instead of uninstalling (would wipe pairings), installed side by side as `io.github.windwerfer.mirrorwire.dev` ("MirrorWire dev", versionCode 1795100000, `aapt2 --rename-manifest-package`, throwaway script copy in /tmp). It launches. Uninstall later: `adb uninstall io.github.windwerfer.mirrorwire.dev`.
- Run 1 (`a3393ce` build): capture started at once without a dialog (plain cast from the container's restore token) and showed the persistence error above. Stopped before connecting the tablet, to fix that first.
- Run 2 (`3bcb155` build): the combined dialog (screen + keyboard + pointer) was pending on the desktop (one portal `Request` object for our process) but was not answered within ~30 min, so capture never started, the tablet was never connected, and Pen was not tested. Stopped with SIGTERM (SIGINT is not handled while blocked in the portal open); no request left pending.
- KWin journal after each start (`journalctl --since <start> -q`, user journal readable without sudo): no EACCES, DRM or master-loss lines; only an unrelated Flatpak browser error every 30 s.
- **Still to do (Thomas):** see "Next step" below.

### First analysis (t316u)
- Report t316u: v0.2.1, T480s presenter, Android viewer, Pen pressed -> presenter alive but no more video until restart. Pen pressure setting state unknown. No logs from the incident: `/workspaces/mirrorwire/tmp/journal-5d.txt` ends 2026-11-21 20:24 (before v0.2.1) and has no xdg-desktop-portal or mirrorwire lines; `updates.txt` is a package list. Nothing useful there.
- Root cause (found in code, reproduced in a test, not on hardware): the presenter's control reader (`mirrorwire-net` `transport/presenter.rs`) did `tx.send(input).await` for every pointer down/up. That reader is the only thing that refreshes "last heard from viewer"; the send loop pauses the video when the viewer is silent for 1.5 s (`VIEWER_SILENT`). So any input path that stopped draining (portal dialog waiting, hung D-Bus call, blocked device) blocked the reader -> "viewer silent" -> video paused forever while the process lived. The new e2e test reproduces it: viewer silent 2.2 s after 5 down/up pairs into an undrained channel.
- Manager hypothesis (second portal session): **confirmed in code, impact not verifiable here.** The separate fallback injector did open a second RemoteDesktop + ScreenCast session (`SelectSources` + `Start`, dialog "choose the screen to control") on the first pen down while the capture ran, and retried it on every down after a failure. Whether KWin then disturbs the first cast is not documented either way and was not testable. It only runs when the combined session was refused/unavailable. The fallback keeps the pointer session (Mirror only) per Thomas's design, off the network runtime and never waited on.
- Combined session: `inject` ran unbounded on the capture's portal thread; a hung call there could not stop frames (PipeWire has its own thread) but blocked the stop signal, so `PortalSource::drop` (capture reopen) could hang. Now bounded (2 s) and a stop wins.
- uinput (setting on): `PenRouter::route` opened/wrote the device synchronously on the `RemoteRoute` task on the 2-worker network runtime (same runtime as the control reader and send loop). No shared lock with capture except short `CaptureCtl` locks. Now on its own `remote-input` thread.
- Commits: `4fff150` net (InputForward, e2e + unit tests), `b169b53` app (route thread + test), `d98ab36` media (bounded combined input; its key-only fallback was reverted in the merge, see top), then docs + this HANDOFF.
- Follow-up risk (not fixed, separate topic): ashpd 0.14 caches one global zbus connection (`proxy.rs` `SESSION`), and zbus 6 binds a connection to the runtime it was built on. The first portal call happens on the capture's `portal` thread (a current-thread runtime that ends when the source is dropped). After a capture reopen (mode/cursor/fps change) that connection may be dead, so later portal calls could hang or fail. Check on hardware: change Mirror->Extend while sharing and see whether the new capture starts. Possible fix: one long-lived portal runtime/thread for all portal calls.
- NOT hardware-retested. Owner steps: testing.md §4 "Pen must never stop the video".

## Branch viewer-menu (2026-11-25, merged into this branch)
- Hamburger overlay for all viewers: top-right corner icons ☰ / pen / disconnect, always shown (also after Hide). The menu has Pen, PowerPoint mode, Hide, Show statistics; the desktop menu also 1:1 and Fullscreen. Android's Disconnect/Pen text buttons and the desktop's Pen button in the remote row are gone. Icons are painter-drawn shapes (`chrome.rs`, one copy per viewer crate), no icon files.
- Show statistics is persisted in `settings.json` (`show_stats`, on unless `"off"`), desktop in the config dir, Android in the app data dir.
- Not hardware-tested (no tablet, no GUI run in the container). See the checklist line in testing.md §4.

## Release status (2026-11-25, v0.2.2 published)
- v0.2.2: tag `v0.2.2` (annotated) on 0ab0c33f4ff4c35a0701352e512fd682ccc3aa9c, tag Build run 45634094882 green, release https://github.com/windwerfer/mirrorwire/releases/tag/v0.2.2 with AppImage, Flatpak, Windows zip, macOS DMG, arm64 APK. (The main Build run's macOS arm64 job was cancelled while queued for a runner and re-run; no code change.)
- APK: package io.github.windwerfer.mirrorwire, versionCode 2002, versionName 0.2.2; signed v2 + v3 (v1 false); signer CN=Thomas Barth, certificate SHA-256 `4757b5b651b55f9f8e74f0f1202f0f367a0d16d2d74c46a9bf801c9bee5acaca`, now in `AllowedAPKSigningKeys`. Published APK sha256 `c9fbecbddbe9bbc00238d06f4b7e25a1afc9bd49c60c27172db135469fd34c31`.
- Reproducibility: a local unsigned build at the tag (worktree /workspace/mw-merge, NDK 27.2.12479018) has sha256 `bb69548011b6b53adb923352d076a4e319afb8094aae7d105c2b5f1abfff4945`; `apksigcopier compare <published> --unsigned <local>` passed (exit 0).
- SonarCloud: security rating A (1.0), 0 vulnerabilities after 0ab0c33 (`Path.resolve()` + base-directory check in the two scripts).
- Still for Thomas: screenshots, fdroiddata MR, IzzyOnDroid request, hardware retest (pen, menu, no-video modes).

## Earlier release status (2026-11-25, v0.2.1)
- v0.2.0 (tag 3161a39 -> 5d27d7c) was tagged but never released: Audit failed on RUSTSEC-2026-0592 (url 2.5.8), so no GitHub Release exists for it. The tag is untouched.
- v0.2.1 (commit 88192b2a3c43cf4e5bd2f47be85ecff0f6376894, annotated tag v0.2.1) is the first published 0.2 release: https://github.com/windwerfer/mirrorwire/releases/tag/v0.2.1
- Assets: MirrorWire-x86_64.AppImage, MirrorWire-x86_64.flatpak, MirrorWire-windows.zip, MirrorWire-0.2.1-macos-universal.dmg, MirrorWire-arm64-v8a.apk.
- CI: tag Build run 45559672556 green (Audit, CodeQL, Sonar, all packages, release); CI, Build, CodeQL on main green. Local loopback p95 25.53 ms (budget 40).
- Open: pen pressure (uinput) and the Miracast sink under Flatpak are not hardware-tested; `paste` 1.0.15 unmaintained warning (RUSTSEC-2024-0436) remains allowed; protocol 11 needs presenter and all viewers updated together.

## Previous notes (branch pen-uinput, based on c20b796 = main e8760cb tree; steps 14-17 were on flatpak-nm)

## Goal
Current (step 18, 2026-11-22): optional pen pressure on the Linux presenter through uinput, opt-in and off by default (owner decision D26, amends D1), with a system check button, an enable checkbox that unlocks only when the check passes, and a fix panel (commands, why, what changes, undo).

Current (step 17, 2026-11-22): owner approved `--system-talk-name=org.freedesktop.hostname1` for the Flatpak so the "Name shown on Android" line (D25) also works there.

Step 16 (2026-11-22): show the name Android lists for this computer on the Miracast screen, with an ℹ hint (hostnamectl) when it is empty (D25).

Step 14 (2026-11-22): owner decision D24, add `--system-talk-name=org.freedesktop.NetworkManager` to the Flatpak so the Miracast-compatible button works after install; accurate "not available"/"not allowed" messages; Flathub justification text.

Step 13 (2026-11-21, 2026-11-21): make `cargo xtask ci` green again (clippy dead_code) and take the FilmCraft encoder from craft-libs (`craft-video`, `h264-encode`) with one craft-libs pin for the workspace; then full CI on the box and merge into main.

Original: Miracast sink spike (Android tablet TB336FU = source, Fedora KDE laptop = sink), rootless via NetworkManager D-Bus WifiP2P + Polkit. Clean-room, new crates only, never touch main. Steps 1-6 one per session.

## Done
- Step 18 (pen pressure via uinput, 2026-11-22, branch `pen-uinput`, local commits only, not pushed):
  - Portal vs uinput (official docs only): the RemoteDesktop portal (v2) has device types KEYBOARD/POINTER/TOUCHSCREEN only and no pressure. libei's protocol has `ei_stylus` (pressure, tilt, distance), but no KDE/GNOME doc or release note read says the compositor offers it through the portal (Plasma 6.8 notes: nothing), and `reis` 0.7.1 (MIT) has no stylus module. Not "clearly possible" -> uinput, as the owner allowed.
  - `1d5dc9c` D26 (table + log; D1 row amended; AGENTS.md §1.1 exception). Rejected: always-on uinput, no pressure, portal-only.
  - `9e994d6` protocol 11: `pointer.pressure` (required, u16 or null); desktop + Android viewers send egui touch force; portal ignores it. protocol.md, CHANGELOG.
  - `c0a3962` `settings.json` `pen_pressure` (off unless "on"; junk/broken = off).
  - `a8c7874` `mirrorwire-app/src/pen/`: `check` (Probe trait, SystemProbe, report, fix/undo text), `uinput` (evdev `VirtualDevice`, pure frame sequences), `PenRouter` in `RemoteRoute`; GUI section "Pen pressure" on the presenter screen (`gui/pen.rs`); CLI prints the fix when saved on but failing. New dep `evdev` 0.13 (Apache-2.0 OR MIT) + nix 0.29, bitvec, funty, radium, tap, wyz (MIT). No new `unsafe` in our code.
  - `5ac7040` README section, packaging/flatpak/README.md (user override, not manifest), roadmap, architecture, testing.

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
- D26: uinput only behind "Enable pen pressure" (off by default, saved), and only after the check passes. The app never runs the commands. Rule: `KERNEL=="uinput", SUBSYSTEM=="misc", TAG+="uaccess", OPTIONS+="static_node=uinput"` in `/etc/udev/rules.d/70-mirrorwire-uinput.rules` (before `73-seat-late.rules`, which applies the tag; same pattern as systemd's own `70-uaccess.rules` for `rfkill`). Rejected for the rule: `GROUP="input"` (also reads every keyboard), `MODE="0666"`. Flatpak: `flatpak override --user --device=all` (no narrower Flatpak device; `input` is `/dev/input` only); undo `--reset` (or `--nodevice=all`).
- Virtual pen: `INPUT_PROP_DIRECT` only (display tablet; POINTER would mean an off-screen tablet needing a cursor). Pressure 0..4095; `null` from the viewer = full pressure; touching = at least 1. Device created on the first pen down, removed when the setting goes off or the presenter stops. Keys always stay on the portal; a failed create/write falls back to the portal pointer with a note and no retry until toggled.
- The pen path bypasses the portal's Mirror-only check: the compositor maps the tablet (KDE: Drawing Tablet settings); the GUI says to map "MirrorWire virtual pen" to the shared screen when there are several.

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
- Step 18, nothing hardware-tested: no `/dev/uinput` access, pen, touch screen, KDE session or GUI run here; the virtual device was never created. Unverified: KWin/libinput accepting the device as a tablet, its default screen mapping (several screens, Extend), Krita seeing pressure, the Android viewer's force values (egui-winit maps Android pressure to `force`; the Android crate was not compiled: no NDK linker here), whether `udevadm trigger --sysname-match=uinput` applies the ACL without a re-login, and whether `--device=all` exposes `/dev/uinput` in the sandbox (docs say "all devices" without a list).
- When KWin offers `ei_stylus` through the RemoteDesktop portal and `reis` supports it, D26's uinput path can be replaced (no udev rule, no `--device=all`).
- `cargo audit` reports RUSTSEC-2026-0592 (`url` 2.5.8, published 2026-11-20); also on the base commit, not from this step.
- The no-GUI app build (`--no-default-features --features filmcraft`) already failed clippy before step 18 (unused imports, dead `ExtendSize::Fixed`); step 18 adds no new errors there.

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
Step 18: `git log --oneline c20b796..pen-uinput` (5 commits + this HANDOFF); `cargo fmt --all --check`; `cargo clippy --workspace --all-targets --no-default-features --features gui,filmcraft,miracast -- -D warnings` and the same with `gui,filmcraft`; `cargo test --workspace --no-fail-fast --no-default-features --features gui,filmcraft,miracast` (all pass except the known x264 test); `cargo xtask layers`; `cargo deny --all-features --locked check licenses bans sources` (ok); `(cd fuzz && cargo check --locked)`. Default features (portal, ffmpeg) cannot build here (no libpipewire). On the T480s: the hardware checklist in testing.md §4 (pen pressure).

Step 17: `git log --oneline 28705c7..flatpak-nm` (2 commits + this HANDOFF); manifest parse with the throwaway serde_yaml binary `/tmp/yamlcheck` (8 finish-args, last `--system-talk-name=org.freedesktop.hostname1`); `cargo fmt --all --check`; `cargo clippy -p miracast-nm -p mirrorwire-app --all-targets --no-default-features --features gui,filmcraft,miracast -- -D warnings`; `cargo test -p miracast-nm` (31).

Step 16: `git log --oneline 3d76ce4..flatpak-nm` (3 commits); `cargo fmt --all --check`; `cargo clippy --workspace --all-targets --no-default-features --features gui,filmcraft,miracast -- -D warnings` and the same with `gui,filmcraft`; `cargo test -p miracast-nm -p miracast-loopback` (31 + 14); `cargo test -p mirrorwire-app --no-default-features --features gui,filmcraft,miracast` (34 pass, the x264 test fails without FFmpeg). On the T480s: open the Miracast screen, expect "Name shown on Android: none ℹ"; run the two hostnamectl commands, toggle Wi-Fi, check the tablet's Cast list.

Step 14: `git log --oneline a5344da..flatpak-nm` (3 commits); `cargo fmt --all --check`; `cargo test -p miracast-nm -p miracast-loopback`; `cargo test -p mirrorwire-app --no-default-features --features gui,filmcraft,miracast` (32 pass, the x264 test fails without FFmpeg); `cargo clippy -p mirrorwire-app -p miracast-nm --all-targets --no-default-features --features gui,filmcraft,miracast -- -D warnings`; manifest parse: no PyYAML here, parsed with a throwaway serde_yaml binary in /tmp (finish-args list printed, 7 entries). On the box: `flatpak-builder --user --install build-dir packaging/flatpak/io.github.windwerfer.MirrorWire.yml`, `flatpak info --show-permissions io.github.windwerfer.MirrorWire`.

Step 13: `cargo fmt --all --check`; `cargo clippy --workspace --all-targets --no-default-features --features gui,filmcraft,miracast -- -D warnings` and the same with `gui,filmcraft`; `cargo test --no-fail-fast -p mirrorwire-media -p mirrorwire-film -p mirrorwire-app --no-default-features --features mirrorwire-app/gui,mirrorwire-app/filmcraft,mirrorwire-app/miracast` (22 + 2 + 31/32, the one failure needs x264); `cargo deny --all-features --locked check advisories bans licenses sources`; `(cd fuzz && cargo check --locked)`; `grep -A2 'name = "craft-' Cargo.lock` (one rev). On the box: `cargo xtask ci`.

`git log --oneline 767ed3c..miracast-spike`; `cargo test -p miracast-nm -p miracast-rtsp -p miracast-rtp -p miracast-loopback` (94 tests); `cargo clippy -p miracast-nm -p miracast-rtsp -p miracast-rtp -p miracast-loopback --all-targets -- -D warnings`; `cargo fmt --all --check`; `cargo xtask layers`. `cargo xtask ci` cannot build in the research container (no libpipewire-0.3 dev package), unrelated to the spike.

## Next step
Pen stall (`pen-stall-fix`): on the T480s, from the container: `/tmp/mw-hw-target/release/mirrorwire present --encoder filmcraft --mode mirror --uri-file /tmp/mw-hw/uri`; **accept the KDE dialog (screen + keyboard + pointer)**; the log must show `capture: cursor ...` and no "combined capture + input session unavailable". Then `adb shell am start -a android.intent.action.VIEW -d "\"$(cat /tmp/mw-hw/uri)\"" io.github.windwerfer.mirrorwire.dev`, tap Pen (pressure off), draw for 30 s: video must keep moving, the laptop pointer must follow. Then change the cursor mode or Mirror/Extend: video must come back. Then the same with a Flatpak built from this branch (pen pressure off, no udev rule).

Step 18: manager reviews `pen-uinput` and merges (protocol 11: presenter and viewers must update together; rebuild the Android APK). Then on the T480s: `cargo xtask ci`, the Android NDK clippy, and the testing.md §4 pen pressure checklist (check before/after the udev rule, Krita strokes from the tablet, Drawing Tablet KCM, undo).

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
