#!/usr/bin/env python3
"""#946 HEAD-only nonce receipt controls; account for every selected mutant.

--check validates anchors. --compile-only compiles every selected mutant.
Provide a new output directory and optional mutation names; CARGO_TARGET_DIR
must be supplied. Commit first: the source guard is never bypassed.
"""
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
# negative_suite owns mutating_sources, run_owned and wait_for_slot.
from negative_suite import matrix_main

ROOT = Path(__file__).resolve().parents[5]
META = ROOT / 'src/pilotdeck-gateway/src/metadata.rs'
CONFIG = ROOT / 'src/pilotdeck-gateway/src/config/mod.rs'
STORE = ROOT / 'src/pilotdeck-gateway/src/config/store.rs'
PROTO = ROOT / 'src/pilotdeck-gateway/src/runtime/mcp/protocol.rs'
REG = ROOT / 'src/pilotdeck-gateway/src/runtime/mcp/registry.rs'
BRIDGE = ROOT / 'src/pilotdeck-gateway/src/runtime/mcp/bridge.rs'
FIXTURE = ROOT / 'src/pilotdeck-gateway/tests/fixtures/metadata-process-cwd'
PREFIX = 'metadata::process_cwd_tests::'
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
     'relative_deleted', '#879 provider error shape: relative-deleted'),
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



def main():
    return matrix_main(ROOT, __file__, PREFIX, MUTATIONS,
                       passed_count=1, child_count=1,
                       paths=[META, CONFIG, STORE, PROTO, REG, BRIDGE, FIXTURE / "regression.rs", FIXTURE / "oracle.json", FIXTURE / "generate.mts"],
                       any_cell=(), exact=True)


if __name__ == '__main__':
    raise SystemExit(main())
