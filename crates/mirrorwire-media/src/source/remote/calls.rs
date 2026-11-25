//! The xdg-desktop-portal RemoteDesktop calls behind the separate input injector in
//! `remote.rs`: open a session (with its permission dialog), send one input. They need a
//! running portal and a user session, so they are not unit-tested; the decisions around them
//! (`Policy`, pointer mapping) stay in `remote.rs` and are.

use super::{BTN_LEFT, INPUT_TIMEOUT};
use anyhow::{anyhow, bail, Context, Result};
use ashpd::desktop::remote_desktop::{DeviceType, KeyState, RemoteDesktop, SelectDevicesOptions};
use ashpd::desktop::screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType};
use ashpd::desktop::{PersistMode, Session};
use mirrorwire_proto::control::{pointer_to_stream, PointerPhase, RemoteInput, RemoteKey};
use std::path::Path;

/// An open RemoteDesktop session. `stream` is the (node id, size) pointer motion is addressed
/// to; `None` for a key-only session.
pub(super) struct Remote {
    pub(super) proxy: RemoteDesktop,
    pub(super) session: Session<RemoteDesktop>,
    pub(super) stream: Option<(u32, (i32, i32))>,
}

pub(super) async fn open_session(token_path: &Path, pointer: bool) -> Result<Remote> {
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
            let _ = tokio::time::timeout(INPUT_TIMEOUT, session.close()).await;
            Err(e)
        }
    }
}

/// SelectDevices (+ SelectSources for the pointer), Start, and keep the restore token.
async fn set_up(
    proxy: &RemoteDesktop,
    session: &Session<RemoteDesktop>,
    token_path: &Path,
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
pub(in crate::source) async fn inject(
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
