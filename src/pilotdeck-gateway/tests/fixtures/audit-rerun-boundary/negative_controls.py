#!/usr/bin/env python3
"""#1723: the audit/rerun boundary guards must fail when their rules fail.

Run after committing the candidate. Never bypass negative_process's HEAD
guard. Each mutation breaks one #1723 boundary device in test_process.rs and
its bound libtest parent must fail at the matching "#1723 ..." assertion:

* kernel-cap-removed — the sh wrapper stops setting `ulimit -f`, so the
  capture runs past the cap for a poll interval before the sampler looks;
  bound to owned_rerun_hits_the_kernel_output_cap's byte-ceiling assertion.
* hit-cap-not-overflow — the `>=` cap criterion regresses to the sampled
  era's strict `>`; a writer the kernel stops at exactly the cap then reads
  as under it. Bound to the surviving-writer test's overflow assertion: its
  writer deterministically parks the file at exactly the cap while the
  leader lives on, so only the sampler's `>=` (or the deadline, which the
  same assertion distinguishes) can classify it.
* reuse-not-flagged — `reappeared` is never set, so a pid that left the
  sweeps and returned with the same exe reads as the child the before-set
  already knew. Bound to the injected-proc-root reuse test's transient
  assertion.
* stopped-not-recorded — the stop branch fires its group kill but never
  records `stopped`. Bound to the deadline test's stopped assertion.
* children-only-sweeps-nothing — the children sweep observes nothing, so
  the window opens empty and every child audit is vacuous. Bound to the
  same reuse test's before-set assertion.
* finished-on-its-own-merged — `finished_on_its_own` regresses to the
  pre-split merged judgment (the clock fact `timed_out` instead of the
  outcome), so a leader that finished on its own inside the deadline's
  poll window reads as stopped again. Bound to the predicate's own
  contract test, which constructs RerunOutput directly: the race window
  is too narrow to drive deterministically through rerun_exact.
* ended-its-own-merged — the merged pass predicate `ended_its_own_or_unstopped`
  (what rerun_audit_case, the grandchild test and the slack retention worker
  branch on, fix-r4) loses its `finished_on_its_own` arm and regresses to the
  same pre-split clock judgment. Bound to the merged predicate's own contract
  test, same construction-only reason as the row above: no audit path
  constructs RerunOutput, and a stopped rerun's status is a SIGKILL, never
  `success()`, so the deterministic paths cannot catch the dropped arm.
* fsize-unit-constant-trusted — the ulimit -f unit probe trusts a
  compile-time constant again instead of the bytes it measured (2048 B is
  wrong on every measured host), so a drifted unit table must fail
  closed. Bound to owned_rerun_hits_the_kernel_output_cap: the probe runs
  before any rerun, so the bound parent dies at the probe's own
  assertion.
* probe-hang-deadline — the ulimit -f unit probe's command is replaced by an
  unbounded `sleep`, so the probe never writes its file and the 10 s probe
  deadline must group-kill it and fail the bound parent at the probe's own
  panic — the bound that keeps a hung probe from pinning every later
  rerun_exact caller through the OnceLock. The reverse direction (mutating
  the deadline away) can only hang the parent and is INVALID by nature, not
  a kill; it is deliberately not a row.

Every row runs its positive control, compiles the mutant (a mutant that
does not compile is INVALID, never a kill), runs the bound parent, then
restores and re-runs the positive control; rows are recorded
KILLED/SURVIVED/INVALID with their reason and summarized at the end — no
first-failure abort. Rows bound to a Linux-only parent
(#[cfg(target_os = "linux")]) are recorded SKIPPED on other hosts instead
of INVALID — but only after observing the premise, never assuming it: the
anchor count *and the bound parent's own `fn <leaf>(` definition* are
checked on every host (a renamed or deleted parent test keeps the anchor
count and would otherwise skip silently forever), and a `cargo test --list`
run — carried out inside the same `mutating_sources` tree lock as the rows,
so it compiles the baseline and not another runner's mutant — must carry a
known-to-exist control test and must not select the bound parent before a
row is recorded SKIPPED. A premise the listing disproves (a relaxed cfg, a
renamed test) is INVALID and fails the run, never a silent skip plus exit
0. A run with zero KILLED rows still exits nonzero: nothing was verified.
"""
import argparse
import difflib
import hashlib
import json
import os
import re
from pathlib import Path
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

ROOT = Path(__file__).resolve().parents[5]
SOURCE = ROOT / 'src/pilotdeck-gateway/src/test_process.rs'
# #2133: neg-for-diff.sh derives the host's negative-control list from the
# diff and matches tables by the literal `dir/file` of every changed file.
# `SOURCE` spells the whole path inline, so its key is already in this
# table's text; the shared helper this table imports is not — the import
# line spells a module name while the host keys `fixtures/negative_process.py`.
DISCOVERY_ANCHORS = ('fixtures/negative_process.py',)

# Bound parent test -> cargo target selection for the workspace manifest.
CARGO_TARGETS = {
    'test_process::owned_rerun_hits_the_kernel_output_cap':
        ['-p', 'pilotdeck-gateway', '--lib'],
    'test_process::owned_rerun_stops_a_writer_that_survives_the_kernel_cap':
        ['-p', 'pilotdeck-gateway', '--lib'],
    'test_process::owned_rerun_kills_the_whole_group_at_the_deadline':
        ['-p', 'pilotdeck-gateway', '--lib'],
    'test_process::the_sampler_reports_a_pid_that_leaves_and_returns':
        ['-p', 'pilotdeck-gateway', '--lib'],
    'test_process::finished_on_its_own_branches_on_the_outcome_not_the_clock':
        ['-p', 'pilotdeck-gateway', '--lib'],
    'test_process::ended_its_own_or_unstopped_branches_on_who_stopped_it':
        ['-p', 'pilotdeck-gateway', '--lib'],
}

# Parents that exist only behind #[cfg(target_os = "linux")] in
# test_process.rs: on any other host the exact filter selects zero tests, so
# a row bound to one of them cannot fire there. SKIPPED, not INVALID — the
# mutation is unreachable on this host, not unfired (the exact-rerun-receipt
# table's platform gate for the same situation).
LINUX_ONLY_TESTS = {
    'test_process::owned_rerun_kills_the_whole_group_at_the_deadline',
    'test_process::the_sampler_reports_a_pid_that_leaves_and_returns',
}
IS_LINUX = sys.platform.startswith('linux')

MUTATIONS = [
    # The kernel cap leaves the wrapper: the capture can run one poll
    # interval of writes past the cap before the sampler's backstop looks.
    ('kernel-cap-removed', SOURCE,
     r'.arg(format!("ulimit -f {} && exec \"$@\"", rerun_fsize_blocks()))',
     r'.arg(format!("exec \"$@\""))',
     'test_process::owned_rerun_hits_the_kernel_output_cap',
     '#1723 the capture stops at the cap, not past it'),
    # The cap criterion regresses to the sampled era's strict `>`: a file at
    # exactly the cap is a writer the kernel stopped, not an under-cap run.
    ('hit-cap-not-overflow', SOURCE,
     'Ok(len) => len >= RERUN_OUTPUT_LIMIT,',
     'Ok(len) => len > RERUN_OUTPUT_LIMIT,',
     'test_process::owned_rerun_stops_a_writer_that_survives_the_kernel_cap',
     '#1723 a writer holding the capture at the cap is an overflow'),
    # A returned pid is no longer flagged: the same-exe reuse reads as the
    # child the before-set already knew, and the window's judgment goes blind.
    ('reuse-not-flagged', SOURCE,
     'let reappeared = self.children.contains_key(&pid) && !self.previous.contains(&pid);',
     'let reappeared = false;',
     'test_process::the_sampler_reports_a_pid_that_leaves_and_returns',
     '#1723 the sampler reports the returned pid as a transient spawn'),
    # The stop branch fires its group kill but never records that it did.
    ('stopped-not-recorded', SOURCE,
     'timed_out = expired;\n            overflow = too_big;\n            stopped = true;',
     'timed_out = expired;\n            overflow = too_big;\n            stopped = false;',
     'test_process::owned_rerun_kills_the_whole_group_at_the_deadline',
     '#1723 the deadline stop must be recorded'),
    # The children sweep observes nothing: the window opens empty and every
    # child judgment is vacuous — the exact false pass the reuse device
    # exists to catch (its fixture child is invisible in the before-set).
    ('children-only-sweeps-nothing', SOURCE,
     'Ok((seen, foreign))',
     'Ok((Vec::new(), foreign))',
     'test_process::the_sampler_reports_a_pid_that_leaves_and_returns',
     "#1723 the window's before-set carries the fixture child"),
    # The judgment regresses to the pre-split merged shape: the clock fact
    # decides again, so a leader that finished on its own inside the
    # deadline's poll window reads as stopped. Bound to the predicate's own
    # contract test — that race window cannot be driven deterministically
    # through rerun_exact, and the literal removed rewrite is dead code on
    # every deterministic path, so the contract is pinned by construction.
    ('finished-on-its-own-merged', SOURCE,
     'self.status.success() && !self.overflow',
     '!self.timed_out && !self.overflow',
     'test_process::finished_on_its_own_branches_on_the_outcome_not_the_clock',
     "#1723 a rerun that finished on its own inside the deadline's window is not a stop"),
    # The unit probe trusts a compile-time constant again instead of the
    # bytes it measured: a drifted unit table (2048 B is wrong on every
    # measured host) must fail closed before any rerun runs.
    ('fsize-unit-constant-trusted', SOURCE,
     'landed > 0 && landed < 4096 && RERUN_OUTPUT_LIMIT.is_multiple_of(landed)',
     'landed == 2048',
     'test_process::owned_rerun_hits_the_kernel_output_cap',
     "#1723 the wrapping shell's ulimit -f unit is"),
    # fix-r4 (sibling#2): the merged pass predicate loses its
    # `finished_on_its_own` arm and regresses to the pre-split clock
    # judgment — the audits' pass condition again reads a leader that
    # finished on its own inside the deadline's poll window as stopped.
    # Bound to the predicate's own contract test (same
    # construction-only reason as finished-on-its-own-merged: no audit
    # path constructs RerunOutput, and a stop-path status is a SIGKILL,
    # never success()).
    ('ended-its-own-merged', SOURCE,
     'self.finished_on_its_own() || (!self.timed_out && !self.overflow)',
     '!self.timed_out && !self.overflow',
     'test_process::ended_its_own_or_unstopped_branches_on_who_stopped_it',
     "#1723 a rerun that finished on its own inside the deadline's window was not stopped by this harness"),
    # fix-r4 (sibling#3): the probe never finishes on its own — the 10 s
    # probe deadline must group-kill it and fail the bound parent at the
    # probe's own panic instead of pinning every later rerun_exact caller
    # through the OnceLock. Deterministic: the mutant's sleep far exceeds
    # the deadline, so the kill lands within it on any host.
    ('probe-hang-deadline', SOURCE,
     r'.arg("ulimit -f 1; head -c 4096 /dev/zero > \"$1\"")',
     r'.arg("sleep 100")',
     'test_process::owned_rerun_hits_the_kernel_output_cap',
     '#1723 the ulimit -f unit probe was still running'),
]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', type=Path, help='fresh evidence directory')
    parser.add_argument('mutations', nargs='*')
    # HEAD after the candidate is committed. Before that, a pre-commit full run
    # can pass the SHA printed by "git stash create" (a dangling commit of the
    # current working tree, no ref moved): every mutated byte is still checked
    # against, and restored to, that recorded tree.
    parser.add_argument('--baseline', default='HEAD')
    args = parser.parse_args()
    if not os.environ.get('CARGO_TARGET_DIR'):
        parser.error('CARGO_TARGET_DIR must be supplied by the caller')
    if args.mutations and not set(args.mutations) <= {row[0] for row in MUTATIONS}:
        parser.error('unknown mutation name')
    selected = [row for row in MUTATIONS
                if not args.mutations or row[0] in args.mutations]
    env = os.environ.copy()
    env['CARGO_PROFILE_DEV_DEBUG'] = 'line-tables-only'
    env['CARGO_PROFILE_TEST_DEBUG'] = 'line-tables-only'
    args.output.mkdir(parents=True, exist_ok=False)

    def save(name, text):
        with (args.output / name).open('x') as stream:
            stream.write(text)

    def run(label, test, compile_only=False):
        slots = wait_for_slot(ROOT / 'scripts/cargo-slots.sh', timeout=600, interval=10)
        save(f'{label}.slots.log', slots.stdout + slots.stderr)
        # No -q: cargo forwards it to libtest as --quiet, which replaces the
        # per-test "test <name> ... ok/FAILED" lines this script's pass/kill
        # criteria are written against (a quiet pass is a lone "." and a quiet
        # failure is "<name> --- FAILED").
        # No `--nocapture` (#1053): capture keeps a printing parent's stdout
        # out of the "test <name> ... ok/FAILED" status line that passed() and
        # classify() match exactly.
        # --test-threads=2 (#942 fix-r1): single-threaded libtest prints the
        # test name before the body runs, so a printing parent would splice
        # its output into the status line under an ambient RUST_TEST_THREADS=1
        # and fail the positive control; the explicit flag pins the layout.
        result = run_owned([
            'cargo', 'test', '--manifest-path', 'src/Cargo.toml',
            *CARGO_TARGETS[test],
            *(['--no-run'] if compile_only else [
                '--', '--exact', test, '--test-threads=2',
            ]),
        ], cwd=ROOT, env=env, timeout=900)
        log = result.stdout + result.stderr
        save(f'{label}.log', log)
        save(f'{label}.exit', f'{result.returncode}\n')
        return result, log

    def passed(result, test, log):
        return (result.returncode == 0 and f'test {test} ... ok' in log
                and ' 1 passed; 0 failed' in log)

    def classify(result, test, assertion, log):
        if passed(result, test, log):
            return 'SURVIVED', 'bound test still passed'
        if (result.returncode != 0 and f'test {test} ... FAILED' in log
                and assertion in log and 'could not compile' not in log):
            return 'KILLED', 'bound test failed at its #1723 assertion'
        return 'INVALID', 'no attributable runtime failure (build error, wrong panic, or missing test)'

    # fix-r3 (boundary#2/concurrency#2): a SKIPPED row's premise — the exact
    # filter cannot select the bound parent on this host — is observed, not
    # assumed. One listing per cargo target, cached (fix-r4, simplify#5: the
    # Linux-only parents share one `-p pilotdeck-gateway --lib` target, so
    # per-test keys would re-run an identical listing): the listing compiles
    # the test binary, so it pays the same cargo slot the rows themselves do.
    listed = {}

    # fix-r4 (boundary#2): the listing must itself be proven alive — a
    # listing that ran against an empty or drifted compilation surface
    # selects nothing for every name, which is indistinguishable from the cfg
    # the skip is supposed to be about. This test exists on every host the
    # row table can run on (#[cfg(unix)], not in LINUX_ONLY_TESTS), so its
    # absence means the listing, not the cfg, is broken.
    SANITY_TEST = 'test_process::finished_on_its_own_branches_on_the_outcome_not_the_clock'

    def list_selects(test):
        """(error, selects, sane): `error` is None when the listing ran;
        `selects` is whether the bound parent is selectable on this host, and
        `sane` is whether the listing carried the known-to-exist control test.
        `--list` (no exact filter) prints one `name: test` line per test in
        the compiled surface: a parent behind #[cfg(target_os = "linux")]
        prints none on other hosts — and so does a parent that no longer
        exists, which is why the caller also anchors the parent's own `fn`
        and why `sane` must be checked before a none-selection is believed."""
        target = tuple(CARGO_TARGETS[test])
        if target not in listed:
            slots = wait_for_slot(ROOT / 'scripts/cargo-slots.sh', timeout=600, interval=10)
            save(f'{test}-list.slots.log', slots.stdout + slots.stderr)
            try:
                result = run_owned([
                    'cargo', 'test', '--manifest-path', 'src/Cargo.toml',
                    *CARGO_TARGETS[test],
                    '--', '--list',
                ], cwd=ROOT, env=env, timeout=900)
            except subprocess.TimeoutExpired as error:
                listed[target] = (f'--list timed out after {error.timeout}s', None)
                return listed[target] + (None,)
            log = result.stdout + result.stderr
            save(f'{test}-list.log', log)
            save(f'{test}-list.exit', f'{result.returncode}\n')
            if result.returncode != 0 or 'could not compile' in log:
                listed[target] = (f'--list exited {result.returncode}', None)
            else:
                listed[target] = (None, result.stdout)
        error, full = listed[target]
        if error is not None:
            return error, None, None
        # A cache hit for a shared target still leaves this row's own
        # evidence file — the bytes are the same listing. Membership is a
        # whole-line compare, not a substring: a test whose name merely ends
        # in the bound parent's would otherwise satisfy it.
        if not (args.output / f'{test}-list.log').exists():
            save(f'{test}-list.log', full)
        lines = full.split('\n')
        return None, f'{test}: test' in lines, f'{SANITY_TEST}: test' in lines

    summary = []
    for name, source, old, new, test, assertion in selected:
        if not IS_LINUX and test in LINUX_ONLY_TESTS:
            # fix-r3 (boundary#2/concurrency#2): verify the skip premise
            # instead of trusting the static whitelist — and run the
            # host-independent anchor check for SKIPPED rows too, so a
            # drifted anchor does not wait for a Linux host to surface.
            # fix-r4 (concurrency#2): the listing runs inside the same
            # `mutating_sources` window (tree lock) — outside it another
            # runner's mutation of the same source could be what the listing
            # compiles, so a broken mutant would land here as "cannot
            # observe the premise" and the premise would not be a baseline
            # fact at all.
            # fix-r4 (boundary#2): the anchor check also counts the bound
            # parent's own definition — a renamed or deleted parent test
            # keeps the mutation anchor count and would otherwise skip
            # silently forever on every non-Linux host.
            with mutating_sources(ROOT, [source], baseline=args.baseline) as snapshots:
                content = snapshots[source].decode()
                # A definition shape, not a substring: a deleted parent whose
                # name survives in a comment or a string literal would
                # otherwise still count as defined (fix-r5, review P2a).
                defined = re.search(
                    rf'^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn '
                    rf'{re.escape(test.rsplit("::", 1)[-1])}\s*\(', content, re.M)
                count = content.count(old)
                if count != 1:
                    status, reason = 'INVALID', f'anchor count {count}'
                elif not defined:
                    status, reason = 'INVALID', (
                        f'bound parent {test} is not defined in the source; renamed or deleted '
                        '— the row cannot be skipped on a rename')
                else:
                    error, selects, sane = list_selects(test)
                    if error is not None:
                        status, reason = 'INVALID', f'cannot observe the skip premise: {error}'
                    elif not sane:
                        status, reason = 'INVALID', (
                            'cannot observe the skip premise: the listing carried no known '
                            f'test ({SANITY_TEST} not listed)')
                    elif selects:
                        status, reason = 'INVALID', (
                            'skip premise broken: the exact filter selects the bound parent '
                            f'{test} on {sys.platform}; fix the cfg or LINUX_ONLY_TESTS')
                    else:
                        status, reason = 'SKIPPED', (
                            f'bound parent {test} is #[cfg(target_os = "linux")]; unreachable '
                            f'on {sys.platform} (verified: --list carries the sanity control '
                            'and selects no such test, and '
                            f'fn {test.rsplit("::", 1)[-1]} is still defined in the source)')
            save(f'{name}.json', json.dumps(
                {'name': name, 'status': status, 'reason': reason}, indent=2) + '\n')
            summary.append({'name': name, 'status': status, 'reason': reason})
            print(f'{name}: {status}: {reason}', flush=True)
            continue
        with mutating_sources(ROOT, [source], baseline=args.baseline) as originals:
            # anchor_selfcheck：每个快照名（snapshots/originals）单文件只许一次
            # with 声明；第二处绑定用 originals，负控语义不变（#1723 fix-r6）。
            original = originals[source]
            content = original.decode()
            if content.count(old) != 1:
                save(f'{name}.json', json.dumps(
                    {'name': name, 'status': 'INVALID',
                     'reason': f'anchor count {content.count(old)}'}, indent=2) + '\n')
                summary.append({'name': name, 'status': 'INVALID',
                                'reason': f'anchor count {content.count(old)}'})
                continue
            result, log = run(f'{name}-positive', test)
            positive = passed(result, test, log)
            changed = content.replace(old, new, 1)
            save(f'{name}.patch', ''.join(difflib.unified_diff(
                content.splitlines(True), changed.splitlines(True),
                fromfile=str(source.relative_to(ROOT)), tofile=str(source.relative_to(ROOT)))))
            source.write_text(changed)
            build, build_log = run(f'{name}-compile', test, compile_only=True)
            compiled = build.returncode == 0 and 'could not compile' not in build_log
            if not compiled:
                status, reason = 'INVALID', 'mutant failed no-run compilation; not a kill'
            else:
                result, log = run(f'{name}-negative', test)
                if positive:
                    status, reason = classify(result, test, assertion, log)
                else:
                    status, reason = 'INVALID', 'positive control did not pass the bound test'
        # Restoration and byte-level SHA validation belong to mutating_sources's
        # finally, which runs under the tree lock — re-checking here would race
        # another runner's mutation of the same file (fix-r1 review).
        with mutating_sources(ROOT, [source], baseline=args.baseline):
            result, restored = run(f'{name}-restored', test)
            if positive and not passed(result, test, restored):
                status, reason = 'INVALID', reason + '; restored bound test did not pass'
        summary.append({'name': name, 'status': status, 'reason': reason,
                        'compiled': compiled, 'sourceSha256': hashlib.sha256(original).hexdigest()})
        save(f'{name}.json', json.dumps(summary[-1], indent=2) + '\n')
        print(f'{name}: {status}: {reason}', flush=True)
    counts = {state: sum(row['status'] == state for row in summary)
              for state in ('KILLED', 'SURVIVED', 'INVALID', 'SKIPPED')}
    save('summary.json', json.dumps({'controls': summary, 'counts': counts}, indent=2) + '\n')
    print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()), flush=True)
    # Every selected case must be accounted for and killed or honestly
    # skipped for a platform the bound parent cannot exist on; never stop at
    # the first failure, and never exit 0 while any case survived, was
    # invalid, or left nothing verified.
    return int(not summary
               or counts['KILLED'] == 0
               or any(row['status'] not in ('KILLED', 'SKIPPED')
                      for row in summary))


if __name__ == '__main__':
    sys.exit(main())
