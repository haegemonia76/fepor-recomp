//! Mods (ADR 0034): folders `<data>/mods/<id>/` with a `mod.toml`, replacement files under `files/` and
//! memory patches. This module reads them, keeps their order and enabled state (`<data>/mods/mods.toml`),
//! and finds the conflicts between them. Applying them is the game's job (`ModdedDisc`, the patch engine).
//!
//! A mod never contains Nintendo content (ADR 0003): it carries only what its author made (changed or new
//! data for files the player's own disc has).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// `<data>/mods`.
pub fn mods_dir(data: &Path) -> PathBuf {
    data.join("mods")
}

/// `mod.toml`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    pub name: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub version: String,
    /// The game the mod is for (`GFEP01`); the mod does nothing for another one.
    pub game_id: String,
    #[serde(default)]
    pub description: String,
    /// Patches are written again every frame instead of once at start.
    #[serde(default)]
    pub continuous: bool,
    #[serde(default, rename = "patch")]
    pub patches: Vec<PatchSpec>,
}

/// The text of a `mod.toml` with only the required and descriptive fields (tools that generate mods).
pub fn manifest_text(name: &str, game_id: &str, description: &str) -> String {
    let mut t = toml::Table::new();
    t.insert("name".into(), name.into());
    t.insert("game_id".into(), game_id.into());
    t.insert("description".into(), description.into());
    toml::to_string(&t).expect("strings serialise")
}

/// `[[patch]]`: `address = 0x80410000`, `bytes = "00 00 00 01"` (hex, spaces optional).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct PatchSpec {
    pub address: u32,
    #[serde(deserialize_with = "hex_bytes")]
    pub bytes: Vec<u8>,
}

fn hex_bytes<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
    let text = String::deserialize(d)?;
    let digits: Vec<u8> = text.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if digits.is_empty() || !digits.len().is_multiple_of(2) {
        return Err(serde::de::Error::custom(
            "bytes: an even number of hex digits",
        ));
    }
    digits
        .chunks(2)
        .map(|p| {
            std::str::from_utf8(p)
                .ok()
                .and_then(|s| u8::from_str_radix(s, 16).ok())
                .ok_or_else(|| serde::de::Error::custom("bytes: not hexadecimal"))
        })
        .collect()
}

impl PatchSpec {
    /// The bytes it writes, as physical addresses (cached, uncached and physical forms are the same bytes).
    pub fn range(&self) -> std::ops::Range<u64> {
        let start = (self.address & 0x3FFF_FFFF) as u64;
        start..start + self.bytes.len() as u64
    }
}

/// One mod folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mod {
    /// The folder's name: the mod's identity in `mods.toml`.
    pub id: String,
    pub manifest: Manifest,
    /// Files under `files/`, `/`-separated paths relative to it (the disc paths they replace).
    pub files: Vec<String>,
}

impl Mod {
    fn load(dir: &Path) -> Result<Mod, String> {
        let id = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let text = std::fs::read_to_string(dir.join("mod.toml")).map_err(|e| e.to_string())?;
        let manifest: Manifest = toml::from_str(&text).map_err(|e| e.message().to_string())?;
        let mut files = Vec::new();
        collect(&dir.join("files"), "", &mut files);
        files.sort();
        Ok(Mod {
            id,
            manifest,
            files,
        })
    }

    /// Host path of a replacement file.
    fn host_file(&self, root: &Path, path: &str) -> PathBuf {
        root.join(&self.id).join("files").join(path)
    }
}

fn collect(dir: &Path, prefix: &str, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        match e.file_type() {
            Ok(t) if t.is_dir() => collect(&e.path(), &format!("{prefix}{name}/"), out),
            Ok(t) if t.is_file() => out.push(format!("{prefix}{name}")),
            _ => {}
        }
    }
}

/// `mods.toml`: the priority order (first wins) and which mods are on.
#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    #[serde(default)]
    order: Vec<String>,
    #[serde(default)]
    enabled: Vec<String>,
}

/// A mod in the list with its state.
#[derive(Debug, Clone)]
pub struct Entry {
    pub m: Mod,
    pub enabled: bool,
}

/// Two enabled mods that change the same thing; `winner` has the higher priority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub winner: String,
    pub loser: String,
    pub what: ConflictKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConflictKind {
    /// The same disc file: the winner's is used.
    File(String),
    /// The same guest bytes (physical address, length of the overlap): the loser's patches are not applied.
    Bytes(u32, u64),
}

impl std::fmt::Display for Conflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.what {
            ConflictKind::File(p) => write!(
                f,
                "{} and {} both replace {p}: {} is used",
                self.winner, self.loser, self.winner
            ),
            ConflictKind::Bytes(a, n) => write!(
                f,
                "{} and {} both patch {n} byte(s) at {a:#010x}: {}'s patches are not applied",
                self.winner, self.loser, self.loser
            ),
        }
    }
}

/// The mods of a data folder in priority order.
#[derive(Debug, Default)]
pub struct ModList {
    root: PathBuf,
    pub entries: Vec<Entry>,
    /// Folders with a `mod.toml` that could not be read: `(folder, reason)`.
    pub broken: Vec<(String, String)>,
}

impl ModList {
    /// Read `<data>/mods`. A missing folder is an empty list.
    pub fn load(data: &Path) -> ModList {
        let root = mods_dir(data);
        let state: State = std::fs::read_to_string(root.join("mods.toml"))
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default();
        let (mut found, mut broken) = (Vec::new(), Vec::new());
        if let Ok(dirs) = std::fs::read_dir(&root) {
            for d in dirs
                .flatten()
                .filter(|d| d.path().join("mod.toml").is_file())
            {
                match Mod::load(&d.path()) {
                    Ok(m) => found.push(m),
                    Err(e) => broken.push((d.file_name().to_string_lossy().into_owned(), e)),
                }
            }
        }
        found.sort_by(|a, b| a.id.cmp(&b.id));
        broken.sort();
        // the saved order first, then the mods it does not know, by name
        let rank = |id: &str| {
            state
                .order
                .iter()
                .position(|o| o == id)
                .unwrap_or(usize::MAX)
        };
        found.sort_by_key(|m| rank(&m.id));
        let entries = found
            .into_iter()
            .map(|m| Entry {
                enabled: state.enabled.contains(&m.id),
                m,
            })
            .collect();
        ModList {
            root,
            entries,
            broken,
        }
    }

    /// Remember the order and the enabled mods in `mods.toml`.
    pub fn save(&self) -> std::io::Result<()> {
        let state = State {
            order: self.entries.iter().map(|e| e.m.id.clone()).collect(),
            enabled: self
                .entries
                .iter()
                .filter(|e| e.enabled)
                .map(|e| e.m.id.clone())
                .collect(),
        };
        std::fs::create_dir_all(&self.root)?;
        let text = toml::to_string(&state).map_err(std::io::Error::other)?;
        std::fs::write(self.root.join("mods.toml"), text)
    }

    /// Move the mod at `i` one place toward the front (`up`) or the back; true when it moved.
    pub fn shift(&mut self, i: usize, up: bool) -> bool {
        let j = if up { i.checked_sub(1) } else { Some(i + 1) };
        match j.filter(|&j| i < self.entries.len() && j < self.entries.len()) {
            Some(j) => {
                self.entries.swap(i, j);
                true
            }
            None => false,
        }
    }

    fn active<'a>(&'a self, game_id: Option<&'a str>) -> impl Iterator<Item = &'a Entry> {
        self.entries
            .iter()
            .filter(move |e| e.enabled && game_id.is_none_or(|g| e.m.manifest.game_id == g))
    }

    /// Conflicts between the enabled mods (of `game_id` when given), in priority order.
    pub fn conflicts(&self, game_id: Option<&str>) -> Vec<Conflict> {
        let active: Vec<&Mod> = self.active(game_id).map(|e| &e.m).collect();
        let mut out = Vec::new();
        for (i, a) in active.iter().enumerate() {
            for b in &active[i + 1..] {
                for f in a.files.iter().filter(|f| b.files.contains(f)) {
                    out.push(Conflict {
                        winner: a.id.clone(),
                        loser: b.id.clone(),
                        what: ConflictKind::File(f.clone()),
                    });
                }
                for (pa, pb) in a
                    .manifest
                    .patches
                    .iter()
                    .flat_map(|p| b.manifest.patches.iter().map(move |q| (p, q)))
                {
                    let (ra, rb) = (pa.range(), pb.range());
                    let (lo, hi) = (ra.start.max(rb.start), ra.end.min(rb.end));
                    if lo < hi {
                        out.push(Conflict {
                            winner: a.id.clone(),
                            loser: b.id.clone(),
                            what: ConflictKind::Bytes(lo as u32, hi - lo),
                        });
                    }
                }
            }
        }
        out
    }

    /// The files to serve from the host: `(disc path, host file)`, one per path, the highest priority wins.
    pub fn replacements(&self, game_id: &str) -> Vec<(String, PathBuf)> {
        let mut out: Vec<(String, PathBuf)> = Vec::new();
        for e in self.active(Some(game_id)) {
            for f in &e.m.files {
                if !out.iter().any(|(p, _)| p == f) {
                    out.push((f.clone(), e.m.host_file(&self.root, f)));
                }
            }
        }
        out
    }

    /// The enabled mods of `game_id` that have patches, in priority order.
    pub fn patch_mods<'a>(&'a self, game_id: &'a str) -> Vec<&'a Mod> {
        self.active(Some(game_id))
            .map(|e| &e.m)
            .filter(|m| !m.manifest.patches.is_empty())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_mod(data: &Path, id: &str, toml: &str, files: &[&str]) {
        let dir = mods_dir(data).join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("mod.toml"), toml).unwrap();
        for f in files {
            let p = dir.join("files").join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, b"x").unwrap();
        }
    }

    fn manifest(name: &str, patches: &str) -> String {
        format!("name = \"{name}\"\ngame_id = \"GFEP01\"\n{patches}")
    }

    fn enable_all(list: &mut ModList) {
        for e in &mut list.entries {
            e.enabled = true;
        }
    }

    #[test]
    fn reads_manifest_files_and_patches() {
        let d = tempfile::tempdir().unwrap();
        write_mod(
            d.path(),
            "a",
            "name = \"A\"\nauthor = \"me\"\nversion = \"1\"\ngame_id = \"GFEP01\"\ndescription = \"d\"\n[[patch]]\naddress = 0x80410000\nbytes = \"00 00 00 01\"\n",
            &["a.bin", "dir/b.bin"],
        );
        let list = ModList::load(d.path());
        let m = &list.entries[0].m;
        assert_eq!(m.id, "a");
        assert_eq!(m.manifest.author, "me");
        assert_eq!(m.files, ["a.bin", "dir/b.bin"]);
        assert_eq!(m.manifest.patches[0].address, 0x8041_0000);
        assert_eq!(m.manifest.patches[0].bytes, [0, 0, 0, 1]);
        assert!(!list.entries[0].enabled);
    }

    #[test]
    fn a_broken_manifest_is_listed_not_fatal() {
        let d = tempfile::tempdir().unwrap();
        write_mod(d.path(), "bad", "name = 3", &[]);
        write_mod(
            d.path(),
            "odd",
            &manifest("O", "[[patch]]\naddress = 0\nbytes = \"abc\"\n"),
            &[],
        );
        write_mod(d.path(), "ok", &manifest("Ok", ""), &[]);
        let list = ModList::load(d.path());
        assert_eq!(list.entries.len(), 1);
        assert_eq!(list.broken.len(), 2);
    }

    #[test]
    fn order_and_enabled_survive_a_save() {
        let d = tempfile::tempdir().unwrap();
        for id in ["a", "b", "c"] {
            write_mod(d.path(), id, &manifest(id, ""), &[]);
        }
        let mut list = ModList::load(d.path());
        assert_eq!(
            list.entries
                .iter()
                .map(|e| e.m.id.as_str())
                .collect::<Vec<_>>(),
            ["a", "b", "c"]
        );
        assert!(list.shift(2, true));
        assert!(!list.shift(0, true) && !list.shift(2, false));
        list.entries[0].enabled = true;
        list.save().unwrap();
        // a mod added later goes to the back, off
        write_mod(d.path(), "0new", &manifest("n", ""), &[]);
        let list = ModList::load(d.path());
        let ids: Vec<_> = list
            .entries
            .iter()
            .map(|e| (e.m.id.as_str(), e.enabled))
            .collect();
        assert_eq!(
            ids,
            [("a", true), ("c", false), ("b", false), ("0new", false)]
        );
    }

    #[test]
    fn file_conflicts_and_priority() {
        let d = tempfile::tempdir().unwrap();
        write_mod(d.path(), "a", &manifest("A", ""), &["x.bin", "only_a.bin"]);
        write_mod(d.path(), "b", &manifest("B", ""), &["x.bin"]);
        let mut list = ModList::load(d.path());
        assert!(
            list.conflicts(None).is_empty(),
            "disabled mods do not conflict"
        );
        enable_all(&mut list);
        let c = list.conflicts(None);
        assert_eq!(c.len(), 1);
        assert_eq!((c[0].winner.as_str(), c[0].loser.as_str()), ("a", "b"));
        assert_eq!(c[0].what, ConflictKind::File("x.bin".into()));
        let r = list.replacements("GFEP01");
        assert_eq!(r.len(), 2);
        assert!(r
            .iter()
            .any(|(p, h)| p == "x.bin" && h.ends_with("a/files/x.bin")));
        list.shift(0, false); // b first now
        let r = list.replacements("GFEP01");
        assert!(r
            .iter()
            .any(|(p, h)| p == "x.bin" && h.ends_with("b/files/x.bin")));
        assert!(list.replacements("GFEJ01").is_empty());
    }

    #[test]
    fn patch_conflicts_compare_physical_bytes() {
        let d = tempfile::tempdir().unwrap();
        write_mod(
            d.path(),
            "a",
            &manifest(
                "A",
                "[[patch]]\naddress = 0x80410000\nbytes = \"00000000 00000000\"\n",
            ),
            &[],
        );
        // the uncached mirror, overlapping the last 4 bytes of a's patch
        write_mod(
            d.path(),
            "b",
            &manifest(
                "B",
                "[[patch]]\naddress = 0xC0410004\nbytes = \"11111111 22222222\"\n",
            ),
            &[],
        );
        write_mod(
            d.path(),
            "c",
            &manifest("C", "[[patch]]\naddress = 0x80410010\nbytes = \"33\"\n"),
            &[],
        );
        let mut list = ModList::load(d.path());
        enable_all(&mut list);
        let c = list.conflicts(Some("GFEP01"));
        assert_eq!(c.len(), 1, "{c:?}");
        assert_eq!(c[0].what, ConflictKind::Bytes(0x410004, 4));
        assert_eq!((c[0].winner.as_str(), c[0].loser.as_str()), ("a", "b"));
        assert_eq!(list.patch_mods("GFEP01").len(), 3);
    }
}
