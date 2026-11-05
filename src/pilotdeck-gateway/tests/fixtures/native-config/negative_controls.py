#!/usr/bin/env python3
"""#1455: --check; [--compile-only] NEW_OUTPUT [mutation ...]. Commit first.

Config-family negative controls: every verification gate of the 7 session
config methods (mode, sessionKey, path, mtime, style name), both remaining
SESSION_BUSY gates, and the CAPABILITY_UNAVAILABLE terminals each have a
mutant bound to a test that fails with a message. Each mutant compiles
before its exact test runs. Every case is reported, then restoration and all
statuses determine the exit code; never bypass HEAD.
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
CFG = 'native_process::methods::config::tests::'
DIR = 'native_process::methods::session_dir::tests::'
mutations = [
    ('unavailable-code', CONFIG, '        code: "CAPABILITY_UNAVAILABLE".into(),', '        code: "INVALID_REQUEST".into(),',
     CFG + 'session_model_set_missing_session_key_hits_capability_gate_first',
     '#1310 session_model_set missing sessionKey hits capability gate first'),
    ('five-method-unavailable-message', CONFIG, 'Err(unavailable(&format!("{method} is unavailable.")))', 'Err(unavailable(&format!("{method} is unavailable on this process.")))',
     CFG + 'session_model_set_missing_session_key_hits_capability_gate_first',
     '#1310 session_model_set unavailable message matches TS shape'),
    ('permission-mode-gate-removed', CONFIG, '                    if !mode_ok {', '                    if !mode_ok && false {',
     CFG + 'set_permission_mode_rejects_invalid_mode',
     '#1310 set_permission_mode invalid mode'),
    ('permission-busy-gate-removed', CONFIG,
     '                    if let Some(ref key) = session_key\n                        && self.owner.has_active_turn(key)\n                    {',
     '                    if let Some(ref key) = session_key\n                        && self.owner.has_active_turn(key)\n                        && false\n                    {',
     CFG + 'set_permission_mode_busy_during_active_turn',
     '#1455 set_permission_mode active turn answers SESSION_BUSY'),
    ('permission-busy-message', CONFIG, '                            "Cannot change permission mode during an active turn.",',
     '                            "Cannot change permission mode while a turn is active.",',
     CFG + 'set_permission_mode_busy_during_active_turn',
     '#1455 set_permission_mode busy message matches the TS shape'),
    ('permission-terminal-message', CONFIG,
     '                        "Permission mode configuration is not installed on this isolated native process.",\n                    ))',
     '                        "Permission mode setting is not installed on this isolated native process.",\n                    ))',
     CFG + 'set_permission_mode_missing_session_key_hits_unavailable',
     '#1455 set_permission_mode terminal message matches the unavailable shape'),
    ('seed-key-gate-removed', CONFIG,
     '                    if !is_trimmed_non_empty(params.get("sessionKey")) {\n                        return Err(error("INVALID_SESSION_KEY", "sessionKey is required."));\n                    }\n                    if !is_trimmed_non_empty(params.get("path")) {',
     '                    if false {\n                        return Err(error("INVALID_SESSION_KEY", "sessionKey is required."));\n                    }\n                    if !is_trimmed_non_empty(params.get("path")) {',
     CFG + 'seed_read_state_requires_session_key',
     '#1455 seed_read_state sessionKey present after the gate'),
    ('seed-path-gate-removed', CONFIG, '                    if !is_trimmed_non_empty(params.get("path")) {', '                    if false {',
     CFG + 'seed_read_state_requires_path',
     '#1310 seed_read_state missing path'),
    ('seed-mtime-boundary', CONFIG, 'n.is_finite() && n.fract() == 0.0 && n >= 0.0', 'n.is_finite() && n.fract() == 0.0 && n > 0.0',
     CFG + 'seed_read_state_unavailable_with_zero_mtime',
     '#1455 seed_read_state zero mtime passes the boundary and reaches unavailable'),
    ('seed-mtime-integer', CONFIG, 'n.fract() == 0.0', 'true',
     CFG + 'seed_read_state_rejects_fractional_mtime',
     '#1310 seed_read_state fractional mtime'),
    ('seed-busy-gate-removed', CONFIG, '                    if self.owner.has_active_turn(&session_key) {',
     '                    if false && self.owner.has_active_turn(&session_key) {',
     CFG + 'seed_read_state_busy_during_active_turn',
     '#1455 seed_read_state active turn answers SESSION_BUSY'),
    ('seed-busy-message', CONFIG, '                            "Cannot seed file read state while a turn is active.",',
     '                            "Cannot seed file read state during an active turn.",',
     CFG + 'seed_read_state_busy_during_active_turn',
     '#1455 seed_read_state busy message matches the TS shape'),
    ('seed-terminal-message', CONFIG,
     '                        "Seed read state is not installed on this isolated native process.",\n                    ))',
     '                        "Seed read state is not installed on this native process.",\n                    ))',
     CFG + 'seed_read_state_unavailable_with_zero_mtime',
     '#1455 seed_read_state terminal message matches the unavailable shape'),
    ('require-session-key-gate-removed', CONFIG,
     'fn require_session_key(params: &JsValue) -> Result<(), RequestError> {\n    if !is_trimmed_non_empty(params.get("sessionKey")) {',
     'fn require_session_key(params: &JsValue) -> Result<(), RequestError> {\n    if false {',
     CFG + 'require_session_key_rejects_missing_key',
     '#1310 require_session_key missing key'),
    ('output-style-name-gate-removed', CONFIG, '            if blank {', '            if blank && false {',
     CFG + 'require_output_style_name_rejects_blank_name',
     '#1310 require_output_style_name blank name'),
    ('shared-trim-dropped', CONFIG,
     '    !crate::metadata::js_trim(&String::from_utf16_lossy(value.units())).is_empty()',
     '    !String::from_utf16_lossy(value.units()).is_empty()',
     CFG + 'require_session_key_rejects_blank_key',
     '#1310 require_session_key blank key'),
    ('session-dir-blank-gate-removed', SESSION_DIR, '        .filter(super::config::trimmed_string_is_non_empty)', '        .filter(|_| true)',
     DIR + 'resume_session_rejects_blank_session_key',
     '#1455 resume_session rejects the blank key'),
    ('session-dir-surrogate-rejected', SESSION_DIR, '        .filter(super::config::trimmed_string_is_non_empty)',
     '        .filter(|s| s.to_utf8().is_ok())',
     DIR + 'resume_session_accepts_lone_surrogate_session_key',
     '#1455 resume_session accepts the surrogate key'),
]


def mutate(case, original):
    count = original.count(case.old)
    if count != 1 or case.old == case.new:
        raise ValueError(f'#1455 {case.mutation}: stale anchor (expected 1, got {count})')
    return original.replace(case.old, case.new, 1)


def main():
    args = sys.argv[1:]
    compile_only = '--compile-only' in args
    args = [arg for arg in args if arg != '--compile-only']
    selected_rows = [Case(*row) for row in mutations]
    if args == ['--check']:
        for case in selected_rows:
            mutate(case, case.path.read_text())
        print(f'#1455 {len(selected_rows)} mutation anchors OK')
        return 0
    if not args:
        raise SystemExit('usage: negative_controls.py --check | [--compile-only] NEW_OUTPUT [mutation ...]')
    env = dict(os.environ)
    if not env.get('CARGO_TARGET_DIR'):
        raise SystemExit('#1455 CARGO_TARGET_DIR must be supplied by the caller')
    for key in ('PDRS_NEGATIVE_RECEIPTS', 'PDRS_NEGATIVE_PARENT_TEST', 'PDRS_MUTANT'):
        env.pop(key, None)
    selected = args[1:]
    if (len(selected) != len(set(selected))
            or set(selected) - {case.mutation for case in selected_rows}):
        raise SystemExit('#1455 unknown/duplicate mutation name')
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
    try:
        paths = list(dict.fromkeys(case.path for case in selected_rows))
        with mutating_sources(ROOT, [*paths, Path(__file__).resolve()]) as originals:
            phase = 'body'
            for case in selected_rows:
                row = invalid(case, '')
                args = [case.test, '--', '--exact']
                try:
                    changed = mutate(case, originals[case.path].decode())
                    if not compile_only and not passed(run(case.mutation + '-positive', args), case.test):
                        raise RuntimeError('#1455 positive control failed')
                    case.path.write_text(changed)
                    compiled = run(case.mutation + '-compile', ['--no-run'])
                    row['compiled'] = compiled.returncode == 0
                    if not row['compiled']:
                        raise RuntimeError('#1455 mutant did not compile')
                    if compile_only:
                        row.update(status='COMPILED', reason='mutant cargo test --no-run passed')
                    else:
                        row['status'], row['reason'] = classify(run(case.mutation + '-negative', args), case)
                except Exception as error:
                    row.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
                finally:
                    phase = 'restore'
                    case.path.write_bytes(originals[case.path])
                    if case.path.read_bytes() != originals[case.path]:
                        raise RuntimeError('#1455 source restoration failed')
                    phase = 'body'
                if not compile_only:
                    try:
                        if not passed(run(case.mutation + '-restored', args), case.test):
                            raise RuntimeError('#1455 restored positive control failed')
                    except Exception as error:
                        row.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
                record(row)
            phase = 'exit'
    except BaseException as error:
        reason = f'source guard: {type(error).__name__}: {error}'
        invalidate_completed(results, reason)
        for case in selected_rows[len(results):]:
            record(invalid(case, reason))
        publish_summary(out, results)
        return 1 if phase == 'enter' and isinstance(error, Exception) else 2
    publish_summary(out, results)
    success = 'COMPILED' if compile_only else 'KILLED'
    return int(not results or any(row['status'] != success for row in results))


if __name__ == '__main__':
    raise SystemExit(main())
