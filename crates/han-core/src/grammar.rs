//! The grammar of architecture.md section 2. Every pattern is ASCII and must match the whole string.

use crate::json::{MAX_SAFE_INT, Value};

fn all(s: &str, ok: impl Fn(u8) -> bool) -> bool {
    s.bytes().all(ok)
}

fn lower_alnum(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit()
}

/// A lowercase DNS name: dot-separated labels `[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?`.
pub fn is_server_name(s: &str) -> bool {
    !s.is_empty()
        && s.split('.').all(|label| {
            let b = label.as_bytes();
            (1..=63).contains(&b.len())
                && lower_alnum(b[0])
                && lower_alnum(b[b.len() - 1])
                && all(label, |c| lower_alnum(c) || c == b'-')
        })
}

/// `@name:server`, `name` being `[a-z0-9._-]{0,64}`; the empty name is the server actor.
pub fn is_actor_id(s: &str) -> bool {
    let Some(rest) = s.strip_prefix('@') else {
        return false;
    };
    let Some((name, server)) = rest.split_once(':') else {
        return false;
    };
    name.len() <= 64
        && all(name, |c| lower_alnum(c) || matches!(c, b'.' | b'_' | b'-'))
        && is_server_name(server)
}

/// `[a-z0-9]{1,26}:server` (the name `profile` is reserved) or `profile:` + an ActorId.
pub fn is_space_id(s: &str) -> bool {
    if let Some(actor) = s.strip_prefix("profile:") {
        return is_actor_id(actor);
    }
    let Some((id, server)) = s.split_once(':') else {
        return false;
    };
    (1..=26).contains(&id.len()) && all(id, lower_alnum) && is_server_name(server)
}

/// The server a space or actor id names: the part after the last `:`.
pub fn server_of(id: &str) -> Option<&str> {
    id.rsplit_once(':').map(|(_, server)| server)
}

/// EntryId and Hex: 64 lowercase hex digits.
pub fn is_hex(s: &str) -> bool {
    s.len() == 64 && all(s, |c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

/// KeyId: `[A-Za-z0-9_-]{1,64}`.
pub fn is_key_id(s: &str) -> bool {
    (1..=64).contains(&s.len()) && all(s, |c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}

/// A nonce: 26 characters of `[a-z2-7]`.
pub fn is_nonce(s: &str) -> bool {
    s.len() == 26 && all(s, |c| c.is_ascii_lowercase() || (b'2'..=b'7').contains(&c))
}

/// An action: `[a-z0-9][a-z0-9._-]{0,127}`.
pub fn is_action(s: &str) -> bool {
    let b = s.as_bytes();
    (1..=128).contains(&b.len())
        && lower_alnum(b[0])
        && all(s, |c| lower_alnum(c) || matches!(c, b'.' | b'_' | b'-'))
}

/// A Budget unit: `[A-Za-z0-9_-]{1,32}`.
pub fn is_unit(s: &str) -> bool {
    (1..=32).contains(&s.len()) && all(s, |c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}

/// A task's external reference: printable ASCII without spaces, 1 to 1024 characters.
pub fn is_external(s: &str) -> bool {
    (1..=1024).contains(&s.len()) && all(s, |c| (b'!'..=b'~').contains(&c))
}

/// The textual grammar of a `publicKey`: base64url characters, 1 to 128 of them.
/// (The Log additionally requires 32 canonical bytes, see `validate`.)
pub fn is_public_key_text(s: &str) -> bool {
    (1..=128).contains(&s.len()) && all(s, |c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}

/// A media type `type/subtype`, each half `[a-z0-9][a-z0-9!#$&^_.+-]{0,63}`.
pub fn is_media_type(s: &str) -> bool {
    let half = |h: &str| {
        let b = h.as_bytes();
        (1..=64).contains(&b.len())
            && lower_alnum(b[0])
            && all(h, |c| lower_alnum(c) || b"!#$&^_.+-".contains(&c))
    };
    s.split_once('/').is_some_and(|(a, b)| half(a) && half(b))
}

/// The prefixes the core reserves; an unknown kind under one of them is invalid.
const CORE_PREFIXES: [&str; 11] = [
    "space",
    "member",
    "policy",
    "key",
    "message",
    "task",
    "effect",
    "checkpoint",
    "redact",
    "session",
    "agent",
];

/// An extension kind `<prefix>.<name>` whose prefix is not a core prefix.
pub fn is_extension_kind(s: &str) -> bool {
    let Some((prefix, name)) = s.split_once('.') else {
        return false;
    };
    let p = prefix.as_bytes();
    let n = name.as_bytes();
    (1..=32).contains(&p.len())
        && p[0].is_ascii_lowercase()
        && all(prefix, |c| lower_alnum(c) || c == b'-')
        && (1..=64).contains(&n.len())
        && lower_alnum(n[0])
        && all(name, |c| lower_alnum(c) || matches!(c, b'.' | b'_' | b'-'))
        && !CORE_PREFIXES.contains(&prefix)
}

/// A HAN Time `YYYY-MM-DDTHH:MM:SSZ`: a real proleptic Gregorian date (year 0001 to 9999), seconds 00 to 59.
pub fn is_time(s: &str) -> bool {
    epoch_seconds(s).is_some()
}

/// Seconds since 1970-01-01T00:00:00Z of a valid HAN Time, or `None` if `s` is not one.
pub fn epoch_seconds(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() != 20 {
        return None;
    }
    let shape = b"dddd-dd-ddTdd:dd:ddZ";
    for (c, p) in b.iter().zip(shape) {
        let ok = if *p == b'd' {
            c.is_ascii_digit()
        } else {
            c == p
        };
        if !ok {
            return None;
        }
    }
    let num = |from: usize, to: usize| s[from..to].parse::<i64>().ok();
    let (year, month, day) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (hour, minute, second) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let days_in_month = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => 28 + i64::from(leap),
        _ => return None,
    };
    if year < 1 || day < 1 || day > days_in_month || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    // Days from civil (Howard Hinnant).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hour * 3600 + minute * 60 + second)
}

/// Opaque Json as the core allows it (extension bodies and contents, `plan.args`): integers in
/// `[0, 2^53)` only, no floats, at most 32 levels deep.
pub fn is_opaque_json(v: &Value) -> bool {
    fn go(v: &Value, depth: usize) -> bool {
        depth <= 32
            && match v {
                Value::Null | Value::Bool(_) | Value::Str(_) => true,
                Value::Int(i) => (0..=MAX_SAFE_INT).contains(i),
                Value::Float(_) => false,
                Value::Array(a) => a.iter().all(|x| go(x, depth + 1)),
                Value::Object(m) => m.values().all(|x| go(x, depth + 1)),
            }
    }
    go(v, 0)
}
