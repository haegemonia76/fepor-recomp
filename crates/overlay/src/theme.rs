//! The overlay's look, after the game's own menus: dark green bars with scrollwork, thin gold frames with
//! squared corners, cream text. Everything is drawn here from colours and lines: nothing from the game is
//! copied. The font is Marcellus (SIL Open Font License, `assets/OFL.txt`).

use std::sync::Arc;

use egui::{
    Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Pos2, Rect, RichText,
    Stroke, TextStyle, Vec2,
};

pub const GOLD: Color32 = Color32::from_rgb(0xe3, 0xbb, 0x50);
pub const GOLD_DARK: Color32 = Color32::from_rgb(0x94, 0x70, 0x26);
pub const GREEN_TOP: Color32 = Color32::from_rgb(0x12, 0x4a, 0x39);
pub const GREEN_BOTTOM: Color32 = Color32::from_rgb(0x05, 0x1f, 0x18);
/// Bars (buttons, slider tracks): the game's menu bars are this green.
pub const BAR: Color32 = Color32::from_rgb(0x0b, 0x33, 0x28);
pub const BAR_HOVER: Color32 = Color32::from_rgb(0x13, 0x4d, 0x3b);
pub const BAR_ACTIVE: Color32 = Color32::from_rgb(0x1c, 0x66, 0x4d);
pub const CREAM: Color32 = Color32::from_rgb(0xf3, 0xeb, 0xcf);
pub const DIM: Color32 = Color32::from_rgb(0xa9, 0xa5, 0x8d);

/// Set the font, sizes and widget colours on `ctx`.
pub fn install(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "Marcellus".into(),
        Arc::new(FontData::from_static(include_bytes!(
            "../assets/Marcellus-Regular.ttf"
        ))),
    );
    if let Some(family) = fonts.families.get_mut(&FontFamily::Proportional) {
        // the game's lettering first; egui's own fonts still cover what Marcellus lacks
        family.insert(0, "Marcellus".into());
    }
    ctx.set_fonts(fonts);

    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (
                TextStyle::Heading,
                FontId::new(27.0, FontFamily::Proportional),
            ),
            (TextStyle::Body, FontId::new(16.0, FontFamily::Proportional)),
            (
                TextStyle::Button,
                FontId::new(16.0, FontFamily::Proportional),
            ),
            (
                TextStyle::Small,
                FontId::new(13.5, FontFamily::Proportional),
            ),
            (
                TextStyle::Monospace,
                FontId::new(14.0, FontFamily::Monospace),
            ),
        ]
        .into();
        style.spacing.item_spacing = Vec2::new(10.0, 7.0);
        style.spacing.button_padding = Vec2::new(14.0, 5.0);
        style.spacing.interact_size.y = 28.0;
        style.spacing.slider_width = 220.0;

        let v = &mut style.visuals;
        v.dark_mode = true;
        v.override_text_color = None;
        v.panel_fill = Color32::TRANSPARENT;
        v.window_fill = GREEN_BOTTOM;
        v.window_stroke = Stroke::new(1.0, GOLD_DARK);
        v.extreme_bg_color = Color32::from_rgb(0x04, 0x17, 0x12);
        v.faint_bg_color = Color32::from_rgb(0x0a, 0x2b, 0x22);
        v.hyperlink_color = GOLD;
        v.warn_fg_color = GOLD;
        v.selection.bg_fill = Color32::from_rgb(0x7a, 0x5d, 0x1b);
        v.selection.stroke = Stroke::new(1.5, GOLD);
        v.slider_trailing_fill = true;
        let round = CornerRadius::same(5);
        let w = &mut v.widgets;
        w.noninteractive.bg_fill = BAR;
        w.noninteractive.weak_bg_fill = BAR;
        w.noninteractive.bg_stroke = Stroke::new(1.0, GOLD_DARK);
        w.noninteractive.fg_stroke = Stroke::new(1.0, CREAM);
        w.noninteractive.corner_radius = round;
        w.inactive.bg_fill = BAR;
        w.inactive.weak_bg_fill = BAR;
        w.inactive.bg_stroke = Stroke::new(1.0, GOLD_DARK);
        w.inactive.fg_stroke = Stroke::new(1.0, CREAM);
        w.inactive.corner_radius = round;
        w.hovered.bg_fill = BAR_HOVER;
        w.hovered.weak_bg_fill = BAR_HOVER;
        w.hovered.bg_stroke = Stroke::new(1.5, GOLD);
        w.hovered.fg_stroke = Stroke::new(1.5, Color32::WHITE);
        w.hovered.corner_radius = round;
        w.active.bg_fill = BAR_ACTIVE;
        w.active.weak_bg_fill = BAR_ACTIVE;
        w.active.bg_stroke = Stroke::new(1.5, GOLD);
        w.active.fg_stroke = Stroke::new(1.5, GOLD);
        w.active.corner_radius = round;
        w.open = w.active.clone();
    });
}

/// A section caption in gold.
pub fn caption(text: impl Into<String>) -> RichText {
    RichText::new(text).color(GOLD)
}

/// Fill `rect` with the panel's green gradient and frame it in gold with the game's squared corners.
pub fn paint_panel(painter: &egui::Painter, rect: Rect) {
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(rect.left_top(), GREEN_TOP.gamma_multiply(0.96));
    mesh.colored_vertex(rect.right_top(), GREEN_TOP.gamma_multiply(0.96));
    mesh.colored_vertex(rect.right_bottom(), GREEN_BOTTOM.gamma_multiply(0.97));
    mesh.colored_vertex(rect.left_bottom(), GREEN_BOTTOM.gamma_multiply(0.97));
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(egui::Shape::mesh(mesh));
    frame(painter, rect.shrink(7.0));
}

/// A thin gold frame with the corner pieces of the game's windows: an inner line, a heavier corner bracket and
/// a small diamond.
pub fn frame(painter: &egui::Painter, rect: Rect) {
    painter.rect_stroke(
        rect,
        0.0,
        Stroke::new(1.5, GOLD_DARK),
        egui::StrokeKind::Middle,
    );
    painter.rect_stroke(
        rect.shrink(4.0),
        0.0,
        Stroke::new(1.0, GOLD_DARK.gamma_multiply(0.7)),
        egui::StrokeKind::Middle,
    );
    let arm = 22.0;
    let corners = [
        (rect.left_top(), 1.0, 1.0),
        (rect.right_top(), -1.0, 1.0),
        (rect.left_bottom(), 1.0, -1.0),
        (rect.right_bottom(), -1.0, -1.0),
    ];
    for (c, sx, sy) in corners {
        let p = |dx: f32, dy: f32| Pos2::new(c.x + sx * dx, c.y + sy * dy);
        let thick = Stroke::new(3.0, GOLD);
        painter.line_segment([p(-1.5, 0.0), p(arm, 0.0)], thick);
        painter.line_segment([p(0.0, -1.5), p(0.0, arm)], thick);
        // the diamond just inside the corner
        let d = 4.5;
        let m = p(9.0, 9.0);
        painter.add(egui::Shape::convex_polygon(
            vec![
                Pos2::new(m.x, m.y - d),
                Pos2::new(m.x + d, m.y),
                Pos2::new(m.x, m.y + d),
                Pos2::new(m.x - d, m.y),
            ],
            GOLD,
            Stroke::NONE,
        ));
    }
}

/// A tab: a green bar with a gold edge, lit when it is the current page.
pub fn tab(ui: &mut egui::Ui, selected: bool, text: &str) -> egui::Response {
    let (fill, stroke, color) = if selected {
        (BAR_ACTIVE, Stroke::new(1.5, GOLD), GOLD)
    } else {
        (BAR, Stroke::new(1.0, GOLD_DARK), CREAM)
    };
    ui.add(
        egui::Button::new(RichText::new(text).color(color))
            .fill(fill)
            .stroke(stroke)
            .min_size(Vec2::new(0.0, 28.0)),
    )
}
