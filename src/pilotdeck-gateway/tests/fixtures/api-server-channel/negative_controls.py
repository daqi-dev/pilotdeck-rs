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
REPLAY = "channel::api_server::tests::replays_real_ts_api_server_cells"
DUPLICATE = "channel::api_server::tests::duplicate_chat_is_busy_then_free"
OVERSIZED = "channel::api_server::tests::oversized_body_closes_without_response"
EXACT_LIMIT = "channel::api_server::tests::body_at_the_exact_limit_is_read_then_rejected_as_json"
RENDER_VISIBLE = "channel::api_server::tests::render_visible_failure_status_names_match_ts"
DEFAULT_PORT_TEST = "channel::api_server::tests::unconfigured_port_falls_back_to_the_ts_default"

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
        "auth-bypass",
        SERVER,
        "    if !shared.api_key.is_empty() && !bearer_matches(&authorization, &shared.api_key) {",
        "    if false && !shared.api_key.is_empty() && !bearer_matches(&authorization, &shared.api_key) {",
        REPLAY,
        "#1333 auth request 0: status differs",
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
        RENDER,
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
