//! What the engine needs from its surroundings: durable storage, and the space's view of keys
//! and roles.

use std::collections::HashMap;

/// The format version of stored data (TL-125).
pub const FORMAT_VERSION: u32 = 1;

/// Durable storage of one history. Implementations make each call atomic (TL-122).
pub trait Store {
    /// The format version the stored data carries, or `None` for an empty store.
    fn format_version(&self) -> Result<Option<u32>, String>;
    /// Records the format version.
    fn set_format_version(&mut self, version: u32) -> Result<(), String>;
    /// Every integrated envelope, in the order it was integrated.
    fn envelopes(&self) -> Result<Vec<Vec<u8>>, String>;
    /// Appends an integrated envelope.
    fn append(&mut self, envelope: &[u8]) -> Result<(), String>;
    /// The durable counter high-water mark of a device key (TL-120); 0 if none.
    fn high_water(&self, actor: &str, key: &str) -> Result<u64, String>;
    /// Raises the high-water mark of a device key; must be durable when it returns.
    fn raise_high_water(&mut self, actor: &str, key: &str, counter: u64) -> Result<(), String>;
}

/// A [`Store`] in memory.
#[derive(Clone, Debug, Default)]
pub struct MemoryStore {
    format: Option<u32>,
    envelopes: Vec<Vec<u8>>,
    high_water: HashMap<(String, String), u64>,
}

impl Store for MemoryStore {
    fn format_version(&self) -> Result<Option<u32>, String> {
        Ok(self.format)
    }
    fn set_format_version(&mut self, version: u32) -> Result<(), String> {
        self.format = Some(version);
        Ok(())
    }
    fn envelopes(&self) -> Result<Vec<Vec<u8>>, String> {
        Ok(self.envelopes.clone())
    }
    fn append(&mut self, envelope: &[u8]) -> Result<(), String> {
        self.envelopes.push(envelope.to_vec());
        Ok(())
    }
    fn high_water(&self, actor: &str, key: &str) -> Result<u64, String> {
        Ok(self
            .high_water
            .get(&(actor.to_string(), key.to_string()))
            .copied()
            .unwrap_or(0))
    }
    fn raise_high_water(&mut self, actor: &str, key: &str, counter: u64) -> Result<(), String> {
        let hw = self
            .high_water
            .entry((actor.to_string(), key.to_string()))
            .or_insert(0);
        *hw = (*hw).max(counter);
        Ok(())
    }
}

/// A device key as the actor's profile lists it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyInfo {
    /// The Ed25519 public key.
    pub public_key: [u8; 32],
    /// Whether the key is `valid` (not revoked) in the profile.
    pub valid: bool,
}

/// Who may write a history (TL-103).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope<'a> {
    /// A worktree or data history: members and owners of the space.
    Space,
    /// A transcript history: only the actor of the `session.open` entry with this EntryId.
    Session(&'a str),
}

/// The receiver's latest view of the space and its actors' profiles.
pub trait Directory {
    /// The device key `key` of `actor`, if the actor's profile lists it.
    fn key(&self, actor: &str, key: &str) -> Option<KeyInfo>;
    /// Whether `actor` may write in `scope`: holds `member` or `owner` in the space, or is the
    /// session's actor.
    fn may_write(&self, actor: &str, scope: Scope<'_>) -> bool;
}
