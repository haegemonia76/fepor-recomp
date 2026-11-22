//! Shared by the vector tests of both backends (the C++ one in `ppc-lift`, the JIT in `fepor-jit`).
#![allow(dead_code)]

use fepor_core::dol::Dol;
use ppc750cl::Ins;

pub const TEXT: u32 = 0x8000_3100;
pub const MEM_WORDS: usize = 32;
pub struct Case {
    pub ps1: bool,
    pub words: Vec<u32>,
    pub input: Vec<(u8, u8, u64)>,
    pub output: Vec<(u8, u8, u64)>,
}

pub struct Rd<'a>(pub &'a [u8]);
impl Rd<'_> {
    fn take<const N: usize>(&mut self) -> [u8; N] {
        let (a, b) = self.0.split_at(N);
        self.0 = b;
        a.try_into().unwrap()
    }
    fn u8(&mut self) -> u8 {
        self.take::<1>()[0]
    }
    fn u16(&mut self) -> u16 {
        u16::from_le_bytes(self.take())
    }
    fn u32(&mut self) -> u32 {
        u32::from_le_bytes(self.take())
    }
    fn u64(&mut self) -> u64 {
        u64::from_le_bytes(self.take())
    }
    fn entries(&mut self) -> Vec<(u8, u8, u64)> {
        let n = self.u16();
        (0..n).map(|_| (self.u8(), self.u8(), self.u64())).collect()
    }
}

pub fn parse(data: &[u8]) -> Vec<Case> {
    assert_eq!(&data[..4], b"FPV2");
    let mut r = Rd(&data[4..]);
    let n = r.u32();
    (0..n)
        .map(|_| {
            let flags = r.u8();
            let nw = r.u8();
            let words = (0..nw).map(|_| r.u32()).collect();
            let input = r.entries();
            let output = r.entries();
            Case {
                ps1: flags & 1 != 0,
                words,
                input,
                output,
            }
        })
        .collect()
}

/// Final state as dumped by the harness, in a fixed layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct State {
    pub r: [u32; 32],
    pub f0: [u64; 32],
    pub f1: [u64; 32],
    pub cr: u32,
    pub xer: u32,
    pub lr: u32,
    pub ctr: u32,
    pub fpscr: u32,
    pub mem: [u32; MEM_WORDS],
}

impl State {
    pub fn zero() -> State {
        State {
            r: [0; 32],
            f0: [0; 32],
            f1: [0; 32],
            cr: 0,
            xer: 0,
            lr: 0,
            ctr: 0,
            fpscr: 0,
            mem: [0; MEM_WORDS],
        }
    }
    pub fn apply(&mut self, entries: &[(u8, u8, u64)]) {
        for &(kind, idx, v) in entries {
            let i = idx as usize;
            match kind {
                0 => self.r[i] = v as u32,
                1 => self.f0[i] = v,
                2 => self.f1[i] = v,
                3 => self.cr = v as u32,
                4 => self.xer = v as u32,
                5 => self.lr = v as u32,
                6 => self.ctr = v as u32,
                7 => self.fpscr = v as u32,
                8 => self.mem[i] = v as u32,
                _ => panic!("bad kind"),
            }
        }
    }
    pub fn parse(rd: &mut Rd) -> State {
        let mut s = State::zero();
        for x in &mut s.r {
            *x = rd.u32();
        }
        for x in &mut s.f0 {
            *x = rd.u64();
        }
        for x in &mut s.f1 {
            *x = rd.u64();
        }
        s.cr = rd.u32();
        s.xer = rd.u32();
        s.lr = rd.u32();
        s.ctr = rd.u32();
        s.fpscr = rd.u32();
        for x in &mut s.mem {
            *x = rd.u32();
        }
        s
    }
    pub fn diff(&self, want: &State, ps1: bool) -> Vec<String> {
        let mut d = Vec::new();
        for i in 0..32 {
            if self.r[i] != want.r[i] {
                d.push(format!(
                    "r{i}: got {:#010x} want {:#010x}",
                    self.r[i], want.r[i]
                ));
            }
            if self.f0[i] != want.f0[i] {
                d.push(format!(
                    "f{i}.ps0: got {:#018x} ({}) want {:#018x} ({})",
                    self.f0[i],
                    f64::from_bits(self.f0[i]),
                    want.f0[i],
                    f64::from_bits(want.f0[i])
                ));
            }
            if ps1 && self.f1[i] != want.f1[i] {
                d.push(format!(
                    "f{i}.ps1: got {:#018x} ({}) want {:#018x} ({})",
                    self.f1[i],
                    f64::from_bits(self.f1[i]),
                    want.f1[i],
                    f64::from_bits(want.f1[i])
                ));
            }
        }
        // XER is checked through the programs' `mfxer r0` epilogue, FPSCR fields through `mcrfs`.
        for (name, a, b) in [
            ("cr", self.cr, want.cr),
            ("lr", self.lr, want.lr),
            ("ctr", self.ctr, want.ctr),
        ] {
            if a != b {
                d.push(format!("{name}: got {a:#010x} want {b:#010x}"));
            }
        }
        for i in 0..MEM_WORDS {
            if self.mem[i] != want.mem[i] {
                d.push(format!(
                    "mem[{:#x}]: got {:#010x} want {:#010x}",
                    i * 4,
                    self.mem[i],
                    want.mem[i]
                ));
            }
        }
        d
    }
}

pub fn build_dol(cases: &[Case]) -> (Dol, Vec<u32>) {
    let mut text: Vec<u32> = Vec::new();
    let mut addrs = Vec::new();
    for c in cases {
        addrs.push(TEXT + 4 * text.len() as u32);
        text.extend(&c.words);
        text.push(0); // padding separates programs
    }
    let mut h = vec![0u8; 0x100];
    let put =
        |h: &mut Vec<u8>, off: usize, v: u32| h[off..off + 4].copy_from_slice(&v.to_be_bytes());
    put(&mut h, 0, 0x100);
    put(&mut h, 0x48, TEXT);
    put(&mut h, 0x90, text.len() as u32 * 4);
    put(&mut h, 0xE0, TEXT);
    for w in &text {
        h.extend_from_slice(&w.to_be_bytes());
    }
    (Dol::parse(h).unwrap(), addrs)
}

pub fn describe(words: &[u32]) -> String {
    words
        .iter()
        .map(|&w| {
            let p = Ins::new(w).simplified();
            let args: Vec<String> = p.args_iter().map(|a| a.to_string()).collect();
            format!("{:08x} {} {}", w, p.mnemonic, args.join(", "))
        })
        .collect::<Vec<_>>()
        .join(" ; ")
}
