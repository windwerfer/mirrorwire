//! "Pen pressure" on the presenter screen (Linux, decisions D26): the system check, the
//! opt-in checkbox, and the fix panel with the commands to copy.

use super::widgets::{copy_with_feedback, show_copy_note};
use super::MirrorApp;
use crate::pen::check::{self, CheckReport, Fix, SystemProbe, WHY};
use eframe::egui;

/// State of the pen pressure section.
pub(super) struct PenUi {
    /// The saved choice (D26: off by default).
    pub enabled: bool,
    /// The last check, if one ran.
    pub check: Option<CheckReport>,
}

impl PenUi {
    pub fn load(config_dir: &std::path::Path) -> Self {
        Self {
            enabled: mirrorwire_net::settings::pen_pressure_saved(config_dir),
            check: None,
        }
    }

    fn passed(&self) -> bool {
        self.check.as_ref().is_some_and(CheckReport::passed)
    }

    /// What the presenter should use right now.
    pub fn active(&self) -> bool {
        self.enabled && self.passed()
    }
}

pub(super) fn pen_pressure_ui(
    ui: &mut egui::Ui,
    app: &mut MirrorApp,
    status: &crate::presenter::CaptureStatus,
) {
    // A saved "on" needs a fresh check before it counts; opening /dev/uinput never prompts.
    if app.pen.enabled && app.pen.check.is_none() {
        app.pen.check = Some(check::run(&SystemProbe));
    }
    ui.add_space(8.0);
    egui::CollapsingHeader::new("Pen pressure")
        .id_salt("pen-pressure")
        .default_open(app.pen.enabled)
        .show(ui, |ui| {
            ui.weak(
                "Optional. Lets drawing apps such as Krita see how hard the viewer's pen \
                 presses. Off by default.",
            );
            if ui
                .button("Check pen pressure support on my system")
                .clicked()
            {
                app.pen.check = Some(check::run(&SystemProbe));
            }
            let passed = app.pen.passed();
            let mut shown = app.pen.enabled && passed;
            let toggle = ui
                .add_enabled(
                    passed,
                    egui::Checkbox::new(&mut shown, "Enable pen pressure"),
                )
                .on_disabled_hover_text("Run the check first; it has to pass.");
            if toggle.changed() {
                app.pen.enabled = shown;
                if let Err(e) = mirrorwire_net::settings::save_pen_pressure(&app.config_dir, shown)
                {
                    app.error = Some(format!("Could not save the pen pressure setting: {e:#}"));
                }
            }
            if let Some(h) = &app.presenting {
                if status.pen_pressure != app.pen.active() {
                    h.set_pen_pressure(app.pen.active());
                }
            }
            if let Some(report) = app.pen.check.clone() {
                result_ui(ui, &report);
                if let Some(fix) = report.fix() {
                    fix_ui(ui, app, &fix);
                }
            }
            if app.pen.active() {
                ui.weak(
                    "With more than one screen, choose the shared screen for \
                     \"MirrorWire virtual pen\" in the desktop's tablet settings \
                     (KDE: System Settings → Drawing Tablet).",
                );
            }
            if let Some(note) = &status.pen_note {
                ui.weak(note);
            }
        });
}

fn result_ui(ui: &mut egui::Ui, report: &CheckReport) {
    let color = if report.passed() {
        egui::Color32::from_rgb(0, 180, 80)
    } else {
        egui::Color32::from_rgb(230, 160, 40)
    };
    ui.colored_label(color, report.headline());
    for line in report.details() {
        ui.weak(format!("• {line}"));
    }
}

fn fix_ui(ui: &mut egui::Ui, app: &mut MirrorApp, fix: &Fix) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.strong("How to turn it on");
        ui.label(if fix.in_flatpak {
            "Run these commands in a terminal (they ask for your password), then restart \
             MirrorWire and check again:"
        } else {
            "Run these commands in a terminal (they ask for your password), then check again:"
        });
        commands_ui(ui, app, "pen-fix", &fix.commands, "Copy commands");
        ui.add_space(6.0);
        ui.strong("Why is this needed?");
        ui.add(egui::Label::new(WHY).wrap());
        ui.add_space(6.0);
        ui.strong("What changes");
        ui.add(egui::Label::new(fix.what_changes()).wrap());
        ui.add_space(6.0);
        ui.strong("How to undo");
        commands_ui(ui, app, "pen-undo", &fix.undo, "Copy undo commands");
    });
}

/// A read-only, selectable command box with a copy button.
fn commands_ui(ui: &mut egui::Ui, app: &mut MirrorApp, id: &str, text: &str, button: &str) {
    let mut view = text;
    ui.add(
        egui::TextEdit::multiline(&mut view)
            .id_salt(id)
            .code_editor()
            .desired_width(f32::INFINITY)
            .desired_rows(text.lines().count().max(1)),
    );
    if ui.button(button).clicked() {
        copy_with_feedback(ui.ctx(), text.to_owned(), &mut app.copy_note);
    }
    show_copy_note(ui, &app.copy_note);
}
