//! Presenter remote: taps keys (Right, Left, Space, Escape) and moves a pen/finger pointer
//! through the xdg-desktop-portal RemoteDesktop interface. No uinput, no root (decisions D1);
//! the portal asks the user once (the restore token is kept so the dialog does not return) and
//! works inside the Flatpak sandbox.
//!
//! Normally capture and input share one session (`portal.rs`), and [`inject`] runs there. The
//! separate injector here is the fallback when that combined session was refused or is
//! unavailable. It is **key-only**: absolute pointer motion is addressed to a screen-cast
//! stream of the injecting session, so pen/touch would need a second ScreenCast session
//! (`SelectSources` + `Start`, another "choose the screen" dialog) started while the capture
//! runs. Nothing in the portal documentation promises that a second cast leaves the running
//! one alone, and the presenter must never risk its video for input (owner report t316u), so
//! pen/touch is refused with a note instead. The key session opens lazily on the first tap.

use anyhow::{bail, Context, Result};
use ashpd::desktop::remote_desktop::{DeviceType, KeyState, RemoteDesktop, SelectDevicesOptions};
use ashpd::desktop::{PersistMode, Session};
use mirrorwire_proto::control::{pointer_to_stream, PointerPhase, RemoteInput, RemoteKey};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

/// Linux evdev code of the left mouse button (what the portal's button call takes).
const BTN_LEFT: i32 = 0x110;

/// Longest one input may take in the portal before it is given up. The calls are plain D-Bus
/// method calls that answer at once; one that does not is a stuck portal, and the input path
/// must move on (a later input retries).
pub const INPUT_TIMEOUT: Duration = Duration::from_secs(2);

/// Receives one human-readable line whenever the remote's state changes (shown in the
/// presenter window, AGENTS.md §1.8).
pub type RemoteStatusSink = Arc<dyn Fn(String) + Send + Sync>;

/// Shown when pen/touch reaches the key-only fallback injector.
pub const POINTER_NEEDS_SHARED_SESSION: &str = "pen/touch is not available: capture and input \
     could not share one portal session. Stop and start sharing and allow keyboard and pointer \
     control in the dialog (pen pressure through the virtual pen works without it)";

/// What the key-only injector does with one input.
#[derive(Debug, PartialEq, Eq)]
enum Action {
    /// Tap the key (opening the key session first if needed).
    Key(RemoteKey),
    /// Nothing to do here.
    Ignore,
    /// Refuse the pointer; `true` = show the note (once per run of pointer input).
    RefusePointer(bool),
}

/// The fallback injector's policy, apart from the portal so it is testable: keys go through,
/// pointer input never opens a session.
#[derive(Default)]
struct KeyOnly {
    pointer_refused: bool,
}

impl KeyOnly {
    fn action(&mut self, input: RemoteInput) -> Action {
        match input {
            RemoteInput::Key(key) => {
                self.pointer_refused = false;
                Action::Key(key)
            }
            // A mode change is the app's business; it must never open a session here.
            RemoteInput::PenMode(_) => Action::Ignore,
            RemoteInput::Pointer { .. } => {
                Action::RefusePointer(!std::mem::replace(&mut self.pointer_refused, true))
            }
        }
    }
}

/// Start the key-only injector task on the shared portal runtime (`portal_rt`). Send taps
/// into the returned sender; pen/touch sent here is refused with a note.
pub fn spawn_remote_injector(
    restore_token_path: PathBuf,
    status: RemoteStatusSink,
) -> std::io::Result<mpsc::Sender<RemoteInput>> {
    let rt = crate::source::portal_rt::portal_runtime()?;
    let (tx, mut rx) = mpsc::channel::<RemoteInput>(64);
    status("presenter remote: waiting for the first button press".into());
    rt.spawn(async move {
        let mut session: Option<Remote> = None;
        let mut policy = KeyOnly::default();
        while let Some(input) = rx.recv().await {
            let key = match policy.action(input) {
                Action::Key(key) => key,
                Action::Ignore => continue,
                Action::RefusePointer(note) => {
                    if note {
                        status(POINTER_NEEDS_SHARED_SESSION.into());
                    }
                    continue;
                }
            };
            if session.is_none() {
                status("presenter remote: asking the desktop for keyboard permission…".into());
                match open_session(&restore_token_path).await {
                    Ok(s) => {
                        status("presenter remote: ready (taps Right/Left/Space/Esc)".into());
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
            let sent = tokio::time::timeout(
                INPUT_TIMEOUT,
                inject(&remote.proxy, &remote.session, None, RemoteInput::Key(key)),
            )
            .await
            .unwrap_or_else(|_| Err(anyhow::anyhow!("the portal did not answer")));
            if let Err(e) = sent {
                tracing::warn!("presenter remote input failed: {e:#}");
                status(format!(
                    "presenter remote: input failed ({e:#}); reconnecting on the next tap"
                ));
                if let Some(old) = session.take() {
                    let _ = tokio::time::timeout(INPUT_TIMEOUT, old.session.close()).await;
                }
            }
        }
    });
    Ok(tx)
}

/// An open key-only RemoteDesktop session.
struct Remote {
    proxy: RemoteDesktop,
    session: Session<RemoteDesktop>,
}

async fn open_session(token_path: &Path) -> Result<Remote> {
    let proxy = RemoteDesktop::new()
        .await
        .context("the desktop has no RemoteDesktop portal")?;
    let available = proxy.available_device_types().await.unwrap_or_default();
    if !available.contains(DeviceType::KEYBOARD) {
        bail!("the RemoteDesktop portal offers no keyboard");
    }
    let session = proxy.create_session(Default::default()).await?;
    match set_up(&proxy, &session, token_path).await {
        Ok(()) => Ok(Remote { proxy, session }),
        Err(e) => {
            // Never leave a half-set-up session behind on the desktop.
            let _ = tokio::time::timeout(INPUT_TIMEOUT, session.close()).await;
            Err(e)
        }
    }
}

/// SelectDevices (keyboard), Start, and keep the restore token.
async fn set_up(
    proxy: &RemoteDesktop,
    session: &Session<RemoteDesktop>,
    token_path: &Path,
) -> Result<()> {
    let token = tokio::fs::read_to_string(token_path)
        .await
        .ok()
        .map(|t| t.trim().to_owned())
        .filter(|t| !t.is_empty());
    proxy
        .select_devices(
            session,
            SelectDevicesOptions::default()
                .set_devices(DeviceType::KEYBOARD)
                .set_persist_mode(PersistMode::ExplicitlyRevoked)
                .set_restore_token(token.as_deref()),
        )
        .await?
        .response()?;
    let started = proxy
        .start(session, None, Default::default())
        .await?
        .response()
        .context("remote access was cancelled or denied")?;
    if !started.devices().contains(DeviceType::KEYBOARD) {
        bail!("keyboard access was not granted");
    }
    if let Some(t) = started.restore_token() {
        if let Some(dir) = token_path.parent() {
            tokio::fs::create_dir_all(dir).await?;
        }
        tokio::fs::write(token_path, t).await?;
    }
    Ok(())
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

    fn ptr(phase: PointerPhase) -> RemoteInput {
        RemoteInput::Pointer {
            phase,
            x: 1,
            y: 2,
            pressure: None,
        }
    }

    /// Owner report t316u: the fallback injector must never start a second screen-cast
    /// session (a new portal dialog and a competing capture) for pen/touch while the capture
    /// runs. Pointer input is refused with one note; keys still work.
    #[test]
    fn the_fallback_injector_never_opens_a_session_for_the_pointer() {
        let mut p = KeyOnly::default();
        assert_eq!(
            p.action(ptr(PointerPhase::Down)),
            Action::RefusePointer(true)
        );
        assert_eq!(
            p.action(ptr(PointerPhase::Move)),
            Action::RefusePointer(false)
        );
        assert_eq!(
            p.action(ptr(PointerPhase::Up)),
            Action::RefusePointer(false)
        );
        assert_eq!(
            p.action(ptr(PointerPhase::Down)),
            Action::RefusePointer(false)
        );
        let key = RemoteInput::Key(RemoteKey::Next);
        assert_eq!(p.action(key), Action::Key(RemoteKey::Next));
        // After a key, the next pen stroke shows the note again (the line was replaced).
        assert_eq!(
            p.action(ptr(PointerPhase::Down)),
            Action::RefusePointer(true)
        );
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
