#!/usr/bin/env python3
"""#1128 harness controls: --check | [--compile-only] NEW_OUTPUT_DIR [name ...].

Mutations anchor the #1128 object-density harness (the workload builders,
the pretty reindentation copy, and the FNV digest) to its own self-check
`object_density_harness_selfcheck` in `src/pilotdeck-protocol/tests/object_density.rs`,
and anchor the copy to the CLI implementation it mirrors through
`object_density_pretty_copy_tracks_cli_implementation` (a mutation of the
CLI's `write_json_pretty_into` must fail that test, so the copy cannot
outlive a changed product pipeline). Every row is judged from the named
test's own panic payload (plain libtest messages, like the protocol rows of
sdk-decode-budget; no receipts).

Commit the patch before running mutants. CARGO_TARGET_DIR must be supplied by
the caller; each mutant compiles, runs the bound test, restores and rechecks
it. The scripts run against committed sources only (mutating_sources refuses
uncommitted product files), so the coordinator runs them on the host after
landing the patch. A stale anchor is an error, never a SURVIVED.
"""
from collections import namedtuple
import difflib
import json
import os
from pathlib import Path
import subprocess
import sys

sys.dont_write_bytecode = True
root = Path(__file__).resolve().parents[5]
shared = root / 'src/pilotdeck-gateway/tests/fixtures'
sys.path.insert(0, str(shared))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import Case, classify, passed

harness = root / 'src/pilotdeck-protocol/tests/object_density.rs'
cli_main = root / 'src/pilotdeck-cli/src/main.rs'
# The mutated source and the cargo test target are separate bindings: the
# mutated harness file's stem is the test target (#983 fix-r1 lesson).
Mutation = namedtuple('Mutation', 'name path package target old new test assertion')
mutations = [
    # The reindentation must keep the two-space indent of the product's
    # write_json_pretty_into; the deep-spine pretty byte golden pins it.
    Mutation('pretty-indent', harness, 'pilotdeck-protocol', 'object_density',
             'out.extend_from_slice(b"  ");',
             'out.extend_from_slice(b" ");',
             'object_density_harness_selfcheck', '#1128 deep-spine pretty bytes'),
    # ... and its ": " separator after colons (sibling of #983's
    # pretty-colon-space anchor on the CLI's own copy).
    Mutation('pretty-colon-space', harness, 'pilotdeck-protocol', 'object_density',
             "':' => out.extend_from_slice(b\": \"),",
             "':' => out.extend_from_slice(b\":\"),",
             'object_density_harness_selfcheck', '#1128 deep-spine pretty bytes'),
    # An empty container must stay inline: `[]`/`{}` never gain a newline. The
    # anchor carries the preceding extend_from_slice line because the pinned
    # CLI function text below re-spells the bare `let close` line verbatim.
    Mutation('pretty-empty-close', harness, 'pilotdeck-protocol', 'object_density',
             '                out.extend_from_slice(value.encode_utf8(&mut utf8).as_bytes());\n'
             "                let close = if value == '{' { '}' } else { ']' };",
             '                out.extend_from_slice(value.encode_utf8(&mut utf8).as_bytes());\n'
             "                let close = '}';",
             'object_density_harness_selfcheck',
             '#1128 empty containers stay inline in pretty output'),
    # The small-object grid is one `{"a":1}` per 8 wire bytes...
    Mutation('workload-small-element', harness, 'pilotdeck-protocol', 'object_density',
             'const SMALL_ELEMENT: &str = "{\\"a\\":1}";',
             'const SMALL_ELEMENT: &str = "{\\"a\\":2}";',
             'object_density_harness_selfcheck', '#1128 small-object builder exact bytes'),
    # ... and the self-check fixture (23 bytes) sits on the divisor boundary:
    # 23/8 = 2 elements but 23/7 = 3, so dropping the `+ 1` separator byte
    # changes the golden bytes (#1128 fix-r1, review opus #1).
    Mutation('workload-small-divisor', harness, 'pilotdeck-protocol', 'object_density',
             'let count = target_bytes / (SMALL_ELEMENT.len() + 1);',
             'let count = target_bytes / SMALL_ELEMENT.len();',
             'object_density_harness_selfcheck', '#1128 small-object builder exact bytes'),
    # The empty-object grid is one `{}` per 3 wire bytes, isolating the
    # per-object Box allocation from key/value/hash costs.
    Mutation('workload-empty-element', harness, 'pilotdeck-protocol', 'object_density',
             'const EMPTY_ELEMENT: &str = "{}";',
             'const EMPTY_ELEMENT: &str = "[]";',
             'object_density_harness_selfcheck', '#1128 empty-object builder exact bytes'),
    # Its fixture (23 bytes) is likewise on the divisor boundary: 23/3 = 7
    # elements but 23/2 = 11.
    Mutation('workload-empty-divisor', harness, 'pilotdeck-protocol', 'object_density',
             'let count = target_bytes / (EMPTY_ELEMENT.len() + 1);',
             'let count = target_bytes / EMPTY_ELEMENT.len();',
             'object_density_harness_selfcheck', '#1128 empty-object builder exact bytes'),
    # The deep spine pads each of the 10^4 levels through key "p"...
    Mutation('workload-deep-pad', harness, 'pilotdeck-protocol', 'object_density',
             'const DEEP_PAD_KEY: &str = "p";',
             'const DEEP_PAD_KEY: &str = "q";',
             'object_density_harness_selfcheck', '#1128 deep-spine builder exact bytes'),
    # ... and the self-check fixture (48 bytes at depth 2) distinguishes the
    # `- 1` in the pad math: 47/2 and 46/2 both pad 10, 48/2 pads 11
    # (#1128 fix-r1, review opus #6).
    Mutation('workload-deep-pad-boundary', harness, 'pilotdeck-protocol', 'object_density',
             'let pad = (target_bytes.saturating_sub(1) / depth).saturating_sub(13);',
             'let pad = (target_bytes / depth).saturating_sub(13);',
             'object_density_harness_selfcheck', '#1128 deep-spine builder exact bytes'),
    # The wide grid's key width is fixed at six digits; a different width
    # changes the exact golden bytes.
    Mutation('workload-wide-digits', harness, 'pilotdeck-protocol', 'object_density',
             'const WIDE_KEY_DIGITS: usize = 6;',
             'const WIDE_KEY_DIGITS: usize = 7;',
             'object_density_harness_selfcheck', '#1128 wide-object builder exact bytes'),
    # The cross-rep stability digest is FNV-1a 64 with the standard prime.
    Mutation('digest-fnv-prime', harness, 'pilotdeck-protocol', 'object_density',
             'hash = hash.wrapping_mul(0x0000_0100_0000_01b3);',
             'hash = hash.wrapping_mul(0x0000_0100_0000_01b5);',
             'object_density_harness_selfcheck', '#1128 pinned compact FNV digest'),
    # The pretty copy must track the CLI's own write_json_pretty_into: the
    # binding test pins that function's exact text, so any change to the
    # product pipeline fails the harness (#1128 fix-r1, review P2). The
    # indent literal is one knob; the colon separator is its sibling (#1128
    # fix-r2, review sibling #1: one knob per mutation, no blanket trust).
    Mutation('cli-pretty-indent', cli_main, 'pilotdeck-protocol', 'object_density',
             'out.write_all(b"  ")?;',
             'out.write_all(b" ")?;',
             'object_density_pretty_copy_tracks_cli_implementation',
             '#1128 CLI write_json_pretty_into changed; re-derive the harness pretty copy'),
    Mutation('cli-pretty-colon-space', cli_main, 'pilotdeck-protocol', 'object_density',
             "':' => out.write_all(b\": \")?,",
             "':' => out.write_all(b\":\")?,",
             'object_density_pretty_copy_tracks_cli_implementation',
             '#1128 CLI write_json_pretty_into changed; re-derive the harness pretty copy'),
    # The binding test also diffs the pinned CLI text against the live copy's
    # byte literals: mutating the pinned constant alone simulates a CLI change
    # where only the constant was re-derived and `reindent_pretty` stayed
    # stale, which must fail at that parity assertion, not pass (#1128 fix-r2,
    # review opus #3). The anchor lives only inside the pinned constant text
    # in the harness; the live copy spells the same rule with
    # extend_from_slice.
    Mutation('cli-pretty-colon-parity', harness, 'pilotdeck-protocol', 'object_density',
             "':' => out.write_all(b\": \")?,",
             "':' => out.write_all(b\":\")?,",
             'object_density_pretty_copy_tracks_cli_implementation',
             '#1128 pretty byte literals diverged between CLI and the harness copy'),
]


def check(row):
    count = row.path.read_text().count(row.old)
    if count != 1 or row.old == row.new or not row.test or not row.assertion:
        raise ValueError(f'{row.name}: stale/invalid anchor: expected=1 actual={count}')
    target = root / 'src' / row.package / 'tests' / f'{row.target}.rs'
    if not target.is_file():
        raise ValueError(f'{row.name}: cargo test target missing: {target.relative_to(root)}')
    if row.test not in target.read_text():
        raise ValueError(
            f'{row.name}: bound test {row.test} not declared in {target.relative_to(root)}')


def crate_files(rel):
    # Enumerate from the HEAD tree, not the index: a staged deletion must not
    # drop a file from the guard while Cargo still reads it from disk (#983 r6).
    out = subprocess.run(['git', '-C', str(root), 'ls-tree', '-r', '-z', '--name-only', 'HEAD', '--', rel],
                         check=True, capture_output=True).stdout
    files = [root / name for name in out.decode().split('\0') if name]
    if not files:
        raise SystemExit(f'#1128 no tracked files under {rel}')
    return files


def tree_matches_head():
    """Every tracked file (index and worktree) equals HEAD: closes transitive build inputs (#983 r6)."""
    staged = subprocess.run(['git', '-C', str(root), 'diff', '--cached', '--quiet', 'HEAD'])
    unstaged = subprocess.run(['git', '-C', str(root), 'diff', '--quiet', 'HEAD'])
    if staged.returncode != 0 or unstaged.returncode != 0:
        listing = subprocess.run(['git', '-C', str(root), 'status', '--porcelain', '--untracked-files=no'],
                                 capture_output=True, text=True).stdout
        raise RuntimeError('#1128 tracked tree differs from HEAD before positive control:\n' + listing)


def main():
    args = sys.argv[1:]
    if args == ['--check']:
        errors = []
        for row in mutations:
            try:
                check(row)
            except Exception as error:
                errors.append(str(error))
        for error in errors:
            print('INVALID ' + error)
        print(f'#1128 anchors: {len(mutations) - len(errors)}/{len(mutations)} valid')
        return int(bool(errors))
    compile_only = '--compile-only' in args
    args = [arg for arg in args if arg != '--compile-only']
    if not args:
        raise SystemExit('usage: --check | [--compile-only] NEW_OUTPUT_DIR [name ...]')
    selected = [row for row in mutations if not args[1:] or row.name in args[1:]]
    if not selected or (args[1:] and len(selected) != len(args[1:])):
        raise SystemExit('unknown/duplicate mutation selection')
    env = dict(os.environ)
    if not env.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
    out = Path(args[0]).resolve()
    out.mkdir(parents=True, exist_ok=False)

    def run(label, row, run_env, *, compile=False):
        # No -q: cargo forwards it to libtest as --quiet, which drops the
        # per-test `test <name> ... ok` line the judge needs (#273, #978).
        slots = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=600, interval=15)
        (out / f'{label}.slots.log').write_text(slots.stdout + slots.stderr)
        command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
                   '-p', row.package, '--test', row.target]
        command += (['--no-run'] if compile else
                   [row.test, '--', '--exact', '--show-output', '--test-threads=1'])
        result = run_owned(command, cwd=root, env=run_env, timeout=900)
        (out / f'{label}.stdout').write_text(result.stdout)
        (out / f'{label}.stderr').write_text(result.stderr)
        return result

    guarded = [harness, Path(__file__).resolve(),
               root / 'src/pilotdeck-protocol/Cargo.toml',
               root / 'src/Cargo.toml',
               # Close the class instead of one file per round — every tracked
               # file of the crate the bound test compiles (#983 r5).
               *crate_files('src/pilotdeck-protocol'),
               # The binding test reads the CLI's main.rs at compile time, so a
               # mutated or drifted main.rs is a build input of this suite.
               cli_main,
               root / 'src/Cargo.lock', shared / 'negative_process.py',
               shared / 'negative_suite.py', shared / 'negative_panic.py',
               shared / 'pending_reanchor.py',
               root / 'scripts/cargo-slots.sh']
    results = []
    for row in selected:
        item = dict(name=row.name, test=row.test, status='INVALID', reason='not run',
                    compiled=False)
        try:
            check(row)
            with mutating_sources(root, guarded) as originals:
                original = originals[row.path].decode()
                try:
                    tree_matches_head()
                    if not compile_only and not passed(run(row.name + '-positive', row, env),
                                                        row.test):
                        raise RuntimeError('positive bound test failed')
                    changed = original.replace(row.old, row.new, 1)
                    (out / f'{row.name}.patch').write_text(''.join(difflib.unified_diff(
                        original.splitlines(True), changed.splitlines(True),
                        fromfile=str(row.path.relative_to(root)),
                        tofile=str(row.path.relative_to(root)))))
                    row.path.write_text(changed)
                    if run(row.name + '-compile', row, env, compile=True).returncode != 0:
                        raise RuntimeError('mutant compile failed; not evidence')
                    item['compiled'] = True
                    if compile_only:
                        item.update(status='COMPILED', reason='mutant no-run passed')
                    else:
                        result = run(row.name + '-negative', row, env)
                        status, reason = classify(result, Case(
                            row.name, row.path, row.old, row.new, row.test, row.assertion))
                        item.update(status=status, reason=reason)
                finally:
                    # Only restore bytes supplied by the official HEAD guard.
                    row.path.write_bytes(originals[row.path])
                    if row.path.read_bytes() != originals[row.path]:
                        raise RuntimeError('restoration mismatch')
                if not compile_only and not passed(run(row.name + '-restored', row, env),
                                                    row.test):
                    item.update(status='INVALID', reason=item['reason'] +
                                '; restored bound test failed')
        except Exception as error:
            item.update(status='INVALID', compiled=False, reason=f'{type(error).__name__}: {error}')
        results.append(item)
        print(f'{row.name}: {item["status"]}: {item["reason"]}', flush=True)
    counts = {state: sum(item['status'] == state for item in results)
              for state in ['KILLED', 'SURVIVED', 'INVALID', 'COMPILED']}
    (out / 'summary.json').write_text(json.dumps(dict(controls=results, counts=counts),
                                                 indent=2) + '\n')
    print(json.dumps(counts), flush=True)
    return int(any(item['status'] != ('COMPILED' if compile_only else 'KILLED')
                   for item in results))


if __name__ == '__main__':
    raise SystemExit(main())
