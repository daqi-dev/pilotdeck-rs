#!/usr/bin/env python3
"""#742/#936 guarded product mutations. Commit before running; never bypass HEAD.

--audit is read-only. --compile OUT [names] proves every selected mutant builds.
OUT [names] runs the shared baseline/mutant/restored suite, accounting for all.
The full suite is intentionally a coordinator/host job, not a background task.
"""
import json
import os
from pathlib import Path
import sys

root = Path(__file__).resolve().parents[5]
fixtures = root / 'src/pilotdeck-gateway/tests/fixtures'
sys.path[:0] = [str(fixtures), str(fixtures / 'turn-runner')]
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import Case, run_cases
from _negative_common import mutate

tail = root / 'src/pilotdeck-gateway/src/runtime/turn/runner_tail.rs'
artifacts = root / 'src/pilotdeck-gateway/src/runtime/turn/runner_artifacts.rs'
runner = root / 'src/pilotdeck-gateway/src/runtime/turn/runner.rs'
live = root / 'src/pilotdeck-gateway/src/runtime/turn/live.rs'
prompt = root / 'src/pilotdeck-gateway/src/runtime/turn/runner_prompt.rs'
recording = root / 'src/pilotdeck-gateway/src/runtime/turn/runner_recording.rs'
oracle = Path(__file__).with_name('oracle.json')
unit = 'runtime::turn::runner::tail::tests::'
live_unit = 'runtime::turn::live::tail_tests::'
cases = [
    ('title-after-dispose', tail, 'if self.disposed.is_cancelled()\n            || titled', 'if false\n            || titled', None, unit+'title_start_after_dispose', '#742 title'),
    ('nonregular-file', artifacts, 'if !meta.is_file() {', 'if false {', None, unit+'collector_policy', '#742 collector'),
    ('env-dot-boundary', artifacts, '.is_some_and(|suffix| !suffix.is_empty())', '.is_some_and(|_| true)', None, unit+'collector_policy', '#742 collector'),
    ('root-symlink', artifacts, 'if path == self.cwd {', 'if false {', None, unit+'collector_policy', '#742 collector'),
    ('listing-first-overwrite', tail, '.get("firstPrompt")', '.get("missingFirstPrompt")', None, unit+'listing_two_turns', '#742 tail'),
    ('host-prefix-constant', runner, '["turn_started", "input_accepted", "user_prompt_submitted"]', '["turn_started", "input_accepted", "mutated_user_prompt_submitted"]', None, 'runtime::turn::live::tests::host_lifecycle_set_is_complete', '#936'),
    ('delete-suggestion', tail, 'Ok(Some(suggestion)) if !suggestion.is_empty()', 'Ok(Some(suggestion)) if suggestion.is_empty()', None, unit+'suggestion_on', '#742 tail'),
    ('drop-suggestion-error-diagnostic', tail, '(tail.session.config.debug)(message);', 'let _ = message;', None, unit+'suggestion_failure_injected', '#742 tail'),
    ('suggestion-empty-assistant', tail, 'if response.is_empty() {', 'if !response.is_empty() {', None, unit+'suggestion_on', '#742 tail'),
    ('artifact-omitted', tail, 'if artifacts.is_empty() {', 'if true || artifacts.is_empty() {', None, unit+'files_created', '#742 tail'),
    ('artifact-status', tail, '"incomplete"', '"complete"', None, unit+'files_aborted', '#742 tail'),
    ('artifact-disabled', tail, 'if !tail.session.config.collect_file_artifacts {', 'if false {', None, unit+'files_collection_disabled', '#742 tail'),
    ('listing-disabled', tail, 'if prompt.is_empty() && model.is_none() {', 'if true {', None, unit+'listing_two_turns', '#742 tail'),
    ('listing-utf16-limit', tail, 'prompt.len_utf16().min(1200)', 'prompt.len_utf16().min(1199)', None, unit+'listing_utf16_limit', '#742 tail'),
    ('listing-first', tail, 'patch.insert("firstPrompt".into(), bounded.clone());', 'patch.insert("lostFirstPrompt".into(), bounded.clone());', None, unit+'listing_two_turns', '#742 tail'),
    ('listing-last', tail, 'patch.insert("lastPrompt".into(), bounded);', 'patch.insert("lostLastPrompt".into(), bounded);', None, unit+'listing_two_turns', '#742 tail'),
    ('listing-model', tail, 'patch.insert("modelSelection".into(), model.clone());', 'patch.insert("lostModelSelection".into(), model.clone());', None, unit+'listing_empty_model', '#742 tail'),
    ('snapshot-marker', tail, 'snapshot.insert("isSnapshot".into(), JsValue::Bool(true));', 'snapshot.insert("isSnapshot".into(), JsValue::Bool(false));', None, unit+'files_none', '#742 tail'),
    ('snapshot-mutates-memory', tail, 'if !snapshot {', 'if true {', None, unit+'files_none', '#742 tail'),
    ('title-disabled', tail, 'if !self.config.auto_generate_title || text.is_empty()', 'if true || text.is_empty()', None, unit+'title_before', '#742 tail'),
    ('title-aborted-write', tail, '!pending.signal.is_cancelled()', 'true', None, unit+'title_abort_ignored', '#742 tail'),
    ('title-provenance', tail, 'fields.insert("titleProviderId".into(), JsValue::String(id));', 'fields.insert("lostTitleProviderId".into(), JsValue::String(id));', None, unit+'title_before', '#742 tail'),
    ('title-sequence', tail, 'JsValue::Array(vec![JsValue::Number(sequence as f64)])', 'JsValue::Array(vec![JsValue::Number(0.0)])', None, unit+'title_before', '#742 tail'),
    ('title-locked-port', tail, 'drop(state);', 'let _state = state;', None, unit+'title_port_reentrance_and_detach_race', '#742 title'),
    ('title-detach-race', tail, 'if pending.linked.swap(false,Ordering::SeqCst) { pending.signal.cancel(None); }', 'pending.signal.cancel(None); pending.linked.store(false,Ordering::SeqCst);', None, unit+'title_port_reentrance_and_detach_race', '#742 title'),
    ('metadata-owner', tail, 'Some((self.lease.clone(), self.owner.clone()))', 'None', None, unit+'metadata_write_retains_owner_and_serial_lock', '#742 metadata'),
    ('metadata-serial', tail, 'let _serial = serial;', 'drop(serial);', None, unit+'metadata_write_retains_owner_and_serial_lock', '#742 metadata'),
    ('tool-source', artifacts, '(p.clone(), "tool", None)', '(p.clone(), "workspace_diff", None)', None, unit+'files_explicit_path', '#742 tail'),
    ('tool-observation', artifacts, '!READ_ONLY.contains(&name.to_lowercase().as_str()) && (success || !excluded)', 'false && (success || !excluded)', None, unit+'files_created', '#742 tail'),
    ('same-file-hash', artifacts, 'if before.is_some_and(|b| b.1 == hash) {', 'if false {', None, unit+'collector_policy', '#742 collector'),
    ('overlap-diff', artifacts, 'self.mutation && !self.overlap.load(Ordering::SeqCst)', 'self.mutation', None, unit+'collector_policy', '#742 collector'),
    ('sensitive-file', artifacts, 'if sensitive(&name) {', 'if false {', None, unit+'collector_policy', '#742 collector'),
    ('hard-internal', artifacts, '|| hard_internal(relative)', '|| false', None, unit+'collector_policy', '#742 collector'),
    ('allowed-input', artifacts, '!internal || self.allowed.iter().any(|allowed| allowed == path)', '!internal', None, unit+'collector_policy', '#742 collector'),
    ('readonly-tool', artifacts, '"read_file",', '"read_file_mutated",', None, unit+'collector_policy', '#742 collector'),
    ('denied-tool', artifacts, '"permission_denied",', '"permission_denied_mutated",', None, unit+'collector_policy', '#742 collector'),
    ('env-example', artifacts, 'if name == ".env.example" {', 'if false {', None, unit+'collector_policy', '#742 collector'),
    ('known-depth', artifacts, 'if depth > 4 {', 'if depth > 3 {', None, unit+'collector_policy', '#742 collector'),
    ('artifact-size', artifacts, 'JsValue::Number(size as f64)', 'JsValue::Number(0.0)', None, unit+'files_created', '#742 tail'),
    ('artifact-operation', artifacts, 'if before.is_some() {', 'if false {', None, unit+'files_updated', '#742 tail'),
    ('artifact-id', artifacts, '[..24].to_owned()', '[..23].to_owned()', None, unit+'files_created', '#742 tail'),
    ('live-tail-wiring', live, 'runner = runner.with_tail(tail, cancellation.clone(), tail_options);', 'let _ = (tail, tail_options);', None, live_unit+'live_tail_progress_and_drop', '#742 live tail'),
    ('live-tool-observation', live, 'runner.observe_tool_result(event.get("result").unwrap_or(&JsValue::Undefined));', 'let _ = runner;', None, live_unit+'live_tail_progress_and_drop', '#742 live tail'),
    ('empty-blocks', prompt, 'AgentInput::Blocks(blocks) => {', 'AgentInput::Blocks(blocks) => { if blocks.is_empty() { return Err(RecorderError::State("mutated empty blocks".into())); }', None, 'runtime::turn::runner::tests::prompts::blocks_empty', '#936 empty blocks'),
    ('host-prompt-rejection', live, '.any(|kind| event_type.equals_str(kind))', '.any(|kind| *kind != "user_prompt_submitted" && event_type.equals_str(kind))', None, 'runtime::turn::live::tests::host_lifecycle_set_is_complete', '#936'),
    ('regeneration', oracle, '"sourceCommit": "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85"', '"sourceCommit": "0000000000000000000000000000000000000000"', None, unit+'regenerates_identically', '#742 tail fixture regeneration'),
]

def audit():
    results = []
    for name, path, old, new, scope, test, assertion in cases:
        try:
            mutate(path.read_text(), scope, [(old, new)])
            results.append({'mutation': name, 'status': 'CHECKED', 'test': test})
        except Exception as error:
            results.append({'mutation': name, 'status': 'INVALID', 'reason': str(error)})
    print(json.dumps(results, indent=2))
    return int(any(r['status'] != 'CHECKED' for r in results))

if sys.argv[1:] == ['--audit']:
    raise SystemExit(audit())
compile_only = len(sys.argv) > 1 and sys.argv[1] == '--compile'
args = sys.argv[2:] if compile_only else sys.argv[1:]
if not args:
    raise SystemExit('usage: negative_controls.py [--compile] OUT [mutation names]')
env = os.environ.copy()
if not env.get('CARGO_TARGET_DIR') or not env.get('PDRS_BASELINE'):
    raise SystemExit('caller must set CARGO_TARGET_DIR and PDRS_BASELINE')
env.update(CARGO_PROFILE_DEV_DEBUG='line-tables-only', CARGO_PROFILE_TEST_DEBUG='line-tables-only', CARGO_TERM_QUIET='false')
names = set(args[1:])
if names - {c[0] for c in cases}:
    raise SystemExit('unknown mutation')
chosen = [c for c in cases if not names or c[0] in names]
out = Path(args[0]); out.mkdir(parents=True, exist_ok=False)
if compile_only:
    results = []
    try:
        with mutating_sources(root, list(dict.fromkeys(c[1] for c in chosen))) as originals:
            for name, path, old, new, scope, test, assertion in chosen:
                try:
                    path.write_text(mutate(originals[path].decode(), scope, [(old,new)]))
                    slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=900)
                    (out / (name + '.slots.log')).write_text(slot.stdout + slot.stderr)
                    result = run_owned(['cargo','test','--offline','--manifest-path','src/Cargo.toml','-p','pilotdeck-gateway','--lib','--no-run'],cwd=root,env=env,timeout=900)
                    (out / (name + '.log')).write_text(result.stdout + result.stderr)
                    results.append({'mutation':name,'status':'COMPILED' if result.returncode == 0 else 'INVALID'})
                except Exception as error:
                    results.append({'mutation':name,'status':'INVALID','reason':str(error)})
                finally:
                    path.write_bytes(originals[path])
    except Exception as error:
        results.extend({'mutation':c[0],'status':'INVALID','reason':str(error)} for c in chosen if not any(r['mutation']==c[0] for r in results))
    (out / 'summary.json').write_text(json.dumps(results,indent=2)+'\n')
    print(json.dumps(results,indent=2))
    raise SystemExit(int(any(r['status'] != 'COMPILED' for r in results)))
raise SystemExit(run_cases(root,[Case(n,p,o,v,t,a,s) for n,p,o,v,s,t,a in chosen],out,env,mutate,timeout=600,baseline='HEAD'))
