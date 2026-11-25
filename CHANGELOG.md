# Changelog

## Unreleased

### Added
- Start screen › Advanced: **Use the FilmCraft encoder**, remembered in `settings.json`
  (`filmcraft_encoder`). The GUI presenter then starts with FilmCraft instead of `auto`, so a
  machine whose VAAPI/x264 fails at once can still present. Always on in builds without
  FFmpeg.

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
