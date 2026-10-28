#!/usr/bin/env python3
"""#994 scratch mutations with persistent target and separate build/run clocks."""
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from _shared.negative_control import mutation_suite
from negative_panic import panic_evidence

root = Path(__file__).resolve().parents[5]
background = root / 'src/pilotdeck-gateway/src/runtime/background.rs'
store = root / 'src/pilotdeck-gateway/src/runtime/background/output.rs'
mutations = [
    ('threshold-minus-one', background, 'const DEFAULT_MEMORY_BYTES: usize = 1_000_000;', 'const DEFAULT_MEMORY_BYTES: usize = 999_999;'),
    ('threshold-plus-one', background, 'const DEFAULT_MEMORY_BYTES: usize = 1_000_000;', 'const DEFAULT_MEMORY_BYTES: usize = 1_000_001;'),
    ('read-order', store, 'content = disk;', 'content = disk.into_iter().rev().collect();'),
    ('unconfigured-spill', store, 'spill: path.map(|path| Spill {', 'spill: path.or_else(|| Some(std::path::PathBuf::from(std::env::var_os("PDRS_553_NEGATIVE_SPILL_PATH").unwrap()))).map(|path| Spill {'),
    ('overflow-only-spill', store, 'spill.append(chunk.to_vec());', 'if self.memory + chunk.len() > self.max { spill.append(chunk.to_vec()); }'),
    ('truncate-existing-log', store, '.append(true)', '.write(true).truncate(true)'),
    ('skip-flush', store, 'let _ = tail.wait_for(Option::is_some).await;', 'let _ = tail;'),
    ('retry-disabled', store, 'fs::create_dir_all(path.parent().expect("spill parent"))?;', 'if !path.parent().expect("spill parent").exists() { return Err(std::io::Error::from_raw_os_error(libc::EACCES)); }'),
    ('failed-write-marked-ready', store, 'write().is_ok()', '{ let _ = write(); true }'),
    ('gap-unreported', store, 'truncated = true;', 'truncated = false;'),
    ('runtime-layout', background, 'dir.join("output")', 'dir.join("wrong-output")'),
    ('x-dispose-no-flush-await', background, '                flushed.await;', '                drop(flushed);'),
    ('x-begin-close-not-closing', store, 'self.closed = true;\n        self.flush()', 'self.flush()'),
    ('utf8-disk-decoded-before-ring', store, 'content = disk;', 'content = String::from_utf8_lossy(&disk).into_owned().into_bytes();'),
    ('disk-cap-falls-through', store, 'if remaining <= 0.0 {\n            return slice', 'if remaining < 0.0 {\n            return slice'),
    ('closed-disk-unreadable', store, 'let path = &self.spill.as_ref()?.path;', 'if self.closed { return None; } let path = &self.spill.as_ref()?.path;'),
    ('ring-leading-replacement-lost', store, 'String::from_utf8_lossy(&content).into_owned()', "if truncated { String::from_utf8_lossy(&content).trim_start_matches('\\u{fffd}').to_owned() } else { String::from_utf8_lossy(&content).into_owned() }"),
    ('closed-accepts-output', store, 'if self.closed || chunk.is_empty() {', 'if chunk.is_empty() {'),
]


def selection(name):
    target = ('--lib',)
    test = 'runtime::background::output::oracle_tests::spill_replays_real_ts_oracle'
    if name == 'runtime-layout':
        target = ('--test', 'background_output')
        test = 'configured_runtime_preserves_complete_ts_log_after_dispose'
    elif name == 'x-dispose-no-flush-await':
        test = 'runtime::background::output::dispose_tests::dispose_without_snapshot_waits_for_gated_write'
    elif name == 'x-dispose-no-flush-await-cancelled':
        test = 'runtime::background::output::dispose_tests::dispose_without_snapshot_waits_for_cancelled_write'
    elif name in ('x-begin-close-not-closing', 'closed-accepts-output'):
        test = 'runtime::background::output::tests::begin_close_rejects_append_before_ring_is_released'
    elif name in ('utf8-disk-decoded-before-ring', 'disk-cap-falls-through',
                  'closed-disk-unreadable', 'ring-leading-replacement-lost'):
        test = 'runtime::background::output::oracle_tests::spill_replays_added_ts_boundary_cases'
    return target, [test, '--exact', '--show-output', '--test-threads=1']


def passed(result, name):
    test = selection(name)[1][0]
    return (result.returncode == 0 and f'test {test} ... ok' in result.stdout
            and 'test result: ok. 1 passed; 0 failed;' in result.stdout)


def classify(name, result, test):
    bound = test or name
    if passed(result, bound):
        return 'SURVIVED', 'bound test still passed'
    log = result.stdout + result.stderr
    test = selection(bound)[1][0]
    marker = {
        'runtime-layout': '#994 background-output expected runtime log',
        'x-dispose-no-flush-await': '#994 dispose-flush must await gated write without snapshot store',
        'x-begin-close-not-closing': 'begin_close must freeze accepted output #994 background-output assertion',
        'closed-accepts-output': 'begin_close must freeze accepted output #994 background-output assertion',
    }.get(name, '#994 background-output oracle comparison')
    evidence = panic_evidence(log, test)
    if (result.returncode == 101 and f'test {test} ... FAILED' in log
            and 'test result: FAILED. 0 passed; 1 failed;' in log
            and marker in evidence):
        return 'KILLED', 'bound #994 assertion failed'
    return 'INVALID', 'missing bound runtime assertion failure'


def main():
    if len(sys.argv) < 2:
        raise SystemExit('usage: negative.py NEW_OUTPUT_DIR [mutation ...]')
    requested = sys.argv[2:]
    selected = [row for row in mutations if not requested or row[0] in requested]
    if requested and (len(selected) != len(requested) or len(set(requested)) != len(requested)):
        raise SystemExit('unknown/duplicate mutation')
    rows = [(*row, [row[0], row[0] + '-cancelled']
                   if row[0] == 'x-dispose-no-flush-await' else [row[0]]) for row in selected]
    return mutation_suite('background-output', rows, sys.argv[1], selection, passed, classify,
                          guarded=[root / 'src/pilotdeck-gateway/tests/background_output.rs'])


if __name__ == '__main__':
    raise SystemExit(main())
