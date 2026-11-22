//! The "View your Android device (Miracast-compatible)" flow as a pure state machine (decisions D21, D22,
//! feature `miracast`). The session thread feeds it the typed events of the Wi-Fi Direct join
//! ([`FlowEvent`]) and of the RTSP/RTP sink ([`SinkEvent`]); the GUI asks [`view`] what to show.
//! No egui here, so every state and message is unit-tested.
//!
//! Order matters on hardware (R1/R2, miracast.md §11): the laptop is only listed on the
//! Android device while our Wi-Fi Direct profile is activating, and that starts only after
//! discovery found the device. So the user first turns on wireless display, and is told to
//! tap this computer only once we are listed ([`Phase::Joining`]).

use miracast_loopback::nm_flow::FlowEvent;
use miracast_loopback::{SinkEvent, SinkReport};
use miracast_nm::{P2pError, SinkName};
use miracast_rtp::diag::NoRtp;

/// Where the Miracast flow is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    /// Button pressed, NetworkManager not reached yet.
    Starting,
    /// Discovery runs; the Android device must turn on wireless display.
    Searching,
    /// Found `peer`; our profile (with the WFD IEs) is activating, so the laptop is listed
    /// there and the user must tap it. `attempt` counts group formations (retried on failure).
    Joining { peer: String, attempt: u32 },
    /// Group up; connecting to the device's RTSP server.
    Connecting { peer: String },
    /// RTSP done, PLAY sent. `warning` is a problem that does not end the session (no RTP yet,
    /// or the video stalled).
    Streaming { warning: Option<Issue> },
    /// The flow failed; the GUI offers Try again.
    Failed(Issue),
    /// Casting ended normally: stopped on the Android device (`by_source`) or by Cancel.
    Ended { by_source: bool },
}

/// A problem the user has to understand (AGENTS.md 1.8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Issue {
    /// No NetworkManager, no Wi-Fi P2P device, or (Flatpak) NM cannot be reached.
    NotAvailable(String),
    /// NetworkManager (Polkit) refused the Wi-Fi Direct request.
    NotAllowed(String),
    /// Discovery found no Android device.
    NoSource,
    /// The Wi-Fi Direct group did not form (also after the automatic retries).
    GroupFailed(String),
    /// The group formed but the device never started its stream (RTSP server).
    SourceNeverStarted,
    /// RTSP worked but no video arrives: almost always the firewall.
    NoRtp(NoRtp),
    /// Video stopped: the link was lost.
    VideoStopped,
    /// The session ended for another reason.
    Other(String),
}

impl Phase {
    /// Progress of the Wi-Fi Direct join.
    pub fn on_flow(&self, e: &FlowEvent) -> Phase {
        match e {
            FlowEvent::Discovering => Phase::Searching,
            FlowEvent::SourceFound { name, .. } => Phase::Joining {
                peer: name.clone(),
                attempt: 1,
            },
            FlowEvent::AttemptFailed { attempt, .. } => Phase::Joining {
                peer: self.peer().unwrap_or_default(),
                attempt: attempt + 1,
            },
            FlowEvent::Joined(_) => Phase::Connecting {
                peer: self.peer().unwrap_or_default(),
            },
        }
    }

    /// The join failed: what it means depends on how far it got.
    pub fn on_join_error(&self, e: &P2pError) -> Phase {
        Phase::Failed(match (self, e) {
            (_, P2pError::NotAvailable(m)) => Issue::NotAvailable(m.clone()),
            (_, P2pError::PermissionDenied(m)) => Issue::NotAllowed(m.clone()),
            (Phase::Starting | Phase::Searching, P2pError::Timeout(_)) => Issue::NoSource,
            (_, e) => Issue::GroupFailed(e.to_string()),
        })
    }

    /// Events of the running stream.
    pub fn on_sink(&self, e: &SinkEvent) -> Phase {
        match (self, e) {
            (_, SinkEvent::MediaStarted { .. }) => Phase::Streaming { warning: None },
            (Phase::Streaming { .. }, SinkEvent::NoRtp(d)) => Phase::Streaming {
                warning: Some(Issue::NoRtp(d.clone())),
            },
            (Phase::Streaming { .. }, SinkEvent::Stalled(_)) => Phase::Streaming {
                warning: Some(Issue::VideoStopped),
            },
            (p, _) => p.clone(),
        }
    }

    /// How the stream ended; `cancelled` = the user pressed Cancel/Back or closed the window.
    pub fn on_end(&self, report: &SinkReport, cancelled: bool) -> Phase {
        if cancelled {
            Phase::Ended { by_source: false }
        } else if report.no_rtsp {
            Phase::Failed(Issue::SourceNeverStarted)
        } else if report.link_lost {
            Phase::Failed(Issue::VideoStopped)
        } else if report.close_reason.as_deref() == Some("stopped by the user") {
            // The Android device's DISCONNECT (its TEARDOWN trigger).
            Phase::Ended { by_source: true }
        } else {
            Phase::Failed(Issue::Other(
                report
                    .close_reason
                    .clone()
                    .unwrap_or_else(|| "the stream ended".into()),
            ))
        }
    }

    fn peer(&self) -> Option<String> {
        match self {
            Phase::Joining { peer, .. } | Phase::Connecting { peer } => Some(peer.clone()),
            _ => None,
        }
    }

    /// True while the flow still runs (Cancel makes sense).
    pub fn is_active(&self) -> bool {
        !matches!(self, Phase::Failed(_) | Phase::Ended { .. })
    }
}

/// What the GUI shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum View {
    /// Instruction or progress, with a spinner and Cancel.
    Wait { title: String, body: String },
    /// The picture (the normal viewer).
    Video,
    /// Finished or failed, with Try again and Back.
    Done { title: String, body: String },
}

/// The screen for `phase`; `device` is the name the Android device most likely lists this
/// computer under (D25), `has_picture` whether a decoded picture exists.
pub fn view(phase: &Phase, device: &SinkName, has_picture: bool) -> View {
    let device = &tap_target(device);
    let wait = |title: &str, body: String| View::Wait {
        title: title.into(),
        body,
    };
    let open_cast = "On your Android device open Settings > Cast (Wireless display) and turn \
                     wireless display on. Keep that screen open.";
    match phase {
        Phase::Starting | Phase::Searching => wait(
            "Waiting for your Android device",
            format!("{open_cast}\n\nLooking for it…"),
        ),
        Phase::Joining { peer, attempt } => {
            let mut body = format!(
                "On {peer}, tap {device} in the Cast list (it may be listed as \
                 \"Wireless display\").\n\nIf {peer} asks whether to connect, choose Cancel \
                 and tap it in the list instead."
            );
            if *attempt > 1 {
                body.push_str(&format!(
                    "\n\nThe connection did not form; {peer} will ask again (try {attempt} of 3)."
                ));
            }
            wait("Now tap this computer on your Android device", body)
        }
        Phase::Connecting { peer } => wait(
            "Connecting",
            format!("Connected to {peer}. Starting the stream…"),
        ),
        Phase::Streaming { .. } if has_picture => View::Video,
        Phase::Streaming {
            warning: Some(issue),
        } => wait("No video yet", message(issue, device)),
        Phase::Streaming { warning: None } => wait(
            "Starting the stream",
            "Waiting for the first picture…".into(),
        ),
        Phase::Failed(issue) => View::Done {
            title: title(issue).into(),
            body: message(issue, device),
        },
        Phase::Ended { by_source: true } => View::Done {
            title: "Casting stopped".into(),
            body: "Your Android device stopped casting.".into(),
        },
        Phase::Ended { by_source: false } => View::Done {
            title: "Stopped".into(),
            body: "Receiving from your Android device was stopped.".into(),
        },
    }
}

fn title(issue: &Issue) -> &'static str {
    match issue {
        Issue::NotAvailable(_) => "Receiving from Android is not available",
        Issue::NotAllowed(_) => "Receiving from Android was not allowed",
        Issue::NoSource => "No Android device found",
        Issue::GroupFailed(_) => "The connection could not be set up",
        Issue::SourceNeverStarted => "Your Android device did not start casting",
        Issue::NoRtp(_) => "No video arrives",
        Issue::VideoStopped => "Video stopped",
        Issue::Other(_) => "The stream ended",
    }
}

/// The plain-language explanation of `issue`.
fn message(issue: &Issue, device: &str) -> String {
    match issue {
        Issue::NotAvailable(m) => not_available(m, in_flatpak()),
        Issue::NotAllowed(m) => format!(
            "NetworkManager refused the Wi-Fi Direct request. Try again and allow it if a \
             password prompt appears; on a managed computer the administrator may have to \
             allow NetworkManager network control. ({m})"
        ),
        Issue::NoSource => "No Android device was found. Turn wireless display on in Settings > \
                            Cast on your Android device, keep that screen open, and try again."
            .into(),
        Issue::GroupFailed(m) => format!(
            "The Wi-Fi Direct connection to your Android device did not form. Try again; tap \
             {device} in the Cast list when it appears. ({m})"
        ),
        Issue::SourceNeverStarted => format!(
            "Your Android device joined, but it did not start casting. On the device, open \
             Cast and tap {device} in the list; answering its \"connect\" question alone \
             is not enough."
        ),
        Issue::NoRtp(d) => {
            format!(
            "Your Android device accepted the connection, but no video arrives on UDP port {}. \
             The firewall probably blocks it ({} is {}in firewalld zone \"{}\"). To allow it \
             until the next reboot, run: {}",
            d.rtp_port,
            d.interface.as_deref().unwrap_or("the Wi-Fi Direct connection"),
            if d.zone.is_some() { "" } else { "most likely " },
            d.zone_name(),
            d.fix_command()
        )
        }
        Issue::VideoStopped => "Video stopped: the Wi-Fi Direct connection to your Android \
                                device was lost. This is more likely when the laptop's Wi-Fi \
                                uses another channel than the Android device's. Try again."
            .into(),
        Issue::Other(reason) => format!("The stream ended: {reason}."),
    }
}

/// How the instructions name this computer in the Android Cast list.
fn tap_target(name: &SinkName) -> String {
    match name {
        SinkName::Named(n) => format!("\"{n}\""),
        SinkName::Empty => "the entry without a name".into(),
        SinkName::Unknown => "this computer".into(),
    }
}

/// The "Name shown on Android" line of the Miracast screen (D25); `None` when the name is
/// not known (show nothing rather than a wrong "none"). The bool says whether the name is
/// missing, so the GUI adds the ℹ explanation ([`NAME_HELP`]).
pub fn name_line(name: &SinkName) -> Option<(String, bool)> {
    match name {
        SinkName::Named(n) => Some((format!("Name shown on Android: {n}"), false)),
        SinkName::Empty => Some(("Name shown on Android: none".into(), true)),
        SinkName::Unknown => None,
    }
}

/// Why the name is empty and how to set it (behind the ℹ of [`name_line`]). "Most likely":
/// the name comes from the system, and its link to the hostname is a best estimate (D25).
pub const NAME_HELP: &str = "Android lists this computer under its Wi-Fi Direct name. \
     The system sets that name, not MirrorWire, most likely from the computer's hostname, and \
     no hostname is set here. To set one, run in a terminal (it may ask for your password):\n\n\
     hostnamectl hostname --pretty \"My Laptop\"\n\
     hostnamectl hostname --static my-laptop\n\n\
     Then turn Wi-Fi off and on so the new name is used.";

/// Text for [`Issue::NotAvailable`]. In the Flatpak, NM is reachable through the manifest's
/// `--system-talk-name=org.freedesktop.NetworkManager` (D24); a user who removed it with an
/// override gets the same "cannot be reached" error, so the restore command is added.
fn not_available(m: &str, flatpak: bool) -> String {
    let mut text = format!(
        "This computer cannot use Wi-Fi Direct through NetworkManager right now. Receiving \
         from Android needs NetworkManager with Wi-Fi Direct (P2P) support. ({m})"
    );
    if flatpak {
        text.push_str(
            "\n\nIf MirrorWire's NetworkManager permission was removed (Flatseal or flatpak \
             override), give it back with: flatpak override --user \
             --system-talk-name=org.freedesktop.NetworkManager io.github.windwerfer.MirrorWire",
        );
    }
    text
}

/// True inside a Flatpak sandbox (the runtime always provides `/.flatpak-info`).
fn in_flatpak() -> bool {
    std::path::Path::new("/.flatpak-info").exists()
}

#[cfg(test)]
mod tests {
    use super::*;
    use miracast_nm::{GroupInfo, GroupRole};
    use std::net::Ipv4Addr;
    use std::time::Duration;

    fn dev() -> SinkName {
        SinkName::Named("t480s".into())
    }

    fn group() -> GroupInfo {
        GroupInfo {
            interface: "p2p-wlp58s0-3".into(),
            role: GroupRole::Client,
            local_ip: Ipv4Addr::new(192, 168, 49, 74),
            peer_ip: Some(Ipv4Addr::new(192, 168, 49, 1)),
        }
    }

    fn no_rtp() -> NoRtp {
        NoRtp {
            rtp_port: 19000,
            waited: Duration::from_secs(5),
            interface: Some("p2p-wlp58s0-3".into()),
            zone: None,
        }
    }

    fn wait_body(v: View) -> String {
        match v {
            View::Wait { body, .. } => body,
            other => panic!("expected Wait, got {other:?}"),
        }
    }

    fn done(v: View) -> (String, String) {
        match v {
            View::Done { title, body } => (title, body),
            other => panic!("expected Done, got {other:?}"),
        }
    }

    #[test]
    fn happy_path_from_button_to_video() {
        let p = Phase::Starting;
        assert!(wait_body(view(&p, &dev(), false)).contains("turn wireless display on"));
        let p = p.on_flow(&FlowEvent::Discovering);
        assert_eq!(p, Phase::Searching);
        let p = p.on_flow(&FlowEvent::SourceFound {
            name: "Android_qqQ0".into(),
            hw_address: "7A:1B:85:73:E1:76".into(),
            rtsp_port: 7236,
        });
        let body = wait_body(view(&p, &dev(), false));
        assert!(body.contains("tap \"t480s\""), "{body}");
        assert!(body.contains("choose Cancel"), "{body}");
        let p = p.on_flow(&FlowEvent::AttemptFailed {
            attempt: 1,
            attempts: 3,
            reason: "did not form".into(),
        });
        assert_eq!(
            p,
            Phase::Joining {
                peer: "Android_qqQ0".into(),
                attempt: 2
            }
        );
        assert!(wait_body(view(&p, &dev(), false)).contains("try 2 of 3"));
        let p = p.on_flow(&FlowEvent::Joined(group()));
        assert_eq!(
            p,
            Phase::Connecting {
                peer: "Android_qqQ0".into()
            }
        );
        let p = p.on_sink(&SinkEvent::Connected("192.168.49.1:7236".parse().unwrap()));
        assert!(matches!(p, Phase::Connecting { .. }));
        let p = p.on_sink(&SinkEvent::MediaStarted { rtp_port: 19000 });
        assert!(matches!(view(&p, &dev(), false), View::Wait { .. }));
        assert_eq!(view(&p, &dev(), true), View::Video);
        assert!(p.is_active());
    }

    #[test]
    fn firewall_message_names_port_zone_and_fix_and_keeps_waiting() {
        let p = Phase::Streaming { warning: None }.on_sink(&SinkEvent::NoRtp(no_rtp()));
        assert!(
            p.is_active(),
            "the session stays up: the user may open the port"
        );
        let body = wait_body(view(&p, &dev(), false));
        assert!(body.contains("UDP port 19000"), "{body}");
        assert!(
            body.contains("most likely in firewalld zone \"public\""),
            "{body}"
        );
        assert!(
            body.contains("firewall-cmd --zone=public --add-port=19000/udp"),
            "{body}"
        );
        // Once video arrives it is shown.
        assert_eq!(view(&p, &dev(), true), View::Video);
    }

    #[test]
    fn source_that_never_starts_gets_the_tap_hint() {
        let report = SinkReport {
            no_rtsp: true,
            ..SinkReport::default()
        };
        let p = Phase::Connecting { peer: "A".into() }.on_end(&report, false);
        assert_eq!(p, Phase::Failed(Issue::SourceNeverStarted));
        let (title, body) = done(view(&p, &dev(), false));
        assert!(title.contains("did not start casting"));
        assert!(body.contains("tap \"t480s\""), "{body}");
        assert!(!p.is_active());
    }

    #[test]
    fn lost_link_is_video_stopped_even_with_a_picture() {
        let stalled = Phase::Streaming { warning: None }.on_sink(&SinkEvent::Stalled(
            miracast_rtp::diag::RtpStalled {
                rtp_port: 19000,
                silent_for: Duration::from_secs(5),
            },
        ));
        assert_eq!(
            stalled,
            Phase::Streaming {
                warning: Some(Issue::VideoStopped)
            }
        );
        let report = SinkReport {
            link_lost: true,
            close_reason: Some("stopped by the user".into()),
            ..SinkReport::default()
        };
        let p = stalled.on_end(&report, false);
        assert_eq!(p, Phase::Failed(Issue::VideoStopped));
        let (title, body) = done(view(&p, &dev(), true));
        assert_eq!(title, "Video stopped");
        assert!(body.contains("connection to your Android device was lost"));
    }

    #[test]
    fn endings_by_the_device_and_by_cancel() {
        let by_device = SinkReport {
            close_reason: Some("stopped by the user".into()),
            ..SinkReport::default()
        };
        let p = Phase::Streaming { warning: None };
        assert_eq!(
            p.on_end(&by_device, false),
            Phase::Ended { by_source: true }
        );
        assert_eq!(
            p.on_end(&by_device, true),
            Phase::Ended { by_source: false }
        );
        let other = SinkReport {
            close_reason: Some("the source stopped talking (RTSP keepalive timeout)".into()),
            ..SinkReport::default()
        };
        let (_, body) = done(view(&p.on_end(&other, false), &dev(), false));
        assert!(body.contains("keepalive timeout"));
    }

    #[test]
    fn join_errors_depend_on_how_far_the_flow_got() {
        let timeout = P2pError::Timeout("x".into());
        assert_eq!(
            Phase::Searching.on_join_error(&timeout),
            Phase::Failed(Issue::NoSource)
        );
        let joining = Phase::Joining {
            peer: "A".into(),
            attempt: 3,
        };
        assert!(matches!(
            joining.on_join_error(&timeout),
            Phase::Failed(Issue::GroupFailed(_))
        ));
        let denied = P2pError::PermissionDenied("polkit".into());
        let p = Phase::Starting.on_join_error(&denied);
        assert!(matches!(p, Phase::Failed(Issue::NotAllowed(_))));
        let (title, body) = done(view(&p, &dev(), false));
        assert!(title.contains("not allowed"));
        assert!(body.contains("polkit") && body.contains("refused"));
        let missing = P2pError::NotAvailable("no NM".into());
        let p = Phase::Starting.on_join_error(&missing);
        assert!(matches!(p, Phase::Failed(Issue::NotAvailable(_))));
        assert!(done(view(&p, &dev(), false)).1.contains("no NM"));
        assert!(done(view(&Phase::Failed(Issue::NoSource), &dev(), false))
            .1
            .contains("wireless display on"));
    }

    #[test]
    fn name_line_and_tap_text_follow_the_sink_name() {
        let joining = Phase::Joining {
            peer: "A".into(),
            attempt: 1,
        };
        // Known name: shown, no hint, and the instruction uses it.
        assert_eq!(
            name_line(&dev()),
            Some(("Name shown on Android: t480s".into(), false))
        );
        // Empty (the T480s case): "none" with the hint, and the blank entry is described.
        let (line, hint) = name_line(&SinkName::Empty).unwrap();
        assert_eq!(line, "Name shown on Android: none");
        assert!(hint);
        let body = wait_body(view(&joining, &SinkName::Empty, false));
        assert!(body.contains("tap the entry without a name"), "{body}");
        // Unknown (e.g. Flatpak without hostname1): no line at all, neutral instruction.
        assert_eq!(name_line(&SinkName::Unknown), None);
        let body = wait_body(view(&joining, &SinkName::Unknown, false));
        assert!(body.contains("tap this computer"), "{body}");
        let (_, body) = done(view(
            &Phase::Failed(Issue::SourceNeverStarted),
            &SinkName::Empty,
            false,
        ));
        assert!(body.contains("tap the entry without a name"), "{body}");
    }

    #[test]
    fn name_help_gives_the_hostnamectl_commands() {
        assert!(NAME_HELP.contains("hostnamectl hostname --pretty \"My Laptop\""));
        assert!(NAME_HELP.contains("hostnamectl hostname --static my-laptop"));
        assert!(NAME_HELP.contains("Wi-Fi off and on"));
        assert!(NAME_HELP.contains("most likely"));
    }

    #[test]
    fn flatpak_hint_only_inside_the_sandbox() {
        let native = not_available("not running", false);
        assert!(native.contains("not running") && !native.contains("flatpak override"));
        let sandboxed = not_available("not running", true);
        assert!(sandboxed.contains(
            "flatpak override --user --system-talk-name=org.freedesktop.NetworkManager \
             io.github.windwerfer.MirrorWire"
        ));
    }

    #[test]
    fn events_out_of_order_do_not_jump_into_streaming_warnings() {
        // A NoRtp before media started (cannot happen today) leaves the phase alone.
        let p = Phase::Connecting { peer: "A".into() };
        assert_eq!(p.on_sink(&SinkEvent::NoRtp(no_rtp())), p);
        assert_eq!(p.on_sink(&SinkEvent::Stats("stats: 5s".into())), p);
    }
}
