//! The Miracast screen (decision D21, feature `miracast`): instruction and waiting state with
//! Cancel, the normal video view once pictures arrive, and a result page with Try again.
//! What to show comes from the pure state machine in `crate::miracast_flow`.

use super::widgets::{copy_with_feedback, show_copy_note};
use super::MirrorApp;
use crate::miracast_flow::{view, View};
use crate::sync::LockExt;
use eframe::egui;

impl MirrorApp {
    pub(super) fn miracast_ui(&mut self, ui: &mut egui::Ui) {
        let Some(session) = &self.miracast_session else {
            self.go_home(ui.ctx());
            return;
        };
        let phase = session.phase();
        let first_frame = self
            .watching
            .as_ref()
            .is_some_and(|h| h.status.lock_ok().first_frame);
        let shown = view(&phase, &self.name, self.has_picture() || first_frame);
        if phase.is_active() && self.watching.is_none() {
            // Left the video with Esc: the stop is already requested.
            self.go_home(ui.ctx());
            return;
        }
        match shown {
            View::Video => {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ui, |ui| self.video_ui(ui));
            }
            View::Wait { title, body } => {
                egui::CentralPanel::default().show(ui, |ui| {
                    ui.add_space(40.0);
                    ui.vertical_centered(|ui| {
                        ui.heading(title);
                        ui.add_space(16.0);
                        ui.add(egui::Label::new(egui::RichText::new(&body).size(16.0)).wrap());
                        ui.add_space(16.0);
                        ui.add(egui::Spinner::new().size(28.0));
                        ui.add_space(16.0);
                        copy_firewall_command(self, ui, &body);
                        if ui
                            .add(egui::Button::new("Cancel").min_size(egui::vec2(160.0, 32.0)))
                            .clicked()
                        {
                            self.go_home(ui.ctx());
                        }
                    });
                });
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(200));
            }
            View::Done { title, body } => {
                // The session is over: drop the last picture and the viewer handle.
                if self.watching.is_some() {
                    self.disconnect(ui.ctx());
                }
                egui::CentralPanel::default().show(ui, |ui| {
                    ui.add_space(40.0);
                    ui.vertical_centered(|ui| {
                        ui.heading(title);
                        ui.add_space(16.0);
                        ui.add(egui::Label::new(egui::RichText::new(&body).size(16.0)).wrap());
                        ui.add_space(16.0);
                        copy_firewall_command(self, ui, &body);
                        ui.horizontal(|ui| {
                            if ui
                                .add(
                                    egui::Button::new("Try again")
                                        .min_size(egui::vec2(160.0, 32.0)),
                                )
                                .clicked()
                            {
                                let peer = self.miracast.clone().flatten();
                                self.connect_miracast(ui.ctx(), peer);
                            }
                            if ui.button("Back").clicked() {
                                self.go_home(ui.ctx());
                            }
                        });
                    });
                });
            }
        }
    }
}

/// A "Copy command" button when the text carries the firewall fix (NoRtp).
fn copy_firewall_command(app: &mut MirrorApp, ui: &mut egui::Ui, body: &str) {
    if let Some(cmd) = body
        .split("run: ")
        .nth(1)
        .filter(|c| c.starts_with("firewall-cmd"))
    {
        if ui.button("Copy firewall command").clicked() {
            copy_with_feedback(ui.ctx(), cmd.trim().to_owned(), &mut app.copy_note);
        }
        show_copy_note(ui, &app.copy_note);
        ui.add_space(8.0);
    }
}
