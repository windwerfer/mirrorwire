//! Presenter remote: taps keys (Right, Left, Space, Escape) and moves a pen/finger pointer
//! through the xdg-desktop-portal RemoteDesktop interface. No uinput, no root (decisions D1);
//! the portal asks the user once (the restore token is kept so the dialog does not return) and
//! works inside the Flatpak sandbox. The session opens lazily on the first input, so a presenter
//! that nobody uses as a remote never shows the dialog. A key-only session is upgraded (closed
//! and reopened, one more dialog) when the first pointer event arrives: absolute pointer motion
//! is addressed to a screen-cast stream, so the pointer session also selects a monitor source.

use anyhow::{anyhow, bail, Context, Result};
use ashpd::desktop::remote_desktop::{DeviceType, KeyState, RemoteDesktop, SelectDevicesOptions};
use ashpd::desktop::screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType};
use ashpd::desktop::{PersistMode, Session};
use mirrorwire_proto::control::{pointer_to_stream, PointerPhase, RemoteInput, RemoteKey};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Linux evdev code of the left mouse button (what the portal's button call takes).
const BTN_LEFT: i32 = 0x110;

/// Receives one human-readable line whenever the remote's state changes (shown in the
/// presenter window, AGENTS.md §1.8).
pub type RemoteStatusSink = Arc<dyn Fn(String) + Send + Sync>;

/// Whether the pen/finger can be mapped onto the picture right now. This separate injector
/// (the fallback when capture and input cannot share one session, see `portal.rs`) offers a
/// monitor stream of its own, so it maps only in Mirror mode; Extend's virtual monitor is a
/// different stream there and the pointer is refused with a message.
pub type PointerMapped = Arc<dyn Fn() -> bool + Send + Sync>;

/// Mirror maps onto the separate injector's monitor stream; Extend does not.
pub fn separate_session_maps_pointer(mode: super::CaptureMode) -> bool {
    mode == super::CaptureMode::Mirror
}

/// Told `true` while the desktop's permission dialog for pen/touch (pointer) input is open
/// and `false` once it is answered, so the presenter window can explain it.
pub type AskingSink = Arc<dyn Fn(bool) + Send + Sync>;

/// Start the injector task on the current tokio runtime. Send taps into the returned sender.
pub fn spawn_remote_injector(
    restore_token_path: PathBuf,
    status: RemoteStatusSink,
    asking: AskingSink,
    pointer_mapped: PointerMapped,
) -> mpsc::Sender<RemoteInput> {
    let (tx, mut rx) = mpsc::channel::<RemoteInput>(64);
    status("presenter remote: waiting for the first button press".into());
    tokio::spawn(async move {
        let mut session: Option<Remote> = None;
        // Last pointer failure shown, so a hovering pen does not rewrite the line 120x/s.
        let mut pointer_refused = false;
        while let Some(input) = rx.recv().await {
            if matches!(input, RemoteInput::PenMode(_)) {
                // A mode change is the app's business; it must never open a session here.
                continue;
            }
            let wants_pointer = matches!(input, RemoteInput::Pointer { .. });
            if wants_pointer && !pointer_mapped() {
                if !pointer_refused {
                    status(
                        "pen/touch input works in Mirror mode only (the capture session could not be shared with input)".into(),
                    );
                    pointer_refused = true;
                }
                continue;
            }
            pointer_refused = false;
            if session
                .as_ref()
                .is_some_and(|s| wants_pointer && s.stream.is_none())
            {
                // Upgrade a key-only session.
                if let Some(old) = session.take() {
                    let _ = old.session.close().await;
                }
            }
            if session.is_none() {
                // Moves while no session exists must not each open a dialog.
                if matches!(input, RemoteInput::Pointer { phase, .. } if phase != PointerPhase::Down)
                {
                    continue;
                }
                status(if wants_pointer {
                    "presenter remote: asking the desktop for pointer permission (choose the screen to control)…".into()
                } else {
                    "presenter remote: asking the desktop for permission…".into()
                });
                asking(wants_pointer);
                let opened = open_session(&restore_token_path, wants_pointer).await;
                asking(false);
                match opened {
                    Ok(s) => {
                        status(if s.stream.is_some() {
                            "presenter remote: ready (keys and pen/touch pointer)".into()
                        } else {
                            "presenter remote: ready (taps Right/Left/Space/Esc)".into()
                        });
                        session = Some(s);
                    }
                    Err(e) => {
                        tracing::warn!("presenter remote: {e:#}");
                        status(format!(
                            "presenter remote unavailable: {e:#}. Press a remote button again to retry."
                        ));
                        continue;
                    }
                }
            }
            let Some(remote) = &session else {
                continue;
            };
            if let Err(e) = inject(&remote.proxy, &remote.session, remote.stream, input).await {
                tracing::warn!("presenter remote input failed: {e:#}");
                status(format!(
                    "presenter remote: input failed ({e:#}); reconnecting"
                ));
                session = None;
            }
        }
    });
    tx
}

/// An open RemoteDesktop session. `stream` is the (node id, size) pointer motion is addressed
/// to; `None` for a key-only session.
struct Remote {
    proxy: RemoteDesktop,
    session: Session<RemoteDesktop>,
    stream: Option<(u32, (i32, i32))>,
}

async fn open_session(token_path: &PathBuf, pointer: bool) -> Result<Remote> {
    let proxy = RemoteDesktop::new()
        .await
        .context("the desktop has no RemoteDesktop portal")?;
    let available = proxy.available_device_types().await.unwrap_or_default();
    if !available.contains(DeviceType::KEYBOARD) {
        bail!("the RemoteDesktop portal offers no keyboard");
    }
    if pointer && !available.contains(DeviceType::POINTER) {
        bail!("the RemoteDesktop portal offers no pointer");
    }
    let session = proxy.create_session(Default::default()).await?;
    match set_up(&proxy, &session, token_path, pointer).await {
        Ok(stream) => Ok(Remote {
            proxy,
            session,
            stream,
        }),
        Err(e) => {
            // Never leave a half-set-up session behind on the desktop.
            let _ = session.close().await;
            Err(e)
        }
    }
}

/// SelectDevices (+ SelectSources for the pointer), Start, and keep the restore token.
async fn set_up(
    proxy: &RemoteDesktop,
    session: &Session<RemoteDesktop>,
    token_path: &PathBuf,
    pointer: bool,
) -> Result<Option<(u32, (i32, i32))>> {
    let token = tokio::fs::read_to_string(token_path)
        .await
        .ok()
        .map(|t| t.trim().to_owned())
        .filter(|t| !t.is_empty());
    let devices = if pointer {
        DeviceType::KEYBOARD | DeviceType::POINTER
    } else {
        DeviceType::KEYBOARD
    };
    proxy
        .select_devices(
            session,
            SelectDevicesOptions::default()
                .set_devices(devices)
                .set_persist_mode(PersistMode::ExplicitlyRevoked)
                .set_restore_token(token.as_deref()),
        )
        .await?
        .response()?;
    if pointer {
        // Absolute motion is addressed to a screen-cast stream of this same session.
        let cast = Screencast::new()
            .await
            .context("connecting to ScreenCast portal")?;
        // No persist_mode / restore_token here: on a RemoteDesktop session persistence is
        // SelectDevices' job only (ScreenCast spec; KDE: "Remote desktop sessions cannot
        // persist").
        cast.select_sources(
            session,
            SelectSourcesOptions::default()
                .set_cursor_mode(CursorMode::HIDDEN)
                .set_sources(SourceType::MONITOR)
                .set_multiple(false),
        )
        .await?
        .response()?;
    }
    let started = proxy
        .start(session, None, Default::default())
        .await?
        .response()
        .context("remote access was cancelled or denied")?;
    if !started.devices().contains(DeviceType::KEYBOARD) {
        bail!("keyboard access was not granted");
    }
    let stream = if pointer {
        if !started.devices().contains(DeviceType::POINTER) {
            bail!("pointer access was not granted");
        }
        let st = started
            .streams()
            .first()
            .ok_or_else(|| anyhow!("the portal returned no screen to point at"))?;
        let size = st
            .size()
            .ok_or_else(|| anyhow!("the portal did not tell the screen size"))?;
        Some((st.pipe_wire_node_id(), size))
    } else {
        None
    };
    if let Some(t) = started.restore_token() {
        if let Some(dir) = token_path.parent() {
            tokio::fs::create_dir_all(dir).await?;
        }
        tokio::fs::write(token_path, t).await?;
    }
    Ok(stream)
}

/// Send one input through an open RemoteDesktop session. `stream` is the (node id, size) of
/// the screen-cast stream pointer motion is addressed to. Shared with the combined
/// capture + input session in `portal.rs`.
pub(super) async fn inject(
    proxy: &RemoteDesktop,
    session: &Session<RemoteDesktop>,
    stream: Option<(u32, (i32, i32))>,
    input: RemoteInput,
) -> Result<()> {
    match input {
        RemoteInput::Key(key) => tap(proxy, session, key).await,
        // Handled by the app's input route (it reopens the capture with input); nothing to
        // inject.
        RemoteInput::PenMode(_) => Ok(()),
        // The portal has no pressure (no tablet device type); `pressure` goes to the opt-in
        // uinput pen in the app instead (D26).
        RemoteInput::Pointer { phase, x, y, .. } => {
            let Some((node, size)) = stream else {
                return Ok(());
            };
            let Some((px, py)) = pointer_to_stream(x, y, size) else {
                return Ok(());
            };
            proxy
                .notify_pointer_motion_absolute(session, node, px, py, Default::default())
                .await?;
            let state = match phase {
                PointerPhase::Move => return Ok(()),
                PointerPhase::Down => KeyState::Pressed,
                PointerPhase::Up => KeyState::Released,
            };
            proxy
                .notify_pointer_button(session, BTN_LEFT, state, Default::default())
                .await?;
            Ok(())
        }
    }
}

async fn tap(
    proxy: &RemoteDesktop,
    session: &Session<RemoteDesktop>,
    key: RemoteKey,
) -> Result<()> {
    let sym = key.keysym();
    proxy
        .notify_keyboard_keysym(session, sym, KeyState::Pressed, Default::default())
        .await?;
    proxy
        .notify_keyboard_keysym(session, sym, KeyState::Released, Default::default())
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::CaptureMode;

    #[test]
    fn separate_session_maps_mirror_only() {
        assert!(separate_session_maps_pointer(CaptureMode::Mirror));
        assert!(!separate_session_maps_pointer(CaptureMode::Extend));
    }

    /// Extend: the pointer lands on the virtual monitor's own size, whatever size the
    /// encoder was capped to (the viewer sends a position within the whole picture).
    #[test]
    fn extend_pointer_maps_to_stream_size_not_encode_size() {
        let virtual_monitor = (2560, 1600);
        let encoded_cap = (1280, 800); // EncoderConfig::max_size scaled the picture down
        let (x, y) = pointer_to_stream(65535, 32768, virtual_monitor).unwrap();
        assert_eq!(x, 2559.0);
        assert!((y - 799.5).abs() < 0.5);
        assert!(x > f64::from(encoded_cap.0));
    }
}
