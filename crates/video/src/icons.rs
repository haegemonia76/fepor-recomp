//! The button icons the game draws as textures, found by content fingerprint (`FEPOR_DUMP_TEXTURES` writes
//! the fingerprint into the file name). When the player picked another button-name family in the overlay,
//! these textures are replaced by generated icons (ADR 0021).

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
}

const fn icon(sample: u64, width: u32, height: u32, action: Action, shape: Shape) -> KnownIcon {
    KnownIcon {
        sample,
        width,
        height,
        action,
        shape,
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
    ),
    icon(0x0539_8cf4_ac14_cc98, 24, 24, Action::B, FULL),
    icon(0x974d_68d8_18cd_9bd0, 24, 32, Action::X, FULL),
    icon(0x911e_5729_f6e6_815a, 32, 24, Action::Y, FULL),
    icon(0x1a73_805e_f30e_f91c, 48, 24, Action::Z, FULL),
];

pub fn find(sample: u64, width: u32, height: u32) -> Option<&'static KnownIcon> {
    KNOWN
        .iter()
        .find(|k| k.sample == sample && k.width == width && k.height == height)
}
