//! #1333: replay the real TS `ApiServerChannel` cells (wire bytes over a
//! loopback listener) and the lifecycle matrix with a scripted A7 port.
//!
//! The external world is a mock `TurnGateway` only; no account, model or user
//! home is touched. Wire requests are raw HTTP/1.1 so the target the TS
//! generator sent (`//host/health`, `/v1/../health`) is reproduced verbatim.
use super::*;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::Notify,
};

const ORACLE: &str = include_str!("../../../tests/fixtures/api-server-channel/oracle.json");
const FIXED_UUID: &str = "00000000-0000-4000-8000-000000000000";
const FIXED_DATE: &str = "Thu, 01 Jan 1970 00:00:00 GMT";

fn oracle() -> Value {
    crate::test_baseline::oracle(ORACLE);
    serde_json::from_str(ORACLE).expect("#1333 api-server oracle JSON")
}

#[derive(Default, Debug)]
struct RawResponse {
    status: Option<u16>,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
    error: Option<String>,
}

/// `<N bytes>` stands for `N` repeated `x` bytes: the oracle records the
/// request *length*, not a megabyte of filler, so a replay that sends the
/// placeholder literally would exercise a 15-byte body instead of the cap.
/// Any string that is not exactly that shape is the request body itself.
///
/// Expansion is load-bearing, so it is fail-closed (#1606 r1/r2). The
/// `body-limit-exact` cell expects the same `400 invalid_json` for a 15-byte
/// body as for the full megabyte, so a placeholder that quietly failed to
/// expand would leave the cell green while testing nothing. A `<... bytes>`
/// whose middle does not parse is a fixture bug and panics.
fn recorded_body(text: &str) -> Vec<u8> {
    let Some(count) = text
        .strip_prefix('<')
        .and_then(|rest| rest.strip_suffix(" bytes>"))
    else {
        return text.as_bytes().to_vec();
    };
    // #1606 r4: `usize::from_str` also accepts a leading `+`, so "plain
    // decimal" was a promise the code did not quite keep. Reject the quirk
    // instead of documenting it — the placeholder is fixture protocol, so only
    // the canonical spelling should reach the byte count.
    if count.is_empty() || !count.bytes().all(|byte| byte.is_ascii_digit()) {
        panic!("#1333 request placeholder {text:?} is not a plain decimal byte count");
    }
    let count = count.parse::<usize>().unwrap_or_else(|error| {
        panic!("#1333 request placeholder {text:?} does not expand to a byte count: {error}")
    });
    assert_replay_budget_covers_the_cap();
    assert!(
        count <= REPLAY_BODY_BUDGET,
        "#1333 request placeholder {text:?} is beyond the replay budget"
    );
    vec![b'x'; count]
}

/// Allocation ceiling for a fixture-supplied byte count (#1606 r2). The
/// largest placeholder the oracle legitimately records is the over-cap probing
/// body, `MAX_REQUEST_BYTES + 1`; anything past that is a fixture bug, so it is
/// refused *before* `vec![b'x'; count]` can abort the whole test binary on
/// allocation instead of failing one assertion.
///
/// #1759 E: a **literal**, not `MAX_REQUEST_BYTES + 1`. Derived, the budget
/// moved with the `body-limit` negative control, which rewrites the cap in
/// `mod.rs` to `10_000_000`: the budget then became `10_000_001`, so
/// `recorded_body_refuses_an_unbounded_count` (`<9000000 bytes>`) stopped
/// panicking — the mutation that raises the cap switched off the very guard it
/// exists to be caught by. Pinned to the recorded over-cap length instead and
/// tied to the cap by `assert_replay_budget_covers_the_cap`, so raising the
/// product cap fails by name instead of the budget silently widening to match
/// it. A literal drifts the other way — nothing forces it to stay at the cap —
/// so `product_cap_is_pinned_to_the_oversized_body_literal` pins this end too.
const REPLAY_BODY_BUDGET: usize = 1_000_001;

/// #1759 E: the tie the literal above gives up. `REPLAY_BODY_BUDGET` must not
/// track `MAX_REQUEST_BYTES` (see its doc comment), so the relationship is
/// asserted instead of derived: a product cap raised past the budget is a
/// named failure, not a guard that quietly stops guarding.
fn assert_replay_budget_covers_the_cap() {
    let floor = MAX_REQUEST_BYTES + 1;
    assert!(
        floor <= REPLAY_BODY_BUDGET,
        "#1759 the product cap floor is {floor} bytes but `REPLAY_BODY_BUDGET` is only \
         {REPLAY_BODY_BUDGET}: the replay budget must cover `MAX_REQUEST_BYTES + 1`, otherwise \
         `recorded_body_refuses_an_unbounded_count` stops guarding anything"
    );
}

/// Pin each boundary cell's *expanded* body to the cap it exists to probe
/// (#1606 r2). The placeholder's syntax was already fail-closed, but its count
/// was unconstrained: `body-limit-exact` and the `oversized-*` cells all answer
/// exactly as a short body would (`400 invalid_json` / a zero-byte close), so
/// an oracle edited to `<15 bytes>` — or a generator that recorded the wrong
/// count — would leave every one of them green while proving nothing. Each
/// boundary cell must therefore replay the length it claims to test.
/// #1947: the id table is fail-closed too (the sms lane's #1984 shape) — a
/// request body that reaches the cap is a boundary cell even when its id was
/// never declared, so a renamed or newly added cell fails the replay instead
/// of replaying with no length pin at all (the old `_ => return` silent pass).
fn assert_boundary_body_len_checked(id: &str, case: &Value) -> Result<(), String> {
    // #1759 E: before the drift comparison, so a raised product cap is reported
    // as a raised cap instead of being misattributed to a broken fixture.
    assert_replay_budget_covers_the_cap();
    let expected = match id {
        "body-limit-exact" => MAX_REQUEST_BYTES,
        _ if id.starts_with("oversized-") => MAX_REQUEST_BYTES + 1,
        other => {
            // #1947 fail-closed: the switch exists so the negative control can
            // prove the guard rejects an undeclared boundary cell.
            let reaches_cap = case["requests"].as_array().is_some_and(|requests| {
                requests.iter().any(|request| {
                    request["body"]
                        .as_str()
                        .is_some_and(|text| recorded_body(text).len() >= MAX_REQUEST_BYTES)
                })
            });
            if reaches_cap && !crate::mutants::on("api-server-oracle-boundary-unregistered") {
                return Err(format!(
                    "#1333 {other}: records a request body that reaches the {MAX_REQUEST_BYTES}-byte \
                     body cap but is not declared in `assert_boundary_body_len`; declare it with \
                     its expected length"
                ));
            }
            return Ok(());
        }
    };
    let requests = case["requests"].as_array().expect("#1333 requests");
    if requests.len() != 1 {
        return Err(format!(
            "#1333 {id}: a boundary cell replays exactly one request"
        ));
    }
    let Some(text) = requests[0]["body"].as_str() else {
        return Err(format!("#1333 {id}: a boundary cell must record a body"));
    };
    let len = recorded_body(text).len();
    if len != expected {
        return Err(format!(
            "#1333 {id}: boundary cell must replay {expected} bytes (the product cap), not {len}; \
             the fixture and `MAX_REQUEST_BYTES` have drifted apart"
        ));
    }
    Ok(())
}

fn assert_boundary_body_len(id: &str, case: &Value) {
    assert_boundary_body_len_checked(id, case).unwrap_or_else(|error| panic!("{error}"));
}

/// #1683: pin the knobs of the two parser-admission cells, not just their
/// recorded response. If the generator ever degrades `unknown-method` back to
/// an allowed method, or drops `setHost: false` from `missing-host`, the cell
/// would become a plain `/health` 200 control: the replay would then compare
/// 200 == 200 and the admission gate could be removed without a red test.
fn assert_parser_admission_cell_records_its_knob(id: &str, case: &Value) {
    let requests = case["requests"].as_array().expect("#1333 requests");
    let request = requests
        .first()
        .unwrap_or_else(|| panic!("#1333 {id}: the cell must record its request"));
    match id {
        "unknown-method" => {
            let method = request["method"]
                .as_str()
                .unwrap_or_else(|| panic!("#1333 {id}: the request must record a method"));
            assert_eq!(
                method, "FROB",
                "#1333 {id}: the cell must record a method outside Node's `http.METHODS`"
            );
        }
        "missing-host" => assert_eq!(
            request["setHost"],
            Value::Bool(false),
            "#1333 {id}: the cell must record `setHost: false`, or the replay sends a Host \
             header and the request is admitted"
        ),
        other => panic!("#1333 {other}: not a parser-admission cell"),
    }
    let expected = case["expect"]
        .as_array()
        .and_then(|expect| expect.first())
        .unwrap_or_else(|| panic!("#1333 {id}: the cell must record its response"));
    assert_eq!(
        expected["status"].as_u64(),
        Some(400),
        "#1333 {id}: both parser rejections are a bare 400, got {expected}"
    );
}

#[test]
fn recorded_body_expands_the_length_placeholder() {
    assert_eq!(recorded_body("<5 bytes>"), b"xxxxx");
    // A literal, not `MAX_REQUEST_BYTES`: this test owns the placeholder
    // syntax. That the fixture's cap *is* the product cap is asserted where it
    // matters, by `assert_boundary_body_len` over the real oracle cells.
    assert_eq!(recorded_body("<1000000 bytes>").len(), 1_000_000);
    // Anything that is not the placeholder syntax is the body verbatim.
    assert_eq!(recorded_body("{}"), b"{}");
    assert_eq!(recorded_body(""), b"");
}

/// #1759 E: `oversized_body_closes_without_response` builds its body from the
/// literal `1_000_001` on purpose — the `body-limit` negative control rewrites
/// the cap in `mod.rs` and would otherwise move the body along with it, letting
/// the mutant survive. That independence is only honest while the literal
/// really is `MAX_REQUEST_BYTES + 1`, and nothing used to tie the two: a
/// legitimate cap change turned that test red and looked like a replay
/// regression. Pin the tie here, in a test the `body-limit` row does not
/// target, so the failure names the cap instead. `assert_boundary_body_len`
/// carries the same number into the oracle, whose `oversized-*` placeholders
/// record `<1000001 bytes>`. `REPLAY_BODY_BUDGET` is pinned to the same number
/// here for the mirror-image reason: it must not be derived from the cap, but
/// then nothing else stops it from drifting away.
#[test]
fn product_cap_is_pinned_to_the_oversized_body_literal() {
    // The literal cut the derived tie, so it needs one of its own: any value in
    // `[MAX_REQUEST_BYTES + 1, 9_000_000)` satisfies both
    // `assert_replay_budget_covers_the_cap` and the `<9000000 bytes>` ceiling,
    // so without this line the allocation ceiling could be widened ninefold
    // with every test still green.
    assert_eq!(
        REPLAY_BODY_BUDGET, 1_000_001,
        "#1759 `REPLAY_BODY_BUDGET` left the recorded over-cap length; it is a literal, so \
         nothing else holds it in place and a wider value silently weakens the ceiling \
         `recorded_body_refuses_an_unbounded_count` enforces"
    );
    assert_eq!(
        MAX_REQUEST_BYTES + 1,
        REPLAY_BODY_BUDGET,
        "#1759 `MAX_REQUEST_BYTES` moved away from the literal the oversized test and the \
         oracle were recorded against; re-record the fixture and update both ends together"
    );
}

/// #1947: the fail-closed id table — an undeclared boundary id whose recorded
/// body reaches the cap must be rejected, not skipped. The negative control
/// `api-server-oracle-boundary-unregistered` disarms exactly this branch, so
/// the old `_ => return` silent pass would let this assertion turn green under
/// mutation.
#[test]
fn recorded_boundary_body_length_rejects_unregistered_cell() {
    crate::negative_assert!(
        assert_boundary_body_len_checked(
            "body-limit-exact-renamed",
            &json!({"requests": [{"body": format!("<{MAX_REQUEST_BYTES} bytes>")}]})
        )
        .is_err(),
        "#1333 #1947 rejects an undeclared boundary cell"
    );
    // Non-boundary cells are not this guard's business: a small body under an
    // unknown id stays a pass.
    crate::negative_assert!(
        assert_boundary_body_len_checked(
            "body-limit-exact-renamed",
            &json!({"requests": [{"body": "{}"}]})
        )
        .is_ok(),
        "#1333 #1947 a body below the cap is not a boundary cell"
    );
}

#[test]
#[should_panic(expected = "request body must be a string")]
fn req_spec_refuses_a_non_string_body() {
    // `unwrap_or("")` here would replay an empty body against a cell that only
    // distinguishes bodies by "is this valid JSON".
    ReqSpec::from_json(&json!({
        "method": "POST",
        "path": "/v1/chat/completions",
        "body": 15,
    }));
}

#[test]
#[should_panic(expected = "must be a string")]
fn req_spec_refuses_a_non_string_header() {
    // `unwrap_or("")` here would send an empty `connection` and quietly turn a
    // keep-alive cell into a close cell.
    ReqSpec::from_json(&json!({
        "method": "GET",
        "path": "/health",
        "headers": { "connection": true },
    }));
}

#[test]
#[should_panic(expected = "request `setHost` must be a boolean")]
fn req_spec_refuses_a_non_boolean_set_host() {
    // #1683: `setHost` decides whether the replay writes the `Host:` line. A
    // truthy non-boolean (or a string) read as "present" would silently send a
    // Host header and turn the recorded missing-Host 400 into an admitted 200
    // control, which the replay would then compare against itself.
    ReqSpec::from_json(&json!({
        "method": "GET",
        "path": "/health",
        "setHost": "false",
    }));
}

#[test]
#[should_panic(expected = "request headers must be an object")]
fn req_spec_refuses_a_non_object_header_container() {
    // #1606 r4: r3 caught non-string header *values*; a `headers` that is not
    // an object at all still slipped through to "no headers", which silently
    // dropped a recorded `connection: keep-alive`.
    ReqSpec::from_json(&json!({
        "method": "GET",
        "path": "/health",
        "headers": [],
    }));
}

#[test]
#[should_panic(expected = "is not a plain decimal byte count")]
fn recorded_body_refuses_a_malformed_placeholder() {
    // A near-miss placeholder must not fall back to replaying 15 literal
    // bytes: that is the fake-PASS shape this cell exists to rule out.
    recorded_body("<1_000_000 bytes>");
}

#[test]
#[should_panic(expected = "is not a plain decimal byte count")]
fn recorded_body_refuses_a_signed_count() {
    // #1606 r4: `"+5".parse::<usize>()` is `Ok(5)`, so the old reader accepted
    // a spelling no generator writes.
    recorded_body("<+5 bytes>");
}

#[test]
#[should_panic(expected = "does not expand to a byte count")]
fn recorded_body_refuses_an_overflowing_count() {
    // All digits, so it reaches `parse` and fails there — the branch the
    // strict digit check above does not take.
    recorded_body("<99999999999999999999999999 bytes>");
}

#[test]
#[should_panic(expected = "is beyond the replay budget")]
fn recorded_body_refuses_an_unbounded_count() {
    // Parseable by a 32-bit `usize` too, so the test depends on the budget
    // rule and not on the target's pointer width.
    recorded_body("<9000000 bytes>");
}

// #1606 r6: the container-level tri-state, which the 15 oracle cells cannot
// exercise — every one of them records an `options` object, and the 12 cells
// without a gateway record none at all, so a regression to `unwrap_or_default`
// would still replay green.
#[test]
#[should_panic(expected = "options must be an object")]
fn options_refuses_a_non_object_container() {
    options(&json!({ "options": [] }), None);
}

// #1759: the same "the fixture cannot exercise it" argument for the key set.
// Every recorded key is one the reader maps, so a reader that ignored an
// unknown key would stay green on all 15 cells while the generator (which
// spreads the recorded options into the real TS channel) kept honouring it.
#[test]
#[should_panic(expected = "options key \"requestTimeout\" is not read")]
fn options_refuses_an_unknown_key() {
    options(&json!({ "options": { "requestTimeout": 1000 } }), None);
}

// #1759 r3 simplify P2: the roster is a *one-way* pin — it panics when the
// fixture records a key the reader does not know. The other direction (a key
// added to `REPLAY_OPTION_KEYS` and to `generate.mts`, but never mapped into
// `ApiServerOptions`) has no check, and that is exactly the hole the roster
// exists to close: TS honours the setting, Rust silently ignores it, the cell
// stays green. This test sets every roster key to a distinguishable value and
// requires each one to reach the parsed options.
#[test]
fn every_replay_option_key_is_mapped_into_the_channel_options() {
    let recorded = json!({
        "options": {
            "apiKey": "key-from-fixture",
            "modelName": "model-from-fixture",
            "corsOrigins": ["https://fixture.test"],
        }
    });
    let mut recorded_keys: Vec<&str> = recorded["options"]
        .as_object()
        .expect("#1333 test fixture")
        .keys()
        .map(String::as_str)
        .collect();
    recorded_keys.sort_unstable();
    let mut roster = REPLAY_OPTION_KEYS.to_vec();
    roster.sort_unstable();
    assert_eq!(
        recorded_keys, roster,
        "#1759: this test must set every key in `REPLAY_OPTION_KEYS`"
    );

    let parsed = options(&recorded, None);
    // The roster drives the assertions rather than a hand-written list next to
    // it: a fourth key added to `REPLAY_OPTION_KEYS` (and to this test's json)
    // must also gain a mapping assertion here, or this loop panics instead of
    // letting the new setting slide through unproven (r3 boundary P2).
    for key in REPLAY_OPTION_KEYS {
        match key {
            "apiKey" => assert_eq!(
                parsed.api_key,
                Some(JsValue::String(JsString::from("key-from-fixture"))),
                "#1759: `apiKey` must reach the channel options"
            ),
            "modelName" => assert_eq!(
                parsed.model_name.as_deref(),
                Some("model-from-fixture"),
                "#1759: `modelName` must reach the channel options"
            ),
            "corsOrigins" => assert_eq!(
                parsed.cors_origins,
                Some(js_string_array(&[json!("https://fixture.test")])),
                "#1759: `corsOrigins` must reach the channel options"
            ),
            other => panic!(
                "#1759: {other:?} is on REPLAY_OPTION_KEYS but this test has no mapping \
                 assertion for it; teach `options()` and this test before recording it"
            ),
        }
    }
}

#[test]
fn absent_containers_stay_legal() {
    // Absent means "the cell recorded no options / no gateway", which every
    // cell without those keys relies on.
    let options = options(&json!({}), None);
    assert_eq!(options.model_name.as_deref(), Some("parity-model"));
    assert_eq!(
        options.api_key,
        Some(JsValue::String(JsString::from(""))),
        "#1333 absent apiKey replays as the empty string the reader maps"
    );
    gateway_from(&json!({}), None, Arc::new(AtomicBool::new(false)));
}

#[test]
#[should_panic(expected = "gateway must be an object")]
fn gateway_from_refuses_a_non_object_gateway() {
    gateway_from(
        &json!({ "gateway": 5 }),
        None,
        Arc::new(AtomicBool::new(false)),
    );
}

#[test]
#[should_panic(expected = "gateway.turns must be an array")]
fn gateway_from_refuses_non_array_turns() {
    gateway_from(
        &json!({ "gateway": { "turns": "x" } }),
        None,
        Arc::new(AtomicBool::new(false)),
    );
}

#[test]
#[should_panic(expected = "gateway turn events must be an array")]
fn gateway_from_refuses_non_array_events() {
    gateway_from(
        &json!({ "gateway": { "turns": [{ "events": "x" }] } }),
        None,
        Arc::new(AtomicBool::new(false)),
    );
}

#[test]
#[should_panic(expected = "gateway turn error must be a string")]
fn gateway_from_refuses_a_non_string_error() {
    gateway_from(
        &json!({ "gateway": { "turns": [{ "events": [], "error": 7 }] } }),
        None,
        Arc::new(AtomicBool::new(false)),
    );
}

// #1759: the element itself. `Value::get` on `5` answers `None` for every
// field, so `"turns": [5]` used to become an empty turn — the outer
// `gateway` / `turns` containers were tri-state, the elements were not.
#[test]
#[should_panic(expected = "gateway turn must be an object")]
fn gateway_from_refuses_a_non_object_turn() {
    gateway_from(
        &json!({ "gateway": { "turns": [5] } }),
        None,
        Arc::new(AtomicBool::new(false)),
    );
}

// #1606 r6 (MiniMax sibling): r4/r5 added three more fail-closed readers with
// no test of their own. Reverting any of them to `unwrap_or` keeps every cell
// green — the three cells that record CORS / an api key record the
// correct types — so nothing else would notice the ruler going quiet.
#[test]
#[should_panic(expected = "options.apiKey must be a string")]
fn case_option_refuses_a_non_string_api_key() {
    options(&json!({ "options": { "apiKey": 7 } }), None);
}

#[test]
#[should_panic(expected = "options.modelName must be a string")]
fn case_option_refuses_a_non_string_model_name() {
    options(
        &json!({ "options": { "modelName": ["parity-model"] } }),
        None,
    );
}

#[test]
#[should_panic(expected = "options.corsOrigins must be an array")]
fn case_origins_refuses_a_non_array() {
    options(&json!({ "options": { "corsOrigins": "*" } }), None);
}

#[test]
#[should_panic(expected = "corsOrigins entries must be strings")]
fn case_origins_refuses_a_non_string_entry() {
    options(
        &json!({ "options": { "corsOrigins": ["http://a", 5] } }),
        None,
    );
}

struct ReqSpec {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    keep_alive: bool,
    /// #1683: `false` when the oracle recorded a request sent with Node's
    /// `http.request` `setHost: false`, i.e. the HTTP/1.1 request llhttp's
    /// server parser rejects for a missing `Host`. The replay omits the same
    /// header; sending one would turn the cell into a plain 200 control.
    set_host: bool,
}

impl ReqSpec {
    fn from_json(value: &Value) -> Self {
        // #1606 r3: headers were the last field still parsed with
        // `unwrap_or("")` while `body` / `method` / `path` had already been
        // made fail-closed. A non-string header value (say
        // `"connection": true`) silently replayed as an empty header and
        // flipped `keep_alive` to false, so the request that reached the
        // server was not the one the oracle recorded.
        // #1606 r4 (both families): r3 tightened the header *values* but left
        // the *container* fail-open — `value["headers"].as_object()` on a
        // `[]` / `"x"` / `null` returned `None` and `unwrap_or_default()`
        // silently replayed a request with no headers at all, the same
        // "the request that reached the server is not the recorded one" shape.
        // Absent stays legitimate (GET cells may record none); anything else
        // must be an object of strings.
        let headers: Vec<(String, String)> = match value.get("headers") {
            None => Vec::new(),
            Some(Value::Object(headers)) => headers
                .iter()
                .map(|(name, value)| {
                    let value = value.as_str().unwrap_or_else(|| {
                        panic!("#1333 header {name:?} must be a string, found {value:?}")
                    });
                    (name.clone(), value.to_owned())
                })
                .collect(),
            Some(other) => {
                panic!("#1333 request headers must be an object, found {other:?}")
            }
        };
        // #1606 r2: a missing `body` is legitimate (GET/HEAD cells), but a
        // present non-string one is a fixture bug: `unwrap_or("")` would
        // replay an empty body and a boundary cell would stay green.
        let body = match value.get("body") {
            Some(Value::String(text)) => recorded_body(text),
            Some(other) => panic!("#1333 request body must be a string, found {other:?}"),
            None => Vec::new(),
        };
        // Read back off the parsed headers rather than the raw JSON, so the
        // two can never disagree about which connection mode was requested.
        let keep_alive = headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("connection") && value.eq_ignore_ascii_case("keep-alive")
        });
        // #1606 r2: every oracle request carries both fields, so a missing one
        // is fixture drift, not a defaultable cell.
        let method = value["method"]
            .as_str()
            .unwrap_or_else(|| panic!("#1333 request is missing a string `method`: {value}"));
        let path = value["path"]
            .as_str()
            .unwrap_or_else(|| panic!("#1333 request is missing a string `path`: {value}"));
        // #1683: absence is the normal request (a `Host` is written), so only
        // a recorded boolean may suppress it. Anything else is fixture drift
        // that would otherwise replay a differently-framed request.
        let set_host = match value.get("setHost") {
            None => true,
            Some(Value::Bool(set_host)) => *set_host,
            Some(other) => panic!("#1333 request `setHost` must be a boolean, found {other:?}"),
        };
        Self {
            method: method.to_owned(),
            path: path.to_owned(),
            headers,
            body,
            keep_alive,
            set_host,
        }
    }

    fn post(path: &str, chat: &str, body: &str) -> Self {
        Self {
            method: "POST".to_owned(),
            path: path.to_owned(),
            headers: vec![("x-hermes-session-id".to_owned(), chat.to_owned())],
            body: body.as_bytes().to_vec(),
            keep_alive: false,
            set_host: true,
        }
    }

    fn get(path: &str) -> Self {
        Self {
            method: "GET".to_owned(),
            path: path.to_owned(),
            headers: Vec::new(),
            body: Vec::new(),
            keep_alive: false,
            set_host: true,
        }
    }
}

fn parse_head(data: &[u8]) -> Option<(usize, u16, BTreeMap<String, String>)> {
    let end = data.windows(4).position(|window| window == b"\r\n\r\n")? + 4;
    let head = std::str::from_utf8(&data[..end]).ok()?;
    let mut lines = head.split("\r\n");
    let status = lines.next()?.split(' ').nth(1)?.parse().ok()?;
    let mut headers = BTreeMap::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        let (name, value) = line.split_once(':')?;
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
    }
    Some((end, status, headers))
}

fn dechunk(mut body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(line_end) = body.windows(2).position(|window| window == b"\r\n") {
        let Ok(size) =
            usize::from_str_radix(std::str::from_utf8(&body[..line_end]).unwrap_or("0"), 16)
        else {
            break;
        };
        body = &body[line_end + 2..];
        if size == 0 {
            break;
        }
        let take = size.min(body.len());
        out.extend_from_slice(&body[..take]);
        body = &body[take..];
        if body.starts_with(b"\r\n") {
            body = &body[2..];
        }
    }
    out
}

fn complete(data: &[u8], method: &str) -> bool {
    let Some((end, status, headers)) = parse_head(data) else {
        return false;
    };
    if method.eq_ignore_ascii_case("HEAD") || status == 204 {
        return true;
    }
    if headers
        .get("transfer-encoding")
        .map(|value| value.to_ascii_lowercase().contains("chunked"))
        .unwrap_or(false)
    {
        return data.ends_with(b"\r\n0\r\n\r\n");
    }
    match headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
    {
        Some(length) => data.len() >= end + length,
        None => false,
    }
}

async fn exchange(addr: std::net::SocketAddr, spec: &ReqSpec) -> RawResponse {
    match tokio::time::timeout(Duration::from_secs(10), exchange_inner(addr, spec)).await {
        Ok(response) => response,
        Err(_) => RawResponse {
            error: Some("timeout".to_owned()),
            ..RawResponse::default()
        },
    }
}

async fn exchange_inner(addr: std::net::SocketAddr, spec: &ReqSpec) -> RawResponse {
    let mut stream = match TcpStream::connect(addr).await {
        Ok(stream) => stream,
        Err(error) => {
            return RawResponse {
                error: Some(error.to_string()),
                ..RawResponse::default()
            };
        }
    };
    let mut request = format!("{} {} HTTP/1.1\r\n", spec.method, spec.path);
    // #1683: the missing-Host cell was recorded with `setHost: false`; writing
    // the default `Host:` here would replay a different (admitted) request.
    if spec.set_host {
        request.push_str(&format!("Host: {addr}\r\n"));
    }
    let mut has_length = false;
    for (name, value) in &spec.headers {
        if name.eq_ignore_ascii_case("host") || name.eq_ignore_ascii_case("content-length") {
            has_length |= name.eq_ignore_ascii_case("content-length");
        }
        if name.eq_ignore_ascii_case("connection") && !spec.keep_alive {
            continue;
        }
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    if !spec.keep_alive
        && !spec
            .headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("connection"))
    {
        request.push_str("Connection: close\r\n");
    }
    if !has_length && !spec.body.is_empty() {
        request.push_str(&format!("Content-Length: {}\r\n", spec.body.len()));
    }
    request.push_str("\r\n");
    let mut wire = request.into_bytes();
    wire.extend_from_slice(&spec.body);
    let write = async {
        if let Err(error) = stream.write_all(&wire).await {
            return Some(error.to_string());
        }
        None
    };
    let write_error = write.await;

    let mut data = Vec::new();
    let mut buffer = [0u8; 16 * 1024];
    loop {
        if complete(&data, &spec.method) {
            break;
        }
        match stream.read(&mut buffer).await {
            Ok(0) => break,
            Ok(count) => data.extend_from_slice(&buffer[..count]),
            Err(error) => {
                return RawResponse {
                    error: Some(error.to_string()),
                    body: data,
                    ..RawResponse::default()
                };
            }
        }
        if data.len() > 4_000_000 {
            break;
        }
    }
    let Some((end, status, headers)) = parse_head(&data) else {
        return RawResponse {
            error: Some(write_error.unwrap_or_else(|| "connection closed".to_owned())),
            body: data,
            ..RawResponse::default()
        };
    };
    let mut body = data[end..].to_vec();
    if spec.method.eq_ignore_ascii_case("HEAD") || status == 204 {
        body.clear();
    } else if headers
        .get("transfer-encoding")
        .map(|value| value.to_ascii_lowercase().contains("chunked"))
        .unwrap_or(false)
    {
        body = dechunk(&body);
    } else if let Some(length) = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
    {
        body.truncate(length);
    }
    RawResponse {
        status: Some(status),
        headers,
        body,
        error: None,
    }
}

fn mask_text(text: &str) -> String {
    let mut ids = String::new();
    let mut rest = text;
    while let Some(position) = rest.find("\"id\":\"chatcmpl-") {
        ids.push_str(&rest[..position]);
        ids.push_str("\"id\":\"chatcmpl-FIXED\"");
        let after = &rest[position + "\"id\":\"chatcmpl-".len()..];
        let digits = after.bytes().take_while(u8::is_ascii_digit).count();
        rest = &after[digits..];
        if let Some(stripped) = rest.strip_prefix('"') {
            rest = stripped;
        }
    }
    ids.push_str(rest);
    let mut created = String::new();
    let mut rest = ids.as_str();
    while let Some(position) = rest.find("\"created\":") {
        created.push_str(&rest[..position]);
        created.push_str("\"created\":0");
        let after = &rest[position + "\"created\":".len()..];
        let digits = after.bytes().take_while(u8::is_ascii_digit).count();
        rest = &after[digits..];
    }
    created.push_str(rest);
    created.replace(&format!("api-{FIXED_UUID}"), "api-FIXED")
}

/// #1683: `parser_admission` scopes the one registered transport difference to
/// the two cells that actually exhibit it. hyper adds `content-length: 0` to
/// the parser-level bare 400 where Node writes no length at all (the same
/// registered diff the native-http and webhook lanes already normalize —
/// A8.4-NATIVE-HTTP-001, extended to this lane by A8.3-API-SERVER-005). The
/// ledger entry limits that exemption to the bare 400, so a mask applied to
/// every cell would let an unrelated regression (say, hyper starting to emit
/// `content-length: 0` on a `HEAD` the baseline answers without one) compare
/// green. The replay's raw-byte pin
/// (`parser_rejections_keep_the_registered_transport_shape`) asserts the line
/// really is there, so the masking cannot hide a missing rejection either.
fn actual_headers(response: &RawResponse, parser_admission: bool) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (name, value) in &response.headers {
        if parser_admission && name == "content-length" && value == "0" {
            continue;
        }
        out.insert(
            name.clone(),
            if name == "date" {
                FIXED_DATE.to_owned()
            } else {
                mask_text(value)
            },
        );
    }
    out
}

/// #1759: one rule for every recorded expectation, selected by *shape* rather
/// than by cell id. r8 made the generator record the measured byte count and
/// any status line it saw on the error path, but the pins that read those
/// fields lived only in the `oversized-*` branch — a renamed or newly added
/// error cell would leave the measurements uncompared. `bytes` is a
/// measurement: a clean close delivered zero bytes and no status line, so any
/// other shape is fixture (or baseline) drift and panics here.
///
/// The entry itself is tri-state like every other reader in this file: a
/// non-object is a fixture bug, not an expectation with "no error key". And
/// the two recorded shapes are mutually exclusive — an error close has no
/// status line, an answered request has one — so a cell cannot silently lose
/// its `error` key and leave the rest uncompared (#1759 r1 boundary P1).
fn assert_recorded_error_shape(case: &str, index: usize, expected: &Value) {
    assert!(
        expected.is_object(),
        "#1333 {case} request {index}: a recorded expectation must be an object, got {expected}"
    );
    if expected.get("error").is_none() {
        // `Value::get` answers `Some(Null)` for an explicit `"status": null`,
        // and `compare_response` reads the status with `as_u64()`, so a key
        // that merely exists would let a null status compare equal to a
        // connection close (#1759 r2 boundary P1). Pin the value, not the key.
        let status = match expected.get("status").and_then(Value::as_u64) {
            Some(status) => status,
            None => panic!(
                "#1333 {case} request {index}: a recorded expectation with no `error` must carry \
                 a numeric status line, got {expected}"
            ),
        };
        // `compare_response` narrows the recorded status with `as u16`, so a
        // recorded value outside that range would alias a real one and compare
        // equal; zero is not a status line either (#1759 r4 boundary P3).
        assert!(
            (1..=u64::from(u16::MAX)).contains(&status),
            "#1333 {case} request {index}: a recorded status line must fit the `u16` the \
             comparison narrows to, got {expected}"
        );
        // Cells that skip the replay (`body-read-timeout`) read this map
        // directly, so a missing `headers` would make their `date` pin a
        // vacuous `None` instead of a comparison (#1759 r2 sibling P3).
        assert!(
            expected["headers"].is_object(),
            "#1333 {case} request {index}: a recorded response must carry a headers object, \
             got {expected}"
        );
        return;
    }
    assert!(
        expected["error"].is_string(),
        "#1333 {case} request {index}: a recorded connection error must be a string, got {expected}"
    );
    // A missing `bytes` key and a non-zero count are different drifts: one is
    // a recording that lost its measurement, the other a partially delivered
    // response. `Value::get` on the missing key answers `None`, which used to
    // panic with the "partially delivered" wording either way
    // (#1759 r5 boundary P3).
    assert!(
        expected.get("bytes").is_some(),
        "#1333 {case} request {index}: a recorded connection error must carry the measured \
         `bytes` count, got {expected}"
    );
    assert_eq!(
        expected["bytes"].as_u64(),
        Some(0),
        "#1333 {case} request {index}: a recorded connection error must be a zero-byte close \
         (a partially delivered response is not a clean close), got {expected}"
    );
    assert!(
        expected.get("status").is_none(),
        "#1333 {case} request {index}: a recorded connection error must not carry a status \
         line, got {expected}"
    );
    // The generator's oversized-cell shape check (`generate.mts`) records an
    // error close as exactly `{error, bytes}` and refuses to record anything
    // else there, so a recorded close that grew a headers map is drift, not a
    // shape to tolerate; leaving it open made the two shapes non-exclusive
    // (#1759 r4 boundary P2). Other error paths record `{error, bytes, status}`
    // without a gate, and this reader refuses those here.
    assert!(
        expected.get("headers").is_none(),
        "#1333 {case} request {index}: a recorded connection error must not carry headers, \
         got {expected}"
    );
}

/// #1759 r4 (boundary P0): the `oversized-*` cells are skipped by the replay,
/// so this is the only place their recorded responses are read. `Iterator::all`
/// is vacuously true on an empty slice, so a cell that lost every expectation
/// satisfied the claim below while the whole surface stayed green. Require one
/// recorded outcome per request first, then the connection-error claim itself.
fn assert_oversized_cell_records_only_connection_errors(id: &str, case: &Value) {
    let requests = case["requests"]
        .as_array()
        .expect("#1333 oversized cell requests");
    let expectations = case["expect"]
        .as_array()
        .expect("#1333 oversized cell expect");
    assert!(
        !expectations.is_empty(),
        "#1333 {id}: an oversized cell must record at least one expectation, got \
         {expectations:?}"
    );
    assert_eq!(
        expectations.len(),
        requests.len(),
        "#1333 {id}: an oversized cell must record one expectation per request, got \
         {expectations:?}"
    );
    assert!(
        expectations
            .iter()
            .all(|expected| matches!(expected.get("error"), Some(Value::String(_)))),
        "#1333 {id}: an oversized body must record a connection error, not a response, \
         got {expectations:?}"
    );
}

// #1759 r2 (boundary P1 / sibling P3): the two shapes the mutual-exclusion pin
// exists to refuse. `#[should_panic]` is the point here — the assertion is the
// product, and these tests would turn red the moment either check is loosened
// back to "the key exists".
#[test]
#[should_panic(expected = "must carry a numeric status line")]
fn recorded_expectation_refuses_a_null_status() {
    assert_recorded_error_shape("cell", 0, &json!({ "status": null, "headers": {} }));
}

#[test]
#[should_panic(expected = "must carry a headers object")]
fn recorded_expectation_refuses_a_missing_headers_object() {
    assert_recorded_error_shape("cell", 0, &json!({ "status": 200 }));
}

// #1759 r3 sibling P2: the other four panic points of the same reader. Without
// these, loosening `bytes == 0` or `is_string()` back to "the key exists"
// keeps all 15 cells green — the real oracle never records a bad shape, so
// only a direct call can tell the two apart (the same argument r4/r5 used for
// the request readers).
#[test]
#[should_panic(expected = "a recorded expectation must be an object")]
fn recorded_expectation_refuses_a_non_object() {
    assert_recorded_error_shape("cell", 0, &json!("ECONNRESET"));
}

#[test]
#[should_panic(expected = "a recorded connection error must be a string")]
fn recorded_expectation_refuses_a_non_string_error() {
    assert_recorded_error_shape("cell", 0, &json!({ "error": 5, "bytes": 0 }));
}

#[test]
#[should_panic(expected = "a recorded connection error must be a zero-byte close")]
fn recorded_expectation_refuses_a_nonzero_byte_close() {
    assert_recorded_error_shape("cell", 0, &json!({ "error": "ECONNRESET", "bytes": 1 }));
}

#[test]
#[should_panic(expected = "a recorded connection error must not carry a status line")]
fn recorded_expectation_refuses_a_status_on_an_error_close() {
    assert_recorded_error_shape(
        "cell",
        0,
        &json!({ "error": "ECONNRESET", "bytes": 0, "status": 200 }),
    );
}

#[test]
#[should_panic(expected = "must not carry headers")]
fn recorded_expectation_refuses_headers_on_a_clean_close() {
    assert_recorded_error_shape(
        "cell",
        0,
        &json!({ "error": "ECONNRESET", "bytes": 0, "headers": {} }),
    );
}

// #1759 r4 boundary P3: `compare_response` narrows the recorded status with
// `as u16`, so an out-of-range recording would alias a real status and compare
// equal. Neither value is a status line the baseline can produce.
#[test]
#[should_panic(expected = "must fit the `u16` the comparison narrows to")]
fn recorded_expectation_refuses_a_status_above_the_u16_range() {
    assert_recorded_error_shape("cell", 0, &json!({ "status": 65_792, "headers": {} }));
}

#[test]
#[should_panic(expected = "must fit the `u16` the comparison narrows to")]
fn recorded_expectation_refuses_a_zero_status() {
    assert_recorded_error_shape("cell", 0, &json!({ "status": 0, "headers": {} }));
}

// #1759 r4 boundary P2: the key-set guard has to run before a cell can be
// skipped, so it is a standalone function over the recorded `options`, not
// something a replayed cell happens to trigger inside `options()`.
#[test]
#[should_panic(expected = "options key \"requestTimeout\" is not read")]
fn recorded_options_refuse_an_unknown_key_before_a_cell_is_skipped() {
    assert_recorded_options_are_known(&json!({ "options": { "requestTimeout": 1000 } }));
}

// #1759 r4 boundary P0: the skipped `oversized-*` branch claimed "every
// recorded response is a connection error" with `Iterator::all`, which is true
// of an empty slice, so a cell that recorded nothing passed while the surface
// stayed green. These three are the shapes that claim has to refuse.
#[test]
#[should_panic(expected = "must record at least one expectation")]
fn oversized_cell_refuses_an_empty_expectation_list() {
    assert_oversized_cell_records_only_connection_errors(
        "oversized-x",
        &json!({
            "requests": [{ "method": "POST", "path": "/v1/chat/completions" }],
            "expect": [],
        }),
    );
}

#[test]
#[should_panic(expected = "must record one expectation per request")]
fn oversized_cell_refuses_a_dropped_expectation() {
    assert_oversized_cell_records_only_connection_errors(
        "oversized-x",
        &json!({
            "requests": [
                { "method": "POST", "path": "/v1/chat/completions" },
                { "method": "POST", "path": "/v1/chat/completions" },
            ],
            "expect": [{ "error": "ECONNRESET", "bytes": 0 }],
        }),
    );
}

#[test]
#[should_panic(expected = "must record a connection error, not a response")]
fn oversized_cell_refuses_an_answered_response() {
    assert_oversized_cell_records_only_connection_errors(
        "oversized-x",
        &json!({
            "requests": [{ "method": "POST", "path": "/v1/chat/completions" }],
            "expect": [{ "status": 400, "headers": {}, "body": "" }],
        }),
    );
}

fn compare_response(case: &str, index: usize, expected: &Value, actual: &RawResponse) {
    if expected.get("error").is_some() {
        assert!(
            actual.error.is_some(),
            "#1333 {case} request {index}: expected a zero-byte connection close, got status {:?} body {:?}",
            actual.status,
            String::from_utf8_lossy(&actual.body)
        );
        // A hung connection also ends with `error`/no status once the 10 s
        // client timeout fires; without this the ruler would pass on a hang,
        // which the TS generator (no timeout) can never do.
        assert_ne!(
            actual.error.as_deref(),
            Some("timeout"),
            "#1333 {case} request {index}: the connection must close, not hang"
        );
        return;
    }
    assert_eq!(
        actual.status,
        expected["status"].as_u64().map(|status| status as u16),
        "#1333 {case} request {index}: status differs (body {:?})",
        String::from_utf8_lossy(&actual.body)
    );
    let expected_headers = expected["headers"]
        .as_object()
        .expect("#1333 oracle headers")
        .iter()
        .map(|(name, value)| {
            // #1606 r4: the expectation side gets the same fail-closed rule as
            // the request side. `unwrap_or("")` compared a malformed recorded
            // header as an empty one, so a drifted oracle could still match.
            let value = value.as_str().unwrap_or_else(|| {
                panic!("#1333 oracle header {name:?} must be a string, found {value:?}")
            });
            (name.to_ascii_lowercase(), value.to_owned())
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        actual_headers(actual, matches!(case, "unknown-method" | "missing-host")),
        expected_headers,
        "#1333 {case} request {index}: headers differ"
    );
    assert_eq!(
        mask_text(&String::from_utf8_lossy(&actual.body)),
        expected["body"].as_str().expect("#1333 oracle body"),
        "#1333 {case} request {index}: body differs"
    );
}

struct Latch {
    started: AtomicBool,
    released: AtomicBool,
    started_notify: Notify,
    released_notify: Notify,
}

impl Latch {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            started: AtomicBool::new(false),
            released: AtomicBool::new(false),
            started_notify: Notify::new(),
            released_notify: Notify::new(),
        })
    }
    async fn wait_started(&self) {
        loop {
            let notified = self.started_notify.notified();
            if self.started.load(Ordering::SeqCst) {
                return;
            }
            notified.await;
        }
    }
    fn release(&self) {
        self.released.store(true, Ordering::SeqCst);
        self.released_notify.notify_one();
    }
    async fn wait_release(&self) {
        loop {
            let notified = self.released_notify.notified();
            if self.released.load(Ordering::SeqCst) {
                return;
            }
            notified.await;
        }
    }
}

struct ScriptedTurn {
    events: Vec<Value>,
    error: Option<String>,
}

struct ScriptStream {
    events: VecDeque<JsValue>,
    error: Option<String>,
    latch: Option<Arc<Latch>>,
    dropped: Arc<AtomicBool>,
}

impl TurnEventStream for ScriptStream {
    fn next(
        &mut self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Option<JsValue>, ChannelError>> + Send + '_>,
    > {
        Box::pin(async move {
            if let Some(latch) = self.latch.take() {
                latch.started.store(true, Ordering::SeqCst);
                latch.started_notify.notify_one();
                latch.wait_release().await;
            }
            if let Some(error) = self.error.take() {
                return Err(ChannelError::Error(error));
            }
            Ok(self.events.pop_front())
        })
    }
}

impl Drop for ScriptStream {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

struct ScriptedGateway {
    turns: Vec<ScriptedTurn>,
    calls: Arc<Mutex<Vec<Value>>>,
    latch: Option<Arc<Latch>>,
    dropped: Arc<AtomicBool>,
    index: Mutex<usize>,
}

impl ScriptedGateway {
    fn with_latch(
        turns: Vec<ScriptedTurn>,
        latch: Option<Arc<Latch>>,
        dropped: Arc<AtomicBool>,
    ) -> Arc<Self> {
        Arc::new(Self {
            turns,
            calls: Arc::default(),
            latch,
            dropped,
            index: Mutex::new(0),
        })
    }
}

impl TurnGateway for ScriptedGateway {
    fn submit(
        &self,
        request: TurnRequest,
    ) -> BoxFuture<Result<Box<dyn TurnEventStream>, ChannelError>> {
        let mut index = self.index.lock().expect("#1333 scripted gateway index");
        assert!(
            !self.turns.is_empty(),
            "#1333 scripted gateway needs at least one turn"
        );
        let position = (*index).min(self.turns.len() - 1);
        let turn = &self.turns[position];
        *index += 1;
        let events = turn
            .events
            .iter()
            .map(|value| {
                pilotdeck_protocol::parse_json(&value.to_string())
                    .expect("#1333 scripted turn event")
            })
            .collect();
        let error = turn.error.clone();
        self.calls
            .lock()
            .expect("#1333 scripted gateway calls")
            .push(json!({
                "sessionKey": request.session_key.to_utf8().unwrap_or_default(),
                "channelKey": request.channel_key.to_string(),
                "message": request.message.to_utf8().unwrap_or_default(),
            }));
        let latch = self.latch.clone();
        let dropped = self.dropped.clone();
        Box::pin(async move {
            Ok(Box::new(ScriptStream {
                events,
                error,
                latch,
                dropped,
            }) as Box<dyn TurnEventStream>)
        })
    }
}

/// #1737: a gateway whose `submit` rejects outright. No scripted turn can
/// reach the `Err` arms of `stream_turn` / `buffered_turn`: `ScriptedGateway`
/// always returns a stream and `ScriptedTurn.error` only fails the stream's
/// `next()`, so the submit-rejection path gets its own mock. The counter is
/// the no-retry assertion: both call sites must submit exactly once.
struct FailingGateway {
    calls: AtomicUsize,
}

impl TurnGateway for FailingGateway {
    fn submit(
        &self,
        _request: TurnRequest,
    ) -> BoxFuture<Result<Box<dyn TurnEventStream>, ChannelError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            Err(ChannelError::Error(
                "#1737 scripted submit rejection".to_owned(),
            ))
        })
    }
}

fn js_string_array(values: &[Value]) -> JsValue {
    JsValue::Array(
        values
            .iter()
            .map(|value| match value.as_str() {
                // #1606 r5: an element that is not a string used to become an
                // empty origin, i.e. an allow-list entry nobody wrote.
                Some(origin) => JsValue::String(JsString::from(origin)),
                None => panic!("#1333 corsOrigins entries must be strings, found {value:?}"),
            })
            .collect(),
    )
}

/// #1606 r5: `corsOrigins` is the last field in this struct that was still
/// two-state. Present-but-not-an-array silently started a server with no CORS
/// at all — the same "absent vs. wrong type" conflation this file removes
/// everywhere else.
fn case_origins(options: &Value) -> Option<JsValue> {
    match options.get("corsOrigins") {
        None => None,
        Some(Value::Array(values)) => Some(js_string_array(values)),
        Some(other) => panic!("#1333 options.corsOrigins must be an array, found {other:?}"),
    }
}

/// #1606 r4: the same tri-state rule the request fields follow. A recorded
/// option that is present but not a string is fixture drift: `unwrap_or`
/// would quietly start a server with different options than the cell was
/// captured under, and the cell would still be green (the recorded default
/// happens to equal the fallback).
fn case_option(options: &Value, key: &str, fallback: &str) -> String {
    match options.get(key) {
        Some(Value::String(text)) => text.to_owned(),
        Some(other) => panic!("#1333 options.{key} must be a string, found {other:?}"),
        None => fallback.to_owned(),
    }
}

/// #1759: every `options` key this replay maps into `ApiServerOptions`. The
/// oracle is allowed to record only these, because `generate.mts` spreads the
/// recorded object into the real `ApiServerChannel` — an unmapped key would be
/// a TS-side setting the Rust comparison silently ignores.
const REPLAY_OPTION_KEYS: [&str; 3] = ["apiKey", "modelName", "corsOrigins"];

fn options(case: &Value, timeout: Option<Duration>) -> ApiServerOptions {
    assert_recorded_options_are_known(case);
    let options = &case["options"];
    ApiServerOptions {
        port: Some(JsValue::Number(0.0)),
        api_key: Some(JsValue::String(JsString::from(case_option(
            options, "apiKey", "",
        )))),
        host: Some("127.0.0.1".to_owned()),
        model_name: Some(case_option(options, "modelName", "parity-model")),
        cors_origins: case_origins(options),
        mapper: None,
        request_timeout: timeout,
        body_read_timeout: None,
        accepted: None,
        uuid: Some(Arc::new(|| FIXED_UUID.to_owned())),
    }
}

/// #1759 r4 (boundary P2): the `options` container and key-set guards used to
/// live inside `options()`, which the replay only calls for cells it actually
/// replays — `oversized-*` and `body-read-timeout` `continue` first, so a key
/// recorded on those four cells was never checked. That is the same "skipped
/// cell is unread" hole the shape pin closes for expectations, so the guard is
/// shape/roster-only (it never parses a value) and runs for every cell before
/// any branch can skip it.
fn assert_recorded_options_are_known(case: &Value) {
    // #1606 r6 (both zai roles, independently): r5 tightened every key *inside*
    // `options` and claimed this file no longer conflates absent with
    // wrong-type — but the container itself was still two-state. With
    // `"options": []` / `"x"` / `5` every key below reads as absent through
    // `Value::get`, so the cell starts under the recorded defaults while the
    // oracle recorded something else, and stays green whenever that option is
    // not visible in the compared response. Same shape as the `headers`
    // container fixed in r4. Absent stays legitimate (`&case["options"]` is
    // `Null` and every key falls back); present-but-not-an-object is drift.
    if let Some(other) = case.get("options").filter(|value| !value.is_object()) {
        panic!("#1333 options must be an object, found {other:?}");
    }
    let options = &case["options"];
    // #1759: the *key set* was still fail-open. `generate.mts` spreads the
    // recorded options verbatim into the real `ApiServerChannel`
    // (`...(options ?? {})`), so a key the generator records takes effect on
    // the TS side; a reader that ignores it would compare a cell against a
    // differently-configured Rust server and still stay green. The roster is
    // closed: a new key must be mapped here before the oracle may record it.
    if let Some(recorded) = options.as_object() {
        for key in recorded.keys() {
            assert!(
                REPLAY_OPTION_KEYS.contains(&key.as_str()),
                "#1333 options key {key:?} is not read by the replay (known keys: \
                 {REPLAY_OPTION_KEYS:?}); the fixture and the reader have drifted apart"
            );
        }
    }
}

struct Started {
    channel: ApiServerChannel,
    handle: Arc<dyn ChannelHandle>,
    addr: std::net::SocketAddr,
    logs: Arc<Mutex<Vec<Value>>>,
}

async fn start(options: ApiServerOptions, gateway: Option<Arc<dyn TurnGateway>>) -> Started {
    let channel = ApiServerChannel::with_env(options, HashMap::new());
    let logs: Arc<Mutex<Vec<Value>>> = Arc::default();
    let sink = logs.clone();
    let handle = channel
        .start(ChannelStartDeps {
            config: None,
            report_status: None,
            log: Some(Arc::new(move |level: &str, message: &str| {
                sink.lock()
                    .expect("#1333 log sink")
                    .push(json!([level, mask_text(message)]));
            })),
            turn_gateway: gateway,
        })
        .await
        .expect("#1333 channel start");
    let addr = channel.bound_addr().expect("#1333 bound listener address");
    Started {
        channel,
        handle,
        addr,
        logs,
    }
}

fn gateway_from(
    case: &Value,
    latch: Option<Arc<Latch>>,
    dropped: Arc<AtomicBool>,
) -> Arc<ScriptedGateway> {
    // #1606 r6 (zai boundary P3, same family as the `options` container above):
    // `as_array().unwrap_or_default()` made a present-but-wrong-type `gateway`
    // or `turns` read as "this cell scripts no turns", so a drifted oracle
    // started a gateway with an empty script. Most such cells would still fail
    // loudly on the response, but the shape is the one r2–r5 removed
    // everywhere else, so the container gets the same tri-state rule. Absent
    // (`Null`) stays legitimate — most cells record no gateway at all.
    let gateway = &case["gateway"];
    let turns = match gateway {
        Value::Null => Vec::new(),
        Value::Object(_) => match &gateway["turns"] {
            Value::Null => Vec::new(),
            Value::Array(turns) => turns
                .iter()
                .map(|turn| {
                    // #1759: `Value::get` answers `None` for a non-object, so
                    // `"turns": [5]` used to read as a turn with no `events`
                    // and no `error` — the absent/wrong-type conflation r4/r6
                    // removed from the outer containers, still open on the
                    // element itself. Only an absent `turns` key means "no
                    // turn"; a `null` element is drift like any other type.
                    let turn = match turn {
                        Value::Object(_) => turn,
                        other => {
                            panic!("#1333 gateway turn must be an object, found {other:?}")
                        }
                    };
                    ScriptedTurn {
                        events: match turn.get("events") {
                            None | Some(Value::Null) => Vec::new(),
                            Some(Value::Array(events)) => events.clone(),
                            Some(other) => {
                                panic!(
                                    "#1333 gateway turn events must be an array, found {other:?}"
                                )
                            }
                        },
                        error: match turn.get("error") {
                            None | Some(Value::Null) => None,
                            Some(Value::String(text)) => Some(text.clone()),
                            Some(other) => {
                                panic!("#1333 gateway turn error must be a string, found {other:?}")
                            }
                        },
                    }
                })
                .collect(),
            other => panic!("#1333 gateway.turns must be an array, found {other:?}"),
        },
        other => panic!("#1333 gateway must be an object, found {other:?}"),
    };
    ScriptedGateway::with_latch(turns, latch, dropped)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replays_real_ts_api_server_cells() {
    let oracle = oracle();
    assert_eq!(
        oracle["sourceCommit"], "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85",
        "#1333 baseline commit"
    );
    assert_eq!(oracle["node"], "v22.22.0", "#1333 Node baseline");
    let cases = oracle["cases"].as_array().expect("#1333 oracle cases");
    // Exact ids, not a count: a regenerated oracle that renames or swaps one
    // cell would keep the length unchanged while silently dropping a surface.
    let ids: Vec<&str> = cases
        .iter()
        .map(|case| case["id"].as_str().expect("#1333 case id"))
        .collect();
    assert_eq!(
        ids,
        [
            "method-dispatch",
            "unknown-method",
            "missing-host",
            "cors-exact",
            "cors-star",
            "auth",
            "chat-validation",
            "buffered",
            "stream",
            "new-command",
            "gateway-missing",
            "oversized-0",
            "oversized-1",
            "oversized-2",
            "body-limit-exact",
            "body-read-timeout",
            "keep-alive",
        ],
        "#1333 oracle must keep every TS surface cell, in order"
    );
    for case in cases {
        let id = case["id"].as_str().expect("#1333 case id");
        // The length check runs for the boundary cells only; the
        // budget-vs-cap check at its top runs for every cell, so no cell can
        // replay a body while the product cap sits above the budget.
        assert_boundary_body_len(id, case);
        // #1683: the two parser-admission cells must keep the knobs that make
        // them rejections (FROB / `setHost: false`), not drift into controls.
        if id == "unknown-method" || id == "missing-host" {
            assert_parser_admission_cell_records_its_knob(id, case);
        }
        // #1606 r3: `compare_response` walks `expect` by request index, so a
        // cell that records *more* responses than requests would leave the
        // surplus never compared — a TS surface silently skipped while the
        // cell stays green. Fewer responses already panics on the index; this
        // closes the other direction for every cell, not just boundary ones.
        let recorded_requests = case["requests"].as_array().expect("#1333 requests");
        // #1606 r4: the pairing below is 0 == 0 for a cell that records
        // neither, and such a cell would replay nothing while still counting
        // as covered.
        assert!(
            !recorded_requests.is_empty(),
            "#1333 {id}: a cell must record at least one request"
        );
        assert_eq!(
            case["expect"].as_array().map(Vec::len),
            Some(recorded_requests.len()),
            "#1333 {id}: every recorded TS response must pair with a recorded request"
        );
        // #1759: by shape, before any branch can skip a cell — the
        // `oversized-*` and `body-read-timeout` cells never reach
        // `compare_response`, so an id-scoped pin would leave a future error
        // cell's recorded `bytes` / `status` unread.
        //
        // The same applies to `options`: the guard lives in `options()`, which
        // only replayed cells ever call, so the skipped cells' recorded option
        // keys were unread (#1759 r4 boundary P2).
        assert_recorded_options_are_known(case);
        for (index, expected) in case["expect"]
            .as_array()
            .expect("#1333 expect")
            .iter()
            .enumerate()
        {
            assert_recorded_error_shape(id, index, expected);
        }
        if id == "body-read-timeout" {
            // #1620: a client that declares `Content-Length` and then stops
            // sending is answered by Node's `requestTimeout` (300 s) with the
            // 408 and a close. Not replayed here because the real cell waits
            // out that full 300 s and hyper frames the same 408 with a
            // `Transfer-Encoding` header the oracle's parsed view does not
            // carry. `partial_body_is_answered_at_the_body_read_deadline`
            // pins the Rust side with a shortened injected deadline instead.
            //
            // Pin the TS side so the skip is not blind: the baseline must
            // deliver the 408, not reset the connection, and it must record
            // how much of the declared body it sent before stalling.
            let expected = &case["expect"][0];
            assert_eq!(
                expected["status"].as_u64(),
                Some(408),
                "#1333 {id}: the baseline answers a stalled body with 408, got {expected}"
            );
            assert_eq!(
                expected["body"].as_str(),
                Some(""),
                "#1333 {id}: the requestTimeout body is empty, got {expected}"
            );
            assert!(
                expected.get("error").is_none(),
                "#1333 {id}: the baseline must deliver the 408, not reset the connection, got {expected}"
            );
            // The Rust side asserts this 408 carries no `Date`
            // (`partial_body_is_answered_at_the_body_read_deadline`). Pin the
            // same contract on the recorded baseline answer, so the two sides
            // cannot drift into opposite conventions without a red test.
            assert!(
                expected["headers"].get("date").is_none(),
                "#1333 {id}: the baseline requestTimeout 408 has no Date header, got {expected}"
            );
            let request = &recorded_requests[0];
            let declared: u64 = request["headers"]["content-length"]
                .as_str()
                .and_then(|value| value.parse().ok())
                .unwrap_or_else(|| {
                    panic!(
                        "#1333 {id}: the request must declare a numeric Content-Length: {request}"
                    )
                });
            let stalled = request["stalledAfter"].as_u64().unwrap_or_else(|| {
                panic!("#1333 {id}: the request must record how many bytes it sent: {request}")
            });
            assert!(
                stalled < declared,
                "#1333 {id}: the request must stall before the declared body is complete \
                 (sent {stalled} of {declared})"
            );
            continue;
        }
        if id.starts_with("oversized-") {
            // The three oversized cells record the same request; one of them is
            // replayed for real by `oversized_body_closes_without_response`
            // (`assert_boundary_body_len` above keeps the other two honest
            // about their length). Not replayed here because the server
            // destroys the connection mid-write, and the exact error string
            // (ECONNRESET / EPIPE / a clean EOF) depends on write progress.
            // `body-limit-exact` stays in this loop: a body at the cap is read
            // in full and answered, so its response is deterministic.
            //
            // #1606 r4 (both families): skipping the replay also skipped the
            // recorded response, so nothing compared the TS side of these
            // cells at all. If the baseline cap ever rises above the recorded
            // `OVER_CAP_BYTES`, the generator would record `400 invalid_json`
            // here while the placeholder and the Rust cap stay at 1_000_001 —
            // the whole surface green and the two sides disagreeing.
            //
            // #1759: the recorded fields are pinned for *every* error
            // expectation by `assert_recorded_error_shape` above. What stays
            // here is the cell's own claim — this surface must be a connection
            // error at all, not an answered `400` — because the replay skips
            // the comparison and nothing else would notice the whole response
            // turning into a normal one (#1759 r1 boundary P1). The helper also
            // requires one recorded outcome per request before it makes that
            // claim, so an emptied `expect` cannot pass vacuously
            // (#1759 r4 boundary P0).
            assert_oversized_cell_records_only_connection_errors(id, case);
            continue;
        }
        let dropped = Arc::new(AtomicBool::new(false));
        let gateway = case
            .get("gateway")
            .map(|_| gateway_from(case, None, dropped.clone()) as Arc<dyn TurnGateway>);
        let started = start(options(case, None), gateway.clone()).await;
        for (index, request) in recorded_requests.iter().enumerate() {
            let spec = ReqSpec::from_json(request);
            let response = exchange(started.addr, &spec).await;
            compare_response(id, index, &case["expect"][index], &response);
        }
        started
            .handle
            .stop("surface-done")
            .await
            .expect("#1333 channel stop");
    }
}

/// #1683: the replay normalization drops `content-length: 0` (registered
/// transport diff), so the raw bytes have to be pinned separately or a lane
/// that stopped emitting the rejection entirely could still compare green on
/// some other 400. Both requests are the oracle's own knobs, sent through the
/// same `ReqSpec`/`exchange` path the replay uses.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn parser_rejections_keep_the_registered_transport_shape() {
    let started = start(options(&json!({ "options": {} }), None), None).await;
    let bad_method = exchange(
        started.addr,
        &ReqSpec {
            method: "FROB".to_owned(),
            path: "/health".to_owned(),
            headers: Vec::new(),
            body: Vec::new(),
            keep_alive: false,
            set_host: true,
        },
    )
    .await;
    assert_eq!(bad_method.status, Some(400), "#1683 FROB is a parser 400");
    assert_eq!(bad_method.body, b"", "#1683 the bare 400 has no body bytes");
    assert!(
        !bad_method.headers.contains_key("date"),
        "#1683 the bare 400 carries no Date, got {:?}",
        bad_method.headers
    );
    assert_eq!(
        bad_method.headers.get("connection").map(String::as_str),
        Some("close"),
        "#1683 the bare 400 closes the connection"
    );
    assert_eq!(
        bad_method.headers.get("content-length").map(String::as_str),
        Some("0"),
        "#1683 pin the registered hyper `content-length: 0` the replay normalizes away"
    );

    let missing_host = exchange(
        started.addr,
        &ReqSpec {
            method: "GET".to_owned(),
            path: "/health".to_owned(),
            headers: Vec::new(),
            body: Vec::new(),
            keep_alive: false,
            set_host: false,
        },
    )
    .await;
    assert_eq!(
        missing_host.status,
        Some(400),
        "#1683 HTTP/1.1 without Host is a parser 400"
    );
    assert_eq!(
        missing_host.body, b"",
        "#1683 the missing-Host 400 has an empty body"
    );
    assert!(
        missing_host.headers.contains_key("date"),
        "#1683 the missing-Host 400 carries a Date, got {:?}",
        missing_host.headers
    );
    assert_eq!(
        missing_host
            .headers
            .get("transfer-encoding")
            .map(String::as_str),
        Some("chunked"),
        "#1683 the missing-Host 400 is chunked (empty chunk terminator)"
    );
    assert_eq!(
        missing_host.headers.get("connection").map(String::as_str),
        Some("close"),
        "#1683 the missing-Host 400 closes the connection"
    );
    started.handle.stop("pin-done").await.expect("#1333 stop");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn submit_inputs_match_real_ts() {
    let oracle = oracle();
    // #1606 r5: `submits` is optional, so a regenerated oracle that lost (or
    // renamed) every `submits` key would leave this test green while comparing
    // nothing — the same "0 == 0 counts as covered" shape as the replay loop.
    // The cells that carry `submitTurn` inputs are part of the fixture
    // contract, so pin them.
    let mut with_submits = Vec::new();
    for case in oracle["cases"].as_array().expect("#1333 oracle cases") {
        if case.get("submits").is_some() {
            with_submits.push(case["id"].as_str().expect("#1333 case id"));
        }
    }
    assert_eq!(
        with_submits,
        ["buffered", "stream", "new-command"],
        "#1333 the cells that record `submitTurn` inputs changed; \
         `submit_inputs_match_real_ts` must be updated with the oracle"
    );
    for case in oracle["cases"].as_array().expect("#1333 oracle cases") {
        let Some(expected) = case.get("submits") else {
            continue;
        };
        let id = case["id"].as_str().expect("#1333 case id");
        let dropped = Arc::new(AtomicBool::new(false));
        let gateway = gateway_from(case, None, dropped);
        let started = start(
            options(case, None),
            Some(gateway.clone() as Arc<dyn TurnGateway>),
        )
        .await;
        for request in case["requests"].as_array().expect("#1333 requests") {
            let spec = ReqSpec::from_json(request);
            let response = exchange(started.addr, &spec).await;
            assert!(
                response.status.is_some(),
                "#1333 {id} request must reach the listener"
            );
        }
        started.handle.stop("surface-done").await.unwrap();
        let calls = gateway.calls.lock().expect("#1333 gateway calls").clone();
        assert_eq!(
            Value::Array(calls),
            *expected,
            "#1333 {id}: submitTurn inputs differ from the real TS channel"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_drains_in_flight_buffered_turn() {
    let latch = Latch::new();
    let dropped = Arc::new(AtomicBool::new(false));
    let gateway = ScriptedGateway::with_latch(
        vec![ScriptedTurn {
            events: vec![json!({"type":"assistant_text_delta","text":"done"})],
            error: None,
        }],
        Some(latch.clone()),
        dropped,
    );
    let started = start(options(&json!({"options":{}}), None), Some(gateway.clone())).await;
    let addr = started.addr;
    let spec = ReqSpec::post(
        "/v1/chat/completions",
        "chat-drain",
        "{\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}",
    );
    let request = tokio::spawn(async move { exchange(addr, &spec).await });
    tokio::time::timeout(Duration::from_secs(5), latch.wait_started())
        .await
        .expect("#1333 buffered turn reached the mock gateway");
    let handle = started.handle.clone();
    let stop = tokio::spawn(async move { handle.stop("drain-probe").await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !stop.is_finished(),
        "#1333 stop must stay pending while a buffered turn is in flight"
    );
    latch.release();
    let response = request.await.expect("#1333 buffered request task");
    assert_eq!(response.status, Some(200), "#1333 buffered turn completes");
    let stop = tokio::time::timeout(Duration::from_secs(5), stop)
        .await
        .expect("#1333 stop resolves after drain")
        .expect("#1333 stop task");
    assert!(stop.is_ok(), "#1333 stop result");
    let logs = started.logs.lock().expect("#1333 logs").clone();
    assert!(
        logs.iter()
            .any(|entry| entry[1] == "api-server: stopping (drain-probe)"),
        "#1333 stop reason is logged: {logs:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_drains_in_flight_stream() {
    let latch = Latch::new();
    let dropped = Arc::new(AtomicBool::new(false));
    let gateway = ScriptedGateway::with_latch(
        vec![ScriptedTurn {
            events: vec![json!({"type":"assistant_text_delta","text":"streamed"})],
            error: None,
        }],
        Some(latch.clone()),
        dropped,
    );
    let started = start(options(&json!({"options":{}}), None), Some(gateway)).await;
    let addr = started.addr;
    let spec = ReqSpec::post(
        "/v1/chat/completions",
        "chat-drain-stream",
        "{\"stream\":true,\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}",
    );
    let request = tokio::spawn(async move { exchange(addr, &spec).await });
    tokio::time::timeout(Duration::from_secs(5), latch.wait_started())
        .await
        .expect("#1333 stream reached the mock gateway");
    let handle = started.handle.clone();
    let stop = tokio::spawn(async move { handle.stop("drain-probe").await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !stop.is_finished(),
        "#1333 stop must stay pending while an SSE stream is in flight"
    );
    latch.release();
    let response = request.await.expect("#1333 stream request task");
    assert_eq!(response.status, Some(200), "#1333 stream status");
    let body = String::from_utf8_lossy(&response.body);
    assert!(
        body.contains("\"content\":\"streamed\"") && body.contains("data: [DONE]"),
        "#1333 stream keeps writing while stop waits: {body:?}"
    );
    tokio::time::timeout(Duration::from_secs(5), stop)
        .await
        .expect("#1333 stop resolves after the stream ends")
        .expect("#1333 stop task")
        .expect("#1333 stop result");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn duplicate_chat_is_busy_then_free() {
    let latch = Latch::new();
    let gateway = ScriptedGateway::with_latch(
        vec![
            ScriptedTurn {
                events: vec![json!({"type":"assistant_text_delta","text":"first"})],
                error: None,
            },
            ScriptedTurn {
                events: vec![json!({"type":"assistant_text_delta","text":"second"})],
                error: None,
            },
        ],
        Some(latch.clone()),
        Arc::new(AtomicBool::new(false)),
    );
    let started = start(options(&json!({"options":{}}), None), Some(gateway.clone())).await;
    let body = "{\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}";
    let addr = started.addr;
    let spec = ReqSpec::post("/v1/chat/completions", "chat-dup", body);
    let first = tokio::spawn(async move { exchange(addr, &spec).await });
    tokio::time::timeout(Duration::from_secs(5), latch.wait_started())
        .await
        .expect("#1333 first turn started");
    let second = exchange(
        started.addr,
        &ReqSpec::post("/v1/chat/completions", "chat-dup", body),
    )
    .await;
    assert_eq!(second.status, Some(429), "#1333 duplicate chat is rejected");
    let text = String::from_utf8_lossy(&second.body);
    assert!(
        text.contains("\"code\":\"session_busy\"") && text.contains("\"scope\":\"session\""),
        "#1333 duplicate chat error body: {text}"
    );
    latch.release();
    let first = first.await.expect("#1333 first request task");
    assert_eq!(first.status, Some(200), "#1333 first turn completes");
    let third = exchange(
        started.addr,
        &ReqSpec::post("/v1/chat/completions", "chat-dup", body),
    )
    .await;
    assert_eq!(
        third.status,
        Some(200),
        "#1333 chat is reusable after the turn"
    );
    assert_eq!(
        gateway.calls.lock().expect("#1333 calls").len(),
        2,
        "#1333 rejected duplicate is never submitted"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejected_new_while_busy_never_reroutes_the_chat() {
    // TS has no await between `activeChats.has` and `mapper.resolve`, so a
    // rejected duplicate can never touch the routing table. Here the admission
    // must be authoritative before `mapper.resolve` runs (#1333 concurrency).
    let latch = Latch::new();
    let gateway = ScriptedGateway::with_latch(
        vec![
            ScriptedTurn {
                events: vec![json!({"type":"assistant_text_delta","text":"first"})],
                error: None,
            },
            ScriptedTurn {
                events: vec![json!({"type":"assistant_text_delta","text":"second"})],
                error: None,
            },
        ],
        Some(latch.clone()),
        Arc::new(AtomicBool::new(false)),
    );
    let started = start(options(&json!({"options":{}}), None), Some(gateway.clone())).await;
    let addr = started.addr;
    let first = tokio::spawn(async move {
        exchange(
            addr,
            &ReqSpec::post(
                "/v1/chat/completions",
                "chat-new",
                "{\"messages\":[{\"role\":\"user\",\"content\":\"/new first\"}]}",
            ),
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(5), latch.wait_started())
        .await
        .expect("#1333 first turn started");
    let busy = exchange(
        started.addr,
        &ReqSpec::post(
            "/v1/chat/completions",
            "chat-new",
            "{\"messages\":[{\"role\":\"user\",\"content\":\"/new hijack\"}]}",
        ),
    )
    .await;
    assert_eq!(
        busy.status,
        Some(429),
        "#1333 a duplicate /new while busy is rejected"
    );
    latch.release();
    let first = first.await.expect("#1333 first request task");
    assert_eq!(first.status, Some(200), "#1333 first turn completes");
    let follow = exchange(
        started.addr,
        &ReqSpec::post(
            "/v1/chat/completions",
            "chat-new",
            "{\"messages\":[{\"role\":\"user\",\"content\":\"again\"}]}",
        ),
    )
    .await;
    assert_eq!(follow.status, Some(200), "#1333 follow-up completes");
    let calls = gateway.calls.lock().expect("#1333 calls");
    assert_eq!(calls.len(), 2, "#1333 the rejected /new is never submitted");
    assert_eq!(
        calls[0]["sessionKey"], calls[1]["sessionKey"],
        "#1333 a rejected /new must not reroute the chat"
    );
}

#[tokio::test]
async fn falsy_text_parts_match_the_ts_empty_message() {
    // TS returns `item.text` verbatim and `.filter(Boolean)` drops it, so a
    // numeric/bool `text` is an empty part -> 400 empty_message, while a
    // truthy non-string `text` is kept and stringified by `join`.
    let started = start(options(&json!({"options":{}}), None), None).await;
    for (index, literal) in ["0", "false"].into_iter().enumerate() {
        let body = format!(
            "{{\"messages\":[{{\"role\":\"user\",\"content\":[{{\"type\":\"text\",\"text\":{literal}}}]}}]}}"
        );
        let chat = format!("chat-falsy-{index}");
        let reply = exchange(
            started.addr,
            &ReqSpec::post("/v1/chat/completions", &chat, &body),
        )
        .await;
        assert_eq!(
            reply.status,
            Some(400),
            "#1333 a {literal} text part is an empty message"
        );
        assert!(
            String::from_utf8_lossy(&reply.body).contains("\"code\":\"empty_message\""),
            "#1333 falsy text part error body"
        );
    }
    let body = "{\"messages\":[{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":5}]}]}";
    let reply = exchange(
        started.addr,
        &ReqSpec::post("/v1/chat/completions", "chat-truthy-5", body),
    )
    .await;
    assert_eq!(
        reply.status,
        Some(503),
        "#1333 a truthy non-string text part still becomes user text"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restart_rebinds_listener_and_stop_is_idempotent() {
    let started = start(options(&json!({"options":{}}), None), None).await;
    let health = exchange(started.addr, &ReqSpec::get("/health")).await;
    assert_eq!(
        health.status,
        Some(200),
        "#1333 first listener serves /health"
    );
    started
        .handle
        .stop("first")
        .await
        .expect("#1333 first stop");
    started
        .handle
        .stop("again")
        .await
        .expect("#1333 stop is idempotent");
    let closed = exchange(started.addr, &ReqSpec::get("/health")).await;
    assert!(
        closed.error.is_some(),
        "#1333 stopped listener refuses connections"
    );
    let restarted = started
        .channel
        .start(ChannelStartDeps {
            config: None,
            report_status: None,
            log: None,
            turn_gateway: None,
        })
        .await
        .expect("#1333 restart");
    let addr = started.channel.bound_addr().expect("#1333 rebound address");
    let health = exchange(addr, &ReqSpec::get("/health")).await;
    assert_eq!(
        health.status,
        Some(200),
        "#1333 restarted listener serves /health"
    );
    restarted.stop("last").await.expect("#1333 last stop");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_disconnect_releases_chat_without_resubmit() {
    let latch = Latch::new();
    let dropped = Arc::new(AtomicBool::new(false));
    let gateway = ScriptedGateway::with_latch(
        vec![
            ScriptedTurn {
                events: vec![json!({"type":"assistant_text_delta","text":"never delivered"})],
                error: None,
            },
            ScriptedTurn {
                events: vec![json!({"type":"assistant_text_delta","text":"ok"})],
                error: None,
            },
        ],
        Some(latch.clone()),
        dropped.clone(),
    );
    let started = start(options(&json!({"options":{}}), None), Some(gateway.clone())).await;
    // Send the SSE request, read the response head, then drop the client
    // socket while the mock turn is still held.
    let mut socket = TcpStream::connect(started.addr).await.unwrap();
    let body = "{\"stream\":true,\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}";
    let wire = format!(
        "POST /v1/chat/completions HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nx-hermes-session-id: chat-cancel\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    socket.write_all(wire.as_bytes()).await.unwrap();
    let mut head = [0u8; 256];
    let _ = tokio::time::timeout(Duration::from_secs(5), socket.read(&mut head))
        .await
        .expect("#1333 SSE response head");
    tokio::time::timeout(Duration::from_secs(5), latch.wait_started())
        .await
        .expect("#1333 cancel turn started");
    drop(socket);
    latch.release();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !dropped.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("#1333 disconnected stream is dropped");
    tokio::time::timeout(Duration::from_secs(5), async {
        while !started
            .channel
            .0
            .active
            .lock()
            .expect("#1333 active chats")
            .is_empty()
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("#1333 disconnect releases the active chat guard");
    let body = "{\"messages\":[{\"role\":\"user\",\"content\":\"again\"}]}";
    let response = exchange(
        started.addr,
        &ReqSpec::post("/v1/chat/completions", "chat-cancel", body),
    )
    .await;
    assert_eq!(
        response.status,
        Some(200),
        "#1333 chat is free after disconnect"
    );
    assert_eq!(
        gateway.calls.lock().expect("#1333 calls").len(),
        2,
        "#1333 disconnect must not resubmit the cancelled turn"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn null_body_is_registered_divergence() {
    // TS cc7c4e6 crashes the process on a `null` body (probe4.out); Rust
    // answers the ordinary `messages_required` 400 instead. Registered in
    // specs/gateway-parity/diffs/A8.3-API-SERVER-001.md.
    let started = start(options(&json!({"options":{}}), None), None).await;
    let response = exchange(
        started.addr,
        &ReqSpec::post("/v1/chat/completions", "chat-null", "null"),
    )
    .await;
    assert_eq!(response.status, Some(400), "#1333 null body stays a 400");
    assert!(
        String::from_utf8_lossy(&response.body).contains("\"code\":\"messages_required\""),
        "#1333 null body uses messages_required"
    );
    started.handle.stop("done").await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oversized_body_closes_without_response() {
    let started = start(options(&json!({"options":{}}), None), None).await;
    // Deliberately a literal, not `MAX_REQUEST_BYTES + 1`: the `body-limit`
    // negative control rewrites the cap in `mod.rs` to 10,000,000 and expects
    // *this* test to go red. Deriving the body from the cap would move the
    // body with the mutation and let it survive, so the two stay independent
    // and `assert_boundary_body_len` is what ties the fixture to the cap.
    let body = "x".repeat(1_000_001);
    let response = exchange(
        started.addr,
        &ReqSpec {
            method: "POST".to_owned(),
            path: "/v1/chat/completions".to_owned(),
            headers: Vec::new(),
            body: body.into_bytes(),
            keep_alive: false,
            set_host: true,
        },
    )
    .await;
    assert!(
        response.error.is_some() && response.status.is_none(),
        "#1333 oversized body closes with zero bytes, got status {:?}",
        response.status
    );
    // #1606 r2: the TS baseline sends *nothing* back, so a truncated response
    // head before the reset must not pass as "closed with zero bytes".
    assert!(
        response.body.is_empty(),
        "#1333 oversized body must close without sending any bytes, got {}",
        response.body.len()
    );
    assert_ne!(
        response.error.as_deref(),
        Some("timeout"),
        "#1333 oversized body must close promptly, not hang until the client timeout"
    );
    started.handle.stop("done").await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn body_at_the_exact_limit_is_read_then_rejected_as_json() {
    // The cap is exclusive (`> MAX_REQUEST_BYTES`, mirroring TS
    // `ApiServerChannel.ts:409` `if (size > max)`), so a body of exactly
    // 1,000,000 bytes is read in full and then fails JSON parsing: 400
    // invalid_json, *not* the zero-byte close. The `body-limit-exact` oracle
    // cell now replays this boundary too (#1606), but keep this test: it is
    // the target the `body-limit-exact` negative-control mutation binds to,
    // and it builds the 1 MB body directly instead of through the oracle's
    // `<N bytes>` placeholder.
    let started = start(options(&json!({"options":{}}), None), None).await;
    let body = "x".repeat(1_000_000);
    let response = exchange(
        started.addr,
        &ReqSpec {
            method: "POST".to_owned(),
            path: "/v1/chat/completions".to_owned(),
            headers: Vec::new(),
            body: body.into_bytes(),
            keep_alive: false,
            set_host: true,
        },
    )
    .await;
    assert_eq!(
        response.status,
        Some(400),
        "#1333 a body of exactly MAX_REQUEST_BYTES is read, got error {:?}",
        response.error
    );
    assert!(
        String::from_utf8_lossy(&response.body).contains("\"code\":\"invalid_json\""),
        "#1333 a body of exactly MAX_REQUEST_BYTES fails as invalid_json, got {:?}",
        String::from_utf8_lossy(&response.body)
    );
    started.handle.stop("done").await.unwrap();
}

/// #1620: a client that declares `Content-Length` and then stops sending —
/// without closing — used to park `read_body` forever, and `stop()` drains the
/// connection tasks, so it hung too. Node's `requestTimeout` answers at the
/// deadline (webhook lane: `shutdown_with_partial_body_finishes_at_request_timeout`);
/// this pins the api-server lane to the same behavior with a shortened,
/// injected deadline. This is the test the `api-server-body-read-no-timeout`
/// negative control removes the deadline from.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn partial_body_is_answered_at_the_body_read_deadline() {
    const DEADLINE: Duration = Duration::from_secs(5);
    let mut case_options = options(&json!({"options":{}}), None);
    case_options.body_read_timeout = Some(Duration::from_millis(200));
    // #2047: `stop()` breaks the accept loop and drops the listener, so a
    // connection still sitting in the kernel backlog is reset instead of
    // answered — under a loaded runner the accept loop can be a few
    // milliseconds behind, and the RST then eats the 408 this test is about.
    // Wait until the loop reports it took *this* connection out of the
    // backlog before asking for a stop.
    let accepted = Arc::new(Notify::new());
    let notify = accepted.clone();
    case_options.accepted = Some(Arc::new(move || notify.notify_one()));
    let started = start(case_options, None).await;
    let port = started.addr.port();
    let handle = started.handle.clone();
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("#1333 connect to the stalled-body listener");
    tokio::time::timeout(DEADLINE, accepted.notified())
        .await
        .expect("#1333 the accept loop takes the stalled-body connection before stop()");
    stream
        .write_all(
            b"POST /v1/chat/completions HTTP/1.1\r\nHost: 127.0.0.1\r\n\
             Content-Length: 64\r\nConnection: keep-alive\r\n\r\nxxxxxxxx",
        )
        .await
        .expect("#1333 send the partial body");
    // Start `stop()` while the body is still stalled: without the deadline the
    // connection task never finishes and this join never resolves.
    let stopping = tokio::spawn(async move { handle.stop("partial-body").await });
    let mut response = Vec::new();
    tokio::time::timeout(DEADLINE, stream.read_to_end(&mut response))
        .await
        .expect("#1333 a stalled body is answered at the body-read deadline")
        .expect("#1333 read the stalled-body response bytes");
    let (end, status, headers) =
        parse_head(&response).expect("#1333 the stalled-body answer carries a response head");
    assert_eq!(
        status, 408,
        "#1333 a stalled body is answered with Node's requestTimeout 408"
    );
    assert_eq!(
        headers.get("connection").map(String::as_str),
        Some("close"),
        "#1333 the requestTimeout 408 closes the connection"
    );
    // Node's `requestTimeoutResponse` is 51 bytes with no `Date` line, and the
    // registered divergence specs (A8.3-API-SERVER-003-R1 / -004) state that
    // the Rust answer must not add one. `handle` skips `finish` (the only
    // place this lane adds `Date`) exactly for that reason, so pin its absence:
    // without this the whole `BodyReadTimeout` marker could be simplified away
    // and every test would stay green while the header regressed.
    assert!(
        !headers.contains_key("date"),
        "#1333 the requestTimeout 408 must not gain a Date header, got {:?}",
        headers
    );
    // Compare the raw framing, not `dechunk(..).is_empty()`: `dechunk` returns
    // an empty vector for anything that does not start with a valid hex
    // chunk-size line, so "could not de-frame" would pass as "empty body"
    // (GLM boundary P2). Node's 51 bytes carry no body and hyper frames an
    // unknown-length empty body as the bare terminator, which
    // `A8.3-API-SERVER-004` registers as this lane's on-the-wire shape.
    assert_eq!(
        &response[end..],
        b"0\r\n\r\n".as_slice(),
        "#1333 the requestTimeout 408 body is the empty chunked terminator, got {:?}",
        String::from_utf8_lossy(&response[end..])
    );
    tokio::time::timeout(DEADLINE, stopping)
        .await
        .expect("#1333 stop finishes after the body-read deadline")
        .expect("#1333 stop join")
        .expect("#1333 stop ok after the body-read deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sse_close_request_uses_hyper_close_header() {
    // Registered divergence A8.3-API-SERVER-002: Node keeps the handler's
    // `Connection: keep-alive` even when the request says `close`; hyper
    // 1.7 forces `close` once the request disabled keep-alive. The oracle's
    // SSE cells use keep-alive requests, so `replays_real_ts_api_server_cells`
    // does not cover this corner; this test pins the Rust side.
    let gateway = ScriptedGateway::with_latch(
        vec![ScriptedTurn {
            events: vec![json!({"type":"assistant_text_delta","text":"close"})],
            error: None,
        }],
        None,
        Arc::new(AtomicBool::new(false)),
    );
    let started = start(options(&json!({"options":{}}), None), Some(gateway)).await;
    let response = exchange(
        started.addr,
        &ReqSpec::post(
            "/v1/chat/completions",
            "chat-close",
            "{\"stream\":true,\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}",
        ),
    )
    .await;
    assert_eq!(response.status, Some(200), "#1333 close SSE status");
    assert_eq!(
        response.headers.get("connection").map(String::as_str),
        Some("close"),
        "#1333 hyper forces close when the request asked for it"
    );
    assert!(
        String::from_utf8_lossy(&response.body).contains("data: [DONE]"),
        "#1333 close SSE still completes the stream"
    );
    started.handle.stop("done").await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_listening_log_and_bind_failure_returns_noop() {
    let first = start(options(&json!({"options":{}}), None), None).await;
    let logs = first.logs.lock().expect("#1333 logs").clone();
    assert!(
        logs.iter().any(|entry| {
            entry[0] == "info"
                && entry[1]
                    .as_str()
                    .unwrap_or("")
                    .starts_with("api-server: listening on http://127.0.0.1:")
        }),
        "#1333 listening log: {logs:?}"
    );

    let mut busy = options(&json!({"options":{}}), None);
    busy.port = Some(JsValue::Number(f64::from(first.addr.port())));
    let second = ApiServerChannel::with_env(busy, HashMap::new());
    let failed_logs: Arc<Mutex<Vec<Value>>> = Arc::default();
    let sink = failed_logs.clone();
    let noop = second
        .start(ChannelStartDeps {
            config: None,
            report_status: None,
            log: Some(Arc::new(move |level: &str, message: &str| {
                sink.lock()
                    .expect("#1333 bind failure log")
                    .push(json!([level, message]));
            })),
            turn_gateway: None,
        })
        .await
        .expect("#1333 bind failure still returns Ok");
    assert!(
        second.bound_addr().is_none(),
        "#1333 bind failure returns a no-op handle"
    );
    let logs = failed_logs.lock().expect("#1333 bind failure logs").clone();
    assert!(
        logs.iter().any(|entry| entry[0] == "error"
            && entry[1]
                .as_str()
                .unwrap_or("")
                .starts_with("api-server: failed to start:")),
        "#1333 bind failure log: {logs:?}"
    );
    noop.stop("noop").await.expect("#1333 no-op stop");
    first.handle.stop("done").await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_closes_idle_keep_alive_connection() {
    let started = start(options(&json!({"options":{}}), None), None).await;
    let mut socket = TcpStream::connect(started.addr).await.unwrap();
    let wire = format!(
        "GET /health HTTP/1.1\r\nHost: {}\r\nConnection: keep-alive\r\n\r\n",
        started.addr
    );
    socket.write_all(wire.as_bytes()).await.unwrap();
    let mut buffer = [0u8; 1024];
    let read = tokio::time::timeout(Duration::from_secs(5), socket.read(&mut buffer))
        .await
        .expect("#1333 idle keep-alive response")
        .expect("#1333 idle keep-alive read");
    assert!(
        String::from_utf8_lossy(&buffer[..read]).starts_with("HTTP/1.1 200"),
        "#1333 idle keep-alive health response"
    );
    tokio::time::timeout(Duration::from_secs(2), started.handle.stop("idle-close"))
        .await
        .expect("#1333 stop resolves with an idle keep-alive connection")
        .expect("#1333 idle stop");
    let closed = tokio::time::timeout(Duration::from_secs(2), socket.read(&mut buffer))
        .await
        .expect("#1333 idle socket closes on stop")
        .expect("#1333 idle socket read after stop");
    assert_eq!(closed, 0, "#1333 idle keep-alive socket is closed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stream_timeout_ends_without_done() {
    let latch = Latch::new();
    let gateway = ScriptedGateway::with_latch(
        vec![ScriptedTurn {
            events: vec![json!({"type":"assistant_text_delta","text":"late"})],
            error: None,
        }],
        Some(latch.clone()),
        Arc::new(AtomicBool::new(false)),
    );
    let started = start(
        options(&json!({"options":{}}), Some(Duration::from_millis(100))),
        Some(gateway),
    )
    .await;
    let response = exchange(
        started.addr,
        &ReqSpec::post(
            "/v1/chat/completions",
            "chat-timeout",
            "{\"stream\":true,\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}",
        ),
    )
    .await;
    assert_eq!(response.status, Some(200), "#1333 timeout SSE status");
    let body = String::from_utf8_lossy(&response.body);
    assert!(
        !body.contains("[DONE]"),
        "#1333 timed-out stream must not close with [DONE]: {body:?}"
    );
    latch.release();
    started.handle.stop("done").await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn buffered_timeout_only_warns_and_keeps_turn() {
    let latch = Latch::new();
    let gateway = ScriptedGateway::with_latch(
        vec![ScriptedTurn {
            events: vec![json!({"type":"assistant_text_delta","text":"late"})],
            error: None,
        }],
        Some(latch.clone()),
        Arc::new(AtomicBool::new(false)),
    );
    let started = start(
        options(&json!({"options":{}}), Some(Duration::from_millis(100))),
        Some(gateway),
    )
    .await;
    let addr = started.addr;
    let spec = ReqSpec::post(
        "/v1/chat/completions",
        "chat-buffered-timeout",
        "{\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}",
    );
    let request = tokio::spawn(async move { exchange(addr, &spec).await });
    tokio::time::timeout(Duration::from_secs(5), latch.wait_started())
        .await
        .expect("#1333 buffered timeout turn started");
    tokio::time::sleep(Duration::from_millis(250)).await;
    let logs = started.logs.lock().expect("#1333 timeout logs").clone();
    assert!(
        logs.iter().any(|entry| {
            entry[0] == "warn"
                && entry[1]
                    .as_str()
                    .unwrap_or("")
                    .contains("buffered turn timeout for chat-buffered-timeout")
        }),
        "#1333 buffered timeout only warns: {logs:?}"
    );
    assert!(
        !request.is_finished(),
        "#1333 buffered timeout must not abort the turn"
    );
    latch.release();
    let response = request.await.expect("#1333 buffered timeout request");
    assert_eq!(
        response.status,
        Some(200),
        "#1333 buffered timeout completes"
    );
    assert!(
        String::from_utf8_lossy(&response.body).contains("\"content\":\"late\""),
        "#1333 buffered timeout keeps the eventual reply"
    );
    started.handle.stop("done").await.unwrap();
}

#[test]
fn render_visible_failure_status_names_match_ts() {
    // `isVisibleFailureAgentStatusEvent` (api-server-render.ts cc7c4e6) treats a
    // listed `VISIBLE_FAILURE_STATUS_EVENTS` name as visible even when its
    // `detail` is not error-severity. Every `agent_status` detail the oracle
    // streams is error-severity, so `replays_real_ts_api_server_cells` would
    // stay green if the name list were emptied; this pins the name branch.
    fn render_json(value: Value) -> Option<String> {
        let event = pilotdeck_protocol::parse_json(&value.to_string()).expect("#1333 event JSON");
        render::render(&event).map(|text| text.to_utf8().expect("#1333 rendered text"))
    }

    assert_eq!(
        render_json(json!({
            "type": "agent_status",
            "event": "turn_failed",
            "detail": {"message": "boom"}
        })),
        Some("\nError: boom\n".to_owned()),
        "#1333 a listed status name is visible without an error severity"
    );
    assert_eq!(
        render_json(json!({
            "type": "agent_status",
            "event": "some_other_event",
            "detail": {"message": "boom"}
        })),
        None,
        "#1333 an unlisted status name stays hidden"
    );
    assert_eq!(
        render_json(json!({
            "type": "agent_status",
            "event": "turn_failed",
            "detail": {"visible": false}
        })),
        None,
        "#1333 explicit visible:false still hides a listed name"
    );
}

#[test]
fn admit_chat_claims_a_chat_id_exactly_once() {
    // A `contains` check followed by a later `insert` lets two requests with the
    // same chat id both pass; the admission must be one atomic insert, so exactly
    // one of the racing callers wins (#1333 concurrency review).
    use std::collections::HashSet;
    let active = Arc::new(Mutex::new(HashSet::new()));
    let winners = Arc::new(Mutex::new(0usize));
    let mut workers = Vec::new();
    for _ in 0..8 {
        let active = active.clone();
        let winners = winners.clone();
        workers.push(std::thread::spawn(move || {
            if server::admit_chat(&active, "chat-race") {
                *winners.lock().expect("#1333 winners") += 1;
            }
        }));
    }
    for worker in workers {
        worker.join().expect("#1333 admission thread");
    }
    assert_eq!(
        *winners.lock().expect("#1333 winners"),
        1,
        "#1333 concurrent duplicates must admit exactly one request"
    );
    assert!(
        !server::admit_chat(&active, "chat-race"),
        "#1333 an admitted chat stays busy until it is released"
    );
    active.lock().expect("#1333 active").remove("chat-race");
    assert!(
        server::admit_chat(&active, "chat-race"),
        "#1333 a released chat id is reusable"
    );
}

#[test]
fn body_read_deadline_defaults_to_the_node_request_timeout() {
    // The baseline calls `http.createServer(...)` without setting
    // `server.requestTimeout`, so Node v22.22.0's default 300 s is the whole
    // contract (#1620). Nothing else pins the constant: the Rust test injects
    // a 200 ms deadline and the oracle replay skips this cell, so the default
    // could drift to 30 s or 1 h with every check still green.
    assert_eq!(
        DEFAULT_BODY_READ_TIMEOUT,
        Duration::from_secs(300),
        "#1620 the body-read deadline must stay at Node's default requestTimeout"
    );
    assert_eq!(
        ApiServerOptions::default().body_read_timeout,
        None,
        "#1620 an unset option means the Node default, not a shorter deadline"
    );
}

#[test]
fn unconfigured_port_falls_back_to_the_ts_default() {
    // TS `Number(options.port ?? env.API_SERVER_PORT ?? DEFAULT_PORT)`: a port
    // always resolves, so an adapter with no configuration still binds 8642.
    // Resolving to "no port" left the channel silently unbound (#1333 boundary
    // review: DEFAULT_PORT was declared but never read).
    let channel = ApiServerChannel::with_env(ApiServerOptions::default(), HashMap::new());
    assert_eq!(
        channel.resolve(None).port,
        DEFAULT_PORT,
        "#1333 an unconfigured port must default to 8642"
    );

    let explicit = ApiServerOptions {
        port: Some(JsValue::Number(0.0)),
        ..ApiServerOptions::default()
    };
    assert_eq!(
        ApiServerChannel::with_env(explicit, HashMap::new())
            .resolve(None)
            .port,
        0.0,
        "#1333 an explicit port still wins"
    );

    let env = HashMap::from([("API_SERVER_PORT".to_owned(), "9100".to_owned())]);
    assert_eq!(
        ApiServerChannel::with_env(ApiServerOptions::default(), env)
            .resolve(None)
            .port,
        9100.0,
        "#1333 the env port wins over the default"
    );
}

/// #1737: `stream_turn`'s submit-rejection arm — the SSE head is already on
/// the wire, so the client must still get a `channel_submit_failed` frame
/// (not a hang, not `[DONE]`), the chat guard must be released, and the
/// submit is not retried. This is the test the `stream-submit-err-arm`
/// negative control binds to.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn submit_error_stream_turn_emits_channel_submit_failed() {
    let gateway = Arc::new(FailingGateway {
        calls: AtomicUsize::new(0),
    });
    let started = start(
        options(&json!({"options": {}}), None),
        Some(gateway.clone() as Arc<dyn TurnGateway>),
    )
    .await;
    let response = exchange(
        started.addr,
        &ReqSpec::post(
            "/v1/chat/completions",
            "chat-submit-err-stream",
            "{\"stream\":true,\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}",
        ),
    )
    .await;
    // The SSE head is sent before `submit` runs (by design), so a rejected
    // submit still streams — a 500 here would mean the head moved after submit.
    assert_eq!(
        response.status,
        Some(200),
        "#1737 a rejected stream submit keeps the already-sent SSE head, got {:?}",
        response.status
    );
    assert_eq!(
        response.headers.get("content-type").map(String::as_str),
        Some("text/event-stream"),
        "#1737 a rejected stream submit still answers as SSE, got {:?}",
        response.headers
    );
    let body = String::from_utf8_lossy(&response.body);
    assert!(
        body.contains("data: {"),
        "#1737 the rejected submit frames its error as SSE data, got {body:?}"
    );
    assert!(
        body.contains("\"event\":\"channel_submit_failed\""),
        "#1737 a rejected stream submit emits channel_submit_failed, got {body:?}"
    );
    assert!(
        // The trailing comma pins `error.status` (the last `detail.status` has
        // `}` after it) and keeps `"status":5000` from matching the prefix.
        body.contains("\"status\":500,"),
        "#1737 the channel_submit_failed frame carries status 500, got {body:?}"
    );
    assert!(
        body.contains("\"scope\":\"channel\""),
        "#1737 the channel_submit_failed frame carries scope channel, got {body:?}"
    );
    assert!(
        body.contains("\"type\":\"server_error\""),
        "#1737 the channel_submit_failed frame carries type server_error, got {body:?}"
    );
    assert!(
        !body.contains("[DONE]"),
        "#1737 a rejected submit must not close the stream with [DONE], got {body:?}"
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while !started
            .channel
            .0
            .active
            .lock()
            .expect("#1737 active chats")
            .is_empty()
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("#1737 a rejected submit releases the active chat guard");
    assert_eq!(
        gateway.calls.load(Ordering::SeqCst),
        1,
        "#1737 a rejected stream submit is not retried"
    );
    started
        .handle
        .stop("submit-error-stream")
        .await
        .expect("#1737 stop after the submit-error stream");
}

/// #1737: `buffered_turn`'s submit-rejection arm — the 500 `submit_failed`
/// body (`error` + `agent_status`, `error_body.rs` field set), the released
/// chat guard and the single submit. This is the test the
/// `buffered-submit-err-arm` negative control binds to.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn submit_error_buffered_turn_returns_submit_failed() {
    let gateway = Arc::new(FailingGateway {
        calls: AtomicUsize::new(0),
    });
    let started = start(
        options(&json!({"options": {}}), None),
        Some(gateway.clone() as Arc<dyn TurnGateway>),
    )
    .await;
    let response = exchange(
        started.addr,
        &ReqSpec::post(
            "/v1/chat/completions",
            "chat-submit-err-buffered",
            "{\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}",
        ),
    )
    .await;
    assert_eq!(
        response.status,
        Some(500),
        "#1737 a rejected buffered submit answers 500, got error {:?}",
        response.error
    );
    let body: Value = serde_json::from_slice(&response.body).unwrap_or_else(|error| {
        panic!(
            "#1737 the submit_failed body is JSON: {error}, got {:?}",
            String::from_utf8_lossy(&response.body)
        )
    });
    assert_eq!(
        body["error"]["code"],
        json!("channel_submit_failed"),
        "#1737 the submit_failed error code is channel_submit_failed, got {body}"
    );
    assert_eq!(
        body["error"]["event"],
        json!("channel_submit_failed"),
        "#1737 the submit_failed error event is channel_submit_failed, got {body}"
    );
    assert_eq!(
        body["error"]["status"],
        json!(500),
        "#1737 the submit_failed error status is 500, got {body}"
    );
    assert_eq!(
        body["error"]["scope"],
        json!("channel"),
        "#1737 the submit_failed error scope is channel, got {body}"
    );
    assert_eq!(
        body["error"]["type"],
        json!("server_error"),
        "#1737 the submit_failed error type is server_error, got {body}"
    );
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("#1737 scripted submit rejection"),
        "#1737 the submit_failed error message carries the rejection, got {body}"
    );
    assert_eq!(
        body["agent_status"]["event"],
        json!("channel_submit_failed"),
        "#1737 the submit_failed agent_status event is channel_submit_failed, got {body}"
    );
    assert_eq!(
        body["agent_status"]["detail"]["code"],
        json!("channel_submit_failed"),
        "#1737 the submit_failed agent_status detail code is channel_submit_failed, got {body}"
    );
    assert_eq!(
        body["agent_status"]["detail"]["status"],
        json!(500),
        "#1737 the submit_failed agent_status detail status is 500, got {body}"
    );
    assert_eq!(
        body["agent_status"]["detail"]["scope"],
        json!("channel"),
        "#1737 the submit_failed agent_status detail scope is channel, got {body}"
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while !started
            .channel
            .0
            .active
            .lock()
            .expect("#1737 active chats")
            .is_empty()
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("#1737 a rejected buffered submit releases the active chat guard");
    assert_eq!(
        gateway.calls.load(Ordering::SeqCst),
        1,
        "#1737 a rejected buffered submit is not retried"
    );
    started
        .handle
        .stop("submit-error-buffered")
        .await
        .expect("#1737 stop after the submit-error buffered turn");
}

#[test]
fn port_numbers_follow_the_es_string_numeric_literal() {
    // TS `Number(value)` (ApiServerChannel.ts:51): a sign may only precede a
    // decimal literal or `Infinity`, so `Number("+0x10")` is NaN and
    // `server.listen(NaN)` fails the start. A lone surrogate cannot be spelled
    // in UTF-8 and is NaN too, never `Number("") = 0` (which would bind an
    // ephemeral port). #1333 boundary review.
    let number = |text: &str| server::js_number(&JsValue::String(JsString::from(text)));
    assert_eq!(number(" 0x10 "), 16.0, "#1333 unsigned hex still parses");
    assert_eq!(number("0b101"), 5.0, "#1333 unsigned binary still parses");
    assert_eq!(number(""), 0.0, "#1333 the empty string is zero");
    assert!(
        number("+0x10").is_nan(),
        "#1333 a signed hex literal is NaN"
    );
    assert!(
        number("-0xFF").is_nan(),
        "#1333 a negative hex literal is NaN"
    );
    assert_eq!(number("-Infinity"), f64::NEG_INFINITY);
    let lone = JsString::from_utf16(vec![0xd800]);
    assert!(
        server::js_number(&JsValue::String(lone)).is_nan(),
        "#1333 a lone-surrogate port string is NaN, not the empty string"
    );
}

// ---------------------------------------------------------------- loader

/// #2428: a buffered-turn request with an optional Bearer token.
fn bearer_chat(token: Option<&str>) -> ReqSpec {
    ReqSpec {
        method: "POST".to_owned(),
        path: "/v1/chat/completions".to_owned(),
        headers: match token {
            Some(token) => vec![
                ("authorization".to_owned(), format!("Bearer {token}")),
                ("x-hermes-session-id".to_owned(), "chat-2428".to_owned()),
            ],
            None => vec![("x-hermes-session-id".to_owned(), "chat-2428".to_owned())],
        },
        body: b"{\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}".to_vec(),
        keep_alive: false,
        set_host: true,
    }
}

/// #2428: `options.apiKey ?? process.env.API_SERVER_KEY ?? ""` keeps any
/// non-nullish value; `checkAuth`'s `Buffer.from(this.apiKey)` then throws on
/// a number/`true`/object, the catch answers 401 for every request. The Rust
/// mount used to fold a non-string into `None` and fall through to the
/// env/"" fallback — a configured number silently disabled auth (fail-open).
/// Each classification is pinned: a number never matches any Bearer, `false`
/// is `Off` like TS's `!this.apiKey` gate, and an array compares as its
/// `Buffer.from` bytes (`[104, 105]` is `"hi"`).
///
/// Reachability note (review r1, arbitrated against the TS source): this
/// seam is the loader's option/`extra` mount, where a non-nullish
/// non-string genuinely reaches `ApiKey::of`. A non-string **platform**
/// `apiKey` never gets here — the TS snapshot layer folds it first
/// (`parsePlatformAdapter` reads it with `stringField`, `parseGatewayConfig`
/// :102-112, which keeps only strings) and Rust's `platform()` mirrors that
/// with `as_string()`; that snapshot-side fold is pinned in
/// `config/tests.rs::snapshot_folds_a_non_string_platform_api_key_like_string_field`.
#[tokio::test]
async fn non_string_api_keys_never_fold_to_unset() {
    let turn_gateway = || {
        Some(gateway_from(
            &json!({"gateway": {"turns": [{"events": []}]}}),
            None,
            Arc::new(AtomicBool::new(false)),
        ) as Arc<dyn TurnGateway>)
    };
    let number = start(
        ApiServerOptions {
            port: Some(JsValue::Number(0.0)),
            api_key: Some(JsValue::Number(7.0)),
            ..ApiServerOptions::default()
        },
        turn_gateway(),
    )
    .await;
    crate::negative_assert_eq!(
        exchange(number.addr, &bearer_chat(None)).await.status,
        Some(401),
        "#2428 a configured number apiKey still rejects a request without the Bearer key"
    );
    crate::negative_assert_eq!(
        exchange(number.addr, &bearer_chat(Some("7"))).await.status,
        Some(401),
        "#2428 a configured number apiKey rejects even the string it never equals"
    );
    number
        .handle
        .stop("test")
        .await
        .expect("#2428 api_server stop");

    // `false` is falsy, not nullish: TS keeps it and `!this.apiKey` opens the
    // gate — it must not be folded into the env/"" fallback either.
    let false_key = start(
        ApiServerOptions {
            port: Some(JsValue::Number(0.0)),
            api_key: Some(JsValue::Bool(false)),
            ..ApiServerOptions::default()
        },
        turn_gateway(),
    )
    .await;
    crate::negative_assert_eq!(
        exchange(false_key.addr, &bearer_chat(None)).await.status,
        Some(200),
        "#2428 a configured false apiKey answers 200 like TS's !this.apiKey gate"
    );
    false_key
        .handle
        .stop("test")
        .await
        .expect("#2428 api_server stop");

    let array = start(
        ApiServerOptions {
            port: Some(JsValue::Number(0.0)),
            api_key: Some(JsValue::Array(vec![
                JsValue::Number(104.0),
                JsValue::Number(105.0),
            ])),
            ..ApiServerOptions::default()
        },
        turn_gateway(),
    )
    .await;
    crate::negative_assert_eq!(
        exchange(array.addr, &bearer_chat(Some("hi"))).await.status,
        Some(200),
        "#2428 an array apiKey matches its Buffer.from bytes"
    );
    crate::negative_assert_eq!(
        exchange(array.addr, &bearer_chat(Some("hi!"))).await.status,
        Some(401),
        "#2428 an array apiKey only matches its Buffer.from bytes"
    );
    array
        .handle
        .stop("test")
        .await
        .expect("#2428 api_server stop");
}

/// #2428 review r1: the classification table pinned at the seam — the
/// server-level test above exercises only `7`/`false`/`[104, 105]`, leaving
/// the falsy numbers, `true`, objects, the empty array and `Buffer.from`'s
/// ToUint8 boundaries unpinned. Every row follows the Node v22.22.0 probes
/// in `_tmp/20261026-issue-p2batch-f/s4-probe/api-key-buffer-gates.raw`:
/// `Buffer.from(42)`/`(true)`/`({})` throw (the 401 `Never` limb), the
/// empty array is truthy (`![]` is false) and classifies as an empty `Key`,
/// and `[256, 257, -1, 1.7, -1.7, "65", "a", null]` maps through ToUint8
/// to `[0, 1, 255, 1, 255, 65, 0, 0]`.
#[test]
fn api_key_classification_pins_the_checkauth_table() {
    let of = |value: &JsValue| ApiKey::of(value);
    // The `??`-kept falsy values `!this.apiKey` opens the gate for.
    crate::negative_assert_eq!(
        of(&JsValue::Undefined),
        ApiKey::Off,
        "#2428 an undefined apiKey is Off"
    );
    crate::negative_assert_eq!(
        of(&JsValue::Null),
        ApiKey::Off,
        "#2428 a null apiKey is Off"
    );
    crate::negative_assert_eq!(
        of(&JsValue::Bool(false)),
        ApiKey::Off,
        "#2428 a false apiKey is Off"
    );
    crate::negative_assert_eq!(
        of(&JsValue::String(JsString::from(""))),
        ApiKey::Off,
        "#2428 an empty-string apiKey is Off"
    );
    // A falsy number apiKey is Off — `!0`, `!(-0)` and `!NaN` are all true.
    crate::negative_assert_eq!(
        of(&JsValue::Number(0.0)),
        ApiKey::Off,
        "#2428 a falsy number apiKey is Off"
    );
    crate::negative_assert_eq!(
        of(&JsValue::Number(-0.0)),
        ApiKey::Off,
        "#2428 a falsy number apiKey is Off (-0)"
    );
    crate::negative_assert_eq!(
        of(&JsValue::Number(f64::NAN)),
        ApiKey::Off,
        "#2428 a falsy number apiKey is Off (NaN)"
    );
    // Truthy values `Buffer.from` refuses — every request answers 401.
    crate::negative_assert_eq!(
        of(&JsValue::Bool(true)),
        ApiKey::Never,
        "#2428 a boolean apiKey is Never"
    );
    crate::negative_assert_eq!(
        of(&JsValue::Number(7.0)),
        ApiKey::Never,
        "#2428 a truthy number apiKey is Never"
    );
    crate::negative_assert_eq!(
        of(&JsValue::Object(JsObject::new())),
        ApiKey::Never,
        "#2428 an object apiKey is Never"
    );
    // A truthy string is its UTF-8 bytes.
    crate::negative_assert_eq!(
        of(&JsValue::String(JsString::from("hi"))),
        ApiKey::Key(b"hi".to_vec()),
        "#2428 a string apiKey is its UTF-8 bytes"
    );
    // The empty array is truthy (`![]` is false) but holds zero bytes.
    crate::negative_assert_eq!(
        of(&JsValue::Array(vec![])),
        ApiKey::Key(vec![]),
        "#2428 an empty array apiKey is an empty Key"
    );
    // `Buffer.from(array)`'s per-element ToUint8: NaN/±Infinity are 0,
    // fractions truncate toward zero, negatives and out-of-range values
    // wrap modulo 256, strings go through ToNumber first.
    crate::negative_assert_eq!(
        of(&JsValue::Array(vec![
            JsValue::Number(256.0),
            JsValue::Number(257.0),
            JsValue::Number(-1.0),
            JsValue::Number(1.7),
            JsValue::Number(-1.7),
            JsValue::String(JsString::from("65")),
            JsValue::String(JsString::from("a")),
            JsValue::Null,
        ])),
        ApiKey::Key(vec![0, 1, 255, 1, 255, 65, 0, 0]),
        "#2428 an array apiKey wraps elements modulo 256"
    );
    // The boundary rows on the converter itself, so the wrap cannot drift
    // with the classification still pinned only at ASCII values.
    crate::negative_assert_eq!(to_uint8(256.0), 0, "#2428 ToUint8 wraps 256 to 0");
    crate::negative_assert_eq!(to_uint8(257.0), 1, "#2428 ToUint8 wraps 257 to 1");
    crate::negative_assert_eq!(to_uint8(-1.0), 255, "#2428 ToUint8 wraps -1 to 255");
    crate::negative_assert_eq!(to_uint8(1.7), 1, "#2428 ToUint8 truncates 1.7 to 1");
    crate::negative_assert_eq!(to_uint8(-1.7), 255, "#2428 ToUint8 wraps -1.7 to 255");
    crate::negative_assert_eq!(to_uint8(0.5), 0, "#2428 ToUint8 truncates 0.5 to 0");
    crate::negative_assert_eq!(to_uint8(f64::NAN), 0, "#2428 ToUint8 maps NaN to 0");
    crate::negative_assert_eq!(
        to_uint8(f64::INFINITY),
        0,
        "#2428 ToUint8 maps Infinity to 0"
    );
    crate::negative_assert_eq!(
        to_uint8(f64::NEG_INFINITY),
        0,
        "#2428 ToUint8 maps -Infinity to 0"
    );
}

/// #2428 review r1: the empty array is truthy (`![]` is false, so checkAuth's
/// gate stays closed) but holds zero bytes — its only possible Bearer match
/// is the empty token (`timingSafeEqual(empty, empty)` is true, Node v22.22.0
/// probe). A raw `Bearer ` (trailing space) never reaches that comparison on
/// either side — Node's and hyper's parsers both trim trailing OWS, so the
/// header arrives as `Bearer` and the prefix check fails — which is why the
/// empty-token limb is pinned by the classification table
/// (`ApiKey::Key(vec![])`). The empty-token wire cell below pins that trim
/// end-to-end in this suite (r2 boundary): the raw header carries the
/// trailing space, the parsed value must not. The remaining wire-level pin is
/// the closed gate: unlike `Off` (which answers 200 to the no-Bearer
/// request), an empty array apiKey must answer 401 to every request.
#[tokio::test]
async fn empty_array_api_key_keeps_the_gate_closed() {
    let turn_gateway = || {
        Some(gateway_from(
            &json!({"gateway": {"turns": [{"events": []}]}}),
            None,
            Arc::new(AtomicBool::new(false)),
        ) as Arc<dyn TurnGateway>)
    };
    let empty = start(
        ApiServerOptions {
            port: Some(JsValue::Number(0.0)),
            api_key: Some(JsValue::Array(vec![])),
            ..ApiServerOptions::default()
        },
        turn_gateway(),
    )
    .await;
    crate::negative_assert_eq!(
        exchange(empty.addr, &bearer_chat(None)).await.status,
        Some(401),
        "#2428 an empty array apiKey keeps the gate closed for a request without the Bearer key"
    );
    crate::negative_assert_eq!(
        exchange(empty.addr, &bearer_chat(Some("x"))).await.status,
        Some(401),
        "#2428 an empty array apiKey rejects a non-empty Bearer token"
    );
    // The raw header is `Bearer ` with the trailing space; both Node and
    // hyper trim trailing OWS, so the parsed value must arrive as `Bearer`
    // and the empty-token limb stays unreachable over the wire (r2 boundary:
    // pin the trim in this suite, not only in #1349's other channel).
    crate::negative_assert_eq!(
        exchange(empty.addr, &bearer_chat(Some(""))).await.status,
        Some(401),
        "#2428 a trailing-space Bearer header is trimmed to no token and rejected"
    );
    empty
        .handle
        .stop("test")
        .await
        .expect("#2428 api_server stop");
}

// #2301: the mount is the production construction seam — the startup
// assembly registers it through `production_loader_mounts` and calls
// `load_enabled_channels` (pilotdeckServer.ts:288-296, the loadEnabledChannels
// port). The mount forwards `extra.port` / `apiKey`; `resolve` folds them
// back into the env / `DEFAULT_PORT` chain, so a concrete port through the
// loader config reaches the real listener and the TS dist start line —
// `api-server: listening on http://127.0.0.1:<port>` — is observable. The
// port is an OS-assigned free port (bind :0, read, drop, rebind through the
// channel), the same trick the busy-port blocker uses, rather than a
// hard-coded constant that could collide on a shared runner.
#[tokio::test]
async fn loader_mount_builds_api_server_channel() {
    let mut mounts = crate::channel::loader::ChannelLoaderMounts::default();
    register(&mut mounts);
    assert!(mounts.is_registered("apiServer"), "#1333 loader mount key");
    let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("#2301 free-port probe");
    let port = probe.local_addr().expect("#2301 probe address").port();
    drop(probe);
    let adapters = pilotdeck_protocol::parse_json(
        &json!({
            "apiServer": {"enabled": true, "apiKey": "probe-key", "extra": {"port": port}},
            "telegram": {"enabled": false},
        })
        .to_string(),
    )
    .expect("#2301 adapters JSON");
    let mut errors = Vec::new();
    let channels = crate::channel::loader::load_enabled_channels(
        Some(&adapters),
        &crate::channel::loader::LoadEnabledChannelsOptions { pilot_home: None },
        &mounts,
        &mut |message| errors.push(message),
    );
    assert!(errors.is_empty(), "#2301 loader errors: {errors:?}");
    assert_eq!(
        channels.len(),
        1,
        "#2301 exactly the api_server channel loads"
    );
    assert_eq!(channels[0].channel_key().to_string(), "api_server");

    // The constructed adapter really listens through the mount's options: the
    // start line is the TS dist anchor and the port accepts a connection.
    let logs: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
    let sink = logs.clone();
    let handle = channels[0]
        .start(ChannelStartDeps {
            config: None,
            report_status: None,
            log: Some(Arc::new(move |level: &str, message: &str| {
                sink.lock()
                    .expect("#2301 log sink")
                    .push((level.to_owned(), message.to_owned()));
            })),
            turn_gateway: Some(gateway_from(
                &json!({"gateway": {"turns": [{"events": []}]}}),
                None,
                Arc::new(AtomicBool::new(false)),
            )),
        })
        .await
        .expect("#2301 loader-built api_server start");
    let listening = format!("api-server: listening on http://127.0.0.1:{port}");
    assert!(
        logs.lock()
            .expect("#2301 log sink")
            .iter()
            .any(|(level, message)| level == "info" && message == &listening),
        "#2301 the TS dist anchor line {listening:?} in {:?}",
        logs.lock().expect("#2301 log sink")
    );
    TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("#2301 the loader-built listener accepts connections");

    // #2428: the mount forwards `cfg.apiKey` raw and `resolve` keeps the TS
    // `??` order, so a configured key reaches checkAuth — a request without
    // the Bearer token and one with a wrong token answer 401, and only the
    // configured key reaches the gateway and answers 200.
    let address: std::net::SocketAddr = format!("127.0.0.1:{port}")
        .parse()
        .expect("#2428 loopback address");
    let missing = exchange(address, &bearer_chat(None)).await;
    crate::negative_assert_eq!(
        missing.status,
        Some(401),
        "#2428 a request without the Bearer key is rejected"
    );
    let wrong = exchange(address, &bearer_chat(Some("wrong-key"))).await;
    crate::negative_assert_eq!(
        wrong.status,
        Some(401),
        "#2428 a wrong Bearer token is rejected"
    );
    let correct = exchange(address, &bearer_chat(Some("probe-key"))).await;
    crate::negative_assert_eq!(
        correct.status,
        Some(200),
        "#2428 the configured Bearer key answers 200"
    );
    handle.stop("test").await.expect("#2301 api_server stop");
    assert!(
        TcpStream::connect(("127.0.0.1", port)).await.is_err(),
        "#2301 the listener is gone after stop"
    );
}
