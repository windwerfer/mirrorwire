use crate::applog::LogStore;
use crate::capture_prefs::{
    CapturePrefs, CursorPref, ExtendSize, FpsPref, EXTEND_SIZES, FEC_CHOICES,
};
use crate::presenter::{self, PresentPhase, PresenterHandle};
use crate::sync::LockExt;
use crate::viewer::{self, viewer_ui, Picture, ViewerHandle, ViewerOptions, ViewerUi};
use crate::yuv_glow::{YuvPaint, YuvRenderer};
use crate::{GuiLaunch, RendererChoice, ViewArgs};
use anyhow::{anyhow, Result};
use eframe::egui;
use mirrorwire_media::encode::{
    EncoderBackend, EncoderConfig, RateControl, DEFAULT_BITRATE_KBPS, DEFAULT_QP,
};
use mirrorwire_net::discovery::{Browser, Presenter};
use mirrorwire_net::identity::{short_fingerprint, Identity};
use mirrorwire_net::pairing::ConnectInfo;
use mirrorwire_proto::control::CursorMode;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

mod chrome;
/// Shown under the encoder row while RC is CQP or ICQ: those modes hold a fixed quality, and
/// the pipeline leaves the encoder alone when the adaptive bitrate moves its cap
/// (`pipeline::maybe_replace_encoder`, `quality_mode`). Only a change of the user's own
/// setting applies. FEC and pacing still follow the link.
#[cfg(all(feature = "miracast", target_os = "linux"))]
mod miracast;
#[cfg(target_os = "linux")]
mod pen;
mod present;
mod scan;
mod video;
mod widgets;
use widgets::*;

const CQP_ICQ_NOTE: &str = "CQP/ICQ keep a fixed quality: the adaptive bitrate does not change \
the encoder's rate in these modes (it still adjusts FEC and pacing), so a slow link can \
overflow. Use Auto, CBR or VBR to follow the link.";

const PRESENT_AVAILABLE: bool = cfg!(all(target_os = "linux", feature = "portal"));
/// This build has FFmpeg (VAAPI / x264 / VideoToolbox encoders).
const HAS_FFMPEG: bool = cfg!(feature = "ffmpeg");
/// This build has the pure-Rust FilmCraft encoder.
const HAS_FILMCRAFT: bool = cfg!(feature = "filmcraft");

/// Encoder the GUI presenter starts with: FilmCraft when the build has no FFmpeg (the only
/// encoder then) or when the user chose it under Advanced, otherwise `auto`.
fn start_encoder(has_ffmpeg: bool, has_filmcraft: bool, use_filmcraft: bool) -> EncoderBackend {
    if !has_ffmpeg || (has_filmcraft && use_filmcraft) {
        EncoderBackend::Filmcraft
    } else {
        EncoderBackend::Auto
    }
}

/// The line under the "Use the FilmCraft encoder" checkbox.
fn filmcraft_note(has_ffmpeg: bool, has_filmcraft: bool) -> &'static str {
    match (has_ffmpeg, has_filmcraft) {
        (true, true) => {
            "Off: VAAPI, then x264 (FFmpeg). On: FilmCraft, slower but needs no FFmpeg or GPU \
             driver; it does not scale. Still switchable in the presenter's Quality panel."
        }
        (false, true) => "This build has no FFmpeg, so FilmCraft is the only encoder.",
        (true, false) => "Not in this build (cargo feature `filmcraft`).",
        (false, false) => {
            "This build has neither FFmpeg nor FilmCraft: it cannot encode. Rebuild with \
             `--features filmcraft`."
        }
    }
}

/// Background result of the "Open port for me…" button.
enum FirewallFix {
    Running,
    Ok(String),
    Applied(String),
    NotChanged,
    Err(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResolvedRenderer {
    Glow,
    Wgpu,
}

impl ResolvedRenderer {
    fn label(self) -> &'static str {
        match self {
            Self::Glow => "OpenGL (glow)",
            Self::Wgpu => "wgpu",
        }
    }

    fn eframe(self) -> eframe::Renderer {
        match self {
            Self::Glow => eframe::Renderer::Glow,
            Self::Wgpu => eframe::Renderer::Wgpu,
        }
    }
}

/// The start-screen button text (decision D22: "Miracast" only descriptively).
pub(crate) const MIRACAST_BUTTON: &str = "View your Android device (Miracast-compatible)";

enum Screen {
    Home,
    Present,
    View,
    /// "View your Android device (Miracast-compatible)" (D21, D22; feature `miracast`).
    #[cfg_attr(not(all(feature = "miracast", target_os = "linux")), allow(dead_code))]
    Miracast,
}

/// The running Miracast flow next to `watching` (feature `miracast`; `()` without it).
#[cfg(all(feature = "miracast", target_os = "linux"))]
type MiracastState = crate::miracast::MiracastSession;
#[cfg(not(all(feature = "miracast", target_os = "linux")))]
type MiracastState = ();

pub fn run(
    launch: GuiLaunch,
    args: ViewArgs,
    config_dir: &Path,
    renderer: RendererChoice,
    log: LogStore,
    present_port: u16,
) -> Result<()> {
    let identity = Arc::new(Identity::load_or_create(config_dir)?);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let handle = runtime.handle().clone();
    let initial = args.uri.as_deref().map(ConnectInfo::parse).transpose()?;
    let name = args.name.clone().unwrap_or_else(crate::default_name);
    let config_dir = config_dir.to_path_buf();
    let start_in_view = matches!(launch, GuiLaunch::View);
    let miracast = args.miracast.then(|| args.miracast_peer.clone());

    let preferred = match renderer {
        RendererChoice::Gl => ResolvedRenderer::Glow,
        RendererChoice::Wgpu => ResolvedRenderer::Wgpu,
        RendererChoice::Auto => ResolvedRenderer::Wgpu,
    };
    let allow_fallback = !matches!(renderer, RendererChoice::Gl);

    tracing::info!(
        "GUI renderer preference: {:?} (YUV→RGB on the CPU via swscale; OpenGL default on macOS x86_64)",
        renderer
    );

    let make_app = {
        let identity = identity.clone();
        let config_dir = config_dir.clone();
        let handle = handle.clone();
        let name = name.clone();
        let initial = initial.clone();
        let miracast = miracast.clone();
        let log = log.clone();
        let hwdec = args.want_hwdec();
        let software_decoder = args.software_decoder();
        move |used: ResolvedRenderer| {
            let identity = identity.clone();
            let config_dir = config_dir.clone();
            let handle = handle.clone();
            let name = name.clone();
            let initial = initial.clone();
            let miracast = miracast.clone();
            let log = log.clone();
            Box::new(move |cc: &eframe::CreationContext<'_>| {
                let capture =
                    crate::capture_prefs::resolve(None, None, None, None, None, &config_dir);
                #[cfg(target_os = "linux")]
                let pen = pen::PenUi::load(&config_dir);
                let use_filmcraft =
                    !HAS_FFMPEG || mirrorwire_net::settings::filmcraft_encoder_saved(&config_dir);
                let show_stats = mirrorwire_net::settings::show_stats_saved(&config_dir);
                let mut app = MirrorApp {
                    identity,
                    runtime: handle,
                    config_dir,
                    browser: Browser::start()
                        .map_err(|e| tracing::warn!("mDNS browse unavailable: {e:#}"))
                        .ok(),
                    hwdec,
                    software_decoder,
                    name,
                    uri_input: String::new(),
                    scan: None,
                    code_input: String::new(),
                    error: None,
                    watching: None,
                    presenting: None,
                    texture: None,
                    qr_texture: None,
                    overlay: chrome::Overlay {
                        hidden: false,
                        show_stats,
                        powerpoint: false,
                    },
                    overlay_note: None,
                    pen_mode: false,
                    view_mode: ViewMode::Video,
                    video_paused_sent: None,
                    sampler: mirrorwire_proto::control::PenSampler::default(),
                    pen_pressure: None,
                    fullscreen: false,
                    display_latency_ms: 0.0,
                    screen: if start_in_view {
                        Screen::View
                    } else {
                        Screen::Home
                    },
                    renderer: used,
                    can_go_home: !start_in_view,
                    log,
                    last_target: None,
                    present_port,
                    pixel_exact: false,
                    yuv: None,
                    yuv_gpu: Arc::new(Mutex::new(None)),
                    copy_note: None,
                    display_server: None,
                    bitrate_mbps: DEFAULT_BITRATE_KBPS as f64 / 1000.0,
                    rc: RateControl::Auto,
                    qp: DEFAULT_QP,
                    keyint_sec: 10,
                    encoder: EncoderBackend::Auto,
                    use_filmcraft,
                    pen_rights_open: false,
                    pen_rights_note: None,
                    firewall_fix: None,
                    capture,
                    cursor_tex: None,
                    cursor_rate: (Instant::now(), 0, 0.0),
                    miracast: None,
                    miracast_session: None,
                    miracast_name_help: false,
                    #[cfg(target_os = "linux")]
                    pen,
                };
                if let Some(target) = initial {
                    app.screen = Screen::View;
                    app.connect(&cc.egui_ctx, target);
                } else if let Some(peer) = miracast {
                    app.screen = Screen::View;
                    app.connect_miracast(&cc.egui_ctx, peer);
                }
                Ok(Box::new(app) as Box<dyn eframe::App>)
            }) as eframe::AppCreator<'_>
        }
    };

    let result = try_run_native(preferred, make_app(preferred));
    let result = match result {
        Err(e) if allow_fallback && preferred == ResolvedRenderer::Wgpu => {
            tracing::warn!("wgpu renderer failed ({e}); falling back to OpenGL (glow)");
            try_run_native(ResolvedRenderer::Glow, make_app(ResolvedRenderer::Glow))
        }
        other => other,
    };

    drop(runtime);
    result
}

fn try_run_native(renderer: ResolvedRenderer, creator: eframe::AppCreator<'_>) -> Result<()> {
    tracing::info!("starting GUI with {}", renderer.label());
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("MirrorWire")
            .with_inner_size([1280.0, 720.0]),
        renderer: renderer.eframe(),
        ..Default::default()
    };
    eframe::run_native("MirrorWire", options, creator).map_err(|e| anyhow!("gui: {e}"))
}

struct MirrorApp {
    identity: Arc<Identity>,
    runtime: tokio::runtime::Handle,
    config_dir: PathBuf,
    browser: Option<Browser>,
    hwdec: bool,
    software_decoder: mirrorwire_media::decode::SoftwareDecoder,
    name: String,
    uri_input: String,
    /// Running camera QR scan (the Scan button), if any.
    scan: Option<scan::Scan>,
    code_input: String,
    error: Option<String>,
    watching: Option<ViewerHandle>,
    presenting: Option<PresenterHandle>,
    texture: Option<egui::TextureHandle>,
    qr_texture: Option<egui::TextureHandle>,
    /// Video overlay: Hide, Show statistics (saved; S toggles), PowerPoint mode (the
    /// presenter-remote row; R toggles).
    overlay: chrome::Overlay,
    /// Why the Show statistics choice could not be saved (shown in the menu).
    overlay_note: Option<String>,
    /// Pen/touch/mouse over the picture drives the presenter's pointer (off by default, so a
    /// normal tap does not click on the presenter).
    pen_mode: bool,
    /// ☰ menu: show the video, or only a pen surface / the remote buttons (no video).
    view_mode: ViewMode,
    /// `video_paused` last sent on this connection (`None` = not yet: send on connect).
    video_paused_sent: Option<bool>,
    /// Stroke state and hover/move coalescing for the pen (`mirrorwire_proto::control`).
    sampler: mirrorwire_proto::control::PenSampler,
    /// Last pen/touch force seen while pen mode is on (`None`: no sensor, e.g. a mouse).
    pen_pressure: Option<u16>,
    fullscreen: bool,
    display_latency_ms: f64,
    screen: Screen,
    renderer: ResolvedRenderer,
    can_go_home: bool,
    log: LogStore,
    last_target: Option<ConnectInfo>,
    present_port: u16,
    pixel_exact: bool,
    yuv: Option<Arc<mirrorwire_media::decode::YuvImage>>,
    yuv_gpu: Arc<Mutex<Option<YuvRenderer>>>,
    copy_note: Option<(Instant, bool, String)>,
    /// "Wayland (native)", "X11 (XWayland)", "macOS"…, read from the real window handle.
    display_server: Option<String>,
    bitrate_mbps: f64,
    rc: RateControl,
    qp: u32,
    keyint_sec: u32,
    encoder: EncoderBackend,
    /// Start screen › Advanced: present with the FilmCraft encoder (saved). Always on in a
    /// build without FFmpeg, where FilmCraft is the only encoder.
    use_filmcraft: bool,
    /// Start screen: the ✏ info box about the remembered pen/keys permission is open.
    #[cfg_attr(not(all(target_os = "linux", feature = "portal")), allow(dead_code))]
    pen_rights_open: bool,
    /// Result of the last "Forget" click, shown in that box.
    #[cfg_attr(not(all(target_os = "linux", feature = "portal")), allow(dead_code))]
    pen_rights_note: Option<String>,
    firewall_fix: Option<Arc<Mutex<FirewallFix>>>,
    /// Presenter cursor mode and frame-rate cap (Quality panel).
    capture: CapturePrefs,
    /// Viewer: texture of the separately sent cursor, by image id.
    cursor_tex: Option<(u32, egui::TextureHandle)>,
    /// Viewer: (window start, updates at start, updates/s) for the HUD.
    cursor_rate: (Instant, u64, f64),
    /// Watching a Miracast source (`view --miracast`), with its peer-name filter; for Retry.
    miracast: Option<Option<String>>,
    /// The last Miracast flow; kept after Cancel so closing the window can wait for its NM
    /// cleanup.
    #[cfg_attr(not(all(feature = "miracast", target_os = "linux")), allow(dead_code))]
    miracast_session: Option<MiracastState>,
    /// The ℹ explanation under "Name shown on Android: none" is open (D25).
    #[cfg_attr(not(all(feature = "miracast", target_os = "linux")), allow(dead_code))]
    miracast_name_help: bool,
    /// Presenter: opt-in pen pressure (D26).
    #[cfg(target_os = "linux")]
    pen: pen::PenUi,
}

/// What the viewer's video area shows (☰ menu). The no-video modes ask the presenter to stop
/// encoding (`video_paused`, protocol 12).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ViewMode {
    Video,
    /// "Wacom pen (no video)": an empty surface in the presenter's aspect ratio; pen on.
    Pen,
    /// "PowerPoint remote only (no video)": large remote buttons.
    Remote,
}

impl MirrorApp {
    fn connect(&mut self, ctx: &egui::Context, target: ConnectInfo) {
        let repaint = ctx.clone();
        self.video_paused_sent = None;
        self.error = None;
        self.scan = None;
        self.texture = None;
        self.yuv = None;
        self.cursor_tex = None;
        self.overlay.show_stats = mirrorwire_net::settings::show_stats_saved(&self.config_dir);
        self.last_target = Some(target.clone());
        self.watching = Some(viewer::spawn(
            &self.runtime,
            self.identity.clone(),
            target,
            ViewerOptions {
                hwdec: self.hwdec,
                software_decoder: self.software_decoder,
                simulate_loss: 0.0,
                simulate_burst: 1.0,
                simulate_bandwidth_kbps: 0,
                rgba: self.renderer != ResolvedRenderer::Glow,
                name: self.name.clone(),
                display: {
                    let r = ctx.content_rect();
                    mirrorwire_proto::control::DisplayInfo::from_points(
                        r.width(),
                        r.height(),
                        ctx.pixels_per_point(),
                    )
                },
                reconnect: Some(viewer::ReconnectPolicy::default()),
            },
            Some(Arc::new(move || repaint.request_repaint())),
        ));
    }

    /// The Miracast button (D21) and `view --miracast`: starts discovery at once, so this
    /// computer is visible and waiting before the user taps it on the Android device.
    #[cfg(all(feature = "miracast", target_os = "linux"))]
    fn connect_miracast(&mut self, ctx: &egui::Context, peer: Option<String>) {
        let repaint = ctx.clone();
        self.disconnect(ctx);
        self.wait_for_miracast_cleanup();
        self.error = None;
        self.texture = None;
        self.yuv = None;
        self.cursor_tex = None;
        self.overlay.show_stats = mirrorwire_net::settings::show_stats_saved(&self.config_dir);
        self.last_target = None;
        self.miracast = Some(peer.clone());
        self.miracast_name_help = false;
        let (viewer, session) = crate::miracast::spawn(
            &self.runtime,
            crate::miracast::MiracastOptions {
                hwdec: self.hwdec,
                software_decoder: self.software_decoder,
                rgba: self.renderer != ResolvedRenderer::Glow,
                peer,
                rtp_port: 19000,
            },
            Some(Arc::new(move || repaint.request_repaint())),
        );
        self.watching = Some(viewer);
        self.miracast_session = Some(session);
        self.screen = Screen::Miracast;
    }

    /// A previous flow must have left its Wi-Fi Direct group before a new one starts or the
    /// app exits (its `stop_find`/deactivate would hit the new one, or the profile would stay).
    #[cfg(all(feature = "miracast", target_os = "linux"))]
    fn wait_for_miracast_cleanup(&mut self) {
        if let Some(old) = self.miracast_session.take() {
            if let Some(h) = self.watching.take() {
                h.stop();
            }
            let waited = old.finished.recv_timeout(std::time::Duration::from_secs(5));
            if waited.is_err() && old.phase().is_active() {
                tracing::warn!("miracast: the previous session did not finish in 5 s");
            }
        }
    }

    #[cfg(not(all(feature = "miracast", target_os = "linux")))]
    fn connect_miracast(&mut self, _ctx: &egui::Context, _peer: Option<String>) {
        self.error = Some("this build cannot receive Miracast-compatible streams (cargo feature `miracast`, Linux)".into());
    }

    #[cfg(not(all(feature = "miracast", target_os = "linux")))]
    fn wait_for_miracast_cleanup(&mut self) {}

    #[cfg(not(all(feature = "miracast", target_os = "linux")))]
    fn miracast_ui(&mut self, ui: &mut egui::Ui) {
        self.go_home(ui.ctx());
    }

    fn disconnect(&mut self, ctx: &egui::Context) {
        if let Some(h) = self.watching.take() {
            h.stop();
        }
        self.texture = None;
        self.yuv = None;
        if self.fullscreen {
            self.fullscreen = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
        }
    }

    fn start_presenting(&mut self, ctx: &egui::Context) {
        self.error = None;
        self.qr_texture = None;
        let repaint = ctx.clone();
        // The Quality panel's encoder selector starts on what actually runs.
        self.encoder = start_encoder(HAS_FFMPEG, HAS_FILMCRAFT, self.use_filmcraft);
        self.presenting = Some(presenter::spawn_for_gui(
            self.runtime.clone(),
            self.config_dir.clone(),
            (*self.identity).clone(),
            self.name.clone(),
            self.present_port,
            self.encoder,
            Some(Arc::new(move || repaint.request_repaint())),
        ));
        self.screen = Screen::Present;
    }

    fn stop_presenting(&mut self) {
        if let Some(h) = self.presenting.take() {
            h.stop();
        }
        self.qr_texture = None;
    }

    fn go_home(&mut self, ctx: &egui::Context) {
        self.disconnect(ctx);
        self.stop_presenting();
        self.error = None;
        self.screen = Screen::Home;
    }

    fn footer(&self, ui: &mut egui::Ui) {
        ui.separator();
        ui.weak(format!(
            "Renderer: {}  ·  Window system: {}",
            self.renderer.label(),
            self.display_server.as_deref().unwrap_or("?")
        ));
        ui.weak(format!("Log: {}", self.log.path().display()));
    }

    /// Start screen (D21): a large Presenter button, a small Viewer button, the Miracast
    /// button (feature `miracast`), and the rarely needed settings under "Advanced".
    fn home_ui(&mut self, ui: &mut egui::Ui) {
        ui.add_space(40.0);
        ui.vertical_centered(|ui| {
            ui.heading("MirrorWire");
            ui.add_space(24.0);
            let present = ui.add_enabled(
                PRESENT_AVAILABLE,
                egui::Button::new(egui::RichText::new("Presenter").size(24.0).strong())
                    .min_size(egui::vec2(320.0, 72.0)),
            );
            if present.clicked() {
                self.start_presenting(ui.ctx());
            }
            self.pen_rights_button(ui, present.rect);
            ui.weak("Share this screen.");
            if !PRESENT_AVAILABLE {
                ui.weak("Screen capture is not available on this platform yet.");
            }
            self.pen_rights_box(ui);
            ui.add_space(16.0);
            if ui
                .add(egui::Button::new("Viewer").min_size(egui::vec2(160.0, 28.0)))
                .clicked()
            {
                self.error = None;
                self.screen = Screen::View;
            }
            if cfg!(all(feature = "miracast", target_os = "linux")) {
                ui.add_space(12.0);
                if ui
                    .add(egui::Button::new(MIRACAST_BUTTON).min_size(egui::vec2(320.0, 32.0)))
                    .clicked()
                {
                    self.connect_miracast(ui.ctx(), None);
                }
            }
            if let Some(err) = &self.error {
                ui.add_space(16.0);
                ui.colored_label(egui::Color32::LIGHT_RED, err);
            }
            ui.add_space(24.0);
            egui::CollapsingHeader::new("Advanced")
                .default_open(false)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.weak("Presenter UDP port:");
                        if ui
                            .add(egui::DragValue::new(&mut self.present_port).range(1..=65535))
                            .changed()
                        {
                            if let Err(e) = mirrorwire_net::settings::save_port(
                                &self.config_dir,
                                self.present_port,
                            ) {
                                tracing::warn!("could not save port: {e:#}");
                            }
                        }
                    });
                    ui.weak(
                        "The viewer finds this port automatically (mDNS, QR and the link all \
                         include it).",
                    );
                    self.filmcraft_setting(ui);
                });
        });
        ui.add_space(24.0);
        self.footer(ui);
    }

    /// The ✏ right of the Presenter button: green when the desktop's pen/keys permission is
    /// remembered (we hold a restore token), grey when the next start will ask. Click: explain.
    #[cfg(all(target_os = "linux", feature = "portal"))]
    fn pen_rights_button(&mut self, ui: &mut egui::Ui, presenter: egui::Rect) {
        let remembered = crate::remote_rights::remembered(&self.config_dir);
        let color = if remembered {
            egui::Color32::from_rgb(0, 180, 80)
        } else {
            ui.visuals().weak_text_color()
        };
        let size = egui::vec2(36.0, 36.0);
        let rect = egui::Rect::from_min_size(
            presenter.right_center() + egui::vec2(8.0, -size.y / 2.0),
            size,
        );
        let hover = if remembered {
            "Pen and keys: permission remembered. Click for details."
        } else {
            "Pen and keys: the desktop will ask. Click for details."
        };
        let button = egui::Button::new(egui::RichText::new("✏").size(20.0).color(color))
            .selected(self.pen_rights_open);
        if ui.put(rect, button).on_hover_text(hover).clicked() {
            self.pen_rights_open = !self.pen_rights_open;
            self.pen_rights_note = None;
        }
    }

    #[cfg(not(all(target_os = "linux", feature = "portal")))]
    fn pen_rights_button(&mut self, _ui: &mut egui::Ui, _presenter: egui::Rect) {}

    /// The info box the ✏ opens: what is remembered and a button to forget it.
    #[cfg(all(target_os = "linux", feature = "portal"))]
    fn pen_rights_box(&mut self, ui: &mut egui::Ui) {
        if !self.pen_rights_open {
            return;
        }
        let remembered = crate::remote_rights::remembered(&self.config_dir);
        ui.add_space(8.0);
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_max_width(520.0);
            if remembered {
                ui.colored_label(
                    egui::Color32::from_rgb(0, 180, 80),
                    "✏ Pen and keys: permission remembered",
                );
                ui.label(
                    "Your desktop saved the permission (\"Allow restoring\" in KDE's dialog). \
                     The presenter starts with screen and input together without asking, and \
                     the viewer's pen and remote keys work right away.",
                );
            } else {
                ui.strong("✏ Pen and keys: not remembered");
                ui.label(
                    "The presenter starts with screen sharing only. When the viewer switches \
                     its pen on, the desktop asks to allow pointer and keyboard input (the \
                     picture pauses for a moment). Tick \"Allow restoring\" (KDE) there, so it \
                     is not asked again; this ✏ then turns green.",
                );
            }
            ui.add_space(4.0);
            let forget = ui
                .add_enabled(
                    remembered,
                    egui::Button::new("Forget the pen and keys permission"),
                )
                .on_hover_text("The next presenter start asks again.");
            if forget.clicked() {
                self.pen_rights_note = Some(match crate::remote_rights::forget(&self.config_dir) {
                    Ok(_) => "Forgotten. The next presenter start asks again (a running \
                              presenter keeps its permission until it stops)."
                        .into(),
                    Err(e) => {
                        tracing::warn!("could not forget the pen permission: {e:#}");
                        format!("Could not forget the permission: {e:#}")
                    }
                });
            }
            if let Some(note) = &self.pen_rights_note {
                ui.label(note);
            }
            ui.weak(
                "MirrorWire deletes its own restore tokens. The desktop offers apps no way to \
                 revoke a permission, so it may keep its record of the choice; remove it in the \
                 desktop's settings if it lists one.",
            );
        });
    }

    #[cfg(not(all(target_os = "linux", feature = "portal")))]
    fn pen_rights_box(&mut self, _ui: &mut egui::Ui) {}

    /// Start screen › Advanced: "Use the FilmCraft encoder", saved in `settings.json`. Here
    /// and not in the presenter window, because a presenter whose default encoder fails never
    /// reaches that window's Quality panel.
    fn filmcraft_setting(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        let response = ui.add_enabled(
            HAS_FILMCRAFT && HAS_FFMPEG,
            egui::Checkbox::new(
                &mut self.use_filmcraft,
                "Use the FilmCraft encoder (pure Rust, CPU)",
            ),
        );
        if response.changed() {
            if let Err(e) = mirrorwire_net::settings::save_filmcraft_encoder(
                &self.config_dir,
                self.use_filmcraft,
            ) {
                tracing::warn!("could not save the encoder choice: {e:#}");
                self.error = Some(format!("Could not save the encoder choice: {e:#}"));
            }
        }
        ui.weak(filmcraft_note(HAS_FFMPEG, HAS_FILMCRAFT));
    }

    fn browse_ui(&mut self, ui: &mut egui::Ui) {
        if self.can_go_home && ui.button("Back").clicked() {
            self.go_home(ui.ctx());
            return;
        }
        ui.heading("MirrorWire viewer");
        if let Some(err) = &self.error {
            ui.colored_label(egui::Color32::LIGHT_RED, err);
        }
        ui.separator();
        ui.label("Presenters on this network:");
        let found: Vec<Presenter> = self
            .browser
            .as_mut()
            .map(|b| b.poll().to_vec())
            .unwrap_or_default();
        if found.is_empty() {
            ui.weak("searching…");
        }
        ui.horizontal(|ui| {
            ui.label("Pairing code (first connection only):");
            ui.add(egui::TextEdit::singleline(&mut self.code_input).desired_width(80.0));
        });
        let mut chosen = None;
        for p in &found {
            ui.horizontal(|ui| {
                if ui.button("Connect").clicked() {
                    chosen = Some(ConnectInfo {
                        addr: p.addr,
                        addrs: p.addrs.clone(),
                        fingerprint: p.fingerprint.clone(),
                        pairing_code: Some(self.code_input.trim().to_owned())
                            .filter(|c| !c.is_empty()),
                        name: Some(p.name.clone()),
                    });
                }
                let addrs = if p.addrs.len() > 1 {
                    p.addrs
                        .iter()
                        .map(|a| a.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                } else {
                    p.addr.to_string()
                };
                ui.label(format!(
                    "{}  {}  {}  [{}]",
                    p.name,
                    addrs,
                    p.resolution.as_deref().unwrap_or(""),
                    short_fingerprint(&p.fingerprint)
                ));
                if let Some(note) = p.version_problem() {
                    ui.colored_label(egui::Color32::from_rgb(230, 160, 40), note);
                }
            });
        }
        ui.separator();
        ui.label("Or paste the mirrorwire:// link from the presenter's QR code:");
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.uri_input).desired_width(500.0));
            if ui.button("Connect").clicked() {
                match ConnectInfo::parse(&self.uri_input) {
                    Ok(t) => chosen = Some(t),
                    Err(e) => self.error = Some(format!("{e:#}")),
                }
            }
            if scan::AVAILABLE {
                if self.scan.is_some() {
                    if ui.button("Stop scanning").clicked() {
                        self.scan = None;
                    }
                } else if ui.button("Scan QR with camera").clicked() {
                    self.error = None;
                    self.scan = Some(scan::Scan::start());
                }
            }
        });
        if let Some(scan) = self.scan.as_mut() {
            match scan.poll() {
                scan::Poll::Running(status) => {
                    ui.label(format!("Scanning: {status}"));
                    ui.label("Hold the presenter's QR code in front of the camera.");
                }
                scan::Poll::Found(info) => {
                    self.scan = None;
                    chosen = Some(info);
                }
                scan::Poll::Failed(e) => {
                    self.scan = None;
                    self.error = Some(e);
                }
            }
        }
        ui.checkbox(
            &mut self.hwdec,
            "Hardware decoding (VideoToolbox / VAAPI, software fallback)",
        );
        ui.checkbox(&mut self.pixel_exact, "1:1 pixel-exact display (no scale)");
        ui.weak("On macOS, VideoToolbox is the default. An older GPU (HD 6770M on OCLP) may not decode H.264; the viewer then uses a tuned software decoder and shows which one is active.");
        if let Some(target) = chosen {
            self.connect(ui.ctx(), target);
        }
        log_panel(ui, &self.log, &mut self.copy_note);
        self.footer(ui);
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(500));
    }

    fn has_picture(&self) -> bool {
        self.texture.is_some() || self.yuv.is_some()
    }

    fn should_show_video(&self) -> bool {
        let Some(handle) = &self.watching else {
            return false;
        };
        let status = handle.status.lock_ok();
        // Pen and remote-only modes need no picture (the presenter may not send one at all).
        (self.view_mode != ViewMode::Video && status.connected)
            || viewer_ui(status.connected, status.first_frame, self.has_picture())
                == ViewerUi::Video
    }

    fn connecting_ui(&mut self, ui: &mut egui::Ui) {
        let handle = match &self.watching {
            Some(h) => h,
            None => return,
        };
        let status = handle.status.lock_ok().clone();
        if ui.button("Back").clicked() {
            self.disconnect(ui.ctx());
            return;
        }
        ui.add_space(8.0);
        ui.heading("Connecting");
        if let Some(t) = &self.last_target {
            ui.monospace(format!(
                "Target: {}  {}",
                t.name.clone().unwrap_or_default(),
                t.addr
            ));
        }
        ui.add_space(8.0);
        let progress = status.trace.snapshot();
        for step in &progress.steps {
            step_row(ui, step);
        }
        if let Some(err) = progress.error.as_ref().or(status.error.as_ref()) {
            ui.add_space(8.0);
            ui.colored_label(egui::Color32::LIGHT_RED, err);
            if err.contains("firewall-cmd") {
                ui.horizontal(|ui| {
                    if ui.button("Copy firewall command").clicked() {
                        let text = err
                            .split("On Fedora:")
                            .nth(1)
                            .map(|s| s.trim().to_owned())
                            .unwrap_or_else(|| err.clone());
                        copy_with_feedback(ui.ctx(), text, &mut self.copy_note);
                    }
                    show_copy_note(ui, &self.copy_note);
                });
            }
        } else {
            ui.add_space(8.0);
            ui.label("connecting…");
            if !status.decoder.is_empty() {
                ui.weak(format!("Decoder: {}", status.decoder));
            }
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("Retry").clicked() {
                if let Some(target) = self.last_target.clone() {
                    self.disconnect(ui.ctx());
                    self.connect(ui.ctx(), target);
                } else if let Some(peer) = self.miracast.clone() {
                    self.disconnect(ui.ctx());
                    self.connect_miracast(ui.ctx(), peer);
                }
            }
            if ui.button("Cancel").clicked() {
                self.disconnect(ui.ctx());
            }
        });
        log_panel(ui, &self.log, &mut self.copy_note);
        self.footer(ui);
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(200));
    }
}

impl eframe::App for MirrorApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        if self.display_server.is_none() {
            let label = crate::session::window_system(frame);
            tracing::info!("window system: {label}");
            self.display_server = Some(label);
        }
        match self.screen {
            Screen::Home => {
                egui::CentralPanel::default().show(ui, |ui| self.home_ui(ui));
            }
            Screen::Present => {
                egui::CentralPanel::default().show(ui, |ui| self.present_ui(ui));
            }
            Screen::View if self.should_show_video() => {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ui, |ui| self.video_ui(ui));
            }
            Screen::View if self.watching.is_some() => {
                egui::CentralPanel::default().show(ui, |ui| self.connecting_ui(ui));
            }
            Screen::View => {
                egui::CentralPanel::default().show(ui, |ui| self.browse_ui(ui));
            }
            Screen::Miracast => self.miracast_ui(ui),
        }
    }
}

impl Drop for MirrorApp {
    fn drop(&mut self) {
        if let Some(h) = self.watching.take() {
            h.stop();
        }
        // Closing the window during a Miracast flow: leave the group and remove our NM
        // profile before the runtime goes away.
        self.wait_for_miracast_cleanup();
        if let Some(h) = self.presenting.take() {
            h.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_encoder_follows_the_build_and_the_setting() {
        assert_eq!(start_encoder(true, true, false), EncoderBackend::Auto);
        assert_eq!(start_encoder(true, true, true), EncoderBackend::Filmcraft);
        // A saved "on" from another build is ignored when FilmCraft is not compiled in.
        assert_eq!(start_encoder(true, false, true), EncoderBackend::Auto);
        // Without FFmpeg FilmCraft is the only encoder, whatever was saved.
        assert_eq!(start_encoder(false, true, false), EncoderBackend::Filmcraft);
        assert_eq!(
            start_encoder(false, false, false),
            EncoderBackend::Filmcraft
        );
    }

    #[test]
    fn filmcraft_note_says_what_each_build_can_do() {
        assert!(filmcraft_note(true, true).contains("VAAPI, then x264"));
        assert!(filmcraft_note(false, true).contains("only encoder"));
        assert!(filmcraft_note(true, false).contains("`filmcraft`"));
        assert!(filmcraft_note(false, false).contains("cannot encode"));
    }

    #[test]
    fn renderer_labels_and_eframe_choice_match() {
        assert_eq!(ResolvedRenderer::Glow.label(), "OpenGL (glow)");
        assert_eq!(ResolvedRenderer::Wgpu.label(), "wgpu");
        assert!(matches!(
            ResolvedRenderer::Glow.eframe(),
            eframe::Renderer::Glow
        ));
        assert!(matches!(
            ResolvedRenderer::Wgpu.eframe(),
            eframe::Renderer::Wgpu
        ));
    }

    #[test]
    fn the_start_screen_button_names_miracast_only_descriptively() {
        // D22: "Miracast" is a description of compatibility, never a product claim.
        assert!(MIRACAST_BUTTON.contains("Miracast-compatible"));
    }
}
