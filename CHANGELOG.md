# Changelog

## 0.2.2

### Breaking
- Protocol version 12: new `pen_mode` and `video_paused` control messages. Update the
  presenter, the desktop viewer and the Android viewer together.

### Added
- Start screen › Advanced: **Use the FilmCraft encoder**, remembered in `settings.json`
  (`filmcraft_encoder`). The GUI presenter then starts with FilmCraft instead of `auto`, so a
  machine whose VAAPI/x264 fails at once can still present. Always on in builds without
  FFmpeg.
- Start screen: a **✏** next to the Presenter button shows whether the desktop's pen/keys
  permission is remembered (green); click it for an explanation and **Forget the pen and
  keys permission**.
- Viewer ☰ menu (desktop and Android): **Wacom pen (no video)** and **PowerPoint remote
  only (no video)**. The presenter then skips encoding and sending until the viewer shows the
  picture again (resumes with a keyframe).
- Presenter window: a dialog explains the desktop's pointer-permission prompt while the
  capture reopens with input for the viewer's pen.
- Reproducible Android build: pinned toolchain (`rust-toolchain.toml`, Rust 1.100.0), fixed
  timestamps (`SOURCE_DATE_EPOCH` = commit time), remapped build paths, stored `classes.dex`,
  fixed zip order; `MIRRORWIRE_UNSIGNED=1 packaging/android/build-apk.sh` writes the unsigned
  aligned APK. Two clones at different paths give the same bytes and `apksigcopier compare`
  accepts the signed APK. CI checks this on every build.
- F-Droid / IzzyOnDroid preparation: `fastlane/metadata/android/en-US/` and a draft
  `packaging/fdroid/io.github.windwerfer.mirrorwire.yml` with `packaging/fdroid/README.md`
  (not submitted).

### Changed
- The presenter starts with screen sharing only. Pointer and keyboard permission are asked
  when the viewer switches its pen on (the capture reopens once, the picture pauses for a
  moment), or at start without a dialog when the desktop already remembers that permission.
- Android `versionCode` is derived from the version (major*1000000 + minor*1000 + patch, 0.2.2
  is 2002) instead of the CI run number (v0.2.1 had 139, so this upgrades it). The dev app adds
  2000000000. The APK is signed with v2 + v3 only (no v1 JAR signature; minSdk is 26).
- `scripts/run-stats.py` and `scripts/sim-compare.py` only read files inside the current
  directory, the repository, the temp directory or `MIRRORWIRE_SCRIPT_BASE` (Sonar S8707).

### Fixed
- Pen/touch and remote keys never worked on KDE: the presenter passed `persist_mode` and a
  restore token to ScreenCast `SelectSources` on RemoteDesktop sessions, which the spec does
  not allow; KDE answered "Remote desktop sessions cannot persist" for the combined session
  and for every pen attempt. Persistence now goes through `SelectDevices` only, and a session
  that fails half-way is closed instead of left open.

## 0.2.1

First published release of the 0.2 series. v0.2.0 was tagged but never released: its Audit job
failed on RUSTSEC-2026-0592 (`url` 2.5.8), so the GitHub Release job never ran. The v0.2.0 tag
stays as it is; everything listed under 0.2.0 below ships in this release.

### Breaking
- Protocol version 11 (unchanged from the 0.2.0 tag): the presenter and all viewers must be
  updated together, including the new Android APK.

### Security
- Upgrade `url` 2.5.8 to 2.5.9 (and `form_urlencoded` 1.2.3) to fix RUSTSEC-2026-0592 (stack
  overflow in `Url::origin()` on nested `blob:` URLs).

### Untested on hardware
- Opt-in pen pressure (uinput) and the Miracast sink under Flatpak have not been tested on
  real hardware yet.

## 0.2.0 (tagged, never released)


### Breaking
- Protocol version 11: the `pointer` control message carries a required `pressure`
  (0..65535, or `null` without a sensor). **The presenter and all viewers must be updated
  together, including the new Android APK**; a peer on another version is rejected with an
  "update both devices" message.

### Added
- Miracast-compatible sink on Linux (D21/D22): the Linux app can show the screen of an Android
  device that casts with "Wireless display".
- Flatpak permission `--system-talk-name=org.freedesktop.NetworkManager` (D24) for Wi-Fi P2P
  when receiving from Android.
- "Name shown on Android" setting, with the read-only `--system-talk-name=org.freedesktop.hostname1`
  Flatpak permission (D25).
- Optional pen pressure on the Linux presenter (off by default, decision D26): a "Check pen
  pressure support on my system" button, an "Enable pen pressure" checkbox that only unlocks
  when the check passes, and a fix panel with the udev rule (and the Flatpak override) to copy.
  When on, pen input goes to a virtual uinput pen with pressure instead of the RemoteDesktop
  portal pointer.

### Fixed
- Release job: the Windows artifact is zipped only when it is a folder, then attached to the
  GitHub Release.

### Untested on hardware
- Opt-in pen pressure (uinput) and the Miracast sink under Flatpak have not been tested on
  real hardware yet.

## 0.1.0

First release. Low-latency screen mirroring over a LAN.

### What works
- Presenter (Linux, Wayland): capture through the xdg-desktop-portal ScreenCast and PipeWire,
  H.264 encode (VAAPI, x264 fallback), QUIC with TLS 1.3, pairing code once then remembered by
  certificate fingerprint. Mirror and Extend modes, adaptive bitrate, FEC.
- Viewers: Linux, Windows, macOS and Android; discovery by mDNS, `mirrorwire://` URI or QR
  code (the Linux desktop viewer can also scan it with a camera).
- Presenter remote (next/previous keys) and pen/touch as a pointer, via the RemoteDesktop portal.
- Protocol version 10. Presenter and viewer must be the same release; a different version is
  rejected with an "update both devices" message.

### Untested on hardware
- Pen/touch in Mirror and Extend, camera QR scan, Extend sizing on KDE, the Flatpak and
  AppImage builds on FFmpeg versions other than the ones in CI, Windows and macOS viewers on
  real machines.

### Known limits
- The presenter runs on Linux only. LAN only (no internet or relay streaming).
- No audio; no remote desktop control beyond the presenter remote and pen/touch pointer.
- The Windows build is unsigned; the macOS build is ad-hoc signed, not notarized; the Android
  APK is debug-signed unless a release key is configured.
