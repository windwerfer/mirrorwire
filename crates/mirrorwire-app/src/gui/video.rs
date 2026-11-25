//! The video screen of the viewer window (moved out of `gui/mod.rs`).

use super::*;

impl MirrorApp {
    pub(super) fn video_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let Some(handle) = &self.watching else { return };
        let mut latest = None;
        while let Ok(f) = handle.frames.try_recv() {
            latest = Some(f);
        }
        if let Some(f) = latest {
            self.display_latency_ms = mirrorwire_net::transport::ms(
                mirrorwire_proto::clock::now_us(),
                f.capture_us_viewer,
            );
            match f.picture {
                Picture::Rgba(rgba) => {
                    let image = egui::ColorImage::from_rgba_unmultiplied(
                        [f.width as usize, f.height as usize],
                        &rgba,
                    );
                    let filter = if self.pixel_exact {
                        egui::TextureOptions::NEAREST
                    } else {
                        egui::TextureOptions::LINEAR
                    };
                    match &mut self.texture {
                        Some(tex) => tex.set(image, filter),
                        None => self.texture = Some(ctx.load_texture("video", image, filter)),
                    }
                    self.yuv = None;
                }
                Picture::Yuv(img) => {
                    self.yuv = Some(Arc::new(img));
                    self.texture = None;
                }
            }
        }
        let status = handle.status.lock_ok().clone();
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
        let ppp = ui.ctx().pixels_per_point();
        let mut scale_info = String::new();
        let mut picture_rect: Option<egui::Rect> = None;
        let (tex_w, tex_h) = if let Some(tex) = &self.texture {
            (tex.size()[0] as f32, tex.size()[1] as f32)
        } else if let Some(yuv) = &self.yuv {
            (yuv.width as f32, yuv.height as f32)
        } else {
            (0.0, 0.0)
        };
        if self.view_mode == ViewMode::Pen {
            picture_rect = pen_surface(ui, rect, &status);
        }
        if self.view_mode == ViewMode::Video && tex_w > 0.0 && tex_h > 0.0 {
            let native_pts = egui::vec2(tex_w / ppp, tex_h / ppp);
            let draw_size = if self.pixel_exact {
                native_pts
            } else {
                let fit = (rect.width() / native_pts.x).min(rect.height() / native_pts.y);
                native_pts * fit
            };
            let draw = egui::Rect::from_center_size(rect.center(), draw_size);
            picture_rect = Some(draw);
            if let Some(yuv) = &self.yuv {
                ui.painter().add(
                    YuvPaint::new(yuv.clone(), self.yuv_gpu.clone(), self.pixel_exact)
                        .callback(draw),
                );
            } else if let Some(tex) = &self.texture {
                ui.painter().image(
                    tex.id(),
                    draw,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            }
            if status.cursor_mode == Some(CursorMode::Separate) {
                if let Some(cursor) = &status.cursor {
                    let current = cursor.lock_ok().current();
                    if let Some((p, img)) = current {
                        self.draw_cursor(ui, &p, &img, draw);
                    }
                }
            }
            let drawn_px = draw_size * ppp;
            let sx = drawn_px.x / tex_w;
            let sy = drawn_px.y / tex_h;
            let convert = if self.yuv.is_some() {
                "GL YUV shader"
            } else {
                "CPU RGBA"
            };
            scale_info = format!(
                "texture {tex_w:.0}x{tex_h:.0}  window {:.0}x{:.0} px  ppp {ppp:.2}  scale {sx:.2}x/{sy:.2}x  {}  {convert}",
                rect.width() * ppp,
                rect.height() * ppp,
                if self.pixel_exact { "1:1 nearest" } else { "fit linear" },
            );
        }
        let updates = status
            .cursor
            .as_ref()
            .map(|c| c.lock_ok().updates)
            .unwrap_or(0);
        let elapsed = self.cursor_rate.0.elapsed().as_secs_f64();
        if elapsed >= 1.0 {
            self.cursor_rate = (
                Instant::now(),
                updates,
                updates.saturating_sub(self.cursor_rate.1) as f64 / elapsed,
            );
        }
        let cursor_line = match status.cursor_mode {
            Some(CursorMode::Separate) => {
                format!("cursor separate ({:.0} updates/s)", self.cursor_rate.2)
            }
            Some(CursorMode::Embedded) => "cursor embedded (in video)".to_owned(),
            Some(CursorMode::Hidden) => "cursor hidden".to_owned(),
            None => "cursor ?".to_owned(),
        };
        if self.show_stats {
            let stream = status.stream.as_ref();
            let text = format!(
                "{}x{}  source {:.1} fps  {:.2} Mbit/s  {} ({})  {}\n{}  decode {:.2} ms  {}  capture->texture {:.1} ms\nwindow: {}\n{}\n{}\n{}\n{}\nS: stats  R: presenter remote  1: 1:1 pixels  F / double-click: fullscreen  Esc: disconnect",
                status.native_width.max(stream.map(|s| s.width).unwrap_or(0)),
                status.native_height.max(stream.map(|s| s.height).unwrap_or(0)),
                status.source_fps,
                status.bitrate_mbps,
                stream.map(|s| s.codec.as_str()).unwrap_or("h264"),
                stream.map(|s| s.encoder.as_str()).unwrap_or("?"),
                cursor_line,
                status.decoder,
                status.decode_ms,
                self.renderer.label(),
                self.display_latency_ms,
                self.display_server.as_deref().unwrap_or("?"),
                status.last_second,
                status.link.line(),
                status.bottleneck,
                scale_info,
            );
            ui.painter().text(
                rect.left_top() + egui::vec2(8.0, 8.0),
                egui::Align2::LEFT_TOP,
                text,
                egui::FontId::monospace(12.0),
                egui::Color32::from_rgb(0, 255, 120),
            );
        }

        self.view_menu(&ctx, &status);
        if self.view_mode == ViewMode::Remote {
            remote_only_buttons(&ctx, &status);
        } else if self.show_remote {
            egui::Area::new(egui::Id::unique("remote_buttons"))
                .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -16.0))
                .show(&ctx, |ui| {
                    ui.horizontal(|ui| {
                        let pen = egui::Button::new(egui::RichText::new("Pen").size(22.0))
                            .min_size(egui::vec2(110.0, 52.0))
                            .selected(self.pen_mode);
                        if ui.add(pen).clicked() {
                            self.pen_mode = !self.pen_mode;
                            // Tell the presenter at once (protocol 12): it asks its desktop
                            // for pointer permission now, not on the first stroke.
                            if let Some(tx) = &status.control {
                                let _ = tx.send(mirrorwire_proto::control::Control::PenMode {
                                    on: self.pen_mode,
                                });
                            }
                        }
                        for key in mirrorwire_proto::control::RemoteKey::ALL {
                            let button =
                                egui::Button::new(egui::RichText::new(key.label()).size(22.0))
                                    .min_size(egui::vec2(110.0, 52.0));
                            if ui.add(button).clicked() {
                                if let Some(tx) = &status.control {
                                    let _ =
                                        tx.send(mirrorwire_proto::control::Control::RemoteKey {
                                            key,
                                        });
                                }
                            }
                        }
                    });
                });
        }
        let response = ui.interact(
            rect,
            ui.scope_id().with("video"),
            egui::Sense::click_and_drag(),
        );
        self.send_pen(&ctx, &response, picture_rect, &status);
        // Never call Response methods inside `ctx.input(..)`: on a click,
        // `double_clicked()` takes the context lock again, and a nested read
        // lock deadlocks as soon as the decode thread is waiting to write
        // (`request_repaint`). That froze the iMac viewer on a click.
        let double_clicked = response.double_clicked() && !self.pen_mode;
        let (toggle_fs, toggle_stats, toggle_1to1, leave, toggle_remote) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::F) || double_clicked,
                i.key_pressed(egui::Key::S),
                i.key_pressed(egui::Key::Num1),
                i.key_pressed(egui::Key::Escape),
                i.key_pressed(egui::Key::R),
            )
        });
        if toggle_remote {
            self.show_remote = !self.show_remote;
        }
        if toggle_stats {
            self.show_stats = !self.show_stats;
        }
        if toggle_1to1 {
            self.pixel_exact = !self.pixel_exact;
        }
        if toggle_fs {
            self.fullscreen = !self.fullscreen;
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.fullscreen));
        }
        let failed = status.error.clone();
        if leave || failed.is_some() && self.has_picture() && !status.connected {
            self.error = failed;
            self.disconnect(&ctx);
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(16));
    }

    /// The ☰ menu at the top right: the two no-video modes.
    fn view_menu(&mut self, ctx: &egui::Context, status: &crate::viewer::ViewerStatus) {
        let mut chosen = None;
        egui::Area::new(egui::Id::unique("view_menu"))
            .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-12.0, 12.0))
            .show(ctx, |ui| {
                ui.menu_button(egui::RichText::new("☰").size(24.0), |ui| {
                    let mut pen = self.view_mode == ViewMode::Pen;
                    if ui.checkbox(&mut pen, "Wacom pen (no video)").changed() {
                        chosen = Some(if pen { ViewMode::Pen } else { ViewMode::Video });
                    }
                    let mut remote = self.view_mode == ViewMode::Remote;
                    if ui
                        .checkbox(&mut remote, "PowerPoint remote only (no video)")
                        .changed()
                    {
                        chosen = Some(if remote {
                            ViewMode::Remote
                        } else {
                            ViewMode::Video
                        });
                    }
                    ui.weak("Without video the presenter encodes and sends nothing.");
                });
            });
        if let Some(mode) = chosen {
            self.set_view_mode(mode, status);
        }
    }

    /// Switch the video area's mode. The pen is on exactly in "Wacom pen" mode (switching
    /// into it asks the presenter's desktop for pointer permission, protocol 12).
    fn set_view_mode(&mut self, mode: ViewMode, status: &crate::viewer::ViewerStatus) {
        if mode == self.view_mode {
            return;
        }
        let pen = mode == ViewMode::Pen;
        let pen_changed = pen != self.pen_mode && (pen || self.view_mode == ViewMode::Pen);
        self.view_mode = mode;
        if pen_changed {
            self.pen_mode = pen;
            if let Some(tx) = &status.control {
                let _ = tx.send(mirrorwire_proto::control::Control::PenMode { on: pen });
            }
        }
    }

    /// Pen/touch/mouse as the presenter's pointer (pen mode only): down/up on the reliable
    /// control stream, moves coalesced to about 60 per second.
    fn send_pen(
        &mut self,
        ctx: &egui::Context,
        response: &egui::Response,
        picture: Option<egui::Rect>,
        status: &crate::viewer::ViewerStatus,
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
        let pos = ctx.input(|i| i.pointer.interact_pos().or(i.pointer.latest_pos()));
        let (pressed, released) = ctx.input(|i| {
            (
                i.pointer.primary_pressed(),
                i.pointer.primary_released() || !i.pointer.primary_down(),
            )
        });
        let Some(pos) = pos else { return };
        if !response.contains_pointer() && !self.pen_down {
            return;
        }
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
        } else if self.pen_down && released {
            self.pen_down = false;
            PointerPhase::Up
        } else if self.pen_down {
            let now = Instant::now();
            if self
                .pen_last_move
                .is_some_and(|t| now.duration_since(t) < std::time::Duration::from_millis(16))
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

    /// The presenter's cursor as a textured quad over the video, mapped
    /// through the same rectangle the video is drawn in.
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

/// "Wacom pen (no video)": an empty surface in the presenter's aspect ratio that stands for its
/// whole screen. Returns it as the rectangle pen positions are mapped through.
fn pen_surface(
    ui: &egui::Ui,
    area: egui::Rect,
    status: &crate::viewer::ViewerStatus,
) -> Option<egui::Rect> {
    let stream = status.stream.as_ref();
    let w = status
        .native_width
        .max(stream.map(|s| s.width).unwrap_or(0));
    let h = status
        .native_height
        .max(stream.map(|s| s.height).unwrap_or(0));
    // Leave room for the ☰ button above and the hint below.
    let inner = area.shrink2(egui::vec2(16.0, 56.0));
    let (l, t, fw, fh) = mirrorwire_proto::control::fit_rect(
        (inner.left(), inner.top(), inner.width(), inner.height()),
        w,
        h,
    )?;
    let surface = egui::Rect::from_min_size(egui::pos2(l, t), egui::vec2(fw, fh));
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
        format!("✏ Wacom pen: this area is the presenter's screen ({w}x{h}). No video is sent."),
        egui::FontId::proportional(15.0),
        egui::Color32::from_gray(160),
    );
    Some(surface)
}

/// "PowerPoint remote only (no video)": the four remote keys as large buttons.
fn remote_only_buttons(ctx: &egui::Context, status: &crate::viewer::ViewerStatus) {
    use mirrorwire_proto::control::{Control, RemoteKey};
    egui::Area::new(egui::Id::unique("remote_only"))
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.label(
                    egui::RichText::new("PowerPoint remote (no video)")
                        .size(20.0)
                        .color(egui::Color32::from_gray(170)),
                );
                ui.add_space(16.0);
                let send = |key: RemoteKey| {
                    if let Some(tx) = &status.control {
                        let _ = tx.send(Control::RemoteKey { key });
                    }
                };
                ui.horizontal(|ui| {
                    for key in [RemoteKey::Previous, RemoteKey::Next] {
                        let b = egui::Button::new(egui::RichText::new(key.label()).size(36.0))
                            .min_size(egui::vec2(260.0, 180.0));
                        if ui.add(b).clicked() {
                            send(key);
                        }
                    }
                });
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    for key in [RemoteKey::Space, RemoteKey::Escape] {
                        let b = egui::Button::new(egui::RichText::new(key.label()).size(24.0))
                            .min_size(egui::vec2(260.0, 72.0));
                        if ui.add(b).clicked() {
                            send(key);
                        }
                    }
                });
            });
        });
}
