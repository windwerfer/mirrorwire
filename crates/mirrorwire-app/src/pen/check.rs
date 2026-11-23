//! "Check pen pressure support on my system" (decisions D26): can this process open
//! `/dev/uinput` for writing, without root and without a prompt? And if not, which commands
//! would fix it. Pure logic behind [`Probe`], so the tests mock the device open.
//!
//! The commands are shown, never run: the app does not change the system (D14, D26).

use std::io;
use std::path::Path;

/// The kernel's uinput device node (kernel docs, `input/uinput.rst`).
pub const UINPUT_PATH: &str = "/dev/uinput";
/// The rule file the fix panel suggests. `70-` sorts before systemd's `73-seat-late.rules`,
/// which turns the `uaccess` tag into an ACL for the active session's user.
pub const RULE_PATH: &str = "/etc/udev/rules.d/70-mirrorwire-uinput.rules";
/// The udev rule: `uaccess` (systemd sd-login: access follows the active seat session),
/// matched to the one `misc` device named `uinput`. `static_node=` passes the tag on to the
/// static `/dev/uinput` node that loads the module on first access (udev(7)).
pub const RULE: &str =
    r#"KERNEL=="uinput", SUBSYSTEM=="misc", TAG+="uaccess", OPTIONS+="static_node=uinput""#;
/// Loads the module at boot (modules-load.d(5)); only suggested when it is missing.
pub const MODULES_LOAD_PATH: &str = "/etc/modules-load.d/mirrorwire-uinput.conf";
const APP_ID: &str = "io.github.windwerfer.MirrorWire";

/// What the check needs to know about the system. [`SystemProbe`] asks the real one.
pub trait Probe {
    /// Inside a Flatpak sandbox (`/.flatpak-info` exists).
    fn in_flatpak(&self) -> bool;
    /// `/dev/uinput` exists (inside the Flatpak only with `--device=all`).
    fn node_exists(&self) -> bool;
    /// The `uinput` module is loaded or built in (`/sys/module/uinput`).
    fn module_loaded(&self) -> bool;
    /// Open `/dev/uinput` for writing and close it again. Creates no device.
    fn open_for_writing(&self) -> io::Result<()>;
}

/// The real system.
pub struct SystemProbe;

impl Probe for SystemProbe {
    fn in_flatpak(&self) -> bool {
        Path::new("/.flatpak-info").exists()
    }

    fn node_exists(&self) -> bool {
        Path::new(UINPUT_PATH).exists()
    }

    fn module_loaded(&self) -> bool {
        Path::new("/sys/module/uinput").exists()
    }

    fn open_for_writing(&self) -> io::Result<()> {
        // Read + write, as the virtual device is opened later (`evdev::uinput`).
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(UINPUT_PATH)
            .map(drop)
    }
}

/// How the open went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Access {
    /// Opened for writing: pen pressure can be used.
    Ok,
    /// No `/dev/uinput` in this process's view.
    NoDevice,
    /// The node exists but this user may not write to it.
    Denied,
    /// Anything else, with the OS message.
    Failed(String),
}

/// Result of one check, shown as a short list in the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckReport {
    pub in_flatpak: bool,
    pub node_exists: bool,
    pub module_loaded: bool,
    pub access: Access,
}

/// Run the check against `probe`. Never panics, never asks for anything.
pub fn run(probe: &dyn Probe) -> CheckReport {
    let node_exists = probe.node_exists();
    let access = match probe.open_for_writing() {
        Ok(()) => Access::Ok,
        Err(e) => match e.kind() {
            io::ErrorKind::NotFound => Access::NoDevice,
            io::ErrorKind::PermissionDenied => Access::Denied,
            _ => Access::Failed(e.to_string()),
        },
    };
    CheckReport {
        in_flatpak: probe.in_flatpak(),
        node_exists,
        module_loaded: probe.module_loaded(),
        access,
    }
}

impl CheckReport {
    /// Pen pressure can be switched on.
    pub fn passed(&self) -> bool {
        self.access == Access::Ok
    }

    /// One line for the result headline.
    pub fn headline(&self) -> String {
        match &self.access {
            Access::Ok => "Pen pressure is supported: MirrorWire can create a virtual pen.".into(),
            Access::NoDevice if self.in_flatpak => {
                "Not available yet: the Flatpak sandbox hides /dev/uinput.".into()
            }
            Access::NoDevice => "Not available: /dev/uinput does not exist.".into(),
            Access::Denied => "Not available yet: this user may not write to /dev/uinput.".into(),
            Access::Failed(e) => format!("Not available: opening /dev/uinput failed ({e})."),
        }
    }

    /// The checked items, one line each.
    pub fn details(&self) -> Vec<String> {
        let yes_no = |b: bool| if b { "yes" } else { "no" };
        let mut lines = vec![
            format!(
                "Device node /dev/uinput: {}",
                if self.node_exists {
                    "found"
                } else {
                    "not found"
                }
            ),
            format!(
                "Sandbox: {}",
                if self.in_flatpak { "Flatpak" } else { "none" }
            ),
        ];
        // The sandbox does not show /sys/module reliably, so only say "no" outside it.
        if !self.in_flatpak || self.module_loaded {
            lines.push(format!(
                "Kernel module uinput loaded: {}",
                yes_no(self.module_loaded)
            ));
        }
        lines.push(format!(
            "Open for writing (no root, no prompt): {}",
            match &self.access {
                Access::Ok => "ok".to_owned(),
                Access::NoDevice => "no device".to_owned(),
                Access::Denied => "permission denied".to_owned(),
                Access::Failed(e) => e.clone(),
            }
        ));
        lines
    }

    /// The fix panel, or `None` when the check passed.
    pub fn fix(&self) -> Option<Fix> {
        if self.passed() {
            return None;
        }
        // Outside the sandbox a missing node means the module is not loaded (or not built);
        // inside it we cannot tell, so the module lines are only offered as a second step.
        let module = !self.in_flatpak && !self.node_exists && !self.module_loaded;
        Some(Fix::new(self.in_flatpak, module))
    }
}

/// The commands to copy and the honest explanation around them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fix {
    pub in_flatpak: bool,
    /// Shell commands, one per line (comments start with `#`).
    pub commands: String,
    pub undo: String,
}

/// "Why is this needed?"
pub const WHY: &str = "Wayland desktops do not let apps send pressure-sensitive pen input \
directly; the RemoteDesktop portal only knows keyboard, mouse and touch. The kernel's uinput \
creates a virtual drawing tablet instead, and apps like Krita then see pen pressure.";

/// "What changes" (native and AppImage).
pub const WHAT_CHANGES: &str = "Afterwards MirrorWire can create a virtual pen with pressure. \
Warning: the permission is not limited to MirrorWire. Every program running as your user can \
then create virtual input devices, including keyboards and mice, so it could type or click \
for you. Only do this on a computer you trust.";

/// Extra line for the Flatpak.
pub const WHAT_CHANGES_FLATPAK: &str = "In the Flatpak, --device=all also gives MirrorWire \
access to your other devices (for example webcams). Flatpak has no permission for \
/dev/uinput alone.";

impl Fix {
    fn new(in_flatpak: bool, module: bool) -> Self {
        let mut c = Vec::new();
        if module {
            c.push("# Load the uinput kernel module now and at every boot".to_owned());
            c.push("sudo modprobe uinput".to_owned());
            c.push(format!("echo uinput | sudo tee {MODULES_LOAD_PATH}"));
        }
        c.push("# Let the user at this screen write to /dev/uinput".to_owned());
        c.push(format!("echo '{RULE}' | sudo tee {RULE_PATH}"));
        c.push("sudo udevadm control --reload".to_owned());
        c.push("sudo udevadm trigger --action=change --sysname-match=uinput".to_owned());
        if in_flatpak {
            c.push("# Let the MirrorWire Flatpak see /dev/uinput (all devices)".to_owned());
            c.push(format!("flatpak override --user --device=all {APP_ID}"));
            c.push("# Then quit and restart MirrorWire".to_owned());
        }

        let mut u = vec![
            format!("sudo rm -f {RULE_PATH}"),
            "sudo udevadm control --reload".to_owned(),
            "sudo udevadm trigger --action=change --sysname-match=uinput".to_owned(),
        ];
        if module {
            u.push(format!("sudo rm -f {MODULES_LOAD_PATH}"));
        }
        if in_flatpak {
            u.push(
                "# Removes all your overrides for MirrorWire (or use --nodevice=all to undo only this one)"
                    .to_owned(),
            );
            u.push(format!("flatpak override --user --reset {APP_ID}"));
        }
        Self {
            in_flatpak,
            commands: c.join("\n"),
            undo: u.join("\n"),
        }
    }

    /// The "What changes" text for this install.
    pub fn what_changes(&self) -> String {
        if self.in_flatpak {
            format!("{WHAT_CHANGES} {WHAT_CHANGES_FLATPAK}")
        } else {
            WHAT_CHANGES.to_owned()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Mock {
        flatpak: bool,
        node: bool,
        module: bool,
        /// `None`: the open succeeds.
        open: Option<io::ErrorKind>,
    }

    impl Probe for Mock {
        fn in_flatpak(&self) -> bool {
            self.flatpak
        }
        fn node_exists(&self) -> bool {
            self.node
        }
        fn module_loaded(&self) -> bool {
            self.module
        }
        fn open_for_writing(&self) -> io::Result<()> {
            match self.open {
                None => Ok(()),
                Some(kind) => Err(io::Error::from(kind)),
            }
        }
    }

    fn mock(flatpak: bool, node: bool, module: bool, open: Option<io::ErrorKind>) -> CheckReport {
        run(&Mock {
            flatpak,
            node,
            module,
            open,
        })
    }

    #[test]
    fn open_ok_passes_without_a_fix() {
        let r = mock(false, true, true, None);
        assert!(r.passed());
        assert_eq!(r.access, Access::Ok);
        assert_eq!(r.fix(), None);
        assert!(r.headline().contains("supported"));
    }

    #[test]
    fn errors_are_classified() {
        assert_eq!(
            mock(false, true, true, Some(io::ErrorKind::PermissionDenied)).access,
            Access::Denied
        );
        assert_eq!(
            mock(false, false, false, Some(io::ErrorKind::NotFound)).access,
            Access::NoDevice
        );
        let busy = mock(false, true, true, Some(io::ErrorKind::ResourceBusy));
        assert!(matches!(busy.access, Access::Failed(_)));
        assert!(!busy.passed());
        assert!(busy.fix().is_some());
    }

    #[test]
    fn native_denied_fix_is_only_the_udev_rule() {
        let fix = mock(false, true, true, Some(io::ErrorKind::PermissionDenied))
            .fix()
            .unwrap();
        assert_eq!(
            fix.commands,
            "# Let the user at this screen write to /dev/uinput\n\
             echo 'KERNEL==\"uinput\", SUBSYSTEM==\"misc\", TAG+=\"uaccess\", OPTIONS+=\"static_node=uinput\"' | sudo tee /etc/udev/rules.d/70-mirrorwire-uinput.rules\n\
             sudo udevadm control --reload\n\
             sudo udevadm trigger --action=change --sysname-match=uinput"
        );
        assert_eq!(
            fix.undo,
            "sudo rm -f /etc/udev/rules.d/70-mirrorwire-uinput.rules\n\
             sudo udevadm control --reload\n\
             sudo udevadm trigger --action=change --sysname-match=uinput"
        );
        assert!(!fix.commands.contains("flatpak"));
        assert!(!fix.what_changes().contains("--device=all"));
        assert!(fix.what_changes().contains("keyboards and mice"));
    }

    #[test]
    fn native_missing_module_adds_modprobe() {
        let fix = mock(false, false, false, Some(io::ErrorKind::NotFound))
            .fix()
            .unwrap();
        let lines: Vec<&str> = fix.commands.lines().collect();
        assert_eq!(lines.get(1), Some(&"sudo modprobe uinput"));
        assert_eq!(
            lines.get(2),
            Some(&"echo uinput | sudo tee /etc/modules-load.d/mirrorwire-uinput.conf")
        );
        assert!(fix
            .undo
            .contains("sudo rm -f /etc/modules-load.d/mirrorwire-uinput.conf"));
        assert!(!fix.commands.contains("flatpak"));
    }

    #[test]
    fn flatpak_fix_adds_the_override_and_its_undo() {
        let r = mock(true, false, false, Some(io::ErrorKind::NotFound));
        assert!(r.headline().contains("Flatpak"));
        let fix = r.fix().unwrap();
        assert!(fix.in_flatpak);
        // The sandbox cannot see the module state: no modprobe guess.
        assert!(!fix.commands.contains("modprobe"));
        assert!(fix.commands.contains(RULE));
        assert!(fix
            .commands
            .contains("flatpak override --user --device=all io.github.windwerfer.MirrorWire"));
        assert!(fix
            .undo
            .contains("flatpak override --user --reset io.github.windwerfer.MirrorWire"));
        assert!(fix.undo.contains("--nodevice=all"));
        assert!(fix.what_changes().contains("webcams"));
        // The module line is hidden inside the sandbox unless it is seen loaded.
        assert!(!r.details().iter().any(|l| l.contains("module")));
    }

    #[test]
    fn details_list_every_item() {
        let d = mock(false, true, false, Some(io::ErrorKind::PermissionDenied)).details();
        assert_eq!(
            d,
            vec![
                "Device node /dev/uinput: found",
                "Sandbox: none",
                "Kernel module uinput loaded: no",
                "Open for writing (no root, no prompt): permission denied",
            ]
        );
    }
}
