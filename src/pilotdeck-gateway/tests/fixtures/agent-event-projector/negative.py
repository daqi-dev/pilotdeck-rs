#!/usr/bin/env python3
"""Mutation controls for the agent-event projector fixture.

Every mutant rewrites a copy of `projector.rs` (or the oracle hash ledger) in an
isolated /tmp crate and must make the real replay fail with an assertion naming
the affected case/test. The baseline copy must pass. No source in the worktree
is modified.
"""
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile

output = Path(sys.argv[1]).resolve()
output.mkdir(parents=True, exist_ok=True)
fixture = Path(__file__).resolve().parent
rust = fixture.parents[3]
originals = {
    "projector.rs": (rust / "pilotdeck-gateway/src/runtime/turn/projector.rs").read_text(),
    "projector_tests.rs": (
        rust / "pilotdeck-gateway/src/runtime/turn/projector_tests.rs"
    ).read_text(),
    "oracle.json": (fixture / "oracle.json").read_text(),
}
oracle = originals["oracle.json"]
env = dict(os.environ, CARGO_NET_OFFLINE="true")
assert env.get("CARGO_HOME") and env.get("PDRS_BASELINE")

# Projector hash ledger entry; a single hex digit is flipped below.
PROJECTOR_HASH = "8236f898bec8b263827b8c66f27ac190cdd497baa0ac27723637e34847832225"

SYNC_WRITE_BEFORE = """        let failed = match sender.as_ref() {
            Some(tx) => tx.send(Box::new(job)).is_err(),
            None => return false,
        };"""
SYNC_WRITE_AFTER = """        let failed = match sender.as_ref() {
            Some(_tx) => {
                job();
                false
            }
            None => return false,
        };"""

# Restore the pre-fix recursive traversal (HEAD 40628dc0). Renaming the
# iterative body and routing calls through the old recursion is the actual P0
# regression: the 30,000-deep case must then die on the native stack instead of
# returning a catchable RangeError.
OLD_SANITIZE_BEFORE = (
    "fn sanitize_tool_data_value(value: &JsValue) -> ProjectionResult<JsValue> {"
)
OLD_SANITIZE_AFTER = """fn old_recursive_sanitize(value: &JsValue) -> JsValue {
    match value {
        JsValue::String(value) => limit_tool_data_string(value),
        JsValue::Array(items) => {
            JsValue::Array(items.iter().map(old_recursive_sanitize).collect())
        }
        JsValue::Object(object) => {
            let mut output = JsObject::new();
            for (key, value) in object.entries() {
                output.insert(key.clone(), old_recursive_sanitize(value));
            }
            JsValue::Object(output)
        }
        _ => value.clone(),
    }
}
fn sanitize_tool_data_value(value: &JsValue) -> ProjectionResult<JsValue> {
    Ok(old_recursive_sanitize(value))
}
fn unused_iterative_sanitize_tool_data_value(value: &JsValue) -> ProjectionResult<JsValue> {"""

# Reintroduce the pre-fix bounded `String()` conversion: nested arrays at
# depth 512+ degraded to `[object Object]` instead of joining like TS.
DEEP_CLAMP_BEFORE = """fn checked_string_units(value: &JsValue) -> ProjectionResult<Vec<u16>> {
    match crate::js_value::to_string_units_checked(value) {
        Ok(units) => Ok(units),
        Err(crate::js_value::ToStringError::StackOverflow) => Err(ProjectionError::range_error(
            crate::js_value::StackOverflow::MESSAGE,
        )),
        Err(crate::js_value::ToStringError::NotPrimitive) => Err(ProjectionError::type_error(
            crate::js_value::ToStringError::NOT_PRIMITIVE,
        )),
    }
}"""
DEEP_CLAMP_AFTER = """fn checked_string_units(value: &JsValue) -> ProjectionResult<Vec<u16>> {
    fn join(value: &JsValue, out: &mut Vec<u16>, depth: usize) {
        match value {
            JsValue::Array(children) => {
                if depth >= 512 {
                    out.extend("[object Object]".encode_utf16());
                    return;
                }
                for (index, child) in children.iter().enumerate() {
                    if index > 0 {
                        out.push(u16::from(b','));
                    }
                    if !matches!(child, JsValue::Undefined | JsValue::Null) {
                        join(child, out, depth + 1);
                    }
                }
            }
            JsValue::String(value) => out.extend_from_slice(value.units()),
            JsValue::Undefined => out.extend("undefined".encode_utf16()),
            JsValue::Null => out.extend("null".encode_utf16()),
            JsValue::Bool(value) => out.extend(value.to_string().encode_utf16()),
            JsValue::Number(value) => {
                out.extend(pilotdeck_protocol::number::number_to_string(*value).encode_utf16())
            }
            JsValue::Object(_) => out.extend("[object Object]".encode_utf16()),
        }
    }
    let mut out = Vec::new();
    join(value, &mut out, 0);
    Ok(out)
}"""

mutants = [
    # Existing round-0 controls.
    (
        "drop-timeline-projection",
        "projector.rs",
        'if !attachment && truthy(field(event, "timeline")) {',
        'if false && !attachment && truthy(field(event, "timeline")) {',
        "turn-started-with-timeline-and-boundary",
    ),
    (
        "drop-appended-run-id",
        "projector.rs",
        "if !has_run_id {",
        "if false && !has_run_id {",
        "steer-applied-with-block-timeline",
    ),
    (
        "preview-counts-scalars-not-utf16",
        "projector.rs",
        "units.len() - MAX_GATEWAY_TOOL_RESULT_PREVIEW_CHARS",
        "String::from_utf16_lossy(units).chars().count() - MAX_GATEWAY_TOOL_RESULT_PREVIEW_CHARS",
        "tool-result-preview-truncation-utf16",
    ),
    (
        "attach-read-file-images",
        "projector.rs",
        'if !is_type(tool_name, "read_file") {',
        "if true {",
        "tool-result-read-file-image-has-no-attachment",
    ),
    (
        "skip-tool-data-string-limit",
        "projector.rs",
        "if value.len_utf16() <= MAX_GATEWAY_TOOL_DATA_STRING_CHARS {",
        "if true {",
        "tool-result-data-string-truncation",
    ),
    (
        "ignore-forward-subagent-text",
        "projector.rs",
        "if forward_subagent_text {",
        "if !forward_subagent_text {",
        "subagent-model-event-text-forwarded",
    ),
    (
        "null-total-not-nullish",
        "projector.rs",
        "    } else if nullish(explicit_total) {",
        "    } else if explicit_total.is_undefined() {",
        "context-budget-null-fallbacks",
    ),
    (
        "skip-provider-raw-truncation",
        "projector.rs",
        "if units.len() > 1_200 {",
        "if false {",
        "model-recovery-failed-with-truncated-raw",
    ),
    (
        "timeline-on-attachment",
        "projector.rs",
        'if !attachment && truthy(field(event, "timeline")) {',
        'if truthy(field(event, "timeline")) {',
        "tool-result-large-with-images-and-file",
    ),
    # Fix round 1: one control per P0/P1 (plus the P2 path normalization).
    (
        "restore-old-recursive-sanitize",
        "projector.rs",
        OLD_SANITIZE_BEFORE,
        OLD_SANITIZE_AFTER,
        ("has overflowed its stack", "should throw"),
    ),
    (
        "skip-tool-data-depth-guard",
        "projector.rs",
        "            if depth > MAX_ARRAY_DEPTH {",
        "            if false && depth > MAX_ARRAY_DEPTH {",
        "deep-tool-data-array",
    ),
    (
        "sync-advisory-write",
        "projector.rs",
        SYNC_WRITE_BEFORE,
        SYNC_WRITE_AFTER,
        "advisory_persist_returns_before_the_write_completes",
    ),
    (
        "keep-__proto__-own-property",
        "projector.rs",
        'while next < entries.len() && entries[next].0.equals_str("__proto__") {',
        'while false && next < entries.len() && entries[next].0.equals_str("__proto__") {',
        "tool-result-data-drops-proto-object-value",
    ),
    (
        "ignore-missing-receiver-errors",
        "projector.rs",
        "        return Err(missing_property_error(base, name));",
        "        return Ok(field(base, name));",
        "error-null-event",
    ),
    (
        "unbounded-json-stringify",
        "projector.rs",
        "    crate::js_value::json_stringify(value)",
        "    pilotdeck_protocol::stringify_json(value).map_err(|_| crate::js_value::StackOverflow)",
        "deep_provider_raw_and_args_preview_fall_back_to_ts_string_conversion",
    ),
    (
        "skip-path-normalization",
        "projector.rs",
        "            Component::ParentDir => {\n                normalized.pop();\n            }",
        "            Component::ParentDir => {\n                normalized.push(component.as_os_str());\n            }",
        # The renamed case now only exercises in-root `.` normalization; the
        # `..` case that still reaches `ParentDir` is the escape test.
        "artifact_session_parent_dir_escape_is_refused",
    ),
    # Fix round 2 (P1): the `..` root guard must stay wired. Without it the
    # escaped path is queued and handed back as `resultPath`.
    (
        "drop-artifact-escape-guard",
        "projector.rs",
        '''        if path.strip_prefix(&root).is_err() {
            return Ok(None);
        }
''',
        "",
        "artifact_session_parent_dir_escape_keeps_projection_without_a_path",
    ),
    # Fix round 2: one control per P1 (silent path fields, queue overflow,
    # detached advisory writer, deep-array `String()` clamp).
    (
        "silent-missing-image-mime",
        "projector.rs",
        '''    let lowered: Vec<u16> = string_method(mime_type, "mimeType.toLowerCase")?
        .units()
        .iter()''',
        '''    let lowered: Vec<u16> = checked_string_units(mime_type)?
        .iter()''',
        "error-tool-result-image-missing-mime-type",
    ),
    (
        "silent-missing-image-tool-name",
        "projector.rs",
        '    Ok(safe_path_part(string_method(value, "value.trim")?.units()))',
        '    Ok(safe_path_part(&checked_string_units(value)?))',
        "error-tool-result-image-missing-tool-name",
    ),
    (
        "silent-missing-file-path",
        "projector.rs",
        '''    let value = string_method(value, expression)?;
    Ok(last_path_segment(value.units()))''',
        '''    let value = checked_string_units(value)?;
    Ok(last_path_segment(&value))''',
        "error-tool-result-file-missing-path",
    ),
    (
        "silent-missing-media-path",
        "projector.rs",
        '''                            JsValue::String(last_path_segment_js(
                                field(block, "path"),
                                "block.path.split",
                            )?),''',
        '                            JsValue::String(last_path_segment(&checked_string_units(field(block, "path"))?)),',
        "error-tool-results-projected-media-reference-missing-path",
    ),
    (
        "silent-missing-image-data",
        "projector.rs",
        '''        // TS reads `content.data.length`; that throws when the field is absent.
        push_units(&mut out, &js_length(field(item, "data"))?)?;''',
        '        push_units(&mut out, &JsValue::Number(checked_string_units(field(item, "data"))?.len() as f64))?;',
        "error-tool-result-image-missing-data",
    ),
    (
        "silent-missing-canonical-image-data",
        "projector.rs",
        '''        } else {
            push_units(&mut out, &js_length(field(block, "data"))?)?;
            push_str(&mut out, " base64 characters");''',
        '''        } else {
            push_units(&mut out, &JsValue::Number(checked_string_units(field(block, "data"))?.len() as f64))?;
            push_str(&mut out, " base64 characters");''',
        "error-tool-results-projected-tool-result-image-missing-data",
    ),
    (
        "deep-array-512-clamp",
        "projector.rs",
        DEEP_CLAMP_BEFORE,
        DEEP_CLAMP_AFTER,
        "tool-result-deep-array-content-text",
    ),
    (
        "drop-advisory-writes",
        "projector.rs",
        "            Some(tx) => tx.send(Box::new(job)).is_err(),",
        "            Some(_tx) => false,",
        "every_advisory_write_is_eventually_persisted",
    ),
    (
        "drop-joins-writes",
        "projector.rs",
        "impl ArtifactWrites {",
        '''impl Drop for ArtifactWrites {
    fn drop(&mut self) {
        let (done_tx, done_rx) = mpsc::channel();
        if let Some(sender) = self.sender.lock().unwrap().as_ref() {
            let _ = sender.send(Box::new(move || {
                let _ = done_tx.send(());
            }));
            let _ = done_rx.recv();
        }
    }
}

impl ArtifactWrites {''',
        "dropping_the_store_does_not_join_a_blocked_artifact_write",
    ),
    # Fix round 3 (gate3 P1): present-but-non-string wire fields must follow
    # the TS method/property call, not silent coercion.
    (
        "coerce-trim-non-string",
        "projector.rs",
        '    Ok(safe_path_part(string_method(value, "value.trim")?.units()))',
        '''    let value = match value {
        JsValue::String(value) => value,
        JsValue::Undefined | JsValue::Null => {
            return Err(missing_property_error(value, "trim"));
        }
        other => return Ok(safe_path_part(&checked_string_units(other)?)),
    };
    Ok(safe_path_part(value.units()))''',
        "type-tool-result-image-tool-name-number",
    ),
    (
        "length-from-string-coercion",
        "projector.rs",
        '''        JsValue::String(value) => JsValue::Number(value.len_utf16() as f64),
        JsValue::Array(items) => JsValue::Number(items.len() as f64),
        JsValue::Object(object) => object.get("length").cloned().unwrap_or(JsValue::Undefined),
        JsValue::Number(_) | JsValue::Bool(_) => JsValue::Undefined,''',
        '''        JsValue::String(value) => JsValue::Number(value.len_utf16() as f64),
        other => JsValue::Number(checked_string_units(other)?.len() as f64),''',
        # Round 4 added an earlier case that the coercion breaks first (an
        # object `data.length` whose value has an own `toString`); the round-3
        # number case fails the same way right after it.
        "error-tool-result-image-data-length-to-string-throws",
    ),
    (
        "coerce-persist-session-id",
        "projector.rs",
        '        session_id: field(event, "sessionId").clone(),',
        '        session_id: checked_js_text(field(event, "sessionId"))?,',
        "persist-session-id-number",
    ),
    (
        "swallow-default-store-path-error",
        "projector.rs",
        '        let session_id = safe_path_part_js(&input.session_id)?;',
        '        let session_id = safe_path_part_js(&input.session_id).unwrap_or_else(|_| "value".to_string());',
        "default_store_throws_on_non_string_path_part_like_ts",
    ),
    # Fix round 4: join renders nullish elements as "", nullish array
    # elements throw on the first property read, `String()` throws for an own
    # `toString`, `+` converts via ToPrimitive, and V8 prints the full
    # property-access expression for a non-array method receiver.
    # (Round 5 unified both content-text join paths behind one `Deferred`
    # branch in `join_content_texts`, so the former literal/canonical pair of
    # nullish mutants is now a single mutant covering both paths.)
    (
        "join-nullish-literal",
        "projector.rs",
        '''            ContentTextPart::Deferred(value) => {
                if !nullish(&value) {
                    out.extend(checked_string_units(&value)?);
                }
            }''',
        '''            ContentTextPart::Deferred(value) => {
                out.extend(checked_string_units(&value)?);
            }''',
        "tool-result-text-nullish-values",
    ),
    (
        "silent-null-call-timeline",
        "projector.rs",
        '                let timeline = required(call, "timeline")?;',
        '                let timeline = field(call, "timeline");',
        "error-tool-calls-null-call",
    ),
    (
        "silent-null-steer-block",
        "projector.rs",
        '''        let timeline = required(block, "timeline")?;
        if truthy(timeline) {''',
        '''        let timeline = field(block, "timeline");
        if truthy(timeline) {''',
        "error-steer-content-null-block",
    ),
    (
        "silent-null-assistant-block",
        "projector.rs",
        '        let block_type = required(block, "type")?;',
        '        let block_type = field(block, "type");',
        "error-assistant-content-null-block",
    ),
    (
        "silent-null-subagent-call",
        "projector.rs",
        '                let id = required(call, "id")?;',
        '                let id = field(call, "id");',
        "error-subagent-tool-calls-null-call",
    ),
    (
        "unchecked-to-string",
        "projector.rs",
        '''    match crate::js_value::to_string_units_checked(value) {
        Ok(units) => Ok(units),
        Err(crate::js_value::ToStringError::StackOverflow) => Err(ProjectionError::range_error(
            crate::js_value::StackOverflow::MESSAGE,
        )),
        Err(crate::js_value::ToStringError::NotPrimitive) => Err(ProjectionError::type_error(
            crate::js_value::ToStringError::NOT_PRIMITIVE,
        )),
    }''',
        '''    crate::js_value::to_string_units(value)
        .map_err(|_| ProjectionError::range_error(crate::js_value::StackOverflow::MESSAGE))''',
        "error-tool-result-text-to-string-throws",
    ),
    (
        "js-add-skip-to-primitive",
        "projector.rs",
        '''    let left = js_to_primitive(left)?;
    let right = js_to_primitive(right)?;''',
        '''    let left = left.clone();
    let right = right.clone();''',
        "context-budget-array-and-string-addition",
    ),
    (
        "bare-method-expression-flat-map",
        "projector.rs",
        '"event.message.content.flatMap",',
        '"flatMap",',
        "error-assistant-content-flat-map-number",
    ),
    (
        "bare-method-expression-map",
        "projector.rs",
        '''    store: &dyn GatewayToolResultArtifactStorePort,
) -> ProjectionResult<Vec<JsValue>> {
    let result = field(event, "result");
    let content = required_array(required(result, "content")?, "event.result.content.map")?;''',
        '''    store: &dyn GatewayToolResultArtifactStorePort,
) -> ProjectionResult<Vec<JsValue>> {
    let result = field(event, "result");
    let content = required_array(required(result, "content")?, "map")?;''',
        "error-tool-result-content-map-number",
    ),
    (
        "bare-method-expression-flatten",
        "projector.rs",
        'required_array(required(block, "content")?, "content.map")?',
        'required_array(required(block, "content")?, "map")?',
        "error-flatten-content-map-number",
    ),
    # Fix round 5: `contentToText`/`toolResultContentBlockToText` return the
    # text field raw, so its conversion only happens in `join`, after the whole
    # map — a later element's map-phase error must win. The facade's default
    # path must also stay on the process-level projector (one bounded writer).
    (
        "eager-text-conversion",
        "projector.rs",
        '''        // `contentToText` returns `content.text` unchanged; the nullish check
        // and ToString happen in `Array.prototype.join`, after the whole map.
        return Ok(ContentTextPart::Deferred(field(item, "text").clone()));''',
        '''        let value = field(item, "text");
        if !nullish(value) {
            return Ok(ContentTextPart::Converted(checked_string_units(value)?));
        }
        return Ok(ContentTextPart::Converted(Vec::new()));''',
        "error-tool-result-join-after-null-item",
    ),
    (
        "eager-text-conversion-canonical",
        "projector.rs",
        '''        // Join element like `tool_result_content_text`: the raw field comes
        // back and only `Array.prototype.join` converts it, after the map.
        return Ok(ContentTextPart::Deferred(field(block, "text").clone()));''',
        '''        let value = field(block, "text");
        if !nullish(value) {
            return Ok(ContentTextPart::Converted(checked_string_units(value)?));
        }
        return Ok(ContentTextPart::Converted(Vec::new()));''',
        "error-tool-results-projected-join-after-null-item",
    ),
    (
        "per-call-default-projector",
        "projector.rs",
        '''        let projector = default_projector();
        #[cfg(test)]
        DEFAULT_FACADE_PROJECTOR_ADDR.store(
            std::ptr::from_ref(projector).addr(),
            std::sync::atomic::Ordering::SeqCst,
        );
        projector.project(&input)''',
        '''        let projector = GatewayAgentEventProjector::new(options);
        #[cfg(test)]
        DEFAULT_FACADE_PROJECTOR_ADDR.store(0, std::sync::atomic::Ordering::SeqCst);
        projector.project(&input)''',
        "map_agent_event_default_calls_share_one_artifact_writer",
    ),
    (
        "tampered-source-hash",
        "oracle.json",
        PROJECTOR_HASH,
        PROJECTOR_HASH[:-1] + ("0" if PROJECTOR_HASH[-1] != "0" else "1"),
        "oracle hash for",
    ),
]


def run_tests(command):
    """Run cargo test in its own session so a timeout kills the whole tree."""
    process = subprocess.Popen(
        command,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        start_new_session=True,
    )
    try:
        stdout, stderr = process.communicate(timeout=900)
        return process.returncode, stdout + stderr
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)
        stdout, stderr = process.communicate()
        return -9, stdout + stderr + "\nTIMEOUT after 900s\n"


summary = []
with tempfile.TemporaryDirectory(prefix="pdrs-aep-negative-", dir="/tmp") as temp:
    scratch = Path(temp)
    env["CARGO_TARGET_DIR"] = str(scratch / "target")
    (scratch / "src/runtime/turn").mkdir(parents=True)
    (scratch / "tests/fixtures/agent-event-projector").mkdir(parents=True)
    manifest = (
        '[package]\nname="pdrs-aep-negative"\nversion="0.0.0"\nedition="2024"\n'
        "[dependencies]\n"
        f"pilotdeck-protocol={{path={json.dumps(str(rust / 'pilotdeck-protocol'))}}}\n"
        'sha2="=0.10.9"\n'
        'uuid={version="=1.27.0",features=["v4"]}\n'
        'libc="=0.2.190"\n'
        'rustix={version="=1.1.5",features=["fs","process"]}\n'
    )
    (scratch / "Cargo.toml").write_text(manifest)
    # `projector.rs` also uses `crate::node_io::node_tmpdir` and
    # `crate::project_files::create_directories_at`. Both real modules pull in
    # more than the projector needs (`node_io` has libuv errno fixtures and
    # `project_files` imports the `metadata`/icu tree), so the scratch crate
    # carries minimal stand-ins that mirror the exact functions used here.
    base_lib = (
        'mod metadata { pub(crate) fn js_trim(text: &str) -> &str { text.trim() } }\n'
        '#[path="js_value.rs"] pub mod js_value;\n'
        '#[path="node_io_shim.rs"] pub mod node_io;\n'
        '#[path="project_files_shim.rs"] pub mod project_files;\n'
        '#[path="runtime/turn/projector.rs"] pub mod projector;\n'
        f'#[cfg(test)] #[path={json.dumps(str(rust / "pilotdeck-gateway/tests/common/scratch.rs"))}] '
        "mod test_scratch;\n"
        f'#[cfg(test)] #[path={json.dumps(str(rust / "pilotdeck-gateway/tests/common/baseline.rs"))}] '
        "mod test_baseline;\n"
    )
    (scratch / "src/lib.rs").write_text(base_lib)
    # `projector.rs` reuses the gateway's bounded `js_value` helpers. The
    # scratch crate provides the module plus an unused `js_trim` shim (only
    # `to_number`/`string_to_number` touch it, which this fixture never calls).
    (scratch / "src/js_value.rs").write_text(
        (rust / "pilotdeck-gateway/src/js_value.rs").read_text()
    )
    # Mirrors `node_io.rs::node_tmpdir` (Node `os.tmpdir()` fallbacks).
    (scratch / "src/node_io_shim.rs").write_text(
        """\
//! Minimal stand-in for `node_io::node_tmpdir` (node_io.rs): TMPDIR -> TMP ->
//! TEMP, skipping empty values, then `/tmp`; one trailing slash is dropped.
//! Keep in step with the production function.
#![allow(dead_code)]
pub(crate) fn node_tmpdir(var: impl Fn(&str) -> Option<std::ffi::OsString>) -> String {
    let mut path = ["TMPDIR", "TMP", "TEMP"]
        .into_iter()
        .find_map(|key| {
            var(key)
                .map(|value| value.to_string_lossy().into_owned())
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_else(|| "/tmp".into());
    if path.len() > 1 && path.ends_with('/') {
        path.pop();
    }
    path
}
"""
    )
    # Mirrors `project_files.rs:138-180` (`create_directories_at`): every
    # lookup/mkdir is relative to the pinned parent and no symlink is followed.
    (scratch / "src/project_files_shim.rs").write_text(
        """\
//! Minimal stand-in for the gateway's `project_files::create_directories_at`
//! (project_files.rs) used by the projector's advisory writer. The real module
//! imports the `metadata` tree (icu/base64), so the negative-control crate
//! carries just this no-follow mkdir walk; keep it in step with production.
#![allow(dead_code)]
use std::{fs::File, io, path::{Component, Path}};

#[cfg(unix)]
pub(crate) fn create_directories_at(
    root: &File,
    relative: &Path,
    mode: rustix::fs::Mode,
) -> io::Result<()> {
    use rustix::fs::{AtFlags, FileType, Mode, mkdirat, openat, statat};
    let components: Vec<_> = relative.components().collect();
    let mut parent = root.try_clone()?;
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(name) = component else {
            return Err(io::Error::from_raw_os_error(libc::EACCES));
        };
        let name: &std::ffi::OsStr = name;
        let open = || openat(&parent, name, directory_flags(), Mode::empty());
        let opened = match open() {
            Err(rustix::io::Errno::NOENT) => {
                match mkdirat(&parent, name, mode) {
                    Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                    Err(error) => return Err(error.into()),
                }
                open()
            }
            result => result,
        };
        parent = File::from(opened.map_err(|error| {
            if matches!(error, rustix::io::Errno::NOTDIR | rustix::io::Errno::LOOP)
                && let Ok(stat) = statat(&parent, name, AtFlags::SYMLINK_NOFOLLOW)
            {
                let kind = FileType::from_raw_mode(stat.st_mode);
                if kind == FileType::Symlink {
                    return io::Error::from_raw_os_error(libc::ELOOP);
                }
                if kind != FileType::Directory && index + 1 == components.len() {
                    return io::Error::from_raw_os_error(libc::EEXIST);
                }
            }
            error.into()
        })?);
    }
    Ok(())
}

#[cfg(unix)]
fn directory_flags() -> rustix::fs::OFlags {
    use rustix::fs::OFlags;
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW
}
"""
    )

    for name, filename, before, after, marker in [("baseline", None, None, None, None)] + mutants:
        for file, content in originals.items():
            if file == filename:
                assert content.count(before) == 1, (name, file, content.count(before))
                content = content.replace(before, after)
            destination = (
                scratch / "tests/fixtures/agent-event-projector/oracle.json"
                if file == "oracle.json"
                else scratch / "src/runtime/turn" / file
            )
            destination.write_text(content)
        try:
            gate = subprocess.run(
                [
                    "bash",
                    "-c",
                    'until bash "$1"; do sleep 30; done',
                    "cargo-slots",
                    str(fixture.parents[4] / "scripts/cargo-slots.sh"),
                ],
                capture_output=True,
                text=True,
                timeout=1800,
            )
        except subprocess.TimeoutExpired:
            raise SystemExit("cargo-slots gate timed out after 1800s")
        command = [
            "cargo",
            "test",
            "--offline",
            "--manifest-path",
            str(scratch / "Cargo.toml"),
            "--lib",
            "projector::tests::",
            "--",
            "--nocapture",
        ]
        returncode, raw = run_tests(command)
        if name == "baseline":
            passed = returncode == 0 and "test result: ok." in raw and "0 failed" in raw
        elif isinstance(marker, tuple):
            # Restoring the old recursion must kill the test process on the
            # native stack; accept a plain assertion if this platform's stack
            # happens to survive 30,000 frames (still a red test).
            passed = (
                returncode != 0
                and any(text in raw for text in marker)
                and "could not compile" not in raw
                and "deep_tool_data_returns_ts_range_error" in raw
            )
        else:
            # `assert!(cond, "msg")` panics with only the message, while
            # `assert_eq!` adds "assertion `left == right` failed".
            failed_assertion = "assertion" in raw or "panicked at" in raw
            passed = (
                returncode == 101
                and failed_assertion
                and "could not compile" not in raw
                and marker in raw
            )
        (output / f"negative-{name}.log").write_text(
            gate.stdout
            + gate.stderr
            + "COMMAND "
            + " ".join(command)
            + "\n"
            + raw
            + f"\nexit={returncode}\n"
        )
        summary.append(
            {
                "name": name,
                "mutation": None if filename is None else {"file": filename, "before": before, "after": after},
                "expected_case": marker[0] if isinstance(marker, tuple) else marker,
                "exit": returncode,
                "passed": passed,
            }
        )
        (output / "negative-summary.json").write_text(
            json.dumps(summary, ensure_ascii=False, indent=2) + "\n"
        )
        print(name, "PASS" if passed else "INVALID", flush=True)
        if not passed:
            raise SystemExit(1)
print("all controls rejected; temporary crate removed")
