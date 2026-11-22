//! The button icons the game draws as textures, found by content fingerprint (`FEPOR_DUMP_TEXTURES` writes
//! the fingerprint into the file name). When the player picked another button-name family in the overlay,
//! these textures are replaced by generated icons and the face buttons are moved from the GameCube's layout
//! into the diamond of other pads (ADR 0021).

use fepor_input::Action;
use fepor_overlay::glyphs::Shape;

pub struct KnownIcon {
    /// The renderer's content fingerprint of the texture (`Renderer::texture`).
    pub sample: u64,
    pub width: u32,
    pub height: u32,
    /// The GameCube button the icon stands for.
    pub action: Action,
    /// How the replacement sits in the texture.
    pub shape: Shape,
    /// Where the game draws the icon, relative to the centre of the button cluster, in pixels; `None` for
    /// icons that are not moved (the A texture is the cluster's base, shoulders keep their place).
    pub origin: Option<(f32, f32)>,
}

const fn icon(
    sample: u64,
    width: u32,
    height: u32,
    action: Action,
    shape: Shape,
    origin: Option<(f32, f32)>,
) -> KnownIcon {
    KnownIcon {
        sample,
        width,
        height,
        action,
        shape,
        origin,
    }
}

const FULL: Shape = Shape {
    scale: 1.0,
    halo: false,
};

/// The map screen's button hints (bottom right): the game draws the A button as a small dot on a grey halo,
/// the others at the size of their texture.
pub const KNOWN: &[KnownIcon] = &[
    icon(
        0xc42c_0bdf_f1dd_8d51,
        64,
        64,
        Action::A,
        Shape {
            scale: 0.48,
            halo: true,
        },
        None,
    ),
    // positions measured with FEPOR_TRACE=icons relative to the centre of the A texture (the cluster's
    // centre): the game puts B lower left, X to the right and Y above
    icon(
        0x0539_8cf4_ac14_cc98,
        24,
        24,
        Action::B,
        FULL,
        Some((-25.0, 15.0)),
    ),
    icon(
        0x974d_68d8_18cd_9bd0,
        24,
        32,
        Action::X,
        FULL,
        Some((31.0, -6.0)),
    ),
    icon(
        0x911e_5729_f6e6_815a,
        32,
        24,
        Action::Y,
        FULL,
        Some((-4.0, -26.0)),
    ),
    icon(0x1a73_805e_f30e_f91c, 48, 24, Action::Z, FULL, None),
];

/// What a label (the text next to a button of the hint) is placed relative to.
pub enum Anchor {
    /// Next to the icon of this GameCube button: the label sits beside it, on the outer side of the
    /// diamond, at this offset from the cluster centre in the GameCube layout.
    Icon(Action, (f32, f32)),
    /// Moves by this many pixels whenever the diamond is in use (the C-stick's dot and caption move to the
    /// right side, which has no label, to leave the left for the X button's caption).
    Fixed((f32, f32)),
}

/// A text or dot texture of the hint that belongs to a button.
pub struct KnownLabel {
    pub sample: u64,
    pub width: u32,
    pub height: u32,
    pub anchor: Anchor,
}

/// Measured with `FEPOR_TRACE=icons`, relative to the centre of the A texture. "Map Menu" (Z) and "Move" (A)
/// stay where they are.
pub const LABELS: &[KnownLabel] = &[
    // "Status" (Y)
    KnownLabel {
        sample: 0xc5fa_c3fb_5e0f_05a6,
        width: 48,
        height: 16,
        anchor: Anchor::Icon(Action::Y, (33.0, -33.0)),
    },
    // "Next Unit" (X)
    KnownLabel {
        sample: 0x16fc_5c01_5eee_0bad,
        width: 40,
        height: 32,
        anchor: Anchor::Icon(Action::X, (67.0, -6.0)),
    },
    // "Cond" caption
    KnownLabel {
        sample: 0x9da3_0589_66e9_502f,
        width: 40,
        height: 16,
        anchor: Anchor::Fixed((109.0, 17.0)),
    },
    // the grey dot under the "Cond" caption (the C-stick)
    KnownLabel {
        sample: 0xe100_f3c3_acc3_391c,
        width: 16,
        height: 16,
        anchor: Anchor::Fixed((107.0, 17.0)),
    },
];

/// What a texture of the hint is.
pub enum Known {
    Icon(&'static KnownIcon),
    Label(&'static KnownLabel),
}

pub fn find(sample: u64, width: u32, height: u32) -> Option<&'static KnownIcon> {
    KNOWN
        .iter()
        .find(|k| k.sample == sample && k.width == width && k.height == height)
}

pub fn find_any(sample: u64, width: u32, height: u32) -> Option<Known> {
    find(sample, width, height).map(Known::Icon).or_else(|| {
        LABELS
            .iter()
            .find(|l| l.sample == sample && l.width == width && l.height == height)
            .map(Known::Label)
    })
}

/// Whether a texture of this size can be one of the known textures (a cheap test before fingerprinting).
pub fn could_be(width: u32, height: u32) -> bool {
    KNOWN.iter().any(|k| k.width == width && k.height == height)
        || LABELS
            .iter()
            .any(|l| l.width == width && l.height == height)
}

/// Where the game draws the icon of `action`, relative to the cluster centre (0, 0 for the A texture).
pub fn origin_of(action: Action) -> (f32, f32) {
    KNOWN
        .iter()
        .find(|k| k.action == action)
        .and_then(|k| k.origin)
        .unwrap_or((0.0, 0.0))
}

/// A label's move in pixels, given the diamond slot of the button it describes.
pub fn label_shift(
    anchor: &Anchor,
    slot_of: impl Fn(Action) -> Option<(f32, f32)>,
) -> Option<(f32, f32)> {
    match anchor {
        Anchor::Fixed(delta) => slot_of(Action::A).map(|_| *delta),
        Anchor::Icon(action, label) => {
            let slot = slot_of(*action)?;
            let icon = origin_of(*action);
            let (mut dx, dy) = (label.0 - icon.0, label.1 - icon.1);
            // the caption goes on the outer side of the diamond
            if slot.0 < 0.0 {
                dx = -dx.abs();
            } else if slot.0 > 0.0 {
                dx = dx.abs();
            }
            Some((slot.0 + dx - label.0, slot.1 + dy - label.1))
        }
    }
}

/// A drawn rectangle in EFB pixels.
#[derive(Debug, Clone, Copy)]
pub struct Quad {
    pub cx: f32,
    pub cy: f32,
    pub w: f32,
    pub h: f32,
}

/// The pixel rectangle covered by vertices with these clip-space positions, for a viewport
/// `[x, y, w, h, near, far]` in EFB pixels.
pub fn quad_px(positions: impl Iterator<Item = [f32; 4]>, vp: [f32; 6]) -> Option<Quad> {
    let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
    for p in positions {
        if p[3] <= 0.0 {
            return None;
        }
        let x = vp[0] + (p[0] / p[3] * 0.5 + 0.5) * vp[2];
        let y = vp[1] + (0.5 - p[1] / p[3] * 0.5) * vp[3];
        lo = [lo[0].min(x), lo[1].min(y)];
        hi = [hi[0].max(x), hi[1].max(y)];
    }
    (lo[0] <= hi[0]).then_some(Quad {
        cx: (lo[0] + hi[0]) / 2.0,
        cy: (lo[1] + hi[1]) / 2.0,
        w: hi[0] - lo[0],
        h: hi[1] - lo[1],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_screen_quad_covers_the_viewport() {
        let vp = [0.0, 0.0, 640.0, 528.0, 0.0, 1.0];
        let q = quad_px(
            [[-1.0, -1.0, 0.0, 1.0], [1.0, 1.0, 0.0, 1.0]].into_iter(),
            vp,
        )
        .unwrap();
        assert_eq!((q.cx, q.cy, q.w, q.h), (320.0, 264.0, 640.0, 528.0));
    }

    fn xbox_slots(a: Action) -> Option<(f32, f32)> {
        match a {
            Action::A => Some((0.0, 20.0)),
            Action::B => Some((20.0, 0.0)),
            Action::X => Some((-20.0, 0.0)),
            Action::Y => Some((0.0, -20.0)),
            _ => None,
        }
    }

    #[test]
    fn the_x_caption_moves_to_the_left_of_its_button_and_y_keeps_its_side() {
        // "Next Unit" was right of X (at 67, -6): now left of X (at -20, 0), mirrored: -56, 0
        let (dx, dy) = label_shift(&Anchor::Icon(Action::X, (67.0, -6.0)), xbox_slots).unwrap();
        assert_eq!((67.0 + dx, -6.0 + dy), (-56.0, 0.0));
        // "Status" was right of Y: it stays right of Y's new place
        let (dx, dy) = label_shift(&Anchor::Icon(Action::Y, (33.0, -33.0)), xbox_slots).unwrap();
        assert_eq!((33.0 + dx, -33.0 + dy), (37.0, -27.0));
    }

    #[test]
    fn nothing_moves_in_the_game_cube_layout() {
        assert!(label_shift(&Anchor::Fixed((1.0, 1.0)), |_| None).is_none());
        assert!(label_shift(&Anchor::Icon(Action::X, (67.0, -6.0)), |_| None).is_none());
    }
}
