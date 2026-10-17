//! Strict I-JSON (RFC 7493) parsing and JCS (RFC 8785) canonicalization.
//!
//! HAN frames are I-JSON with one further rule: every number is an integer in
//! `[-(2^53 - 1), 2^53 - 1]` written without a fraction or an exponent (architecture.md section 2).
//! [`parse`] enforces exactly that. Duplicate object keys, lone surrogates, invalid UTF-8, `-0`, and
//! integers outside the safe range are errors; nothing is repaired.
//!
//! [`Value`] has a `Float` variant only so that the canonicalizer is a complete RFC 8785
//! implementation (ES6 number formatting included) that can be checked against an independent
//! one. [`parse`] never produces it; only [`parse_with`]`(.., Numbers::Any)` does.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde::ser::{Serialize, SerializeMap, SerializeSeq, Serializer};
use thiserror::Error;

/// The largest integer I-JSON can carry exactly: `2^53 - 1`.
pub const MAX_SAFE_INT: i64 = (1 << 53) - 1;

/// How deep arrays and objects may nest before [`parse`] gives up.
pub const MAX_DEPTH: usize = 128;

/// A JSON value. Objects are sorted maps; [`canonicalize`] orders keys by UTF-16 code units.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// `null`
    Null,
    /// `true` or `false`
    Bool(bool),
    /// An integer in `[-(2^53 - 1), 2^53 - 1]`.
    Int(i64),
    /// A finite float. Never produced by [`parse`]; entry validation rejects it.
    Float(f64),
    /// A string. Rust strings cannot hold lone surrogates, so none can appear here.
    Str(String),
    /// An array.
    Array(Vec<Value>),
    /// An object with unique keys.
    Object(BTreeMap<String, Value>),
}

impl Value {
    /// The member `key` of an object, if this is an object holding it.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(m) => m.get(key),
            _ => None,
        }
    }

    /// The string, if this is one.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    /// The integer, if this is one.
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }

    /// The non-negative integer, if this is one.
    pub fn as_u64(&self) -> Option<u64> {
        self.as_i64().and_then(|i| u64::try_from(i).ok())
    }

    /// The array items, if this is an array.
    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }

    /// The members, if this is an object.
    pub fn as_object(&self) -> Option<&BTreeMap<String, Value>> {
        match self {
            Value::Object(m) => Some(m),
            _ => None,
        }
    }
}

static NULL: Value = Value::Null;

impl std::ops::Index<&str> for Value {
    type Output = Value;

    /// The member `key`, or `null` if this is not an object or lacks it.
    fn index(&self, key: &str) -> &Value {
        self.get(key).unwrap_or(&NULL)
    }
}

/// What can go wrong parsing or canonicalizing JSON.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum JsonError {
    /// The input is not UTF-8.
    #[error("input is not valid UTF-8")]
    InvalidUtf8,
    /// The input is not JSON.
    #[error("syntax error at byte {offset}: {what}")]
    Syntax {
        /// Byte offset of the problem.
        offset: usize,
        /// What was wrong.
        what: &'static str,
    },
    /// An object has the same key twice.
    #[error("duplicate object key {0:?}")]
    DuplicateKey(String),
    /// A `\uD800`-style escape is not part of a valid pair.
    #[error("lone surrogate in a string escape")]
    LoneSurrogate,
    /// An integer is outside `[-(2^53 - 1), 2^53 - 1]`.
    #[error("integer outside the I-JSON safe range")]
    IntegerOutOfRange,
    /// A fraction, an exponent or `-0` where only integers are allowed.
    #[error("number is not an integer in plain form")]
    NonInteger,
    /// A float that is NaN or infinite.
    #[error("non-finite number")]
    NonFinite,
    /// Arrays and objects nest deeper than [`MAX_DEPTH`].
    #[error("nesting deeper than {MAX_DEPTH}")]
    TooDeep,
    /// The bytes are valid JSON but not in canonical form ([`check_canonical`]).
    #[error("not in canonical (JCS) form")]
    NotCanonical,
    /// A typed value could not be converted to or from a [`Value`].
    #[error("conversion: {0}")]
    Convert(String),
}

/// Which numbers [`parse_with`] accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Numbers {
    /// HAN's rule: plain integers in the safe range.
    IntegersOnly,
    /// Any I-JSON number; non-integers become [`Value::Float`].
    Any,
}

/// Parses a HAN frame: strict I-JSON with integers only.
pub fn parse(input: &[u8]) -> Result<Value, JsonError> {
    parse_with(input, Numbers::IntegersOnly)
}

/// Parses strict I-JSON, with the number rule chosen by `numbers`.
pub fn parse_with(input: &[u8], numbers: Numbers) -> Result<Value, JsonError> {
    let text = std::str::from_utf8(input).map_err(|_| JsonError::InvalidUtf8)?;
    parse_text(text, numbers, false).map(|(v, _)| v)
}

/// Parses one frame (G-07): strict I-JSON with integers only, except that an integer outside
/// `[-(2^53 - 1), 2^53 - 1]` is not an error. It becomes `i64::MAX` and the flag in the result is
/// set, so the caller can answer on the request's own id. `-0`, fractions, exponents, duplicate
/// keys and nesting over [`MAX_DEPTH`] stay errors.
pub fn parse_frame(input: &[u8]) -> Result<(Value, bool), JsonError> {
    let text = std::str::from_utf8(input).map_err(|_| JsonError::InvalidUtf8)?;
    parse_text(text, Numbers::IntegersOnly, true)
}

fn parse_text(text: &str, numbers: Numbers, wide: bool) -> Result<(Value, bool), JsonError> {
    let mut p = Parser {
        src: text,
        bytes: text.as_bytes(),
        pos: 0,
        numbers,
        wide,
        saw_wide: false,
    };
    p.ws();
    let v = p.value(0)?;
    p.ws();
    if p.pos != p.bytes.len() {
        return Err(p.err("trailing characters"));
    }
    Ok((v, p.saw_wide))
}

struct Parser<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    numbers: Numbers,
    /// Tolerate integers outside the safe range (see [`parse_frame`]).
    wide: bool,
    saw_wide: bool,
}

impl Parser<'_> {
    fn err(&self, what: &'static str) -> JsonError {
        JsonError::Syntax {
            offset: self.pos,
            what,
        }
    }

    fn ws(&mut self) {
        while matches!(self.bytes.get(self.pos), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn eat(&mut self, lit: &str, what: &'static str) -> Result<(), JsonError> {
        if self.bytes[self.pos..].starts_with(lit.as_bytes()) {
            self.pos += lit.len();
            Ok(())
        } else {
            Err(self.err(what))
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value, JsonError> {
        match self.bytes.get(self.pos) {
            None => Err(self.err("unexpected end of input")),
            Some(b'n') => self.eat("null", "bad literal").map(|()| Value::Null),
            Some(b't') => self.eat("true", "bad literal").map(|()| Value::Bool(true)),
            Some(b'f') => self
                .eat("false", "bad literal")
                .map(|()| Value::Bool(false)),
            Some(b'"') => self.string().map(Value::Str),
            Some(b'[') => self.array(depth),
            Some(b'{') => self.object(depth),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(_) => Err(self.err("unexpected character")),
        }
    }

    fn array(&mut self, depth: usize) -> Result<Value, JsonError> {
        if depth >= MAX_DEPTH {
            return Err(JsonError::TooDeep);
        }
        self.pos += 1;
        let mut items = Vec::new();
        self.ws();
        if self.bytes.get(self.pos) == Some(&b']') {
            self.pos += 1;
            return Ok(Value::Array(items));
        }
        loop {
            self.ws();
            items.push(self.value(depth + 1)?);
            self.ws();
            match self.bytes.get(self.pos) {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Value::Array(items));
                }
                _ => return Err(self.err("expected ',' or ']'")),
            }
        }
    }

    fn object(&mut self, depth: usize) -> Result<Value, JsonError> {
        if depth >= MAX_DEPTH {
            return Err(JsonError::TooDeep);
        }
        self.pos += 1;
        let mut members = BTreeMap::new();
        self.ws();
        if self.bytes.get(self.pos) == Some(&b'}') {
            self.pos += 1;
            return Ok(Value::Object(members));
        }
        loop {
            self.ws();
            if self.bytes.get(self.pos) != Some(&b'"') {
                return Err(self.err("expected a string key"));
            }
            let key = self.string()?;
            self.ws();
            if self.bytes.get(self.pos) != Some(&b':') {
                return Err(self.err("expected ':'"));
            }
            self.pos += 1;
            self.ws();
            let v = self.value(depth + 1)?;
            if members.insert(key.clone(), v).is_some() {
                return Err(JsonError::DuplicateKey(key));
            }
            self.ws();
            match self.bytes.get(self.pos) {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(Value::Object(members));
                }
                _ => return Err(self.err("expected ',' or '}'")),
            }
        }
    }

    fn hex4(&mut self) -> Result<u32, JsonError> {
        let digits = self
            .bytes
            .get(self.pos..self.pos + 4)
            .ok_or_else(|| self.err("short \\u escape"))?;
        let mut n = 0u32;
        for &b in digits {
            let d = (b as char)
                .to_digit(16)
                .ok_or_else(|| self.err("bad \\u escape"))?;
            n = n * 16 + d;
        }
        self.pos += 4;
        Ok(n)
    }

    fn string(&mut self) -> Result<String, JsonError> {
        self.pos += 1; // opening quote
        let mut out = String::new();
        let mut run = self.pos;
        loop {
            let Some(&b) = self.bytes.get(self.pos) else {
                return Err(self.err("unterminated string"));
            };
            match b {
                b'"' => {
                    out.push_str(&self.src[run..self.pos]);
                    self.pos += 1;
                    return Ok(out);
                }
                0..=0x1f => return Err(self.err("control character in string")),
                b'\\' => {
                    out.push_str(&self.src[run..self.pos]);
                    self.pos += 1;
                    let Some(&e) = self.bytes.get(self.pos) else {
                        return Err(self.err("unterminated escape"));
                    };
                    self.pos += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let hi = self.hex4()?;
                            let cp = match hi {
                                0xD800..=0xDBFF => {
                                    if !self.bytes[self.pos..].starts_with(b"\\u") {
                                        return Err(JsonError::LoneSurrogate);
                                    }
                                    self.pos += 2;
                                    let lo = self.hex4()?;
                                    if !(0xDC00..=0xDFFF).contains(&lo) {
                                        return Err(JsonError::LoneSurrogate);
                                    }
                                    0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                                }
                                0xDC00..=0xDFFF => return Err(JsonError::LoneSurrogate),
                                _ => hi,
                            };
                            out.push(char::from_u32(cp).ok_or(JsonError::LoneSurrogate)?);
                        }
                        _ => return Err(self.err("bad escape")),
                    }
                    run = self.pos;
                }
                _ => self.pos += 1,
            }
        }
    }

    fn number(&mut self) -> Result<Value, JsonError> {
        let start = self.pos;
        let negative = self.bytes[self.pos] == b'-';
        if negative {
            self.pos += 1;
        }
        let int_start = self.pos;
        match self.bytes.get(self.pos) {
            Some(b'0') => self.pos += 1,
            Some(b'1'..=b'9') => self.digits(),
            _ => return Err(self.err("bad number")),
        }
        let int_digits = &self.src[int_start..self.pos];
        let mut plain = true;
        if self.bytes.get(self.pos) == Some(&b'.') {
            self.pos += 1;
            if !matches!(self.bytes.get(self.pos), Some(b'0'..=b'9')) {
                return Err(self.err("bad fraction"));
            }
            self.digits();
            plain = false;
        }
        if matches!(self.bytes.get(self.pos), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.bytes.get(self.pos), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if !matches!(self.bytes.get(self.pos), Some(b'0'..=b'9')) {
                return Err(self.err("bad exponent"));
            }
            self.digits();
            plain = false;
        }
        if plain {
            let magnitude = if int_digits.len() > 16 {
                i64::MAX
            } else {
                int_digits.parse().map_err(|_| self.err("bad number"))?
            };
            if magnitude > MAX_SAFE_INT {
                if self.wide {
                    self.saw_wide = true;
                    return Ok(Value::Int(i64::MAX));
                }
                return Err(JsonError::IntegerOutOfRange);
            }
            if negative && magnitude == 0 && self.numbers == Numbers::IntegersOnly {
                return Err(JsonError::NonInteger); // "-0"
            }
            if negative && magnitude == 0 {
                return Ok(Value::Float(-0.0));
            }
            return Ok(Value::Int(if negative { -magnitude } else { magnitude }));
        }
        if self.numbers == Numbers::IntegersOnly {
            return Err(JsonError::NonInteger);
        }
        let f: f64 = self.src[start..self.pos]
            .parse()
            .map_err(|_| self.err("bad number"))?;
        if f.is_finite() {
            Ok(Value::Float(f))
        } else {
            Err(JsonError::NonFinite)
        }
    }

    fn digits(&mut self) {
        while matches!(self.bytes.get(self.pos), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
    }
}

/// The JCS (RFC 8785) bytes of `v`: no whitespace, keys ordered by UTF-16 code units, ES6 numbers.
///
/// Fails on an integer outside the safe range or a non-finite float, because those have no exact
/// canonical form.
pub fn canonicalize(v: &Value) -> Result<Vec<u8>, JsonError> {
    let mut out = String::new();
    write_canonical(v, &mut out)?;
    Ok(out.into_bytes())
}

/// Succeeds only if `input` is valid HAN JSON that is already byte-for-byte its own canonical form.
pub fn check_canonical(input: &[u8]) -> Result<(), JsonError> {
    let v = parse(input)?;
    if canonicalize(&v)? == input {
        Ok(())
    } else {
        Err(JsonError::NotCanonical)
    }
}

fn write_canonical(v: &Value, out: &mut String) -> Result<(), JsonError> {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Int(i) => {
            if i.unsigned_abs() > MAX_SAFE_INT.unsigned_abs() {
                return Err(JsonError::IntegerOutOfRange);
            }
            let _ = write!(out, "{i}");
        }
        Value::Float(f) => out.push_str(&es6_number(*f)?),
        Value::Str(s) => write_string(s, out),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out)?;
            }
            out.push(']');
        }
        Value::Object(members) => {
            let mut keys: Vec<&String> = members.keys().collect();
            keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            out.push('{');
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_string(k, out);
                out.push(':');
                write_canonical(&members[k], out)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

fn write_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// ECMAScript `Number::toString(10)` for a finite double (RFC 8785 section 3.2.2.3).
///
/// Delegates to `ryu-js`, which implements the ECMAScript algorithm exactly, including the
/// choice of the even digit when two shortest candidates are equally close (e.g. `x.25`).
fn es6_number(f: f64) -> Result<String, JsonError> {
    if !f.is_finite() {
        return Err(JsonError::NonFinite);
    }
    if f == 0.0 {
        return Ok("0".into());
    }
    let mut buf = ryu_js::Buffer::new();
    Ok(buf.format_finite(f).to_owned())
}

/// Converts a typed value (an `Entry`, a `Receipt`, ...) to a [`Value`] through serde.
pub fn to_value<T: Serialize>(t: &T) -> Result<Value, JsonError> {
    let sj = serde_json::to_value(t).map_err(|e| JsonError::Convert(e.to_string()))?;
    from_serde(&sj)
}

/// Converts a [`Value`] to a typed value through serde. Unknown fields and wrong types are errors
/// whenever the target type is closed.
pub fn from_value<T: serde::de::DeserializeOwned>(v: &Value) -> Result<T, JsonError> {
    serde_json::from_value(to_serde(v)).map_err(|e| JsonError::Convert(e.to_string()))
}

/// Converts a [`Value`] to a closed typed value with nothing implied: an object only from an
/// object, a sequence only from an array, an enum only from its name, and `null` never for an
/// optional field (absent is how a body leaves one out). This is how bodies and contents are
/// decoded, so that a value decodes exactly when it would encode back to itself.
pub fn from_value_strict<T: serde::de::DeserializeOwned>(v: &Value) -> Result<T, JsonError> {
    T::deserialize(Strict(v)).map_err(|e| JsonError::Convert(e.to_string()))
}

/// The deserializer of [`from_value_strict`].
#[derive(Clone, Copy)]
struct Strict<'a>(&'a Value);

type StrictError = de::value::Error;

impl<'a> de::IntoDeserializer<'a, StrictError> for Strict<'a> {
    type Deserializer = Self;

    fn into_deserializer(self) -> Self {
        self
    }
}

impl Strict<'_> {
    fn wrong<T>(self, want: &str) -> Result<T, StrictError> {
        Err(de::Error::custom(format!("expected {want}")))
    }
}

impl<'de> Deserializer<'de> for Strict<'de> {
    type Error = StrictError;

    fn deserialize_any<V: Visitor<'de>>(self, v: V) -> Result<V::Value, StrictError> {
        match self.0 {
            Value::Null => v.visit_unit(),
            Value::Bool(b) => v.visit_bool(*b),
            Value::Int(i) => v.visit_i64(*i),
            Value::Float(f) => v.visit_f64(*f),
            Value::Str(s) => v.visit_borrowed_str(s),
            Value::Array(a) => {
                let mut seq =
                    de::value::SeqDeserializer::<_, StrictError>::new(a.iter().map(Strict));
                let out = v.visit_seq(&mut seq)?;
                seq.end()?;
                Ok(out)
            }
            Value::Object(m) => {
                let mut map = de::value::MapDeserializer::<_, StrictError>::new(
                    m.iter().map(|(k, x)| (k.as_str(), Strict(x))),
                );
                let out = v.visit_map(&mut map)?;
                map.end()?;
                Ok(out)
            }
        }
    }

    fn deserialize_option<V: Visitor<'de>>(self, v: V) -> Result<V::Value, StrictError> {
        match self.0 {
            Value::Null => self.wrong("a value, not null"),
            _ => v.visit_some(self),
        }
    }

    fn deserialize_seq<V: Visitor<'de>>(self, v: V) -> Result<V::Value, StrictError> {
        match self.0 {
            Value::Array(_) => self.deserialize_any(v),
            _ => self.wrong("an array"),
        }
    }

    fn deserialize_map<V: Visitor<'de>>(self, v: V) -> Result<V::Value, StrictError> {
        match self.0 {
            Value::Object(_) => self.deserialize_any(v),
            _ => self.wrong("an object"),
        }
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _fields: &'static [&'static str],
        v: V,
    ) -> Result<V::Value, StrictError> {
        self.deserialize_map(v)
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        v: V,
    ) -> Result<V::Value, StrictError> {
        match self.0 {
            Value::Str(s) => v.visit_enum(de::IntoDeserializer::into_deserializer(s.as_str())),
            _ => self.wrong("a name"),
        }
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        v: V,
    ) -> Result<V::Value, StrictError> {
        v.visit_newtype_struct(self)
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string bytes byte_buf
        unit unit_struct tuple tuple_struct identifier ignored_any
    }
}

fn from_serde(v: &serde_json::Value) -> Result<Value, JsonError> {
    Ok(match v {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                if i.unsigned_abs() > MAX_SAFE_INT.unsigned_abs() {
                    return Err(JsonError::IntegerOutOfRange);
                }
                Value::Int(i)
            } else if n.is_u64() {
                return Err(JsonError::IntegerOutOfRange);
            } else {
                Value::Float(n.as_f64().ok_or(JsonError::NonFinite)?)
            }
        }
        serde_json::Value::String(s) => Value::Str(s.clone()),
        serde_json::Value::Array(a) => {
            Value::Array(a.iter().map(from_serde).collect::<Result<_, _>>()?)
        }
        serde_json::Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, v)| Ok((k.clone(), from_serde(v)?)))
                .collect::<Result<_, JsonError>>()?,
        ),
    })
}

fn to_serde(v: &Value) -> serde_json::Value {
    match v {
        Value::Null => serde_json::Value::Null,
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Int(i) => serde_json::Value::from(*i),
        Value::Float(f) => serde_json::Number::from_f64(*f)
            .map_or(serde_json::Value::Null, serde_json::Value::Number),
        Value::Str(s) => serde_json::Value::String(s.clone()),
        Value::Array(a) => serde_json::Value::Array(a.iter().map(to_serde).collect()),
        Value::Object(m) => {
            serde_json::Value::Object(m.iter().map(|(k, v)| (k.clone(), to_serde(v))).collect())
        }
    }
}

impl Serialize for Value {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Value::Null => s.serialize_unit(),
            Value::Bool(b) => s.serialize_bool(*b),
            Value::Int(i) => s.serialize_i64(*i),
            Value::Float(f) => s.serialize_f64(*f),
            Value::Str(t) => s.serialize_str(t),
            Value::Array(a) => {
                let mut seq = s.serialize_seq(Some(a.len()))?;
                for item in a {
                    seq.serialize_element(item)?;
                }
                seq.end()
            }
            Value::Object(m) => {
                let mut map = s.serialize_map(Some(m.len()))?;
                for (k, v) in m {
                    map.serialize_entry(k, v)?;
                }
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for Value {
    /// Builds a value from any serde source. Sources that merge duplicate keys are not checked
    /// here; strictness is [`parse`]'s job.
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Value;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("an I-JSON value")
            }
            fn visit_unit<E>(self) -> Result<Value, E> {
                Ok(Value::Null)
            }
            fn visit_none<E>(self) -> Result<Value, E> {
                Ok(Value::Null)
            }
            fn visit_bool<E>(self, b: bool) -> Result<Value, E> {
                Ok(Value::Bool(b))
            }
            fn visit_i64<E: de::Error>(self, i: i64) -> Result<Value, E> {
                if i.unsigned_abs() > MAX_SAFE_INT.unsigned_abs() {
                    return Err(E::custom("integer outside the I-JSON safe range"));
                }
                Ok(Value::Int(i))
            }
            fn visit_u64<E: de::Error>(self, u: u64) -> Result<Value, E> {
                i64::try_from(u)
                    .map_err(|_| E::custom("integer outside the I-JSON safe range"))
                    .and_then(|i| self.visit_i64(i))
            }
            fn visit_f64<E: de::Error>(self, f: f64) -> Result<Value, E> {
                if f.is_finite() {
                    Ok(Value::Float(f))
                } else {
                    Err(E::custom("non-finite number"))
                }
            }
            fn visit_str<E>(self, s: &str) -> Result<Value, E> {
                Ok(Value::Str(s.to_owned()))
            }
            fn visit_string<E>(self, s: String) -> Result<Value, E> {
                Ok(Value::Str(s))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Value, A::Error> {
                let mut items = Vec::new();
                while let Some(item) = a.next_element()? {
                    items.push(item);
                }
                Ok(Value::Array(items))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<Value, A::Error> {
                let mut members = BTreeMap::new();
                while let Some((k, v)) = a.next_entry::<String, Value>()? {
                    members.insert(k, v);
                }
                Ok(Value::Object(members))
            }
        }
        d.deserialize_any(V)
    }
}
