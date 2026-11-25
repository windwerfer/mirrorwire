//! egui UI for the Android viewer: discovered presenters, a pairing-code
//! keypad, `mirrorwire://` links (typed, or opened from the camera app's QR
//! scan via the Java shell), and the full-screen video view.

use crate::chrome;
use crate::session::{self, Handle};
use eframe::egui;
use mirrorwire_net::discovery::Browser;
use mirrorwire_net::identity::{short_fingerprint, Identity};
use mirrorwire_net::pairing::ConnectInfo;
use mirrorwire_proto::control::DisplayInfo;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use winit::platform::android::activity::AndroidApp;

/// The corner icons and the stats start this far below the top (clear of the status bar).
const CORNER_TOP: f32 = 40.0;

/// Written by the Java shell when a `mirrorwire://` link opens the app.
const PENDING_URI_FILE: &str = "pending_uri.txt";

pub fn run(app: AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("mirrorwire"),
    );
    let data_dir = app
        .internal_data_path()
        .unwrap_or_else(|| PathBuf::from("/data/local/tmp/mirrorwire"));
    tracing::info!(
        "MirrorWire Android viewer {} starting, data {}",
        env!("CARGO_PKG_VERSION"),
        data_dir.display()
    );
    let options = eframe::NativeOptions {
        android_app: Some(app),
        ..Default::default()
    };
    let result = eframe::run_native(
        "MirrorWire",
        options,
        Box::new(move |cc| Ok(Box::new(ViewerApp::new(cc, data_dir)?))),
    );
    if let Err(e) = result {
        tracing::error!("eframe: {e}");
    }
}

struct ViewerApp {
    runtime: tokio::runtime::Runtime,
    identity: Option<Arc<Identity>>,
    data_dir: PathBuf,
    browser: Option<Browser>,
    browse_error: Option<String>,
    uri_input: String,
    code: String,
    error: Option<String>,
    session: Option<Handle>,
    target_name: String,
    texture: Option<egui::TextureHandle>,
    picture_size: (u32, u32),
    /// Hide (menu or a tap on the video), Show statistics (saved in `settings.json`) and
    /// PowerPoint mode (the presenter-remote row, on by default here).
    overlay: chrome::Overlay,
    /// Why the Show statistics choice could not be saved (shown in the menu).
    overlay_note: Option<String>,
    /// Pen/finger drives the presenter's pointer (off by default: a tap then toggles the
    /// overlay instead of clicking on the presenter).
    pen_mode: bool,
    /// ☰ menu: show the video, or only a pen surface / the remote buttons (no video).
    view_mode: ViewMode,
    /// `video_paused` last sent on this connection (`None` = not yet: send on connect).
    video_paused_sent: Option<bool>,
    pen_down: bool,
    /// Last pen/touch force seen while pen mode is on (`None`: the device reports none).
    pen_pressure: Option<u16>,
    pen_last_move: Option<Instant>,
    last_uri_check: Instant,
    /// Texture of the separately sent cursor, by image id.
    cursor_tex: Option<(u32, egui::TextureHandle)>,
    /// Zero-copy video output; `None` means the CPU path (YUV → RGBA texture).
    gpu: Option<crate::gpu::SharedRenderer>,
    /// Why the GPU path is off (shown in the HUD).
    gpu_off: Option<String>,
    /// The GPU renderer has a picture for this session.
    gpu_picture: bool,
    /// Home-screen switch: skip the hardware decoders and use FilmCraft.
    force_software: bool,
}

impl ViewerApp {
    fn new(
        cc: &eframe::CreationContext<'_>,
        data_dir: PathBuf,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        cc.egui_ctx
            .set_pixels_per_point(cc.egui_ctx.pixels_per_point().max(2.0));
        let (gpu, gpu_off) = match cc.gl.as_deref().map(crate::gpu::VideoRenderer::new) {
            Some(Ok(r)) => (Some(r), None),
            Some(Err(e)) => (None, Some(format!("{e:#}"))),
            None => (None, Some("no GL context".into())),
        };
        match &gpu_off {
            None => tracing::info!("video output: GPU (hardware buffers as external textures)"),
            Some(why) => tracing::warn!("video output: CPU copy, GPU path unavailable: {why}"),
        }
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let mut error = None;
        let identity = match Identity::load_or_create(&data_dir) {
            Ok(id) => Some(Arc::new(id)),
            Err(e) => {
                error = Some(format!("Could not create this device's certificate: {e:#}"));
                None
            }
        };
        let show_stats = mirrorwire_net::settings::show_stats_saved(&data_dir);
        let (browser, browse_error) = {
            let _guard = runtime.enter();
            match Browser::start() {
                Ok(b) => (Some(b), None),
                Err(e) => (
                    None,
                    Some(format!("Network discovery (mDNS) unavailable: {e:#}")),
                ),
            }
        };
        Ok(Self {
            runtime,
            identity,
            data_dir,
            browser,
            browse_error,
            uri_input: String::new(),
            code: String::new(),
            error,
            session: None,
            target_name: String::new(),
            texture: None,
            picture_size: (0, 0),
            overlay: chrome::Overlay {
                hidden: false,
                show_stats,
                powerpoint: true,
            },
            overlay_note: None,
            pen_mode: false,
            view_mode: ViewMode::Video,
            video_paused_sent: None,
            pen_down: false,
            pen_pressure: None,
            pen_last_move: None,
            last_uri_check: Instant::now() - Duration::from_secs(1),
            cursor_tex: None,
            gpu,
            gpu_off,
            gpu_picture: false,
            force_software: false,
        })
    }

    fn connect(&mut self, ctx: &egui::Context, mut target: ConnectInfo) {
        let Some(identity) = self.identity.clone() else {
            return;
        };
        if target.pairing_code.is_none() && self.code.len() == 6 {
            target.pairing_code = Some(self.code.clone());
        }
        self.error = None;
        self.clear_picture();
        self.cursor_tex = None;
        self.video_paused_sent = None;
        self.target_name = target
            .name
            .clone()
            .unwrap_or_else(|| target.addr.to_string());
        let repaint = ctx.clone();
        // Written by the Java shell at start-up (MediaCodecList has no NDK API).
        let decoders =
            std::fs::read_to_string(self.data_dir.join(crate::codec_list::CODEC_LIST_FILE))
                .map(|t| crate::codec_list::parse(&t))
                .unwrap_or_default();
        tracing::info!(
            "H.264 decoders on this device: {}",
            decoders
                .iter()
                .map(|d| format!("{}{}", d.name, if d.hardware { " (hw)" } else { "" }))
                .collect::<Vec<_>>()
                .join(", ")
        );
        self.session = Some(session::spawn(
            self.runtime.handle(),
            identity,
            target,
            "Android".into(),
            {
                let r = ctx.content_rect();
                DisplayInfo::from_points(r.width(), r.height(), ctx.pixels_per_point())
            },
            decoders,
            self.gpu.is_some(),
            self.force_software,
            Arc::new(move || repaint.request_repaint()),
        ));
    }

    fn clear_picture(&mut self) {
        self.texture = None;
        self.gpu_picture = false;
        if let Some(r) = &self.gpu {
            r.lock().unwrap_or_else(|e| e.into_inner()).clear();
        }
    }

    fn disconnect(&mut self) {
        if let Some(s) = self.session.take() {
            s.stop();
        }
        self.clear_picture();
    }

    /// Links opened from outside (QR code scanned with the camera app).
    fn check_pending_uri(&mut self, ctx: &egui::Context) {
        if self.last_uri_check.elapsed() < Duration::from_millis(500) {
            return;
        }
        self.last_uri_check = Instant::now();
        let path = self.data_dir.join(PENDING_URI_FILE);
        let Ok(text) = std::fs::read_to_string(&path) else {
            return;
        };
        let _ = std::fs::remove_file(&path);
        let uri = text.trim();
        if uri.is_empty() || uri.len() > 4096 {
            return;
        }
        match ConnectInfo::parse(uri) {
            Ok(info) => {
                self.disconnect();
                self.connect(ctx, info);
            }
            Err(e) => self.error = Some(format!("That link is not a MirrorWire link: {e:#}")),
        }
    }

    fn home_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("MirrorWire viewer");
        ui.add_space(8.0);
        if let Some(err) = &self.error {
            ui.colored_label(egui::Color32::LIGHT_RED, err);
            ui.add_space(6.0);
        }

        ui.label("Pairing code (shown on the presenter):");
        ui.horizontal(|ui| {
            let shown: String = (0..6)
                .map(|i| self.code.chars().nth(i).unwrap_or('_'))
                .collect();
            ui.label(egui::RichText::new(shown).monospace().size(28.0));
        });
        keypad(ui, &mut self.code);
        ui.weak("Only needed the first time; the presenter then remembers this device.");
        ui.checkbox(
            &mut self.force_software,
            "Software decoder (FilmCraft) instead of the hardware one",
        );
        ui.separator();

        ui.label("Presenters on this network:");
        let found: Vec<_> = self
            .browser
            .as_mut()
            .map(|b| b.poll().to_vec())
            .unwrap_or_default();
        if let Some(e) = &self.browse_error {
            ui.colored_label(egui::Color32::from_rgb(230, 160, 40), e);
        } else if found.is_empty() {
            ui.weak("Searching… (phone and presenter must be on the same Wi-Fi)");
        }
        let mut pick = None;
        for p in &found {
            let label = format!(
                "{}  ·  {}{}",
                p.name,
                p.addr,
                p.resolution
                    .as_deref()
                    .map(|r| format!("  ·  {r}"))
                    .unwrap_or_default()
            );
            let label = match p.version_problem() {
                Some(note) => format!("{label}\n{note}"),
                None => label,
            };
            if ui
                .add(egui::Button::new(label).min_size(egui::vec2(ui.available_width(), 48.0)))
                .clicked()
            {
                pick = Some(ConnectInfo {
                    addr: p.addr,
                    addrs: p.addrs.clone(),
                    fingerprint: p.fingerprint.clone(),
                    pairing_code: None,
                    name: Some(p.name.clone()),
                });
            }
        }
        if let Some(target) = pick {
            let ctx = ui.ctx().clone();
            self.connect(&ctx, target);
        }
        ui.separator();

        ui.label("Or a mirrorwire:// link:");
        ui.add(
            egui::TextEdit::singleline(&mut self.uri_input)
                .desired_width(f32::INFINITY)
                .hint_text("mirrorwire://…"),
        );
        if ui
            .add(egui::Button::new("Connect to link").min_size(egui::vec2(200.0, 44.0)))
            .clicked()
        {
            match ConnectInfo::parse(self.uri_input.trim()) {
                Ok(info) => {
                    let ctx = ui.ctx().clone();
                    self.connect(&ctx, info);
                }
                Err(e) => self.error = Some(format!("Not a MirrorWire link: {e:#}")),
            }
        }
        ui.weak("Tip: scan the presenter's QR code with the camera app; the link opens MirrorWire directly.");
        ui.separator();
        if let Some(id) = &self.identity {
            ui.weak(format!(
                "This device: {}",
                short_fingerprint(&id.fingerprint())
            ));
        }
        ui.weak(format!(
            "MirrorWire {}  ·  logs: adb logcat -s mirrorwire",
            env!("CARGO_PKG_VERSION")
        ));
    }

    fn connecting_ui(&mut self, ui: &mut egui::Ui, status: &session::Status) {
        ui.heading(format!("Connecting to {}…", self.target_name));
        let progress = status.trace.snapshot();
        for step in &progress.steps {
            let mark = match step.state {
                mirrorwire_net::diag::StepState::Pending => "·",
                mirrorwire_net::diag::StepState::Active => "…",
                mirrorwire_net::diag::StepState::Ok => "✓",
                mirrorwire_net::diag::StepState::Failed => "✗",
            };
            ui.label(format!("{mark} {}  {}", step.id.label(), step.detail));
        }
        if let Some(err) = &status.error {
            ui.colored_label(egui::Color32::LIGHT_RED, err);
        }
        if let Some(advice) = &progress.advice {
            ui.weak(advice);
        }
        if ui
            .add(egui::Button::new("Back").min_size(egui::vec2(160.0, 44.0)))
            .clicked()
        {
            if let Some(err) = status.error.clone() {
                self.error = Some(err);
            }
            self.disconnect();
        }
    }

    fn video_ui(&mut self, ui: &mut egui::Ui, status: &session::Status) {
        // Tell the presenter whether we show video (again after every reconnect).
        let want_pause = self.view_mode != ViewMode::Video;
        if let Some(tx) = &status.control {
            if self.video_paused_sent != Some(want_pause) {
                let _ =
                    tx.send(mirrorwire_proto::control::Control::VideoPaused { paused: want_pause });
                self.video_paused_sent = Some(want_pause);
            }
        }
        let rect = ui.max_rect();
        ui.painter().rect_filled(rect, 0.0, egui::Color32::BLACK);
        let mut picture_rect: Option<egui::Rect> = None;
        if self.view_mode == ViewMode::Pen {
            picture_rect = pen_surface(ui, rect, status);
        } else if self.view_mode == ViewMode::Remote {
            // Buttons come after the full-area interaction below, so they get the taps.
        } else if self.texture.is_some() || self.gpu_picture {
            let (w, h) = (
                self.picture_size.0.max(1) as f32,
                self.picture_size.1.max(1) as f32,
            );
            let scale = (rect.width() / w).min(rect.height() / h);
            let size = egui::vec2(w * scale, h * scale);
            let img_rect = egui::Rect::from_center_size(rect.center(), size);
            picture_rect = Some(img_rect);
            match (&self.texture, &self.gpu) {
                (Some(tex), _) => {
                    ui.painter().image(
                        tex.id(),
                        img_rect,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        egui::Color32::WHITE,
                    );
                }
                (None, Some(r)) => {
                    let r = r.clone();
                    let callback = eframe::egui_glow::CallbackFn::new(move |_info, painter| {
                        r.lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .paint(painter.gl());
                    });
                    ui.painter().add(egui::PaintCallback {
                        rect: img_rect,
                        callback: Arc::new(callback),
                    });
                }
                (None, None) => {}
            }
            if status.cursor_mode == Some(mirrorwire_proto::control::CursorMode::Separate) {
                let current = status
                    .cursor
                    .as_ref()
                    .and_then(|c| c.lock().unwrap_or_else(|e| e.into_inner()).current());
                if let Some((p, img)) = current {
                    self.draw_cursor(ui, &p, &img, img_rect);
                }
            }
        } else {
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "Waiting for the first picture…",
                egui::FontId::proportional(18.0),
                egui::Color32::GRAY,
            );
        }
        let menu_id = egui::Id::unique("mirrorwire_overlay_menu");
        // Read before the popup runs this frame: the tap that dismisses the menu must not also
        // hide the overlay or press the pen on the presenter.
        let menu_was_open = egui::Popup::is_id_open(ui.ctx(), menu_id);
        let response = ui.interact(
            rect,
            ui.scope_id().with("video"),
            egui::Sense::click_and_drag(),
        );
        if response.clicked() && !self.pen_mode && !menu_was_open {
            self.overlay.hidden = !self.overlay.hidden;
        }
        if !menu_was_open || self.pen_down {
            self.send_pen(ui.ctx(), &response, picture_rect, status);
        }
        if self.view_mode == ViewMode::Remote {
            remote_only_buttons(ui, rect, status);
        }
        let hud_pos = rect.left_top() + egui::vec2(8.0, CORNER_TOP);
        let mut warning_pos = hud_pos;
        if self.overlay.stats_visible() {
            let stream = status.stream.as_ref();
            let text = format!(
                "{}x{}  {:.1} fps  {:.2} Mbit/s  presenter encodes with {}\n{}\ndecode {:.1} ms  to RGBA {:.1} ms  lost {}\npicture age {:.0} ms  reader behind {:.0} ms  out of order {}\n{}\n{}\ncursor {}\n{}\ntap: hide / show  ·  menu: top right",
                stream.map(|s| s.width).unwrap_or(0),
                stream.map(|s| s.height).unwrap_or(0),
                status.fps,
                status.mbps,
                stream.map(|s| s.encoder.as_str()).unwrap_or("?"),
                match &self.gpu_off {
                    Some(why) => format!("{} (GPU output off: {why})", status.decoder),
                    None => status.decoder.clone(),
                },
                status.decode_ms,
                status.convert_ms,
                status.lost,
                status.picture_age_ms,
                status.reader_behind_ms,
                status.out_of_order,
                status.pipeline.line(),
                status.bottleneck,
                status.cursor_mode.map_or("?", |m| m.as_str()),
                status.link.line(),
            );
            let galley = ui.painter().layout(
                text,
                egui::FontId::monospace(12.0),
                egui::Color32::from_rgb(0, 255, 120),
                (rect.width() - chrome::COLUMN_WIDTH - 16.0).max(80.0),
            );
            warning_pos = hud_pos + egui::vec2(0.0, galley.size().y + 8.0);
            ui.painter()
                .galley(hud_pos, galley, egui::Color32::from_rgb(0, 255, 120));
        }
        // Failures stay visible with the stats off or the overlay hidden (AGENTS.md §1.8).
        let gpu_error = self
            .gpu
            .as_ref()
            .and_then(|r| r.lock().unwrap_or_else(|e| e.into_inner()).error.clone());
        if let Some(warning) = gpu_error
            .or_else(|| status.pipeline.stall())
            .or_else(|| stale_picture_warning(status))
        {
            let warn = ui.painter().layout(
                warning,
                egui::FontId::proportional(15.0),
                egui::Color32::from_rgb(255, 90, 90),
                (rect.width() - chrome::COLUMN_WIDTH - 16.0).max(80.0),
            );
            ui.painter().rect_filled(
                egui::Rect::from_min_size(warning_pos, warn.size()).expand(4.0),
                4.0,
                egui::Color32::from_black_alpha(200),
            );
            ui.painter()
                .galley(warning_pos, warn, egui::Color32::from_rgb(255, 90, 90));
        }
        if self.view_mode != ViewMode::Remote && self.overlay.powerpoint_visible() {
            // Presenter remote: one tap = one key on the presenter (protocol 9).
            let keys = mirrorwire_proto::control::RemoteKey::ALL;
            let (bw, bh, gap) = (150.0, 64.0, 8.0);
            let row_w = bw * keys.len() as f32 + gap * (keys.len() as f32 - 1.0);
            let row_x = (rect.center().x - row_w / 2.0).max(rect.left() + 4.0);
            for (i, key) in keys.into_iter().enumerate() {
                let r = egui::Rect::from_min_size(
                    egui::pos2(row_x + i as f32 * (bw + gap), rect.bottom() - bh - 24.0),
                    egui::vec2(bw, bh),
                );
                let label = egui::RichText::new(key.label()).size(22.0);
                if ui.put(r, egui::Button::new(label)).clicked() {
                    if let Some(tx) = &status.control {
                        let _ = tx.send(mirrorwire_proto::control::Control::RemoteKey { key });
                    }
                }
            }
        }
        // Corner icons: always shown, also when the rest of the overlay is hidden.
        let corner = chrome::corner(ui, rect, CORNER_TOP, self.pen_mode);
        let mut toggle_pen = corner.pen_clicked;
        let mut chosen_mode = None;
        let mut pen_mode = self.pen_mode;
        let (mut wacom, mut remote_only) = (
            self.view_mode == ViewMode::Pen,
            self.view_mode == ViewMode::Remote,
        );
        let mut overlay = self.overlay;
        let mut stats_changed = false;
        egui::Popup::menu(&corner.menu)
            .id(menu_id)
            .align(egui::RectAlign::LEFT_START)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| {
                ui.set_min_width(260.0);
                ui.style_mut().spacing.interact_size.y = 44.0;
                ui.style_mut().spacing.icon_width = 24.0;
                let text = |t: &str| egui::RichText::new(t).size(20.0);
                toggle_pen |= ui.checkbox(&mut pen_mode, text("Pen")).changed();
                ui.checkbox(&mut overlay.powerpoint, text("PowerPoint mode"));
                ui.checkbox(&mut overlay.hidden, text("Hide"));
                stats_changed = ui
                    .checkbox(&mut overlay.show_stats, text("Show statistics"))
                    .changed();
                if let Some(note) = &self.overlay_note {
                    ui.colored_label(egui::Color32::LIGHT_RED, note);
                }
                ui.separator();
                // The presenter sends and encodes no video in these two (protocol 12).
                if ui
                    .checkbox(&mut wacom, text("Wacom pen (no video)"))
                    .changed()
                {
                    chosen_mode = Some(if wacom {
                        ViewMode::Pen
                    } else {
                        ViewMode::Video
                    });
                }
                if ui
                    .checkbox(&mut remote_only, text("PowerPoint remote only (no video)"))
                    .changed()
                {
                    chosen_mode = Some(if remote_only {
                        ViewMode::Remote
                    } else {
                        ViewMode::Video
                    });
                }
            });
        if let Some(mode) = chosen_mode {
            self.set_view_mode(mode, status);
        } else if toggle_pen {
            // "Wacom pen (no video)" has the pen on by definition: its toggle leaves the mode.
            if self.view_mode == ViewMode::Pen {
                self.set_view_mode(ViewMode::Video, status);
            } else {
                self.set_pen(!self.pen_mode, status);
            }
        }
        self.overlay = overlay;
        if stats_changed {
            self.overlay_note =
                mirrorwire_net::settings::save_show_stats(&self.data_dir, overlay.show_stats)
                    .err()
                    .map(|e| {
                        tracing::warn!("saving Show statistics: {e:#}");
                        format!("Could not save this choice: {e:#}")
                    });
        }
        if corner.disconnect_clicked {
            self.disconnect();
        }
    }
}

impl ViewerApp {
    /// Pen on or off; the presenter is told at once (protocol 12) so it asks its desktop for
    /// pointer permission now, not on the first stroke.
    fn set_pen(&mut self, on: bool, status: &session::Status) {
        if on == self.pen_mode {
            return;
        }
        self.pen_mode = on;
        if let Some(tx) = &status.control {
            let _ = tx.send(mirrorwire_proto::control::Control::PenMode { on });
        }
    }

    /// Switch the video area's mode. The pen is on exactly in "Wacom pen" mode (switching into
    /// it asks the presenter's desktop for pointer permission, protocol 12).
    fn set_view_mode(&mut self, mode: ViewMode, status: &session::Status) {
        if mode == self.view_mode {
            return;
        }
        let pen = mode == ViewMode::Pen;
        let pen_changed = pen != self.pen_mode && (pen || self.view_mode == ViewMode::Pen);
        self.view_mode = mode;
        if pen_changed {
            self.set_pen(pen, status);
        }
    }

    /// Finger/pen as the presenter's pointer (pen mode only): down/up on the reliable control
    /// stream, moves coalesced to about 60 per second.
    fn send_pen(
        &mut self,
        ctx: &egui::Context,
        response: &egui::Response,
        picture: Option<egui::Rect>,
        status: &session::Status,
    ) {
        use mirrorwire_proto::control::{
            pointer_in_rect, pressure_from_force, Control, PointerPhase,
        };
        let Some(tx) = &status.control else { return };
        let Some(pic) = picture else { return };
        if !self.pen_mode {
            if self.pen_down {
                self.pen_down = false;
                // Leaving pen mode mid-press: release so the presenter button is not stuck.
                let _ = tx.send(Control::Pointer {
                    phase: PointerPhase::Up,
                    x: 0,
                    y: 0,
                    pressure: None,
                });
            }
            return;
        }
        // Latest pen/touch force this frame (protocol 11); a mouse has none. Read every frame
        // (also when a move is coalesced away) and kept, because touch events do not arrive
        // with every repaint.
        let force = ctx.input(|i| {
            i.raw.events.iter().rev().find_map(|e| match e {
                egui::Event::Touch { force, .. } => Some(*force),
                _ => None,
            })
        });
        if let Some(f) = force {
            self.pen_pressure = pressure_from_force(f);
        }
        let Some(pos) = ctx.input(|i| i.pointer.interact_pos().or(i.pointer.latest_pos())) else {
            return;
        };
        let (pressed, down) =
            ctx.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_down()));
        let Some((x, y)) = pointer_in_rect(
            pos.x,
            pos.y,
            (pic.left(), pic.top(), pic.width(), pic.height()),
        ) else {
            return;
        };
        let phase = if pressed && response.contains_pointer() && !self.pen_down {
            self.pen_down = true;
            PointerPhase::Down
        } else if self.pen_down && !down {
            self.pen_down = false;
            PointerPhase::Up
        } else if self.pen_down {
            let now = Instant::now();
            if self
                .pen_last_move
                .is_some_and(|t| now.duration_since(t) < Duration::from_millis(16))
            {
                return;
            }
            self.pen_last_move = Some(now);
            PointerPhase::Move
        } else {
            return;
        };
        let pressure = self.pen_pressure;
        if phase == PointerPhase::Up {
            self.pen_pressure = None;
        }
        let _ = tx.send(Control::Pointer {
            phase,
            x,
            y,
            pressure,
        });
    }

    /// The presenter's cursor as a textured quad over the video (same
    /// mapping as the desktop viewer). Drawn inside this app's own surface,
    /// so no overlay permission is involved.
    fn draw_cursor(
        &mut self,
        ui: &egui::Ui,
        p: &mirrorwire_proto::cursor::PointerUpdate,
        img: &mirrorwire_proto::cursor::CursorImage,
        video: egui::Rect,
    ) {
        let rect = mirrorwire_proto::cursor::RectF {
            x: video.min.x,
            y: video.min.y,
            w: video.width(),
            h: video.height(),
        };
        let Some(r) = mirrorwire_proto::cursor::cursor_rect(p, img, rect) else {
            return;
        };
        if self.cursor_tex.as_ref().is_none_or(|(id, _)| *id != img.id) {
            let image = egui::ColorImage::from_rgba_premultiplied(
                [img.width as usize, img.height as usize],
                &img.rgba,
            );
            let tex = ui
                .ctx()
                .load_texture("cursor", image, egui::TextureOptions::LINEAR);
            self.cursor_tex = Some((img.id, tex));
        }
        if let Some((_, tex)) = &self.cursor_tex {
            ui.painter().with_clip_rect(video).image(
                tex.id(),
                egui::Rect::from_min_size(egui::pos2(r.x, r.y), egui::vec2(r.w, r.h)),
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }
    }
}

impl eframe::App for ViewerApp {
    fn on_exit(&mut self, gl: Option<&eframe::glow::Context>) {
        if let (Some(r), Some(gl)) = (&self.gpu, gl) {
            r.lock().unwrap_or_else(|e| e.into_inner()).destroy(gl);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.check_pending_uri(&ctx);
        let status = self.session.as_ref().map(|s| s.status());
        if let Some(s) = &self.session {
            if let Ok(mut img) = s.frames.try_recv() {
                let gpu_picture = match (img.gpu.take(), img.yuv.take(), &self.gpu) {
                    (Some(frame), _, Some(r)) => {
                        r.lock().unwrap_or_else(|e| e.into_inner()).push(frame);
                        true
                    }
                    (_, Some(planes), Some(r)) => {
                        r.lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .push_planes(planes);
                        true
                    }
                    _ => false,
                };
                if gpu_picture {
                    self.picture_size = (img.width, img.height);
                    self.gpu_picture = true;
                    s.counters
                        .presented
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                } else {
                    let image = egui::ColorImage::from_rgba_unmultiplied(
                        [img.width as usize, img.height as usize],
                        &img.rgba,
                    );
                    self.picture_size = (img.width, img.height);
                    match &mut self.texture {
                        Some(t) => t.set(image, egui::TextureOptions::LINEAR),
                        None => {
                            self.texture =
                                Some(ctx.load_texture("video", image, egui::TextureOptions::LINEAR))
                        }
                    }
                    let _ = s.recycle.try_send(img.rgba);
                    s.counters
                        .presented
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            }
        }
        match status {
            Some(st) if st.connected => {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ui, |ui| self.video_ui(ui, &st));
            }
            Some(st) => {
                egui::CentralPanel::default().show(ui, |ui| {
                    ui.add_space(32.0); // status bar
                    self.connecting_ui(ui, &st)
                });
            }
            None => {
                egui::CentralPanel::default().show(ui, |ui| {
                    ui.add_space(32.0); // status bar
                    egui::ScrollArea::vertical().show(ui, |ui| self.home_ui(ui));
                });
            }
        }
        ctx.request_repaint_after(Duration::from_millis(250));
    }
}

/// Big on-screen digits: soft keyboards are unreliable in a NativeActivity.
fn keypad(ui: &mut egui::Ui, code: &mut String) {
    let size = egui::vec2(72.0, 52.0);
    egui::Grid::new("keypad")
        .spacing(egui::vec2(8.0, 8.0))
        .show(ui, |ui| {
            for row in [
                ["1", "2", "3"],
                ["4", "5", "6"],
                ["7", "8", "9"],
                ["⌫", "0", "C"],
            ] {
                for key in row {
                    if ui
                        .add(egui::Button::new(egui::RichText::new(key).size(22.0)).min_size(size))
                        .clicked()
                    {
                        match key {
                            "⌫" => {
                                code.pop();
                            }
                            "C" => code.clear(),
                            d if code.len() < 6 => code.push_str(d),
                            _ => {}
                        }
                    }
                }
                ui.end_row();
            }
        });
}

/// Counters can move while the screen shows an old picture (MTK tablet,
/// 2026-11-15: 20 fps "shown", the mirrored clock minutes behind). The
/// picture's own timestamp tells.
fn stale_picture_warning(status: &session::Status) -> Option<String> {
    (status.first_frame && status.picture_age_ms > 2000.0).then(|| {
        format!(
            "The picture on screen is {:.1} s old: the decoder hands out stale images (reader behind {:.1} s).",
            status.picture_age_ms / 1000.0,
            status.reader_behind_ms / 1000.0
        )
    })
}

/// What the video area shows (☰ menu). The no-video modes ask the presenter to stop encoding
/// (`video_paused`, protocol 12).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ViewMode {
    Video,
    /// "Wacom pen (no video)": an empty surface in the presenter's aspect ratio; pen on.
    Pen,
    /// "PowerPoint remote only (no video)": large remote buttons.
    Remote,
}

/// "Wacom pen (no video)": an empty surface in the presenter's aspect ratio that stands for its
/// whole screen. Returns it as the rectangle pen positions are mapped through.
fn pen_surface(ui: &egui::Ui, area: egui::Rect, status: &session::Status) -> Option<egui::Rect> {
    let stream = status.stream.as_ref()?;
    // Leave room for the ☰ button above and the hint below.
    let inner = area.shrink2(egui::vec2(16.0, 56.0));
    let (l, t, w, h) = mirrorwire_proto::control::fit_rect(
        (inner.left(), inner.top(), inner.width(), inner.height()),
        stream.width,
        stream.height,
    )?;
    let surface = egui::Rect::from_min_size(egui::pos2(l, t), egui::vec2(w, h));
    let painter = ui.painter();
    painter.rect_filled(surface, 6.0, egui::Color32::from_gray(28));
    painter.rect_stroke(
        surface,
        6.0,
        egui::Stroke::new(2.0, egui::Color32::from_gray(90)),
        egui::StrokeKind::Inside,
    );
    painter.text(
        surface.center_bottom() + egui::vec2(0.0, 22.0),
        egui::Align2::CENTER_CENTER,
        format!(
            "✏ Wacom pen: this area is the presenter's screen ({}x{}). No video is sent.",
            stream.width, stream.height
        ),
        egui::FontId::proportional(15.0),
        egui::Color32::from_gray(160),
    );
    Some(surface)
}

/// "PowerPoint remote only (no video)": the four remote keys as large buttons.
fn remote_only_buttons(ui: &mut egui::Ui, area: egui::Rect, status: &session::Status) {
    use mirrorwire_proto::control::{Control, RemoteKey};
    let send = |key: RemoteKey| {
        if let Some(tx) = &status.control {
            let _ = tx.send(Control::RemoteKey { key });
        }
    };
    let c = area.center();
    let (big, small, gap) = (egui::vec2(280.0, 200.0), egui::vec2(280.0, 80.0), 16.0);
    let rows = [
        (
            RemoteKey::Previous,
            egui::pos2(c.x - big.x - gap / 2.0, c.y - big.y),
            big,
            36.0,
        ),
        (
            RemoteKey::Next,
            egui::pos2(c.x + gap / 2.0, c.y - big.y),
            big,
            36.0,
        ),
        (
            RemoteKey::Space,
            egui::pos2(c.x - small.x - gap / 2.0, c.y + gap),
            small,
            24.0,
        ),
        (
            RemoteKey::Escape,
            egui::pos2(c.x + gap / 2.0, c.y + gap),
            small,
            24.0,
        ),
    ];
    for (key, min, size, text) in rows {
        let r = egui::Rect::from_min_size(min, size);
        if ui
            .put(
                r,
                egui::Button::new(egui::RichText::new(key.label()).size(text)),
            )
            .clicked()
        {
            send(key);
        }
    }
}
