//! Presenter remote: taps keys (Right, Left, Space, Escape) and moves a pen/finger pointer
//! through the xdg-desktop-portal RemoteDesktop interface. No uinput, no root (decisions D1);
//! the portal asks the user once (the restore token is kept so the dialog does not return) and
//! works inside the Flatpak sandbox. The session opens lazily on the first input, so a presenter
//! that nobody uses as a remote never shows the dialog. A key-only session is upgraded (closed
//! and reopened, one more dialog) when the first pointer event arrives: absolute pointer motion
//! is addressed to a screen-cast stream, so the pointer session also selects a monitor source.
//!
//! This is the fallback when capture and input cannot share one session (`portal.rs`); the
//! pointer works here in Mirror mode only, otherwise only keys do.
//!
//! Stall-proofing (owner report t316u): the task runs on the shared portal runtime
//! (`portal_rt`), never on the network runtime, and the app hands input over with `try_send`
//! only. A permission dialog nobody answers, a refusal or a hung portal call therefore
//! leaves this task waiting while its channel fills and drops input; the control reader and
//! the capture never wait for it. Calls on an open session and closing one are bounded by
//! [`INPUT_TIMEOUT`]; only the user-paced permission dialog itself is not.

mod calls;

pub(super) use self::calls::inject;
use self::calls::{open_session, Remote};
use anyhow::anyhow;
use mirrorwire_proto::control::{PointerPhase, RemoteInput};
use std::path::PathBuf;
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

/// What the injector does with one input (decided apart from the portal, so it is testable).
#[derive(Debug, PartialEq, Eq)]
enum Action {
    /// A mode change is the app's business; it must never open a session here.
    Ignore,
    /// Pen/touch cannot be mapped now; `true` = show the note (once per run of pointer input).
    RefusePointer(bool),
    /// Send it, opening (or upgrading) the session first if needed.
    Send,
}

#[derive(Default)]
struct Policy {
    /// Last pointer refusal shown, so a hovering pen does not rewrite the line 120x/s.
    pointer_refused: bool,
}

impl Policy {
    fn action(&mut self, input: &RemoteInput, pointer_mapped: bool) -> Action {
        match input {
            RemoteInput::PenMode(_) => Action::Ignore,
            RemoteInput::Pointer { .. } if !pointer_mapped => {
                Action::RefusePointer(!std::mem::replace(&mut self.pointer_refused, true))
            }
            _ => {
                self.pointer_refused = false;
                Action::Send
            }
        }
    }
}

/// Start the injector task on the shared portal runtime (`portal_rt`). Send taps into the
/// returned sender (with `try_send`: a task waiting on a permission dialog does not drain it).
pub fn spawn_remote_injector(
    restore_token_path: PathBuf,
    status: RemoteStatusSink,
    asking: AskingSink,
    pointer_mapped: PointerMapped,
) -> std::io::Result<mpsc::Sender<RemoteInput>> {
    let rt = crate::source::portal_rt::portal_runtime()?;
    let (tx, mut rx) = mpsc::channel::<RemoteInput>(64);
    status("presenter remote: waiting for the first button press".into());
    rt.spawn(async move {
        let mut session: Option<Remote> = None;
        let mut policy = Policy::default();
        while let Some(input) = rx.recv().await {
            match policy.action(&input, pointer_mapped()) {
                Action::Ignore => continue,
                Action::RefusePointer(note) => {
                    if note {
                        status(
                            "pen/touch input works in Mirror mode only (the capture session could not be shared with input)".into(),
                        );
                    }
                    continue;
                }
                Action::Send => {}
            }
            let wants_pointer = matches!(input, RemoteInput::Pointer { .. });
            if session
                .as_ref()
                .is_some_and(|s| wants_pointer && s.stream.is_none())
            {
                // Upgrade a key-only session.
                if let Some(old) = session.take() {
                    let _ = tokio::time::timeout(INPUT_TIMEOUT, old.session.close()).await;
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
            let sent = tokio::time::timeout(
                INPUT_TIMEOUT,
                inject(&remote.proxy, &remote.session, remote.stream, input),
            )
            .await
            .unwrap_or_else(|_| Err(anyhow!("the portal did not answer")));
            if let Err(e) = sent {
                tracing::warn!("presenter remote input failed: {e:#}");
                status(format!(
                    "presenter remote: input failed ({e:#}); reconnecting"
                ));
                if let Some(old) = session.take() {
                    let _ = tokio::time::timeout(INPUT_TIMEOUT, old.session.close()).await;
                }
            }
        }
    });
    Ok(tx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::CaptureMode;
    use mirrorwire_proto::control::{pointer_to_stream, RemoteKey};

    fn ptr(phase: PointerPhase) -> RemoteInput {
        RemoteInput::Pointer {
            phase,
            x: 1,
            y: 2,
            pressure: None,
        }
    }

    #[test]
    fn separate_session_maps_mirror_only() {
        assert!(separate_session_maps_pointer(CaptureMode::Mirror));
        assert!(!separate_session_maps_pointer(CaptureMode::Extend));
    }

    /// Extend (pointer not mappable): keys still go through, the pointer is refused with one
    /// note, and nothing here ever opens a session for it. A mode change is ignored.
    #[test]
    fn unmappable_pointer_is_refused_with_one_note_and_keys_still_work() {
        let mut p = Policy::default();
        assert_eq!(
            p.action(&ptr(PointerPhase::Down), false),
            Action::RefusePointer(true)
        );
        assert_eq!(
            p.action(&ptr(PointerPhase::Move), false),
            Action::RefusePointer(false)
        );
        assert_eq!(
            p.action(&ptr(PointerPhase::Up), false),
            Action::RefusePointer(false)
        );
        assert_eq!(p.action(&RemoteInput::PenMode(true), false), Action::Ignore);
        assert_eq!(
            p.action(&RemoteInput::Key(RemoteKey::Next), false),
            Action::Send
        );
        // After a key, the next refusal shows the note again (the line was replaced).
        assert_eq!(
            p.action(&ptr(PointerPhase::Down), false),
            Action::RefusePointer(true)
        );
    }

    /// Mirror: the pointer is sent (opening the pointer session on a press).
    #[test]
    fn mappable_pointer_is_sent() {
        let mut p = Policy::default();
        assert_eq!(p.action(&ptr(PointerPhase::Down), true), Action::Send);
        assert_eq!(p.action(&ptr(PointerPhase::Up), true), Action::Send);
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
