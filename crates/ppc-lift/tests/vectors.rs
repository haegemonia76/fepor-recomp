//! Differential instruction tests. `vectors.bin` (tools/gen_vectors.py) holds programs with inputs and
//! expected outputs taken from Unicorn/QEMU (PowerPC 750CL) or, for Gekko-only instructions, from an
//! independent model. Each program is lifted to C++, compiled with clang and executed; every tracked
//! register and the memory window must match.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use fepor_core::analysis::analyze;
use fepor_core::config::AnalysisConfig;
use fepor_core::dol::Dol;
use fepor_ppc_lift::{generate, Options};
use ppc750cl::Ins;

const TEXT: u32 = 0x8000_3100;
const MEM_WORDS: usize = 32;

struct Case {
    ps1: bool,
    words: Vec<u32>,
    input: Vec<(u8, u8, u64)>,
    output: Vec<(u8, u8, u64)>,
}

struct Rd<'a>(&'a [u8]);
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

fn parse(data: &[u8]) -> Vec<Case> {
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
struct State {
    r: [u32; 32],
    f0: [u64; 32],
    f1: [u64; 32],
    cr: u32,
    xer: u32,
    lr: u32,
    ctr: u32,
    fpscr: u32,
    mem: [u32; MEM_WORDS],
}

impl State {
    fn zero() -> State {
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
    fn apply(&mut self, entries: &[(u8, u8, u64)]) {
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
    fn parse(rd: &mut Rd) -> State {
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
    fn diff(&self, want: &State, ps1: bool) -> Vec<String> {
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

const HARNESS: &str = r#"
#include <bit>
#include <cstdio>
#include <cstdlib>
#include <vector>
#include "fepor/ppc.h"
using namespace fepor;

static std::vector<uint8_t> slurp(const char* path) {
    FILE* f = fopen(path, "rb");
    if (!f) { perror(path); exit(2); }
    std::vector<uint8_t> v;
    uint8_t buf[65536];
    size_t n;
    while ((n = fread(buf, 1, sizeof buf, f)) > 0) v.insert(v.end(), buf, buf + n);
    fclose(f);
    return v;
}
struct Rd {
    const uint8_t* p;
    template <class T> T get() { T v; memcpy(&v, p, sizeof v); p += sizeof v; return v; }
};

int main(int argc, char** argv) {
    auto vec = slurp(argv[1]);
    auto addrs = slurp(argv[2]);
    FILE* out = fopen(argv[3], "wb");
    Rd r{vec.data() + 4};
    uint32_t n = r.get<uint32_t>();
    std::vector<uint8_t> ram(kRamSize + 64);
    const uint32_t MEM = 0x80002000;
    for (uint32_t i = 0; i < n; i++) {
        Context c{};
        c.ram = ram.data();
        for (uint32_t k = 0; k < 32; k++) st32(c.ram, MEM + 4 * k, 0);
        r.get<uint8_t>();
        uint8_t nw = r.get<uint8_t>();
        r.p += 4 * nw;
        uint16_t nin = r.get<uint16_t>();
        for (uint16_t e = 0; e < nin; e++) {
            uint8_t kind = r.get<uint8_t>(), idx = r.get<uint8_t>();
            uint64_t v = r.get<uint64_t>();
            switch (kind) {
                case 0: c.r[idx] = (uint32_t)v; break;
                case 1: c.f[idx].x = std::bit_cast<double>(v); break;
                case 2: c.f[idx].y = std::bit_cast<double>(v); break;
                case 3: for (int k = 0; k < 8; k++) { uint32_t nib = (uint32_t)v >> (28 - 4 * k) & 15; c.cr[k] = CRF{(uint8_t)(nib >> 3 & 1), (uint8_t)(nib >> 2 & 1), (uint8_t)(nib >> 1 & 1), (uint8_t)(nib & 1)}; } break;
                case 4: c.xer = xer_unpack((uint32_t)v); break;
                case 5: c.lr = (uint32_t)v; break;
                case 6: c.ctr = (uint32_t)v; break;
                case 7: c.fpscr = (uint32_t)v; break;
                case 8: st32(c.ram, MEM + 4 * idx, (uint32_t)v); break;
            }
        }
        uint16_t nout = r.get<uint16_t>();
        r.p += 10 * nout;
        uint32_t addr;
        memcpy(&addr, addrs.data() + 4 * i, 4);
        lookup(0, addr)(c);
        auto w32 = [&](uint32_t v) { fwrite(&v, 4, 1, out); };
        auto w64 = [&](uint64_t v) { fwrite(&v, 8, 1, out); };
        for (int k = 0; k < 32; k++) w32(c.r[k]);
        for (int k = 0; k < 32; k++) w64(std::bit_cast<uint64_t>(c.f[k].x));
        for (int k = 0; k < 32; k++) w64(std::bit_cast<uint64_t>(c.f[k].y));
        w32(mfcr(c.cr));
        w32(xer_pack(c.xer) & 0xE0000000u);
        w32(c.lr);
        w32(c.ctr);
        w32(c.fpscr & 3);
        for (uint32_t k = 0; k < 32; k++) w32(ld32(c.ram, MEM + 4 * k));
    }
    fclose(out);
    return 0;
}
"#;

fn build_dol(cases: &[Case]) -> (Dol, Vec<u32>) {
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

fn describe(words: &[u32]) -> String {
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

#[test]
fn lifted_instructions_match_the_reference_vectors() {
    let found = Command::new("clang++").arg("--version").output().is_ok();
    assert!(
        found || std::env::var_os("FEPOR_REQUIRE_CLANG").is_none(),
        "clang++ is required but not found"
    );
    if !found {
        eprintln!("clang++ not found, skipping");
        return;
    }
    let vec_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/vectors.bin");
    let raw = std::fs::read(&vec_path).unwrap();
    let cases = parse(&raw);
    let (dol, addrs) = build_dol(&cases);

    let analysis = analyze(&dol, &AnalysisConfig::default());
    // Unreachable code behind an always-taken branch may become extra functions; every program start must exist.
    let starts: std::collections::HashSet<u32> =
        analysis.functions.iter().map(|f| f.start).collect();
    for (i, a) in addrs.iter().enumerate() {
        assert!(
            starts.contains(a),
            "program {i} at {a:#x} is not a function: {}",
            describe(&cases[i].words)
        );
    }

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let runtime = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../runtime");
    let opts = Options {
        functions_per_unit: 256,
        runtime_dir: runtime.display().to_string(),
        trace_calls: false,
        hle: Vec::new(),
    };
    let stats = generate(&dol, &analysis, &opts, &mut |name, content| {
        std::fs::write(dir.join(name), content)
    })
    .unwrap();
    assert!(
        stats.unimplemented.is_empty(),
        "instructions emitted as traps: {:?}",
        stats.unimplemented
    );

    std::fs::write(dir.join("harness.cpp"), HARNESS).unwrap();
    let addr_bytes: Vec<u8> = addrs.iter().flat_map(|a| a.to_le_bytes()).collect();
    std::fs::write(dir.join("addrs.bin"), addr_bytes).unwrap();

    let mut cmd = Command::new("clang++");
    cmd.args(["-std=c++20", "-O1", "-w", "-I"])
        .arg(dir)
        .arg("-I")
        .arg(runtime.join("include"));
    for f in std::fs::read_dir(dir).unwrap() {
        let p = f.unwrap().path();
        if p.extension().is_some_and(|e| e == "cpp") {
            cmd.arg(p);
        }
    }
    cmd.arg(runtime.join("src/runtime.cpp"))
        .arg(runtime.join("src/fp.cpp"))
        .arg("-o")
        .arg(dir.join("harness"));
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "clang failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let run = Command::new(dir.join("harness"))
        .arg(&vec_path)
        .arg(dir.join("addrs.bin"))
        .arg(dir.join("results.bin"))
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "harness failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let results = std::fs::read(dir.join("results.bin")).unwrap();
    let mut rd = Rd(&results);

    let mut failures: BTreeMap<String, usize> = BTreeMap::new();
    let mut shown = 0;
    for (i, c) in cases.iter().enumerate() {
        let got = State::parse(&mut rd);
        let mut want = State::zero();
        want.apply(&c.input);
        want.apply(&c.output);
        let diffs = got.diff(&want, c.ps1);
        if !diffs.is_empty() {
            let name = Ins::new(c.words[c.words.len().saturating_sub(2).min(0)])
                .simplified()
                .mnemonic;
            *failures.entry(name.to_string()).or_default() += 1;
            if shown < 40 {
                shown += 1;
                let mut inputs = State::zero();
                inputs.apply(&c.input);
                eprintln!(
                    "case {i}: {}\n  {}",
                    describe(&c.words[..c.words.len() - 1]),
                    diffs.join("\n  ")
                );
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} cases differ: {:?}",
        failures.values().sum::<usize>(),
        cases.len(),
        failures
    );
}
