//! The presenter screen and its quality panel (moved out of `gui/mod.rs`).

use super::*;

impl MirrorApp {
    pub(super) fn present_ui(&mut self, ui: &mut egui::Ui) {
        let Some(handle) = &self.presenting else {
            self.screen = Screen::Home;
            return;
        };
        let status = handle.poll();
        if status.capture.remote_asking {
            pen_permission_dialog(ui.ctx());
        }

        if ui.button("Stop presenting").clicked() {
            self.stop_presenting();
            self.screen = Screen::Home;
            return;
        }

        // Everything below scrolls, so small windows and laptop screens can
        // always reach the controls; the pairing code and QR stay first.
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(8.0);
                ui.heading(format!("Presenting: {}", status.name));
                ui.add_space(8.0);

                match status.phase {
                    PresentPhase::Starting | PresentPhase::WaitingForCapture => {
                        ui.label("Waiting for the screen share dialog…");
                        ui.weak("Choose the display or window to share, then come back to this window for the pairing code.");
                        firewall_panel(ui, self, status.firewall.as_ref(), false);
                    }
                    PresentPhase::Streaming => {
                        if let Some(code) = &status.pairing_code {
                            ui.label("Pairing code (first connection only):");
                            ui.label(egui::RichText::new(code).monospace().size(42.0).strong());
                        } else {
                            ui.label("Pairing is off; any viewer can connect.");
                        }
                        ui.add_space(8.0);
                        if let Some(uri) = &status.uri {
                            if self.qr_texture.is_none() {
                                match qr_color_image(uri) {
                                    Ok(image) => {
                                        self.qr_texture = Some(ui.ctx().load_texture(
                                            "presenter-qr",
                                            image,
                                            egui::TextureOptions::NEAREST,
                                        ));
                                    }
                                    Err(e) => self.error = Some(format!("QR code: {e:#}")),
                                }
                            }
                            if let Some(tex) = &self.qr_texture {
                                let side = 240.0;
                                ui.image((tex.id(), egui::vec2(side, side)));
                            }
                            ui.add_space(4.0);
                            ui.monospace(uri);
                            if ui.button("Copy link").clicked() {
                                copy_with_feedback(ui.ctx(), uri.clone(), &mut self.copy_note);
                            }
                            show_copy_note(ui, &self.copy_note);
                        }
                        ui.add_space(8.0);
                        let silent = status.capture.fec.viewer_silent_for();
                        if let (true, Some(quiet)) = (status.viewer_connected, silent) {
                            ui.colored_label(
                                egui::Color32::from_rgb(220, 140, 0),
                                format!(
                                    "The viewer has not responded for {:.0} s (app closed, or the \
                                     network dropped). Video is paused; the connection ends \
                                     after 10 s of silence.",
                                    quiet.as_secs_f32()
                                ),
                            );
                        } else if status.viewer_connected && status.capture.video_paused {
                            ui.colored_label(
                                egui::Color32::from_rgb(0, 180, 80),
                                "A viewer is connected in pen or remote-only mode: no video is \
                                 encoded or sent until it shows the picture again.",
                            );
                        } else if status.viewer_connected {
                            ui.colored_label(
                                egui::Color32::from_rgb(0, 180, 80),
                                "A viewer is connected.",
                            );
                        } else {
                            ui.label("Waiting for a viewer…");
                        }
                        firewall_panel(
                            ui,
                            self,
                            status.firewall.as_ref(),
                            status.viewer_connected || !status.attempts.is_empty(),
                        );
                        if status.viewer_connected {
                            ui.add_space(4.0);
                            ui.label(status.capture.fec.bitrate_line());
                        }
                        // Extend mode caps the encoded size at the chosen size (Auto: the viewer's display).
                        if let Some(h) = &self.presenting {
                            let cap = &status.capture;
                            let max = (cap.prefs.mode == mirrorwire_media::source::CaptureMode::Extend)
                                .then(|| {
                                    let viewer = cap.viewer_display.map(|d| (d.width, d.height));
                                    cap.prefs.extend_size.resolve(viewer)
                                })
                                .flatten();
                            h.set_max_size(max);
                        }
                        quality_controls(ui, self, &status.settings);
                        #[cfg(target_os = "linux")]
                        super::pen::pen_pressure_ui(ui, self, &status.capture);
                        ui.add_space(8.0);
                        // Diagnostics stay one click away; the essentials above fit a laptop screen.
                        egui::CollapsingHeader::new("Details")
                            .id_salt("presenter-details")
                            .default_open(false)
                            .show(ui, |ui| {
                            if let Some(addr) = &status.listen_addr {
                                ui.weak(format!("Listening on {addr}"));
                            }
                            if let (Some(enc), Some(src)) = (&status.encoder, &status.source) {
                                // The HUD name follows live encoder switches; `status.encoder` is the first one.
                                let enc = if status.hud.encoder.is_empty() {
                                    enc
                                } else {
                                    &status.hud.encoder
                                };
                                ui.weak(format!("Encoder: {enc}  ·  Source: {src}"));
                            }
                            if let Some(fp) = &status.fingerprint {
                                ui.weak(format!("Fingerprint: {}", short_fingerprint(fp)));
                            }
                            ui.weak(format!("UDP port {}", status.port));
                            if !status.advertised.is_empty() {
                                ui.weak(format!("Advertised: {}", status.advertised.join(", ")));
                            }
                            if let Some(sum) = &status.rc_summary {
                                ui.weak(sum);
                            }
                            let hud = &status.hud;
                            let path = if hud.path.is_empty() {
                                status.conversion.as_deref().unwrap_or("?")
                            } else {
                                hud.path.as_str()
                            };
                            let zero_copy = path == mirrorwire_media::encode::path::DMABUF_ZERO_COPY;
                            ui.horizontal(|ui| {
                                ui.weak("Capture path:");
                                if zero_copy {
                                    ui.colored_label(egui::Color32::from_rgb(0, 180, 80), path);
                                } else {
                                    ui.colored_label(egui::Color32::from_rgb(230, 160, 40), path);
                                }
                            });
                            if let Some(note) = &hud.fallback_note {
                                ui.colored_label(egui::Color32::from_rgb(230, 160, 40), note);
                            }
                            let cap = &status.capture;
                            ui.horizontal(|ui| {
                                ui.weak("Cursor:");
                                let text = match cap.cursor_mode {
                                    CursorMode::Separate => "separate (sent as coordinates, drawn by the viewer)",
                                    CursorMode::Embedded => "embedded (drawn into the video)",
                                    CursorMode::Hidden => "hidden",
                                };
                                let color = if cap.cursor_mode == CursorMode::Separate || cap.prefs.cursor != CursorPref::Separate {
                                    egui::Color32::from_rgb(0, 180, 80)
                                } else {
                                    egui::Color32::from_rgb(230, 160, 40)
                                };
                                ui.colored_label(color, text);
                            });
                            if let Some(note) = &cap.cursor_note {
                                ui.colored_label(egui::Color32::from_rgb(230, 160, 40), note);
                            }
                            if let Some(note) = &cap.remote_note {
                                ui.weak(note);
                            }
                            if cap.prefs.mode == mirrorwire_media::source::CaptureMode::Extend {
                                let viewer = cap.viewer_display.map(|d| (d.width, d.height));
                                let target = cap.prefs.extend_size.resolve(viewer);
                                match target {
                                    Some((w, h)) => {
                                        ui.weak(format!(
                                            "Extend: target size {w}x{h}{}. Encoding is capped to this size. The virtual monitor keeps the desktop's default size; set {w}x{h} for it in System Settings → Display.",
                                            if cap.prefs.extend_size == ExtendSize::Auto { " (the viewer's display)" } else { "" }
                                        ));
                                    }
                                    None => {
                                        ui.weak("Extend: waiting for a viewer to learn its display size, or pick a size above.");
                                    }
                                }
                            }
                            ui.weak(format!(
                                "Frame rate: asked up to {} fps  ·  compositor allows {}  ·  encoder runs at {} fps",
                                cap.fps_requested,
                                cap.fps_negotiated.map(|f| format!("{f} fps")).unwrap_or_else(|| "? (not reported)".into()),
                                status.settings.fps
                            ));
                            ui.weak(fec_status_line(&cap.fec, status.viewer_connected));
                            if hud.capture_w > 0 {
                                ui.weak(format!(
                                    "Capture {}x{}  →  encode {}x{}  ·  source {:.1} fps (+{:.1} repeats/s)  ·  map/pack {:.1} ms  convert {:.1} ms  upload {:.1} ms  encode {:.1} ms (avg {:.1} ms)",
                                    hud.capture_w,
                                    hud.capture_h,
                                    status.settings.width,
                                    status.settings.height,
                                    hud.source_fps,
                                    hud.repeat_fps,
                                    hud.pack_ms,
                                    hud.convert_ms,
                                    hud.upload_ms,
                                    hud.encode_ms,
                                    hud.encode_avg_ms
                                ));
                            }
                                attempts_panel(ui, &status.attempts);
                            });
                        log_panel(ui, &self.log, &mut self.copy_note);
                    }
                    PresentPhase::Failed => {
                        ui.colored_label(
                            egui::Color32::LIGHT_RED,
                            status.error.as_deref().unwrap_or("presenter failed"),
                        );
                    }
                    PresentPhase::Stopped => {
                        ui.label("Presenter stopped.");
                    }
                }

                if let Some(err) = &self.error {
                    ui.colored_label(egui::Color32::LIGHT_RED, err);
                }
                self.footer(ui);
            });
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(250));
    }
}

/// Shown while the desktop asks whether the viewer's pen may control the pointer (the viewer
/// switched its pen on; the capture reopens with input). Closes by itself once answered.
fn pen_permission_dialog(ctx: &egui::Context) {
    egui::Window::new("✏ The viewer switched on the pen")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .show(ctx, |ui| {
            ui.label(
                "Your desktop now asks whether MirrorWire may control the pointer and \
                 keyboard (and which screen to share with it). Answer its dialog.",
            );
            ui.label(
                "Tick \"Allow restoring\" (KDE) or \"Remember\" there, so it is not asked again: \
                 the next presenter start then includes the pen without a dialog, and the ✏ \
                 next to the Presenter button on the start screen turns green.",
            );
            ui.weak("The picture pauses for a moment while the capture reopens.");
        });
    // The answer arrives from the portal thread; look again soon.
    ctx.request_repaint_after(std::time::Duration::from_millis(300));
}

pub(super) fn quality_controls(ui: &mut egui::Ui, app: &mut MirrorApp, live: &EncoderConfig) {
    ui.add_space(8.0);
    ui.strong("Quality");
    ui.horizontal(|ui| {
        ui.label("Encoder");
        egui::ComboBox::from_id_salt("encoder")
            .selected_text(app.encoder.as_str())
            .show_ui(ui, |ui| {
                for b in EncoderBackend::selectable() {
                    ui.selectable_value(&mut app.encoder, b, b.as_str());
                }
            });
        ui.label("Bitrate max (Mbit/s)");
        ui.add(
            egui::DragValue::new(&mut app.bitrate_mbps)
                .range(1.0..=80.0)
                .speed(0.5),
        );
        ui.label("RC");
        egui::ComboBox::from_id_salt("rc")
            .selected_text(app.rc.as_str())
            .show_ui(ui, |ui| {
                for rc in RateControl::all() {
                    ui.selectable_value(&mut app.rc, *rc, rc.as_str());
                }
            });
        if matches!(app.rc, RateControl::Cqp | RateControl::Icq) {
            ui.label("QP");
            ui.add(egui::DragValue::new(&mut app.qp).range(10..=40));
        }
        ui.label("Keyint (s)");
        ui.add(egui::DragValue::new(&mut app.keyint_sec).range(1..=60));
        if ui.button("Apply").clicked() {
            if let Some(h) = &app.presenting {
                let mut cfg = live.clone();
                cfg.bitrate_kbps = (app.bitrate_mbps * 1000.0).round().max(1.0) as u32;
                cfg.rate_control = app.rc;
                cfg.qp = app.qp;
                cfg.keyint = Some(app.keyint_sec.saturating_mul(cfg.fps.max(1)));
                cfg.backend = app.encoder;
                h.apply_settings(cfg);
            }
        }
    });
    if matches!(app.rc, RateControl::Cqp | RateControl::Icq) {
        ui.label(
            egui::RichText::new(CQP_ICQ_NOTE)
                .small()
                .color(ui.visuals().warn_fg_color),
        );
    }
    ui.horizontal(|ui| {
        let before = app.capture;
        ui.label("Mode");
        egui::ComboBox::from_id_salt("capture-mode")
            .selected_text(app.capture.mode.as_str())
            .show_ui(ui, |ui| {
                for m in mirrorwire_media::source::CaptureMode::all() {
                    ui.selectable_value(&mut app.capture.mode, m, m.as_str());
                }
            });
        if app.capture.mode == mirrorwire_media::source::CaptureMode::Extend {
            ui.label("Size");
            egui::ComboBox::from_id_salt("extend-size")
                .selected_text(app.capture.extend_size.label())
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut app.capture.extend_size,
                        ExtendSize::Auto,
                        ExtendSize::Auto.label(),
                    );
                    for (w, h) in EXTEND_SIZES {
                        ui.selectable_value(
                            &mut app.capture.extend_size,
                            ExtendSize::Fixed(w, h),
                            format!("{w}x{h}"),
                        );
                    }
                });
        }
        ui.label("Cursor");
        egui::ComboBox::from_id_salt("cursor")
            .selected_text(app.capture.cursor.as_str())
            .show_ui(ui, |ui| {
                for c in CursorPref::all() {
                    ui.selectable_value(&mut app.capture.cursor, c, c.as_str());
                }
            });
        ui.label("Max fps");
        egui::ComboBox::from_id_salt("max-fps")
            .selected_text(app.capture.fps.label())
            .show_ui(ui, |ui| {
                for f in [FpsPref::Fixed(30), FpsPref::Fixed(60), FpsPref::Max] {
                    ui.selectable_value(&mut app.capture.fps, f, f.label());
                }
            });
        ui.label("FEC");
        egui::ComboBox::from_id_salt("fec")
            .selected_text(app.capture.fec.as_string())
            .show_ui(ui, |ui| {
                for m in FEC_CHOICES {
                    ui.selectable_value(&mut app.capture.fec, m, m.as_string());
                }
            });
        ui.checkbox(&mut app.capture.adaptive, "Adaptive bitrate");
        if app.capture != before {
            if let Some(h) = &app.presenting {
                h.apply_capture(app.capture);
            }
        }
    });
    ui.weak("FEC adds parity packets so frames survive Wi-Fi loss without a keyframe round trip (auto adapts to the loss the viewer reports, up to +35 % / +60 % for keyframes). Parity is paid from the bitrate, so the video gets a bit less. Adaptive bitrate treats the bitrate as a maximum: it lowers the rate when the viewer reports loss or the delay grows, and raises it slowly when the link is clean. Both apply to the next frame and are saved.");
    ui.weak("Mode: mirror shares an existing screen; extend asks the desktop for a virtual monitor (KDE Plasma 6, GNOME) that you arrange in the system display settings, where its resolution and position are also set. If the desktop cannot, mirror is used and the note shows why. Changing it re-opens the screen-share dialog once per mode.");
    ui.weak("Cursor and Max fps apply at once (the capture restarts without a dialog) and are saved. separate: the viewer draws the cursor from coordinates, so it moves without waiting for video; embedded: the old way, for viewers or desktops where separate misbehaves.");
    ui.weak("Apply restarts the encoder with an IDR. filmcraft is the optional pure-Rust encoder (CPU, for comparison; auto never picks it). Switching away from vaapi uses shared-memory capture until the next start. 4:4:4 chroma is not available with VAAPI on UHD 620 or with VideoToolbox decode.");
}
