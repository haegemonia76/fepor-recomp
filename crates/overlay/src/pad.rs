//! Drawings of controllers, to see and change which input does what.
//!
//! Xbox, Nintendo and PlayStation families show a gamepad of that family (shapes after the real ones: the
//! Xbox's black shell with the left stick high and the D-pad low, the DualSense's white shell with a
//! touchpad and sticks side by side, the Switch Pro's dark grey one): click a physical input, then pick the
//! GameCube button it stands for. The GameCube family shows the GameCube controller with its own layout
//! (the big green A, the bean-shaped X and Y, the yellow C stick): click a GameCube button, then press the
//! input of your pad that should do it.
//!
//! Everything is drawn from shapes and from the generated icons of `glyphs`: no artwork is copied.

use std::collections::HashMap;

use egui::{
    Align2, Color32, FontId, Painter, Pos2, Rect, Sense, Shape as Paint, Stroke, TextureHandle,
    TextureOptions, Vec2,
};
use fepor_input::{Action, Controls, GlyphSet};

use crate::{glyphs, theme};

/// The drawing is laid out on a canvas of this size and scaled to fit.
const CANVAS: Vec2 = Vec2::new(720.0, 330.0);

#[derive(Clone, Copy)]
enum Dir {
    Up,
    Down,
    Left,
    Right,
}

#[derive(Clone, Copy)]
enum Shape {
    /// An icon from `glyphs`, `w` x `h` canvas units.
    Icon(f32, f32),
    /// One direction of a D-pad or a stick.
    Arrow(Dir),
    /// The cap of a stick, which can be pressed.
    Cap,
}

/// One input of a gamepad drawn on a pad.
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

/// The four arms of a D-pad centred at `(x, y)`.
fn dpad(x: f32, y: f32) -> [Spot; 4] {
    let r = 30.0;
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
    let r = 40.0;
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

/// A diamond of four face buttons around `(x, y)`.
fn face(x: f32, y: f32) -> [Spot; 4] {
    let d = 38.0;
    [
        icon("y", x, y - d, 34.0, 34.0),
        icon("x", x - d, y, 34.0, 34.0),
        icon("b", x + d, y, 34.0, 34.0),
        icon("a", x, y + d, 34.0, 34.0),
    ]
}

/// The inputs on the gamepad of `family` (positions on the canvas).
fn layout(family: GlyphSet) -> Vec<Spot> {
    let mut s = Vec::new();
    match family {
        GlyphSet::PlayStation => {
            s.extend(dpad(190.0, 135.0));
            s.extend(face(530.0, 135.0));
            s.extend(stick("left", 262.0, 215.0));
            s.extend(stick("right", 458.0, 215.0));
            s.push(icon("back", 305.0, 100.0, 46.0, 22.0));
            s.push(icon("start", 415.0, 100.0, 26.0, 26.0));
        }
        _ => {
            // Xbox and Switch Pro (and the GameCube family's names): left stick high, D-pad and right stick low
            s.extend(stick("left", 190.0, 135.0));
            s.extend(face(530.0, 135.0));
            s.extend(dpad(262.0, 215.0));
            s.extend(stick("right", 455.0, 215.0));
            s.push(icon("back", 318.0, 118.0, 46.0, 22.0));
            s.push(icon("start", 402.0, 118.0, 26.0, 26.0));
        }
    }
    s.push(icon("lefttrigger", 150.0, 22.0, 70.0, 28.0));
    s.push(icon("righttrigger", 570.0, 22.0, 70.0, 28.0));
    s.push(icon("leftshoulder", 150.0, 58.0, 70.0, 28.0));
    s.push(icon("rightshoulder", 570.0, 58.0, 70.0, 28.0));
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

/// Fill the union of convex `pieces` and outline it: the outlines are drawn first, wider, then the fills.
fn silhouette(painter: &Painter, pieces: &[Vec<Pos2>], fill: Color32, edge: Color32, width: f32) {
    for p in pieces {
        painter.add(Paint::convex_polygon(
            p.clone(),
            edge,
            Stroke::new(width * 2.0, edge),
        ));
    }
    for p in pieces {
        painter.add(Paint::convex_polygon(p.clone(), fill, Stroke::NONE));
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

/// A cross of two bars (a D-pad) centred at `c`.
fn cross(painter: &Painter, c: Pos2, arm: f32, thick: f32, fill: Color32, edge: Color32, s: f32) {
    let bars = [
        rrect(c, Vec2::new(arm * 2.0, thick), thick * 0.3, 0.0),
        rrect(c, Vec2::new(thick, arm * 2.0), thick * 0.3, 0.0),
    ];
    silhouette(painter, &bars, fill, edge, 1.2 * s);
}

/// A stick: a rim, a ring and a cap.
fn stick_shape(
    painter: &Painter,
    c: Pos2,
    r: f32,
    rim: Color32,
    cap: Color32,
    edge: Color32,
    s: f32,
) {
    painter.circle(c, r, rim, Stroke::new(1.5 * s, edge));
    painter.circle_stroke(c, r * 0.72, Stroke::new(1.0 * s, edge.gamma_multiply(0.8)));
    painter.circle(c, r * 0.5, cap, Stroke::new(1.0 * s, edge));
}

// ------------------------------------------------------------------------------------------ the bodies

struct Palette {
    shell: Color32,
    edge: Color32,
    dark: Color32,
    pad: Color32,
}

fn palette(family: GlyphSet) -> Palette {
    match family {
        // the DualSense: white shell, black centre
        GlyphSet::PlayStation => Palette {
            shell: Color32::from_rgb(0xe9, 0xeb, 0xef),
            edge: Color32::from_rgb(0x9a, 0xa0, 0xaa),
            dark: Color32::from_rgb(0x1c, 0x1d, 0x22),
            pad: Color32::from_rgb(0xd5, 0xd8, 0xdf),
        },
        // the Switch Pro controller: dark grey
        GlyphSet::Nintendo => Palette {
            shell: Color32::from_rgb(0x3d, 0x3e, 0x44),
            edge: Color32::from_rgb(0x1a, 0x1b, 0x1f),
            dark: Color32::from_rgb(0x14, 0x15, 0x18),
            pad: Color32::from_rgb(0x2a, 0x2b, 0x30),
        },
        // the Xbox controller: black
        _ => Palette {
            shell: Color32::from_rgb(0x2a, 0x2c, 0x30),
            edge: Color32::from_rgb(0x0e, 0x0f, 0x11),
            dark: Color32::from_rgb(0x12, 0x13, 0x15),
            pad: Color32::from_rgb(0x1d, 0x1e, 0x22),
        },
    }
}

/// The shell and the fixed parts of the pad of `family` (stick rims, D-pad plate, touchpad).
fn draw_pad_body(painter: &Painter, at: &dyn Fn(f32, f32) -> Pos2, s: f32, family: GlyphSet) {
    let p = palette(family);
    let v = |x: f32, y: f32| Vec2::new(x * s, y * s);
    let slim = family == GlyphSet::Nintendo;
    // triggers and bumpers sit above the shell
    let mut pieces = Vec::new();
    for (sx, ang) in [(150.0, 0.12f32), (570.0, -0.12)] {
        pieces.push(rrect(at(sx, 22.0), v(78.0, 30.0), 12.0 * s, 0.0));
        pieces.push(rrect(at(sx, 58.0), v(96.0, 24.0), 10.0 * s, ang));
    }
    silhouette(painter, &pieces, p.pad, p.edge, 1.5 * s);
    // the shell: a rounded body and two grips that lean outwards
    let grip = if slim {
        v(98.0, 168.0)
    } else {
        v(112.0, 178.0)
    };
    let shell = vec![
        rrect(at(360.0, 150.0), v(470.0, 160.0), 64.0 * s, 0.0),
        rrect(at(175.0, 232.0), grip, 46.0 * s, 0.3),
        rrect(at(545.0, 232.0), grip, 46.0 * s, -0.3),
    ];
    silhouette(painter, &shell, p.shell, p.edge, 2.0 * s);
    match family {
        GlyphSet::PlayStation => {
            // the black centre and the touchpad
            painter.add(Paint::convex_polygon(
                rrect(at(360.0, 225.0), v(190.0, 150.0), 50.0 * s, 0.0),
                p.dark,
                Stroke::NONE,
            ));
            painter.add(Paint::convex_polygon(
                rrect(at(360.0, 92.0), v(170.0, 56.0), 22.0 * s, 0.0),
                p.pad,
                Stroke::new(1.0 * s, p.edge),
            ));
            painter.circle(
                at(360.0, 160.0),
                11.0 * s,
                p.shell,
                Stroke::new(1.0 * s, p.edge),
            );
            for c in [(190.0, 135.0)] {
                cross(painter, at(c.0, c.1), 30.0 * s, 22.0 * s, p.pad, p.edge, s);
            }
            for c in [(262.0, 215.0), (458.0, 215.0)] {
                stick_shape(
                    painter,
                    at(c.0, c.1),
                    30.0 * s,
                    p.dark,
                    p.pad,
                    Color32::from_gray(0x55),
                    s,
                );
            }
        }
        _ => {
            // the X-like centre button, the stick rims and the D-pad
            painter.circle(
                at(360.0, 92.0),
                11.0 * s,
                p.pad,
                Stroke::new(1.0 * s, p.edge),
            );
            stick_shape(
                painter,
                at(190.0, 135.0),
                32.0 * s,
                p.dark,
                p.pad,
                Color32::from_gray(0x55),
                s,
            );
            stick_shape(
                painter,
                at(455.0, 215.0),
                32.0 * s,
                p.dark,
                p.pad,
                Color32::from_gray(0x55),
                s,
            );
            cross(
                painter,
                at(262.0, 215.0),
                30.0 * s,
                22.0 * s,
                p.dark,
                Color32::from_gray(0x55),
                s,
            );
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
    let scale = (ui.available_width() / CANVAS.x).min(1.4);
    let (resp, painter) = ui.allocate_painter(CANVAS * scale, Sense::hover());
    let origin = resp.rect.min;
    let at = move |x: f32, y: f32| Pos2::new(origin.x + x * scale, origin.y + y * scale);
    if family == GlyphSet::GameCube {
        show_game_cube(ui, &painter, &at, scale, controls);
        return;
    }
    draw_pad_body(&painter, &at, scale, family);

    let cfg = controls.config().clone();
    let ctx = ui.ctx().clone();
    let mut clicked = None;
    for spot in layout(family) {
        let c = at(spot.at.0, spot.at.1);
        let (w, h) = match spot.shape {
            Shape::Icon(w, h) => (w * scale, h * scale),
            Shape::Arrow(_) => (20.0 * scale, 20.0 * scale),
            Shape::Cap => (30.0 * scale, 30.0 * scale),
        };
        let rect = Rect::from_center_size(c, Vec2::new(w, h));
        let id = egui::Id::unique(("pad spot", spot.source));
        let r = ui.interact(rect, id, Sense::click());
        let hot = r.hovered() || r.has_focus() || *selected == Some(spot.source);
        match spot.shape {
            Shape::Icon(iw, ih) => {
                if let Some(t) = view.texture(&ctx, family, spot.source, iw as u32, ih as u32) {
                    painter.image(
                        t,
                        rect,
                        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                        Color32::WHITE,
                    );
                }
            }
            // the cap is part of the drawn stick: only a ring shows when it is chosen
            Shape::Cap => {}
            Shape::Arrow(dir) => {
                let fill = if hot {
                    theme::GOLD
                } else {
                    Color32::from_white_alpha(150)
                };
                painter.add(Paint::convex_polygon(
                    arrow_points(c, dir, 7.5 * scale),
                    fill,
                    Stroke::new(0.8 * scale, Color32::from_black_alpha(120)),
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
        // the GameCube button bound to it, outside the arrow or beside the icon
        if let Some(action) = cfg.bindings.gamepad.get(spot.source) {
            tag(&painter, &spot, rect, c, action.short(), scale);
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
fn tag(painter: &Painter, spot: &Spot, rect: Rect, c: Pos2, text: &str, scale: f32) {
    let (anchor, pos) = match spot.shape {
        Shape::Arrow(Dir::Up) => (Align2::CENTER_BOTTOM, Pos2::new(c.x, rect.top() - 1.0)),
        Shape::Arrow(Dir::Left) => (Align2::RIGHT_CENTER, Pos2::new(rect.left() - 2.0, c.y)),
        Shape::Arrow(Dir::Right) => (Align2::LEFT_CENTER, Pos2::new(rect.right() + 2.0, c.y)),
        // shoulders and triggers are stacked: their tags go outside, beside them
        Shape::Icon(..) if spot.source.starts_with("left") && spot.source != "leftstick" => {
            (Align2::RIGHT_CENTER, Pos2::new(rect.left() - 4.0, c.y))
        }
        Shape::Icon(..) if spot.source.starts_with("right") && spot.source != "rightstick" => {
            (Align2::LEFT_CENTER, Pos2::new(rect.right() + 4.0, c.y))
        }
        _ => (Align2::CENTER_TOP, Pos2::new(c.x, rect.bottom() + 1.0)),
    };
    // a dark backing keeps the tag readable over the shell, whatever its colour
    let galley = painter.layout_no_wrap(
        text.to_string(),
        FontId::proportional(13.0 * scale.max(0.8)),
        theme::GOLD,
    );
    let area = anchor.anchor_size(pos, galley.size()).expand(2.0);
    painter.rect_filled(area, 3.0, Color32::from_black_alpha(185));
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
    let green = Color32::from_rgb(0x3c, 0xa5, 0x58);
    let red = Color32::from_rgb(0xd2, 0x3f, 0x3a);
    let grey = Color32::from_rgb(0xb9, 0xbc, 0xc2);
    let blue = Color32::from_rgb(0x3b, 0x6e, 0xd8);
    let violet = Color32::from_rgb(0x6c, 0x5f, 0xb0);
    let s = |a, x, y, kind| GcSpot {
        action: a,
        at: (x, y),
        kind,
    };
    let mut v = vec![
        s(
            Action::L,
            150.0,
            22.0,
            GcKind::Pill(70.0, 28.0, violet, "L"),
        ),
        s(
            Action::R,
            570.0,
            22.0,
            GcKind::Pill(70.0, 28.0, violet, "R"),
        ),
        s(Action::Z, 598.0, 64.0, GcKind::Pill(52.0, 22.0, blue, "Z")),
        s(Action::A, 538.0, 150.0, GcKind::Disc(34.0, green, "A")),
        s(Action::B, 490.0, 205.0, GcKind::Disc(20.0, red, "B")),
        s(Action::X, 608.0, 140.0, GcKind::Bean(24.0, 44.0, grey, "X")),
        s(Action::Y, 536.0, 92.0, GcKind::Bean(44.0, 24.0, grey, "Y")),
        s(Action::Start, 335.0, 135.0, GcKind::Disc(13.0, grey, "")),
    ];
    // main stick (left), C stick (lower right) and the D-pad (lower left)
    let dirs = |a: [Action; 4], x: f32, y: f32, r: f32| {
        [
            s(a[0], x, y - r, GcKind::Arrow(Dir::Up)),
            s(a[1], x, y + r, GcKind::Arrow(Dir::Down)),
            s(a[2], x - r, y, GcKind::Arrow(Dir::Left)),
            s(a[3], x + r, y, GcKind::Arrow(Dir::Right)),
        ]
    };
    v.extend(dirs(
        [
            Action::StickUp,
            Action::StickDown,
            Action::StickLeft,
            Action::StickRight,
        ],
        190.0,
        120.0,
        42.0,
    ));
    v.extend(dirs(
        [
            Action::DpadUp,
            Action::DpadDown,
            Action::DpadLeft,
            Action::DpadRight,
        ],
        235.0,
        235.0,
        32.0,
    ));
    v.extend(dirs(
        [Action::CUp, Action::CDown, Action::CLeft, Action::CRight],
        420.0,
        250.0,
        38.0,
    ));
    v
}

fn show_game_cube(
    ui: &mut egui::Ui,
    painter: &Painter,
    at: &dyn Fn(f32, f32) -> Pos2,
    s: f32,
    controls: &mut Controls,
) {
    let v = |x: f32, y: f32| Vec2::new(x * s, y * s);
    let shell = Color32::from_rgb(0x57, 0x47, 0x9b);
    let edge = Color32::from_rgb(0x2b, 0x22, 0x55);
    let trig = Color32::from_rgb(0x41, 0x35, 0x78);
    // triggers, then the body with two handles
    let mut pieces = vec![
        rrect(at(150.0, 22.0), v(84.0, 30.0), 12.0 * s, 0.0),
        rrect(at(570.0, 22.0), v(84.0, 30.0), 12.0 * s, 0.0),
    ];
    silhouette(painter, &pieces, trig, edge, 1.5 * s);
    pieces = vec![
        rrect(at(360.0, 162.0), v(500.0, 196.0), 76.0 * s, 0.0),
        rrect(at(165.0, 245.0), v(104.0, 150.0), 44.0 * s, 0.25),
        rrect(at(555.0, 245.0), v(104.0, 150.0), 44.0 * s, -0.25),
    ];
    silhouette(painter, &pieces, shell, edge, 2.0 * s);
    // the control stick's well, the D-pad plate and the C stick's yellow cap
    painter.circle(
        at(190.0, 120.0),
        30.0 * s,
        Color32::from_rgb(0x9a, 0x9d, 0xa6),
        Stroke::new(1.5 * s, edge),
    );
    painter.circle(
        at(190.0, 120.0),
        14.0 * s,
        Color32::from_rgb(0xc9, 0xcc, 0xd2),
        Stroke::new(1.0 * s, edge),
    );
    cross(
        painter,
        at(235.0, 235.0),
        28.0 * s,
        20.0 * s,
        Color32::from_rgb(0xb0, 0xb3, 0xbb),
        edge,
        s,
    );
    painter.circle(
        at(420.0, 250.0),
        22.0 * s,
        Color32::from_rgb(0xe6, 0xc2, 0x2f),
        Stroke::new(1.5 * s, edge),
    );

    let cfg = controls.config().clone();
    let waiting = controls
        .remap_target()
        .and_then(|(keyboard, a)| (!keyboard).then_some(a));
    for spot in gc_spots() {
        let c = at(spot.at.0, spot.at.1);
        let size = match spot.kind {
            GcKind::Disc(r, ..) => Vec2::splat(r * 2.0 * s),
            GcKind::Bean(w, h, ..) => v(w, h),
            GcKind::Pill(w, h, ..) => v(w, h),
            GcKind::Arrow(_) => Vec2::splat(20.0 * s),
        };
        let rect = Rect::from_center_size(c, size);
        let r = ui.interact(
            rect,
            egui::Id::unique(("gc spot", spot.action.label())),
            Sense::click(),
        );
        let hot = r.hovered() || r.has_focus() || waiting == Some(spot.action);
        let dark = Color32::from_rgb(0x1a, 0x1a, 0x1e);
        match spot.kind {
            GcKind::Disc(rad, fill, label) => {
                painter.circle(c, rad * s, fill, Stroke::new(1.5 * s, edge));
                painter.text(
                    c,
                    Align2::CENTER_CENTER,
                    label,
                    FontId::proportional(rad * 1.1 * s),
                    dark,
                );
            }
            GcKind::Bean(w, h, fill, label) => {
                painter.add(Paint::convex_polygon(
                    rrect(c, v(w, h), w.min(h) * 0.5 * s, 0.0),
                    fill,
                    Stroke::new(1.5 * s, edge),
                ));
                painter.text(
                    c,
                    Align2::CENTER_CENTER,
                    label,
                    FontId::proportional(13.0 * s),
                    dark,
                );
            }
            GcKind::Pill(w, h, fill, label) => {
                painter.add(Paint::convex_polygon(
                    rrect(c, v(w, h), h * 0.5 * s, 0.0),
                    fill,
                    Stroke::new(1.5 * s, edge),
                ));
                painter.text(
                    c,
                    Align2::CENTER_CENTER,
                    label,
                    FontId::proportional(14.0 * s),
                    Color32::WHITE,
                );
            }
            GcKind::Arrow(dir) => {
                let fill = if hot {
                    theme::GOLD
                } else {
                    Color32::from_white_alpha(170)
                };
                painter.add(Paint::convex_polygon(
                    arrow_points(c, dir, 7.5 * s),
                    fill,
                    Stroke::new(0.8 * s, Color32::from_black_alpha(120)),
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
        // the input of the pad that does it
        let bound = cfg.sources_of(false, spot.action).into_iter().next();
        let label = bound.map(|b| pad_name(cfg.glyphs, b));
        let as_spot = Spot {
            source: "",
            at: spot.at,
            shape: match spot.kind {
                GcKind::Arrow(d) => Shape::Arrow(d),
                _ => Shape::Cap,
            },
        };
        let text = label.unwrap_or_else(|| "-".to_string());
        tag(painter, &as_spot, rect, c, &text, s);
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

    #[test]
    fn the_game_cube_controller_has_every_game_cube_button_once() {
        let spots = gc_spots();
        for a in Action::ALL {
            assert_eq!(spots.iter().filter(|s| s.action == a).count(), 1, "{a:?}");
        }
    }
}
