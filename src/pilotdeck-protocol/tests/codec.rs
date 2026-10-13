use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};

fn encoded(value: &JsValue) -> String {
    stringify_json(value)
        .expect("serializable value")
        .expect("defined value")
}

#[test]
fn js_numbers_preserve_binary64_and_ecmascript_format() {
    for (raw, expected) in [
        ("1.0", "1"),
        ("-0", "0"),
        ("1e400", "null"),
        ("-1e400", "null"),
        ("1e-400", "0"),
        ("9007199254740993", "9007199254740992"),
        ("1e21", "1e+21"),
        ("1e20", "100000000000000000000"),
        ("1e-6", "0.000001"),
        ("1e-7", "1e-7"),
        ("5e-324", "5e-324"),
        ("1.7976931348623157e308", "1.7976931348623157e+308"),
        ("1000000000000000128", "1000000000000000100"),
        ("333333333.33333329", "333333333.3333333"),
    ] {
        assert_eq!(encoded(&parse_json(raw).expect(raw)), expected, "{raw}");
    }
    assert!(
        parse_json("-0")
            .unwrap()
            .as_number()
            .unwrap()
            .is_sign_negative()
    );
    assert_eq!(encoded(&JsValue::Number(f64::NAN)), "null");
    let giant_exponent = format!("1e{}", "9".repeat(10_000));
    assert_eq!(encoded(&parse_json(&giant_exponent).unwrap()), "null");
}

#[test]
fn utf16_escape_and_raw_unicode_roundtrip_without_replacement() {
    let value = parse_json(r#"["\ud800","\udc00","\ud800A","\ud800\ud800","\ud83d\ude00","😀","\u0000\b\t\n\f\r\"\\\/"]"#).unwrap();
    assert_eq!(
        encoded(&value),
        "[\"\\ud800\",\"\\udc00\",\"\\ud800A\",\"\\ud800\\ud800\",\"😀\",\"😀\",\"\\u0000\\b\\t\\n\\f\\r\\\"\\\\/\"]"
    );
    let items = match &value {
        JsValue::Array(items) => items,
        _ => panic!("array"),
    };
    assert!(items[0].as_string().unwrap().to_utf8().is_err());
    assert_eq!(items[4].as_string().unwrap().len_utf16(), 2);
    assert_eq!(parse_json(&encoded(&value)).unwrap(), value);
    assert_eq!(
        encoded(&JsValue::String(JsString::from_utf16(vec![0x2028, 0x2029]))),
        "\"\u{2028}\u{2029}\""
    );
}

#[test]
fn duplicate_keys_keep_first_slot_and_last_value_including_surrogates() {
    let value = parse_json(r#"{"b":1,"2":2,"a":3,"b":4,"02":5,"0":6,"4294967294":7,"4294967295":8,"-0":9,"\ud800":10,"\ud800":11}"#).unwrap();
    assert_eq!(
        encoded(&value),
        r#"{"0":6,"2":2,"4294967294":7,"b":4,"a":3,"02":5,"4294967295":8,"-0":9,"\ud800":11}"#
    );
    assert_eq!(value.as_object().unwrap().len(), 9);
    assert!(value.as_object().unwrap().entries()[0].0.equals_str("b"));
    assert_eq!(value.clone(), value);
    assert_eq!(encoded(&value.clone()), encoded(&value));
}

#[test]
fn missing_undefined_null_have_distinct_presence_and_serialization() {
    let mut object = JsObject::new();
    object.insert("u".into(), JsValue::Undefined);
    object.insert("n".into(), JsValue::Null);
    object.insert(
        "arr".into(),
        JsValue::Array(vec![JsValue::Undefined, JsValue::Null]),
    );
    let value = JsValue::Object(object);
    assert!(value.get("missing").is_none());
    assert!(value.get("u").unwrap().is_undefined());
    assert_eq!(stringify_json(&JsValue::Undefined).unwrap(), None);
    assert_eq!(encoded(&value), r#"{"n":null,"arr":[null,null]}"#);
    let cloned = value.clone();
    assert!(cloned.get("u").unwrap().is_undefined());
    assert_eq!(cloned, value);
}

#[test]
fn parse_rejects_illegal_json_not_js_expression_syntax() {
    for raw in [
        "",
        " ",
        "null null",
        "01",
        "+1",
        "1.",
        ".1",
        "1e",
        "1e+",
        "NaN",
        "Infinity",
        "undefined",
        "[1,]",
        "{\"x\":1,}",
        "{x:1}",
        "{\"x\" 1}",
        "[}",
        "\"\\x00\"",
        "\"\\u00xz\"",
        "\"line\nbreak\"",
        "/*comment*/null",
        "\u{feff}null",
        "[true false]",
        "\"unterminated",
    ] {
        let error = parse_json(raw).expect_err(raw);
        assert!(error.offset <= raw.len(), "{raw:?}: {error}");
        assert!(!error.message.is_empty());
    }
    assert_eq!(
        encoded(&parse_json(" \r\n{\"x\": [ true, false, null ]}\t ").unwrap()),
        r#"{"x":[true,false,null]}"#
    );
}

#[test]
fn deeply_nested_valid_malformed_clone_debug_eq_drop_on_small_stack() {
    std::thread::Builder::new()
        .stack_size(128 * 1024)
        .spawn(|| {
            let depth = 30_000;
            let array = format!("{}0{}", "[".repeat(depth), "]".repeat(depth));
            let value = parse_json(&array).unwrap();
            assert_eq!(encoded(&value), array);
            let cloned = value.clone();
            assert_eq!(cloned, value);
            assert_eq!(format!("{cloned:?}"), array);
            drop(cloned);
            drop(value);
            let object = format!("{}null{}", "{\"x\":".repeat(depth), "}".repeat(depth));
            let value = parse_json(&object).unwrap();
            assert_eq!(encoded(&value), object);
            let cloned = value.clone();
            assert_eq!(cloned, value);
            drop(cloned);
            drop(value);
            let malformed = format!("{}{{\"bad\":]", "[".repeat(depth));
            assert!(parse_json(&malformed).is_err());
            // Duplicate replacement destroys an already deeply constructed value.
            let replacement = format!("{{\"a\":{array},\"a\":1}}");
            assert_eq!(encoded(&parse_json(&replacement).unwrap()), "{\"a\":1}");
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn large_inputs_have_no_unannounced_eight_mib_limit_or_quadratic_lookup() {
    let large = format!("\"{}\"", "x".repeat(9 * 1024 * 1024));
    assert_eq!(encoded(&parse_json(&large).unwrap()), large);
    let mut object = JsObject::new();
    for index in 0..20_000 {
        object.insert(
            format!("field-{index}").into(),
            JsValue::Number(f64::from(index)),
        );
    }
    let value = JsValue::Object(object);
    let raw = encoded(&value);
    let parsed = parse_json(&raw).unwrap();
    assert_eq!(parsed, value);
    assert_eq!(
        parsed.get("field-19999").unwrap().as_number(),
        Some(19999.0)
    );
    assert_eq!(parsed.clone(), parsed);
}

#[test]
fn deterministic_finite_random_numbers_roundtrip_exact_bits() {
    let mut state = 0x6a09e667f3bcc909u64;
    for _ in 0..20_000 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let value = f64::from_bits(state);
        if !value.is_finite() || value == 0.0 {
            continue;
        }
        let raw = encoded(&JsValue::Number(value));
        let parsed = parse_json(&raw).unwrap().as_number().unwrap();
        assert_eq!(
            parsed.to_bits(),
            value.to_bits(),
            "bits {state:016x}, {raw}"
        );
    }
}

#[test]
fn cloned_tree_is_independent_and_preserves_property_order() {
    let source = parse_json(r#"{"x":[{"a":1}],"10":10,"2":2}"#).unwrap();
    let clone = source.clone();
    assert_eq!(clone, source);
    assert_eq!(encoded(&clone), "{\"2\":2,\"10\":10,\"x\":[{\"a\":1}]}");
    let mut changed = clone;
    if let JsValue::Object(object) = &mut changed {
        object.insert("x".into(), JsValue::Bool(false));
    }
    assert_ne!(changed, source);
    assert_eq!(encoded(&source), "{\"2\":2,\"10\":10,\"x\":[{\"a\":1}]}");
}

#[test]
fn finite_random_numbers_match_real_node_binary64_json_oracle() {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut input = String::new();
    let mut expected = Vec::new();
    let mut state = 0x510e527fade682d1u64;
    for _ in 0..10_000 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let number = f64::from_bits(state);
        if !number.is_finite() || number == 0.0 {
            continue;
        }
        let raw = encoded(&JsValue::Number(number));
        input.push_str(&format!("{state:016x}|{raw}\n"));
        expected.push((state, raw));
    }
    let script = r#"
const fs = require('node:fs');
if (!process.version.startsWith('v22.')) throw new Error('Original oracle requires Node22');
const lines = fs.readFileSync(0, 'utf8').trim().split('\n');
for (const line of lines) {
  const [hex, supplied] = line.split('|');
  const bytes = Buffer.from(hex, 'hex');
  const original = bytes.readDoubleBE(0);
  const decoded = JSON.parse(supplied);
  const encoded = JSON.stringify(original);
  const roundtrip = Buffer.alloc(8); roundtrip.writeDoubleBE(decoded, 0);
  process.stdout.write(JSON.stringify([encoded, roundtrip.toString('hex')]) + '\n');
}
process.stderr.write('Original numeric oracle ' + process.version + ': ' + lines.length + ' samples\n');
"#;
    let mut child = Command::new("node")
        .args(["-e", script])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Node22 original-language test oracle must be available; no production Node codec");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    eprintln!("{}", String::from_utf8_lossy(&output.stderr));
    let actual = String::from_utf8(output.stdout).unwrap();
    let lines = actual.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), expected.len());
    for (line, (bits, ours)) in lines.iter().zip(expected) {
        // serde is safe for this test-only tuple of scalar UTF-8 strings.
        let (original, decoded_bits): (String, String) = serde_json::from_str(line).unwrap();
        assert_eq!(ours, original, "JSON.stringify for bits {bits:016x}");
        assert_eq!(
            decoded_bits,
            format!("{bits:016x}"),
            "Node JSON.parse({ours})"
        );
    }
}

#[test]
fn every_utf16_code_unit_matches_real_node_well_formed_json_stringify() {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let units = (0..=u16::MAX).collect::<Vec<_>>();
    let string = JsString::from_utf16(units.clone());
    let ours = encoded(&JsValue::String(string));
    let input = units
        .iter()
        .map(|unit| format!("{unit:04x}"))
        .collect::<String>();
    let script = r#"
const fs = require('node:fs');
if (!process.version.startsWith('v22.')) throw new Error('Original oracle requires Node22');
const hex = fs.readFileSync(0, 'utf8');
const chars = [];
for (let at = 0; at < hex.length; at += 4) chars.push(String.fromCharCode(parseInt(hex.slice(at, at + 4), 16)));
process.stdout.write(JSON.stringify(chars.join('')));
process.stderr.write('Original UTF16 oracle ' + process.version + ': ' + chars.length + ' code units\n');
"#;
    let mut child = Command::new("node")
        .args(["-e", script])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    eprintln!("{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(ours, String::from_utf8(output.stdout).unwrap());
    let parsed = parse_json(&ours).unwrap();
    assert_eq!(parsed.as_string().unwrap().units(), units);
}

#[test]
fn canonical_index_key_boundaries_are_not_generic_numeric_sorting() {
    let raw = r#"{"1e0":1,"+1":2,"-0":3,"00":4,"01":5,"4294967295":6,"4294967296":7,"1":8,"0":9,"4294967294":10,"000":11,"٠":12}"#;
    assert_eq!(
        encoded(&parse_json(raw).unwrap()),
        r#"{"0":9,"1":8,"4294967294":10,"1e0":1,"+1":2,"-0":3,"00":4,"01":5,"4294967295":6,"4294967296":7,"000":11,"٠":12}"#
    );
}

#[test]
fn clone_preserves_own_undefined_banned_field_before_wire_omission() {
    use pilotdeck_protocol::module::validate_module_message;
    let mut value = parse_json(r#"{"kind":"request","messageId":"m","method":"execute","runId":"r","operationId":"o","requestId":"q","payload":{}}"#).unwrap();
    if let JsValue::Object(object) = &mut value {
        object.insert("attemptId".into(), JsValue::Undefined);
    }
    let cloned = value.clone();
    assert_eq!(
        validate_module_message(&cloned).unwrap_err().code,
        "UNSUPPORTED_FIELD"
    );
    assert!(cloned.get("attemptId").unwrap().is_undefined());
    let transported = parse_json(&encoded(&cloned)).unwrap();
    assert!(transported.get("attemptId").is_none());
    assert!(validate_module_message(&transported).is_ok());
}
