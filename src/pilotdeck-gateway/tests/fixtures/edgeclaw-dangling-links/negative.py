#!/usr/bin/env python3
"""#563 negative control: each dangling-link operation needs its own red test.

Four mutants each permit exactly one of read / write / delete / edit through a
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
# #597 moved the write behind a root-directory handle: `write_bytes` pins the
# boundary with `mutation_target` (whose `checked_path` refuses a dangling
# link) and then overwrites the relative target.  The old path-based anchor
# (`let absolute = self.path(path)?;`) no longer exists.  Mutating the live
# unix path keeps the defect shape: resolve the leaf link the boundary check
# refused and write through it, like TS `writeFileSync`.
WRITE_TARGET_BEFORE = '''            let (root, target) = self.mutation_target(relative)?;
            // A root-addressed relative has no parent: Node's writeFileSync on
'''
WRITE_TARGET_AFTER = '''            // Mutant: a leaf symlink the boundary check refuses is resolved by
            // hand and written through, like TS writeFileSync follows the link.
            let symlink = fs::symlink_metadata(self.root.join(relative))
                .is_ok_and(|m| m.file_type().is_symlink());
            let (root, target) = match self.mutation_target(relative) {
                Ok(pinned) => pinned,
                Err(_) if symlink => {
                    let link = absolute(&self.root.join(relative))?;
                    let target = fs::read_link(&link)
                        .map_err(|e| crate::node_io::path_error(e, "readlink", &link))?;
                    let target = if target.is_relative() {
                        link.parent().unwrap_or(self.root.as_path()).join(target)
                    } else {
                        target
                    };
                    // Retry the resolved target through the normal pinned-root
                    // path, so the descriptor root and the relative target are
                    // derived from the same (lexical) boundary.
                    let boundary = absolute(&self.root)?;
                    let resolved = target
                        .strip_prefix(&boundary)
                        .unwrap_or(&target)
                        .to_string_lossy()
                        .into_owned();
                    self.mutation_target(&resolved)?
                }
                Err(e) => return Err(e),
            };
            // A root-addressed relative has no parent: Node's writeFileSync on
'''

# #597 replaced the old `fs::remove_file(self.path(...))` block with the
# descriptor-relative `unlink`.  The rejection now lives in two places: the
# `deleteEntries` guard (`within` + `path`) and `unlink`'s own `within`.
# Mutating both removes exactly that check, so the dangling link is deleted
# instead of rejected; the descriptor `unlink_at` leg otherwise stays intact.
DELETE_BEFORE = '''                    if self.within(&path)? && self.path(&path)?.exists() {
                        self.unlink(&path)?;
                        changed.push(path);
                    }
'''
DELETE_AFTER = '''                    // Mutant: a lexically present path (dangling link included)
                    // is accepted instead of rejected by the boundary check.
                    if fs::symlink_metadata(self.root.join(&path)).is_ok() {
                        self.unlink(&path)?;
                        changed.push(path);
                    }
'''
DELETE_UNLINK_BEFORE = '''            self.within(relative)?;
            // unlink removes an allowed leaf symlink, not its canonical target.
'''
DELETE_UNLINK_AFTER = '''            // Mutant: the caller's guard already accepted the path, so unlink
            // no longer re-checks it and removes the dangling link itself.
            // unlink removes an allowed leaf symlink, not its canonical target.
'''

EDIT_LOOKUP_BEFORE = '''            "editEntry" => {
                let path = text(input, "relativePath");
                let old = self
                    .records(std::slice::from_ref(&path), 5000.0)?
                    .into_iter()
                    .next()
                    .ok_or_else(|| {
                        input_error("Memory entry not found: ", input, "relativePath")
                    })?;
'''
EDIT_LOOKUP_AFTER = '''            "editEntry" => {
                let path = text(input, "relativePath");
                let old = self
                    .records(std::slice::from_ref(&path), 5000.0)
                    .unwrap_or_default()
                    .into_iter()
                    .next()
                    .unwrap_or_else(|| obj(&[("type", s("user")), ("scope", s("global"))]));
'''
EDIT_WRITE_BEFORE = '                JsValue::Object(self.write(&path, &fm, &body(&candidate))?)\n'
EDIT_WRITE_AFTER = '''                let relative = match self.within(&path) {
                    Ok(true) => path.clone(),
                    _ => {
                        let absolute = absolute(&self.root.join(&path))?;
                        let target = fs::read_link(&absolute)
                            .map_err(|e| crate::node_io::path_error(e, "readlink", &absolute))?;
                        let target = if target.is_relative() {
                            absolute
                                .parent()
                                .unwrap_or(self.root.as_path())
                                .join(target)
                        } else {
                            target
                        };
                        target
                            .strip_prefix(&self.root)
                            .unwrap_or(target.as_path())
                            .to_string_lossy()
                            .into_owned()
                    }
                };
                JsValue::Object(self.write(&relative, &fm, &body(&candidate))?)
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
            (WRITE_TARGET_BEFORE, WRITE_TARGET_AFTER),
        ],
    ),
    dict(
        name='allow-delete-dangling',
        scenario='delete',
        test='dangling_link_delete_is_rejected_before_any_io',
        replacements=[
            (DELETE_BEFORE, DELETE_AFTER),
            (DELETE_UNLINK_BEFORE, DELETE_UNLINK_AFTER),
        ],
    ),
    dict(
        name='allow-edit-dangling',
        scenario='edit-anchor',
        test='dangling_link_edit_is_rejected_before_any_io',
        replacements=[
            (EDIT_LOOKUP_BEFORE, EDIT_LOOKUP_AFTER),
            (EDIT_WRITE_BEFORE, EDIT_WRITE_AFTER),
        ],
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
