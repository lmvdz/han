//! JCS and I-JSON: RFC 8785 number vectors, string escaping, strict-parse rejections, and a
//! property test against `serde_json_canonicalizer` (a test oracle only).

use std::collections::BTreeMap;

use han_core::json::{
    JsonError, MAX_DEPTH, MAX_SAFE_INT, Numbers, Value, canonicalize, check_canonical, parse,
    parse_with,
};
use proptest::prelude::*;

fn canon(v: &Value) -> String {
    String::from_utf8(canonicalize(v).unwrap()).unwrap()
}

#[test]
fn rfc8785_appendix_b_number_formatting() {
    let cases: [(u64, &str); 13] = [
        (0x0000_0000_0000_0000, "0"),
        (0x8000_0000_0000_0000, "0"),
        (0x4340_0000_0000_0000, "9007199254740992"),
        (0x4430_0000_0000_0000, "295147905179352830000"),
        (0x44b5_2d02_c7e1_4af5, "9.999999999999997e+22"),
        (0x44b5_2d02_c7e1_4af6, "1e+23"),
        (0x44b5_2d02_c7e1_4af7, "1.0000000000000001e+23"),
        (0x444b_1ae4_d6e2_ef4e, "999999999999999700000"),
        (0x444b_1ae4_d6e2_ef4f, "999999999999999900000"),
        (0x444b_1ae4_d6e2_ef50, "1e+21"),
        (0x3eb0_c6f7_a0b5_ed8c, "9.999999999999997e-7"),
        (0x3eb0_c6f7_a0b5_ed8d, "0.000001"),
        (0x0000_0000_0000_0001, "5e-324"),
    ];
    for (bits, expected) in cases {
        assert_eq!(
            canon(&Value::Float(f64::from_bits(bits))),
            expected,
            "{bits:#x}"
        );
    }
    assert_eq!(canon(&Value::Float(f64::MAX)), "1.7976931348623157e+308");
    assert_eq!(canon(&Value::Float(-4.5)), "-4.5");
    assert_eq!(
        canon(&Value::Float(333_333_333.333_333_2)),
        "333333333.3333332"
    );
    assert!(canonicalize(&Value::Float(f64::NAN)).is_err());
    assert!(canonicalize(&Value::Float(f64::INFINITY)).is_err());
}

#[test]
fn integers_are_exact_within_the_safe_range_only() {
    assert_eq!(canon(&Value::Int(MAX_SAFE_INT)), "9007199254740991");
    assert_eq!(canon(&Value::Int(-MAX_SAFE_INT)), "-9007199254740991");
    assert_eq!(
        canonicalize(&Value::Int(MAX_SAFE_INT + 1)),
        Err(JsonError::IntegerOutOfRange)
    );
    assert_eq!(parse(b"9007199254740991"), Ok(Value::Int(MAX_SAFE_INT)));
    assert_eq!(
        parse(b"9007199254740992"),
        Err(JsonError::IntegerOutOfRange)
    );
    assert_eq!(
        parse(b"-9007199254740992"),
        Err(JsonError::IntegerOutOfRange)
    );
    assert_eq!(
        parse(b"99999999999999999999999"),
        Err(JsonError::IntegerOutOfRange)
    );
}

#[test]
fn string_escaping_follows_rfc8785() {
    let v = Value::Str("\u{0}\u{1f}\u{8}\t\n\u{c}\r\"\\/\u{7f}\u{2028}é😀".into());
    assert_eq!(
        canon(&v),
        "\"\\u0000\\u001f\\b\\t\\n\\f\\r\\\"\\\\/\u{7f}\u{2028}é😀\""
    );
    // Escapes in the input are decoded, not preserved.
    assert_eq!(
        parse(br#""\u0041\ud83d\ude00\/""#),
        Ok(Value::Str("A😀/".into()))
    );
}

#[test]
fn keys_sort_by_utf16_code_units_not_code_points() {
    // U+1F600 is the pair D83D DE00; U+FFFD is a single unit FFFD, which sorts after D83D.
    let v = parse("{\"\u{fffd}\":1,\"\u{1f600}\":2,\"b\":3,\"a\":4,\"\":5}".as_bytes()).unwrap();
    assert_eq!(
        canon(&v),
        "{\"\":5,\"a\":4,\"b\":3,\"\u{1f600}\":2,\"\u{fffd}\":1}"
    );
}

#[test]
fn strict_parse_rejects_what_ijson_forbids() {
    let reject: &[(&[u8], &str)] = &[
        (br#"{"a":1,"a":2}"#, "duplicate key"),
        (br#"{"a":{"b":1,"b":1}}"#, "nested duplicate key"),
        (br#"{"a":1,"\u0061":2}"#, "duplicate key through an escape"),
        (br#""\ud800""#, "lone high surrogate"),
        (br#""\udc00""#, "lone low surrogate"),
        (br#""\ud800\u0041""#, "high surrogate then a non-surrogate"),
        (br#""\ud800x""#, "high surrogate then text"),
        (br#""\udc00\ud800""#, "reversed pair"),
        (b"1.0", "fraction"),
        (b"1e3", "exponent"),
        (b"1E3", "exponent"),
        (b"-0", "negative zero"),
        (b"0.5", "fraction"),
        (b"01", "leading zero"),
        (b"+1", "plus sign"),
        (b"1.", "bare dot"),
        (b".5", "bare fraction"),
        (b"NaN", "NaN"),
        (b"Infinity", "Infinity"),
        (b"[1,]", "trailing comma in array"),
        (br#"{"a":1,}"#, "trailing comma in object"),
        (b"[1 2]", "missing comma"),
        (b"{'a':1}", "single quotes"),
        (b"{a:1}", "bare key"),
        (b"[] []", "trailing value"),
        (b"", "empty input"),
        (b"\"a\nb\"", "literal newline in a string"),
        (b"\"a\tb\"", "literal tab in a string"),
        (br#""\x41""#, "bad escape"),
        (br#""\u12""#, "short unicode escape"),
        (b"\"unterminated", "unterminated string"),
        (b"\xff\xfe", "invalid UTF-8"),
        (b"\"\xc3\x28\"", "invalid UTF-8 inside a string"),
        (b"\xef\xbb\xbf{}", "byte order mark"),
    ];
    for (input, why) in reject {
        assert!(parse(input).is_err(), "should reject {why}: {input:?}");
    }
    let too_deep = format!("{}{}", "[".repeat(MAX_DEPTH + 1), "]".repeat(MAX_DEPTH + 1));
    assert_eq!(parse(too_deep.as_bytes()), Err(JsonError::TooDeep));
    let ok_deep = format!("{}{}", "[".repeat(MAX_DEPTH), "]".repeat(MAX_DEPTH));
    assert!(parse(ok_deep.as_bytes()).is_ok());
}

#[test]
fn any_mode_accepts_floats_and_still_rejects_the_rest() {
    assert_eq!(parse_with(b"1.5", Numbers::Any), Ok(Value::Float(1.5)));
    assert_eq!(parse_with(b"1e3", Numbers::Any), Ok(Value::Float(1000.0)));
    assert!(parse_with(b"1e999", Numbers::Any).is_err());
    assert!(parse_with(br#"{"a":1,"a":1}"#, Numbers::Any).is_err());
    assert_eq!(parse(b"1.5"), Err(JsonError::NonInteger));
}

#[test]
fn check_canonical_demands_exact_bytes() {
    assert_eq!(check_canonical(br#"{"a":1,"b":[true,null]}"#), Ok(()));
    assert_eq!(
        check_canonical(br#"{"b":1,"a":2}"#),
        Err(JsonError::NotCanonical)
    );
    assert_eq!(
        check_canonical(br#"{"a": 1}"#),
        Err(JsonError::NotCanonical)
    );
    assert_eq!(
        check_canonical(b"{\"a\":\"\\u00e9\"}"),
        Err(JsonError::NotCanonical)
    );
}

// ───────────────────────── property tests against the oracle ─────────────────────────

fn arb_value() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        (-MAX_SAFE_INT..=MAX_SAFE_INT).prop_map(Value::Int),
        any::<f64>()
            .prop_filter("finite", |f| f.is_finite())
            .prop_map(Value::Float),
        any::<String>().prop_map(Value::Str),
    ];
    leaf.prop_recursive(5, 48, 6, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..6).prop_map(Value::Array),
            prop::collection::btree_map(any::<String>(), inner, 0..6)
                .prop_map(|m: BTreeMap<String, Value>| Value::Object(m)),
        ]
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    /// Our canonical bytes equal the oracle's for any value in the oracle's range.
    #[test]
    fn canonical_output_matches_the_oracle(v in arb_value()) {
        let oracle_input = serde_json::to_value(&v).unwrap();
        let expected = serde_json_canonicalizer::to_vec(&oracle_input).unwrap();
        prop_assert_eq!(canonicalize(&v).unwrap(), expected);
    }

    /// The same, starting from text: parse what a generic JSON writer wrote, canonicalize.
    #[test]
    fn parsed_text_canonicalizes_like_the_oracle(v in arb_value()) {
        let sj = serde_json::to_value(&v).unwrap();
        let text = serde_json::to_vec(&sj).unwrap();
        let ours = canonicalize(&parse_with(&text, Numbers::Any).unwrap()).unwrap();
        prop_assert_eq!(ours, serde_json_canonicalizer::to_vec(&sj).unwrap());
    }

    /// Canonical output is a fixed point: it parses (integers only) and canonicalizes to itself.
    #[test]
    fn canonical_form_is_a_fixed_point(v in arb_value().prop_filter("integers only", no_floats)) {
        let bytes = canonicalize(&v).unwrap();
        prop_assert_eq!(check_canonical(&bytes), Ok(()));
        prop_assert_eq!(parse(&bytes).unwrap(), v);
    }
}

fn no_floats(v: &Value) -> bool {
    match v {
        Value::Float(_) => false,
        Value::Array(a) => a.iter().all(no_floats),
        Value::Object(m) => m.values().all(no_floats),
        _ => true,
    }
}
