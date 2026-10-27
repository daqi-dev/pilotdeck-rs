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

ROOT = Path(__file__).resolve().parents[5]
META = ROOT / 'src/pilotdeck-gateway/src/metadata.rs'
CONFIG = ROOT / 'src/pilotdeck-gateway/src/config/mod.rs'
STORE = ROOT / 'src/pilotdeck-gateway/src/config/store.rs'
PROTO = ROOT / 'src/pilotdeck-gateway/src/runtime/mcp/protocol.rs'
REG = ROOT / 'src/pilotdeck-gateway/src/runtime/mcp/registry.rs'
BRIDGE = ROOT / 'src/pilotdeck-gateway/src/runtime/mcp/bridge.rs'
FIXTURE = ROOT / 'src/pilotdeck-gateway/tests/fixtures/metadata-process-cwd'
PREFIX = 'metadata::process_cwd_tests::'
ANCHOR_COUNTS = {'config-paths-swallow': 2, 'loader-wrong-error-variant': 2}
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
    ('config-paths-swallow', CONFIG,
     'cwd: absolute_cwd(cwd)?,',
     'cwd: absolute_cwd(cwd).unwrap_or_else(|_| std::path::PathBuf::from("/")),',
     'relative_deleted', '#879 config must propagate cwd failure'),
    ('config-error-shape', CONFIG,
     'name: "Error",\n            requires_host: false,',
     'name: "PilotConfigError",\n            requires_host: false,',
     'relative_deleted', '#879 config error shape:'),
    ('watch-path-swallow', STORE,
     'let cwd = super::absolute_cwd(&options.cwd)?;',
     'let cwd = super::absolute_cwd(&options.cwd).unwrap_or_else(|_| PathBuf::from("/"));',
     'relative_deleted', '#879 config must propagate cwd failure'),
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
     'relative_deleted', '#879 loader must propagate cwd failure'),
    ('loader-wrong-error-variant', PROTO,
     '.map_err(McpConfigLoadError::Cwd)?',
     '.map_err(|_| McpConfigLoadError::CallStackExceeded(CallStackExceeded))?',
     'relative_deleted', '#879 loader cwd variant'),
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
     '            images.push(content_block(vec![("type", "text".into()), ("text", error.to_string().into())]));\n'
     '            continue;\n        }};',
     'relative_deleted', '#879 bridge outcome: relative-deleted'),
]


def write_new(path, value):
    with path.open('x') as stream:
        stream.write(value)


def run(out, label, test, env):
    slots = wait_for_slot(ROOT / 'scripts/cargo-slots.sh', timeout=600, interval=15)
    write_new(out / f'{label}.slots.log', slots.stdout + slots.stderr)
    command = ['cargo', 'test', '-q', '-p', 'pilotdeck-gateway', '--lib', PREFIX + test,
               '--', '--exact', '--nocapture', '--format=pretty', '--test-threads=1']
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
    env = os.environ.copy()
    if not env.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
    if len(sys.argv) < 2:
        raise SystemExit('usage: negative_controls.py NEW_OUTPUT_DIRECTORY [mutation ...]')
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    names = sys.argv[2:]
    selected = [row for row in MUTATIONS if not names or row[0] in names]
    if not selected or (names and len(selected) != len(names)):
        raise SystemExit('unknown or duplicate mutation name')
    guarded = [META, CONFIG, STORE, PROTO, REG, BRIDGE, FIXTURE / 'regression.rs',
               FIXTURE / 'oracle.json', FIXTURE / 'generate.mts', Path(__file__).resolve()]
    results = []
    for name, source, before, after, test, marker in selected:
        item = {'name': name, 'test': PREFIX + test, 'status': 'INVALID', 'restoredPass': False}
        try:
            with mutating_sources(ROOT, guarded) as originals:
                original = originals[source].decode()
                expected = ANCHOR_COUNTS.get(name, 1)
                actual = original.count(before)
                if actual != expected:
                    raise RuntimeError(f'stale anchor: expected {expected}, actual {actual}')
                result, log = run(out, f'{name}-positive', test, env)
                positive(result, log, test)
                changed = original.replace(before, after)
                write_new(out / f'{name}.patch', ''.join(difflib.unified_diff(
                    original.splitlines(True), changed.splitlines(True),
                    fromfile=str(source.relative_to(ROOT)), tofile=str(source.relative_to(ROOT)))))
                source.write_text(changed)
                result, log = run(out, f'{name}-negative', test, env)
                if (result.returncode == 101 and 'panicked at' in log and marker in log
                        and PREFIX + test in log
                        and 'test result: FAILED. 0 passed; 1 failed; 0 ignored;' in log):
                    item.update(status='KILLED', reason=f'named runtime assertion: {marker}')
                elif result.returncode == 0:
                    positive(result, log, test)
                    item.update(status='SURVIVED', reason='mutated named test passed')
                else:
                    item['reason'] = 'compile/setup/timeout/wrong-assertion failures are not kills'
            with mutating_sources(ROOT, guarded):
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
              for status in ['KILLED', 'SURVIVED', 'INVALID']}
    print(json.dumps(counts), flush=True)
    return 0 if all(row['status'] == 'KILLED' and row['restoredPass'] for row in results) else 1


if __name__ == '__main__':
    sys.exit(main())
