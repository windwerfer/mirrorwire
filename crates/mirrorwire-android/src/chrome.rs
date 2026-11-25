//! Corner controls over the video, the same on every viewer: a column of menu, pen and
//! disconnect icons in the top-right corner, always shown, plus the overlay state they and the
//! hamburger menu drive.
//!
//! The desktop viewer has the original of this file (`crates/mirrorwire-app/src/gui/chrome.rs`);
//! keep the two in step. The icons are plain shapes drawn with the egui painter (no icon
//! font or image files).

use eframe::egui;

/// Side of one corner button, in points (Android's old buttons were 44 high).
pub const BUTTON: f32 = 48.0;
/// Space between two corner buttons.
pub const GAP: f32 = 8.0;
/// Space between the buttons and the right screen edge.
pub const MARGIN: f32 = 8.0;
/// Width the corner column takes, including its margins: keep the stats text left of it.
pub const COLUMN_WIDTH: f32 = BUTTON + 2.0 * MARGIN;

/// What the overlay over the video shows. `hidden` is the menu's "Hide": it hides the stats
/// HUD and the PowerPoint bar but never the corner icons, and it does not change the saved
/// `show_stats` choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Overlay {
    pub hidden: bool,
    /// The user's "Show statistics" choice (saved in `settings.json`).
    pub show_stats: bool,
    /// "PowerPoint mode": the presenter-remote key row at the bottom.
    pub powerpoint: bool,
}

impl Overlay {
    pub fn stats_visible(&self) -> bool {
        !self.hidden && self.show_stats
    }

    pub fn powerpoint_visible(&self) -> bool {
        !self.hidden && self.powerpoint
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Menu,
    Pen,
    Disconnect,
}

impl Icon {
    /// Accessible name and tooltip.
    pub fn name(self) -> &'static str {
        match self {
            Icon::Menu => "Menu",
            Icon::Pen => "Pen",
            Icon::Disconnect => "Disconnect",
        }
    }
}

/// What the corner column did this frame.
pub struct Corner {
    /// The hamburger button: anchor for the menu popup.
    pub menu: egui::Response,
    pub pen_clicked: bool,
    pub disconnect_clicked: bool,
}

/// The corner column in the top-right of `screen`, starting `top` points below its top edge.
pub fn corner(ui: &mut egui::Ui, screen: egui::Rect, top: f32, pen_on: bool) -> Corner {
    let x = screen.right() - MARGIN - BUTTON;
    let at = |i: f32| {
        egui::Rect::from_min_size(
            egui::pos2(x, screen.top() + top + i * (BUTTON + GAP)),
            egui::vec2(BUTTON, BUTTON),
        )
    };
    let menu = icon_button(ui, at(0.0), Icon::Menu, false);
    let pen_clicked = icon_button(ui, at(1.0), Icon::Pen, pen_on).clicked();
    let disconnect_clicked = icon_button(ui, at(2.0), Icon::Disconnect, false).clicked();
    Corner {
        menu,
        pen_clicked,
        disconnect_clicked,
    }
}

/// One square icon button over the video, with an accessible name and a tooltip.
pub fn icon_button(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    icon: Icon,
    selected: bool,
) -> egui::Response {
    let response = ui.interact(
        rect,
        ui.scope_id().with(("corner", icon.name())),
        egui::Sense::click(),
    );
    response.widget_info(|| match icon {
        Icon::Pen => egui::WidgetInfo::selected(egui::Role::Button, true, selected, icon.name()),
        _ => egui::WidgetInfo::labeled(egui::Role::Button, true, icon.name()),
    });
    let fill = if selected {
        ui.visuals().selection.bg_fill
    } else if response.is_pointer_button_down_on() {
        egui::Color32::from_gray(90)
    } else if response.hovered() {
        egui::Color32::from_rgba_unmultiplied(60, 60, 60, 220)
    } else {
        egui::Color32::from_black_alpha(170)
    };
    let painter = ui.painter();
    painter.rect(
        rect,
        8.0,
        fill,
        egui::Stroke::new(1.0, egui::Color32::from_gray(120)),
        egui::StrokeKind::Inside,
    );
    let color = match icon {
        Icon::Disconnect => egui::Color32::from_rgb(255, 120, 120),
        _ => egui::Color32::WHITE,
    };
    paint_icon(painter, rect.shrink(BUTTON * 0.22), icon, color);
    response.on_hover_text(icon.name())
}

/// Draw `icon` inside the square `r`.
pub fn paint_icon(painter: &egui::Painter, r: egui::Rect, icon: Icon, color: egui::Color32) {
    let s = r.width().min(r.height());
    let p = |x: f32, y: f32| r.min + egui::vec2(x * s, y * s);
    let stroke = egui::Stroke::new((s * 0.11).max(2.0), color);
    match icon {
        Icon::Menu => {
            for y in [0.18, 0.5, 0.82] {
                painter.line_segment([p(0.05, y), p(0.95, y)], stroke);
            }
        }
        Icon::Pen => {
            // A pencil from the bottom-left (tip) to the top-right.
            let d = egui::vec2(1.0, -1.0).normalized();
            let n = egui::vec2(1.0, 1.0).normalized() * (s * 0.13);
            let tip = p(0.08, 0.92);
            let len = s * 1.2;
            let tip_base = tip + d * (s * 0.3);
            let body_start = tip + d * (s * 0.38);
            let end = tip + d * len;
            painter.add(egui::Shape::convex_polygon(
                vec![tip, tip_base + n, tip_base - n],
                color,
                egui::Stroke::NONE,
            ));
            painter.add(egui::Shape::convex_polygon(
                vec![body_start + n, end + n, end - n, body_start - n],
                color,
                egui::Stroke::NONE,
            ));
        }
        Icon::Disconnect => {
            // A power symbol: a ring open at the top and a bar through the gap.
            let center = p(0.5, 0.55);
            let radius = s * 0.42;
            let gap = 40f32.to_radians();
            let start = -std::f32::consts::FRAC_PI_2 + gap;
            let sweep = std::f32::consts::TAU - 2.0 * gap;
            let points: Vec<egui::Pos2> = (0..=32)
                .map(|i| {
                    let a = start + sweep * i as f32 / 32.0;
                    center + egui::vec2(a.cos(), a.sin()) * radius
                })
                .collect();
            painter.add(egui::Shape::line(points, stroke));
            painter.line_segment([p(0.5, 0.0), p(0.5, 0.5)], stroke);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hide_keeps_choices_and_only_hides() {
        let mut o = Overlay {
            hidden: false,
            show_stats: true,
            powerpoint: true,
        };
        assert!(o.stats_visible() && o.powerpoint_visible());
        o.hidden = true;
        assert!(!o.stats_visible() && !o.powerpoint_visible());
        assert!(o.show_stats && o.powerpoint);
        o.hidden = false;
        o.show_stats = false;
        assert!(!o.stats_visible() && o.powerpoint_visible());
    }
}
