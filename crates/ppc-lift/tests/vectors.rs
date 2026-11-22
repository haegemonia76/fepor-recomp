//! Differential instruction tests. `vectors.bin` (tools/gen_vectors.py) holds programs with inputs and
//! expected outputs taken from Unicorn/QEMU (PowerPC 750CL) or, for Gekko-only instructions, from an
//! independent model. Each program is lifted to C++, compiled with clang and executed; every tracked
//! register and the memory window must match.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use fepor_core::analysis::analyze;
use fepor_core::config::AnalysisConfig;
use fepor_ppc_lift::{generate, Options};
use ppc750cl::Ins;

mod common;
use common::*;

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
