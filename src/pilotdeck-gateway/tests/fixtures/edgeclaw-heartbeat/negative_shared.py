#!/usr/bin/env python3
"""Shared-helper source mutations for edgeclaw-heartbeat (#852 P3).

Data-only table: ``negative.py`` imports ``mutations`` and runs it under the
same ``mutating_sources`` lock as the helpers table, so one command still runs
all 36 cases.

This is a separate file because main's ``anchor_selfcheck.py`` registers
``edgeclaw-heartbeat/negative.py`` as a single-source script targeting
``heartbeat/helpers.rs`` (IMPLICIT); the three mutants here replace anchors in
``js_value.rs``, ``storage.rs`` and ``text.rs`` and therefore use the generic
``(name, path, before, after, test, anchor)`` table that the same checker
supports for every other fixture script.
"""
from pathlib import Path

root = Path(__file__).resolve().parents[5]
text = root / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/text.rs'
storage = root / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/storage.rs'
js_value = root / 'src/pilotdeck-gateway/src/js_value.rs'

GENERIC = 'heartbeat_helpers_match_ts'
DEPTH = 'kv_detail_depth_error_is_reported_not_panicking'

# (name, path, before, after, test, specific failure anchor)
mutations = [
    # 10. The shared depth bound stops rejecting 2001 containers.
    ('kv-depth-bound', js_value,
     'pub(crate) const MAX_ARRAY_DEPTH: usize = 2000;',
     'pub(crate) const MAX_ARRAY_DEPTH: usize = 3001;',
     DEPTH, "Rust's shared 2000-container bound rejects 2001"),
    # 11. Heartbeat still uses the shared truthiness helper.
    ('shared-truthy', storage,
     '        JsValue::Array(_) | JsValue::Object(_) => true,',
     '        JsValue::Array(_) | JsValue::Object(_) => false,',
     GENERIC, 'step/createStep'),
    # 12. Heartbeat routes its joins through the shared text helper.
    ('shared-join-text', text,
     '            units.extend(separator.encode_utf16());',
     '            units.extend(separator.encode_utf16().skip(1));',
     GENERIC, 'routing/buildCandidateRoutingQuery'),
]
