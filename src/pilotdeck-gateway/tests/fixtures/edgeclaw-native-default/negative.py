#!/usr/bin/env python3
"""#778 compiled controls: compile every switch; run the selected positive,
negative and restored cases with negative_suite verdicts. No source rewriting,
no alternate HEAD and no second target directory. Run one sample in the authoring
session; the coordinator can run the complete list after reviewing the patch.
"""
import argparse
import os
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from _shared import mutant_switch
ROOT = Path(__file__).resolve().parents[5]
FIXTURES = ROOT / 'src/pilotdeck-gateway/tests/fixtures'
SOURCE = ROOT / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/factory.rs'
BUNDLE = ROOT / 'src/pilotdeck-gateway/src/runtime/memory/mod.rs'
SERVICE = ROOT / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/service/mod.rs'
INDEXER = ROOT / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/heartbeat/indexer.rs'
STORAGE = ROOT / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/storage.rs'
FILES = ROOT / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/file_memory.rs'
TESTS = ROOT / 'src/pilotdeck-gateway/tests/edgeclaw_native_default.rs'
PREFIX = 'runtime::memory::edgeclaw::factory::'
switches = [
    ('native-storage-error-prefix', STORAGE, 'tests::heartbeat_batch_numeric_edges', '#778 heartbeat numeric flush batch-edge-2'),
    ('native-indexer-batch-truncated', INDEXER, 'tests::heartbeat_batch_numeric_edges', '#778 heartbeat numeric flush batch-edge-2'),
    ('native-indexer-override-ignored', INDEXER, 'tests::heartbeat_batch_numeric_edges', '#778 heartbeat numeric flush batch-override-fraction'),
    ('native-database-validate-early', STORAGE, 'tests::factory_config_matrix', '#778 database failure directory layout sqlite-corrupt'),
    ('native-opaque-telemetry-rejected', SOURCE, 'tests::memory_native_default', '#778 host telemetry native default stage'),
    ('native-provider-retrieval-mode', SOURCE, 'tests::factory_config_matrix', '#778 factory provider configuration'),
    ('native-heartbeat-fraction-truncated', SOURCE, 'tests::factory_config_matrix', '#778 factory configuration batch-edge-2'),
    ('native-max-message-chars-drop', SOURCE, 'tests::factory_config_matrix', '#778 factory configuration chars-edge-0'),
    ('native-pending-limit-truncated', STORAGE, 'tests::heartbeat_batch_numeric_edges', '#778 heartbeat numeric flush batch-edge-2'),
    ('native-pending-preferred-round-up', STORAGE, 'tests::heartbeat_batch_numeric_edges', '#778 heartbeat numeric selection batch-edge-2 preferred'),
    ('native-storage-skip-initialize', STORAGE, 'tests::storage_invalid_paths', '#778 direct storage rejects corrupt'),
    ('native-storage-directory-accepted', STORAGE, 'tests::storage_invalid_paths', '#778 direct storage rejects directory'),
    ('native-database-open-late', FILES, 'tests::factory_config_matrix', '#778 database failure directory layout sqlite-directory'),
    ('native-missing-m21-cell', TESTS, 'tests::m21_denominator', '#778 Rust M21 registrations exactly match oracle'),
    ('native-default-empty', BUNDLE, 'tests::memory_native_default', '#778 native default must publish real three handles'),
    ('native-root-ignored', SOURCE, 'tests::factory_config_matrix', '#778 factory configuration'),
    ('native-factory-ignore-llm', SOURCE, 'tests::factory_config_matrix', '#778 factory configuration'),
    ('native-heartbeat-batch', SOURCE, 'tests::factory_config_matrix', '#778 factory configuration'),
    ('native-provider-source', SOURCE, 'tests::factory_config_matrix', '#778 factory provider configuration'),
    ('native-maintenance-noop', SOURCE, 'tests::memory_native_end_to_end', '#778 E2E recall hits captured content'),
    ('native-close-noop', SOURCE, 'tests::memory_native_default', '#778 real database closes'),
    ('native-error-code-drop', SERVICE, 'tests::disposed_maintenance', '#778 M21 disposed-maintenance'),
    ('native-get-omitted-maxlines', SERVICE, 'tests::service_get_maxlines_defaults_1052', '#778 #1052 maxLines defaults'),
    ('native-get-undefined-maxlines', SERVICE, 'tests::service_get_maxlines_defaults_1052', '#778 #1052 maxLines defaults'),
    ('get-null-maxlines', SERVICE, 'tests::service_get_maxlines_defaults_1052', '#778 #1052 maxLines defaults'),
]
GUARDED = [SOURCE, BUNDLE, SERVICE, TESTS, STORAGE,
    ROOT / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/provider.rs',
    ROOT / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/file_memory.rs',
    ROOT / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/heartbeat/indexer.rs',
    ROOT / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/llm/index.rs',
    ROOT / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/llm/transport.rs',
    ROOT / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/mod.rs',
    ROOT / 'src/pilotdeck-gateway/tests/common/baseline.rs',
    ROOT / 'src/pilotdeck-gateway/tests/support/llm_stub.rs',
    ROOT / 'src/pilotdeck-gateway/src/mutants.rs',
    ROOT / 'src/pilotdeck-gateway/src/lib.rs',
    ROOT / 'src/pilotdeck-gateway/Cargo.toml', ROOT / 'src/Cargo.toml', ROOT / 'src/Cargo.lock',
    FIXTURES / '_shared/mutant_switch.py', FIXTURES / 'negative_suite.py', FIXTURES / 'negative_process.py',
    Path(__file__), Path(__file__).with_name('oracle.json'),
    Path(__file__).with_name('generate.mts'), Path(__file__).with_name('observe.mts'),
    Path(__file__).with_name('verify.mts'),
]

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check', action='store_true')
    parser.add_argument('output', nargs='?')
    parser.add_argument('names', nargs='*')
    args = parser.parse_args()
    try:
        rows = [dict(zip(('name', 'path', 'test', 'anchor'), row)) for row in switches]
        chosen = mutant_switch.selected(rows, args.names)
        mutant_switch.preflight(rows)
        if args.check:
            if args.output or args.names:
                raise ValueError('--check does not take output or names')
            print(f'{len(rows)} switch anchors and bindings checked')
            return 0
        if not args.output or not os.environ.get('CARGO_TARGET_DIR'):
            raise ValueError('NEW_OUTPUT_DIR and caller CARGO_TARGET_DIR are required')
        output = Path(args.output).resolve()
        output.mkdir(parents=True, exist_ok=False)
        return mutant_switch.run(ROOT, output, chosen,
            mutant_switch.clean_environment(os.environ), guarded=GUARDED,
            prefix=PREFIX, script=Path(__file__), requested=args.names)
    except (ValueError, OSError) as error:
        print(f'preflight: {error}', file=sys.stderr)
        return 2

if __name__ == '__main__':
    sys.exit(main())
