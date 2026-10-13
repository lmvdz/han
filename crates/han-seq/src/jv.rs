//! Small helpers for building and reading `Value`s.

use std::collections::BTreeMap;

use han_core::json::Value;

/// An object from pairs.
pub(crate) fn obj<const N: usize>(pairs: [(&str, Value); N]) -> Value {
    Value::Object(
        pairs
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v))
            .collect::<BTreeMap<_, _>>(),
    )
}

/// A string value.
pub(crate) fn s(text: &str) -> Value {
    Value::Str(text.to_owned())
}

/// An integer value from an unsigned number (saturating at the safe range).
pub(crate) fn int(n: u64) -> Value {
    Value::Int(i64::try_from(n.min(han_core::json::MAX_SAFE_INT as u64)).unwrap_or(i64::MAX))
}

/// The wire form of `v`: JCS, which is compact I-JSON.
pub(crate) fn text(v: &Value) -> String {
    // Canonicalizing can fail only for a float, which no value of ours holds.
    han_core::json::canonicalize(v)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .unwrap_or_default()
}
