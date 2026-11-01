#!/usr/bin/env python3
"""#1231 guarded native-route mutations; commit first, never bypass HEAD.

NEW_OUTPUT [mutation ...]. --check validates anchors without changing sources.
Every mutant must compile, kill its exact test, and pass after restoration.
"""
from pathlib import Path
import difflib
import os
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import Case, classify, passed, publish_summary

ROOT = Path(__file__).resolve().parents[5]
SOURCE = ROOT / 'src/pilotdeck-gateway/src/native_process/management.rs'
REQUESTS = ROOT / 'src/pilotdeck-gateway/src/native_process/requests.rs'
ASSEMBLY = ROOT / 'src/pilotdeck-gateway/src/native_process/assembly.rs'
REFRESH = ROOT / 'src/pilotdeck-gateway/src/runtime/refresh.rs'
OWNER = ROOT / 'src/pilotdeck-gateway/src/runtime/session/owner.rs'
ORACLE = Path(__file__).resolve().parent / 'oracle.json'
TEST = 'native_process::management_tests::route_refresh_reread_matches_ts'
ADMISSION = 'native_process::management_tests::admission_and_unwired_families_stay_closed'
HOST = 'native_process::management_tests::production_host_management_and_restart'
mutations = [
    ('omit-snapshot', SOURCE, 'if method.starts_with("snapshot_") {', 'if method.starts_with("unwired_snapshot_") {', TEST, '#1231 routed first result'),
    ('omit-manager', SOURCE, 'if matches!(method, "manager_sessions" | "manager_browsers") {', 'if matches!(method, "unwired_manager_sessions" | "manager_browsers") {', TEST, '#1231 routed first result'),
    ('omit-commands', SOURCE, 'return Some(self.commands(params).await);', 'return Some(Err(unavailable()));', TEST, '#1231 routed first result'),
    ('omit-skills', SOURCE, 'let skill = SkillMethod::from_wire(method).ok_or_else(unavailable)?;', 'let skill = SkillMethod::from_wire("unwired_skills").ok_or_else(unavailable)?;', TEST, '#1231 routed first result'),
    ('dirty-not-updated', OWNER, 'if record.context.project_key.as_ref() == Some(project) {\n                record.dirty = !reason.is_empty();', 'if record.context.project_key.as_ref() == Some(project) {\n                record.dirty = reason.is_empty();', TEST, '#1231 dirty flag recreates cached session'),
    ('limit-fraction-truncated', SOURCE, 'n.fract() == 0.0 && (1.0..=100.0).contains(n)', '(1.0..=100.0).contains(n)', TEST, '#1231 method wire result'),
    ('missing-project-defaulted', SOURCE, 'validate_command_project(params)?;', 'if false { validate_command_project(params)?; }', TEST, '#1231 method wire result'),
    ('host-boundary-leaked', SOURCE, 'if e.requires_host {\n        return unavailable();\n    }', 'if false && e.requires_host {\n        return unavailable();\n    }', ADMISSION, '#1231 host-dependent skill rejected'),
    ('surrogate-target-accepted', SOURCE, '!s.to_utf8().is_ok_and(|p| self.options.serves_project(&p))', 's.to_utf8().is_ok_and(|p| !self.options.serves_project(&p))', ADMISSION, '#1231 surrogate restore target admission'),
    ('capability-omitted', REQUESTS, '"skill_scan",', '"unwired_skill_scan",', HOST, '#1231 wired method declared'),
    ('capability-snapshot_list', REQUESTS, '"snapshot_list",', '"unwired_snapshot_list",', HOST, '#1231 wired method declared'),
    ('capability-snapshot_get', REQUESTS, '"snapshot_get",', '"unwired_snapshot_get",', HOST, '#1231 wired method declared'),
    ('capability-snapshot_restore', REQUESTS, '"snapshot_restore",', '"unwired_snapshot_restore",', HOST, '#1231 wired method declared'),
    ('capability-manager_sessions', REQUESTS, '"manager_sessions",', '"unwired_manager_sessions",', HOST, '#1231 wired method declared'),
    ('capability-commands_list', REQUESTS, '"commands_list",', '"unwired_commands_list",', HOST, '#1231 wired method declared'),
    ('capability-skill_list', REQUESTS, '"skill_list",', '"unwired_skill_list",', HOST, '#1231 wired method declared'),
    ('capability-skill_read', REQUESTS, '"skill_read",', '"unwired_skill_read",', HOST, '#1231 wired method declared'),
    ('capability-skill_write', REQUESTS, '"skill_write",', '"unwired_skill_write",', HOST, '#1231 wired method declared'),
    ('capability-skill_create', REQUESTS, '"skill_create",', '"unwired_skill_create",', HOST, '#1231 wired method declared'),
    ('capability-skill_delete', REQUESTS, '"skill_delete",', '"unwired_skill_delete",', HOST, '#1231 wired method declared'),
    ('capability-skill_import', REQUESTS, '"skill_import",', '"unwired_skill_import",', HOST, '#1231 wired method declared'),
    ('capability-skill_validate', REQUESTS, '"skill_validate",', '"unwired_skill_validate",', HOST, '#1231 wired method declared'),
    ('skills-restart-refused', ASSEMBLY, 'if management && name == "skills" {', 'if management && name == "unwired_skills" {', HOST, '#1231 native management boot/restart'),
    ('snapshot-storage-omitted', ASSEMBLY, '.entry("PILOTDECK_STORAGE_ROOT".into())', '.entry("UNWIRED_STORAGE_ROOT".into())', HOST, '#1231 production route success'),
    ('oracle-corruption', ORACLE, '"node": "v22.22.0"', '"node": "v0.0.0"', 'native_process::management_tests::regenerates_identically', '#280 native-management/oracle.json differs'),
]
mutations.append(('cursor-array-refused', SOURCE, 'JsValue::Array(items) => encode(items),', 'JsValue::Array(_items) => Err(invalid()),', TEST, '#1231 method wire result'))
mutations.append(('pending-io-blocks-host', SOURCE, '        Some(\n            tokio::task::spawn_blocking(move || {\n                runtime.block_on(requests.management(&method, &params))\n            })\n            .await\n            .map_err(|e| RequestError::new(e.to_string()))\n            .and_then(|result| result),\n        )', '        let _ = runtime; Some(requests.management(&method, &params).await)', 'native_process::management_tests::pending_file_read_does_not_block_serial_host', '#1231 pending file IO must not block host'))

mutations.append(('commands-nested-blocking', SOURCE, 'return Some(self.commands(params).await);', 'let requests = self.clone(); let params = params.clone(); let runtime = tokio::runtime::Handle::current();\n            return Some(tokio::task::spawn_blocking(move || runtime.block_on(requests.commands(&params))).await.map_err(|e| RequestError::new(e.to_string())).and_then(|r| r));', 'native_process::management_tests::management_progress_with_one_available_blocking_thread', '#1231 management progress with one blocking slot'))

mutations.append(('skill-execution-enabled', ASSEMBLY, 'validate_profile_inner(options, env, false).await', 'validate_profile_inner(options, env, true).await', HOST, '#1231 skill management does not enable execution'))

mutations.append(('management-nested-acquire', SOURCE, 'let lease = self\n            .project\n            .acquire(None)\n            .map_err(|e| RequestError::new(e.to_string()))?;', 'let project = self.project.clone();\n        let lease = tokio::task::spawn_blocking(move || project.acquire(None)).await.map_err(|e| RequestError::new(e.to_string()))?.map_err(|e| RequestError::new(e.to_string()))?;', 'native_process::management_tests::management_progress_with_one_available_blocking_thread', '#1231 management progress with one blocking slot'))

mutations.append(('query-surrogate-lossy', SOURCE, 'Some(JsValue::String(text)) => Some(text.clone()),', 'Some(JsValue::String(text)) => Some(String::from_utf16_lossy(text.units()).into()),', TEST, '#1231 method wire result'))

# Move the one notification before invalidate/dirty, preserving payload and count.
mutations.append(('notification-before-dirty', REFRESH, '''        // `if (input?.projectKey)`: an empty key is the all-runtimes branch.
        match input.project_key.filter(|key| !key.is_empty()) {''', '''        broadcast(
            &self.inner,
            &ConfigChangeNotice {
                changed_paths: changed_paths.clone(),
                change_classes: vec!["extension-changed".into()],
            },
        );
        // `if (input?.projectKey)`: an empty key is the all-runtimes branch.
        match input.project_key.filter(|key| !key.is_empty()) {''', TEST, '#1231 invalidate dirty notification order'))


def mutate(case, text):
    if text.count(case.old) != 1 or case.old == case.new:
        raise ValueError(f'#1231 stale anchor: {case.mutation}: {text.count(case.old)}')
    text = text.replace(case.old, case.new, 1)
    if case.mutation == 'notification-before-dirty':
        block = '''        broadcast(
            &self.inner,
            &ConfigChangeNotice {
                changed_paths: changed_paths.clone(),
                change_classes: vec!["extension-changed".into()],
            },
        );'''
        if text.count(block) != 2:
            raise ValueError('#1231 stale trailing notification anchor')
        index = text.rfind(block)
        text = text[:index] + text[index + len(block):]
    return text


def main():
    selected_rows = [Case(*row) for row in mutations]
    if sys.argv[1:] == ['--check']:
        for case in selected_rows:
            mutate(case, case.path.read_text())
        print(f'#1231 {len(selected_rows)} mutation anchors OK')
        return 0
    if len(sys.argv) < 2:
        raise SystemExit('usage: negative_controls.py NEW_OUTPUT [mutation ...]')
    env = dict(os.environ)
    if not env.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
    selected = sys.argv[2:]
    if set(selected) - {case.mutation for case in selected_rows}:
        raise SystemExit('unknown mutation name')
    if selected:
        selected_rows = [case for case in selected_rows if case.mutation in selected]
    if any(case.mutation == 'oracle-corruption' for case in selected_rows) and not env.get('PDRS_BASELINE'):
        raise SystemExit('PDRS_BASELINE required for oracle-corruption')
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    env.update(CARGO_PROFILE_DEV_DEBUG='line-tables-only', CARGO_PROFILE_TEST_DEBUG='line-tables-only', LANG='en_US.UTF-8')
    env.pop('LC_ALL', None)
    env.pop('LC_MESSAGES', None)
    command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml', '-p', 'pilotdeck-gateway', '--lib']
    results = []

    def run(label, args):
        slot = wait_for_slot(ROOT / 'scripts/cargo-slots.sh', timeout=900)
        (out / (label + '.slots.log')).write_text(slot.stdout + slot.stderr)
        result = run_owned(command + args, cwd=ROOT, env=env, timeout=900)
        (out / (label + '.log')).write_text(result.stdout + result.stderr)
        return result

    for case in selected_rows:
        row = dict(mutation=case.mutation, test=case.test, assertion=case.assertion,
                   status='INVALID', reason='', compiled=False)
        args = [case.test, '--', '--exact', '--include-ignored']
        try:
            # Protect the executable fixture and judge, as well as product bytes.
            with mutating_sources(ROOT, [case.path, Path(__file__).resolve(), ORACLE,
                    ORACLE.with_name('generate.mts'), ORACLE.with_name('session.jsonl'),
                    ROOT / 'src/pilotdeck-gateway/src/native_process/management_tests.rs']) as originals:
                original = originals[case.path].decode()
                changed = mutate(case, original)
                if not passed(run(case.mutation + '-positive', args), case.test):
                    raise RuntimeError('positive control failed')
                (out / (case.mutation + '.patch')).write_text(''.join(difflib.unified_diff(
                    original.splitlines(True), changed.splitlines(True), fromfile=str(case.path), tofile=str(case.path))))
                case.path.write_text(changed)
                compiled = run(case.mutation + '-compile', ['--no-run'])
                row['compiled'] = compiled.returncode == 0
                if not row['compiled']:
                    raise RuntimeError('mutant did not compile')
                row['status'], row['reason'] = classify(run(case.mutation + '-negative', args), case)
            if not passed(run(case.mutation + '-restored', args), case.test):
                raise RuntimeError('restored positive control failed')
        except Exception as error:
            row.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
        results.append(row)
        print(f"{case.mutation}: {row['status']}: {row['reason']}", flush=True)
    publish_summary(out, results)
    return int(not results or any(row['status'] != 'KILLED' for row in results))


if __name__ == '__main__':
    raise SystemExit(main())
