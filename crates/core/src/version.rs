//! Supported game versions. Other regions are added as new table rows (ADR 0003).

use sha1::{Digest, Sha1};

#[derive(Debug)]
pub struct Version {
    pub game_id: &'static str,
    pub name: &'static str,
    /// SHA-1 of `main.dol`, lowercase hex.
    pub dol_sha1: &'static str,
}

pub const VERSIONS: &[Version] = &[Version {
    game_id: "GFEP01",
    name: "Fire Emblem: Path of Radiance (EU)",
    dol_sha1: "1cf18d42d52c745d533715895c3a6649802324e9",
}];

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum VersionError {
    #[error("unsupported game ID {0:?} (supported: GFEP01)")]
    Unsupported(String),
    #[error("main.dol hash mismatch for {game_id}: expected {expected}, found {found}")]
    HashMismatch { game_id: String, expected: &'static str, found: String },
}

pub fn sha1_hex(data: &[u8]) -> String {
    Sha1::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

/// Check the disc's game ID and `main.dol` hash against the version table.
pub fn check(game_id: &str, dol: &[u8]) -> Result<&'static Version, VersionError> {
    let v = VERSIONS
        .iter()
        .find(|v| v.game_id == game_id)
        .ok_or_else(|| VersionError::Unsupported(game_id.into()))?;
    let found = sha1_hex(dol);
    if found != v.dol_sha1 {
        return Err(VersionError::HashMismatch { game_id: game_id.into(), expected: v.dol_sha1, found });
    }
    Ok(v)
}
