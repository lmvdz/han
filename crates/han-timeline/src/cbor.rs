//! Core deterministic CBOR (RFC 8949 §4.2.1), the only encoding of the profile (TL-90).
//!
//! Decoding is strict: definite lengths, no tags, no floats, no simple values other than
//! `false`, `true` and `null`, valid UTF-8, and the input must be exactly the deterministic
//! encoding of what it decodes to (checked by re-encoding).

use std::cmp::Ordering;

/// One CBOR data item of the subset the profile uses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cbor {
    /// An unsigned integer (major type 0).
    Uint(u64),
    /// A negative integer `-1 - n` (major type 1), holding `n`.
    Nint(u64),
    /// A byte string.
    Bytes(Vec<u8>),
    /// A text string.
    Text(String),
    /// An array.
    Array(Vec<Cbor>),
    /// A map, in any order; encoding sorts it.
    Map(Vec<(Cbor, Cbor)>),
    /// `false` or `true`.
    Bool(bool),
    /// `null`.
    Null,
}

/// Why bytes are not acceptable deterministic CBOR.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CborError {
    /// Not well-formed, not of the subset, or not deterministic.
    Malformed,
    /// Nested deeper than the decoder allows.
    TooDeep,
}

/// The deepest nesting the decoder accepts; protocol structures stay far below it.
const MAX_DEPTH: usize = 48;

impl Cbor {
    /// Shorthand for a small unsigned integer.
    pub fn u(n: u64) -> Cbor {
        Cbor::Uint(n)
    }

    /// Shorthand for a text string.
    pub fn text(s: impl Into<String>) -> Cbor {
        Cbor::Text(s.into())
    }

    /// The deterministic encoding.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode_into(&mut out);
        out
    }

    /// Appends the deterministic encoding to `out`.
    pub fn encode_into(&self, out: &mut Vec<u8>) {
        match self {
            Cbor::Uint(n) => head(out, 0, *n),
            Cbor::Nint(n) => head(out, 1, *n),
            Cbor::Bytes(b) => {
                head(out, 2, b.len() as u64);
                out.extend_from_slice(b);
            }
            Cbor::Text(s) => {
                head(out, 3, s.len() as u64);
                out.extend_from_slice(s.as_bytes());
            }
            Cbor::Array(items) => {
                head(out, 4, items.len() as u64);
                for item in items {
                    item.encode_into(out);
                }
            }
            Cbor::Map(entries) => {
                let mut encoded: Vec<(Vec<u8>, Vec<u8>)> = entries
                    .iter()
                    .map(|(k, v)| (k.encode(), v.encode()))
                    .collect();
                encoded.sort_by(|a, b| a.0.cmp(&b.0));
                head(out, 5, encoded.len() as u64);
                for (k, v) in encoded {
                    out.extend_from_slice(&k);
                    out.extend_from_slice(&v);
                }
            }
            Cbor::Bool(false) => out.push(0xf4),
            Cbor::Bool(true) => out.push(0xf5),
            Cbor::Null => out.push(0xf6),
        }
    }

    /// Decodes exactly one deterministic item that spans all of `bytes`.
    pub fn decode(bytes: &[u8]) -> Result<Cbor, CborError> {
        let mut reader = Reader { bytes, pos: 0 };
        let item = reader.item(0)?;
        if reader.pos != bytes.len() {
            return Err(CborError::Malformed);
        }
        // Shortest forms are checked while reading; sorted, unique map keys are checked here.
        if item.encode() != bytes {
            return Err(CborError::Malformed);
        }
        check_unique_keys(&item)?;
        Ok(item)
    }

    /// The map entry with unsigned integer key `key`.
    pub fn get(&self, key: u64) -> Option<&Cbor> {
        match self {
            Cbor::Map(entries) => entries
                .iter()
                .find(|(k, _)| *k == Cbor::Uint(key))
                .map(|(_, v)| v),
            _ => None,
        }
    }
}

fn head(out: &mut Vec<u8>, major: u8, n: u64) {
    let m = major << 5;
    if n < 24 {
        out.push(m | n as u8);
    } else if n <= 0xff {
        out.push(m | 24);
        out.push(n as u8);
    } else if n <= 0xffff {
        out.push(m | 25);
        out.extend_from_slice(&(n as u16).to_be_bytes());
    } else if n <= 0xffff_ffff {
        out.push(m | 26);
        out.extend_from_slice(&(n as u32).to_be_bytes());
    } else {
        out.push(m | 27);
        out.extend_from_slice(&n.to_be_bytes());
    }
}

fn check_unique_keys(item: &Cbor) -> Result<(), CborError> {
    match item {
        Cbor::Array(items) => items.iter().try_for_each(check_unique_keys),
        Cbor::Map(entries) => {
            let keys: Vec<Vec<u8>> = entries.iter().map(|(k, _)| k.encode()).collect();
            if keys.windows(2).any(|w| w[0].cmp(&w[1]) != Ordering::Less) {
                return Err(CborError::Malformed);
            }
            entries
                .iter()
                .try_for_each(|(k, v)| check_unique_keys(k).and_then(|()| check_unique_keys(v)))
        }
        _ => Ok(()),
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn byte(&mut self) -> Result<u8, CborError> {
        let b = *self.bytes.get(self.pos).ok_or(CborError::Malformed)?;
        self.pos += 1;
        Ok(b)
    }

    fn take(&mut self, n: u64) -> Result<&[u8], CborError> {
        let n = usize::try_from(n).map_err(|_| CborError::Malformed)?;
        let end = self.pos.checked_add(n).ok_or(CborError::Malformed)?;
        let slice = self.bytes.get(self.pos..end).ok_or(CborError::Malformed)?;
        self.pos = end;
        Ok(slice)
    }

    /// The argument of a head, rejecting non-shortest forms and indefinite lengths.
    fn argument(&mut self, info: u8) -> Result<u64, CborError> {
        let n = match info {
            0..=23 => return Ok(u64::from(info)),
            24 => u64::from(self.byte()?),
            25 => u64::from(u16::from_be_bytes(
                self.take(2)?.try_into().unwrap_or([0; 2]),
            )),
            26 => u64::from(u32::from_be_bytes(
                self.take(4)?.try_into().unwrap_or([0; 4]),
            )),
            27 => u64::from_be_bytes(self.take(8)?.try_into().unwrap_or([0; 8])),
            _ => return Err(CborError::Malformed),
        };
        let shortest = match info {
            24 => n >= 24,
            25 => n > 0xff,
            26 => n > 0xffff,
            _ => n > 0xffff_ffff,
        };
        if shortest {
            Ok(n)
        } else {
            Err(CborError::Malformed)
        }
    }

    fn item(&mut self, depth: usize) -> Result<Cbor, CborError> {
        if depth > MAX_DEPTH {
            return Err(CborError::TooDeep);
        }
        let initial = self.byte()?;
        let major = initial >> 5;
        let info = initial & 0x1f;
        if major == 7 {
            return match info {
                20 => Ok(Cbor::Bool(false)),
                21 => Ok(Cbor::Bool(true)),
                22 => Ok(Cbor::Null),
                _ => Err(CborError::Malformed),
            };
        }
        let n = self.argument(info)?;
        match major {
            0 => Ok(Cbor::Uint(n)),
            1 => Ok(Cbor::Nint(n)),
            2 => Ok(Cbor::Bytes(self.take(n)?.to_vec())),
            3 => {
                let raw = self.take(n)?.to_vec();
                String::from_utf8(raw)
                    .map(Cbor::Text)
                    .map_err(|_| CborError::Malformed)
            }
            4 => {
                // Every item takes at least one byte, so a claimed length beyond the input is bogus.
                if n > (self.bytes.len() - self.pos) as u64 {
                    return Err(CborError::Malformed);
                }
                (0..n)
                    .map(|_| self.item(depth + 1))
                    .collect::<Result<_, _>>()
                    .map(Cbor::Array)
            }
            5 => {
                if n > (self.bytes.len() - self.pos) as u64 / 2 {
                    return Err(CborError::Malformed);
                }
                let mut entries = Vec::with_capacity(n as usize);
                for _ in 0..n {
                    let k = self.item(depth + 1)?;
                    let v = self.item(depth + 1)?;
                    entries.push((k, v));
                }
                Ok(Cbor::Map(entries))
            }
            _ => Err(CborError::Malformed), // tags (6)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_rejections() {
        let item = Cbor::Map(vec![
            (Cbor::u(2), Cbor::text("b")),
            (Cbor::u(1), Cbor::Array(vec![Cbor::Nint(0), Cbor::Null])),
        ]);
        let bytes = item.encode();
        assert_eq!(bytes, [0xa2, 0x01, 0x82, 0x20, 0xf6, 0x02, 0x61, b'b']);
        assert!(Cbor::decode(&bytes).is_ok());
        // non-shortest integer
        assert!(Cbor::decode(&[0x18, 0x01]).is_err());
        // unsorted map
        assert!(Cbor::decode(&[0xa2, 0x02, 0xf6, 0x01, 0xf6]).is_err());
        // duplicate key
        assert!(Cbor::decode(&[0xa2, 0x01, 0xf6, 0x01, 0xf6]).is_err());
        // indefinite array, tag, float
        assert!(Cbor::decode(&[0x9f, 0xff]).is_err());
        assert!(Cbor::decode(&[0xc0, 0x01]).is_err());
        assert!(Cbor::decode(&[0xf9, 0x00, 0x00]).is_err());
        // trailing bytes
        assert!(Cbor::decode(&[0x01, 0x01]).is_err());
    }
}
