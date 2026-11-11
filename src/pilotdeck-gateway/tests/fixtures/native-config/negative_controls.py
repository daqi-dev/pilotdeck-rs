#!/usr/bin/env python3
"""#1457: --check; [--compile-only] NEW_OUTPUT [mutation ...]. Commit first.

Config-family negative controls over the #1457 real wiring. Each of the seven
methods has a stub-revert mutant (the dispatch arm goes back to the
pre-#1457 CAPABILITY_UNAVAILABLE terminal) bound to a test that must die,
and the shared gates (selection shape, sessionKey, mode whitelist, thinking
shape, mtime boundary, path, transcript entry, style echo, busy messages)
each have a mutant bound to the test that pins them. Each mutant compiles
before its exact test runs. Every case is reported, then restoration and all
statuses determine the exit code; never bypass HEAD.

#1864 wires the three selectors to a store the next session reads. A
stub-revert alone cannot see that, so each write site (permission, thinking,
output style), each compose-step read (permission, thinking, style merge),
the live adapter's per-turn permission-mode source, the host session tools'
per-turn admission source, and the `output_styles_list` selected echo each
carry their own function-internal mutant.
"""
from pathlib import Path
import os
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import Case, classify, invalidate_completed, passed, publish_summary

ROOT = Path(__file__).resolve().parents[5]
CONFIG = ROOT / 'src/pilotdeck-gateway/src/native_process/methods/config.rs'
SESSION_DIR = ROOT / 'src/pilotdeck-gateway/src/native_process/methods/session_dir.rs'
# #2272: the isolated-project admission source. The row below mutates it while
# its bound assertion lives in the session directory's gate-order tests — the
# #1864 read-side precedent (mutate the writer, assert on the reader) — so the
# path joins SOURCE_TEXTS / LITERAL_ANCHORS below like LIVE and TOOLS did.
REQUESTS = ROOT / 'src/pilotdeck-gateway/src/native_process/requests.rs'
# #2562: the submit path's projectKey gate (SubmitHostServices::workspace_id in
# services.rs) — the same nullish read #2272 gave the admission source above.
# SERVICES joins SOURCE_TEXTS as a mutation source; MANAGEMENT_TESTS joins it
# to carry the pinned assertion literal in LITERAL_ANCHORS.
SERVICES = ROOT / 'src/pilotdeck-gateway/src/native_process/services.rs'
MANAGEMENT_TESTS = ROOT / 'src/pilotdeck-gateway/src/native_process/management_tests.rs'
# #1604: `require_text` / `trim_guard` — the guard the session directory now
# delegates to. Two mutants below target it; the bound tests stay the
# session-directory ones.
METHODS_MOD = ROOT / 'src/pilotdeck-gateway/src/native_process/methods/mod.rs'
# #1864: the read side of the three selectors — the compose step in the
# session factory and the per-turn permission-mode source on the live
# adapter. Both are mutated below so "stored but never read" cannot come back.
ASSEMBLY = ROOT / 'src/pilotdeck-gateway/src/native_process/assembly.rs'
# #1860: hydration is a transcript writer too — the factory seeds the recorder
# from the tail it restored and only then publishes it, so the fence has to
# span both. FACTORY_TESTS carries the message the bound test asserts on.
FACTORY = ROOT / 'src/pilotdeck-gateway/src/runtime/session/factory.rs'
FACTORY_TESTS = ROOT / 'src/pilotdeck-gateway/src/runtime/session/factory_tests.rs'
# #1874: the three-segment cold-write protocol the config, rename and status
# callers share now lives in the runtime's transcript locks — each caller
# passes its own live/cold closures in — so the lease-skip mutant anchors on
# the helper every caller goes through. TRANSCRIPT_LOCKS joins SOURCE_TEXTS
# below.
TRANSCRIPT_LOCKS = ROOT / 'src/pilotdeck-gateway/src/runtime/session/transcript_locks.rs'
LIVE = ROOT / 'src/pilotdeck-gateway/src/runtime/turn/live.rs'
# #1864 fix-r2: the host session tools' admission source — `turn()` writes
# the stored selection back into `Permissions` *and* this turn's local
# `permissionMode` (the copy `tool_context` / `resolve_workspace_path` read),
# so both read points die together when the write-back is removed.
TOOLS = ROOT / 'src/pilotdeck-gateway/src/native_process/tools.rs'
# Two #1864 rows mutate `live.rs` / `tools.rs` but their bound test asserts on a
# message written in the *test* file, so these must be scanned as well or the
# completeness rule cannot see those anchors (#2065 r3).
LIVE_TESTS = ROOT / 'src/pilotdeck-gateway/src/runtime/turn/live_stream_tests.rs'
REVIEW_TESTS = ROOT / 'src/pilotdeck-gateway/src/native_process/review_tests.rs'
# #2195: the cron methods' dispatch seam. The row below removes the
# blocking-pool handoff (the management family's `memory-inline-sync` shape,
# #1322) and binds the structural test that proves the handoff exists; CRON is
# the mutated source, CRON_TESTS carries the bound assertion literal (#2065).
CRON = ROOT / 'src/pilotdeck-gateway/src/native_process/methods/cron.rs'
CRON_TESTS = ROOT / 'src/pilotdeck-gateway/src/native_process/methods/cron/cron_tests.rs'
PROJECT_SCOPE = ROOT / 'src/pilotdeck-gateway/src/native_process/project_scope.rs'
CFG = 'native_process::methods::config::tests::'
FT = 'runtime::session::factory::tests::'
DIR = 'native_process::methods::session_dir::tests::'
mutations = [
    ('set-stub-revert', CONFIG,
     '"session_model_set" => self.session_model_set(&params).await,',
     '"session_model_set" => Err(gateway_error("CAPABILITY_UNAVAILABLE", "session_model_set is unavailable.")),',
     'native_process::methods::config::tests::session_model_set_saves_selection_and_appends_transcript',
     '#1457 session_model_set accepts the fixture selection'),
    ('clear-stub-revert', CONFIG,
     '"session_model_clear" => self.session_model_clear(&params).await,',
     '"session_model_clear" => Err(gateway_error("CAPABILITY_UNAVAILABLE", "session_model_clear is unavailable.")),',
     'native_process::methods::config::tests::session_model_clear_appends_transcript_and_returns_null',
     '#1457 session_model_clear succeeds'),
    ('permission-stub-revert', CONFIG,
     '"set_permission_mode" => self.set_permission_mode(&params).await,',
     '"set_permission_mode" => Err(gateway_error("CAPABILITY_UNAVAILABLE", "set_permission_mode is unavailable.")),',
     'native_process::methods::config::tests::set_permission_mode_applies_and_validates',
     '#1864 set_permission_mode applies the fixture mode'),
    ('thinking-stub-revert', CONFIG,
     '"set_session_thinking" => self.set_session_thinking(&params).await,',
     '"set_session_thinking" => Err(gateway_error("CAPABILITY_UNAVAILABLE", "set_session_thinking is unavailable.")),',
     'native_process::methods::config::tests::set_session_thinking_applies_and_validates',
     '#1864 set_session_thinking stores the mode object'),
    ('styles-list-stub-revert', CONFIG,
     '"output_styles_list" => self.output_styles_list(&params).await,',
     '"output_styles_list" => Err(gateway_error("CAPABILITY_UNAVAILABLE", "output_styles_list is unavailable.")),',
     'native_process::methods::config::tests::output_styles_list_reports_project_styles',
     '#1457 output_styles_list without a plugin'),
    ('styles-set-stub-revert', CONFIG,
     '"set_output_style" => self.set_output_style(&params).await,',
     '"set_output_style" => Err(gateway_error("CAPABILITY_UNAVAILABLE", "set_output_style is unavailable.")),',
     'native_process::methods::config::tests::set_output_style_validates_names_and_key',
     '#1864 set_output_style applies the probe style'),
    ('seed-stub-revert', CONFIG,
     '"seed_read_state" => self.seed_read_state(&params).await,',
     '"seed_read_state" => Err(gateway_error("CAPABILITY_UNAVAILABLE", "seed_read_state is unavailable.")),',
     'native_process::methods::config::tests::seed_read_state_applies_and_validates',
     '#1457 seed_read_state with the exact mtime'),
    ('selection-shape-gate-removed', CONFIG,
     'if !matches!(selection, JsValue::Object(_) | JsValue::Array(_)) {',
     'if false {',
     'native_process::methods::config::tests::session_model_set_gate_order_matches_the_ts_chain',
     '#1457 missing selection message'),
    ('session-key-gate-code', CONFIG,
     '"INVALID_SESSION_KEY",\n        "sessionKey is required.",\n    )',
     '"INVALID_REQUEST",\n        "sessionKey is required.",\n    )',
     'native_process::methods::config::tests::set_permission_mode_applies_and_validates',
     '#1457 missing sessionKey code'),
    ('permission-mode-gate-removed', CONFIG,
     'if !mode_ok {',
     'if !mode_ok && false {',
     'native_process::methods::config::tests::set_permission_mode_applies_and_validates',
     '#1457 set_permission_mode rejects '),
    ('mode-whitelist-narrowed', CONFIG,
     'PERMISSION_MODES\n                .iter()\n                .any(|allowed| m.equals_str(allowed))',
     'PERMISSION_MODES\n                .iter()\n                .any(|allowed| *allowed != "default" && m.equals_str(allowed))',
     'native_process::methods::config::tests::set_permission_mode_applies_and_validates',
     '#1864 set_permission_mode applies the fixture mode'),
    ('thinking-shape-message', CONFIG,
     '            "INVALID_THINKING_CONFIG",\n            "thinking must be null or an object with an enabled boolean.",',
     '            "INVALID_THINKING_CONFIG",\n            "thinking must be null or an object with an enabled flag.",',
     'native_process::methods::config::tests::set_session_thinking_applies_and_validates',
     '#1457 non-object thinking message'),
    ('seed-mtime-boundary', CONFIG,
     'n.is_finite() && n.fract() == 0.0 && *n >= 0.0',
     'n.is_finite() && n.fract() == 0.0 && *n > 0.0',
     'native_process::methods::config::tests::seed_read_state_applies_and_validates',
     '#1457 seed_read_state returns the stale outcome'),
    ('seed-path-gate-code', CONFIG,
     '"INVALID_FILE_PATH", "path is required."',
     '"INVALID_REQUEST", "path is required."',
     'native_process::methods::config::tests::seed_read_state_applies_and_validates',
     '#1457 absent path code'),
    ('transcript-clear-null', CONFIG,
     'selection.cloned().unwrap_or(JsValue::Null),',
     'selection.cloned().unwrap_or(JsValue::Undefined),',
     'native_process::methods::config::tests::session_model_clear_appends_transcript_and_returns_null',
     '#1457 clear entry nulls the model selection'),
    ('transcript-fence-removed-config', CONFIG,
     '        let write_fence = transcript_locks::acquire(&fence_path).await?;',
     '        let write_fence = ();',
     'native_process::methods::config::tests::concurrent_transcript_writers_keep_the_chain_loadable',
     '#1457 transcript sequences strictly increase'),
    ('transcript-fence-removed-rename', SESSION_DIR,
     '        let fence_path = chat_dir.join(format!(\n            "{}.jsonl",\n            crate::storage::sanitize_session_id(&key)\n        ));\n        let write_fence = transcript_locks::acquire(&fence_path).await?;',
     '        let fence_path = chat_dir.join(format!(\n            "{}.jsonl",\n            crate::storage::sanitize_session_id(&key)\n        ));\n        let write_fence = ();',
     'native_process::methods::config::tests::concurrent_transcript_writers_keep_the_chain_loadable',
     '#1457 transcript sequences strictly increase'),
    ('transcript-live-bypass-config', CONFIG,
     '                    super::super::assembly::live_recorder(&recorders, &key)',
     '                    None::<(pilotdeck_protocol::JsString, std::sync::Arc<crate::storage::recorder::Recorder>)>',
     'native_process::methods::config::tests::model_set_appends_through_the_loaded_writer',
     '#1457 the transcript still loads'),
    ('transcript-live-bypass-rename', SESSION_DIR,
     '                    super::super::assembly::live_recorder(&recorders, &key_clone)',
     '                    None::<(pilotdeck_protocol::JsString, std::sync::Arc<crate::storage::recorder::Recorder>)>',
     'native_process::methods::session_dir::tests::rename_appends_through_the_loaded_writer',
     '#1457 the transcript still loads'),
    ('transcript-live-bypass-status', SESSION_DIR,
     '                    super::super::assembly::live_recorder(&recorders, &session_key)',
     '                    None::<(pilotdeck_protocol::JsString, std::sync::Arc<crate::storage::recorder::Recorder>)>',
     'native_process::methods::session_dir::tests::status_appends_through_the_loaded_writer',
     '#1457 the transcript still loads'),
    ('transcript-hydrate-fence-removed', FACTORY,
     '            let _write_fence = super::transcript_locks::acquire(&fence_path)\n                .await\n                .map_err(|error| {\n                    RuntimeError::new(String::from_utf16_lossy(error.message.units()))\n                })?;',
     '            let _write_fence = super::transcript_locks::acquire(&fence_path)\n                .await\n                .map_err(|error| {\n                    RuntimeError::new(String::from_utf16_lossy(error.message.units()))\n                })?;\n            drop(_write_fence);',
     'runtime::session::factory::tests::hydration_holds_the_transcript_fence_until_the_recorder_is_published',
     '#1860 hydrate holds the transcript fence until the recorder is published'),
    ('transcript-cold-lease-skipped', TRANSCRIPT_LOCKS,
     '    let Some(_lease) = cold_write_lease(&storage.transcript_path)? else {\n        return live()?.map(Some).ok_or_else(writer_busy_error);',
     '    let Some(_lease) = Ok::<_, RequestError>(Some::<()>(()))? else {\n        return live()?.map(Some).ok_or_else(writer_busy_error);',
     'native_process::methods::session_dir::tests::cold_metadata_write_reports_busy_while_a_session_holds_the_lease',
     '#1457 a cold append must not run beside a session that owns the writer'),
    ('transcript-cold-lease-skipped-status', TRANSCRIPT_LOCKS,
     '    let Some(_lease) = cold_write_lease(&storage.transcript_path)? else {\n        return live()?.map(Some).ok_or_else(writer_busy_error);',
     '    let Some(_lease) = Ok::<_, RequestError>(Some::<()>(()))? else {\n        return live()?.map(Some).ok_or_else(writer_busy_error);',
     'native_process::methods::session_dir::tests::status_cold_write_reports_busy_while_a_session_holds_the_lease',
     '#1873 a cold status append must not run beside a session that owns the writer'),
    ('transcript-cold-lease-skipped-config', TRANSCRIPT_LOCKS,
     '    let Some(_lease) = cold_write_lease(&storage.transcript_path)? else {\n        return live()?.map(Some).ok_or_else(writer_busy_error);',
     '    let Some(_lease) = Ok::<_, RequestError>(Some::<()>(()))? else {\n        return live()?.map(Some).ok_or_else(writer_busy_error);',
     'native_process::methods::config::tests::model_set_cold_write_reports_busy_while_a_session_holds_the_lease',
     '#1874 a cold model-set append must not run beside a session that owns the writer'),
    ('model-set-checked-append-reverted', CONFIG,
     '                    .record_session_metadata_checked(&key, &turn, entry.clone())',
     '                    .record_session_metadata(&key, &turn, entry.clone())',
     'native_process::methods::config::tests::model_set_reports_a_closed_writer_as_an_error',
     '#1874 a closed session writer is an error, not a silent save'),
    ('rename-checked-append-reverted', SESSION_DIR,
     '                    .record_session_metadata_checked(&key_clone, &turn_id, entry.clone())',
     '                    .record_session_metadata(&key_clone, &turn_id, entry.clone())',
     'native_process::methods::session_dir::tests::rename_reports_a_closed_writer_as_an_error',
     '#1874 a closed session writer is an error, not a silent rename'),
    ('status-checked-append-reverted', SESSION_DIR,
     '                    .record_agent_status_message_checked(&session_key, &turn_id, &status)',
     '                    .record_agent_status_message(&session_key, &turn_id, &status)',
     'native_process::methods::session_dir::tests::status_reports_a_closed_writer_as_an_error',
     '#1874 a closed session writer is an error, not a recorded status'),
    ('writer-lease-folded-to-busy', FACTORY,
     '        Err(crate::storage::LedgerError::Io(ref error))\n            if error.kind() == std::io::ErrorKind::WouldBlock =>\n        {\n            return Ok(None);\n        }\n        Err(error) => return Err(RuntimeError::new(error.to_string())),',
     '        Err(crate::storage::LedgerError::Io(ref error))\n            if error.kind() == std::io::ErrorKind::WouldBlock =>\n        {\n            return Ok(None);\n        }\n        Err(_) => return Ok(None),',
     'native_process::methods::session_dir::tests::status_cold_write_surfaces_a_broken_lock_file_as_an_error',
     '#1873 a broken lock file is a storage error, not the retryable busy answer'),
    ('set-alias-owner-busy-removed', CONFIG,
     '        if let Some(key) = params.get("sessionKey").and_then(JsValue::as_string)\n            && (self.owner.has_active_turn(key)\n                || trimmed_string_is_non_empty(key)\n                    && owner.is_some_and(|owner| self.owner.has_active_turn(owner)))\n        {',
     '        if let Some(key) = params.get("sessionKey").and_then(JsValue::as_string)\n            && self.owner.has_active_turn(key)\n        {',
     'native_process::methods::config::tests::session_model_set_rejects_active_turn_on_alias_owner',
     '#1872 session_model_set is busy on the alias owner'),
    ('set-busy-gate-blank-key-guard-removed', CONFIG,
     '        if let Some(key) = params.get("sessionKey").and_then(JsValue::as_string)\n            && (self.owner.has_active_turn(key)\n                || trimmed_string_is_non_empty(key)\n                    && owner.is_some_and(|owner| self.owner.has_active_turn(owner)))\n        {',
     '        if let Some(key) = params.get("sessionKey").and_then(JsValue::as_string)\n            && (self.owner.has_active_turn(key)\n                || owner.is_some_and(|owner| self.owner.has_active_turn(owner)))\n        {',
     'native_process::methods::config::tests::session_model_set_blank_key_answers_the_session_key_gate',
     '#1872 the blank key is a sessionKey error'),
    ('set-blank-key-busy-reverted-to-gate-skip', CONFIG,
     '        if let Some(key) = params.get("sessionKey").and_then(JsValue::as_string)\n            && (self.owner.has_active_turn(key)\n                || trimmed_string_is_non_empty(key)\n                    && owner.is_some_and(|owner| self.owner.has_active_turn(owner)))\n        {',
     '        if let Some(key) = params.get("sessionKey").and_then(JsValue::as_string)\n            && trimmed_string_is_non_empty(key)\n            && (self.owner.has_active_turn(key)\n                || owner.is_some_and(|owner| self.owner.has_active_turn(owner)))\n        {',
     'native_process::methods::config::tests::session_model_set_reports_busy_for_a_turn_held_on_the_blank_key',
     '#1872 the blank key answers busy'),
    ('permission-blank-key-busy-skipped', CONFIG,
     '        if let Some(JsValue::String(key)) = params.get("sessionKey")\n            && self.owner.has_active_turn(key)\n        {\n            return Err(gateway_error(\n                "SESSION_BUSY",\n                "Cannot change permission mode during an active turn.",\n            ));\n        }',
     '        if let Some(JsValue::String(key)) = params.get("sessionKey")\n            && trimmed_string_is_non_empty(key)\n            && self.owner.has_active_turn(key)\n        {\n            return Err(gateway_error(\n                "SESSION_BUSY",\n                "Cannot change permission mode during an active turn.",\n            ));\n        }',
     'native_process::methods::config::tests::set_permission_mode_reports_busy_for_a_turn_held_on_the_blank_key',
     '#1872 the blank key answers busy for permission'),
    ('clear-blank-key-require-weakened', CONFIG,
     '        let session_key = require_session_key(params)?;\n        if self.owner.has_active_turn(&session_key)\n            || owner.is_some_and(|owner| self.owner.has_active_turn(owner))\n        {',
     '        let session_key = require_session_key(params).unwrap_or_default();\n        if self.owner.has_active_turn(&session_key)\n            || owner.is_some_and(|owner| self.owner.has_active_turn(owner))\n        {',
     'native_process::methods::config::tests::session_model_clear_blank_key_answers_the_session_key_gate',
     '#1872 the blank key is a sessionKey error on clear'),
    ('set-blank-key-owner-reservation-restored', CONFIG,
     '        if let (Some(owner), Some(key)) = (&owner, reserved)\n            && owner != key\n            && trimmed_string_is_non_empty(key)\n        {\n            reservations.push(requests.reserve_model_write(owner, "change the session model")?);\n        }',
     '        if let (Some(owner), Some(key)) = (&owner, reserved)\n            && owner != key\n        {\n            reservations.push(requests.reserve_model_write(owner, "change the session model")?);\n        }',
     'native_process::methods::config::tests::session_model_set_blank_key_ignores_a_busy_owner_reservation',
     '#1872 the blank key is a sessionKey error despite a busy owner reservation'),
    ('clear-blank-key-owner-reservation-restored', CONFIG,
     '        if let (Some(owner), Some(key)) = (&owner, reserved)\n            && owner != key\n            && trimmed_string_is_non_empty(key)\n        {\n            reservations.push(requests.reserve_model_write(owner, "clear the session model")?);\n        }',
     '        if let (Some(owner), Some(key)) = (&owner, reserved)\n            && owner != key\n        {\n            reservations.push(requests.reserve_model_write(owner, "clear the session model")?);\n        }',
     'native_process::methods::config::tests::session_model_clear_blank_key_ignores_a_busy_owner_reservation',
     '#1872 the blank key is a sessionKey error on clear despite a busy owner reservation'),
    ('permission-blank-key-require-weakened', CONFIG,
     '        let session_key = require_session_key(params)?;\n        let mode = match mode.as_string() {',
     '        let session_key = require_session_key(params).unwrap_or_default();\n        let mode = match mode.as_string() {',
     'native_process::methods::config::tests::set_permission_mode_blank_key_answers_the_session_key_gate',
     '#1872 the blank key is a sessionKey error for permission'),
    ('clear-alias-owner-busy-removed', CONFIG,
     '        if self.owner.has_active_turn(&session_key)\n            || owner.is_some_and(|owner| self.owner.has_active_turn(owner))\n        {',
     '        if self.owner.has_active_turn(&session_key) {',
     'native_process::methods::config::tests::session_model_clear_rejects_active_turn_on_alias_owner',
     '#1872 session_model_clear is busy on the alias owner'),
    ('set-alias-owner-reservation-skipped', CONFIG,
     '        if let (Some(owner), Some(key)) = (&owner, reserved)\n            && owner != key\n            && trimmed_string_is_non_empty(key)\n        {\n            reservations.push(requests.reserve_model_write(owner, "change the session model")?);\n        }',
     '        if let (Some(owner), Some(key)) = (&owner, reserved)\n            && owner != key && false\n            && trimmed_string_is_non_empty(key)\n        {\n            reservations.push(requests.reserve_model_write(owner, "change the session model")?);\n        }',
     'native_process::methods::config::tests::session_model_set_holds_the_owner_reservation_through_the_write',
     '#1872 the owner and alias reservations are held while the write is parked'),
    ('clear-alias-owner-reservation-skipped', CONFIG,
     '        if let (Some(owner), Some(key)) = (&owner, reserved)\n            && owner != key\n            && trimmed_string_is_non_empty(key)\n        {\n            reservations.push(requests.reserve_model_write(owner, "clear the session model")?);\n        }',
     '        if let (Some(owner), Some(key)) = (&owner, reserved)\n            && owner != key && false\n            && trimmed_string_is_non_empty(key)\n        {\n            reservations.push(requests.reserve_model_write(owner, "clear the session model")?);\n        }',
     'native_process::methods::config::tests::session_model_clear_holds_the_owner_reservation_through_the_write',
     '#1872 the clear-side owner and alias reservations are held while the write is parked'),
    ('close-owner-fallback-busy-skipped', CONFIG,
     '        if self.owner.has_active_turn(target) {\n            return Err(gateway_error("SESSION_BUSY", busy_message));\n        }',
     '        if false {\n            return Err(gateway_error("SESSION_BUSY", busy_message));\n        }',
     'native_process::methods::config::tests::close_model_selection_owner_reports_busy_for_an_owner_seen_by_the_append',
     '#1872 the fallback reports the admitted turn as busy'),
    ('set-closes-owner-target-reverted', CONFIG,
     '        let target = loaded.unwrap_or(session_key);',
     '        let target = session_key;',
     'native_process::methods::config::tests::model_set_closes_the_owner_spelling_of_an_alias_request',
     '#1872 the close targeted the owner spelling, not the request spelling'),
    ('seed-mtime-wiring', CONFIG,
     '.seed_read_state(&path, mtime)',
     '.seed_read_state(&path, 0.0)',
     'native_process::methods::config::tests::seed_read_state_applies_and_validates',
     '#1457 exact mtime applies'),
    ('set-busy-message', CONFIG,
     '"SESSION_BUSY",\n                "Cannot change the model during an active turn.",',
     '"SESSION_BUSY",\n                "Cannot change the model while a turn is active.",',
     'native_process::methods::config::tests::session_model_set_rejects_active_turn',
     '#1457 busy message'),
    ('clear-busy-message', CONFIG,
     '"SESSION_BUSY",\n                "Cannot clear the model during an active turn.",',
     '"SESSION_BUSY",\n                "Cannot clear the model while a turn is active.",',
     'native_process::methods::config::tests::session_model_clear_gates_key_and_busy',
     '#1457 clear busy message'),
    ('permission-busy-message', CONFIG,
     '"SESSION_BUSY",\n                "Cannot change permission mode during an active turn.",',
     '"SESSION_BUSY",\n                "Cannot change permission mode while a turn is active.",',
     'native_process::methods::config::tests::set_permission_mode_applies_and_validates',
     '#1457 pm busy message'),
    ('thinking-busy-message', CONFIG,
     '"SESSION_BUSY",\n                "Cannot change thinking while a turn is active.",',
     '"SESSION_BUSY",\n                "Cannot change thinking during an active turn.",',
     'native_process::methods::config::tests::set_session_thinking_applies_and_validates',
     '#1457 th busy message'),
    ('style-busy-message', CONFIG,
     '"SESSION_BUSY",\n                "Cannot change output style during an active turn.",',
     '"SESSION_BUSY",\n                "Cannot change output style while a turn is active.",',
     'native_process::methods::config::tests::set_output_style_validates_names_and_key',
     '#1457 style busy message'),
    ('seed-busy-message', CONFIG,
     '"SESSION_BUSY",\n                "Cannot seed file read state while a turn is active.",',
     '"SESSION_BUSY",\n                "Cannot seed file read state during an active turn.",',
     'native_process::methods::config::tests::seed_read_state_applies_and_validates',
     '#1457 seed busy message'),
    ('shared-trim-dropped', CONFIG,
     '!crate::metadata::js_trim(&String::from_utf16_lossy(value.units())).is_empty()',
     '!String::from_utf16_lossy(value.units()).is_empty()',
     'native_process::methods::config::tests::set_output_style_validates_names_and_key',
     '#1457 blank name code'),
    ('session-dir-blank-gate-removed', METHODS_MOD,
     '            if crate::metadata::js_trim(&text).is_empty() {\n                Ok(TrimGuard::Missing)',
     '            if false {\n                Ok(TrimGuard::Missing)',
     'native_process::methods::session_dir::tests::resume_session_rejects_blank_session_key',
     '#1455 resume_session rejects the blank key'),
    ('session-dir-surrogate-rejected', METHODS_MOD,
     '            if crate::metadata::js_trim(&text).is_empty() {',
     '            if value.to_utf8().is_err() || crate::metadata::js_trim(&text).is_empty() {',
     'native_process::methods::session_dir::tests::resume_session_accepts_lone_surrogate_session_key',
     '#1455 resume_session accepts the surrogate key'),
    ('session-dir-project-key-domain-message', SESSION_DIR,
     '            "projectKey is required.",\n        )?;',
     '            "projectKey is missing.",\n        )?;',
     'native_process::methods::session_dir::tests::close_project_sessions_blank_project_key_is_the_domain_error',
     '#1604 the blank projectKey keeps the domain message'),
    ('session-dir-project-key-surrogate-rejected', SESSION_DIR,
     '        let selected = super::super::project_scope::resolve(&project_key)?;',
     '        let project_key = JsString::from(String::from_utf16_lossy(project_key.units()).as_str());\n        let selected = super::super::project_scope::resolve(&project_key)?;',
     'native_process::methods::session_dir::tests::close_project_sessions_surrogate_project_key_reaches_the_capability_gate',
     '#2083 the surrogate projectKey reaches the capability gate'),
    ('admission-null-refused', PROJECT_SCOPE,
     '    let requested = params',
     '    if matches!(params.get("projectKey"), Some(JsValue::Null)) {\n        return Err(RequestError::new("null projectKey refused"));\n    }\n    let requested = params',
     'native_process::methods::session_dir::tests::resume_session_blank_key_gate_order',
     '#2272 a null projectKey falls back like a missing one, so the sessionKey gate answers the blank key'),
    ('submit-null-refused', SERVICES,
     '        if let Some(project) = input\n            .get("projectKey")\n            .filter(|v| !matches!(v, JsValue::Undefined | JsValue::Null))\n        {',
     '        if let Some(project) = input.get("projectKey").filter(|v| !v.is_undefined()) {',
     'native_process::management_tests::submit_workspace_id_treats_an_explicit_null_project_key_as_not_provided',
     '#2562 explicit null is not an error with the recorder on'),
    ('admission-null-refused-close-project', PROJECT_SCOPE,
     '    let requested = params',
     '    if matches!(params.get("projectKey"), Some(JsValue::Null)) {\n        return Err(RequestError::new("null projectKey refused"));\n    }\n    let requested = params',
     'native_process::methods::session_dir::tests::close_project_sessions_null_project_key_is_the_domain_error',
     '#2564 a null projectKey keeps the domain message'),
    ('admission-null-refused-list-sessions', PROJECT_SCOPE,
     '    let requested = params',
     '    if matches!(params.get("projectKey"), Some(JsValue::Null)) {\n        return Err(RequestError::new("null projectKey refused"));\n    }\n    let requested = params',
     'native_process::methods::session_dir::tests::list_sessions_null_project_key_lists_the_configured_project',
     '#2564 a null projectKey lists the configured project'),
    ('admission-null-refused-new-session', PROJECT_SCOPE,
     '    let requested = params',
     '    if matches!(params.get("projectKey"), Some(JsValue::Null)) {\n        return Err(RequestError::new("null projectKey refused"));\n    }\n    let requested = params',
     'native_process::methods::session_dir::tests::new_session_null_project_key_omits_the_project_prefix',
     '#2564 a null projectKey generates a session key'),
    ('permission-store-write-removed', CONFIG,
     '        self.session_overrides\n            .set_permission_mode(&session_key, mode);',
     '        let _ = (&session_key, &mode);',
     'native_process::methods::config::tests::set_permission_mode_applies_and_validates',
     '#1864 the plan selection is stored for the next run to read'),
    ('thinking-store-write-removed', CONFIG,
     '            thinking => self\n                .session_overrides\n                .set_thinking(&session_key, thinking.clone()),',
     '            thinking => {\n                let _ = thinking;\n            }',
     'native_process::methods::config::tests::set_session_thinking_applies_and_validates',
     '#1864 the mode object is stored'),
    ('style-store-write-removed', CONFIG,
     '        self.session_overrides\n            .select_output_style(\n                &session_key,\n                &JsString::from(project.to_string_lossy().into_owned()),\n                name.clone(),\n            )\n            .await\n            .map_err(sdk_selection_error)?;',
     '        let _ = (&session_key, &name);',
     'native_process::methods::config::tests::set_output_style_validates_names_and_key',
     '#1864 set_output_style applies the probe style'),
    ('compose-permission-readback-removed', ASSEMBLY,
     '    let mode = effective_permission_mode(config, stored_permission_mode.cloned());',
     '    let mode = effective_permission_mode(config, None);',
     'native_process::methods::config::tests::selector_writes_reach_the_compose_step',
     '#1864 the stored permission mode reaches the agent config'),
    ('compose-thinking-readback-removed', ASSEMBLY,
     '    if let Some(thinking) = stored_thinking {',
     '    if let Some(thinking) = None::<&JsValue> {',
     'native_process::methods::config::tests::selector_writes_reach_the_compose_step',
     '#1864 the stored thinking reaches the agent config'),
    ('compose-style-merge-removed', ASSEMBLY,
     '    if let Some(content) = style_content {',
     '    if let Some(content) = None::<&str> {',
     'native_process::methods::config::tests::apply_session_overrides_composes_the_selections',
     '#1864 the style content joins the existing prompt'),
    ('styles-list-selected-removed', CONFIG,
     '            && let Some(selected) = self\n                .session_overrides\n                .effective_output_style(key)\n                .await\n                .map_err(sdk_selection_error)?',
     '            && let Some(selected) = None::<JsString>',
     'native_process::methods::config::tests::output_styles_list_reports_project_styles',
     '#1864 the list echoes the stored selection for the session'),
    ('submit-permission-source-removed', LIVE,
     '            if let Some(source) = &self.permission_mode_source',
     '            if let Some(source) = &None::<Arc<dyn Fn() -> Option<JsString> + Send + Sync>>',
     'runtime::turn::live::stream_tests::permission_mode_source_rewrites_the_agent_config_each_turn',
     "#1864 the stored mode reaches the live session's next turn"),
    ('tools-admission-source-removed', TOOLS,
     '            if let Some(mode) = permission::configured_permission_mode(&self.config)\n                .or_else(|| {\n                    self.permission_mode_source\n                        .as_ref()\n                        .and_then(|source| source())\n                })\n                .or_else(|| {\n                    // Host default applies only below managed and stored selections.\n                    // Re-reading false resets a previous persisted bypass on next turn.\n                    (self.permission_mode_source.is_some() && self.fork_scope.is_none()).then(\n                        || {\n                            if persisted_skip {\n                                "bypassPermissions".into()\n                            } else {\n                                "default".into()\n                            }\n                        },\n                    )\n                })\n            {',
     '            if let Some(mode) = None::<JsString> {',
     'native_process::tests::review::session_tools_admission_mode_follows_the_store_each_turn',
     '#1864 the stored mode reaches the host admission context'),
    ('acquire-ask-sync-removed', ASSEMBLY,
     'if !sync_acquired_ask_rules(',
     'if true || !sync_acquired_ask_rules(',
     'native_process::tests::review::acquiring_a_session_syncs_the_mcp_ask_rules',
     '#1429 the acquire path restores the generated ask rule'),
    ('acquire-miss-dedup-removed', ASSEMBLY,
     'if acquire_miss_reported.set(()).is_ok() {',
     'if true {',
     'native_process::tests::review::acquiring_a_session_reports_the_skipped_mcp_ask_sync',
     '#2146 the second acquire reports nothing: one miss report per assembly'),
    ('cron-inline-sync', CRON,
     '            let dispatched = tokio::task::spawn_blocking(move || {\n                dispatch_under_management_gate(&cron, &dispatch_method, &params)\n            })\n            .await;',
     '            let dispatched = Ok::<_, tokio::task::JoinError>(cron::dispatch(cron.as_deref(), &dispatch_method, &params));',
     'native_process::methods::cron::cron_tests::cron_request_dispatches_off_the_async_worker',
     '#1509 cron dispatch must yield the async worker before answering'),
    ('session-dir-unknown-project-write', SESSION_DIR,
     '    async fn session_delete(&self, params: &JsValue) -> Result<JsValue, RequestError> {\n        let project_root = resolve_project(self, params).await?;',
     '    async fn session_delete(&self, params: &JsValue) -> Result<JsValue, RequestError> {\n        let project_root = self.options.project.clone();',
     'native_process::methods::session_dir::tests::unregistered_project_never_creates_session_storage',
     '#2090 unknown project cannot create session storage'),
    # ── #2068: the projectKey read the session_dir and config families now
    # share (`input.projectKey ?? fallback` whose TS chains start at Node
    # `path.resolve`, probes `_tmp/20261110-issue-2068/probe/
    # probe-2068.out` + `probe-2068-r2.out`). Re-adding the pre-#2068 shape
    # where a non-string key silently folds onto the configured project turns
    # the list_sessions TypeError cell's `.expect_err` red; re-adding the
    # `to_utf8().ok()` fold (the lone surrogate falls back instead of staying
    # a real unserved key) turns the real-keys test's surrogate cell red. The
    # config rows re-bind the same code points to the config family's own
    # test — the #2564 one-code-point-many-bound-tests shape.
    # fix-r1 (review P1/P2): the shared read now walks `argument_received`, so
    # the two folded rows mutate the whole non-string arm; the object-fold row
    # re-adds only the object arm's silent fold (the review's Object-arm gap),
    # and the code-reverted row restores the boundary code literal, which the
    # ERR_INVALID_ARG_TYPE assertion must catch (the review's `code 回退` row).
    ('project-key-nonstring-folded', METHODS_MOD,
     '        Some(other) => match crate::metadata::argument_received(other) {\n'
     '            crate::metadata::ArgumentReceived::Text(received) => Err(RequestError {\n'
     '                code: "ERR_INVALID_ARG_TYPE".into(),\n'
     '                message: crate::metadata::invalid_arg_type_message("paths[0]", &received),\n'
     '                details: None,\n'
     '                validation: None,\n'
     '            }),\n'
     '            crate::metadata::ArgumentReceived::Throws { message, .. } => {\n'
     '                Err(RequestError::new(message))\n'
     '            }\n'
     '        },',
     '        Some(_) => Ok(fallback.to_string_lossy().into_owned().into()),',
     DIR + 'list_sessions_non_string_project_key_answers_the_path_resolve_type_error',
     '#2068 a non-string projectKey is the path.resolve TypeError'),
    ('project-key-utf8-folded', METHODS_MOD,
     '        Some(JsValue::String(key)) => Ok(key.clone()),',
     '        Some(JsValue::String(key)) => Ok(key.to_utf8()\n'
     '            .unwrap_or_else(|_| fallback.to_string_lossy().into_owned()).into()),',
     DIR + 'list_sessions_string_project_keys_are_real_keys_not_the_fallback',
     '#2068 a string projectKey is a real key, never the fallback'),
    # Same two code points, config side: with the number folded, the
    # session_model_set call reaches the sessionKey gate and answers
    # INVALID_SESSION_KEY, so the ERR_INVALID_ARG_TYPE code assertion dies
    # first; with the surrogate folded, the served fallback makes the call
    # succeed past the capability gate, so the isolation-code assertion dies
    # first.
    ('config-project-key-nonstring-folded', METHODS_MOD,
     '        Some(other) => match crate::metadata::argument_received(other) {\n'
     '            crate::metadata::ArgumentReceived::Text(received) => Err(RequestError {\n'
     '                code: "ERR_INVALID_ARG_TYPE".into(),\n'
     '                message: crate::metadata::invalid_arg_type_message("paths[0]", &received),\n'
     '                details: None,\n'
     '                validation: None,\n'
     '            }),\n'
     '            crate::metadata::ArgumentReceived::Throws { message, .. } => {\n'
     '                Err(RequestError::new(message))\n'
     '            }\n'
     '        },',
     '        Some(_) => Ok(fallback.to_string_lossy().into_owned().into()),',
     CFG + 'session_model_set_project_key_read_follows_the_ts_chain',
     '#2068 the non-string projectKey answers the Node TypeError code'),
    ('project-key-object-folded', METHODS_MOD,
     '        Some(other) => match crate::metadata::argument_received(other) {',
     '        Some(JsValue::Object(_)) => Ok(fallback.to_string_lossy().into_owned().into()),\n'
     '        Some(other) => match crate::metadata::argument_received(other) {',
     DIR + 'list_sessions_non_string_project_key_answers_the_path_resolve_type_error',
     '#2068 a non-string projectKey is the path.resolve TypeError'),
    ('project-key-code-reverted', METHODS_MOD,
     '                code: "ERR_INVALID_ARG_TYPE".into(),',
     '                code: "gateway_request_failed".into(),',
     DIR + 'list_sessions_non_string_project_key_answers_the_path_resolve_type_error',
     '#2068 the number projectKey answers the Node TypeError code'),
    ('config-project-key-utf8-folded', METHODS_MOD,
     '        Some(JsValue::String(key)) => Ok(key.clone()),',
     '        Some(JsValue::String(key)) => Ok(key.to_utf8()\n'
     '            .unwrap_or_else(|_| fallback.to_string_lossy().into_owned()).into()),',
     CFG + 'session_model_set_project_key_read_follows_the_ts_chain',
     '#2068 the surrogate projectKey reaches the registration gate'),
    # ── #2068: `newSession`'s prefix read is plain truthiness — the three
    # mutants below restore the issue's suspected shapes: a trim gate (blank
    # keys drop the prefix), the number arm always dropping it, and a lossy
    # string rendering (the lone surrogate becomes U+FFFD instead of its
    # raw unit). The lossy row's `collect::<Vec<u16>>` pin keeps the type
    # identical to the original expression — an un-annotated `collect()` has
    # no inferable target through `extend` and fails E0277 on `!` (cb8
    # sandbox compile log, negctl-2068-samples/; the #2195 `Ok::<_, JoinError>`
    # E0282 shape).
    ('new-session-prefix-trim-gate', SESSION_DIR,
     '        Some(JsValue::String(key)) if key.units().is_empty() => Ok(Vec::new()),',
     '        Some(JsValue::String(key)) if crate::metadata::js_trim(\n'
     '            &String::from_utf16_lossy(key.units()),\n'
     '        )\n'
     '        .is_empty() => Ok(Vec::new()),',
     DIR + 'new_session_project_key_prefix_follows_ts_truthiness',
     '#2068 the blank projectKey keeps its truthiness prefix'),
    ('new-session-prefix-number-dropped', SESSION_DIR,
     '        Some(JsValue::Number(n)) if *n == 0.0 || n.is_nan() => Ok(Vec::new()),',
     '        Some(JsValue::Number(_)) => Ok(Vec::new()),',
     DIR + 'new_session_project_key_prefix_follows_ts_truthiness',
     '#2068 the number projectKey keeps its truthiness prefix'),
    ('new-session-prefix-surrogate-stripped', SESSION_DIR,
     '        Some(other) => {\n'
     '            let rendered = crate::js_value::to_string_units_checked(other)\n'
     '                .map_err(|error| RequestError::new(error.message()))?;',
     '        Some(other) => {\n'
     '            let rendered = crate::js_value::to_string_units_checked(other)\n'
     '                .map(|units| String::from_utf16_lossy(&units).encode_utf16().collect::<Vec<u16>>())\n'
     '                .map_err(|error| RequestError::new(error.message()))?;',
     DIR + 'new_session_project_key_prefix_follows_ts_truthiness',
     '#2068 the surrogate projectKey keeps its raw unit inside the prefix'),
    # fix-r1 (review P2): the ToPrimitive failure must serialize the bare V8
    # text — no `name:` prefix (GatewayWsConnection.ts:252 keeps only
    # `error.message`; probe `probe-2068-r2.out`). Re-adding the prefixed
    # shape turns the throwing-object test's bare-message assertion red.
    ('new-session-prefix-throwing-prefixed', SESSION_DIR,
     '                .map_err(|error| RequestError::new(error.message()))?;',
     '                .map_err(|error| RequestError::new(format!("{}: {}", error.name(), error.message())))?;',
     DIR + 'new_session_throwing_project_key_prefix_answers_the_boundary_typeerror',
     '#2068 the ToPrimitive failure keeps the bare V8 text'),
]


# #2065: `expect` / `expect_err` take `&str` and never interpolate, so a panic
# that rides one can only be matched by text the source holds verbatim -- the
# `{label}` stays literal in the payload. `negative_assert!` /
# `negative_assert_eq!` interpolate through `format!`, but only the templates
# that actually contain `{...}` differ from the payload; a macro assertion with
# no placeholder is verbatim source text like any other (#1457 busy message).
# `classify` stays untouched; this refuses a run whose literal anchor is
# verbatim source text yet unregistered, and a pin no row asserts. An invented
# string that matches no scanned file cannot be told apart from a `{...}`
# template, so it is not refused here -- the run finds it as an INVALID.
#
# The table pins every anchor that is verbatim in the scanned files: 39 of the
# 45 rows (34 distinct strings, 35 entries -- `the transcript still loads` is
# registered once per file), the remaining six rows ride a `{...}` template and
# cannot be pinned. Row and entry counts differ on purpose: rows vs. distinct
# strings vs. (string, file) pairs are three different numbers. Pinning is the
# point: when a message is reworded, `--check` fails here instead of surfacing as
# a false INVALID hours into a host run. `check_literal_anchors` also refuses a
# row whose anchor is verbatim source text but was left unpinned, so a new row
# cannot skip registration; registration is per (file, string) pair, so dropping
# one of two registrations of the same string still fails. Anchors that must stay
# *prefixes* of a longer source
# literal are stored as that prefix -- see the `permission-mode-gate-removed` row.
# A pin is bound only to a row asserting it **exactly**, so reverting that row to
# a longer, invented suffix (the #2065 shape) fails as `unbound to any row`
# instead of passing on a substring.
#
# Known softness: the presence test is a plain whole-file substring, so a pin
# whose text also sits in a comment still reads as present. That only weakens
# the warning, never turns a broken run green (the run itself is fail-closed).
#
# #2562 r0 + fix-r1: SERVICES / MANAGEMENT_TESTS joined the scanned files with
# the rows above, so the counts moved to 50 rows / 40 literal-anchor entries;
# the derived counts in the paragraph above describe the #2065 table as it was.
# #2195: CRON / CRON_TESTS joined the scanned files with the row above, so
# the counts moved to 51 rows / 41 literal-anchor entries.
LITERAL_ANCHORS = [
    (CONFIG, '#1457 session_model_set accepts the fixture selection'),
    (CONFIG, '#1457 session_model_clear succeeds'),
    (CONFIG, '#1864 set_permission_mode applies the fixture mode'),
    (CONFIG, '#1864 set_session_thinking stores the mode object'),
    (CONFIG, '#1457 output_styles_list without a plugin'),
    (CONFIG, '#1864 set_output_style applies the probe style'),
    (CONFIG, '#1457 seed_read_state with the exact mtime'),
    (CONFIG, '#1457 set_permission_mode rejects '),
    (CONFIG, '#1457 seed_read_state returns the stale outcome'),
    (CONFIG, '#1457 clear entry nulls the model selection'),
    (CONFIG, '#1457 transcript sequences strictly increase'),
    (CONFIG, '#1457 the transcript still loads'),
    (CONFIG, '#1457 exact mtime applies'),
    (CONFIG, '#1457 busy message'),
    (CONFIG, '#1457 clear busy message'),
    (CONFIG, '#1872 session_model_set is busy on the alias owner'),
    (CONFIG, '#1872 session_model_clear is busy on the alias owner'),
    (CONFIG, '#1872 the owner and alias reservations are held while the write is parked'),
    (CONFIG, '#1872 the clear-side owner and alias reservations are held while the write is parked'),
    (CONFIG, '#1872 the blank key is a sessionKey error'),
    (CONFIG, '#1872 the blank key answers busy'),
    (CONFIG, '#1872 the blank key answers busy for permission'),
    (CONFIG, '#1872 the blank key is a sessionKey error on clear'),
    (CONFIG, '#1872 the blank key is a sessionKey error despite a busy owner reservation'),
    (CONFIG, '#1872 the blank key is a sessionKey error on clear despite a busy owner reservation'),
    (CONFIG, '#1872 the blank key is a sessionKey error for permission'),
    (CONFIG, '#1872 the fallback reports the admitted turn as busy'),
    (CONFIG, '#1872 the close targeted the owner spelling, not the request spelling'),
    (CONFIG, '#1457 pm busy message'),
    (CONFIG, '#1457 th busy message'),
    (CONFIG, '#1457 style busy message'),
    (CONFIG, '#1457 seed busy message'),
    (CONFIG, '#1864 the plan selection is stored for the next run to read'),
    (CONFIG, '#1864 the mode object is stored'),
    (CONFIG, '#1864 the stored permission mode reaches the agent config'),
    (CONFIG, '#1864 the stored thinking reaches the agent config'),
    (CONFIG, '#1864 the style content joins the existing prompt'),
    (CONFIG, '#1864 the list echoes the stored selection for the session'),
    (SESSION_DIR, '#1457 the transcript still loads'),
    (SESSION_DIR, '#1457 a cold append must not run beside a session that owns the writer'),
    (SESSION_DIR, '#1873 a cold status append must not run beside a session that owns the writer'),
    (CONFIG, '#1874 a cold model-set append must not run beside a session that owns the writer'),
    (CONFIG, '#1874 a closed session writer is an error, not a silent save'),
    (SESSION_DIR, '#1874 a closed session writer is an error, not a silent rename'),
    (SESSION_DIR, '#1874 a closed session writer is an error, not a recorded status'),
    (SESSION_DIR, '#1873 a broken lock file is a storage error, not the retryable busy answer'),
    (SESSION_DIR, '#1455 resume_session rejects the blank key'),
    (SESSION_DIR, '#1455 resume_session accepts the surrogate key'),
    (SESSION_DIR, '#2083 the surrogate projectKey reaches the capability gate'),
    (SESSION_DIR, '#2272 a null projectKey falls back like a missing one, so the sessionKey gate answers the blank key'),
    (SESSION_DIR, '#2564 a null projectKey keeps the domain message'),
    (SESSION_DIR, '#2564 a null projectKey lists the configured project'),
    (SESSION_DIR, '#2564 a null projectKey generates a session key'),
    (MANAGEMENT_TESTS, '#2562 explicit null is not an error with the recorder on'),
    (LIVE_TESTS, "#1864 the stored mode reaches the live session's next turn"),
    (FACTORY_TESTS, '#1860 hydrate holds the transcript fence until the recorder is published'),
    (REVIEW_TESTS, '#1864 the stored mode reaches the host admission context'),
    (REVIEW_TESTS, '#1429 the acquire path restores the generated ask rule'),
    (REVIEW_TESTS, '#2146 the second acquire reports nothing: one miss report per assembly'),
    (CRON_TESTS, '#1509 cron dispatch must yield the async worker before answering'),
    (SESSION_DIR, '#2090 unknown project cannot create session storage'),
    # #2068: both families' tests carry the shared TypeError anchor verbatim,
    # so the string is registered once per file (the `#1457 transcript still
    # loads` shape above).
    (SESSION_DIR, '#2068 a non-string projectKey is the path.resolve TypeError'),
    (CONFIG, '#2068 a non-string projectKey is the path.resolve TypeError'),
    (SESSION_DIR, '#2068 a string projectKey is a real key, never the fallback'),
    (CONFIG, '#2068 the non-string projectKey answers the Node TypeError code'),
    (CONFIG, '#2068 the surrogate projectKey reaches the registration gate'),
    (SESSION_DIR, '#2068 the surrogate projectKey keeps its raw unit inside the prefix'),
    # fix-r1: the bare-message mapping the throwing-object test pins.
    (SESSION_DIR, '#2068 the ToPrimitive failure keeps the bare V8 text'),
]

# Where an assertion can live: the mutated sources, the read-side files the
# #1864 rows assert on, and the test files those rows bind to (the #1860 row
# mutates `factory.rs` but asserts on a message in `factory_tests.rs`). A pin may
# not name a path outside this tuple (checked below) -- a pin the completeness
# scan does not read would be a guard with a blind spot.
SOURCE_TEXTS = (
    CONFIG,
    SESSION_DIR,
    METHODS_MOD,
    ASSEMBLY,
    LIVE,
    TOOLS,
    LIVE_TESTS,
    REVIEW_TESTS,
    FACTORY,
    FACTORY_TESTS,
    REQUESTS,
    SERVICES,
    MANAGEMENT_TESTS,
    CRON,
    CRON_TESTS,
    TRANSCRIPT_LOCKS,
    PROJECT_SCOPE,
)


def _assertions(case):
    return case.assertion if isinstance(case.assertion, list) else [case.assertion]


def check_literal_anchors(cases, pins=None, bound_rows=None):
    """`cases` are validated; a pin may bind to any of `bound_rows`.

    A targeted run selects a subset of the rows, so the subset must not be held
    against the whole pin table: binding is judged on the full table (#2065 r5
    review P1 -- a single-row rerun would otherwise refuse every other pin as
    `unbound to any row`).
    """
    pins = LITERAL_ANCHORS if pins is None else pins
    bound_rows = cases if bound_rows is None else bound_rows
    texts = {}
    for path in SOURCE_TEXTS:
        try:
            texts[path] = path.read_text()
        except OSError as error:
            raise SystemExit(f'#2065 scanned source unreadable: {path.name}: {error}')
    for case in cases:
        if case.path not in texts:
            raise SystemExit('#2065 row mutates a file outside SOURCE_TEXTS: '
                             f'{case.mutation} -> {case.path.name}')
    for path, anchor in pins:
        if path not in texts:
            raise SystemExit(
                f'#2065 literal anchor declared outside SOURCE_TEXTS: {path.name}')
        if anchor not in texts[path]:
            raise SystemExit(f'#2065 literal anchor not in {path.name}: {anchor!r}')
        if not any(anchor == assertion
                   for case in bound_rows for assertion in _assertions(case)):
            raise SystemExit(f'#2065 literal anchor unbound to any row: {anchor!r}')
    pinned = set(pins)
    for case in cases:
        for assertion in _assertions(case):
            # Every scanned file that holds the assertion verbatim needs its own
            # registration: one pin does not cover a copy in another file.
            unregistered = [path.name for path, text in texts.items()
                            if assertion in text and (path, assertion) not in pinned]
            if unregistered:
                raise SystemExit('#2065 unpinned literal anchor '
                                 f'(verbatim in {", ".join(unregistered)}): {assertion!r}')


def mutate(case, original):
    count = original.count(case.old)
    if count != 1 or case.old == case.new:
        raise ValueError(f'#1457 {case.mutation}: stale anchor (expected 1, got {count})')
    return original.replace(case.old, case.new, 1)


PUBLISH_DIR = ROOT / '.negctl-publish'


def write_atomic(path, data):
    """Publish the source in one rename so a concurrent reader never sees a
    half-written file (only the old or the mutated bytes) -- #1455 r2 review.

    The temp lives in `PUBLISH_DIR` at the repository root, not beside the
    target (#1692 §9 r2 review): a SIGKILL does not run `finally`, and a residue
    inside `src/` is read back as an "untracked build input" by
    `negative_process._assert_input_areas`, which then refuses every later run
    until someone deletes it by hand. The protection is the LOCATION: the root
    is outside the scanned areas (`src`, `.cargo`, `rust-toolchain*`). The
    `.gitignore`d `.negctl-*` name only keeps `git status` clean -- that scan
    uses `git ls-files --others` and includes ignored files on purpose, so the
    ignore rule is not what protects us. The mode comes from the source because
    `os.replace` publishes the temp's own mode.
    """
    PUBLISH_DIR.mkdir(parents=True, exist_ok=True)
    tmp = PUBLISH_DIR / f'.negctl-{path.name}.tmp'
    mode = path.stat().st_mode & 0o7777
    try:
        with tmp.open('wb') as stream:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        os.chmod(tmp, mode)
        os.replace(tmp, path)
    finally:
        tmp.unlink(missing_ok=True)


def main():
    args = sys.argv[1:]
    compile_only = '--compile-only' in args
    args = [arg for arg in args if arg != '--compile-only']
    every_row = [Case(*row) for row in mutations]
    selected_rows = every_row
    # r1 (DS sibling#2 / MM): this runner keys its evidence files on the
    # mutation name alone, so two rows sharing a name silently overwrite each
    # other's logs. The argv check below cannot see a name repeated in the
    # table itself; refuse the whole run (including `--check`) instead.
    if len({case.mutation for case in every_row}) != len(every_row):
        raise SystemExit('#1457 duplicate mutation name in the table')
    if args == ['--check']:
        # Guard first: it reports an unreadable scanned file as a clean refusal,
        # while the anchor probe below would raise a bare OSError on the same tree.
        check_literal_anchors(selected_rows)
        for case in selected_rows:
            mutate(case, case.path.read_text())
        print(f'#1457 {len(selected_rows)} mutation anchors OK; '
              f'#2065 {len(LITERAL_ANCHORS)} literal anchors OK')
        return 0
    if not args:
        raise SystemExit('usage: negative_controls.py --check | [--compile-only] NEW_OUTPUT [mutation ...]')
    env = dict(os.environ)
    if not env.get('CARGO_TARGET_DIR'):
        raise SystemExit('#1457 CARGO_TARGET_DIR must be supplied by the caller')
    for key in ('PDRS_NEGATIVE_RECEIPTS', 'PDRS_NEGATIVE_PARENT_TEST', 'PDRS_MUTANT'):
        env.pop(key, None)
    selected = args[1:]
    if (len(selected) != len(set(selected))
            or set(selected) - {case.mutation for case in selected_rows}):
        raise SystemExit('#1457 unknown/duplicate mutation name')
    if selected:
        selected_rows = [case for case in selected_rows if case.mutation in selected]
    # Runs the guard on the rows about to execute, so a stale anchor fails in
    # seconds instead of surfacing as an INVALID after the compile + timeout
    # budget of every selected row. Binding is judged against every row: a
    # targeted rerun selects a few rows but still owns the whole pin table.
    check_literal_anchors(selected_rows, bound_rows=every_row)
    out = Path(args[0]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    env.update(CARGO_PROFILE_DEV_DEBUG='line-tables-only', CARGO_PROFILE_TEST_DEBUG='line-tables-only')
    command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml', '-p', 'pilotdeck-gateway', '--lib']
    results = []

    def run(label, args):
        slot = wait_for_slot(ROOT / 'scripts/cargo-slots.sh', timeout=900)
        (out / (label + '.slots.log')).write_text(slot.stdout + slot.stderr)
        result = run_owned(command + args, cwd=ROOT, env=env, timeout=900)
        (out / (label + '.log')).write_text(result.stdout + result.stderr)
        return result

    def record(row):
        results.append(row)
        print(f"{row['mutation']}: {row['status']}: {row['reason']}", flush=True)

    def invalid(case, reason):
        return dict(mutation=case.mutation, test=case.test, assertion=case.assertion,
                    status='INVALID', reason=reason, compiled=False)

    # One lease covers the complete batch, including every restored positive.
    # No summary is published until the guard has verified restoration on exit.
    phase = 'enter'
    body_error = None
    try:
        paths = list(dict.fromkeys(case.path for case in selected_rows))
        with mutating_sources(ROOT, [*paths, Path(__file__).resolve()]) as originals:
            phase = 'body'
            try:
                for case in selected_rows:
                    row = invalid(case, '')
                    args = [case.test, '--', '--exact']
                    try:
                        changed = mutate(case, originals[case.path].decode())
                        if not compile_only and not passed(run(case.mutation + '-positive', args), case.test):
                            raise RuntimeError('#1457 positive control failed')
                        write_atomic(case.path, changed.encode())
                        compiled = run(case.mutation + '-compile', ['--no-run'])
                        row['compiled'] = compiled.returncode == 0
                        if not row['compiled']:
                            raise RuntimeError('#1457 mutant did not compile')
                        if compile_only:
                            row.update(status='COMPILED', reason='mutant cargo test --no-run passed')
                        else:
                            row['status'], row['reason'] = classify(run(case.mutation + '-negative', args), case)
                    except Exception as error:
                        row.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
                    finally:
                        phase = 'restore'
                        write_atomic(case.path, originals[case.path])
                        if case.path.read_bytes() != originals[case.path]:
                            raise RuntimeError('#1457 source restoration failed')
                        phase = 'body'
                    if not compile_only:
                        try:
                            if not passed(run(case.mutation + '-restored', args), case.test):
                                raise RuntimeError('#1457 restored positive control failed')
                        except Exception as error:
                            row.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
                    record(row)
            except BaseException as error:
                # Keep the refusal/cancellation object so the handler below can
                # tell it apart from a failure raised by the guard exit
                # (same contract as negative_suite.run_cases, #1076).
                body_error = error
                raise
            phase = 'exit'
    except BaseException as error:
        reason = f'source guard: {type(error).__name__}: {error}'
        invalidate_completed(results, reason)
        for case in selected_rows[len(results):]:
            record(invalid(case, reason))
        publish_summary(out, results)
        # #1455 r1: port of the run_cases three-state contract. Only an
        # unverified restore, a cancellation, or a guard that raised while
        # unwinding keeps rc=2; a refusal to run before the guard opened keeps
        # rc=1. ``body_error`` tells a body raise apart from a guard-exit
        # failure even while ``phase`` still says 'body'.
        if (phase in ('restore', 'exit') or not isinstance(error, Exception)
                or (body_error is not None and error is not body_error)):
            return 2
        return 1
    publish_summary(out, results)
    success = 'COMPILED' if compile_only else 'KILLED'
    return int(not results or any(row['status'] != success for row in results))


if __name__ == '__main__':
    raise SystemExit(main())
