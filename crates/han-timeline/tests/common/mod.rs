//! Helpers shared by the integration tests.
#![allow(dead_code)]

use std::collections::HashMap;

use han_timeline::{Directory, KeyInfo, KeySigner, Scope, Signer as _};
use sha2::{Digest, Sha256};

pub const SPACE: &str = "s1:a.example";

/// splitmix64.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
    pub fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
    pub fn shuffle<T>(&mut self, v: &mut [T]) {
        for i in (1..v.len()).rev() {
            v.swap(i, self.below(i + 1));
        }
    }
    pub fn text(&mut self) -> String {
        const A: &[&str] = &["a", "b", "\n", "é", "🙂", " "];
        (0..1 + self.below(5))
            .map(|_| A[self.below(A.len())])
            .collect()
    }
}

pub fn signer(actor: &str, key: &str) -> KeySigner {
    let seed: [u8; 32] = Sha256::digest(format!("common-{actor}-{key}")).into();
    KeySigner::from_seed(actor, key, seed)
}

/// Every listed key valid; everyone may write.
pub struct Open(pub HashMap<(String, String), KeyInfo>);

impl Open {
    pub fn new(signers: &[&KeySigner]) -> Self {
        Open(
            signers
                .iter()
                .map(|s| {
                    (
                        (s.actor().to_string(), s.key_id().to_string()),
                        KeyInfo {
                            public_key: s.public_key(),
                            valid: true,
                        },
                    )
                })
                .collect(),
        )
    }
}

impl Directory for Open {
    fn key(&self, actor: &str, key: &str) -> Option<KeyInfo> {
        self.0.get(&(actor.to_string(), key.to_string())).copied()
    }
    fn may_write(&self, _: &str, _: Scope<'_>) -> bool {
        true
    }
}
