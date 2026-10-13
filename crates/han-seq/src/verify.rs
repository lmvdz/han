//! A minimal check of a received chain: what a client does with the records it read before it trusts them.
//! It checks ids, the hash chain, the home's signatures under its published keys, and that receipt times do
//! not decrease. Replaying Policy over the records (R-14) is the verifier's job (M4).

use han_core::{LogRecord, PublicKey};

use crate::identity::ServerKey;

/// Checks that `records` (a contiguous run of one space, in order) are signed by the home under `keys`
/// and linked by `prev`. `first_prev` is the receipt hash before the first record (`""` from seq 1).
pub fn check_chain(
    records: &[LogRecord],
    keys: &[ServerKey],
    first_prev: &str,
) -> Result<(), String> {
    let mut prev = first_prev.to_owned();
    let mut last_seq = None;
    let mut last_time = String::new();
    for rec in records {
        let (e, r) = (&rec.entry, &rec.receipt);
        let at = |what: &str| format!("seq {}: {what}", r.seq);
        if e.id().map_err(|e| e.to_string())? != r.id || e.space != r.space {
            return Err(at("receipt is not for this entry"));
        }
        if let Some(l) = last_seq
            && r.seq != l + 1
        {
            return Err(at("seq gap"));
        }
        if r.prev != prev {
            return Err(at("prev does not link"));
        }
        if r.time < last_time {
            return Err(at("receipt time decreased"));
        }
        let key = keys
            .iter()
            .find(|k| k.key_id == r.home_key && k.covers(&r.time))
            .and_then(|k| PublicKey::from_base64url(&k.public_key).ok())
            .ok_or_else(|| at("no published key covers the receipt"))?;
        r.verify(&key).map_err(|_| at("homeSig does not verify"))?;
        prev = r.hash().map_err(|e| e.to_string())?;
        last_seq = Some(r.seq);
        last_time.clone_from(&r.time);
    }
    Ok(())
}
