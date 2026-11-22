//! The instruction vectors of ADR 0008 run through the Cranelift backend: every program is compiled in
//! process, executed on a `Context`, and every tracked register and the memory window must match what
//! Unicorn (or the independent Gekko model) produced.

use std::collections::{BTreeMap, HashMap};

use fepor_core::analysis::analyze;
use fepor_core::config::AnalysisConfig;
use fepor_jit::Jit;
use fepor_runtime::context::{pack_cr, pack_xer, unpack_cr, unpack_xer};
use fepor_runtime::Context;

#[path = "../../ppc-lift/tests/common/mod.rs"]
mod common;
use common::*;

const MEM: usize = 0x2000; // 0x80002000, the memory window of the vectors
const RAM: usize = 0x0180_0000 + 64;

fn put32(ram: &mut [u8], at: usize, v: u32) {
    ram[at..at + 4].copy_from_slice(&v.to_be_bytes());
}

fn get32(ram: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(ram[at..at + 4].try_into().unwrap())
}

#[test]
fn jit_instructions_match_the_reference_vectors() {
    let raw = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../ppc-lift/tests/vectors.bin"
    ))
    .unwrap();
    let cases = parse(&raw);
    let (dol, addrs) = build_dol(&cases);
    let analysis = analyze(&dol, &AnalysisConfig::default());
    let jit = Jit::install(dol, &analysis, &[], &HashMap::new(), false);

    let mut ram = vec![0u8; RAM];
    let mut failures: BTreeMap<String, usize> = BTreeMap::new();
    let mut shown = 0;
    let mut changed = 0;
    for (i, case) in cases.iter().enumerate() {
        let mut c = Context::new(ram.as_mut_ptr());
        for k in 0..MEM_WORDS {
            put32(&mut ram, MEM + 4 * k, 0);
        }
        let mut want = State::zero();
        want.apply(&case.input);
        for &(kind, idx, v) in &case.input {
            let i = idx as usize;
            match kind {
                0 => c.r[i] = v as u32,
                1 => c.f[i].ps0 = f64::from_bits(v),
                2 => c.f[i].ps1 = f64::from_bits(v),
                3 => unpack_cr(&mut c.cr, v as u32),
                4 => c.xer = unpack_xer(v as u32),
                5 => c.lr = v as u32,
                6 => c.ctr = v as u32,
                7 => c.fpscr = v as u32,
                8 => put32(&mut ram, MEM + 4 * i, v as u32),
                _ => unreachable!(),
            }
        }
        let f = jit.entry(addrs[i]).expect("program start is a function");
        // SAFETY: a compiled function takes the guest context.
        unsafe { f(&mut *c) };

        let mut got = State::zero();
        got.r = c.r;
        for k in 0..32 {
            got.f0[k] = c.f[k].ps0.to_bits();
            got.f1[k] = c.f[k].ps1.to_bits();
        }
        got.cr = pack_cr(&c.cr);
        got.xer = pack_xer(&c.xer) & 0xE000_0000;
        got.lr = c.lr;
        got.ctr = c.ctr;
        got.fpscr = c.fpscr & 3;
        for k in 0..MEM_WORDS {
            got.mem[k] = get32(&ram, MEM + 4 * k);
        }
        let mut want = State::zero();
        want.apply(&case.input);
        let before = want.clone();
        want.apply(&case.output);
        changed += (got != before) as usize;
        let diffs = got.diff(&want, case.ps1);
        if !diffs.is_empty() {
            let name = describe(&case.words[..1])
                .split_whitespace()
                .nth(1)
                .unwrap_or("?")
                .to_string();
            *failures.entry(name).or_default() += 1;
            if shown < 25 {
                shown += 1;
                eprintln!(
                    "case {i}: {}\n  {}",
                    describe(&case.words[..case.words.len() - 1]),
                    diffs.join("\n  ")
                );
            }
        }
    }
    // the programs really did something (a run that executed nothing would also 'match' empty expectations)
    assert!(
        changed > cases.len() * 9 / 10,
        "only {changed} of {} programs changed the state",
        cases.len()
    );
    assert!(
        failures.is_empty(),
        "{} of {} cases differ: {:?}",
        failures.values().sum::<usize>(),
        cases.len(),
        failures
    );
}
