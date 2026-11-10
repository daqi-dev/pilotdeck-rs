#!/usr/bin/env python3
"""#1333 negative controls for the api-server channel port.

Each mutation is applied to the original source, the *positive* control must
pass first, then the named assertion must fail with the bound `#1333` marker.
The shared guard restores and byte-verifies every file after each row.

Run after the #1333 commit exists (new files are compared against HEAD):

    CARGO_TARGET_DIR=<abs target> python3 \
        src/pilotdeck-gateway/tests/fixtures/api-server-channel/negative_controls.py \
        _tmp/<date>-issue-1333/negatives

Run it only after the #1333 sources are committed: `mutating_sources` compares
the working tree against HEAD. Markers are declared as a literal `mutations`
tuple table so `anchor_selfcheck.py` can verify every anchor statically.
"""
from pathlib import Path
import os
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_suite import Case, run_cases
from negative_process import mutating_sources, run_owned, wait_for_slot

# Referenced for the shared-runner contract; run_cases owns the guard calls.
_SHARED_RUNNER = (mutating_sources, run_owned, wait_for_slot)

ROOT = Path(__file__).resolve().parents[5]
SERVER = ROOT / "src/pilotdeck-gateway/src/channel/api_server/server.rs"
MOD = ROOT / "src/pilotdeck-gateway/src/channel/api_server/mod.rs"
RENDER = ROOT / "src/pilotdeck-gateway/src/channel/api_server/render.rs"
# #1615: the visible-failure event-name table moved from `render.rs` into the
# shared `agentStatus` port the webhook lane consumes too.
AGENT_STATUS = ROOT / "src/pilotdeck-gateway/src/channel/agent_status.rs"
REPLAY = "channel::api_server::tests::replays_real_ts_api_server_cells"
DUPLICATE = "channel::api_server::tests::duplicate_chat_is_busy_then_free"
OVERSIZED = "channel::api_server::tests::oversized_body_closes_without_response"
EXACT_LIMIT = "channel::api_server::tests::body_at_the_exact_limit_is_read_then_rejected_as_json"
PARTIAL_BODY = "channel::api_server::tests::partial_body_is_answered_at_the_body_read_deadline"
BODY_DEADLINE = "channel::api_server::tests::body_read_deadline_defaults_to_the_node_request_timeout"
RENDER_VISIBLE = "channel::api_server::tests::render_visible_failure_status_names_match_ts"
DEFAULT_PORT_TEST = "channel::api_server::tests::unconfigured_port_falls_back_to_the_ts_default"
STREAM_SUBMIT_ERR = "channel::api_server::tests::submit_error_stream_turn_emits_channel_submit_failed"
BUFFERED_SUBMIT_ERR = "channel::api_server::tests::submit_error_buffered_turn_returns_submit_failed"
API_KEY_CLASSIFICATION = "channel::api_server::tests::api_key_classification_pins_the_checkauth_table"

mutations = [
    (
        "health-path",
        SERVER,
        '} else if path == "/health" {',
        '} else if path == "/healthz" {',
        REPLAY,
        "#1333 method-dispatch request 0: status differs",
    ),
    (
        "models-path",
        SERVER,
        '} else if path == "/v1/models" {',
        '} else if path == "/v1/model" {',
        REPLAY,
        "#1333 method-dispatch request 3: status differs",
    ),
    (
        # #2428: the gate reads `ApiKey::is_off` now — a truthy key (string,
        # array, or never-matching) always checks the Bearer token.
        "auth-bypass",
        SERVER,
        "    if !shared.api_key.is_off() && !bearer_matches(&authorization, &shared.api_key) {",
        "    if false && !shared.api_key.is_off() && !bearer_matches(&authorization, &shared.api_key) {",
        REPLAY,
        "#1333 auth request 0: status differs",
    ),
    (
        # #2428: a falsy number must classify as `Off` — TS's `!0` opens the
        # gate, so folding it into `Never` would answer 401 where TS answers
        # 200.
        "api-key-falsy-number-never",
        MOD,
        "            JsValue::Number(number) if *number == 0.0 || number.is_nan() => Self::Off,",
        "            JsValue::Number(number) if *number == 0.0 || number.is_nan() => Self::Never,",
        API_KEY_CLASSIFICATION,
        "#2428 a falsy number apiKey is Off",
    ),
    (
        # #2428: `Buffer.from` wraps array elements modulo 256 — a plain
        # truncating cast instead maps -1 to 0 and 256 to 255 (float-to-int
        # casts saturate), drifting from Node's ToUint8.
        "api-key-to-uint8-truncating-cast",
        MOD,
        "    number.trunc().rem_euclid(256.0) as u8",
        "    number.trunc() as u8",
        API_KEY_CLASSIFICATION,
        "#2428 an array apiKey wraps elements modulo 256",
    ),
    (
        "cors-exact-vary",
        SERVER,
        'values.push(("vary", HeaderValue::from_static("Origin")));',
        '// mutant: drop Vary for an exact origin match',
        REPLAY,
        "#1333 cors-exact request 0: headers differ",
    ),
    (
        "sse-done",
        SERVER,
        'format!("data: {}\\n\\ndata: [DONE]\\n\\n", json_text(&body))',
        'format!("data: {}\\n\\n", json_text(&body))',
        REPLAY,
        "#1333 stream request 0: body differs",
    ),
    (
        "stream-session-header",
        SERVER,
        "        headers.insert(SESSION_HEADER, header_value(chat_id));\n",
        "        let _ = chat_id; // mutant: drop X-Hermes-Session-Id\n",
        REPLAY,
        "#1333 stream request 0: headers differ",
    ),
    (
        "buffered-session-header",
        SERVER,
        '        .insert(SESSION_HEADER, header_value(chat_id));',
        '        .insert(SESSION_HEADER, header_value("mutant"));',
        REPLAY,
        "#1333 buffered request 0: headers differ",
    ),
    (
        "visible-failure-name",
        AGENT_STATUS,
        '    "turn_failed",',
        '    "turn_failed_x",',
        RENDER_VISIBLE,
        "#1333 a listed status name is visible without an error severity",
    ),
    (
        "gateway-503",
        SERVER,
        "        StatusCode::SERVICE_UNAVAILABLE,",
        "        StatusCode::BAD_GATEWAY,",
        REPLAY,
        "#1333 auth request 2: status differs",
    ),
    (
        "active-chat-guard",
        SERVER,
        "    if !admit_chat(&shared.active, &chat_id) {",
        "    if false && !admit_chat(&shared.active, &chat_id) {",
        DUPLICATE,
        "#1333 duplicate chat is rejected",
    ),
    (
        "body-limit",
        MOD,
        "1_000_000",
        "10_000_000",
        OVERSIZED,
        "#1333 oversized body closes with zero bytes",
    ),
    (
        # #1759 E: raise the cap by one so `MAX_REQUEST_BYTES + 1` (1000002)
        # exceeds the literal `REPLAY_BODY_BUDGET` (1000001). While the budget
        # was derived from the cap it tracked this mutation instead of catching
        # it: the budget became 1000002, the cap-floor check passed, and
        # `recorded_body_refuses_an_unbounded_count` kept refusing
        # `<9000000 bytes>` — no test named the moved cap. The tie is now
        # asserted, so a moved product cap is reported by name.
        "replay-budget-vs-cap",
        MOD,
        "1_000_000",
        "1_000_001",
        REPLAY,
        "#1759 the product cap floor is 1000002 bytes but `REPLAY_BODY_BUDGET` is only 1000001",
    ),
    (
        "body-limit-exact",
        SERVER,
        "if out.len() + data.len() > MAX_REQUEST_BYTES {",
        "if out.len() + data.len() >= MAX_REQUEST_BYTES {",
        EXACT_LIMIT,
        "#1333 a body of exactly MAX_REQUEST_BYTES is read",
    ),
    (
        "default-port",
        MOD,
        ".unwrap_or(DEFAULT_PORT),",
        ".unwrap_or(0.0),",
        DEFAULT_PORT_TEST,
        "#1333 an unconfigured port must default to 8642",
    ),
    (
        # #1620: remove Node's `requestTimeout` deadline from the body read.
        # Same type as the timed call, so the match arms stay unchanged and the
        # mutant builds; only the deadline disappears.
        "api-server-body-read-no-timeout",
        SERVER,
        "tokio::time::timeout(shared.body_read_timeout, read_body(request.into_body())).await",
        "Ok::<_, tokio::time::error::Elapsed>(read_body(request.into_body()).await)",
        PARTIAL_BODY,
        "#1333 a stalled body is answered at the body-read deadline",
    ),
    (
        # #1620: let the requestTimeout 408 go through `finish` — the only place
        # this lane adds `Date`. Node's 51-byte `requestTimeoutResponse` has no
        # `Date`, so status / `Connection: close` / empty body all still pass
        # and only the "no Date" assertion turns red.
        "api-server-timeout-response-date",
        SERVER,
        "return Ok(crate::channel::webhook::node_request_timeout_response());",
        "return Ok(finish(crate::channel::webhook::node_request_timeout_response(), keep_alive));",
        PARTIAL_BODY,
        "#1333 the requestTimeout 408 must not gain a Date header",
    ),
    (
        # #1620: shorten the default deadline. Every other check injects its own
        # 200 ms deadline or skips the cell, so without this row the constant
        # could drift from Node's 300 s default with the suite still green.
        "api-server-body-read-default-timeout",
        MOD,
        "pub const DEFAULT_BODY_READ_TIMEOUT: Duration = Duration::from_secs(300);",
        "pub const DEFAULT_BODY_READ_TIMEOUT: Duration = Duration::from_secs(30);",
        BODY_DEADLINE,
        "#1620 the body-read deadline must stay at Node's default requestTimeout",
    ),
    (
        # #1737: the stream_turn submit-reject arm is mutated into a success
        # close (sse_chunk + sse_done instead of the channel_submit_failed
        # frame), so the bound frame assertions turn red. `let _ = &body;`
        # keeps the mutant warning-free (body stays constructed above).
        "stream-submit-err-arm",
        SERVER,
        "            let _ = sender.send(sse_error(&body));\n            return;",
        "            let _ = &body;\n"
        "            let _ = sender.send(sse_chunk(&shared.model_name, &JsString::from(\"mutant\")));\n"
        "            let _ = sender.send(sse_done(&shared.model_name));\n"
        "            return;",
        STREAM_SUBMIT_ERR,
        "#1737 a rejected stream submit emits channel_submit_failed",
    ),
    (
        # #1737: the buffered_turn submit-reject arm is mutated into a 200
        # answer (submit_failed -> error_response with StatusCode::OK), so the
        # bound status assertion turns red.
        "buffered-submit-err-arm",
        SERVER,
        "return submit_failed(&error.to_string());",
        "return error_response(StatusCode::OK, \"channel_submit_failed\", &error.to_string(), \"channel_submit_failed\", Some(\"server_error\"), \"PilotDeck failed before this API request could finish. Retry the request; if it repeats, check the API server and gateway logs.\", \"channel\");",
        BUFFERED_SUBMIT_ERR,
        "#1737 a rejected buffered submit answers 500",
    ),
    (
        # #1683: disconnect the shared parser-level admission from this lane.
        # `FROB /health` and the Host-less HTTP/1.1 request then fall through
        # to the `/health` route and answer 200, so the recorded 400 cells go
        # red. The anchor is the whole call site, not a mutant switch: the
        # gate must be *wired here*, and a lane that silently drops the call
        # is exactly the #1683 defect.
        "api-server-parser-admission",
        SERVER,
        "    if let Some(rejection) = crate::native_http::node_parser_admission(&request)? {\n"
        "        return Ok(rejection);\n"
        "    }\n",
        "    // mutant: parser admission disconnected\n",
        REPLAY,
        "#1333 unknown-method request 0: status differs",
    ),
]

def mutate(text, scope, replacements):
    del scope
    for old, new in replacements:
        if old not in text:
            raise RuntimeError(f"#1333 anchor disappeared during mutation: {old!r}")
        text = text.replace(old, new, 1)
    return text


def main():
    if len(sys.argv) < 2:
        raise SystemExit(
            "usage: negative_controls.py <new-output-directory> [mutation ...]"
        )
    target = Path(sys.argv[1]).resolve()
    wanted = set(sys.argv[2:])
    unknown = wanted - {row[0] for row in mutations}
    if unknown:
        raise SystemExit(f"#1333 unknown mutation(s): {', '.join(sorted(unknown))}")
    environment = dict(os.environ)
    if not environment.get("CARGO_TARGET_DIR"):
        raise SystemExit("#1333 CARGO_TARGET_DIR must be set (owned build target)")
    environment.setdefault("CARGO_PROFILE_DEV_DEBUG", "line-tables-only")
    environment.setdefault("CARGO_PROFILE_TEST_DEBUG", "line-tables-only")

    selected = [Case(*row) for row in mutations if not wanted or row[0] in wanted]
    for case in selected:
        path = ROOT / case.path
        occurrences = path.read_text().count(case.old)
        if occurrences != 1:
            raise SystemExit(
                f"#1333 anchor expired for {case.mutation}: "
                f"expected 1 occurrence, found {occurrences}"
            )
    target.mkdir(parents=True, exist_ok=False)
    return run_cases(ROOT, selected, target, environment, mutate)


if __name__ == "__main__":
    raise SystemExit(main())
