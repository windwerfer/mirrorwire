# MirrorWire Flatpak

Manifest: `io.github.windwerfer.MirrorWire.yml`. The `finish-args` are the ceiling set in
AGENTS.md §1.2 and decisions D2/D24; adding one needs the owner.

## Permissions and why

| finish-arg | Why |
|---|---|
| `--socket=wayland` | the window (Wayland-first, D13) |
| `--socket=fallback-x11`, `--share=ipc` | X11 only when no Wayland is available |
| `--share=network` | QUIC streaming and mDNS discovery on the LAN; Miracast RTSP/RTP |
| `--device=dri` | VA-API encode/decode and GPU rendering |
| `--system-talk-name=org.fedoraproject.FirewallD1` | the presenter shows whether firewalld blocks its UDP port and, only after the user clicks "Open port for me…", asks firewalld to open it (Polkit password dialog) |
| `--system-talk-name=org.freedesktop.NetworkManager` | receiving from an Android device (Miracast-compatible, D24): Wi-Fi Direct (P2P) through NetworkManager |

No host filesystem, no session-bus names, no `--talk-name`, no `--device=all`. Screen capture
uses the ScreenCast portal.

## Flathub review: system bus access to NetworkManager

Text for the Flathub submission (reviewers ask for a reason for every system-bus name):

> MirrorWire can receive the screen of an Android device over Wi-Fi Direct (the Wi-Fi Display
> protocol that Android's "Cast" / "Wireless display" uses; Miracast-compatible, not Wi-Fi
> Alliance certified). On Linux the only unprivileged way to run Wi-Fi Direct is
> NetworkManager's D-Bus API. There is no portal for NetworkManager or Wi-Fi P2P, and Flatpak
> has no runtime permission dialog, so without this name the feature cannot work in the
> sandbox. wpa_supplicant's own D-Bus API is root-only and would conflict with NetworkManager,
> so it is not used.
>
> The permission is limited to one well-known name, `org.freedesktop.NetworkManager`. The app
> calls only:
>
> - `GetAllDevices` and the `Device` properties `DeviceType`, `Interface`, `IpInterface`,
>   `State`, `Ip4Config` to find the Wi-Fi P2P device and the group's IP setup;
> - `Device.WifiP2P.StartFind` / `StopFind` and the `Peers` property, and the `WifiP2PPeer`
>   properties `Name`, `HwAddress`, `WfdIEs`, `Strength` to find the Android device;
> - `AddAndActivateConnection2` with `persist: volatile` (an in-memory `wifi-p2p` profile, never
>   written to disk) and `DeactivateConnection` to remove it on stop, cancel or window close;
> - `IP4Config` `AddressData` / `Gateway` to reach the device's stream server;
> - read-only `Device.Wireless.ActiveAccessPoint` and `AccessPoint.Frequency`, shown next to a
>   session as a diagnostic (Wi-Fi Direct shares the radio with the station link).
>
> Nothing runs at startup: NetworkManager is contacted only after the user clicks "View your
> Android device (Miracast-compatible)". NetworkManager's Polkit rules still decide each call
> (`network-control`, `settings.modify.own`, `wifi.scan`); an active local desktop session
> normally needs no password. MirrorWire never changes the user's saved connections,
> needs no root, and installs no Polkit rules.

### Comparison with GNOME Network Displays

GNOME Network Displays (`org.gnome.NetworkDisplays`, GPL-3.0-or-later) is on Flathub and also
uses Wi-Fi Direct for Miracast. Flathub's published metadata
(`https://flathub.org/api/v2/summary/org.gnome.NetworkDisplays`, read 2026-11-22, version
0.99.0) lists `sockets: wayland, fallback-x11, pulseaudio`, `shared: ipc, network`,
`devices: dri` and **no** system-bus name. So it is **not** confirmed precedent for
`--system-talk-name=org.freedesktop.NetworkManager`: the API may leave out system-bus entries, or
the app may reach NetworkManager another way. Before using it as an argument in the
submission, check with `flatpak remote-info --show-metadata flathub org.gnome.NetworkDisplays`
(metadata only; under D20 its source code is not read). If it does not hold, leave the
comparison out: the justification above stands on its own.

## Removing the permission

Users who do not want Wi-Fi Direct can remove the name:

```sh
flatpak override --user --no-system-talk-name=org.freedesktop.NetworkManager io.github.windwerfer.MirrorWire
```

The button then shows "Receiving from Android is not available" with the command that gives it
back (`flatpak override --user --system-talk-name=org.freedesktop.NetworkManager
io.github.windwerfer.MirrorWire`). Everything else keeps working.
