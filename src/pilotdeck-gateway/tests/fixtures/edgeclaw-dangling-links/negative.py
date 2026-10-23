#!/usr/bin/env python3
"""#563 negative control: each dangling-link operation needs its own red test.

Three mutants each permit exactly one of read / write / delete through a
root-internal dangling link.  For every mutant the matching rejection test must
fail while the other operation tests stay green; that proves no operation is
only covered by an earlier panic in a shared loop.  Product source is restored
in ``finally`` and the positive suite is rerun.
"""
import hashlib
import json
import os
from pathlib import Path
import re
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import run_owned, wait_for_slot


READ_BEFORE = '''            let Some(mut e) = self.entry(id)? else {
                continue;
            };
'''
READ_AFTER = '''            let Ok(Some(mut e)) = self.entry(id) else {
                continue;
            };
'''

WRITE_ENTRY_BEFORE = '            let old = self.entry(path)?.unwrap_or_default();\n'
WRITE_ENTRY_AFTER = '            let old = self.entry(path).ok().flatten().unwrap_or_default();\n'
WRITE_PATH_BEFORE = '        let absolute = self.path(path)?;\n'
WRITE_PATH_AFTER = '''        let absolute = match self.path(path) {
            Ok(path) => path,
            Err(_) => absolute(&self.root.join(path))?,
        };
'''

DELETE_BEFORE = '''            "deleteEntries" => {
                let mut changed = vec![];
                for path in array(input, "relativePaths") {
                    if self.within(&path)? && self.path(&path)?.exists() {
                        let absolute = self.path(&path)?;
                        fs::remove_file(&absolute)
                            .map_err(|e| crate::node_io::path_error(e, "unlink", &absolute))?;
                        changed.push(path);
                    }
                }
                self.repair()?;
'''
DELETE_AFTER = '''            "deleteEntries" => {
                let mut changed = vec![];
                for path in array(input, "relativePaths") {
                    let absolute = absolute(&self.root.join(&path))?;
                    if absolute.exists() {
                        fs::remove_file(&absolute)
                            .map_err(|e| crate::node_io::path_error(e, "unlink", &absolute))?;
                        changed.push(path);
                    }
                }
                let _ = self.repair();
'''

MUTANTS = [
    dict(
        name='allow-read-dangling',
        scenario='read',
        test='dangling_link_read_is_rejected_before_any_io',
        replacements=[(READ_BEFORE, READ_AFTER)],
    ),
    dict(
        name='allow-write-dangling',
        scenario='write',
        test='dangling_link_write_is_rejected_before_any_io',
        replacements=[
            (WRITE_ENTRY_BEFORE, WRITE_ENTRY_AFTER),
            (WRITE_PATH_BEFORE, WRITE_PATH_AFTER),
        ],
    ),
    dict(
        name='allow-delete-dangling',
        scenario='delete',
        test='dangling_link_delete_is_rejected_before_any_io',
        replacements=[(DELETE_BEFORE, DELETE_AFTER)],
    ),
]
OPERATION_TESTS = [
    'dangling_link_read_is_rejected_before_any_io',
    'dangling_link_write_is_rejected_before_any_io',
    'dangling_link_delete_is_rejected_before_any_io',
    'dangling_link_edit_is_rejected_before_any_io',
]
STATUS = re.compile(r'^test (\S+) \.\.\. (ok|FAILED|ignored)$', re.MULTILINE)
TESTS = re.compile(r'^test result: (ok|FAILED)\.')


def main():
    root = Path(__file__).resolve().parents[5]
    output = Path(sys.argv[1]).resolve()
    output.mkdir(parents=True, exist_ok=True)
    source = root / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/file_memory.rs'
    original = source.read_text()
    env = dict(os.environ)
    env['CARGO_TERM_COLOR'] = 'never'
    command = ['cargo', 'test', '--offline', '--locked', '--manifest-path',
               str(root / 'src/Cargo.toml'), '-p', 'pilotdeck-gateway',
               '--test', 'edgeclaw_dangling_links']
    results = []

    def run(name, selected=None):
        with (output / f'{name}.log').open('w') as log:
            gate = wait_for_slot(root / 'scripts/cargo-slots.sh')
            log.write(gate.stdout + gate.stderr)
            log.flush()
            args = command + (
                ['--', selected, '--test-threads', '2', '--nocapture']
                if selected
                else ['--', '--test-threads', '2'])
            result = run_owned(args, cwd=root, env=env, timeout=900)
            log.write(result.stdout + result.stderr)
            return result

    positive = run('positive')
    assert positive.returncode == 0, 'positive tests must pass before mutating'
    try:
        for mutant in MUTANTS:
            assert source.read_text() == original, 'another writer changed the product source'
            mutated = original
            for before, after in mutant['replacements']:
                assert mutated.count(before) == 1, \
                    f"{mutant['name']}: mutation anchor must be unique: {before!r}"
                mutated = mutated.replace(before, after, 1)
            source.write_text(mutated)
            try:
                negative = run(mutant['name'], 'dangling_link_')
                raw = (output / f"{mutant['name']}.log").read_text()
                statuses = dict(STATUS.findall(raw))
                suite = [line for line in raw.splitlines() if TESTS.match(line)]
                target = mutant['test']
                others = {name: statuses.get(name) for name in OPERATION_TESTS
                          if name != target}
                marker = f"{mutant['scenario']} must be rejected"
                compiled = 'error: could not compile' not in raw
                killed = (negative.returncode == 101
                          and compiled
                          and 'test result: FAILED' in raw
                          and statuses.get(target) == 'FAILED'
                          and all(value == 'ok' for value in others.values())
                          and marker in raw)
                record = dict(name=mutant['name'], test=target,
                              scenario=mutant['scenario'], exit=negative.returncode,
                              target=statuses.get(target), others=others,
                              suite=suite, marker=marker, killed=killed)
                results.append(record)
                print(f"{mutant['name']}: killed={killed}, "
                      f"target={statuses.get(target)}, others={others}",
                      flush=True)
                assert killed, \
                    f"{mutant['name']}: expected only {target} to fail, got {record}"
            finally:
                assert source.read_text() == mutated, 'another writer changed the mutant'
                source.write_text(original)
    finally:
        restored = source.read_text() == original
        (output / 'summary.json').write_text(json.dumps(dict(
            results=results, restored=restored,
            beforeSha256=hashlib.sha256(original.encode()).hexdigest(),
            afterSha256=hashlib.sha256(source.read_bytes()).hexdigest()), indent=2) + '\n')
        assert restored
    restored_run = run('restored')
    assert restored_run.returncode == 0, 'restored product must pass'


if __name__ == '__main__':
    main()
