//! Writes a PNG with every generated button icon, for looking at them: `cargo run -p fepor-overlay
//! --example glyph_sheet -- out.png`. Rows are the controller families, columns the gamepad buttons.

use fepor_input::GlyphSet;

fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "glyphs.png".into());
    let sources = [
        "a",
        "b",
        "x",
        "y",
        "leftshoulder",
        "rightshoulder",
        "lefttrigger",
        "righttrigger",
        "start",
        "dpup",
    ];
    let families = [GlyphSet::Xbox, GlyphSet::Nintendo, GlyphSet::PlayStation];
    let (cell_w, cell_h) = (96u32, 64u32);
    let (w, h) = (
        cell_w * sources.len() as u32,
        cell_h * families.len() as u32,
    );
    let mut sheet = vec![0x40u8; (w * h * 4) as usize];
    for p in sheet.chunks_exact_mut(4) {
        p[3] = 255;
    }
    for (row, family) in families.iter().enumerate() {
        for (col, source) in sources.iter().enumerate() {
            // face buttons are square, the rest wide, as the game's own textures are
            let (iw, ih) = if matches!(*source, "a" | "b" | "x" | "y") {
                (56, 56)
            } else {
                (88, 44)
            };
            let shape = fepor_overlay::glyphs::Shape::default();
            let Some(img) = fepor_overlay::glyphs::render(*family, source, iw, ih, shape) else {
                continue;
            };
            let (ox, oy) = (
                col as u32 * cell_w + (cell_w - iw) / 2,
                row as u32 * cell_h + (cell_h - ih) / 2,
            );
            for y in 0..ih {
                for x in 0..iw {
                    let s = ((y * iw + x) * 4) as usize;
                    let d = (((oy + y) * w + ox + x) * 4) as usize;
                    let a = img[s + 3] as u32;
                    for k in 0..3 {
                        sheet[d + k] =
                            ((img[s + k] as u32 * a + sheet[d + k] as u32 * (255 - a)) / 255) as u8;
                    }
                }
            }
        }
    }
    let file = std::fs::File::create(&out).expect("create png");
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()
        .expect("png header")
        .write_image_data(&sheet)
        .expect("png data");
    println!("wrote {out}");
}
