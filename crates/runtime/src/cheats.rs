//! Turns the cheat list the player edits in the overlay (`fepor_core::cheats::CheatStore`, ADR 0026) into patch
//! sets of the patch engine (ADR 0025). Runs on the guest thread from `intr::poll`, just before the engine's
//! frame hook, and does nothing until the store's generation moves.

use std::collections::HashMap;
use std::sync::Arc;

use fepor_core::cheats::CheatStore;

use crate::patch::{Patch, PatchEngine, PatchError, PatchSet, SetId};

#[derive(Default)]
pub struct CheatRunner {
    store: Option<Arc<CheatStore>>,
    seen: u64,
    /// Enabled sets by cheat key, with the patches they were built from.
    sets: HashMap<String, (SetId, Vec<Patch>, bool)>,
}

fn describe(e: &PatchError) -> String {
    match e {
        PatchError::Conflict(c) => {
            let c = &c[0];
            format!(
                "overlaps {} at {:#010x}: turn that off first",
                c.with_source,
                0x8000_0000 | c.addr
            )
        }
        other => other.to_string(),
    }
}

impl CheatRunner {
    pub fn attach(&mut self, store: Arc<CheatStore>) {
        self.store = Some(store);
    }

    /// Bring the engine in line with the store: revert the codes turned off, apply those turned on. A cheat is a
    /// continuous set (a code holds its value, the game's own writes are overwritten every frame) unless a native
    /// cheat asks for one write.
    pub fn sync(&mut self, patches: &mut PatchEngine, ram: &mut [u8]) {
        let Some(store) = &self.store else { return };
        let generation = store.generation();
        if generation == self.seen {
            return;
        }
        self.seen = generation;
        let Some(game) = store.game() else { return };
        let wanted: Vec<_> = store
            .wanted()
            .into_iter()
            .map(|w| {
                let p: Vec<Patch> = w.writes.into_iter().map(|w| Patch::new(w.addr, w.bytes).when(w.when)).collect();
                (w.key, w.name, p, w.continuous)
            })
            .collect();
        // revert first: a code turned off may free the bytes of one turned on
        self.sets.retain(|key, (id, old, continuous)| {
            let keep = wanted.iter().any(|(k, _, p, c)| k == key && p == old && c == continuous);
            if !keep {
                let _ = patches.unregister(*id, ram);
            }
            keep
        });
        for (key, name, p, continuous) in wanted {
            if self.sets.contains_key(&key) {
                continue;
            }
            let set = PatchSet {
                source: format!("cheat:{name}"),
                game_id: game.clone(),
                patches: p.clone(),
                continuous,
            };
            let result = patches.register(set).and_then(|id| {
                patches.enable(id, ram).inspect_err(|_| {
                    let _ = patches.unregister(id, ram);
                })?;
                Ok(id)
            });
            match result {
                Ok(id) => {
                    store.set_status(&key, None);
                    self.sets.insert(key, (id, p, continuous));
                }
                Err(e) => store.set_status(&key, Some(describe(&e))),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::RAM_SIZE;

    fn setup(
        ini: &str,
    ) -> (
        tempfile::TempDir,
        Arc<CheatStore>,
        PatchEngine,
        Vec<u8>,
        CheatRunner,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("c.ini");
        std::fs::write(&file, ini).unwrap();
        let store = Arc::new(CheatStore::new(dir.path().join("data")));
        store.set_game("GFEP01", vec![]);
        store.import(&file).unwrap();
        let mut engine = PatchEngine::default();
        engine.set_game("GFEP01").unwrap();
        let mut runner = CheatRunner::default();
        runner.attach(store.clone());
        (dir, store, engine, vec![0; RAM_SIZE], runner)
    }

    #[test]
    fn a_code_is_applied_held_and_reverted_on_toggle() {
        let (_d, store, mut engine, mut ram, mut runner) =
            setup("[Gecko]\n$Money\n04003000 0001869F\n[Gecko_Enabled]\n$Money\n");
        ram[0x3000..0x3004].copy_from_slice(&[1, 2, 3, 4]);
        runner.sync(&mut engine, &mut ram);
        assert_eq!(ram[0x3000..0x3004], [0, 1, 0x86, 0x9F]);
        // the game writes its own value, the next frame holds the cheat's value again
        ram[0x3000..0x3004].copy_from_slice(&[9, 9, 9, 9]);
        engine.frame(1, &mut ram);
        assert_eq!(ram[0x3000..0x3004], [0, 1, 0x86, 0x9F]);
        store.set_enabled("gecko:Money", false);
        runner.sync(&mut engine, &mut ram);
        assert_eq!(ram[0x3000..0x3004], [1, 2, 3, 4]);
        store.set_enabled("gecko:Money", true);
        runner.sync(&mut engine, &mut ram);
        assert_eq!(ram[0x3000..0x3004], [0, 1, 0x86, 0x9F]);
    }

    #[test]
    fn a_code_overlapping_another_source_is_refused_and_reported() {
        let (_d, store, mut engine, mut ram, mut runner) =
            setup("[Gecko]\n$Money\n04003000 0001869F\n[Gecko_Enabled]\n$Money\n");
        store.set_enabled("gecko:Money", false);
        runner.sync(&mut engine, &mut ram);
        let id = engine
            .register(PatchSet {
                source: "mod:other".into(),
                game_id: "GFEP01".into(),
                patches: vec![Patch::word(0x8000_3000, 5)],
                continuous: false,
            })
            .unwrap();
        engine.enable(id, &mut ram).unwrap();
        store.set_enabled("gecko:Money", true);
        runner.sync(&mut engine, &mut ram);
        assert_eq!(ram[0x3003], 5);
        let status = store.list().into_iter().find(|c| c.key == "gecko:Money").unwrap().status.unwrap();
        assert!(status.contains("mod:other"), "{status}");
    }

    #[test]
    fn a_native_cheat_writes_once_or_holds_and_honours_its_condition() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        std::fs::create_dir_all(data.join("cheats")).unwrap();
        std::fs::write(
            data.join("cheats/GFEP01.toml"),
            "[[cheat]]\nname = \"Once\"\n[[cheat.write]]\naddress = 0x80003000\nsize = 1\nvalue = 7\n\
             [[cheat]]\nname = \"Held if flag\"\ncontinuous = true\n[[cheat.write]]\naddress = 0x80003010\nsize = 2\nvalue = 99\n\
             [cheat.write.when]\naddress = 0x80003020\nsize = 1\nop = \"ne\"\nvalue = 0\n",
        )
        .unwrap();
        let store = Arc::new(CheatStore::new(data));
        store.set_game("GFEP01", vec![]);
        let mut engine = PatchEngine::default();
        engine.set_game("GFEP01").unwrap();
        let mut runner = CheatRunner::default();
        runner.attach(store.clone());
        let mut ram = vec![0u8; RAM_SIZE];
        store.set_enabled("native:Once", true);
        store.set_enabled("native:Held if flag", true);
        runner.sync(&mut engine, &mut ram);
        // once: written when turned on, the game's later write stays; the condition is false: nothing yet
        assert_eq!(ram[0x3000], 7);
        assert_eq!(ram[0x3010..0x3012], [0, 0]);
        ram[0x3000] = 1;
        engine.frame(1, &mut ram);
        assert_eq!(ram[0x3000], 1);
        assert_eq!(ram[0x3010..0x3012], [0, 0]);
        // the flag turns on: the next frame holds the value, even after the game overwrites it
        ram[0x3020] = 1;
        engine.frame(2, &mut ram);
        assert_eq!(ram[0x3010..0x3012], [0, 99]);
        ram[0x3011] = 5;
        engine.frame(3, &mut ram);
        assert_eq!(ram[0x3011], 99);
        ram[0x3020] = 0;
        ram[0x3011] = 5;
        engine.frame(4, &mut ram);
        assert_eq!(ram[0x3011], 5);
        // the choice is saved with the Gecko ones
        let saved = std::fs::read_to_string(dir.path().join("data/cheats.toml")).unwrap();
        assert!(saved.contains("native:Once") && saved.contains("native:Held if flag"), "{saved}");
    }
}
