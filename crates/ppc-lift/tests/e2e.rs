//! Generates C++ for a small synthetic DOL, compiles it with clang and runs it.

use std::path::Path;
use std::process::Command;

use fepor_core::analysis::analyze;
use fepor_core::config::AnalysisConfig;
use fepor_core::dol::Dol;
use fepor_ppc_lift::{generate, Options};

const TEXT: u32 = 0x8000_3100;
const DATA: u32 = 0x8001_0000;

fn dol(text: &[u32], data: &[u32]) -> Dol {
    let mut h = vec![0u8; 0x100];
    let tlen = text.len() as u32 * 4;
    let put =
        |h: &mut Vec<u8>, off: usize, v: u32| h[off..off + 4].copy_from_slice(&v.to_be_bytes());
    put(&mut h, 0, 0x100);
    put(&mut h, 0x48, TEXT);
    put(&mut h, 0x90, tlen);
    put(&mut h, 7 * 4, 0x100 + tlen);
    put(&mut h, 0x48 + 7 * 4, DATA);
    put(&mut h, 0x90 + 7 * 4, data.len() as u32 * 4);
    put(&mut h, 0xE0, TEXT);
    for w in text.iter().chain(data) {
        h.extend_from_slice(&w.to_be_bytes());
    }
    Dol::parse(h).unwrap()
}

/// CI sets FEPOR_REQUIRE_CLANG so a missing compiler fails instead of silently skipping.
fn have_clang() -> bool {
    let found = Command::new("clang++").arg("--version").output().is_ok();
    assert!(
        found || std::env::var_os("FEPOR_REQUIRE_CLANG").is_none(),
        "clang++ is required but not found"
    );
    found
}

/// Build the generated code plus `main_cpp` with clang and return the program's stdout.
fn build_and_run(dol: &Dol, main_cpp: &str) -> String {
    let analysis = analyze(dol, &AnalysisConfig::default());
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let runtime = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../runtime");
    let opts = Options {
        functions_per_unit: 2,
        runtime_dir: runtime.display().to_string(),
        trace_calls: false,
        hle: Vec::new(),
    };
    let stats = generate(dol, &analysis, &opts, &mut |name, content| {
        std::fs::write(dir.join(name), content)
    })
    .unwrap();
    assert_eq!(stats.unimplemented.len(), 0, "{:?}", stats.unimplemented);
    std::fs::write(dir.join("main.cpp"), main_cpp).unwrap();

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
        .arg(runtime.join("src/fp.cpp"));
    cmd.arg("-o").arg(dir.join("prog"));
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "clang failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(dir.join("prog")).output().unwrap();
    assert!(
        run.status.success(),
        "program failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    String::from_utf8(run.stdout).unwrap()
}

#[test]
fn calls_loops_memory_jump_tables_and_tail_calls_execute_correctly() {
    if !have_clang() {
        eprintln!("clang++ not found, skipping");
        return;
    }
    let t = |off: u32| TEXT + off;
    let text = [
        0x3860_0005,                   // 00 f0: li r3,5
        0x4800_002D,                   // 04 bl f1 (0x30)
        0x3863_0001,                   // 08 addi r3,r3,1
        0x3880_0003,                   // 0c li r4,3
        0x7C89_03A6,                   // 10 mtctr r4
        0x3863_000A,                   // 14 addi r3,r3,10
        0x4200_FFFC,                   // 18 bdnz 0x14
        0x9061_0010,                   // 1c stw r3,0x10(r1)
        0x8081_0010,                   // 20 lwz r4,0x10(r1)
        0x7C63_2214,                   // 24 add r3,r3,r4
        0x38A0_0001,                   // 28 li r5,1
        0x4800_0015,                   // 2c b f2 (0x40) -- bl bit set below
        0x5463_103A,                   // 30 f1: slwi r3,r3,2
        0x4E80_0020,                   // 34 blr
        0,                             // 38
        0,                             // 3c
        0x2805_0002,                   // 40 f2: cmplwi r5,2
        0x4181_0034,                   // 44 bgt default (0x78)
        0x3CC0_0000 | (DATA >> 16),    // 48 lis r6,hi
        0x38C6_0000 | (DATA & 0xFFFF), // 4c addi r6,r6,lo
        0x54A0_103A,                   // 50 slwi r0,r5,2
        0x7C06_002E,                   // 54 lwzx r0,r6,r0
        0x7C09_03A6,                   // 58 mtctr r0
        0x4E80_0420,                   // 5c bctr
        0x3860_0064,                   // 60 case0: li r3,100
        0x4E80_0020,                   // 64 blr
        0x3863_03E8,                   // 68 case1: addi r3,r3,1000
        0x4E80_0020,                   // 6c blr
        0x3860_012C,                   // 70 case2: li r3,300
        0x4E80_0020,                   // 74 blr
        0x3860_FFFF,                   // 78 default: li r3,-1
        0x4E80_0020,                   // 7c blr
    ];
    let mut text = text;
    text[1] = 0x4800_0001 | 0x2C; // bl +0x2c
    text[11] = 0x4800_0000 | 0x14; // b +0x14 (tail call)
    let data = [t(0x60), t(0x68), t(0x70)];
    let main_cpp = r#"
#include <cstdio>
#include <vector>
#include "fepor/ppc.h"
#include "functions.h"
int main() {
    std::vector<uint8_t> ram(fepor::kRamSize + 64);
    fepor::Context c{};
    c.ram = ram.data();
    c.r[1] = 0x80100000;
    // The DOL's data section is not loaded by the generated code: place the jump table.
    fepor::st32(c.ram, 0x80010000, 0x80003160);
    fepor::st32(c.ram, 0x80010004, 0x80003168);
    fepor::st32(c.ram, 0x80010008, 0x80003170);
    f_80003100(c);
    printf("%u %u\n", c.r[3], c.ctr);
    return 0;
}
"#;
    let out = build_and_run(&dol(&text, &data), main_cpp);
    // r3: 5 -> f1 (<<2) = 20 -> +1 = 21 -> 3 loop iterations +10 = 51 -> store/load/add = 102 -> case1 +1000.
    assert_eq!(out.trim(), "1102 2147496296"); // ctr holds the jump-table target 0x80003168
}

#[test]
fn hle_functions_become_runtime_calls_and_resuming_calls_jump_back_to_the_save_site() {
    // f0: bl save; blr-free body: bl load; blr.   save at 0x10, load at 0x18 (both hooked).
    let text = [
        0x4800_0011u32, // 00 f0: bl save (0x10)
        0x4800_0019,    // 04 bl load (0x1c)
        0x4E80_0020,    // 08 blr
        0,              // 0c
        0x3860_0000,    // 10 save: li r3,0
        0x4E80_0020,    // 14 blr
        0,              // 18
        0x4E80_0020,    // 1c load: blr
    ];
    let d = dol(&text, &[]);
    let analysis = analyze(&d, &AnalysisConfig::default());
    let hle = vec![
        fepor_core::config::HleEntry {
            addr: TEXT + 0x10,
            name: "Save".into(),
            ..Default::default()
        },
        fepor_core::config::HleEntry {
            addr: TEXT + 0x1C,
            name: "Load".into(),
            unwinds: true,
            resumes_at: Some(TEXT + 0x10),
        },
    ];
    let opts = Options {
        functions_per_unit: 16,
        runtime_dir: String::new(),
        trace_calls: false,
        hle,
    };
    let mut files = std::collections::BTreeMap::new();
    generate(&d, &analysis, &opts, &mut |n, c| {
        files.insert(n.to_string(), c.to_string());
        Ok(())
    })
    .unwrap();
    let unit = &files["unit_0000.cpp"];
    assert!(
        unit.contains("fepor_hle_Save(&c);") && unit.contains("fepor_hle_Load(&c);"),
        "{unit}"
    );
    // after the load call: unwind check, then back to the instruction after the save call (0x80003104)
    let call = unit.find("f_8000311c(c);").expect("call to load");
    let tail = &unit[call..];
    assert!(tail.contains("if (c.spr[1023] == 1) unwind();"), "{tail}");
    assert!(tail.contains("goto L_80003104;"), "{tail}");
    assert!(unit.contains("L_80003104:;"));
    assert!(files["functions.h"].contains("extern \"C\" void fepor_hle_Load(fepor::Context*);"));
}
