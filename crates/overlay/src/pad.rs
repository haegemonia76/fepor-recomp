//! Drawings of controllers, to see and change which input does what.
//!
//! The Xbox, Nintendo and PlayStation families show a gamepad of that family: click a physical input, then pick
//! the GameCube button it stands for. The GameCube family shows the GameCube controller: click a GameCube
//! button, then press the input of your pad that should do it.
//!
//! The silhouettes come from photographs (`outlines.rs`, traced with `tools/trace-outline.py`) and the buttons
//! sit where they do on those photographs; everything else is drawn from shapes and from the generated icons
//! of `glyphs`. No photograph or artwork is kept.

use std::collections::HashMap;

use egui::{
    Align2, Color32, FontId, Painter, Pos2, Rect, Sense, Shape as Paint, Stroke, TextureHandle,
    TextureOptions, Vec2,
};
use fepor_input::{Action, Controls, GlyphSet};

use crate::{glyphs, outlines, theme};

/// The canvas is 1000 units wide, whatever the outline; `TOP` units are kept above it for the triggers.
const WIDTH: f32 = 1000.0;
const TOP: f32 = 130.0;
const BOTTOM: f32 = 30.0;

#[derive(Clone, Copy)]
enum Dir {
    Up,
    Down,
    Left,
    Right,
}

#[derive(Clone, Copy)]
enum Shape {
    /// An icon from `glyphs`, `w` x `h` units.
    Icon(f32, f32),
    /// One direction of a D-pad or a stick.
    Arrow(Dir),
    /// The cap of a stick, which can be pressed.
    Cap,
}

/// One input of a gamepad drawn on a pad (positions in canvas units, `v` measured from the outline's top).
struct Spot {
    source: &'static str,
    at: (f32, f32),
    shape: Shape,
}

fn icon(source: &'static str, u: f32, v: f32, w: f32, h: f32) -> Spot {
    Spot {
        source,
        at: (u, v),
        shape: Shape::Icon(w, h),
    }
}

fn arrow(source: &'static str, u: f32, v: f32, dir: Dir) -> Spot {
    Spot {
        source,
        at: (u, v),
        shape: Shape::Arrow(dir),
    }
}

/// The four arms of a D-pad centred at `(u, v)`.
fn dpad(u: f32, v: f32, r: f32) -> [Spot; 4] {
    [
        arrow("dpup", u, v - r, Dir::Up),
        arrow("dpdown", u, v + r, Dir::Down),
        arrow("dpleft", u - r, v, Dir::Left),
        arrow("dpright", u + r, v, Dir::Right),
    ]
}

/// A stick centred at `(u, v)`: four directions around its cap, which is also a button (L3 / R3).
fn stick(side: &'static str, u: f32, v: f32, r: f32) -> [Spot; 5] {
    let (xm, xp, ym, yp, press) = if side == "left" {
        ("leftx-", "leftx+", "lefty-", "lefty+", "leftstick")
    } else {
        ("rightx-", "rightx+", "righty-", "righty+", "rightstick")
    };
    [
        arrow(ym, u, v - r, Dir::Up),
        arrow(yp, u, v + r, Dir::Down),
        arrow(xm, u - r, v, Dir::Left),
        arrow(xp, u + r, v, Dir::Right),
        Spot {
            source: press,
            at: (u, v),
            shape: Shape::Cap,
        },
    ]
}

/// The silhouette of the pad of `family` and its height (the width being 1).
fn outline(family: GlyphSet) -> (&'static [(f32, f32)], f32) {
    match family {
        GlyphSet::PlayStation => (outlines::DUALSENSE, outlines::DUALSENSE_HEIGHT),
        GlyphSet::GameCube => (outlines::GAMECUBE, outlines::GAMECUBE_HEIGHT),
        // the Switch Pro controller is shaped like the Xbox one, close enough
        _ => (outlines::XBOX, outlines::XBOX_HEIGHT),
    }
}

/// The inputs on the gamepad of `family`, where they are on the photographs.
fn layout(family: GlyphSet) -> Vec<Spot> {
    let mut s = Vec::new();
    match family {
        GlyphSet::PlayStation => {
            s.extend(dpad(205.0, 140.0, 66.0));
            s.extend(stick("left", 360.0, 265.0, 76.0));
            s.extend(stick("right", 665.0, 265.0, 76.0));
            s.push(icon("y", 825.0, 80.0, 76.0, 76.0));
            s.push(icon("x", 745.0, 148.0, 76.0, 76.0));
            s.push(icon("b", 890.0, 148.0, 76.0, 76.0));
            s.push(icon("a", 822.0, 215.0, 76.0, 76.0));
            s.push(icon("back", 292.0, 32.0, 70.0, 36.0));
            s.push(icon("start", 738.0, 42.0, 50.0, 50.0));
            s.push(icon("leftshoulder", 150.0, -22.0, 130.0, 52.0));
            s.push(icon("rightshoulder", 860.0, -22.0, 130.0, 52.0));
            s.push(icon("lefttrigger", 150.0, -88.0, 130.0, 52.0));
            s.push(icon("righttrigger", 860.0, -88.0, 130.0, 52.0));
        }
        _ => {
            // Xbox and Switch Pro: left stick high, D-pad and right stick low, face buttons on the right
            s.extend(stick("left", 225.0, 185.0, 76.0));
            s.extend(dpad(370.0, 390.0, 70.0));
            s.extend(stick("right", 685.0, 370.0, 76.0));
            s.push(icon("y", 805.0, 128.0, 76.0, 76.0));
            s.push(icon("x", 738.0, 198.0, 76.0, 76.0));
            s.push(icon("b", 877.0, 198.0, 76.0, 76.0));
            s.push(icon("a", 815.0, 275.0, 76.0, 76.0));
            s.push(icon("back", 440.0, 195.0, 66.0, 34.0));
            s.push(icon("start", 605.0, 195.0, 50.0, 50.0));
            s.push(icon("leftshoulder", 230.0, -22.0, 130.0, 52.0));
            s.push(icon("rightshoulder", 800.0, -22.0, 130.0, 52.0));
            s.push(icon("lefttrigger", 230.0, -88.0, 130.0, 52.0));
            s.push(icon("righttrigger", 800.0, -88.0, 130.0, 52.0));
        }
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
        let rgba = glyphs::render(family, source, w, h, glyphs::Shape::default())?;
        let image = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
        let handle = ctx.load_texture(format!("pad {source}"), image, TextureOptions::LINEAR);
        let id = handle.id();
        self.textures.insert((family, source), handle);
        Some(id)
    }
}

// ---------------------------------------------------------------------------------------------- shapes

/// The outline of a rounded rectangle (convex), turned by `angle` (clockwise, radians) about its centre.
fn rrect(c: Pos2, size: Vec2, r: f32, angle: f32) -> Vec<Pos2> {
    let (hw, hh) = (size.x / 2.0, size.y / 2.0);
    let r = r.min(hw).min(hh);
    let (sin, cos) = angle.sin_cos();
    let corners = [
        (hw - r, -hh + r, -90.0f32),
        (hw - r, hh - r, 0.0),
        (-hw + r, hh - r, 90.0),
        (-hw + r, -hh + r, 180.0),
    ];
    let mut pts = Vec::new();
    for (cx, cy, start) in corners {
        for i in 0..=6 {
            let a = (start + 15.0 * i as f32).to_radians();
            let (px, py) = (cx + r * a.cos(), cy + r * a.sin());
            pts.push(Pos2::new(
                c.x + px * cos - py * sin,
                c.y + px * sin + py * cos,
            ));
        }
    }
    pts
}

/// Ear-clipping triangulation of a simple polygon (the pad outlines are concave): triangle indices.
fn triangulate(pts: &[Pos2]) -> Vec<u32> {
    let n = pts.len();
    let area: f32 = (0..n)
        .map(|i| {
            let (a, b) = (pts[i], pts[(i + 1) % n]);
            a.x * b.y - b.x * a.y
        })
        .sum();
    let mut idx: Vec<usize> = if area > 0.0 {
        (0..n).collect()
    } else {
        (0..n).rev().collect()
    };
    let cross = |a: Pos2, b: Pos2, c: Pos2| (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x);
    let mut out = Vec::new();
    let mut guard = 0;
    while idx.len() > 3 && guard < n * n {
        guard += 1;
        let m = idx.len();
        let mut clipped = false;
        for i in 0..m {
            let (ia, ib, ic) = (idx[(i + m - 1) % m], idx[i], idx[(i + 1) % m]);
            let (a, b, c) = (pts[ia], pts[ib], pts[ic]);
            if cross(a, b, c) <= 0.0 {
                continue;
            }
            // an ear has no other vertex inside it
            let inside = idx.iter().any(|&j| {
                if j == ia || j == ib || j == ic {
                    return false;
                }
                let p = pts[j];
                cross(a, b, p) >= 0.0 && cross(b, c, p) >= 0.0 && cross(c, a, p) >= 0.0
            });
            if !inside {
                out.extend([ia as u32, ib as u32, ic as u32]);
                idx.remove(i);
                clipped = true;
                break;
            }
        }
        if !clipped {
            break;
        }
    }
    if idx.len() == 3 {
        out.extend(idx.iter().map(|&i| i as u32));
    }
    out
}

/// Fill a simple (possibly concave) polygon and outline it.
fn fill_polygon(painter: &Painter, pts: &[Pos2], fill: Color32, edge: Stroke) {
    let mut mesh = egui::Mesh::default();
    for p in pts {
        mesh.colored_vertex(*p, fill);
    }
    mesh.indices = triangulate(pts);
    painter.add(Paint::mesh(mesh));
    painter.add(Paint::closed_line(pts.to_vec(), edge));
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

/// A cross of two bars (a D-pad) centred at `c`.
fn cross(painter: &Painter, c: Pos2, arm: f32, thick: f32, fill: Color32, edge: Color32) {
    for size in [Vec2::new(arm * 2.0, thick), Vec2::new(thick, arm * 2.0)] {
        painter.add(Paint::convex_polygon(
            rrect(c, size, thick * 0.3, 0.0),
            edge,
            Stroke::new(3.0, edge),
        ));
    }
    for size in [Vec2::new(arm * 2.0, thick), Vec2::new(thick, arm * 2.0)] {
        painter.add(Paint::convex_polygon(
            rrect(c, size, thick * 0.3, 0.0),
            fill,
            Stroke::NONE,
        ));
    }
}

/// A stick: a well, a rim and a cap.
fn stick_shape(painter: &Painter, c: Pos2, r: f32, well: Color32, cap: Color32, edge: Color32) {
    painter.circle(c, r * 1.25, well, Stroke::new(1.5, edge));
    painter.circle(c, r, cap, Stroke::new(1.5, edge));
    painter.circle_stroke(c, r * 0.62, Stroke::new(1.0, edge));
}

fn grey(v: u8) -> Color32 {
    Color32::from_gray(v)
}

fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

/// Where canvas units land on the screen.
struct Frame {
    origin: Pos2,
    scale: f32,
}

impl Frame {
    fn at(&self, u: f32, v: f32) -> Pos2 {
        Pos2::new(
            self.origin.x + u * self.scale,
            self.origin.y + (v + TOP) * self.scale,
        )
    }

    fn len(&self, units: f32) -> f32 {
        units * self.scale
    }

    fn size(&self, w: f32, h: f32) -> Vec2 {
        Vec2::new(w * self.scale, h * self.scale)
    }
}

/// The shell of the pad of `family`, from its photograph, and the fixed parts on it.
fn draw_pad_body(painter: &Painter, f: &Frame, family: GlyphSet) {
    let (pts, _) = outline(family);
    let screen: Vec<Pos2> = pts
        .iter()
        .map(|(x, y)| f.at(x * WIDTH, y * WIDTH))
        .collect();
    match family {
        GlyphSet::PlayStation => {
            let (shell, edge) = (rgb(0xe9ebef), rgb(0x8f96a1));
            fill_polygon(painter, &screen, shell, Stroke::new(2.0, edge));
            // the black centre, the touchpad and the D-pad
            painter.add(Paint::convex_polygon(
                rrect(f.at(510.0, 300.0), f.size(500.0, 260.0), f.len(95.0), 0.0),
                rgb(0x17181d),
                Stroke::NONE,
            ));
            painter.add(Paint::convex_polygon(
                rrect(f.at(510.0, 112.0), f.size(380.0, 170.0), f.len(46.0), 0.0),
                rgb(0xd2d5dc),
                Stroke::new(1.5, edge),
            ));
            for u in [360.0, 665.0] {
                stick_shape(
                    painter,
                    f.at(u, 265.0),
                    f.len(56.0),
                    rgb(0x0b0c0f),
                    rgb(0x2b2d33),
                    grey(0x55),
                );
            }
            cross(
                painter,
                f.at(205.0, 140.0),
                f.len(64.0),
                f.len(46.0),
                rgb(0xf6f7f9),
                edge,
            );
        }
        _ => {
            let switch = family == GlyphSet::Nintendo;
            let (shell, edge) = if switch {
                (rgb(0x3b3d43), rgb(0x16171a))
            } else {
                (rgb(0x23262b), rgb(0x0b0c0e))
            };
            let (well, cap, line) = (rgb(0x131518), rgb(0x2c2f35), grey(0x4a));
            fill_polygon(painter, &screen, shell, Stroke::new(2.0, edge));
            stick_shape(painter, f.at(225.0, 185.0), f.len(56.0), well, cap, line);
            stick_shape(painter, f.at(685.0, 370.0), f.len(56.0), well, cap, line);
            if switch {
                cross(
                    painter,
                    f.at(370.0, 390.0),
                    f.len(70.0),
                    f.len(48.0),
                    rgb(0x15161a),
                    line,
                );
                // home and capture
                painter.circle(
                    f.at(605.0, 262.0),
                    f.len(22.0),
                    well,
                    Stroke::new(1.5, line),
                );
                painter.rect_filled(
                    Rect::from_center_size(f.at(440.0, 262.0), f.size(40.0, 40.0)),
                    4.0,
                    well,
                );
            } else {
                painter.circle(
                    f.at(370.0, 390.0),
                    f.len(96.0),
                    well,
                    Stroke::new(1.5, line),
                );
                cross(
                    painter,
                    f.at(370.0, 390.0),
                    f.len(66.0),
                    f.len(46.0),
                    cap,
                    line,
                );
                painter.circle(f.at(520.0, 75.0), f.len(40.0), well, Stroke::new(1.5, line));
                painter.add(Paint::convex_polygon(
                    rrect(f.at(525.0, 262.0), f.size(66.0, 34.0), f.len(14.0), 0.0),
                    well,
                    Stroke::new(1.0, line),
                ));
            }
        }
    }
}

/// Draw and handle the controller of the chosen family. For a gamepad family `selected` is the input the
/// player picked on the drawing; for the GameCube family a click starts a capture of the next input.
pub fn show(
    ui: &mut egui::Ui,
    view: &mut PadView,
    controls: &mut Controls,
    selected: &mut Option<&'static str>,
) {
    let family = controls.config().glyphs;
    let (_, height) = outline(family);
    // as wide as the page allows, but never taller than a screenful
    let scale = (ui.available_width() / WIDTH).min(560.0 / (TOP + height * WIDTH + BOTTOM));
    let canvas = Vec2::new(WIDTH, TOP + height * WIDTH + BOTTOM) * scale;
    let (resp, painter) = ui.allocate_painter(canvas, Sense::hover());
    let frame = Frame {
        origin: resp.rect.min,
        scale,
    };
    if family == GlyphSet::GameCube {
        show_game_cube(ui, &painter, &frame, controls);
        return;
    }
    // triggers and bumpers are drawn first, under the shell
    for spot in layout(family) {
        if spot.source.ends_with("trigger") {
            let Shape::Icon(w, h) = spot.shape else {
                continue;
            };
            let c = frame.at(spot.at.0, spot.at.1);
            painter.add(Paint::convex_polygon(
                rrect(c, frame.size(w + 24.0, h + 18.0), frame.len(24.0), 0.0),
                theme::GREEN_BOTTOM,
                Stroke::new(1.0, theme::GOLD_DARK),
            ));
        }
    }
    draw_pad_body(&painter, &frame, family);

    let cfg = controls.config().clone();
    let ctx = ui.ctx().clone();
    let mut clicked = None;
    for spot in layout(family) {
        let c = frame.at(spot.at.0, spot.at.1);
        let size = match spot.shape {
            Shape::Icon(w, h) => frame.size(w, h),
            Shape::Arrow(_) => frame.size(44.0, 44.0),
            Shape::Cap => frame.size(80.0, 80.0),
        };
        let rect = Rect::from_center_size(c, size);
        let id = egui::Id::unique(("pad spot", spot.source));
        let r = ui.interact(rect, id, Sense::click());
        let hot = r.hovered() || r.has_focus() || *selected == Some(spot.source);
        match spot.shape {
            Shape::Icon(iw, ih) => {
                if let Some(t) = view.texture(
                    &ctx,
                    family,
                    spot.source,
                    (iw * 1.2) as u32,
                    (ih * 1.2) as u32,
                ) {
                    painter.image(
                        t,
                        rect,
                        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                        Color32::WHITE,
                    );
                }
            }
            // the cap belongs to the stick drawn on the shell
            Shape::Cap => {}
            Shape::Arrow(dir) => {
                let fill = if hot {
                    theme::GOLD
                } else {
                    Color32::from_white_alpha(170)
                };
                painter.add(Paint::convex_polygon(
                    arrow_points(c, dir, frame.len(15.0)),
                    fill,
                    Stroke::new(0.8, Color32::from_black_alpha(140)),
                ));
            }
        }
        if hot {
            let ring = rect.shrink(rect.size().min_elem() * 0.08);
            painter.rect_stroke(
                ring,
                egui::CornerRadius::same((ring.height() / 2.0).min(255.0) as u8),
                Stroke::new(2.0, theme::GOLD),
                egui::StrokeKind::Middle,
            );
        }
        // the GameCube button bound to it, outside the arrow or beside the icon
        if let Some(action) = cfg.bindings.gamepad.get(spot.source) {
            tag(&painter, spot.shape, spot.source, rect, c, action.short());
        }
        let r = r.on_hover_text(pad_name(family, spot.source));
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

/// A gold label for a spot, on the outer side of an arrow, beside a shoulder, below other icons.
fn tag(painter: &Painter, shape: Shape, source: &str, rect: Rect, c: Pos2, text: &str) {
    let (anchor, pos) = match shape {
        Shape::Arrow(Dir::Up) => (Align2::CENTER_BOTTOM, Pos2::new(c.x, c.y - 10.0)),
        Shape::Arrow(Dir::Down) => (Align2::CENTER_TOP, Pos2::new(c.x, c.y + 10.0)),
        Shape::Arrow(Dir::Left) => (Align2::RIGHT_CENTER, Pos2::new(c.x - 10.0, c.y)),
        Shape::Arrow(Dir::Right) => (Align2::LEFT_CENTER, Pos2::new(c.x + 10.0, c.y)),
        // shoulders and triggers are stacked: their tags go outside, beside them
        Shape::Icon(..) if source.starts_with("left") && source != "leftstick" => {
            (Align2::RIGHT_CENTER, Pos2::new(rect.left() - 4.0, c.y))
        }
        Shape::Icon(..) if source.starts_with("right") && source != "rightstick" => {
            (Align2::LEFT_CENTER, Pos2::new(rect.right() + 4.0, c.y))
        }
        _ => (Align2::CENTER_TOP, Pos2::new(c.x, rect.bottom() + 1.0)),
    };
    // a dark backing keeps the tag readable over the shell, whatever its colour
    let galley = painter.layout_no_wrap(text.to_string(), FontId::proportional(13.0), theme::GOLD);
    let area = anchor.anchor_size(pos, galley.size()).expand(2.0);
    painter.rect_filled(area, 3.0, Color32::from_black_alpha(190));
    painter.galley(area.min + Vec2::splat(2.0), galley, theme::GOLD);
}

// ------------------------------------------------------------------------------------------- GameCube

#[derive(Clone, Copy)]
enum GcKind {
    Disc(f32, Color32, &'static str),
    Bean(f32, f32, Color32, &'static str),
    Pill(f32, f32, Color32, &'static str),
    Arrow(Dir),
}

struct GcSpot {
    action: Action,
    at: (f32, f32),
    kind: GcKind,
}

fn gc_spots() -> Vec<GcSpot> {
    let s = |a, u, v, kind| GcSpot {
        action: a,
        at: (u, v),
        kind,
    };
    let (teal, red, pale) = (rgb(0x3fbfa4), rgb(0xd5222c), rgb(0xdcdde3));
    let mut v = vec![
        // the triggers sit above the shell, Z is the blue piece on its upper right
        s(
            Action::L,
            150.0,
            -70.0,
            GcKind::Pill(150.0, 52.0, rgb(0x6f6aa8), "L"),
        ),
        s(
            Action::R,
            850.0,
            -70.0,
            GcKind::Pill(150.0, 52.0, rgb(0x6f6aa8), "R"),
        ),
        s(
            Action::Z,
            840.0,
            62.0,
            GcKind::Pill(100.0, 40.0, rgb(0x3a4fc4), "Z"),
        ),
        s(Action::A, 797.0, 232.0, GcKind::Disc(60.0, teal, "A")),
        s(Action::B, 685.0, 290.0, GcKind::Disc(38.0, red, "B")),
        s(
            Action::X,
            903.0,
            215.0,
            GcKind::Bean(56.0, 108.0, pale, "X"),
        ),
        s(
            Action::Y,
            765.0,
            128.0,
            GcKind::Bean(108.0, 58.0, pale, "Y"),
        ),
        s(
            Action::Start,
            497.0,
            248.0,
            GcKind::Disc(26.0, rgb(0xf1f1f4), ""),
        ),
    ];
    let dirs = |a: [Action; 4], u: f32, w: f32, r: f32| {
        [
            s(a[0], u, w - r, GcKind::Arrow(Dir::Up)),
            s(a[1], u, w + r, GcKind::Arrow(Dir::Down)),
            s(a[2], u - r, w, GcKind::Arrow(Dir::Left)),
            s(a[3], u + r, w, GcKind::Arrow(Dir::Right)),
        ]
    };
    v.extend(dirs(
        [
            Action::StickUp,
            Action::StickDown,
            Action::StickLeft,
            Action::StickRight,
        ],
        175.0,
        222.0,
        110.0,
    ));
    v.extend(dirs(
        [
            Action::DpadUp,
            Action::DpadDown,
            Action::DpadLeft,
            Action::DpadRight,
        ],
        330.0,
        450.0,
        78.0,
    ));
    v.extend(dirs(
        [Action::CUp, Action::CDown, Action::CLeft, Action::CRight],
        655.0,
        445.0,
        80.0,
    ));
    v
}

fn show_game_cube(ui: &mut egui::Ui, painter: &Painter, f: &Frame, controls: &mut Controls) {
    let indigo = rgb(0x4b3f98);
    let edge = rgb(0x261f58);
    // the triggers, under the shell
    for (u, trig) in [(150.0, rgb(0x57509a)), (850.0, rgb(0x57509a))] {
        painter.add(Paint::convex_polygon(
            rrect(f.at(u, -70.0), f.size(176.0, 74.0), f.len(30.0), 0.0),
            trig,
            Stroke::new(1.5, edge),
        ));
    }
    let (pts, _) = outline(GlyphSet::GameCube);
    let screen: Vec<Pos2> = pts
        .iter()
        .map(|(x, y)| f.at(x * WIDTH, y * WIDTH))
        .collect();
    fill_polygon(painter, &screen, indigo, Stroke::new(2.0, edge));
    // the control stick's well and cap, the D-pad plate, the C stick's well and its orange cap
    painter.circle(
        f.at(175.0, 222.0),
        f.len(112.0),
        rgb(0x3b3180),
        Stroke::new(1.5, edge),
    );
    painter.circle(
        f.at(175.0, 222.0),
        f.len(74.0),
        rgb(0xd2d4da),
        Stroke::new(1.5, rgb(0x777a85)),
    );
    painter.circle_stroke(
        f.at(175.0, 222.0),
        f.len(36.0),
        Stroke::new(1.0, rgb(0xa5a8b2)),
    );
    painter.circle(
        f.at(330.0, 450.0),
        f.len(100.0),
        rgb(0x3b3180),
        Stroke::new(1.5, edge),
    );
    cross(
        painter,
        f.at(330.0, 450.0),
        f.len(66.0),
        f.len(50.0),
        rgb(0xd2d4da),
        rgb(0x6c6f7a),
    );
    painter.circle(
        f.at(655.0, 445.0),
        f.len(98.0),
        rgb(0x3b3180),
        Stroke::new(1.5, edge),
    );
    painter.circle(
        f.at(655.0, 445.0),
        f.len(68.0),
        rgb(0xf0a830),
        Stroke::new(1.5, rgb(0xb06a10)),
    );
    painter.circle_stroke(
        f.at(655.0, 445.0),
        f.len(40.0),
        Stroke::new(1.0, rgb(0xd88a1c)),
    );

    let cfg = controls.config().clone();
    let waiting = controls
        .remap_target()
        .and_then(|(keyboard, a)| (!keyboard).then_some(a));
    for spot in gc_spots() {
        let c = f.at(spot.at.0, spot.at.1);
        let size = match spot.kind {
            GcKind::Disc(r, ..) => f.size(r * 2.0, r * 2.0),
            GcKind::Bean(w, h, ..) | GcKind::Pill(w, h, ..) => f.size(w, h),
            GcKind::Arrow(_) => f.size(44.0, 44.0),
        };
        let rect = Rect::from_center_size(c, size);
        let r = ui.interact(
            rect,
            egui::Id::unique(("gc spot", spot.action.label())),
            Sense::click(),
        );
        let hot = r.hovered() || r.has_focus() || waiting == Some(spot.action);
        let ink = Color32::from_rgb(0x1c, 0x1c, 0x22);
        match spot.kind {
            GcKind::Disc(rad, fill, label) => {
                painter.circle(c, f.len(rad), fill, Stroke::new(1.5, edge));
                painter.text(
                    c,
                    Align2::CENTER_CENTER,
                    label,
                    FontId::proportional(f.len(rad * 1.05)),
                    ink,
                );
            }
            GcKind::Bean(w, h, fill, label) => {
                painter.add(Paint::convex_polygon(
                    rrect(c, f.size(w, h), f.len(w.min(h) * 0.5), 0.0),
                    fill,
                    Stroke::new(1.5, edge),
                ));
                painter.text(
                    c,
                    Align2::CENTER_CENTER,
                    label,
                    FontId::proportional(14.0),
                    ink,
                );
            }
            GcKind::Pill(w, h, fill, label) => {
                painter.add(Paint::convex_polygon(
                    rrect(c, f.size(w, h), f.len(h * 0.5), 0.0),
                    fill,
                    Stroke::new(1.5, edge),
                ));
                painter.text(
                    c,
                    Align2::CENTER_CENTER,
                    label,
                    FontId::proportional(15.0),
                    Color32::WHITE,
                );
            }
            GcKind::Arrow(dir) => {
                let fill = if hot {
                    theme::GOLD
                } else {
                    Color32::from_white_alpha(190)
                };
                painter.add(Paint::convex_polygon(
                    arrow_points(c, dir, f.len(15.0)),
                    fill,
                    Stroke::new(0.8, Color32::from_black_alpha(140)),
                ));
            }
        }
        if hot {
            let ring = rect.shrink(rect.size().min_elem() * 0.08);
            painter.rect_stroke(
                ring,
                egui::CornerRadius::same((ring.height() / 2.0).min(255.0) as u8),
                Stroke::new(2.0, theme::GOLD),
                egui::StrokeKind::Middle,
            );
        }
        // the input of the pad that does it
        let bound = cfg.sources_of(false, spot.action).into_iter().next();
        let text = bound
            .map(|b| pad_name(cfg.glyphs, b))
            .unwrap_or_else(|| "-".to_string());
        let shape = match spot.kind {
            GcKind::Arrow(d) => Shape::Arrow(d),
            _ => Shape::Cap,
        };
        tag(painter, shape, "", rect, c, &text);
        let r = r.on_hover_text(spot.action.label());
        if r.clicked() {
            controls.rebind(false, spot.action);
        }
    }
}

/// What a gamepad input is called on the drawing: sticks and D-pad directions in words, the rest by the
/// family's names.
fn pad_name(family: GlyphSet, source: &str) -> String {
    let name = match source {
        "dpup" => "Pad Up",
        "dpdown" => "Pad Dn",
        "dpleft" => "Pad Lt",
        "dpright" => "Pad Rt",
        "lefty-" => "LS Up",
        "lefty+" => "LS Dn",
        "leftx-" => "LS Lt",
        "leftx+" => "LS Rt",
        "righty-" => "RS Up",
        "righty+" => "RS Dn",
        "rightx-" => "RS Lt",
        "rightx+" => "RS Rt",
        other => return family.source_label(other),
    };
    name.to_string()
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
    fn spots_are_far_enough_apart_for_a_click_to_reach_only_one() {
        for family in [GlyphSet::Xbox, GlyphSet::PlayStation] {
            let spots = layout(family);
            for (i, a) in spots.iter().enumerate() {
                for b in &spots[i + 1..] {
                    let d = (a.at.0 - b.at.0).hypot(a.at.1 - b.at.1);
                    assert!(
                        d >= 44.0,
                        "{} and {} are {d} apart on {family:?}",
                        a.source,
                        b.source
                    );
                }
            }
        }
    }

    #[test]
    fn the_game_cube_controller_has_every_game_cube_button_once() {
        let spots = gc_spots();
        for a in Action::ALL {
            assert_eq!(spots.iter().filter(|s| s.action == a).count(), 1, "{a:?}");
        }
    }

    #[test]
    fn a_concave_outline_is_fully_covered_by_its_triangles() {
        // an L shape: area 3
        let pts = [
            Pos2::new(0.0, 0.0),
            Pos2::new(2.0, 0.0),
            Pos2::new(2.0, 1.0),
            Pos2::new(1.0, 1.0),
            Pos2::new(1.0, 2.0),
            Pos2::new(0.0, 2.0),
        ];
        let tri = triangulate(&pts);
        assert_eq!(tri.len(), 12);
        let area: f32 = tri
            .chunks(3)
            .map(|t| {
                let (a, b, c) = (pts[t[0] as usize], pts[t[1] as usize], pts[t[2] as usize]);
                ((b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)).abs() / 2.0
            })
            .sum();
        assert!((area - 3.0).abs() < 1e-4, "{area}");
    }

    #[test]
    fn the_traced_outlines_triangulate() {
        for (pts, _) in [
            outline(GlyphSet::Xbox),
            outline(GlyphSet::PlayStation),
            outline(GlyphSet::GameCube),
        ] {
            let screen: Vec<Pos2> = pts
                .iter()
                .map(|(x, y)| Pos2::new(*x * 1000.0, *y * 1000.0))
                .collect();
            assert!(triangulate(&screen).len() >= 3 * (screen.len() - 2) * 9 / 10);
        }
    }
}
