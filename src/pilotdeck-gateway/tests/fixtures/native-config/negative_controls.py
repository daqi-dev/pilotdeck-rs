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
stub-revert alone cannot see that, so the three write sites and both read
sites (the session factory's compose step and the live adapter's per-turn
permission-mode source) each carry their own mutant.
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
# #1864: the read side of the three selectors — the compose step in the
# session factory and the per-turn permission-mode source on the live
# adapter. Both are mutated below so "stored but never read" cannot come back.
ASSEMBLY = ROOT / 'src/pilotdeck-gateway/src/native_process/assembly.rs'
LIVE = ROOT / 'src/pilotdeck-gateway/src/runtime/turn/live.rs'
CFG = 'native_process::methods::config::tests::'
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
     '#1457 set_permission_mode rejects unknown mode'),
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
     '        let write_fence = super::transcript_locks::acquire(project_root, session_key).await;',
     '        let write_fence = ();',
     CFG + 'concurrent_transcript_writers_keep_the_chain_loadable',
     '#1457 transcript sequences strictly increase'),
    ('transcript-fence-removed-rename', SESSION_DIR,
     '        let write_fence =\n'
     '            super::transcript_locks::acquire(&project_root.to_string_lossy(), &key).await;',
     '        let write_fence = ();',
     CFG + 'concurrent_transcript_writers_keep_the_chain_loadable',
     '#1457 transcript sequences strictly increase'),
    # ── fix-r2 P0: a loaded session must be written *through* its recorder.
    # Bypassing the lookup sends the entry to a fresh writer on the same tail;
    # the loaded writer's next append then duplicates `sequence + 1` and the
    # bound test's chain assertion must catch it.
    ('transcript-live-bypass-config', CONFIG,
     '            if let Some((owner, recorder)) =\n'
     '                super::transcript_locks::live_recorder(&recorders, &key)\n'
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
    ('transcript-cold-lease-skipped', SESSION_DIR,
     '            let Some(_lease) = super::transcript_locks::cold_write_lease(&storage.transcript_path)',
     '            let Some(_lease) = Some::<()>(())',
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
    ('session-dir-blank-gate-removed', SESSION_DIR, '        .filter(super::config::trimmed_string_is_non_empty)', '        .filter(|_| true)',
     DIR + 'resume_session_rejects_blank_session_key',
     '#1455 resume_session rejects the blank key'),
    ('session-dir-surrogate-rejected', SESSION_DIR, '        .filter(super::config::trimmed_string_is_non_empty)',
     '        .filter(|s| s.to_utf8().is_ok())',
     DIR + 'resume_session_accepts_lone_surrogate_session_key',
     '#1455 resume_session accepts the surrogate key'),
    # ── #1864: the three selectors now have a reader. Each write and each of
    # the two read paths gets a mutant, so a store that is written but never
    # read (the -R4 defect) turns the bound positive red instead of passing.
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
    ('compose-permission-readback-removed', ASSEMBLY,
     '                let stored_permission_mode =\n'
     '                    session_overrides.permission_mode(&context.session_key);',
     '                let stored_permission_mode = None::<JsString>;',
     CFG + 'selector_writes_reach_the_compose_step',
     '#1864 the stored permission mode reaches the agent config'),
    ('compose-style-merge-removed', ASSEMBLY,
     '    if let Some(content) = style_content {',
     '    if let Some(content) = None::<&str> {',
     CFG + 'apply_session_overrides_composes_the_selections',
     '#1864 the style content joins the existing prompt'),
    ('submit-permission-source-removed', LIVE,
     '            if let Some(source) = &self.permission_mode_source',
     '            if let Some(source) = &None::<Arc<dyn Fn() -> Option<JsString> + Send + Sync>>',
     'runtime::turn::live::stream_tests::permission_mode_source_rewrites_the_agent_config_each_turn',
     "#1864 the stored mode reaches the live session's next turn"),
]


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
    selected_rows = [Case(*row) for row in mutations]
    if args == ['--check']:
        for case in selected_rows:
            mutate(case, case.path.read_text())
        print(f'#1457 {len(selected_rows)} mutation anchors OK')
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
