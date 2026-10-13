//! Values (TL-30): the content of registers and map keys.

use std::collections::BTreeMap;

use crate::cbor::Cbor;
use crate::ids::{MAX_SAFE, OpId, ReplicaId};

/// The encoded size limit of a value (TL-30).
pub const MAX_VALUE_BYTES: usize = 64 * 1024;
/// The nesting limit of a value: arrays and maps inside one another (TL-30).
pub const MAX_VALUE_DEPTH: usize = 16;

/// A value: null, a boolean, a safe integer, a text or byte string, an array, or a map with
/// text keys.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Value {
    /// `null`.
    Null,
    /// A boolean.
    Bool(bool),
    /// An integer in `[-(2^53 - 1), 2^53 - 1]`.
    Int(i64),
    /// A text string.
    Text(String),
    /// A byte string.
    Bytes(Vec<u8>),
    /// An array of values.
    Array(Vec<Value>),
    /// A map from text strings to values.
    Map(BTreeMap<String, Value>),
}

/// Why a decoded item is not a value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueError {
    /// Not of the value subset.
    Malformed,
    /// Over the size or depth limit.
    TooLarge,
}

impl Value {
    /// A text value.
    pub fn text(s: impl Into<String>) -> Value {
        Value::Text(s.into())
    }

    /// The object reference `{"obj": [actor, key, lane, counter]}` naming `id`.
    pub fn object_ref(id: &OpId) -> Value {
        let mut m = BTreeMap::new();
        m.insert("obj".to_string(), op_id_value(id));
        Value::Map(m)
    }

    /// The text, if this is a text value.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Value::Text(s) => Some(s),
            _ => None,
        }
    }

    /// The map, if this is a map value.
    pub fn as_map(&self) -> Option<&BTreeMap<String, Value>> {
        match self {
            Value::Map(m) => Some(m),
            _ => None,
        }
    }

    /// The op id named by the `obj` field of a map value (an object reference or file entry).
    pub fn obj_field(&self) -> Option<OpId> {
        value_op_id(self.as_map()?.get("obj")?)
    }

    /// The CBOR form.
    pub fn to_cbor(&self) -> Cbor {
        match self {
            Value::Null => Cbor::Null,
            Value::Bool(b) => Cbor::Bool(*b),
            Value::Int(i) if *i >= 0 => Cbor::Uint(*i as u64),
            Value::Int(i) => Cbor::Nint((-1 - *i) as u64),
            Value::Text(s) => Cbor::Text(s.clone()),
            Value::Bytes(b) => Cbor::Bytes(b.clone()),
            Value::Array(items) => Cbor::Array(items.iter().map(Value::to_cbor).collect()),
            Value::Map(m) => Cbor::Map(
                m.iter()
                    .map(|(k, v)| (Cbor::Text(k.clone()), v.to_cbor()))
                    .collect(),
            ),
        }
    }

    /// The deterministic encoding; equal values have equal encodings.
    pub fn encode(&self) -> Vec<u8> {
        self.to_cbor().encode()
    }

    /// A value from a decoded item, checking the subset and the limits.
    pub fn from_cbor(item: &Cbor) -> Result<Value, ValueError> {
        let v = convert(item, 0)?;
        if v.encode().len() > MAX_VALUE_BYTES {
            return Err(ValueError::TooLarge);
        }
        Ok(v)
    }

    /// Checks the limits of a value built in memory.
    pub fn check(&self) -> Result<(), ValueError> {
        Value::from_cbor(&self.to_cbor()).map(|_| ())
    }
}

fn convert(item: &Cbor, depth: usize) -> Result<Value, ValueError> {
    Ok(match item {
        Cbor::Null => Value::Null,
        Cbor::Bool(b) => Value::Bool(*b),
        Cbor::Uint(n) if *n <= MAX_SAFE => Value::Int(*n as i64),
        Cbor::Nint(n) if *n < MAX_SAFE => Value::Int(-1 - *n as i64),
        Cbor::Uint(_) | Cbor::Nint(_) => return Err(ValueError::Malformed),
        Cbor::Text(s) => Value::Text(s.clone()),
        Cbor::Bytes(b) => Value::Bytes(b.clone()),
        Cbor::Array(items) => {
            if depth >= MAX_VALUE_DEPTH {
                return Err(ValueError::TooLarge);
            }
            Value::Array(
                items
                    .iter()
                    .map(|i| convert(i, depth + 1))
                    .collect::<Result<_, _>>()?,
            )
        }
        Cbor::Map(entries) => {
            if depth >= MAX_VALUE_DEPTH {
                return Err(ValueError::TooLarge);
            }
            let mut m = BTreeMap::new();
            for (k, v) in entries {
                let Cbor::Text(k) = k else {
                    return Err(ValueError::Malformed);
                };
                m.insert(k.clone(), convert(v, depth + 1)?);
            }
            Value::Map(m)
        }
    })
}

/// `[actor, key, lane, counter]`, the form of an op id inside values and digests.
pub(crate) fn op_id_value(id: &OpId) -> Value {
    Value::Array(vec![
        Value::Text(id.replica.actor.clone()),
        Value::Text(id.replica.key.clone()),
        Value::Int(i64::from(id.replica.lane)),
        Value::Int(id.counter as i64),
    ])
}

/// Reads `[actor, key, lane, counter]`.
pub(crate) fn value_op_id(v: &Value) -> Option<OpId> {
    let Value::Array(parts) = v else { return None };
    let [
        Value::Text(actor),
        Value::Text(key),
        Value::Int(lane),
        Value::Int(counter),
    ] = parts.as_slice()
    else {
        return None;
    };
    let lane = u32::try_from(*lane).ok()?;
    if *counter < 1 {
        return None;
    }
    Some(OpId::new(
        *counter as u64,
        std::sync::Arc::new(ReplicaId::new(actor.clone(), key.clone(), lane)),
    ))
}
