//! A drawing of the gamepad of the chosen family, to see and change which GameCube button each physical
//! button is bound to: click a button on the drawing, then pick the GameCube button for it.
//!
//! The drawing is made of shapes and of the same generated icons the game's hints use (`glyphs`), so it
//! follows the Xbox / Nintendo / PlayStation styles. Every spot is one gamepad input (SDL's name).

use std::collections::HashMap;

use egui::{
    Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, TextureHandle, TextureOptions, Vec2,
};
use fepor_input::{Controls, GlyphSet};

use crate::{glyphs, theme};

/// The drawing is laid out on a canvas of this size and scaled to fit.
const CANVAS: Vec2 = Vec2::new(640.0, 300.0);

#[derive(Clone, Copy)]
enum Shape {
    /// An icon from `glyphs`, `w` x `h` canvas units.
    Icon(f32, f32),
    /// One direction of a D-pad or a stick: an arrow.
    Arrow(Dir),
    /// The cap of a stick, which can be pressed.
    Cap,
}

#[derive(Clone, Copy)]
enum Dir {
    Up,
    Down,
    Left,
    Right,
}

struct Spot {
    source: &'static str,
    at: (f32, f32),
    shape: Shape,
}

fn icon(source: &'static str, x: f32, y: f32, w: f32, h: f32) -> Spot {
    Spot {
        source,
        at: (x, y),
        shape: Shape::Icon(w, h),
    }
}

fn arrow(source: &'static str, x: f32, y: f32, dir: Dir) -> Spot {
    Spot {
        source,
        at: (x, y),
        shape: Shape::Arrow(dir),
    }
}

/// A D-pad centred at `(x, y)`.
fn dpad(x: f32, y: f32) -> [Spot; 4] {
    let r = 28.0;
    [
        arrow("dpup", x, y - r, Dir::Up),
        arrow("dpdown", x, y + r, Dir::Down),
        arrow("dpleft", x - r, y, Dir::Left),
        arrow("dpright", x + r, y, Dir::Right),
    ]
}

/// A stick centred at `(x, y)`: four directions around its cap, which is also a button (L3 / R3).
fn stick(side: &'static str, x: f32, y: f32) -> [Spot; 5] {
    let (xm, xp, ym, yp, press) = if side == "left" {
        ("leftx-", "leftx+", "lefty-", "lefty+", "leftstick")
    } else {
        ("rightx-", "rightx+", "righty-", "righty+", "rightstick")
    };
    let r = 36.0;
    [
        arrow(ym, x, y - r, Dir::Up),
        arrow(yp, x, y + r, Dir::Down),
        arrow(xm, x - r, y, Dir::Left),
        arrow(xp, x + r, y, Dir::Right),
        Spot {
            source: press,
            at: (x, y),
            shape: Shape::Cap,
        },
    ]
}

/// The inputs on the pad of `family`. The GameCube family is drawn as an Xbox pad (its names are Xbox's).
fn layout(family: GlyphSet) -> Vec<Spot> {
    let mut s = vec![
        icon("lefttrigger", 110.0, 14.0, 66.0, 28.0),
        icon("righttrigger", 530.0, 14.0, 66.0, 28.0),
        icon("leftshoulder", 110.0, 50.0, 66.0, 28.0),
        icon("rightshoulder", 530.0, 50.0, 66.0, 28.0),
        icon("back", 292.0, 100.0, 52.0, 26.0),
        icon("start", 348.0, 100.0, 26.0, 26.0),
        // the face buttons, a diamond
        icon("y", 500.0, 100.0, 34.0, 34.0),
        icon("x", 458.0, 136.0, 34.0, 34.0),
        icon("b", 542.0, 136.0, 34.0, 34.0),
        icon("a", 500.0, 172.0, 34.0, 34.0),
    ];
    if family == GlyphSet::PlayStation {
        // D-pad top left, sticks side by side below the middle
        s.extend(dpad(140.0, 135.0));
        s.extend(stick("left", 245.0, 212.0));
        s.extend(stick("right", 395.0, 212.0));
    } else {
        // left stick top left, D-pad and right stick below the middle
        s.extend(stick("left", 140.0, 135.0));
        s.extend(dpad(245.0, 212.0));
        s.extend(stick("right", 395.0, 212.0));
    }
    s
}

/// Textures of the generated icons used in the drawing, made on first use.
#[derive(Default)]
pub struct PadView {
    textures: HashMap<(GlyphSet, &'static str), TextureHandle>,
}

impl PadView {
    fn texture(
        &mut self,
        ctx: &egui::Context,
        family: GlyphSet,
        source: &'static str,
        w: u32,
        h: u32,
    ) -> Option<egui::TextureId> {
        if let Some(t) = self.textures.get(&(family, source)) {
            return Some(t.id());
        }
        // drawn at 3x for sharpness
        let (tw, th) = (w * 3, h * 3);
        let rgba = glyphs::render(family, source, tw, th, glyphs::Shape::default())?;
        let image = egui::ColorImage::from_rgba_unmultiplied([tw as usize, th as usize], &rgba);
        let handle = ctx.load_texture(format!("pad {source}"), image, TextureOptions::LINEAR);
        let id = handle.id();
        self.textures.insert((family, source), handle);
        Some(id)
    }
}

fn arrow_points(c: Pos2, dir: Dir, r: f32) -> Vec<Pos2> {
    let (dx, dy) = match dir {
        Dir::Up => (0.0, -1.0),
        Dir::Down => (0.0, 1.0),
        Dir::Left => (-1.0, 0.0),
        Dir::Right => (1.0, 0.0),
    };
    // a triangle pointing along (dx, dy)
    let tip = Pos2::new(c.x + dx * r, c.y + dy * r);
    let (px, py) = (-dy, dx);
    let base = Pos2::new(c.x - dx * r * 0.6, c.y - dy * r * 0.6);
    vec![
        tip,
        Pos2::new(base.x + px * r * 0.8, base.y + py * r * 0.8),
        Pos2::new(base.x - px * r * 0.8, base.y - py * r * 0.8),
    ]
}

/// Draw the pad and handle clicks. `selected` is the input the player picked on the drawing.
pub fn show(
    ui: &mut egui::Ui,
    view: &mut PadView,
    controls: &Controls,
    selected: &mut Option<&'static str>,
) {
    let cfg = controls.config();
    let family = cfg.glyphs;
    // the GameCube family keeps Xbox's names for the physical buttons
    let drawn = if family == GlyphSet::GameCube {
        GlyphSet::Xbox
    } else {
        family
    };
    let scale = (ui.available_width() / CANVAS.x).min(1.4);
    let (resp, painter) = ui.allocate_painter(CANVAS * scale, Sense::hover());
    let origin = resp.rect.min;
    let at = |x: f32, y: f32| Pos2::new(origin.x + x * scale, origin.y + y * scale);

    // the shell: two grips under a rounded body
    let shell = Color32::from_rgb(0x07, 0x24, 0x1c);
    let edge = Stroke::new(1.5, theme::GOLD_DARK);
    for gx in [105.0, 535.0] {
        painter.circle(at(gx, 235.0), 56.0 * scale, shell, edge);
    }
    painter.rect(
        Rect::from_min_max(at(45.0, 62.0), at(595.0, 245.0)),
        egui::CornerRadius::same((70.0 * scale) as u8),
        shell,
        edge,
        egui::StrokeKind::Middle,
    );

    let ctx = ui.ctx().clone();
    let spots = layout(drawn);
    let mut clicked = None;
    for spot in &spots {
        let c = at(spot.at.0, spot.at.1);
        let (w, h) = match spot.shape {
            Shape::Icon(w, h) => (w * scale, h * scale),
            Shape::Arrow(_) => (20.0 * scale, 20.0 * scale),
            Shape::Cap => (28.0 * scale, 28.0 * scale),
        };
        let rect = Rect::from_center_size(c, Vec2::new(w, h));
        let id = egui::Id::unique(("pad spot", spot.source));
        let r = ui.interact(rect, id, Sense::click());
        let is_selected = *selected == Some(spot.source);
        let hot = r.hovered() || r.has_focus() || is_selected;
        match spot.shape {
            Shape::Icon(iw, ih) => {
                if let Some(t) = view.texture(&ctx, drawn, spot.source, iw as u32, ih as u32) {
                    painter.image(
                        t,
                        rect,
                        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                        Color32::WHITE,
                    );
                }
            }
            Shape::Cap => {
                painter.circle(
                    c,
                    13.0 * scale,
                    Color32::from_rgb(0x1b, 0x2a, 0x26),
                    Stroke::new(1.5, theme::GOLD_DARK),
                );
                painter.circle_stroke(
                    c,
                    7.5 * scale,
                    Stroke::new(1.0, theme::GOLD_DARK.gamma_multiply(0.8)),
                );
            }
            Shape::Arrow(dir) => {
                let fill = if hot {
                    theme::GOLD
                } else {
                    theme::CREAM.gamma_multiply(0.55)
                };
                painter.add(egui::Shape::convex_polygon(
                    arrow_points(c, dir, 8.0 * scale),
                    fill,
                    Stroke::NONE,
                ));
            }
        }
        if hot {
            let ring = rect.expand(2.0);
            painter.rect_stroke(
                ring,
                egui::CornerRadius::same((ring.height() / 2.0).min(255.0) as u8),
                Stroke::new(2.0, theme::GOLD),
                egui::StrokeKind::Middle,
            );
        }
        // the GameCube button bound to it, outside the arrow or below the icon
        if let Some(action) = cfg.bindings.gamepad.get(spot.source) {
            let (anchor, pos) = match spot.shape {
                Shape::Arrow(Dir::Up) => (Align2::CENTER_BOTTOM, Pos2::new(c.x, rect.top() - 1.0)),
                Shape::Arrow(Dir::Left) => {
                    (Align2::RIGHT_CENTER, Pos2::new(rect.left() - 2.0, c.y))
                }
                Shape::Arrow(Dir::Right) => {
                    (Align2::LEFT_CENTER, Pos2::new(rect.right() + 2.0, c.y))
                }
                // shoulders and triggers are stacked: their tags go outside, beside them
                Shape::Icon(..)
                    if spot.source.starts_with("left") && spot.source != "leftstick" =>
                {
                    (Align2::RIGHT_CENTER, Pos2::new(rect.left() - 4.0, c.y))
                }
                Shape::Icon(..)
                    if spot.source.starts_with("right") && spot.source != "rightstick" =>
                {
                    (Align2::LEFT_CENTER, Pos2::new(rect.right() + 4.0, c.y))
                }
                _ => (Align2::CENTER_TOP, Pos2::new(c.x, rect.bottom() + 1.0)),
            };
            painter.text(
                pos,
                anchor,
                action.short(),
                FontId::proportional(12.5 * scale.max(0.8)),
                theme::GOLD,
            );
        }
        let r = r.on_hover_text(family.source_label(spot.source));
        if r.clicked() {
            clicked = Some(spot.source);
        }
    }
    if let Some(source) = clicked {
        // a second click on the same input lets go of it
        *selected = if *selected == Some(source) {
            None
        } else {
            Some(source)
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_pad_has_the_face_buttons_both_sticks_and_a_dpad_once() {
        for family in [GlyphSet::Xbox, GlyphSet::Nintendo, GlyphSet::PlayStation] {
            let spots = layout(family);
            let mut sources: Vec<&str> = spots.iter().map(|s| s.source).collect();
            let n = sources.len();
            sources.sort_unstable();
            sources.dedup();
            assert_eq!(sources.len(), n, "{family:?} lists an input twice");
            for needed in [
                "a",
                "b",
                "x",
                "y",
                "dpup",
                "leftx-",
                "righty+",
                "leftshoulder",
                "start",
                "righttrigger",
            ] {
                assert!(sources.contains(&needed), "{family:?} lacks {needed}");
            }
        }
    }

    #[test]
    fn nothing_on_a_pad_overlaps_another_spot() {
        // spots are at least a button apart, so a click never reaches two of them
        for family in [GlyphSet::Xbox, GlyphSet::PlayStation] {
            let spots = layout(family);
            for (i, a) in spots.iter().enumerate() {
                for b in &spots[i + 1..] {
                    let d = (a.at.0 - b.at.0).hypot(a.at.1 - b.at.1);
                    assert!(
                        d >= 26.0,
                        "{} and {} are {d} apart on {family:?}",
                        a.source,
                        b.source
                    );
                }
            }
        }
    }
}
