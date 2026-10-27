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
MODEL = ROOT / 'src/pilotdeck-gateway/src/model_catalog.rs'
DISPATCH = ROOT / 'src/pilotdeck-gateway/src/dispatch.rs'
FIXTURE = Path(__file__).resolve().parent
PREFIX = 'model_catalog::cwd_tests::'
NORMAL = [f'{kind}_normal_cwd' for kind in
          ['relative', 'empty', 'absolute', 'tilde', 'tilde_child', 'unset']]
RELATIVE = ['relative_deleted_cwd', 'empty_deleted_cwd']
ABSOLUTE = [f'{kind}_deleted_cwd' for kind in ['absolute', 'tilde', 'tilde_child', 'unset']]
NATIVE = 'native metadata matrix:'
CATALOG = 'catalog route matrix:'
SESSION = 'session metadata route matrix:'
BOUNDARY = 'if home_path(env).is_none_or(|home| home.is_absolute()) {'
MUTATIONS = [
    ('cwd-swallowed', MODEL, BOUNDARY, 'if true {', RELATIVE, NATIVE),
    ('absolute-home-error', MODEL, BOUNDARY, 'if home_path(env).is_none() {', ABSOLUTE, NATIVE),
    ('missing-home-error', MODEL, BOUNDARY, 'if false {', ['missing_home_deleted_cwd'], NATIVE),
    ('cwd-message', MODEL, 'diagnostic_or_unknown(&error, "uv_cwd", None)',
     'diagnostic_or_unknown(&error, "read", None)', RELATIVE, NATIVE),
    ('cwd-code', MODEL, 'return Err(config::Error {\n                code: code.into(),',
     'return Err(config::Error {\n                code: "BROKEN".into(),', RELATIVE, NATIVE),
    ('cwd-name', MODEL, 'name: "Error",\n                requires_host: false,',
     'name: "TypeError",\n                requires_host: false,', RELATIVE, NATIVE),
    ('normal-home-layout', MODEL, 'Some(Metadata {\n        home,\n        cwd,',
     'Some(Metadata {\n        home: home.join("mutated"),\n        cwd,', NORMAL, NATIVE),
    ('normal-cwd-layout', MODEL, 'Some(Metadata {\n        home,\n        cwd,',
     'Some(Metadata {\n        home,\n        cwd: cwd.join("mutated"),', NORMAL, NATIVE),
    ('missing-home-default', MODEL, 'let system_home = env.get("HOME")?;',
     'let system_home = env.get("HOME").map(String::as_str).unwrap_or("/unexpected");',
     ['missing_home_normal_cwd'], NATIVE),
    ('catalog-swallow', DISPATCH,
     'Err(error) => metadata_error_response(request, &error),\n    }\n}',
     'Err(_error) => Route::Forward,\n    }\n}', RELATIVE, CATALOG),
    ('session-swallow', DISPATCH,
     'Err(error) => metadata_error_response(request, &error),\n            };',
     'Err(_error) => Route::Forward,\n            };', RELATIVE, SESSION),
    ('catalog-normal-forward', DISPATCH,
     'Ok(Some(metadata)) => crate::model_catalog::response(request, &metadata, env)\n'
     '            .map_or(Route::Forward, Route::LocalResponse),',
     'Ok(Some(_metadata)) => Route::Forward,', NORMAL, CATALOG),
    ('frame-code', DISPATCH, '("code", JsValue::String(error.code.as_str().into())),',
     '("code", "gateway_request_failed".into()),', RELATIVE, CATALOG),
    ('frame-message', DISPATCH, '("message", JsValue::String(error.message.clone())),',
     '("message", "mutated".into()),', RELATIVE, CATALOG),
    ('frame-id', DISPATCH, 'request.get("id").expect("validated request id").clone(),',
     '"mutated-id".into(),', RELATIVE, CATALOG),
]



def main():
    return matrix_main(ROOT, __file__, PREFIX, MUTATIONS,
                       passed_count=14, child_count=14,
                       paths=[MODEL, DISPATCH, FIXTURE / "regression.rs", FIXTURE / "oracle.json", FIXTURE / "config.json", FIXTURE / "generate.mts"],
                       any_cell=(), exact=False)


if __name__ == '__main__':
    raise SystemExit(main())
