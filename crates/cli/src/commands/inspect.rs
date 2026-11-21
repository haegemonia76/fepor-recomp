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
        let word = session.dol.read_u32(a).ok_or_else(|| Fail::Usage(format!("{a:#010x} is outside the DOL image")))?;
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
                (None, Some(t)) => ins.is_direct_branch() && ins.branch_dest(a) == Some(t) && (ins.op == ppc750cl::Opcode::B),
                (None, None) => return Err(Fail::Usage("find needs --op or --callers".into())),
            };
            if hit {
                let i = analysis.functions.partition_point(|f| f.start <= a);
                let func = i.checked_sub(1).map(|i| &analysis.functions[i]).filter(|f| a < f.end);
                let p = ins.simplified();
                let args: Vec<String> = p.args_iter().map(|x| x.to_string()).collect();
                println!("{a:08x}  {} {}  in {}", p.mnemonic, args.join(", "), func.map_or("-".to_string(), |f| format!("{:08x}", f.start)));
            }
        }
    }
    Ok(())
}
