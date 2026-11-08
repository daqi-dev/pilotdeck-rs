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
    # bound test's chain assertion must catch it.
    ('transcript-live-bypass-config', CONFIG,
     '            if let Some((owner, recorder)) = super::super::assembly::live_recorder(&recorders, &key)\n'
     '            {',
     '            if let Some((owner, recorder)) = None::<(JsString, Arc<crate::storage::recorder::Recorder>)>\n'
     '            {',
     CFG + 'model_set_appends_through_the_loaded_writer',
     '#1457 the transcript still loads'),
    ('transcript-live-bypass-rename', SESSION_DIR,
     '            if let Some((_owner, recorder)) = live {',
     '            if let Some((_owner, recorder)) = None::<(JsString, Arc<crate::storage::recorder::Recorder>)> {',
     DIR + 'rename_appends_through_the_loaded_writer',
     '#1457 the transcript still loads'),
    # ── fix-r3: the status live branch and the cold writer lease.
    ('transcript-live-bypass-status', SESSION_DIR,
     '        if let Some((_owner, recorder)) = live_recorder {',
     '        if let Some((_owner, recorder)) =\n'
     '            None::<(JsString, Arc<crate::storage::recorder::Recorder>)>\n'
     '        {',
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
    ('transcript-cold-lease-skipped', SESSION_DIR,
     '            let Some(_lease) = transcript_locks::cold_write_lease(&storage.transcript_path) else {\n'
     '                // #1457 fix-r3: a session claimed the transcript after the',
     '            let Some(_lease) = Some::<()>(()) else {\n'
     '                // #1457 fix-r3: a session claimed the transcript after the',
     DIR + 'cold_metadata_write_reports_busy_while_a_session_holds_the_lease',
     '#1457 a cold append must not run beside a session that owns the writer'),
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
# The table pins every anchor that is verbatim in the scanned files: 37 of the
# 43 rows (32 distinct strings, 33 entries -- `the transcript still loads` is
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
    (SESSION_DIR, '#1455 resume_session rejects the blank key'),
    (SESSION_DIR, '#1455 resume_session accepts the surrogate key'),
    (SESSION_DIR, '#2083 the surrogate projectKey reaches the capability gate'),
    (SESSION_DIR, '#2272 a null projectKey falls back like a missing one, '
                  'so the sessionKey gate answers the blank key'),
    (LIVE_TESTS, "#1864 the stored mode reaches the live session's next turn"),
    (FACTORY_TESTS,
     '#1860 hydrate holds the transcript fence until the recorder is published'),
    (REVIEW_TESTS, '#1864 the stored mode reaches the host admission context'),
]

# Where an assertion can live: the mutated sources, the read-side files the
# #1864 rows assert on, and the test files those rows bind to (the #1860 row
# mutates `factory.rs` but asserts on a message in `factory_tests.rs`). A pin may
# not name a path outside this tuple (checked below) -- a pin the completeness
# scan does not read would be a guard with a blind spot.
SOURCE_TEXTS = (CONFIG, SESSION_DIR, METHODS_MOD, ASSEMBLY, LIVE, TOOLS, LIVE_TESTS,
                REVIEW_TESTS, FACTORY, FACTORY_TESTS, REQUESTS)


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
