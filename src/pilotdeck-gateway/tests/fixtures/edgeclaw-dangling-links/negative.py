#!/usr/bin/env python3
"""#563 negative control: each dangling-link operation needs its own red test.

Four mutants each permit exactly one of read / write / delete / edit through a
root-internal dangling link.  For every mutant the matching rejection test must
fail while the other operation tests stay green; that proves no operation is
only covered by an earlier panic in a shared loop.  Product source is restored
by the guard and the positive suite is rerun.

#1087 fix-r5 (sibling #3): the verdict rows stay in memory for the whole
guarded window and one final ``summary.json`` is published by a single
rename only after ``mutating_sources`` restored and byte-verified the
guarded source (the same skeleton as the edgeclaw tables). An abort inside
the window — a cancellation, a restore failure, the source guard — voids
the recorded rows, publishes one INVALID summary and exits 3. A red
positive baseline keeps exit 1 but still publishes its rows. The old
``finally``-inside-the-window summary write left a partial all-killed
summary behind when the run was cancelled mid-suite.
"""
import hashlib
import json
import os
from pathlib import Path
import re
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_panic import SUMMARY_FAILED, named_test_killed
from negative_process import (detach_dead_streams, mutating_sources,
                               run_owned, unique_summary_tmp,
                               wait_for_slot, write_source_atomic)


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
    env = dict(os.environ)
    env['CARGO_TERM_COLOR'] = 'never'
    command = ['cargo', 'test', '--offline', '--locked', '--manifest-path',
               str(root / 'src/Cargo.toml'), '-p', 'pilotdeck-gateway',
               '--test', 'edgeclaw_dangling_links']
    results = []
    # The guard's snapshot bytes, kept for the final restored comparison
    # after the window closes.
    original = None

    def write_summary():
        """Publish the one final summary atomically (#1087 fix-r5, sibling #3).

        Called exactly once per run, after ``mutating_sources`` restored and
        byte-verified the guarded source — or from the abort handler, after
        the same guard unwound. The old ``finally``-inside-the-window write
        published a summary even when the run had been cancelled mid-suite,
        leaving a partial all-killed file behind.
        """
        # The snapshot never happened (the guard refused a dirty baseline):
        # report that instead of crashing the void path on `None.encode()`.
        restored = None if original is None else source.read_text() == original
        payload = dict(
            results=results,
            restored=restored,
            beforeSha256=(None if original is None
                          else hashlib.sha256(original.encode()).hexdigest()),
            afterSha256=hashlib.sha256(source.read_bytes()).hexdigest())
        # #1087 fix-r5 (concurrency #3): this runner's OUT_DIR is created with
        # plain mkdir, not an exclusive claim, so two overlapping runs of the
        # same table must not share one fixed staging name.
        tmp = unique_summary_tmp(output)
        tmp.write_text(json.dumps(payload, indent=2) + '\n')
        tmp.replace(output / 'summary.json')

    def _void_pending(error):
        """Void the recorded rows and publish one INVALID summary (sibling #3).

        A cancellation or crash inside the guarded window is not a verdict:
        the rows recorded so far are voided, the remaining mutants are
        recorded void, and the single summary is published after the guard
        unwound. The process then exits 3 — deliberately different from 2
        ("never started", the stale-anchor refusal below) so a reader can
        tell a cancelled run from one that never ran.
        """
        reason = f'runner: {type(error).__name__}: {error}'
        for row in results:
            row['killed'] = False
            row['voided'] = reason
        for mutant in MUTANTS[len(results):]:
            results.append(dict(name=mutant['name'], test=mutant['test'],
                                scenario=mutant['scenario'], exit=None,
                                target=None, others={}, suite=[],
                                marker=f"{mutant['scenario']} must be rejected",
                                killed=False, voided=reason))
        write_summary()
        try:
            print(f'voided {len(results)} row(s): {reason}', file=sys.stderr, flush=True)
        except (OSError, ValueError):
            # #1087 fix-r5 (concurrency #2): the summary is already
            # published, so a closed stderr must not replace the exit code
            # below — "aborted = 3" is the only contract left to keep.
            pass
        if isinstance(error, (OSError, ValueError)):
            # #1087 fix-r5 (concurrency #2): a stream-failure abort detaches
            # the dead fds first, so the interpreter-shutdown flush cannot
            # rewrite the exit code below to 120 ("Exception ignored in:").
            detach_dead_streams()
        if isinstance(error, SystemExit):
            # Normalize: 128+signum and any other raw code must not decide
            # how a coordinator reads this run.
            raise SystemExit(3) from error
        if not isinstance(error, Exception):
            raise
        raise SystemExit(3) from error

    # Hold the shared flock from the pristine read through the final restored
    # run; the other edgeclaw in-place runners use the same lock file.
    try:
        with mutating_sources(root, [source]) as snapshots:
            original = snapshots[source].decode()
            # A stale anchor is a harness defect: refuse before any compile
            # (exit 2, nothing measured, nothing published).
            for mutant in MUTANTS:
                for before, _after in mutant['replacements']:
                    count = original.count(before)
                    if count != 1:
                        print(f"STALE ANCHOR {mutant['name']}: {count} matches: {before!r}",
                              file=sys.stderr)
                        raise SystemExit(2)

            def run(name, selected=None, gate=None):
                with (output / f'{name}.log').open('w') as log:
                    gate = gate or wait_for_slot(root / 'scripts/cargo-slots.sh')
                    log.write(gate.stdout + gate.stderr)
                    log.flush()
                    args = command + (
                        ['--', selected, '--test-threads', '2']
                        if selected
                        else ['--', '--test-threads', '2'])
                    result = run_owned(args, cwd=root, env=env, timeout=900)
                    log.write(result.stdout + result.stderr)
                    return result

            positive = run('positive')
            if positive.returncode != 0:
                # The positive baseline is red: nothing below is a verdict.
                # The guard unwinds and verifies while this exception
                # travels to the handler, which still publishes.
                print('positive tests did not pass before mutating', flush=True)
                raise SystemExit(1)

            for mutant in MUTANTS:
                mutated = original
                for before, after in mutant['replacements']:
                    mutated = mutated.replace(before, after, 1)
                # Reserve the Cargo slot before exposing the mutation.
                gate = wait_for_slot(root / 'scripts/cargo-slots.sh')
                # #1087 fix-r5 (sibling #3): one rename, never a truncating
                # in-place write; the shared publish helper (#1903) keeps a
                # SIGKILL mid-write from leaving a source the next preflight
                # would refuse as "differs from HEAD". No `mode=`: the
                # helper's own fallback stat covers the shape (boundary #5).
                write_source_atomic(root, source, mutated.encode())
                try:
                    negative = run(mutant['name'], 'dangling_link_', gate)
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
                              # #1087 fix-r5 (simplify #2): the shared
                              # dotted spelling — stricter than the bare
                              # word, same aborted-run guard.
                              and SUMMARY_FAILED in raw
                              and statuses.get(target) == 'FAILED'
                              and all(value == 'ok' for value in others.values())
                              and named_test_killed(raw, target, marker))
                    results.append(dict(name=mutant['name'], test=target,
                                        scenario=mutant['scenario'], exit=negative.returncode,
                                        target=statuses.get(target), others=others,
                                        suite=suite, marker=marker, killed=killed))
                    print(f"{mutant['name']}: killed={killed}, "
                          f"target={statuses.get(target)}, others={others}",
                          flush=True)
                finally:
                    # Same #1087 fix-r5 window: the in-loop restore is also
                    # one rename; the guard's own exit verification covers
                    # the bytes.
                    write_source_atomic(root, source, original.encode())
            if run('restored').returncode != 0:
                raise RuntimeError('restored product did not pass its tests')
    except SystemExit as error:
        if error.code == 1:
            # The deliberate positive-baseline failure: the guard already
            # restored and byte-verified the tree while unwinding the
            # with-block; publish the (empty) rows and keep the exit code.
            write_summary()
            raise
        if error.code == 2:
            # The deliberate stale-anchor refusal: nothing was measured, so
            # nothing is published — "never started" leaves no summary behind.
            raise
        _void_pending(error)
    except BaseException as error:  # noqa: BLE001 - void, publish, then re-raise
        _void_pending(error)
    # Reaching here means mutating_sources restored the guarded file and
    # verified it against the baseline bytes. Only now may a summary exist.
    write_summary()

    failures = [row['name'] for row in results if not row['killed']]
    print(f"summary: KILLED={sum(r['killed'] for r in results)} "
          f"INVALID={len(failures)}", flush=True)
    if failures:
        print(f'not killed: {", ".join(failures)}', file=sys.stderr)
        sys.exit(1)
    sys.exit(0)


if __name__ == '__main__':
    main()
