//! Button icons drawn at run time to replace the ones the game draws (ADR 0021).
//!
//! The icon shows the physical gamepad button a GameCube button is bound to, in the style of the chosen
//! controller family. Everything is generated here (discs, pills, symbols from distance functions, letters
//! from the font bundled with egui), so no console maker's artwork is shipped.

use std::sync::LazyLock;

use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
use fepor_input::GlyphSet;

static FONT: LazyLock<Option<FontRef<'static>>> =
    LazyLock::new(|| FontRef::try_from_slice(epaint_default_fonts::HACK_REGULAR).ok());

type Rgb = [f32; 3];

fn rgb(hex: u32) -> Rgb {
    [
        (hex >> 16 & 255) as f32 / 255.0,
        (hex >> 8 & 255) as f32 / 255.0,
        (hex & 255) as f32 / 255.0,
    ]
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Symbol {
    Cross,
    Circle,
    Square,
    Triangle,
}

/// What goes on the face of the icon.
#[derive(Clone, PartialEq, Debug)]
enum Mark {
    Text(String),
    Symbol(Symbol),
}

/// An icon: a disc (face buttons) or a pill (shoulders, triggers, menu buttons) with a mark on it.
#[derive(Clone, PartialEq, Debug)]
struct Icon {
    pill: bool,
    fill: Rgb,
    mark: Mark,
    mark_color: Rgb,
}

fn face(fill: u32, mark: Mark, mark_color: u32) -> Icon {
    Icon {
        pill: false,
        fill: rgb(fill),
        mark,
        mark_color: rgb(mark_color),
    }
}

fn pill(text: &str) -> Icon {
    Icon {
        pill: true,
        fill: rgb(0x3a3d46),
        mark: Mark::Text(text.to_string()),
        mark_color: rgb(0xffffff),
    }
}

/// The icon for the gamepad input `source` (SDL's name) in `family`; `None` for the GameCube family, which
/// keeps the game's own art.
fn icon_for(family: GlyphSet, source: &str) -> Option<Icon> {
    let text = |s: &str| Mark::Text(s.to_string());
    Some(match family {
        GlyphSet::GameCube => return None,
        GlyphSet::Xbox => match source {
            "a" => face(0x3f9e4d, text("A"), 0xffffff),
            "b" => face(0xd24b45, text("B"), 0xffffff),
            "x" => face(0x3c78d0, text("X"), 0xffffff),
            "y" => face(0xe0b73a, text("Y"), 0x202020),
            _ => other(family, source),
        },
        GlyphSet::Nintendo => {
            // Nintendo names the buttons by their place on the pad, not by SDL's south/east/west/north
            let letter = match source {
                "a" => "B",
                "b" => "A",
                "x" => "Y",
                "y" => "X",
                _ => return Some(other(family, source)),
            };
            face(0x3a3d46, text(letter), 0xffffff)
        }
        GlyphSet::PlayStation => match source {
            "a" => face(0x2b2d36, Mark::Symbol(Symbol::Cross), 0x6fa0e8),
            "b" => face(0x2b2d36, Mark::Symbol(Symbol::Circle), 0xe8706f),
            "x" => face(0x2b2d36, Mark::Symbol(Symbol::Square), 0xe08fc8),
            "y" => face(0x2b2d36, Mark::Symbol(Symbol::Triangle), 0x4fc29a),
            _ => other(family, source),
        },
    })
}

/// Shoulders, triggers, menu buttons and anything else: a pill with the family's name for it.
fn other(family: GlyphSet, source: &str) -> Icon {
    let name = match (family, source) {
        (GlyphSet::PlayStation, "leftshoulder") => "L1".to_string(),
        (GlyphSet::PlayStation, "rightshoulder") => "R1".to_string(),
        (GlyphSet::PlayStation, "lefttrigger") => "L2".to_string(),
        (GlyphSet::PlayStation, "righttrigger") => "R2".to_string(),
        (GlyphSet::Nintendo, "leftshoulder") => "L".to_string(),
        (GlyphSet::Nintendo, "rightshoulder") => "R".to_string(),
        (GlyphSet::Nintendo, "lefttrigger") => "ZL".to_string(),
        (GlyphSet::Nintendo, "righttrigger") => "ZR".to_string(),
        (GlyphSet::Xbox, "leftshoulder") => "LB".to_string(),
        (GlyphSet::Xbox, "rightshoulder") => "RB".to_string(),
        (GlyphSet::Xbox, "lefttrigger") => "LT".to_string(),
        (GlyphSet::Xbox, "righttrigger") => "RT".to_string(),
        (_, "dpup") => "Up".to_string(),
        (_, "dpdown") => "Dn".to_string(),
        (_, "dpleft") => "Lt".to_string(),
        (_, "dpright") => "Rt".to_string(),
        _ => family.source_label(source),
    };
    pill(&name)
}

/// Coverage (0..1) of `text` centred in a `w` x `h` box, as large as fits (at most `max_px` high).
fn text_coverage(text: &str, w: u32, h: u32, max_px: f32, fit_w: f32) -> Vec<f32> {
    let mut out = vec![0.0; (w * h) as usize];
    let Some(font) = FONT.as_ref() else {
        return out;
    };
    let width_at = |px: f32| {
        let scaled = font.as_scaled(PxScale::from(px));
        text.chars()
            .map(|c| scaled.h_advance(font.glyph_id(c)))
            .sum::<f32>()
    };
    // largest size that fits the box (with a margin), by shrinking from the height limit
    let mut px = max_px.min(h as f32 * 0.8);
    while px > 4.0 && width_at(px) > fit_w * 0.82 {
        px -= 0.5;
    }
    let scaled = font.as_scaled(PxScale::from(px));
    let total = width_at(px);
    let mut x = (w as f32 - total) / 2.0;
    // the font's cap height sits on the baseline: centre it vertically on that
    let cap = scaled.ascent() * 0.72;
    let baseline = (h as f32 + cap) / 2.0;
    for c in text.chars() {
        let id = font.glyph_id(c);
        let glyph = id.with_scale_and_position(PxScale::from(px), ab_glyph::point(x, baseline));
        if let Some(o) = font.outline_glyph(glyph) {
            let b = o.px_bounds();
            o.draw(|gx, gy, cov| {
                let (px, py) = (b.min.x as i32 + gx as i32, b.min.y as i32 + gy as i32);
                if px >= 0 && py >= 0 && (px as u32) < w && (py as u32) < h {
                    let i = (py as u32 * w + px as u32) as usize;
                    out[i] = (out[i] + cov).min(1.0);
                }
            });
        }
        x += scaled.h_advance(id);
    }
    out
}

fn seg_dist(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (abx, aby) = (b.0 - a.0, b.1 - a.1);
    let t = (((p.0 - a.0) * abx + (p.1 - a.1) * aby) / (abx * abx + aby * aby)).clamp(0.0, 1.0);
    ((p.0 - a.0 - t * abx).powi(2) + (p.1 - a.1 - t * aby).powi(2)).sqrt()
}

/// Whether `p` (relative to the centre, in units of the disc's radius) is on the stroke of `symbol`.
fn on_symbol(symbol: Symbol, p: (f32, f32)) -> bool {
    let t = 0.11;
    match symbol {
        Symbol::Circle => (p.0.hypot(p.1) - 0.5).abs() < t,
        Symbol::Square => (p.0.abs().max(p.1.abs()) - 0.46).abs() < t,
        Symbol::Cross => {
            seg_dist(p, (-0.46, -0.46), (0.46, 0.46)) < t
                || seg_dist(p, (-0.46, 0.46), (0.46, -0.46)) < t
        }
        Symbol::Triangle => {
            let (a, b, c) = ((0.0, -0.56), (0.52, 0.38), (-0.52, 0.38));
            seg_dist(p, a, b) < t || seg_dist(p, b, c) < t || seg_dist(p, c, a) < t
        }
    }
}

/// Draw `icon` into a straight-alpha RGBA image of `w` x `h` pixels.
fn draw(icon: &Icon, w: u32, h: u32, shape: Shape) -> Vec<u8> {
    const SS: u32 = 4;
    let (wf, hf) = (w as f32, h as f32);
    // the disc fills the smaller side; the pill is twice as wide as high, or as wide as the image
    let (bw, bh) = if icon.pill {
        let bh = (wf / 2.0).min(hf);
        (wf, bh.max(hf.min(wf / 2.0)))
    } else {
        let d = wf.min(hf);
        (d, d)
    };
    let (bw, bh) = (bw * shape.scale, bh * shape.scale);
    let (cx, cy) = (wf / 2.0, hf / 2.0);
    let halo_radius = wf.min(hf) / 2.0;
    let radius = bh / 2.0;
    let half_len = (bw / 2.0 - radius).max(0.0);
    let border = (bh * 0.07).max(1.0);
    let text = match &icon.mark {
        Mark::Text(t) => Some(text_coverage(t, w, h, bh * 0.62, bw)),
        Mark::Symbol(_) => None,
    };
    let dark = |c: Rgb| c.map(|v| v * 0.55);
    let mut out = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let (mut a, mut col) = (0.0f32, [0.0f32; 3]);
            for sy in 0..SS {
                for sx in 0..SS {
                    let px = x as f32 + (sx as f32 + 0.5) / SS as f32 - cx;
                    let py = y as f32 + (sy as f32 + 0.5) / SS as f32 - cy;
                    // distance to the capsule's axis (a point for a disc)
                    let d = (px.abs() - half_len).max(0.0).hypot(py);
                    if d > radius {
                        // the halo is what the game's A button has around its dot
                        if shape.halo && px.hypot(py) <= halo_radius {
                            a += 0.55;
                            for k in 0..3 {
                                col[k] += 0.33 * 0.55;
                            }
                        }
                        continue;
                    }
                    a += 1.0;
                    let mut c = if d > radius - border {
                        dark(icon.fill)
                    } else {
                        icon.fill
                    };
                    if let Mark::Symbol(s) = &icon.mark {
                        if on_symbol(*s, (px / radius, py / radius)) {
                            c = icon.mark_color;
                        }
                    }
                    for k in 0..3 {
                        col[k] += c[k];
                    }
                }
            }
            if a == 0.0 {
                continue;
            }
            let mut c = col.map(|v| v / a);
            if let Some(t) = &text {
                let cov = t[(y * w + x) as usize];
                for k in 0..3 {
                    c[k] = c[k] * (1.0 - cov) + icon.mark_color[k] * cov;
                }
            }
            let i = ((y * w + x) * 4) as usize;
            for k in 0..3 {
                out[i + k] = (c[k] * 255.0 + 0.5) as u8;
            }
            out[i + 3] = (a / (SS * SS) as f32 * 255.0 + 0.5) as u8;
        }
    }
    out
}

/// How an icon sits in the image of the game's texture it replaces.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shape {
    /// Size of the icon relative to the largest it could be (the game's A button is a small dot on a halo).
    pub scale: f32,
    /// Draw a translucent grey halo over the whole image behind the icon, as the game's A button has.
    pub halo: bool,
}

impl Default for Shape {
    fn default() -> Shape {
        Shape {
            scale: 1.0,
            halo: false,
        }
    }
}

/// The replacement image for the game's icon of a button bound to the gamepad input `source`.
pub fn render(family: GlyphSet, source: &str, w: u32, h: u32, shape: Shape) -> Option<Vec<u8>> {
    icon_for(family, source).map(|i| draw(&i, w, h, shape))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_game_cube_family_keeps_the_games_art() {
        assert!(render(GlyphSet::GameCube, "a", 24, 24, Shape::default()).is_none());
    }

    #[test]
    fn families_draw_the_same_button_differently() {
        let a = |f| render(f, "a", 32, 32, Shape::default()).unwrap();
        assert_ne!(a(GlyphSet::Xbox), a(GlyphSet::PlayStation));
        assert_ne!(a(GlyphSet::Xbox), a(GlyphSet::Nintendo));
        // Nintendo's east button is its A
        assert_eq!(
            icon_for(GlyphSet::Nintendo, "b").unwrap().mark,
            Mark::Text("A".into())
        );
    }

    #[test]
    fn a_disc_is_opaque_inside_and_transparent_in_the_corners() {
        let img = render(GlyphSet::Xbox, "x", 24, 24, Shape::default()).unwrap();
        let alpha = |x: usize, y: usize| img[(y * 24 + x) * 4 + 3];
        assert_eq!(alpha(0, 0), 0);
        assert_eq!(alpha(12, 3), 255);
    }

    #[test]
    fn letters_are_drawn_on_the_disc() {
        let img = render(GlyphSet::Xbox, "a", 48, 48, Shape::default()).unwrap();
        let fill = icon_for(GlyphSet::Xbox, "a")
            .unwrap()
            .fill
            .map(|v| (v * 255.0 + 0.5) as u8);
        let lighter = img
            .chunks_exact(4)
            .filter(|p| p[3] == 255 && p[0] > fill[0] + 60)
            .count();
        assert!(
            lighter > 20,
            "the white letter is missing ({lighter} pixels)"
        );
    }

    #[test]
    fn a_wide_texture_gets_a_pill() {
        let img = render(GlyphSet::Xbox, "rightshoulder", 48, 24, Shape::default()).unwrap();
        let alpha = |x: usize, y: usize| img[(y * 48 + x) * 4 + 3];
        // the pill reaches the left and right edges at mid height
        assert!(alpha(1, 12) > 200 && alpha(46, 12) > 200);
    }
}
