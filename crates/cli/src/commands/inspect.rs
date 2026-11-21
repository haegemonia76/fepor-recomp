//! Small inspection helpers for working on the lifter and analysis.

use std::collections::BTreeMap;

use ppc750cl::Ins;

use crate::session::{Fail, Options, Session};

pub fn parse_addr(s: &str) -> Result<u32, String> {
    let t = s.trim_start_matches("0x");
    u32::from_str_radix(t, 16).map_err(|e| format!("bad address {s:?}: {e}"))
}

pub fn disasm(opts: &Options, addr: u32, count: u32) -> Result<(), Fail> {
    let session = Session::open(opts)?;
    for k in 0..count {
        let a = addr + 4 * k;
        let word = session
            .dol
            .read_u32(a)
            .ok_or_else(|| Fail::Usage(format!("{a:#010x} is outside the DOL image")))?;
        let p = Ins::new(word).simplified();
        let args: Vec<String> = p.args_iter().map(|x| x.to_string()).collect();
        println!("{a:08x}: {word:08x}  {} {}", p.mnemonic, args.join(", "));
    }
    Ok(())
}

pub fn opcodes(opts: &Options) -> Result<(), Fail> {
    let session = Session::open(opts)?;
    let mut hist: BTreeMap<String, usize> = BTreeMap::new();
    for s in session.dol.text_sections() {
        for w in session.dol.section_bytes(s).chunks_exact(4) {
            let ins = Ins::new(u32::from_be_bytes(w.try_into().unwrap()));
            *hist.entry(format!("{:?}", ins.op)).or_default() += 1;
        }
    }
    let mut rows: Vec<_> = hist.into_iter().collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.1));
    for (op, n) in rows {
        println!("{op} {n}");
    }
    Ok(())
}

/// Every occurrence of an opcode, with the function (from the analysis) it sits in.
pub fn find(opts: &Options, op: Option<&str>, callers: Option<u32>) -> Result<(), Fail> {
    let session = Session::open(opts)?;
    let analysis = fepor_core::analysis::analyze(&session.dol, &session.analysis_cfg);
    for s in session.dol.text_sections() {
        for (k, w) in session.dol.section_bytes(s).chunks_exact(4).enumerate() {
            let a = s.addr + 4 * k as u32;
            let ins = Ins::new(u32::from_be_bytes(w.try_into().unwrap()));
            let hit = match (op, callers) {
                (Some(op), _) => format!("{:?}", ins.op).eq_ignore_ascii_case(op),
                (None, Some(t)) => {
                    ins.is_direct_branch()
                        && ins.branch_dest(a) == Some(t)
                        && (ins.op == ppc750cl::Opcode::B)
                }
                (None, None) => return Err(Fail::Usage("find needs --op or --callers".into())),
            };
            if hit {
                let i = analysis.functions.partition_point(|f| f.start <= a);
                let func = i
                    .checked_sub(1)
                    .map(|i| &analysis.functions[i])
                    .filter(|f| a < f.end);
                let p = ins.simplified();
                let args: Vec<String> = p.args_iter().map(|x| x.to_string()).collect();
                println!(
                    "{a:08x}  {} {}  in {}",
                    p.mnemonic,
                    args.join(", "),
                    func.map_or("-".to_string(), |f| format!("{:08x}", f.start))
                );
            }
        }
    }
    Ok(())
}

/// Where an address is referenced as data (function pointers, vtables) or built with `lis` + `addi`/`ori`.
pub fn pointers(opts: &Options, target: u32) -> Result<(), Fail> {
    let session = Session::open(opts)?;
    for s in session.dol.data_sections() {
        for (k, w) in session.dol.section_bytes(s).chunks_exact(4).enumerate() {
            if u32::from_be_bytes(w.try_into().unwrap()) == target {
                println!("data  {:08x}", s.addr + 4 * k as u32);
            }
        }
    }
    let (hi, lo) = (target >> 16, target & 0xFFFF);
    for s in session.dol.text_sections() {
        let words: Vec<u32> = session
            .dol
            .section_bytes(s)
            .chunks_exact(4)
            .map(|w| u32::from_be_bytes(w.try_into().unwrap()))
            .collect();
        for (i, &w) in words.iter().enumerate() {
            // lis rD, hi
            if w >> 26 != 15 || (w >> 16) & 31 != 0 || w & 0xFFFF != hi {
                // addis with the sign-adjusted high half when the low half is negative as an addi
                if !(w >> 26 == 15
                    && (w >> 16) & 31 == 0
                    && w & 0xFFFF == (target.wrapping_add(0x8000) >> 16))
                {
                    continue;
                }
            }
            let rd = (w >> 21) & 31;
            for &n in words.iter().skip(i + 1).take(6) {
                let is_ori = n >> 26 == 24 && (n >> 16) & 31 == rd && n & 0xFFFF == lo;
                let is_addi = n >> 26 == 14
                    && (n >> 16) & 31 == rd
                    && (n & 0xFFFF) as i16 as i32 == (lo as i16) as i32
                    && w & 0xFFFF == (target.wrapping_add(0x8000) >> 16);
                if is_ori || is_addi {
                    println!("code  {:08x}", s.addr + 4 * i as u32);
                }
            }
        }
    }
    Ok(())
}

/// Every file on the disc: disc offset, size and path.
pub fn ls(opts: &Options) -> Result<(), Fail> {
    let session = Session::open(opts)?;
    for f in session.disc.files() {
        println!("{:#011x} {:>10} {}", f.offset, f.size, f.path);
    }
    Ok(())
}
