//! Command stream interpreter.

use std::sync::Arc;

use crate::bp::{self, Bp};
use crate::cp::{Cp, VertexFormat};
use crate::state::{CopyEfb, DrawCall, Event, Primitive, RenderState, Sink, Tluts};
use crate::vertex;
use crate::xf::Xf;

#[derive(Default, Debug, Clone)]
pub struct Stats {
    pub draws: u64,
    pub vertices: u64,
    pub copies: u64,
    pub unknown: u64,
    pub bytes: u64,
}

pub struct Gx {
    pub cp: Cp,
    pub xf: Xf,
    pub bp: Bp,
    pub tluts: Tluts,
    pub stats: Stats,
    log_frame: Option<u64>,
    buf: Vec<u8>,
    snapshot: Option<Arc<RenderState>>,
    trace_tlut: bool,
    /// Set when a command is not valid: the stream is out of step and is skipped until a run of zero bytes.
    lost: bool,
    zero_run: usize,
}

enum Step {
    /// Command complete, this many bytes consumed.
    Done(usize),
    NeedMore,
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

impl Gx {
    pub fn new() -> Gx {
        Gx {
            cp: Cp::default(),
            xf: Xf::default(),
            bp: Bp::default(),
            tluts: Tluts::default(),
            stats: Stats::default(),
            buf: Vec::new(),
            snapshot: None,
            log_frame: std::env::var("FEPOR_GX_LOG")
                .ok()
                .and_then(|v| v.parse().ok()),
            trace_tlut: std::env::var("FEPOR_TRACE")
                .is_ok_and(|v| v.split(',').any(|x| x == "tlut")),
            lost: false,
            zero_run: 0,
        }
    }

    /// Append bytes written to the write-gather pipe and run every complete command.
    pub fn feed(&mut self, bytes: &[u8], ram: &[u8], sink: &mut dyn Sink) {
        self.stats.bytes += bytes.len() as u64;
        self.buf.extend_from_slice(bytes);
        let mut data = std::mem::take(&mut self.buf);
        let mut pos = 0;
        while pos < data.len() {
            match self.exec(&data[pos..], ram, sink) {
                Step::Done(n) => pos += n,
                Step::NeedMore => break,
            }
        }
        // what is left (a command split between two bursts) moves to the front: no new allocation
        data.drain(..pos);
        self.buf = data;
    }

    fn render_state(&mut self) -> Arc<RenderState> {
        if self.snapshot.is_none() {
            self.snapshot = Some(Arc::new(self.bp.render_state(&self.xf)));
        }
        self.snapshot.clone().unwrap()
    }

    fn exec(&mut self, d: &[u8], ram: &[u8], sink: &mut dyn Sink) -> Step {
        let op = d[0];
        if self.lost {
            // `GXFlush` pads the pipe to 32 bytes with zeros: a run of them is where the game's commands start
            // again. Vertex data seldom has so many zero bytes in a row.
            if op == 0 {
                self.zero_run += 1;
                if self.zero_run >= 8 {
                    self.lost = false;
                    self.zero_run = 0;
                }
            } else {
                self.zero_run = 0;
            }
            return Step::Done(1);
        }
        if self.log_frame == Some(self.stats.copies) && d.len() >= 5 {
            match op {
                0x61 => eprintln!(
                    "gxlog: BP {:#04x} <- {:#08x}",
                    d[1],
                    be32(&d[1..]) & 0xFF_FFFF
                ),
                0x10 => eprintln!(
                    "gxlog: XF load {:#06x} (+{})",
                    be32(&d[1..]) & 0xFFFF,
                    (be32(&d[1..]) >> 16) + 1
                ),
                0x08 => eprintln!("gxlog: CP {:#04x} <- {:#010x}", d[1], be32(&d[2..])),
                0x80..=0xBF => eprintln!(
                    "gxlog: draw op {op:#04x} count {}",
                    u16::from_be_bytes([d[1], d[2]])
                ),
                _ => eprintln!("gxlog: op {op:#04x}"),
            }
        }
        match op {
            // NOP, the "reset" and metrics opcodes (nothing to do), and vertex cache invalidation
            0x00 | 0x01 | 0x44 | 0x48 => Step::Done(1),
            0x08 => {
                if d.len() < 6 {
                    return Step::NeedMore;
                }
                self.cp.load(d[1], be32(&d[2..]));
                Step::Done(6)
            }
            0x10 => {
                if d.len() < 5 {
                    return Step::NeedMore;
                }
                let h = be32(&d[1..]);
                // the length field holds 1 to 16 words: the hardware (and Dolphin) read 4 bits of it
                let (count, addr) = ((h >> 16 & 0xF) as usize + 1, (h & 0xFFFF) as usize);
                if d.len() < 5 + 4 * count {
                    return Step::NeedMore;
                }
                for i in 0..count {
                    self.xf.write(addr + i, be32(&d[5 + 4 * i..]));
                }
                // the snapshot reads only XF registers (the viewport): matrix and light loads, which come
                // before every object, keep it
                if addr + count > 0x1000 {
                    self.snapshot = None;
                }
                Step::Done(5 + 4 * count)
            }
            0x20 | 0x28 | 0x30 | 0x38 => {
                if d.len() < 5 {
                    return Step::NeedMore;
                }
                let v = be32(&d[1..]);
                let (index, count, addr) = (
                    (v >> 16) as usize,
                    (v >> 12 & 15) as usize + 1,
                    (v & 0xFFF) as usize,
                );
                let n = 12 + ((op - 0x20) / 8) as usize;
                let base =
                    self.cp.array_base[n] as usize + index * self.cp.array_stride[n] as usize;
                for i in 0..count {
                    if let Some(w) = ram.get(base + 4 * i..base + 4 * i + 4) {
                        self.xf.write(addr + i, be32(w));
                    }
                }
                // indexed loads reach 0x0000..0x0FFF only (matrices, normals, lights): not part of the snapshot
                Step::Done(5)
            }
            0x40 => {
                if d.len() < 9 {
                    return Step::NeedMore;
                }
                let (addr, size) = (
                    (be32(&d[1..]) & 0x01FF_FFFF) as usize,
                    be32(&d[5..]) as usize,
                );
                if self.trace_tlut {
                    eprintln!("tlut: display list {addr:08x} size {size:#x}");
                }
                if let Some(list) = ram.get(addr..addr + size) {
                    let list = list.to_vec();
                    let mut pos = 0;
                    while pos < list.len() {
                        match self.exec(&list[pos..], ram, sink) {
                            Step::Done(n) => pos += n,
                            Step::NeedMore => break, // a truncated list
                        }
                    }
                }
                // a list is self-contained: what is wrong inside it does not carry over
                self.lost = false;
                self.zero_run = 0;
                Step::Done(9)
            }
            0x61 => {
                if d.len() < 5 {
                    return Step::NeedMore;
                }
                let v = be32(&d[1..]);
                self.bp_write((v >> 24) as usize, v & 0x00FF_FFFF, ram, sink);
                Step::Done(5)
            }
            0x80..=0xBF => {
                if d.len() < 3 {
                    return Step::NeedMore;
                }
                let count = u16::from_be_bytes([d[1], d[2]]) as usize;
                let fmt = VertexFormat::new(&self.cp, (op & 7) as usize);
                let size = vertex::vertex_size(&fmt);
                if d.len() < 3 + count * size {
                    return Step::NeedMore;
                }
                let prim = match (op >> 3) & 7 {
                    0 => Some(Primitive::Quads),
                    2 => Some(Primitive::Triangles),
                    3 => Some(Primitive::TriangleStrip),
                    4 => Some(Primitive::TriangleFan),
                    5 => Some(Primitive::Lines),
                    6 => Some(Primitive::LineStrip),
                    7 => Some(Primitive::Points),
                    _ => None,
                };
                if let Some(prim) = prim {
                    let spare = sink.recycle();
                    let vertices = vertex::decode(
                        &fmt,
                        &self.cp,
                        &self.xf,
                        ram,
                        &d[3..3 + count * size],
                        count,
                        spare,
                    );
                    self.stats.draws += 1;
                    self.stats.vertices += count as u64;
                    let state = self.render_state();
                    sink.event(
                        Event::Draw(DrawCall {
                            prim,
                            vertices,
                            state,
                        }),
                        ram,
                        &self.tluts,
                    );
                }
                Step::Done(3 + count * size)
            }
            _ => {
                // The stream is out of step: a draw's vertices did not match the format the game had described
                // (or a command was split wrongly). What follows is vertex data read as commands, which can
                // overwrite any register, so nothing is run until the game's next flush (see `lost`).
                self.stats.unknown += 1;
                if !self.lost && self.stats.unknown <= 8 {
                    eprintln!(
                        "gx: command {op:#04x} is not valid here; the stream is out of step, skipping to the next flush (number {})",
                        self.stats.unknown
                    );
                }
                self.lost = true;
                self.zero_run = 0;
                Step::Done(1)
            }
        }
    }

    fn bp_write(&mut self, reg: usize, value: u32, ram: &[u8], sink: &mut dyn Sink) {
        if self.trace_tlut && matches!(reg, 0x60..=0x6F | 0x98..=0x9B) {
            eprintln!("tlut: bp {reg:#04x} <- {value:#08x}");
        }
        self.bp.write(reg, value);
        match reg {
            bp::TRIGGER_COPY => {
                self.stats.copies += 1;
                let r = &self.bp.regs;
                let (tl, wh) = (r[bp::EFB_TL], r[bp::EFB_WH]);
                let (ar, gb) = (r[bp::CLEAR_AR], r[bp::CLEAR_GB]);
                let f = value >> 3 & 15;
                let copy = CopyEfb {
                    x: tl & 0x3FF,
                    y: tl >> 10 & 0x3FF,
                    width: (wh & 0x3FF) + 1,
                    height: (wh >> 10 & 0x3FF) + 1,
                    dest: r[bp::EFB_ADDR] << 5,
                    stride: (r[bp::EFB_STRIDE] & 0x3FF) << 5,
                    to_xfb: value >> 14 & 1 != 0,
                    clear: value >> 11 & 1 != 0,
                    clear_color: [
                        (ar & 255) as u8,
                        (gb >> 8 & 255) as u8,
                        (gb & 255) as u8,
                        (ar >> 8 & 255) as u8,
                    ],
                    clear_z: r[bp::CLEAR_Z],
                    half_scale: value >> 9 & 1 != 0,
                    format: (f / 2 + (f & 1) * 8) as u8,
                    intensity: value >> 15 & 1 != 0,
                    color_update: r[bp::BLENDMODE] >> 3 & 1 != 0,
                    alpha_update: r[bp::BLENDMODE] >> 4 & 1 != 0,
                    z_update: r[bp::ZMODE] >> 4 & 1 != 0,
                };
                sink.event(Event::Copy(copy), ram, &self.tluts);
            }
            bp::SET_DRAW_DONE => sink.event(Event::Finish, ram, &self.tluts),
            bp::PE_TOKEN | bp::PE_TOKEN_INT => {
                sink.event(Event::Token(value as u16), ram, &self.tluts)
            }
            bp::LOAD_TLUT1 => {
                // main memory is mirrored every 32 MiB: the register has junk bits above the address (the game sets
                // them), which the hardware ignores
                let src =
                    (((self.bp.regs[bp::LOAD_TLUT0] & 0x00FF_FFFF) << 5) & 0x01FF_FFFF) as usize;
                let (tmem, count) = (value & 0x3FF, (value >> 10 & 0x7FF) as usize);
                if let Some(data) = ram.get(src..src + count * 32) {
                    if self.trace_tlut {
                        eprintln!(
                            "tlut: load {src:08x} -> tmem {tmem:x}, {count} blocks, first entries {:02x?}",
                            &data[..8.min(data.len())]
                        );
                    }
                    self.tluts.data.insert(tmem, data.to_vec());
                }
            }
            _ => {}
        }
        if !matches!(
            reg,
            bp::TRIGGER_COPY | bp::SET_DRAW_DONE | bp::PE_TOKEN | bp::PE_TOKEN_INT
        ) {
            self.snapshot = None;
        }
    }
}

impl Default for Gx {
    fn default() -> Self {
        Gx::new()
    }
}
