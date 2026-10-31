#!/usr/bin/env python3
"""#1154 proxy-path controls: --check; --compile-only OUT; OUT [names]."""
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
# matrix_main uses these shared guarded primitives for every mutation.
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import matrix_main

ROOT = Path(__file__).resolve().parents[5]
PAIRING = ROOT / 'src/pilotdeck-gateway/src/pairing.rs'
PAIRING_TESTS = ROOT / 'src/pilotdeck-gateway/src/pairing/tests.rs'
DISPATCH = ROOT / 'src/pilotdeck-gateway/src/dispatch.rs'
NATIVE_ARCHIVE = ROOT / 'src/pilotdeck-gateway/src/native_archive.rs'
PREFIX = 'pairing::tests::'
TEST = 'archive_nullish_project_key_uses_the_proxy_boot_fallback'
MUTATIONS = [
    ('proxy-fill-skipped', PAIRING,
     'if let Some(metadata) = metadata {\n'
     '                            crate::native_archive::fill_request_default_project_key(\n'
     '                                &mut request,\n'
     '                                &metadata.home,\n'
     '                            );\n'
     '                        }',
     'if let Some(metadata) = metadata && false {\n'
     '                            crate::native_archive::fill_request_default_project_key(\n'
     '                                &mut request,\n'
     '                                &metadata.home,\n'
     '                            );\n'
     '                        }',
     TEST, '#1154 proxy archive nullish key uses the boot fallback project'),
    ('proxy-fallback-process-cwd', PAIRING,
     '&metadata.home,', '&std::env::current_dir().unwrap_or_default(),',
     TEST, '#1154 proxy archive nullish key uses the boot fallback project'),
    ('proxy-null-not-defaulted', NATIVE_ARCHIVE,
     'None | Some(JsValue::Undefined | JsValue::Null)', 'None | Some(JsValue::Undefined)',
     TEST, '#1154 proxy archive nullish key uses the boot fallback project'),
    ('proxy-missing-not-defaulted', NATIVE_ARCHIVE,
     'None | Some(JsValue::Undefined | JsValue::Null)', 'Some(JsValue::Undefined | JsValue::Null)',
     TEST, '#1154 proxy archive nullish key uses the boot fallback project'),
    ('proxy-empty-string-defaulted', NATIVE_ARCHIVE,
     'None | Some(JsValue::Undefined | JsValue::Null)',
     'None | Some(JsValue::Undefined | JsValue::Null | JsValue::String(_))',
     TEST, '#1154 proxy archive nullish key uses the boot fallback project'),
]

if __name__ == '__main__':
    # fix-r3 (review P1-2): the bound test itself (pairing/tests.rs) and the
    # local-admission gate (dispatch.rs) are judges and stay guarded.
    raise SystemExit(matrix_main(ROOT, __file__, PREFIX, MUTATIONS,
        passed_count=1, child_count=0, exact=True,
        paths=[PAIRING, PAIRING_TESTS, DISPATCH, NATIVE_ARCHIVE]))
