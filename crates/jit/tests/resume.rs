//! A resume variant (`Jit::resume_entry`, ADR 0035) continues a function after one of its calls with the
//! registers in the context, without running the callee again.

use std::collections::HashMap;

use fepor_core::analysis::analyze;
use fepor_core::config::AnalysisConfig;
use fepor_jit::Jit;
use fepor_runtime::abi::SPR_RESUME_PC;
use fepor_runtime::Context;

#[path = "../../ppc-lift/tests/common/mod.rs"]
mod common;
use common::*;

const RAM: usize = 0x0180_0000 + 64;

#[test]
fn a_function_resumes_after_its_call_with_the_registers_it_had() {
    // A: the usual prologue, `bl B`, then r3 += 100 and the epilogue. B: r3 += 1.
    let a = TEXT;
    let words = vec![
        0x7C08_02A6, // mflr r0
        0x9001_0004, // stw r0, 4(r1)
        0x9421_FFF0, // stwu r1, -16(r1)
        0x4800_001D, // bl B (7 words on)
        0x3863_0064, // addi r3, r3, 100   <- the resume point
        0x8001_0014, // lwz r0, 20(r1)
        0x3821_0010, // addi r1, r1, 16
        0x7C08_03A6, // mtlr r0
        0x4E80_0020, // blr
        0,
        0x3863_0001, // B: addi r3, r3, 1
        0x4E80_0020, // blr
    ];
    let case = Case {
        ps1: false,
        words,
        input: vec![],
        output: vec![],
    };
    let (dol, _) = build_dol(&[case]);
    let analysis = analyze(&dol, &AnalysisConfig::default());
    let jit = Jit::install(dol, &analysis, &[], &HashMap::new(), false);
    let mut ram = vec![0u8; RAM];

    // normal call: B runs, then the rest of A
    let mut c = Context::new(ram.as_mut_ptr());
    c.r[1] = 0x8000_2100;
    c.r[3] = 0;
    c.lr = 0x8000_2200;
    // SAFETY: compiled functions take the guest context.
    unsafe { jit.entry(a).expect("A is a function")(&mut *c) };
    assert_eq!(c.r[3], 101);
    assert_eq!(c.lr, 0x8000_2200);
    assert_eq!(c.r[1], 0x8000_2100);

    // resumed after the call: as if B had just returned r3 = 5, with A's frame in memory
    let mut c = Context::new(ram.as_mut_ptr());
    c.r[1] = 0x8000_20F0;
    c.r[3] = 5;
    ram[0x20F0 + 20..0x20F0 + 24].copy_from_slice(&0x8000_2200u32.to_be_bytes());
    c.spr[SPR_RESUME_PC] = a + 16;
    let f = jit.resume_entry(a + 16).expect("a resume variant");
    // SAFETY: as above.
    unsafe { f(&mut *c) };
    assert_eq!(c.r[3], 105, "the callee must not run again");
    assert_eq!(
        c.lr, 0x8000_2200,
        "LR is the return address of A's caller, as the epilogue restored it"
    );
    assert_eq!(c.r[1], 0x8000_2100);

    // an address outside every function has no variant
    assert!(jit.resume_entry(0x8000_0000).is_none());
}
