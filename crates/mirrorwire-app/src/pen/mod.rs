//! Opt-in pen pressure on the Linux presenter (decisions D26).
//!
//! When "Enable pen pressure" is on, pen input from the viewer goes to a virtual uinput pen
//! ([`uinput::UinputPen`]) that reports `ABS_PRESSURE`, instead of the RemoteDesktop portal
//! pointer (which has no pressure). Keys always stay on the portal. When the setting is off,
//! or the device cannot be created, everything falls back to the portal path with a note in
//! the window.

pub mod check;
pub mod uinput;

use mirrorwire_proto::control::{PointerPhase, RemoteInput};
use std::io;
use std::path::Path;

/// Something that takes pen events (the uinput pen; a mock in tests).
pub trait PenDevice: Send {
    fn send(
        &mut self,
        phase: PointerPhase,
        x: u16,
        y: u16,
        pressure: Option<u16>,
    ) -> io::Result<()>;
}

/// Creates the pen device on the first stroke.
pub type PenOpener = Box<dyn FnMut() -> io::Result<Box<dyn PenDevice>> + Send>;

/// The real opener: a [`uinput::UinputPen`].
pub fn uinput_opener() -> PenOpener {
    Box::new(|| Ok(Box::new(uinput::UinputPen::create()?) as Box<dyn PenDevice>))
}

/// Whether pen pressure is on when the presenter starts: saved as on and the check passes
/// right now (opening `/dev/uinput` is silent; it never prompts).
pub fn enabled_at_start(config_dir: &Path) -> bool {
    mirrorwire_net::settings::pen_pressure_saved(config_dir)
        && check::run(&check::SystemProbe).passed()
}

/// For the terminal presenter: when pen pressure is saved as on but the check fails, the
/// same result and fix text the window shows (AGENTS.md §1.8 is about the GUI; the CLI
/// prints it). `None` when the setting is off or the check passes.
pub fn cli_report(config_dir: &Path) -> Option<String> {
    if !mirrorwire_net::settings::pen_pressure_saved(config_dir) {
        return None;
    }
    let report = check::run(&check::SystemProbe);
    let fix = report.fix()?;
    Some(cli_text(&report, &fix))
}

fn cli_text(report: &check::CheckReport, fix: &check::Fix) -> String {
    let mut out = vec![format!("pen pressure: {}", report.headline())];
    out.extend(report.details().into_iter().map(|l| format!("  {l}")));
    out.push(format!("Why is this needed? {}", check::WHY));
    out.push(format!("What changes: {}", fix.what_changes()));
    out.push("To turn it on, run:".into());
    out.push(fix.commands.clone());
    out.push("To undo:".into());
    out.push(fix.undo.clone());
    out.join("\n")
}

/// Where one input went.
#[derive(Debug, PartialEq, Eq)]
pub struct Routed {
    /// Forward this to the portal injector (keys, and pen input without pressure).
    pub portal: Option<RemoteInput>,
    /// A new line for the window, if the pen state changed.
    pub note: Option<String>,
}

/// Splits remote input between the uinput pen and the portal.
pub struct PenRouter {
    open: PenOpener,
    device: Option<Box<dyn PenDevice>>,
    /// Creating or writing the device failed; stay on the portal until the setting is
    /// switched off and on again.
    failed: bool,
    enabled: bool,
}

/// Window line while the virtual pen is in use.
pub const NOTE_ON: &str = "Pen pressure: on (virtual pen \"MirrorWire virtual pen\")";
/// Window line after switching it off.
pub const NOTE_OFF: &str = "Pen pressure: off (the pen works as a normal pointer)";

impl PenRouter {
    pub fn new(open: PenOpener) -> Self {
        Self {
            open,
            device: None,
            failed: false,
            enabled: false,
        }
    }

    /// Route one input. `enabled` is the current setting (checked and switched on).
    pub fn route(&mut self, input: RemoteInput, enabled: bool) -> Routed {
        let mut note = None;
        if enabled != self.enabled {
            self.enabled = enabled;
            self.failed = false;
            if !enabled && self.device.take().is_some() {
                // Dropping the device lifts a pressed pen and removes it.
                note = Some(NOTE_OFF.to_owned());
            }
        }
        let portal = |note| Routed {
            portal: Some(input),
            note,
        };
        let RemoteInput::Pointer {
            phase,
            x,
            y,
            pressure,
        } = input
        else {
            return portal(note);
        };
        if !enabled || self.failed {
            return portal(note);
        }
        if self.device.is_none() {
            // A stroke that began on the portal finishes there.
            if phase != PointerPhase::Down {
                return portal(note);
            }
            match (self.open)() {
                Ok(d) => {
                    self.device = Some(d);
                    note = Some(NOTE_ON.to_owned());
                }
                Err(e) => {
                    self.failed = true;
                    tracing::warn!("pen pressure: creating the virtual pen failed: {e}");
                    return portal(Some(unavailable(&e)));
                }
            }
        }
        let Some(device) = self.device.as_mut() else {
            return portal(note);
        };
        match device.send(phase, x, y, pressure) {
            Ok(()) => Routed { portal: None, note },
            Err(e) => {
                tracing::warn!("pen pressure: writing to the virtual pen failed: {e}");
                self.device = None;
                self.failed = true;
                portal(Some(unavailable(&e)))
            }
        }
    }
}

fn unavailable(e: &io::Error) -> String {
    format!(
        "Pen pressure is not available ({e}). The pen works without pressure; run \"Check pen \
         pressure support on my system\" again."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    type Log = Arc<Mutex<Vec<(PointerPhase, Option<u16>)>>>;

    struct MockPen {
        log: Log,
        fail_writes: bool,
    }

    impl PenDevice for MockPen {
        fn send(
            &mut self,
            phase: PointerPhase,
            _x: u16,
            _y: u16,
            pressure: Option<u16>,
        ) -> io::Result<()> {
            if self.fail_writes {
                return Err(io::Error::from(io::ErrorKind::BrokenPipe));
            }
            self.log.lock().unwrap().push((phase, pressure));
            Ok(())
        }
    }

    fn router(open_ok: bool, fail_writes: bool) -> (PenRouter, Log, Arc<Mutex<u32>>) {
        let log: Log = Arc::default();
        let opens = Arc::new(Mutex::new(0));
        let (l, o) = (log.clone(), opens.clone());
        let r = PenRouter::new(Box::new(move || {
            *o.lock().unwrap() += 1;
            if open_ok {
                Ok(Box::new(MockPen {
                    log: l.clone(),
                    fail_writes,
                }) as Box<dyn PenDevice>)
            } else {
                Err(io::Error::from(io::ErrorKind::PermissionDenied))
            }
        }));
        (r, log, opens)
    }

    fn ptr(phase: PointerPhase, pressure: Option<u16>) -> RemoteInput {
        RemoteInput::Pointer {
            phase,
            x: 1,
            y: 2,
            pressure,
        }
    }

    #[test]
    fn cli_text_has_the_commands_and_the_warning() {
        let report = check::CheckReport {
            in_flatpak: false,
            node_exists: true,
            module_loaded: true,
            access: check::Access::Denied,
        };
        let fix = report.fix().unwrap();
        let text = cli_text(&report, &fix);
        assert!(text.starts_with("pen pressure: Not available yet"));
        assert!(text.contains(check::RULE));
        assert!(text.contains("keyboards and mice"));
        assert!(text.contains("sudo rm -f /etc/udev/rules.d/70-mirrorwire-uinput.rules"));
    }

    #[test]
    fn off_sends_everything_to_the_portal_and_opens_nothing() {
        let (mut r, log, opens) = router(true, false);
        let down = ptr(PointerPhase::Down, Some(9));
        assert_eq!(
            r.route(down, false),
            Routed {
                portal: Some(down),
                note: None
            }
        );
        assert_eq!(*opens.lock().unwrap(), 0);
        assert!(log.lock().unwrap().is_empty());
    }

    #[test]
    fn on_sends_pen_to_the_device_and_keys_to_the_portal() {
        let (mut r, log, opens) = router(true, false);
        let first = r.route(ptr(PointerPhase::Down, Some(100)), true);
        assert_eq!(first.portal, None);
        assert_eq!(first.note.as_deref(), Some(NOTE_ON));
        assert_eq!(
            r.route(ptr(PointerPhase::Move, Some(200)), true).portal,
            None
        );
        assert_eq!(r.route(ptr(PointerPhase::Up, None), true).portal, None);
        let key = RemoteInput::Key(mirrorwire_proto::control::RemoteKey::Next);
        assert_eq!(r.route(key, true).portal, Some(key));
        assert_eq!(*opens.lock().unwrap(), 1);
        assert_eq!(
            *log.lock().unwrap(),
            vec![
                (PointerPhase::Down, Some(100)),
                (PointerPhase::Move, Some(200)),
                (PointerPhase::Up, None)
            ]
        );
    }

    #[test]
    fn the_device_opens_only_on_a_down() {
        let (mut r, _log, opens) = router(true, false);
        let mv = ptr(PointerPhase::Move, None);
        assert_eq!(r.route(mv, true).portal, Some(mv));
        assert_eq!(*opens.lock().unwrap(), 0);
    }

    #[test]
    fn open_failure_falls_back_with_a_note_and_does_not_retry() {
        let (mut r, _log, opens) = router(false, false);
        let down = ptr(PointerPhase::Down, None);
        let first = r.route(down, true);
        assert_eq!(first.portal, Some(down));
        assert!(first.note.unwrap().contains("not available"));
        assert_eq!(r.route(down, true).portal, Some(down));
        assert_eq!(*opens.lock().unwrap(), 1);
        // Switching off and on again retries once.
        r.route(down, false);
        r.route(down, true);
        assert_eq!(*opens.lock().unwrap(), 2);
    }

    #[test]
    fn write_failure_drops_the_device_and_falls_back() {
        let (mut r, _log, _opens) = router(true, true);
        let down = ptr(PointerPhase::Down, None);
        let routed = r.route(down, true);
        assert_eq!(routed.portal, Some(down));
        assert!(routed.note.unwrap().contains("not available"));
        assert!(r.device.is_none());
    }

    #[test]
    fn switching_off_removes_the_device() {
        let (mut r, _log, _opens) = router(true, false);
        r.route(ptr(PointerPhase::Down, None), true);
        let up = ptr(PointerPhase::Up, None);
        let routed = r.route(up, false);
        assert_eq!(routed.portal, Some(up));
        assert_eq!(routed.note.as_deref(), Some(NOTE_OFF));
        assert!(r.device.is_none());
    }
}
