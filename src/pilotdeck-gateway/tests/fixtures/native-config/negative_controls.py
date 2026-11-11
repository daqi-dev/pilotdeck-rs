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
CFG = 'native_process::methods::config::tests::'
FT = 'runtime::session::factory::tests::'
DIR = 'native_process::methods::session_dir::tests::'
mutations = [
    # ── stub reverts: the dispatch arm returns the pre-#1457 capability
    # terminal; the method's positive test must die.
    ('set-stub-revert', CONFIG,
     '"session_model_set" => self.session_model_set(&params).await,',
     '"session_model_set" => Err(gateway_error("CAPABILITY_UNAVAILABLE", "session_model_set is unavailable.")),',
     CFG + 'session_model_set_saves_selection_and_appends_transcript',
     '#1457 session_model_set accepts the fixture selection'),
    ('clear-stub-revert', CONFIG,
     '"session_model_clear" => self.session_model_clear(&params).await,',
     '"session_model_clear" => Err(gateway_error("CAPABILITY_UNAVAILABLE", "session_model_clear is unavailable.")),',
     CFG + 'session_model_clear_appends_transcript_and_returns_null',
     '#1457 session_model_clear succeeds'),
    ('permission-stub-revert', CONFIG,
     '"set_permission_mode" => self.set_permission_mode(&params).await,',
     '"set_permission_mode" => Err(gateway_error("CAPABILITY_UNAVAILABLE", "set_permission_mode is unavailable.")),',
     CFG + 'set_permission_mode_applies_and_validates',
     '#1864 set_permission_mode applies the fixture mode'),
    ('thinking-stub-revert', CONFIG,
     '"set_session_thinking" => self.set_session_thinking(&params).await,',
     '"set_session_thinking" => Err(gateway_error("CAPABILITY_UNAVAILABLE", "set_session_thinking is unavailable.")),',
     CFG + 'set_session_thinking_applies_and_validates',
     '#1864 set_session_thinking stores the mode object'),
    ('styles-list-stub-revert', CONFIG,
     '"output_styles_list" => self.output_styles_list(&params).await,',
     '"output_styles_list" => Err(gateway_error("CAPABILITY_UNAVAILABLE", "output_styles_list is unavailable.")),',
     CFG + 'output_styles_list_reports_project_styles',
     '#1457 output_styles_list without a plugin'),
    ('styles-set-stub-revert', CONFIG,
     '"set_output_style" => self.set_output_style(&params).await,',
     '"set_output_style" => Err(gateway_error("CAPABILITY_UNAVAILABLE", "set_output_style is unavailable.")),',
     CFG + 'set_output_style_validates_names_and_key',
     '#1864 set_output_style applies the probe style'),
    ('seed-stub-revert', CONFIG,
     '"seed_read_state" => self.seed_read_state(&params).await,',
     '"seed_read_state" => Err(gateway_error("CAPABILITY_UNAVAILABLE", "seed_read_state is unavailable.")),',
     CFG + 'seed_read_state_applies_and_validates',
     '#1457 seed_read_state with the exact mtime'),
    # ── gate mutants over the real wiring.
    ('selection-shape-gate-removed', CONFIG,
     'if !matches!(selection, JsValue::Object(_) | JsValue::Array(_)) {',
     'if false {',
     CFG + 'session_model_set_gate_order_matches_the_ts_chain',
     '#1457 missing selection message'),
    ('session-key-gate-code', CONFIG,
     '"INVALID_SESSION_KEY",\n        "sessionKey is required.",\n    )',
     '"INVALID_REQUEST",\n        "sessionKey is required.",\n    )',
     CFG + 'set_permission_mode_applies_and_validates',
     '#1457 missing sessionKey code'),
    ('permission-mode-gate-removed', CONFIG,
     'if !mode_ok {',
     'if !mode_ok && false {',
     CFG + 'set_permission_mode_applies_and_validates',
     # #2065: the panic rides a literal `expect_err`, so the payload keeps the
     # source's `{label}` verbatim. The prefix stops before it on purpose: it
     # matches both today's un-interpolated payload and the interpolated text
     # #1864 would produce, so the anchor survives either decision there.
     '#1457 set_permission_mode rejects '),
    ('mode-whitelist-narrowed', CONFIG,
     'PERMISSION_MODES\n                .iter()\n                .any(|allowed| m.equals_str(allowed))',
     'PERMISSION_MODES\n                .iter()\n                .any(|allowed| *allowed != "default" && m.equals_str(allowed))',
     CFG + 'set_permission_mode_applies_and_validates',
     '#1864 set_permission_mode applies the fixture mode'),
    ('thinking-shape-message', CONFIG,
     '            "INVALID_THINKING_CONFIG",\n            "thinking must be null or an object with an enabled boolean.",',
     '            "INVALID_THINKING_CONFIG",\n            "thinking must be null or an object with an enabled flag.",',
     CFG + 'set_session_thinking_applies_and_validates',
     '#1457 non-object thinking message'),
    ('seed-mtime-boundary', CONFIG,
     'n.is_finite() && n.fract() == 0.0 && *n >= 0.0',
     'n.is_finite() && n.fract() == 0.0 && *n > 0.0',
     CFG + 'seed_read_state_applies_and_validates',
     '#1457 seed_read_state returns the stale outcome'),
    ('seed-path-gate-code', CONFIG,
     '"INVALID_FILE_PATH", "path is required."',
     '"INVALID_REQUEST", "path is required."',
     CFG + 'seed_read_state_applies_and_validates',
     '#1457 absent path code'),
    ('transcript-clear-null', CONFIG,
     'selection.cloned().unwrap_or(JsValue::Null),',
     'selection.cloned().unwrap_or(JsValue::Undefined),',
     CFG + 'session_model_clear_appends_transcript_and_returns_null',
     '#1457 clear entry nulls the model selection'),
    # ── fix-r1 P0: the per-session single-writer fence. Dropping either side
    # puts two fresh writers back on the same tail, so the concurrent case must
    # see a duplicate `sequence` and a transcript the next restore() rejects.
    ('transcript-fence-removed-config', CONFIG,
     '        let write_fence = transcript_locks::acquire(&fence_path).await?;',
     '        let write_fence = ();',
     CFG + 'concurrent_transcript_writers_keep_the_chain_loadable',
     '#1457 transcript sequences strictly increase'),
    ('transcript-fence-removed-rename', SESSION_DIR,
     '        let fence_path =\n'
     '            transcript_locks::session_transcript_path(&self.options.home, &project_root, &key);\n'
     '        let write_fence = transcript_locks::acquire(&fence_path).await?;',
     '        let fence_path =\n'
     '            transcript_locks::session_transcript_path(&self.options.home, &project_root, &key);\n'
     '        let write_fence = ();',
     CFG + 'concurrent_transcript_writers_keep_the_chain_loadable',
     '#1457 transcript sequences strictly increase'),
    # ── fix-r2 P0: a loaded session must be written *through* its recorder.
    # Bypassing the lookup sends the entry to a fresh writer on the same tail;
    # the loaded writer's next append then duplicates `sequence + 1` and the
    # bound test's chain assertion must catch it. #1874 re-anchored the three
    # rows on the callers' `live` closures — the lookup itself moved into the
    # shared helper, so neutralising the closure's recorder lookup is the
    # same bypass it always was.
    ('transcript-live-bypass-config', CONFIG,
     '                    super::super::assembly::live_recorder(&recorders, &key)',
     '                    None::<(pilotdeck_protocol::JsString,'
     ' std::sync::Arc<crate::storage::recorder::Recorder>)>',
     CFG + 'model_set_appends_through_the_loaded_writer',
     '#1457 the transcript still loads'),
    ('transcript-live-bypass-rename', SESSION_DIR,
     '                    super::super::assembly::live_recorder(&recorders, &key_clone)',
     '                    None::<(pilotdeck_protocol::JsString,'
     ' std::sync::Arc<crate::storage::recorder::Recorder>)>',
     DIR + 'rename_appends_through_the_loaded_writer',
     '#1457 the transcript still loads'),
    # ── fix-r3: the status live branch and the cold writer lease.
    ('transcript-live-bypass-status', SESSION_DIR,
     '                    super::super::assembly::live_recorder(&recorders, &session_key)',
     '                    None::<(pilotdeck_protocol::JsString,'
     ' std::sync::Arc<crate::storage::recorder::Recorder>)>',
     DIR + 'status_appends_through_the_loaded_writer',
     '#1457 the transcript still loads'),
    # ── #1860: hydration itself must hold the fence across
    # `restore()` → publish-recorder. Dropping it right after acquisition is
    # exactly the pre-fix shape, and the bound test's waiter then gets the
    # fence while the hydrate is still parked.
    ('transcript-hydrate-fence-removed', FACTORY,
     '            let _write_fence = super::transcript_locks::acquire(&fence_path)\n'
     '                .await\n'
     '                .map_err(|error| {\n'
     '                    RuntimeError::new(String::from_utf16_lossy(error.message.units()))\n'
     '                })?;',
     '            let _write_fence = super::transcript_locks::acquire(&fence_path)\n'
     '                .await\n'
     '                .map_err(|error| {\n'
     '                    RuntimeError::new(String::from_utf16_lossy(error.message.units()))\n'
     '                })?;\n'
     '            drop(_write_fence);',
     FT + 'hydration_holds_the_transcript_fence_until_the_recorder_is_published',
     '#1860 hydrate holds the transcript fence until the recorder is published'),
    # ── #1874: the cold-write lease now lives once in the shared helper. One
    # anchor, one mutation, three rows — the bound test is what says which
    # caller's busy answer the skipped lease took away. Skipping the lease
    # lets the cold append run beside the holder and the bound expect_err
    # dies on a success.
    ('transcript-cold-lease-skipped', TRANSCRIPT_LOCKS,
     '    let Some(_lease) = cold_write_lease(&storage.transcript_path)? else {\n'
     '        // A session claimed the transcript between the lookup and here.',
     '    let Some(_lease) = Ok::<_, RequestError>(Some::<()>(()))? else {\n'
     '        // A session claimed the transcript between the lookup and here.',
     DIR + 'cold_metadata_write_reports_busy_while_a_session_holds_the_lease',
     '#1457 a cold append must not run beside a session that owns the writer'),
    ('transcript-cold-lease-skipped-status', TRANSCRIPT_LOCKS,
     '    let Some(_lease) = cold_write_lease(&storage.transcript_path)? else {\n'
     '        // A session claimed the transcript between the lookup and here.',
     '    let Some(_lease) = Ok::<_, RequestError>(Some::<()>(()))? else {\n'
     '        // A session claimed the transcript between the lookup and here.',
     DIR + 'status_cold_write_reports_busy_while_a_session_holds_the_lease',
     '#1873 a cold status append must not run beside a session that owns the writer'),
    ('transcript-cold-lease-skipped-config', TRANSCRIPT_LOCKS,
     '    let Some(_lease) = cold_write_lease(&storage.transcript_path)? else {\n'
     '        // A session claimed the transcript between the lookup and here.',
     '    let Some(_lease) = Ok::<_, RequestError>(Some::<()>(()))? else {\n'
     '        // A session claimed the transcript between the lookup and here.',
     CFG + 'model_set_cold_write_reports_busy_while_a_session_holds_the_lease',
     '#1874 a cold model-set append must not run beside a session that owns the writer'),
    # ── #1874: each caller's live closure must use the checked append — a
    # writer that closed under the request is an error, not a silent no-op
    # reported as success. Reverting one caller to the unchecked form makes
    # its closed-writer test's expect_err die on a success.
    ('model-set-checked-append-reverted', CONFIG,
     '                    .record_session_metadata_checked(&key, &turn, entry.clone())',
     '                    .record_session_metadata(&key, &turn, entry.clone())',
     CFG + 'model_set_reports_a_closed_writer_as_an_error',
     '#1874 a closed session writer is an error, not a silent save'),
    ('rename-checked-append-reverted', SESSION_DIR,
     '                    .record_session_metadata_checked(&key_clone, &turn_id, entry.clone())',
     '                    .record_session_metadata(&key_clone, &turn_id, entry.clone())',
     DIR + 'rename_reports_a_closed_writer_as_an_error',
     '#1874 a closed session writer is an error, not a silent rename'),
    ('status-checked-append-reverted', SESSION_DIR,
     '                    .record_agent_status_message_checked(&session_key, &turn_id, &status)',
     '                    .record_agent_status_message(&session_key, &turn_id, &status)',
     DIR + 'status_reports_a_closed_writer_as_an_error',
     '#1874 a closed session writer is an error, not a recorded status'),
    # ── #1873 P2: `try_writer_lease` must not fold storage failures into the
    # retryable busy answer. Reverting to the pre-fix fold makes the broken
    # lock request answer SESSION_WRITER_BUSY, and the bound test's code
    # assertion dies on exactly that.
    ('writer-lease-folded-to-busy', FACTORY,
     '        Err(crate::storage::LedgerError::Io(ref io))\n'
     '            if io.kind() == std::io::ErrorKind::WouldBlock =>\n'
     '        {\n'
     '            return Ok(None);\n'
     '        }\n'
     '        Err(e) => return Err(RuntimeError::new(e.to_string())),',
     '        Err(crate::storage::LedgerError::Io(ref io))\n'
     '            if io.kind() == std::io::ErrorKind::WouldBlock =>\n'
     '        {\n'
     '            return Ok(None);\n'
     '        }\n'
     '        Err(_) => return Ok(None),',
     DIR + 'status_cold_write_surfaces_a_broken_lock_file_as_an_error',
     '#1873 a broken lock file is a storage error, not the retryable busy answer'),
    # ── #1872: the alias-key busy gate. Reverting the owner-side check to the
    # pre-fix request-key-only shape lets the request through; it appends
    # through the owner's recorder and the close aborts the running turn, so
    # the bound test's expect_err dies on a success. #2692 fix-r1
    # (sibling#2): re-anchored onto the router-domain gate — the owner arm is
    # now the string-request conjunction `string_request && matches!(… if
    # trimmed_string_is_non_empty(key)) && owner.is_some_and(…)`; the mutant
    # floors the last conjunct to `false`, the same request-key-only shape.
    ('set-alias-owner-busy-removed', CONFIG,
     '        } || string_request\n'
     '            && matches!(&request_key, Ok(key) if trimmed_string_is_non_empty(key))\n'
     '            && owner.is_some_and(|owner| self.owner.has_active_turn(owner))\n'
     '        {',
     '        } || string_request\n'
     '            && matches!(&request_key, Ok(key) if trimmed_string_is_non_empty(key))\n'
     '            && false\n'
     '        {',
     CFG + 'session_model_set_rejects_active_turn_on_alias_owner',
     '#1872 session_model_set is busy on the alias owner'),
    # ── #1872 fix-r2: the owner-side blank-key arm of the same gate. TS's
    # busy gate consults the request spelling alone, so a blank request key
    # that folds onto the `session` transcript must reach the port write's
    # sessionKey gate; dropping the `trimmed_string_is_non_empty(key)` guard
    # from the conjunction lets the owner-side check fire first and the bound
    # test dies on the SESSION_BUSY answer instead of the sessionKey gate's.
    # The anchor is the test's own assert label — the expect_err message
    # never reaches the panic payload on this path (cb8 sample r1 INVALID).
    # #2692 fix-r1 (sibling#2): re-anchored onto the router-domain gate —
    # `matches!(&request_key, Ok(key) if trimmed_string_is_non_empty(key))`
    # becomes `matches!(&request_key, Ok(_))`.
    ('set-busy-gate-blank-key-guard-removed', CONFIG,
     '        } || string_request\n'
     '            && matches!(&request_key, Ok(key) if trimmed_string_is_non_empty(key))\n'
     '            && owner.is_some_and(|owner| self.owner.has_active_turn(owner))\n'
     '        {',
     '        } || string_request\n'
     '            && matches!(&request_key, Ok(_))\n'
     '            && owner.is_some_and(|owner| self.owner.has_active_turn(owner))\n'
     '        {',
     CFG + 'session_model_set_blank_key_answers_the_session_key_gate',
     '#1872 the blank key is a sessionKey error'),
    # ── #1872 fix-r2: the request-side arm for a blank key. TS answers
    # SESSION_BUSY for a turn held on the blank spelling itself (probe B),
    # so the request-side check must stay live; reverting to the fix-r1
    # whole-gate skip makes the bound test see INVALID_SESSION_KEY instead
    # of the busy code. #2692 fix-r1 (sibling#2): re-anchored onto the
    # router-domain gate — the request arm grows the blank-key skip
    # `trimmed_string_is_non_empty(key) &&` in front of its table check.
    ('set-blank-key-busy-reverted-to-gate-skip', CONFIG,
     '        if match &request_key {\n'
     '            Ok(key) => self.owner.has_active_turn(key),\n'
     '            Err(_) => false,\n'
     '        } || string_request\n',
     '        if match &request_key {\n'
     '            Ok(key) => trimmed_string_is_non_empty(key) && self.owner.has_active_turn(key),\n'
     '            Err(_) => false,\n'
     '        } || string_request\n',
     CFG + 'session_model_set_reports_busy_for_a_turn_held_on_the_blank_key',
     '#1872 the blank key answers busy'),
    # ── #1872 fix-r2 arbitration of the DS P1: the real TS wiring runs
    # hasActiveTurn on the raw request key before the callback's sessionKey
    # gate (probe C), so permission needs no blank-key guard — this mutant
    # is the reviewed-away shape, and the bound test dies on the
    # INVALID_SESSION_KEY answer instead of the busy code. #2692 fix-r1
    # (sibling#2): re-anchored onto the router-domain gate — the request arm
    # grows the blank-key skip inside `is_ok_and`.
    ('permission-blank-key-busy-skipped', CONFIG,
     '        if request_key.is_ok_and(|key| self.owner.has_active_turn(&key)) {',
     '        if request_key\n'
     '            .is_ok_and(|key| trimmed_string_is_non_empty(&key) && self.owner.has_active_turn(&key))\n'
     '        {',
     CFG + 'set_permission_mode_reports_busy_for_a_turn_held_on_the_blank_key',
     '#1872 the blank key answers busy for permission'),
    # ── #1872 fix-r2: clear answers the sessionKey gate first — TS's
    # sessionModelClear runs requireSessionKey before hasActiveTurn. The
    # mutant weakens the require to a blank default, so the busy gate (with
    # a live blank-spelling turn) answers SESSION_BUSY and the bound test
    # dies on the INVALID_SESSION_KEY assert.
    ('clear-blank-key-require-weakened', CONFIG,
     '        let session_key = require_session_key(params)?;\n'
     '        if self.owner.has_active_turn(&session_key)\n'
     '            || owner.is_some_and(|owner| self.owner.has_active_turn(owner))\n'
     '        {',
     '        let session_key = require_session_key(params).unwrap_or_default();\n'
     '        if self.owner.has_active_turn(&session_key)\n'
     '            || owner.is_some_and(|owner| self.owner.has_active_turn(owner))\n'
     '        {',
     CFG + 'session_model_clear_blank_key_answers_the_session_key_gate',
     '#1872 the blank key is a sessionKey error on clear'),
    # ── #1872 fix-r4: the owner-side reservation skip for a blank request
    # key. The owner stays resolved (the busy gate inside keeps its own
    # blank-key guard live); dropping the `owner_reserved` conjunct
    # reserves the folded owner spelling for a request TS rejects as
    # INVALID_SESSION_KEY, so a busy reservation under that owner answers
    # SESSION_BUSY and the bound test dies on the sessionKey-gate assert.
    ('set-blank-key-owner-reservation-restored', CONFIG,
     '        let reserved_key = params.get("sessionKey").and_then(JsValue::as_string);\n'
     '        let owner = self.live_transcript_owner(reserved_key);\n'
     '        let owner_reserved = matches!(\n'
     '            (&owner, reserved_key),\n'
     '            (Some(owner), Some(key)) if owner != key && trimmed_string_is_non_empty(key)\n'
     '        );\n'
     '        let owner_guard = self\n'
     '            .replacement\n'
     '            .reserve_transcript_write_held(\n'
     '                if owner_reserved { owner.as_ref() } else { None },\n'
     '                "change the session model",\n'
     '            )\n'
     '            .map_err(replacement_error)?;',
     '        let reserved_key = params.get("sessionKey").and_then(JsValue::as_string);\n'
     '        let owner = self.live_transcript_owner(reserved_key);\n'
     '        let owner_reserved = matches!(\n'
     '            (&owner, reserved_key),\n'
     '            (Some(owner), Some(key)) if owner != key\n'
     '        );\n'
     '        let owner_guard = self\n'
     '            .replacement\n'
     '            .reserve_transcript_write_held(\n'
     '                if owner_reserved { owner.as_ref() } else { None },\n'
     '                "change the session model",\n'
     '            )\n'
     '            .map_err(replacement_error)?;',
     CFG + 'session_model_set_blank_key_ignores_a_busy_owner_reservation',
     '#1872 the blank key is a sessionKey error despite a busy owner reservation'),
    # ── #1872 fix-r4: the clear twin of the owner-reservation skip.
    ('clear-blank-key-owner-reservation-restored', CONFIG,
     '        let reserved_key = params.get("sessionKey").and_then(JsValue::as_string);\n'
     '        let owner = self.live_transcript_owner(reserved_key);\n'
     '        let owner_reserved = matches!(\n'
     '            (&owner, reserved_key),\n'
     '            (Some(owner), Some(key)) if owner != key && trimmed_string_is_non_empty(key)\n'
     '        );\n'
     '        let owner_guard = self\n'
     '            .replacement\n'
     '            .reserve_transcript_write_held(\n'
     '                if owner_reserved { owner.as_ref() } else { None },\n'
     '                "clear the session model",\n'
     '            )\n'
     '            .map_err(replacement_error)?;',
     '        let reserved_key = params.get("sessionKey").and_then(JsValue::as_string);\n'
     '        let owner = self.live_transcript_owner(reserved_key);\n'
     '        let owner_reserved = matches!(\n'
     '            (&owner, reserved_key),\n'
     '            (Some(owner), Some(key)) if owner != key\n'
     '        );\n'
     '        let owner_guard = self\n'
     '            .replacement\n'
     '            .reserve_transcript_write_held(\n'
     '                if owner_reserved { owner.as_ref() } else { None },\n'
     '                "clear the session model",\n'
     '            )\n'
     '            .map_err(replacement_error)?;',
     CFG + 'session_model_clear_blank_key_ignores_a_busy_owner_reservation',
     '#1872 the blank key is a sessionKey error on clear despite a busy owner reservation'),
    # ── #1872 fix-r4: permission's blank key with no turn must reach the
    # sessionKey gate. Weakening the require to a blank default returns Ok,
    # so the bound test dies at its expect_err — whose message is the
    # anchor itself; an Ok never reaches the assert labels (cb8 r5
    # INVALID, DS fix-r3 sibling#2).
    ('permission-blank-key-require-weakened', CONFIG,
     '        let session_key = require_session_key(params)?;\n'
     '        let mode = match mode.as_string() {',
     '        let session_key = require_session_key(params).unwrap_or_default();\n'
     '        let mode = match mode.as_string() {',
     CFG + 'set_permission_mode_blank_key_answers_the_session_key_gate',
     '#1872 the blank key is a sessionKey error for permission'),
    ('clear-alias-owner-busy-removed', CONFIG,
     '        if self.owner.has_active_turn(&session_key)\n'
     '            || owner.is_some_and(|owner| self.owner.has_active_turn(owner))\n'
     '        {',
     '        if self.owner.has_active_turn(&session_key) {',
     CFG + 'session_model_clear_rejects_active_turn_on_alias_owner',
     '#1872 session_model_clear is busy on the alias owner'),
    # ── #1872: the owner-side transcript-write reservation itself. Without it
    # the check→append→close window admits a turn on the owner's own key —
    # the bound test polls the reservation table while the request is parked
    # on the transcript fence and dies when the owner's key never shows up.
    ('set-alias-owner-reservation-skipped', CONFIG,
     '        let owner_guard = self\n'
     '            .replacement\n'
     '            .reserve_transcript_write_held(\n'
     '                if owner_reserved { owner.as_ref() } else { None },\n'
     '                "change the session model",\n'
     '            )\n'
     '            .map_err(replacement_error)?;',
     '        let owner_guard = self\n'
     '            .replacement\n'
     '            .reserve_transcript_write_held(\n'
     '                if false { owner.as_ref() } else { None },\n'
     '                "change the session model",\n'
     '            )\n'
     '            .map_err(replacement_error)?;',
     CFG + 'session_model_set_holds_the_owner_reservation_through_the_write',
     '#1872 the owner and alias reservations are held while the write is parked'),
    ('clear-alias-owner-reservation-skipped', CONFIG,
     '        let owner_guard = self\n'
     '            .replacement\n'
     '            .reserve_transcript_write_held(\n'
     '                if owner_reserved { owner.as_ref() } else { None },\n'
     '                "clear the session model",\n'
     '            )\n'
     '            .map_err(replacement_error)?;',
     '        let owner_guard = self\n'
     '            .replacement\n'
     '            .reserve_transcript_write_held(\n'
     '                if false { owner.as_ref() } else { None },\n'
     '                "clear the session model",\n'
     '            )\n'
     '            .map_err(replacement_error)?;',
     CFG + 'session_model_clear_holds_the_owner_reservation_through_the_write',
     '#1872 the clear-side owner and alias reservations are held while the write is parked'),
    # ── #1872: the fallback arm of close_model_selection_owner — the owner
    # first seen by the append itself, published while the request was in
    # flight, so the wrapper holds no reservation for it. Removing its
    # has_active_turn recheck makes the close abort the admitted turn, and
    # the bound test dies on a success instead of the busy error.
    ('close-owner-fallback-busy-skipped', CONFIG,
     '        if self.owner.has_active_turn(target) {\n'
     '            self.replacement.release_transcript_write(target);\n'
     '            return Err(gateway_error("SESSION_BUSY", busy_message));\n'
     '        }',
     '        if false {\n'
     '            self.replacement.release_transcript_write(target);\n'
     '            return Err(gateway_error("SESSION_BUSY", busy_message));\n'
     '        }',
     CFG + 'close_model_selection_owner_reports_busy_for_an_owner_seen_by_the_append',
     '#1872 the fallback reports the admitted turn as busy'),
    # ── #1872: the close target. Folding the owner spelling back to the
    # request spelling leaves the owner session cached and serving the old
    # selection; the bound test dies on its current_handle assertion.
    ('set-closes-owner-target-reverted', CONFIG,
     '        let target = loaded.unwrap_or(session_key);',
     '        let target = session_key;',
     CFG + 'model_set_closes_the_owner_spelling_of_an_alias_request',
     '#1872 the close targeted the owner spelling, not the request spelling'),
    ('seed-mtime-wiring', CONFIG,
     '.seed_read_state(&path, mtime)',
     '.seed_read_state(&path, 0.0)',
     CFG + 'seed_read_state_applies_and_validates',
     '#1457 exact mtime applies'),
    ('set-busy-message', CONFIG,
     '"SESSION_BUSY",\n                "Cannot change the model during an active turn.",',
     '"SESSION_BUSY",\n                "Cannot change the model while a turn is active.",',
     CFG + 'session_model_set_rejects_active_turn',
     '#1457 busy message'),
    ('clear-busy-message', CONFIG,
     '"SESSION_BUSY",\n                "Cannot clear the model during an active turn.",',
     '"SESSION_BUSY",\n                "Cannot clear the model while a turn is active.",',
     CFG + 'session_model_clear_gates_key_and_busy',
     '#1457 clear busy message'),
    ('permission-busy-message', CONFIG,
     '"SESSION_BUSY",\n                "Cannot change permission mode during an active turn.",',
     '"SESSION_BUSY",\n                "Cannot change permission mode while a turn is active.",',
     CFG + 'set_permission_mode_applies_and_validates',
     '#1457 pm busy message'),
    ('thinking-busy-message', CONFIG,
     '"SESSION_BUSY",\n                "Cannot change thinking while a turn is active.",',
     '"SESSION_BUSY",\n                "Cannot change thinking during an active turn.",',
     CFG + 'set_session_thinking_applies_and_validates',
     '#1457 th busy message'),
    ('style-busy-message', CONFIG,
     '"SESSION_BUSY",\n                "Cannot change output style during an active turn.",',
     '"SESSION_BUSY",\n                "Cannot change output style while a turn is active.",',
     CFG + 'set_output_style_validates_names_and_key',
     '#1457 style busy message'),
    ('seed-busy-message', CONFIG,
     '"SESSION_BUSY",\n                "Cannot seed file read state while a turn is active.",',
     '"SESSION_BUSY",\n                "Cannot seed file read state during an active turn.",',
     CFG + 'seed_read_state_applies_and_validates',
     '#1457 seed busy message'),
    # ── cross-family: the shared trim helper the session directory reuses.
    ('shared-trim-dropped', CONFIG,
     '!crate::metadata::js_trim(&String::from_utf16_lossy(value.units())).is_empty()',
     '!String::from_utf16_lossy(value.units()).is_empty()',
     CFG + 'set_output_style_validates_names_and_key',
     '#1457 blank name code'),
    # #1604: the session directory now delegates to the shared `require_text`
    # guard instead of keeping its own `?.trim()` chain, so these two mutants
    # move to the guard they are meant to kill. Bound tests are unchanged.
    ('session-dir-blank-gate-removed', METHODS_MOD,
     '            if crate::metadata::js_trim(&text).is_empty() {\n'
     '                Ok(TrimGuard::Missing)',
     '            if false {\n'
     '                Ok(TrimGuard::Missing)',
     DIR + 'resume_session_rejects_blank_session_key',
     '#1455 resume_session rejects the blank key'),
    ('session-dir-surrogate-rejected', METHODS_MOD,
     '            if crate::metadata::js_trim(&text).is_empty() {',
     '            if value.to_utf8().is_err() || crate::metadata::js_trim(&text).is_empty() {',
     DIR + 'resume_session_accepts_lone_surrogate_session_key',
     '#1455 resume_session accepts the surrogate key'),
    # #1604: `close_project_sessions` keeps TS's plain `Error` shape on the
    # missing/blank branch; this mutant rewords the domain message so that
    # branch's test is the one that has to die.
    ('session-dir-project-key-domain-message', SESSION_DIR,
     '            "projectKey is required.",\n'
     '        )?;',
     '            "projectKey is missing.",\n'
     '        )?;',
     DIR + 'close_project_sessions_blank_project_key_is_the_domain_error',
     '#1604 the blank projectKey keeps the domain message'),
    # #2083: a lone-surrogate `projectKey` passes the trim gate and reaches
    # the configured-project check (TS runs it to completion; the baseline
    # gate is only `?.trim()`). Re-adding the pre-#2083 UTF-8 rejection must
    # turn the gate-order test red. r1: renamed off the #1455
    # `session-dir-surrogate-rejected` (METHODS_MOD) — this runner keys its
    # evidence files on the mutation name alone, so two rows sharing a name
    # overwrite each other's logs.
    ('session-dir-project-key-surrogate-rejected', SESSION_DIR,
     '        let project_key_str = String::from_utf16_lossy(project_key.units());',
     '        let project_key_str = project_key\n'
     '            .to_utf8()\n'
     '            .map_err(|_| RequestError::new("projectKey is required."))?;',
     DIR + 'close_project_sessions_surrogate_project_key_reaches_the_capability_gate',
     '#2083 the surrogate projectKey reaches the capability gate'),
    # ── #2272: the admission filter skips `undefined` AND `null` (the TS
    # nullish read `input.projectKey ?? fallback`, createLocalGateway.ts).
    # Dropping the null arm restores the pre-#2272 shape where an explicit
    # null was refused as an unserved project even with a valid sessionKey,
    # so the session directory's null-cell gate-order expectations must die.
    # The bound test fails first at its blank-key null cell, before the
    # per-method valid-key cells that follow it.
    ('admission-null-refused', REQUESTS,
     'params\n'
     '                .get("projectKey")\n'
     '                .filter(|v| !matches!(v, JsValue::Undefined | JsValue::Null))',
     'params\n'
     '                .get("projectKey")\n'
     '                .filter(|v| !v.is_undefined())',
     DIR + 'resume_session_blank_key_gate_order',
     '#2272 a null projectKey falls back like a missing one, so the sessionKey gate answers the blank key'),
    # ── #2562: the submit path's gate is the same nullish read (the TS submit
    # source is `workspaceIdResolver(input.projectKey)` →
    # `resolveProjectStorageId(requestedProjectKey ?? fallbackProjectRoot)`,
    # probes `_tmp/20261026-issue-2562/ts-probe/`). Dropping the null arm
    # restores the pre-#2562 shape where an explicit null was answered the
    # `projectKey must be a valid path string.` type error instead of falling
    # back to the configured project, so the recorder-on null cell must die.
    ('submit-null-refused', SERVICES,
     '        if let Some(project) = input\n'
     '            .get("projectKey")\n'
     '            .filter(|v| !matches!(v, JsValue::Undefined | JsValue::Null))\n'
     '        {',
     '        if let Some(project) = input.get("projectKey").filter(|v| !v.is_undefined()) {',
     'native_process::management_tests::submit_workspace_id_treats_an_explicit_null_project_key_as_not_provided',
     '#2562 explicit null is not an error with the recorder on'),
    # ── #2564: the #2272 admission mutant re-bound to the close_project_sessions
    # path — with the null arm dropped the admission refusal (isolated-project
    # message) answers before the method's own `?.trim()` gate, so the null
    # cell's domain-message assertion must die. Same mutant text as
    # `admission-null-refused` above: one code point, four bound tests.
    ('admission-null-refused-close-project', REQUESTS,
     'params\n'
     '                .get("projectKey")\n'
     '                .filter(|v| !matches!(v, JsValue::Undefined | JsValue::Null))',
     'params\n'
     '                .get("projectKey")\n'
     '                .filter(|v| !v.is_undefined())',
     DIR + 'close_project_sessions_null_project_key_is_the_domain_error',
     '#2564 a null projectKey keeps the domain message'),
    # ── #2564 fix-r1 (harvest reviews: DS sibling#2 + anthropic boundary#2,
    # two lineages hit the same gap): the same admission mutant re-bound to the
    # list/new null cells. With the null arm dropped the admission refusal
    # (isolated-project message) answers before each method's own nullish read,
    # so the cell's `.expect` must die at its own message — without these rows
    # the two cells were guarded only by their positive assertions.
    ('admission-null-refused-list-sessions', REQUESTS,
     'params\n'
     '                .get("projectKey")\n'
     '                .filter(|v| !matches!(v, JsValue::Undefined | JsValue::Null))',
     'params\n'
     '                .get("projectKey")\n'
     '                .filter(|v| !v.is_undefined())',
     DIR + 'list_sessions_null_project_key_lists_the_configured_project',
     '#2564 a null projectKey lists the configured project'),
    ('admission-null-refused-new-session', REQUESTS,
     'params\n'
     '                .get("projectKey")\n'
     '                .filter(|v| !matches!(v, JsValue::Undefined | JsValue::Null))',
     'params\n'
     '                .get("projectKey")\n'
     '                .filter(|v| !v.is_undefined())',
     DIR + 'new_session_null_project_key_omits_the_project_prefix',
     '#2564 a null projectKey generates a session key'),
    # ── #1864: the three selectors now have a reader. Each write site
    # (permission, thinking, output style), each compose-step read leg
    # (permission, thinking, style merge), the live adapter's per-turn source
    # and the `output_styles_list` selected echo get a function-internal
    # mutant, so a store that is written but never read (the -R4 defect)
    # turns the bound positive red instead of passing.
    ('permission-store-write-removed', CONFIG,
     '        self.session_overrides\n'
     '            .set_permission_mode(&session_key, mode);',
     '        let _ = (&session_key, &mode);',
     CFG + 'set_permission_mode_applies_and_validates',
     '#1864 the plan selection is stored for the next run to read'),
    ('thinking-store-write-removed', CONFIG,
     '            thinking => self\n'
     '                .session_overrides\n'
     '                .set_thinking(&session_key, thinking.clone()),',
     '            thinking => {\n'
     '                let _ = thinking;\n'
     '            }',
     CFG + 'set_session_thinking_applies_and_validates',
     '#1864 the mode object is stored'),
    ('style-store-write-removed', CONFIG,
     '            Some(name) => self\n'
     '                .session_overrides\n'
     '                .set_output_style(&session_key, name.clone()),',
     '            Some(name) => {\n'
     '                let _ = (&session_key, name);\n'
     '            }',
     CFG + 'set_output_style_validates_names_and_key',
     '#1864 set_output_style applies the probe style'),
    ('compose-permission-readback-removed', ASSEMBLY,
     '    let mode = effective_permission_mode(config, stored_permission_mode.cloned());',
     '    let mode = effective_permission_mode(config, None);',
     CFG + 'selector_writes_reach_the_compose_step',
     '#1864 the stored permission mode reaches the agent config'),
    ('compose-thinking-readback-removed', ASSEMBLY,
     '    if let Some(thinking) = stored_thinking {',
     '    if let Some(thinking) = None::<&JsValue> {',
     CFG + 'selector_writes_reach_the_compose_step',
     '#1864 the stored thinking reaches the agent config'),
    ('compose-style-merge-removed', ASSEMBLY,
     '    if let Some(content) = style_content {',
     '    if let Some(content) = None::<&str> {',
     CFG + 'apply_session_overrides_composes_the_selections',
     '#1864 the style content joins the existing prompt'),
    ('styles-list-selected-removed', CONFIG,
     '            && let Some(selected) = self.session_overrides.output_style(key)',
     '            && let Some(selected) = None::<JsString>',
     CFG + 'output_styles_list_reports_project_styles',
     '#1864 the list echoes the stored selection for the session'),
    ('submit-permission-source-removed', LIVE,
     '            if let Some(source) = &self.permission_mode_source',
     '            if let Some(source) = &None::<Arc<dyn Fn() -> Option<JsString> + Send + Sync>>',
     'runtime::turn::live::stream_tests::permission_mode_source_rewrites_the_agent_config_each_turn',
     "#1864 the stored mode reaches the live session's next turn"),
    ('tools-admission-source-removed', TOOLS,
     '            if let Some(mode) = permission::managed_default_mode(&self.config).or_else(|| {\n'
     '                self.permission_mode_source\n'
     '                    .as_ref()\n'
     '                    .and_then(|source| source())\n'
     '            }) {',
     '            if let Some(mode) = None::<JsString> {',
     'native_process::tests::review::session_tools_admission_mode_follows_the_store_each_turn',
     '#1864 the stored mode reaches the host admission context'),
    # ── #2146: the two #1429 acquire-path rulers had no mutants of their own.
    # Skipping the sync on acquire and dropping the once-per-assembly report
    # dedup both left the review tests green, so each rule gets a row bound to
    # the test that states it.
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
    # ── #2195 (P2-跟进 #1509): the cron dispatch's blocking-pool handoff was
    # pinned only by the structural positive control — no mutant proved that
    # control could go red. This row inlines the dispatch onto the async
    # worker (the management family's `memory-inline-sync` shape, #1322): on
    # the single-threaded runtime the `join!` polls `create` to completion on
    # its first poll, the sibling never runs in between, and `ran_before`
    # stays false at the bound assertion. The `Ok::<_, JoinError>` pin keeps
    # the value's type identical to the `spawn_blocking` form — plain `Ok(...)`
    # leaves the error type uninferred (E0282, cb8 r1 compile log in
    # _tmp/20261026-issue-2195/review-r1/); the issue's recipe said the bare
    # form compiled, which the first host run disproved.
    # Re-anchored after #2641 merged: the closure body is now
    # `dispatch_under_management_gate(&cron, ...)` — same handoff. The
    # mutant inlines the underlying `cron::dispatch`, not the gate wrapper:
    # the wrapper's `enter_dispatch()` is a blocking-only API (`blocking_read`
    # panics "Cannot block the current thread from within a runtime" when run
    # on the async worker — cb8 r4 judged that shape INVALID as a wrong panic,
    # `_tmp/20261026-issue-2195/review-r4/`), and a mutant is already-broken
    # code whose only contract is failing the bound test at its bound
    # assertion; the gate also has its own race test in `switch_tests.rs`, so
    # bypassing it here leaves no dead code (turbofish kept for the same
    # E0282 reason as above).
    ('cron-inline-sync', CRON,
     '            let dispatched = tokio::task::spawn_blocking(move || {\n'
     '                dispatch_under_management_gate(&cron, &dispatch_method, &params)\n'
     '            })\n'
     '            .await;',
     '            let dispatched = Ok::<_, tokio::task::JoinError>(cron::dispatch(cron.as_deref(), &dispatch_method, &params));',
     'native_process::methods::cron::cron_tests::cron_request_dispatches_off_the_async_worker',
     '#1509 cron dispatch must yield the async worker before answering'),
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
    (SESSION_DIR, '#2272 a null projectKey falls back like a missing one, '
                  'so the sessionKey gate answers the blank key'),
    (SESSION_DIR, '#2564 a null projectKey keeps the domain message'),
    (SESSION_DIR, '#2564 a null projectKey lists the configured project'),
    (SESSION_DIR, '#2564 a null projectKey generates a session key'),
    (MANAGEMENT_TESTS, '#2562 explicit null is not an error with the recorder on'),
    (LIVE_TESTS, "#1864 the stored mode reaches the live session's next turn"),
    (FACTORY_TESTS,
     '#1860 hydrate holds the transcript fence until the recorder is published'),
    (REVIEW_TESTS, '#1864 the stored mode reaches the host admission context'),
    (REVIEW_TESTS, '#1429 the acquire path restores the generated ask rule'),
    (REVIEW_TESTS, '#2146 the second acquire reports nothing: one miss report per assembly'),
    (CRON_TESTS, '#1509 cron dispatch must yield the async worker before answering'),
]

# Where an assertion can live: the mutated sources, the read-side files the
# #1864 rows assert on, and the test files those rows bind to (the #1860 row
# mutates `factory.rs` but asserts on a message in `factory_tests.rs`). A pin may
# not name a path outside this tuple (checked below) -- a pin the completeness
# scan does not read would be a guard with a blind spot.
SOURCE_TEXTS = (CONFIG, SESSION_DIR, METHODS_MOD, ASSEMBLY, LIVE, TOOLS, LIVE_TESTS,
                REVIEW_TESTS, FACTORY, FACTORY_TESTS, REQUESTS, SERVICES,
                MANAGEMENT_TESTS, CRON, CRON_TESTS, TRANSCRIPT_LOCKS)


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
