#!/usr/bin/env python3
"""#1736 / #1739 / #1792 / #1794 HEAD-only controls on the shared suite
runner (negative_suite.run_cases): each mutation must be killed by its named
test's own panic marker, then the tree is restored and re-verified.

Usage: negative_controls.py --check
       negative_controls.py NEW_OUT [mutation ...]
``CARGO_TARGET_DIR`` is caller-owned. Every selected row is reported,
including anchor/build/source-guard failures.

Rows bind one mutation to one named test in ``config::store::tests`` (a lib
test, run with ``--exact``); the kill evidence is that test's own panic
payload carrying its ``#1736`` / ``#1739`` / ``#1792`` / ``#1794`` marker.
Four rows pin the watcher bounds (the debounce ceiling through both the
watcher-level test and the ledger's literal two-hour pin, the poll-interval
ceiling, and the ``checked_add`` overflow fallback in ``deadline_after``);
three pin the metadata short-circuit (#1739: the skipped content read, the
ctime bump that carries a same-length mtime-restored rewrite through it at
both the watcher and the unit level); five pin the #1792 recovery contract
(an incomplete sample must not commit the metadata fingerprint, at watch
start and mid-run; a ``NotFound`` stat or ``read_dir`` is a complete
observation of absence at both the entry and the directory level; a failed
startup probe must not drop the watched path) plus the #1736 pair that must
be restored together (debounce ceiling + unchecked deadline add — the
double-restoration regression that kills the watcher task).

``PDRS_NEGATIVE_BASELINE`` may name another commit object (the sandbox
verification pass pins a ``git stash create`` commit of the reviewed tree,
because the sandbox cannot commit); unset it everywhere else. The ref is
resolved to a full SHA once, up front, and passed to the shared runner — the
bytes validated are the bytes the evidence names. A non-``HEAD`` baseline is
flagged as ``NON-HEAD BASELINE`` on stdout.
"""
import os
from pathlib import Path
import subprocess
import sys

# `python3 -I` does not stop bytecode writes; keep the shared harness imports
# from leaving a __pycache__ inside the fixture tree.
sys.dont_write_bytecode = True

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_suite import Case, run_cases

root = Path(__file__).resolve().parents[5]
source = root / 'src/pilotdeck-gateway/src/config/store.rs'
TESTS = 'config::store::tests::'
mutations = [
    # #1736: the debounce ceiling is removed, so a representable over-long
    # value passes through and the reload lands at the requested two hours
    # instead of the clamped one — the bound test's "inside the bound"
    # assertion fails.
    ('debounce-ceiling-removed', source,
     'fn effective_debounce(options: &WatchOptions) -> Duration {\n'
     '    options.debounce.min(MAX_DEBOUNCE)\n'
     '}',
     'fn effective_debounce(options: &WatchOptions) -> Duration {\n'
     '    options.debounce\n'
     '}',
     TESTS + 'overlong_debounce_is_clamped_and_reloads_within_the_bound',
     '#1736 an overlong debounce lands on MAX_DEBOUNCE, not on the requested value'),
    # #1794 P1-2: the same clamp removal, bound to the ledger's literal
    # two-hour value instead of `2 * MAX_DEBOUNCE` — raising the ceiling to
    # Node's own `2^31 - 1` ms limit is the entry's convergence path and this
    # row is what turns that from a silent behaviour change into a red test.
    ('two-hour-debounce-ceiling-removed', source,
     'fn effective_debounce(options: &WatchOptions) -> Duration {\n'
     '    options.debounce.min(MAX_DEBOUNCE)\n'
     '}',
     'fn effective_debounce(options: &WatchOptions) -> Duration {\n'
     '    options.debounce\n'
     '}',
     TESTS + 'two_hour_debounce_is_clamped_to_the_one_hour_ceiling',
     '#1794 a two-hour debounce the baseline honours verbatim lands on the one-hour ceiling'),
    # #1736: the poll-interval ceiling is removed (back to #1650's floor-only
    # clamp), so a `Duration::MAX` period hits tokio's `Instant::far_future`
    # fallback — the ticker fires once and never again, the change written
    # after watcher start is never sampled, and no reload happens.
    ('poll-ceiling-removed', source,
     'fn effective_poll_interval(options: &WatchOptions) -> Duration {\n'
     '    options\n'
     '        .poll_interval\n'
     '        .clamp(MIN_POLL_INTERVAL, MAX_POLL_INTERVAL)\n'
     '}',
     'fn effective_poll_interval(options: &WatchOptions) -> Duration {\n'
     '    options.poll_interval.max(MIN_POLL_INTERVAL)\n'
     '}',
     TESTS + 'huge_poll_interval_is_clamped_and_keeps_sampling',
     '#1736 a huge poll interval is clamped and the change is still sampled'),
    # #1736: `deadline_after` adds unchecked again, so an unrepresentable
    # duration panics inside the bound test's `catch_unwind` and the expect
    # on its result fails at the #1736 marker.
    ('deadline-add-unchecked', source,
     '    now.checked_add(duration)\n'
     '        .unwrap_or_else(|| now.checked_add(MAX_DEBOUNCE).unwrap_or(now))',
     '    now + duration',
     TESTS + 'deadline_after_handles_unrepresentable_durations',
     '#1736 deadline_after must not panic on an unrepresentable duration'),
    # #1736/#1793: the watcher-level double restoration. Each half alone is
    # caught by its own row above (`debounce-ceiling-removed` only redirects
    # an over-ceiling debounce past the one-hour clamp, `deadline-add-unchecked`
    # only guards the direct call with an already-unrepresentable duration);
    # removing both at once lets the raw `Duration::MAX` reach the unchecked
    # add, the watcher task panics, and the hot reload dies silently. One
    # anchor spanning both functions keeps the pair mutated together.
    ('overflowing-debounce-double-restore', source,
     'fn effective_debounce(options: &WatchOptions) -> Duration {\n'
     '    options.debounce.min(MAX_DEBOUNCE)\n'
     '}\n'
     '\n'
     '/// The debounce deadline for a caller-supplied duration: an unrepresentable\n'
     '/// `now + duration` falls back to the ceiling — or to `now` itself if even\n'
     '/// that overflows, an `Instant` within one hour of the clock\'s maximum —\n'
     '/// never panics (#1736). With [`effective_debounce`] in front this is the\n'
     '/// second line of defence, not the first; its own inner fallback\n'
     '/// (`checked_add(MAX_DEBOUNCE).unwrap_or(now)`) is unreachable from the\n'
     '/// watcher path, where every duration arrives already clamped to ≤ 1 h with\n'
     '/// `now` far below the clock\'s maximum — it only guards a direct call with\n'
     '/// an unclamped `Duration::MAX` (the negative control `deadline-add-unchecked`\n'
     '/// binds that direct-call test), so do not read it as a hot-path defence.\n'
     'fn deadline_after(now: Instant, duration: Duration) -> Instant {\n'
     '    now.checked_add(duration)\n'
     '        .unwrap_or_else(|| now.checked_add(MAX_DEBOUNCE).unwrap_or(now))\n'
     '}',
     'fn effective_debounce(options: &WatchOptions) -> Duration {\n'
     '    options.debounce\n'
     '}\n'
     '\n'
     '/// The debounce deadline for a caller-supplied duration: an unrepresentable\n'
     '/// `now + duration` falls back to the ceiling — or to `now` itself if even\n'
     '/// that overflows, an `Instant` within one hour of the clock\'s maximum —\n'
     '/// never panics (#1736). With [`effective_debounce`] in front this is the\n'
     '/// second line of defence, not the first; its own inner fallback\n'
     '/// (`checked_add(MAX_DEBOUNCE).unwrap_or(now)`) is unreachable from the\n'
     '/// watcher path, where every duration arrives already clamped to ≤ 1 h with\n'
     '/// `now` far below the clock\'s maximum — it only guards a direct call with\n'
     '/// an unclamped `Duration::MAX` (the negative control `deadline-add-unchecked`\n'
     '/// binds that direct-call test), so do not read it as a hot-path defence.\n'
     'fn deadline_after(now: Instant, duration: Duration) -> Instant {\n'
     '    now + duration\n'
     '}',
     TESTS + 'overflowing_debounce_does_not_kill_the_watcher',
     '#1736 the watcher task must survive an unrepresentable debounce'),
    # #1739: the metadata short-circuit is removed, so every sample reads the
    # config file's content again and the read-count assertion fails.
    ('metadata-short-circuit-removed', source,
     '        if metadata_status == SampleStatus::Complete\n'
     '            && self.metadata_fingerprint == Some(next_metadata)\n'
     '        {\n'
     '            return false;\n'
     '        }\n'
     '        let (next, status) = fingerprint(&self.path, directory, self.target.as_deref(), true);\n',
     '        let (next, status) = fingerprint(&self.path, directory, self.target.as_deref(), true);\n',
     TESTS + 'unchanged_metadata_skips_the_content_read',
     '#1739 50 metadata-identical samples must not read the config file content'),
    # #1739: ctime is dropped from the metadata tuple, so a same-length rewrite
    # with mtime restored leaves the metadata sample identical and the
    # short-circuit hides the change — bound at both levels: the watcher-level
    # e2e test and the unit-level `changed()` + read-count pin.
    ('ctime-dropped-from-metadata', source,
     '            (meta.ctime(), meta.ctime_nsec(), meta.ino(), meta.dev()).hash(hasher);',
     '            (meta.ino(), meta.dev()).hash(hasher);',
     TESTS + 'restored_mtime_rewrite_between_samples_is_seen',
     '#1739 a same-length rewrite with mtime restored is still seen (ctime)'),
    ('ctime-dropped-from-metadata-unit', source,
     '            (meta.ctime(), meta.ctime_nsec(), meta.ino(), meta.dev()).hash(hasher);',
     '            (meta.ino(), meta.dev()).hash(hasher);',
     TESTS + 'restored_mtime_rewrite_still_reads_the_content',
     '#1739 the ctime bump is seen through the metadata short-circuit'),
    # #1792: the incomplete sample commits its metadata fingerprint anyway, so
    # identical metadata short-circuits every later sample — the read that
    # failed is never retried and the recovery assertion (no metadata change,
    # change re-reported once reads recover) fails.
    ('incomplete-sample-committed', source,
     '        self.metadata_fingerprint = (metadata_status == SampleStatus::Complete\n'
     '            && status == SampleStatus::Complete)\n'
     '            .then_some(next_metadata);',
     '        self.metadata_fingerprint = Some(next_metadata);',
     TESTS + 'transient_read_failure_is_retried_until_the_content_is_seen',
     '#1792 the read is retried after recovery with no metadata change'),
    # #1792: the startup sample commits its metadata fingerprint even when the
    # content read failed, so the tick after recovery short-circuits before
    # ever reading the content the startup could not.
    ('startup-incomplete-committed', source,
     '        let metadata_fingerprint = (metadata_status == SampleStatus::Complete\n'
     '            && status == SampleStatus::Complete)\n'
     '            .then_some(metadata);',
     '        let metadata_fingerprint = Some(metadata);',
     TESTS + 'startup_read_failure_is_retried_until_the_content_is_seen',
     '#1792 an incomplete startup sample commits no metadata fingerprint'),
    # #1792 r1: any stat error — not just `NotFound` — counts as a complete
    # observation, so an injected `PermissionDenied` stat failure during a
    # watched file's sample commits the folded metadata fingerprint and
    # reports the folded full fingerprint as a change; both halves of the
    # bound assertion fail (the first at its tick-0 message).
    ('stat-failure-counted-complete', source,
     '            if error.kind() == std::io::ErrorKind::NotFound {\n'
     '                SampleStatus::Complete\n'
     '            } else {\n'
     '                SampleStatus::Obscured\n'
     '            }',
     '            if true {\n'
     '                SampleStatus::Complete\n'
     '            } else {\n'
     '                SampleStatus::Obscured\n'
     '            }',
     TESTS + 'stat_failure_leaves_the_metadata_fingerprint_uncommitted',
     '#1792 a stat-failed sample 0 reports no change'),
    # #1792 r1: any read_dir error counts as a complete observation of an
    # empty directory, so an injected `PermissionDenied` read_dir failure
    # commits the folded metadata fingerprint and reports the folded full
    # fingerprint as a change.
    ('read-dir-failure-counted-complete', source,
     '                if error.kind() != std::io::ErrorKind::NotFound {\n'
     '                    status = status.merge(SampleStatus::Obscured);\n'
     '                }',
     '                if false {\n'
     '                    status = status.merge(SampleStatus::Obscured);\n'
     '                }',
     TESTS + 'read_dir_failure_leaves_the_metadata_fingerprint_uncommitted',
     '#1792 a read_dir-failed sample 0 reports no change'),
    # #1792 r1: a failed startup probe drops the watched path again (the
    # pre-r1 `return None`), so `WatchedPath::new` yields `None` and the
    # bound test's expect fails at its own message.
    ('startup-probe-failure-dropped', source,
     '        Err(_) => Some(Self::probing(path, None)),',
     '        Err(_) => None,',
     TESTS + 'startup_stat_failure_keeps_the_path_watched',
     '#1792 a stat failure at startup must not drop the path'),
]


def check_anchor(row, original):
    if original.count(row[2]) != 1 or row[2] == row[3]:
        raise ValueError(f'{row[0]}: stale or unchanged anchor (count={original.count(row[2])})')


def mutate(content, scope, replacements):
    for old, new in replacements:
        if content.count(old) != 1 or old == new:
            raise RuntimeError('#1736/#1739/#1792/#1794 expired or ineffective mutation anchor')
        content = content.replace(old, new, 1)
    return content


def main():
    args = sys.argv[1:]
    if args == ['--check']:
        errors = []
        names = [row[0] for row in mutations]
        duplicates = sorted({name for name in names if names.count(name) > 1})
        if duplicates:
            errors.append(f'duplicate row names (by-name selection needs unique names): {duplicates}')
        for row in mutations:
            try:
                check_anchor(row, row[1].read_text())
            except Exception as error:
                errors.append(str(error))
        if errors:
            print('\n'.join(errors))
            return 1
        distinct = len({(str(row[1]), row[2]) for row in mutations})
        print(f'#1736/#1739/#1792/#1794 {len(mutations)} rows / {distinct} distinct anchors checked')
        return 0
    if not args:
        raise SystemExit('provide NEW_OUT and optional mutation names, or --check')
    names = args[1:]
    if names:
        # Requested names first: a duplicate request would silently run one
        # row per name twice (or, with a duplicate row name, expand one name
        # into several rows) — both refuse rather than misreport.
        duplicates = sorted({name for name in names if names.count(name) > 1})
        if duplicates:
            raise SystemExit(f'duplicate mutation names requested: {duplicates}')
        known = {row[0] for row in mutations}
        unknown = [name for name in names if name not in known]
        if unknown:
            raise SystemExit(f'unknown mutation names: {unknown}')
        wanted = set(names)
        selected = [row for row in mutations if row[0] in wanted]
        if len(selected) != len(wanted):
            raise SystemExit('unknown or duplicate mutation')
    else:
        selected = list(mutations)
    env = dict(os.environ)
    if not env.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
    # cb8's zh_CN default locale has produced false reds in libtest output;
    # an explicit caller choice is never overridden.
    env.setdefault('LANG', 'en_US.UTF-8')
    out = Path(args[0]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    # No caller-supplied mutable ref: resolve the baseline once, up front, and
    # hand the immutable SHA to the shared runner — the bytes validated are
    # the bytes the evidence names.
    baseline_ref = os.environ.get('PDRS_NEGATIVE_BASELINE') or 'HEAD'
    resolved = subprocess.run(
        ['git', '-C', str(root), 'rev-parse', '--verify', baseline_ref + '^{commit}'],
        capture_output=True, check=False)
    if resolved.returncode != 0:
        raise SystemExit('cannot resolve baseline '
                         f'{baseline_ref!r}: {resolved.stderr.decode(errors="replace").strip()}')
    baseline_sha = resolved.stdout.decode().strip()
    print(f'baseline: {baseline_sha}', flush=True)
    if baseline_ref != 'HEAD':
        print(f'NON-HEAD BASELINE: {baseline_sha} via PDRS_NEGATIVE_BASELINE; '
              'the host gate only trusts HEAD evidence', flush=True)
    bound_cases = [Case(name, path, old, new, test, assertion)
                   for name, path, old, new, test, assertion in selected]
    return run_cases(root, bound_cases, out, env, mutate,
                     timeout=900, baseline=baseline_sha, script=__file__)


if __name__ == '__main__':
    raise SystemExit(main())
