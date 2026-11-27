//! The guest-thread side of the memory watcher (ADR 0048). The overlay never reads or writes guest RAM: it
//! sends [`Request`]s through the [`Link`], and the per-frame hook of the patch engine serves them here, between
//! guest instructions, then publishes a copy of what the page shows.

use std::sync::Arc;

use fepor_core::memwatch::{Held, Link, Published, Request, ScanInfo, Scanner, Width};

use crate::machine::RAM_SIZE;
use crate::patch::{Patch, PatchEngine, PatchSet, SetId};

/// Candidates published to the page.
const HITS: usize = 200;
/// Bytes of the hex view at most.
const VIEW_MAX: u32 = 512;
/// Owner of the sets that hold values (conflicts with other sources are reported like any other).
const SOURCE: &str = "watch";

pub struct Watcher {
    link: Arc<Link>,
    scan: Option<Scanner>,
    view: (u32, u32),
    watches: Vec<(u32, Width)>,
    held: Vec<(Held, SetId)>,
    message: String,
    served: u64,
}

fn in_ram(addr: u32, width: Width) -> Option<usize> {
    let p = (addr & 0x3FFF_FFFF) as usize;
    (p + width.bytes() <= RAM_SIZE).then_some(p)
}

impl Watcher {
    pub fn new(link: Arc<Link>) -> Watcher {
        Watcher {
            link,
            scan: None,
            view: (0x8000_0000, 64),
            watches: Vec::new(),
            held: Vec::new(),
            message: String::new(),
            served: 0,
        }
    }

    /// Serve the requests that came in and publish when the page is shown (or something was asked).
    pub fn service(&mut self, engine: &mut PatchEngine, ram: &mut [u8]) {
        let requests = self.link.take_requests();
        let asked = !requests.is_empty();
        for r in requests {
            self.serve(r, engine, ram);
        }
        if asked || self.link.is_active() {
            self.served += 1;
            self.publish(engine, ram);
        }
    }

    fn serve(&mut self, r: Request, engine: &mut PatchEngine, ram: &mut [u8]) {
        let main = &ram[..RAM_SIZE];
        match r {
            Request::NewScan { width, equal } => {
                let s = Scanner::start(main, width, equal);
                self.message = format!("scan started: {} candidates", s.count());
                self.scan = Some(s);
            }
            Request::Narrow(f) => match &mut self.scan {
                Some(s) => {
                    s.narrow(main, f);
                    self.message = format!("{} candidates left", s.count());
                }
                None => self.message = "start a scan first".into(),
            },
            Request::ClearScan => {
                self.scan = None;
                self.message.clear();
            }
            Request::Poke {
                addr,
                width,
                value,
                hold,
            } => self.poke(addr, width, value, hold, engine, ram),
            Request::Release(addr) => {
                self.release(addr, engine, ram);
            }
            Request::View { addr, len } => self.view = (addr, len.min(VIEW_MAX)),
            Request::Watch(w) => self.watches = w,
        }
    }

    fn release(&mut self, addr: u32, engine: &mut PatchEngine, ram: &mut [u8]) {
        let phys = addr & 0x3FFF_FFFF;
        while let Some(i) = self
            .held
            .iter()
            .position(|(h, _)| h.addr & 0x3FFF_FFFF == phys)
        {
            let (_, id) = self.held.remove(i);
            let _ = engine.unregister(id, ram);
        }
    }

    fn poke(
        &mut self,
        addr: u32,
        width: Width,
        value: u32,
        hold: bool,
        engine: &mut PatchEngine,
        ram: &mut [u8],
    ) {
        let Some(p) = in_ram(addr, width) else {
            self.message = format!("{addr:#010x} is not in main RAM");
            return;
        };
        self.release(addr, engine, ram);
        let bytes = width.bytes_of(value);
        if !hold {
            ram[p..p + bytes.len()].copy_from_slice(&bytes);
            self.message = format!("wrote {value:#x} at {addr:#010x}");
            return;
        }
        let Some(game) = engine.game().map(str::to_owned) else {
            self.message = "no game selected: nothing can be held".into();
            return;
        };
        let set = PatchSet {
            source: SOURCE.into(),
            game_id: game,
            patches: vec![Patch::new(addr, bytes)],
            continuous: true,
        };
        let result = engine.register(set).and_then(|id| {
            engine.enable(id, ram).inspect_err(|_| {
                let _ = engine.unregister(id, ram);
            })?;
            Ok(id)
        });
        match result {
            Ok(id) => {
                self.held.push((Held { addr, width, value }, id));
                self.message = format!("holding {value:#x} at {addr:#010x}");
            }
            Err(e) => self.message = format!("cannot hold {addr:#010x}: {e}"),
        }
    }

    fn publish(&self, engine: &PatchEngine, ram: &[u8]) {
        let main = &ram[..RAM_SIZE];
        let read = |addr: u32, width: Width| in_ram(addr, width).map(|p| width.read(&main[p..]));
        let (va, vl) = self.view;
        let vp = (va & 0x3FFF_FFFF) as usize;
        let view = main
            .get(vp..(vp + vl as usize).min(RAM_SIZE))
            .unwrap_or_default()
            .to_vec();
        self.link.publish(Published {
            frame: self.served,
            game: engine.game().map(str::to_owned),
            scan: self.scan.as_ref().map(|s| ScanInfo {
                width: Some(s.width()),
                count: s.count(),
                hits: s.hits(main, HITS),
            }),
            view_addr: va,
            view,
            watches: self
                .watches
                .iter()
                .map(|&(a, w)| (a, w, read(a, w)))
                .collect(),
            held: self.held.iter().map(|(h, _)| h.clone()).collect(),
            message: self.message.clone(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fepor_core::memwatch::{Filter, Width};

    fn setup() -> (Watcher, PatchEngine, Vec<u8>, Arc<Link>) {
        let link = Arc::new(Link::default());
        let mut e = PatchEngine::new();
        e.set_game("GFEP01").unwrap();
        (Watcher::new(link.clone()), e, vec![0; RAM_SIZE + 64], link)
    }

    #[test]
    fn scan_through_the_link_finds_a_counter() {
        let (mut w, mut e, mut ram, link) = setup();
        link.send(Request::NewScan {
            width: Width::U32,
            equal: None,
        });
        w.service(&mut e, &mut ram);
        for n in 1..=3u32 {
            ram[0x1000..0x1004].copy_from_slice(&n.to_be_bytes()); // counter
            ram[0x2000..0x2004].copy_from_slice(&(n % 2).to_be_bytes()); // flips
            link.send(Request::Narrow(Filter::Changed));
            w.service(&mut e, &mut ram);
        }
        link.send(Request::Narrow(Filter::Increased));
        ram[0x1000..0x1004].copy_from_slice(&9u32.to_be_bytes());
        w.service(&mut e, &mut ram);
        let p = link.published();
        let scan = p.scan.unwrap();
        assert_eq!(scan.count, 1);
        assert_eq!(scan.hits[0].addr, 0x8000_1000);
        assert_eq!(scan.hits[0].now, 9);
    }

    #[test]
    fn idle_link_publishes_nothing_until_the_page_is_active() {
        let (mut w, mut e, mut ram, link) = setup();
        w.service(&mut e, &mut ram);
        assert_eq!(link.published().frame, 0);
        link.set_active(true);
        w.service(&mut e, &mut ram);
        assert_eq!(link.published().frame, 1);
    }

    #[test]
    fn poke_writes_and_hold_goes_through_the_engine_every_frame() {
        let (mut w, mut e, mut ram, link) = setup();
        link.send(Request::Poke {
            addr: 0x8000_0100,
            width: Width::U16,
            value: 0x1234,
            hold: false,
        });
        link.send(Request::Poke {
            addr: 0x8000_0200,
            width: Width::U32,
            value: 99,
            hold: true,
        });
        link.send(Request::Watch(vec![(0x8000_0200, Width::U32)]));
        w.service(&mut e, &mut ram);
        assert_eq!(ram[0x100..0x102], [0x12, 0x34]);
        ram[0x200..0x204].copy_from_slice(&5u32.to_be_bytes()); // the game overwrites
        assert!(e.frame(1, &mut ram));
        assert_eq!(ram[0x200..0x204], 99u32.to_be_bytes());
        assert_eq!(link.published().held.len(), 1);
        assert_eq!(link.published().watches[0].2, Some(99));
        link.send(Request::Release(0x8000_0200));
        w.service(&mut e, &mut ram);
        assert!(link.published().held.is_empty());
        assert_eq!(
            ram[0x200..0x204],
            [0; 4],
            "release puts back what was there at the hold"
        );
    }

    #[test]
    fn out_of_range_and_conflicts_are_reported() {
        let (mut w, mut e, mut ram, link) = setup();
        link.send(Request::Poke {
            addr: 0x8180_0000,
            width: Width::U8,
            value: 1,
            hold: false,
        });
        w.service(&mut e, &mut ram);
        assert!(link.published().message.contains("not in main RAM"));
        let id = e
            .register(PatchSet {
                source: "cheat:x".into(),
                game_id: "GFEP01".into(),
                patches: vec![Patch::word(0x8000_0300, 1)],
                continuous: false,
            })
            .unwrap();
        e.enable(id, &mut ram).unwrap();
        link.send(Request::Poke {
            addr: 0x8000_0300,
            width: Width::U32,
            value: 2,
            hold: true,
        });
        w.service(&mut e, &mut ram);
        assert!(link.published().message.contains("cannot hold"));
        assert!(link.published().held.is_empty());
    }
}
