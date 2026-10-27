#!/usr/bin/env python3
"""#879 HEAD-only controls. Run all selected mutations before reporting failure.

New/uncommitted sources deliberately fail closed; commit first on the host.
"""
import difflib
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import target_hits

ROOT = Path(__file__).resolve().parents[5]
META = ROOT / 'src/pilotdeck-gateway/src/metadata.rs'
CONFIG = ROOT / 'src/pilotdeck-gateway/src/config/mod.rs'
STORE = ROOT / 'src/pilotdeck-gateway/src/config/store.rs'
PROTO = ROOT / 'src/pilotdeck-gateway/src/runtime/mcp/protocol.rs'
REG = ROOT / 'src/pilotdeck-gateway/src/runtime/mcp/registry.rs'
BRIDGE = ROOT / 'src/pilotdeck-gateway/src/runtime/mcp/bridge.rs'
FIXTURE = ROOT / 'src/pilotdeck-gateway/tests/fixtures/metadata-process-cwd'
PREFIX = 'metadata::process_cwd_tests::'
WORKER = 'cwd_worker'  # regression.rs's bounded case worker thread
MUTATIONS = [
    ('root-fallback', META,
     'let process_cwd = std::env::current_dir()\n        .map_err(|error| io::Error::new(error.kind(), ProcessCwdError(error)))?;',
     'let process_cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));',
     'relative_deleted', '#879 resolver outcome: relative-deleted'),
    ('absolute-path-needs-cwd', META,
     'if path.is_absolute() {\n        return Ok(resolve_path(Path::new(""), path));\n    }',
     'if path.is_absolute() {\n        let _ = std::env::current_dir()?;\n        return Ok(resolve_path(Path::new(""), path));\n    }',
     'absolute_path_deleted', '#879 cwd Display matches from_env shape'),
    ('absolute-base-needs-cwd', META,
     'if cwd.is_absolute() {\n        return Ok(resolve_path(cwd, path));\n    }',
     'if cwd.is_absolute() {\n        let _ = std::env::current_dir()?;\n        return Ok(resolve_path(cwd, path));\n    }',
     'absolute_base_deleted', '#879 cwd Display matches from_env shape'),
    ('normal-path-layout', META,
     'Ok(resolve_path(&resolve_path(&process_cwd, cwd), path))',
     'Ok(resolve_path(&resolve_path(&process_cwd, cwd), path).join("mutated"))',
     'relative_normal', '#879 resolver outcome: relative-normal'),
    ('empty-path-layout', META,
     'Ok(resolve_path(&resolve_path(&process_cwd, cwd), path))',
     'Ok(resolve_path(&resolve_path(&process_cwd, cwd), path).join("mutated"))',
     'empty_normal', '#879 resolver outcome: empty-normal'),
    ('cwd-display-syscall', META,
     'diagnostic_or_unknown(&self.0, "uv_cwd", None)',
     'diagnostic_or_unknown(&self.0, "read", None)',
     'relative_deleted', '#879 cwd Display matches from_env shape'),
    ('cwd-errno-lost', META,
     '.map_or(error, |inner| &inner.0);',
     '.map_or(error, |_inner| error);',
     'relative_deleted', '#879 cwd Display matches from_env shape'),
    ('config-load-snapshot-swallow', CONFIG,
     'cwd: absolute_cwd(cwd)?,\n        files: crate::metadata::NativeFiles,\n    };\n    Ok(load_project_with_files_inner(',
     'cwd: absolute_cwd(cwd).unwrap_or_else(|_| std::path::PathBuf::from("/")),\n        files: crate::metadata::NativeFiles,\n    };\n    Ok(load_project_with_files_inner(',
     'relative_deleted', '#879 config must propagate cwd failure: load-snapshot/relative-deleted'),
    ('config-load-project-swallow', CONFIG,
     'cwd: absolute_cwd(cwd)?,\n        files: crate::metadata::NativeFiles,\n    };\n    load_project_with_files(',
     'cwd: absolute_cwd(cwd).unwrap_or_else(|_| std::path::PathBuf::from("/")),\n        files: crate::metadata::NativeFiles,\n    };\n    load_project_with_files(',
     'relative_deleted', '#879 config must propagate cwd failure: load-project/relative-deleted'),
    ('pilot-project-path-swallow', CONFIG,
     'crate::metadata::resolve_process_path(cwd, std::path::Path::new(project))',
     'Ok::<_, std::io::Error>(crate::metadata::resolve_process_path(cwd, std::path::Path::new(project)).unwrap_or_else(|_| std::path::PathBuf::from("/")))',
     'relative_deleted', '#879 pilot-project outcome: relative-deleted'),
    ('config-error-shape', CONFIG,
     'name: "Error",\n        requires_host: false,',
     'name: "PilotConfigError",\n        requires_host: false,',
     'relative_deleted', '#879 pilot-project outcome: relative-deleted'),
    ('watch-path-swallow', STORE,
     'let cwd = super::absolute_cwd(&options.cwd)?;',
     'let cwd = super::absolute_cwd(&options.cwd).unwrap_or_else(|_| PathBuf::from("/"));',
     'relative_deleted', '#879 config must propagate cwd failure: watch-paths/relative-deleted'),
    ('global-path-swallow', PROTO,
     'Ok(absolute(pilot_home)?.join(MCP_CONFIG_FILE_NAME))',
     'Ok(absolute(pilot_home).unwrap_or_else(|_| PathBuf::from("/")).join(MCP_CONFIG_FILE_NAME))',
     'relative_deleted', '#879 global outcome: relative-deleted'),
    ('project-path-swallow', PROTO,
     'Ok(absolute(project_root)?',
     'Ok(absolute(project_root).unwrap_or_else(|_| PathBuf::from("/"))',
     'relative_deleted', '#879 project outcome: relative-deleted'),
    ('registry-path-swallow', REG,
     'crate::metadata::resolve_process_path(Path::new(""), Path::new(path))\n        .map(|path| path.to_string_lossy().into_owned())',
     'Ok(crate::metadata::resolve_process_path(Path::new(""), Path::new(path))\n        .unwrap_or_else(|_| std::path::PathBuf::from("/")).to_string_lossy().into_owned())',
     'relative_deleted', '#879 patch outcome: funasr/relative-deleted'),
    ('provider-path-swallow', REG,
     'return Err(cwd_runtime_error(error));',
     'let _ = error; JsObject::new()',
     'relative_deleted', '#879 provider must propagate cwd failure'),
    ('funasr-home-swallow', REG,
     'resolve_path(pilot_home)?.trim_end_matches',
     'resolve_path(pilot_home).unwrap_or_else(|_| "/".into()).trim_end_matches',
     'relative_deleted', '#879 patch outcome: funasr-home/relative-deleted'),
    ('loader-project-swallow', PROTO,
     '&project_mcp_config_file_path(project_root).map_err(McpConfigLoadError::Cwd)?,',
     '&project_mcp_config_file_path(project_root).unwrap_or_else(|_| PathBuf::from("/missing")),',
     'relative_deleted', '#879 loader must propagate cwd failure: load-project/relative-deleted'),
    ('loader-global-wrong-error-variant', PROTO,
     'global_mcp_config_file_path(pilot_home).map_err(McpConfigLoadError::Cwd)?',
     'global_mcp_config_file_path(pilot_home).map_err(|_| McpConfigLoadError::CallStackExceeded(CallStackExceeded))?',
     'relative_deleted', '#879 loader cwd variant: load/relative-deleted'),
    ('loader-project-wrong-error-variant', PROTO,
     'project_mcp_config_file_path(project_root).map_err(McpConfigLoadError::Cwd)?',
     'project_mcp_config_file_path(project_root).map_err(|_| McpConfigLoadError::CallStackExceeded(CallStackExceeded))?',
     'relative_deleted', '#879 loader cwd variant: load-project/relative-deleted'),
    ('browser-skips-cwd', REG,
     'let _ = resolve_path("")?;',
     'let _ = ();',
     'absolute_base_deleted', '#879 patch outcome: browser-use/absolute-base-deleted'),
    ('provider-error-shape', REG,
     'name: "Error".into(),\n        code: Some(code.into()),',
     'name: "RuntimeError".into(),\n        code: Some(code.into()),',
     'relative_deleted', '#879 provider error shape:'),
    ('remote-spec-needs-cwd', REG,
     'else {\n        return Ok(spec);\n    };\n    let replacements:',
     'else {\n        let _ = resolve_path(project_root)?;\n        return Ok(spec);\n    };\n    let replacements:',
     'relative_deleted', '#879 remote spec must not need cwd'),
    ('ordinary-spec-needs-cwd', REG,
     '_ => return Ok(spec),',
     '_ => { let _ = resolve_path(project_root)?; return Ok(spec); },',
     'relative_deleted', '#879 patch outcome: ordinary/relative-deleted'),
    ('bridge-drop-images', BRIDGE,
     'let Some(bytes) = policy.read(&path) else {',
     'let Some(bytes) = policy.read(&path).filter(|_| false) else {',
     'relative_normal', '#879 bridge outcome: relative-normal'),
    ('bridge-surfaces-resolution-error', BRIDGE,
     'let Ok(path) = resolve_path(cwd, relative) else {\n            continue;\n        };',
     'let path = match resolve_path(cwd, relative) { Ok(path) => path, Err(error) => {\n'
     '            images.push(content_block(vec![("type", "text".into()), ("text", JsValue::String(error.to_string().into()))]));\n'
     '            continue;\n        }};',
     'relative_deleted', '#879 bridge outcome: relative-deleted'),
]


def write_new(path, value):
    with path.open('x') as stream:
        stream.write(value)


def run(out, label, test, env, *, compile_only=False):
    slots = wait_for_slot(ROOT / 'scripts/cargo-slots.sh', timeout=600, interval=15)
    write_new(out / f'{label}.slots.log', slots.stdout + slots.stderr)
    command = ['cargo', 'test', '--offline', '-q', '-p', 'pilotdeck-gateway', '--lib', PREFIX + test,
               '--', '--exact', '--nocapture', '--format=pretty', '--test-threads=1']
    if compile_only:
        command = ['cargo', 'check', '--offline', '-q', '-p', 'pilotdeck-gateway', '--tests']
    write_new(out / f'{label}.command.json', json.dumps({
        'command': command, 'cwd': str(ROOT / 'src'),
        'CARGO_TARGET_DIR': env['CARGO_TARGET_DIR']}, indent=2) + '\n')
    result = run_owned(command, cwd=ROOT / 'src', env=env, timeout=900)
    log = result.stdout + result.stderr
    write_new(out / f'{label}.log', log)
    return result, log


def positive(result, log, test):
    if not (result.returncode == 0
            and 'test result: ok. 1 passed; 0 failed; 0 ignored;' in log
            and log.count(f'PDRS_VERIFIED_CHILD {test.replace("_", "-")}') == 1):
        raise RuntimeError('#879 positive/restored run must pass exactly one named test and receipt')


def main():
    if sys.argv[1:] == ['--check']:
        failures = []
        for name, source, before, _after, _test, _marker in MUTATIONS:
            count = source.read_text().count(before)
            if count != 1:
                failures.append(f'{name}: expected 1 anchor, actual {count}')
        print('\n'.join(failures) if failures else f'#879 anchors: {len(MUTATIONS)} unique sites OK')
        return int(bool(failures))
    compile_only = '--compile-only' in sys.argv[1:]
    args = [arg for arg in sys.argv[1:] if arg != '--compile-only']
    env = os.environ.copy()
    if not env.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
    if not args:
        raise SystemExit('usage: negative_controls.py --check | [--compile-only] NEW_OUTPUT_DIRECTORY [mutation ...]')
    out = Path(args[0]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    names = args[1:]
    selected = [row for row in MUTATIONS if not names or row[0] in names]
    if not selected or (names and len(selected) != len(names)):
        raise SystemExit('unknown or duplicate mutation name')
    # The default is HEAD; NEGATIVE_BASELINE only lets a pre-submit sample
    # point at a temporary `git stash create`/`commit-tree` object without
    # moving HEAD.
    baseline = os.environ.get('NEGATIVE_BASELINE', 'HEAD')
    guarded = [META, CONFIG, STORE, PROTO, REG, BRIDGE, FIXTURE / 'regression.rs',
               FIXTURE / 'oracle.json', FIXTURE / 'generate.mts', Path(__file__).resolve()]
    results = []
    for name, source, before, after, test, marker in selected:
        item = {'name': name, 'test': PREFIX + test, 'status': 'INVALID', 'restoredPass': False}
        try:
            with mutating_sources(ROOT, guarded, baseline=baseline) as originals:
                original = originals[source].decode()
                expected = 1
                actual = original.count(before)
                if actual != expected:
                    raise RuntimeError(f'stale anchor: expected {expected}, actual {actual}')
                if not compile_only:
                    result, log = run(out, f'{name}-positive', test, env)
                    positive(result, log, test)
                changed = original.replace(before, after)
                write_new(out / f'{name}.patch', ''.join(difflib.unified_diff(
                    original.splitlines(True), changed.splitlines(True),
                    fromfile=str(source.relative_to(ROOT)), tofile=str(source.relative_to(ROOT)))))
                source.write_text(changed)
                result, log = run(out, f'{name}-compile', test, env, compile_only=True)
                if result.returncode != 0:
                    raise RuntimeError('mutant did not compile; not a runtime kill')
                item['compiled'] = True
                if compile_only:
                    item.update(status='COMPILED', reason='mutant cargo check --tests passed')
                else:
                    result, log = run(out, f'{name}-negative', test, env)
                    if result.returncode == 0:
                        positive(result, log, test)
                        item.update(status='SURVIVED', reason='mutated named test passed')
                    else:
                        # The shared judgment binds the target to its own
                        # evidence: the outer failures list names it and its own
                        # panic block carries the marker in the embedded
                        # cwd_worker's own panic payload. Neither a plain echo
                        # in worker stdout nor the parent's message ever
                        # counts (#889). The matrix() wrapper re-panics after
                        # catch_unwind without the worker's output, and this
                        # --exact single-test run has no foreign test to echo,
                        # so one own block decides (every_block=False).
                        hit, unrelated = target_hits(
                            result, PREFIX, [(test, marker)], WORKER,
                            every_block=False)
                        if (result.returncode == 101
                                and 'test result: FAILED. 0 passed; 1 failed; 0 ignored;' in log
                                and 'error: could not compile' not in log
                                and not unrelated and hit):
                            item.update(status='KILLED',
                                        reason=f'named runtime assertion: {marker}')
                        else:
                            item['reason'] = 'compile/setup/timeout/wrong-assertion failures are not kills'
            if not compile_only:
                with mutating_sources(ROOT, guarded, baseline=baseline):
                    result, log = run(out, f'{name}-restored', test, env)
                    positive(result, log, test)
                    item['restoredPass'] = True
        except Exception as error:
            item.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
        results.append(item)
        write_new(out / f'{name}.result.json', json.dumps(item, indent=2) + '\n')
        print(f'{name}: {item["status"]}: {item["reason"]}', flush=True)
    write_new(out / 'summary.json', json.dumps(results, indent=2) + '\n')
    counts = {status: sum(row['status'] == status for row in results)
              for status in ['KILLED', 'SURVIVED', 'INVALID', 'COMPILED']}
    print(json.dumps(counts), flush=True)
    success = 'COMPILED' if compile_only else 'KILLED'
    return 0 if all(row['status'] == success and (compile_only or row['restoredPass'])
                    for row in results) else 1


if __name__ == '__main__':
    sys.exit(main())
