#!/usr/bin/env python3
"""#1736 / #1739 / #1792 / #1794 / #1899 / #1920 / #1987 / #2122 HEAD-only
controls on the shared suite runner (negative_suite.run_cases): each mutation
must be killed by its named test's own panic marker, then the tree is restored
and re-verified.

Usage: negative_controls.py --check
       negative_controls.py NEW_OUT [mutation ...]
``CARGO_TARGET_DIR`` is caller-owned. Every selected row is reported,
including anchor/build/source-guard failures.

Rows bind one mutation to one named test in ``config::store::tests`` (a lib
test, run with ``--exact``); the kill evidence is that test's own panic
payload carrying its ``#1736`` / ``#1739`` / ``#1792`` / ``#1794`` /
``#1899`` / ``#1987`` marker. Four rows pin the watcher bounds (the debounce
ceiling through both the
watcher-level test and the ledger's literal two-hour pin, the poll-interval
ceiling, and the ``checked_add`` overflow fallback in ``deadline_after``);
three pin the metadata short-circuit (#1739: the skipped content read, the
ctime bump that carries a same-length mtime-restored rewrite through it at
both the watcher and the unit level); five pin the #1792 recovery contract
(an incomplete sample must not commit the metadata fingerprint, at watch
start and mid-run; a non-``NotFound`` stat or ``read_dir`` failure is not a
complete observation at the entry and the directory level; a failed startup
probe must not drop the watched path); eight pin the #1792 r2 rules (the
first complete sample after a failed startup probe must report a change
made during the failure; an obscured sample commits nothing — not the
metadata shortcut, not the folded fingerprint — and is compared only
against the previous obscured sample, so a delete during a stat outage is
seen and a persistent obstruction does not silence the directory; a
``NotFound`` on the unclassified re-probe must not fix the mode to file;
the per-tick type probe that keeps a directory-replaced-by-a-file watch
alive; a reported change must not commit the metadata shortcut; a
``NotFound`` stat or ``read_dir`` is a complete observation at the entry
and the directory level); six pin #1987's watcher windows (the first
obscured tick after a complete one still compares the watched config file
against its last complete fingerprint; the startup takes one full sample
and commits no metadata shortcut; a mixed tick is a complete observation,
not an obscured one; an obscured startup sample commits the folded value
into the obscured chain only; a ``read_dir`` iteration error mid-listing
obscures the sample; a failed startup probe on the parent of a missing
config file keeps that parent watched); four pin #2122's discrimination
gaps (the second obscured tick re-reporting the same state — the B6 test's
armed entry budget used to run out one tick early; the startup committing a
shortcut again — the A2 assertion only binds before the first ``changed()``;
a mixed tick whose folded content read failed routed as complete again; a
truncated listing that never reached the target read as vanished) and seven
pin its fix-r1 (the unknown-read twin gate disabled — the content-unknown
fold falling through to the complete path again — and widened — the one
report a changed config is owed while the read keeps failing swallowed; both
obscured guards widened back to the F5 first cut, on the between-samples
path and on the mixed one; the twin not committed by a reported complete
sample; the metadata shortcut surviving a quiet content-unknown tick, so
the read is never retried; the startup sample not seeding the twin); fourteen
pin the #1899/#1920
watcher-stop evidence (the
join-based check reverted to ``JoinHandle::is_finished()``, which tokio also
reports for a task that died by panic; each of the three death shapes — a
panicking watcher loop, one that returns cleanly right after spawn, one that
exits on its first in-loop tick — bound against the handle half of the
split drop test, against the store half and against the stop test; one that
never stops at all; #1920's own regression — ``WatchHandle::stop``
swallowing the ``JoinError`` again, so a watcher that died by panic is
reported as a clean stop; the same triage relabeling the loop's own clean
return as ``Cancelled``; and the triage's else arm — ``stop`` called after
the task was already taken out of the handle — silenced back into a quiet
``Finished`` return — all of them are #1650's silent-death path) plus
the #1736 pair that must be restored together (debounce ceiling + unchecked
deadline add — the double-restoration regression that kills the watcher
task).

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
tests = root / 'src/pilotdeck-gateway/src/config/store/tests.rs'
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
    # config file's content again and the read-count assertion fails. #1792
    # r2: the shortcut is also skipped while the mode is unclassified (D10).
    # #1987 A4 moved the guard into the else arm of the metadata sample's
    # status split; the anchor follows.
    ('metadata-short-circuit-removed', source,
     '        } else {\n'
     '            if !unclassified\n'
     '                && metadata_status == SampleStatus::Complete\n'
     '                && self.metadata_fingerprint == Some(next_metadata)\n'
     '            {\n'
     '                return false;\n'
     '            }\n'
     '            fingerprint(&self.path, directory, self.target.as_deref(), true)\n'
     '        };\n',
     '        } else {\n'
     '            fingerprint(&self.path, directory, self.target.as_deref(), true)\n'
     '        };\n',
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
    # change re-reported once reads recover) fails. #1792 r2: the commit
    # condition also requires a quiet, classified tick (D10/D11).
    ('incomplete-sample-committed', source,
     '        self.metadata_fingerprint = if changed\n'
     '            || unclassified\n'
     '            || metadata_status != SampleStatus::Complete\n'
     '            || status != SampleStatus::Complete\n'
     '        {\n'
     '            None\n'
     '        } else {\n'
     '            Some(next_metadata)\n'
     '        };',
     '        self.metadata_fingerprint = Some(next_metadata);',
     TESTS + 'transient_read_failure_is_retried_until_the_content_is_seen',
     '#1792 the read is retried after recovery with no metadata change'),
    # #1792: the startup sample commits its metadata fingerprint even when the
    # content read failed, so the tick after recovery short-circuits before
    # ever reading the content the startup could not. #1987 A2 removed the
    # startup metadata commit outright; the mutation restores one — the
    # metadata-only sample is taken before the full one's `fingerprint`
    # binding shadows the function, and its value lands in the shortcut slot.
    ('startup-incomplete-committed', source,
     '        let (fingerprint, status, target_sample, twin) =\n'
     '            fingerprint(&path, directory, target.as_deref(), true);\n'
     '        // #1987 A2 test hook: fires right after the startup sample. Pre-fix\n'
     '        // it fired between the metadata-only and the full sample; the\n'
     '        // negative control restores that order around it.\n'
     '        startup_sample_gate(&path);\n'
     '        // #1792 r2: a startup sample that is already obscured commits the\n'
     '        // folded sample to the obscured slot only, so the first complete\n'
     '        // sample after recovery reports (there is no trustworthy previous\n'
     '        // full sample to compare against).\n'
     '        let (fingerprint, obscured_fingerprint) = if status == SampleStatus::Obscured {\n'
     '            (None, Some(fingerprint))\n'
     '        } else {\n'
     '            (Some(fingerprint), None)\n'
     '        };\n'
     '        Self {\n'
     '            path,\n'
     '            directory: Some(directory),\n'
     '            target,\n'
     '            fingerprint,\n'
     '            // #1792 keeps applying: an incomplete startup sample (any stat,\n'
     '            // `read_dir`, or content read that failed) commits no metadata\n'
     '            // fingerprint, and after #1987 A2 a complete one does not either.\n'
     '            metadata_fingerprint: None,\n'
     '            obscured_fingerprint,\n',
     '        let metadata = fingerprint(&path, directory, target.as_deref(), false).0;\n'
     '        let (fingerprint, status, target_sample, twin) =\n'
     '            fingerprint(&path, directory, target.as_deref(), true);\n'
     '        // #1987 A2 test hook: fires right after the startup sample. Pre-fix\n'
     '        // it fired between the metadata-only and the full sample; the\n'
     '        // negative control restores that order around it.\n'
     '        startup_sample_gate(&path);\n'
     '        // #1792 r2: a startup sample that is already obscured commits the\n'
     '        // folded sample to the obscured slot only, so the first complete\n'
     '        // sample after recovery reports (there is no trustworthy previous\n'
     '        // full sample to compare against).\n'
     '        let (fingerprint, obscured_fingerprint) = if status == SampleStatus::Obscured {\n'
     '            (None, Some(fingerprint))\n'
     '        } else {\n'
     '            (Some(fingerprint), None)\n'
     '        };\n'
     '        Self {\n'
     '            path,\n'
     '            directory: Some(directory),\n'
     '            target,\n'
     '            fingerprint,\n'
     '            metadata_fingerprint: Some(metadata),\n'
     '            obscured_fingerprint,\n',
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
     '                    listing_obscured = true;\n'
     '                }',
     '                if false {\n'
     '                    listing_obscured = true;\n'
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
    # #1792 r2 (D8): the first complete sample after a failed startup probe
    # only records again (`None` read as "unchanged from something"), so a
    # change made while the probe was failing is never reported.
    ('startup-recovery-not-reported', source,
     '        let changed = previous != Some(next);',
     '        let changed = previous.is_some_and(|previous| previous != next);',
     TESTS + 'startup_stat_failure_keeps_the_path_watched',
     '#1792 r2 the first complete sample after a failed probe reports the change made during the failure'),
    # #1792 r2 (D9): an obscured sample commits its folded fingerprint again
    # (the r1 rule). The folded fingerprint hashes a failed stat exactly like
    # a missing entry, so a delete made during the outage compares equal and
    # is never reported. #2122 F6: the obscured-tick body the mutation
    # corrupts is now the one shared helper; the anchor is the two code lines
    # of the first call site (12-space indent — the between-samples twin sits
    # at 8), no comment text, so a comment edit cannot expire it. #2122 fix-r1:
    # the guard is `== Obscured` now (a content-unknown fold must not enter
    # the obscured chain); the mutant keeps routing obscured ticks there.
    ('recovery-trusts-folded-fingerprint', source,
     '            if status == SampleStatus::Obscured {\n'
     '                return self.observe_obscured_tick(next, target_sample);\n',
     '            if status == SampleStatus::Obscured {\n'
     '                let changed = self.observe_obscured_tick(next, target_sample);\n'
     '                self.fingerprint = Some(next_metadata);\n'
     '                return changed;\n',
     TESTS + 'stat_failure_during_change_is_seen_after_recovery',
     '#1792 r2 a delete made during a stat outage is seen after recovery'),
    # #1792 r2 (D10): a `NotFound` on the unclassified re-probe fixes the mode
    # to file again (the r1 bug), so the path is never re-classified when it
    # appears as a directory.
    ('probe-not-found-fixes-file-mode', source,
     '                Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,',
     '                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {\n'
     '                    self.directory = Some(false);\n'
     '                    false\n'
     '                }',
     TESTS + 'probe_failure_then_directory_is_reclassified',
     '#1792 r2 a NotFound re-probe must not fix the mode to file'),
    # #1792 r2 (D11): the tick that reports a change also commits the metadata
    # shortcut again, so the next tick short-circuits without re-reading the
    # content — the two samples happen at different times.
    ('report-commits-metadata-shortcut', source,
     '        self.metadata_fingerprint = if changed\n'
     '            || unclassified',
     '        self.metadata_fingerprint = if unclassified',
     TESTS + 'a_reported_change_does_not_commit_the_metadata_shortcut',
     '#1792 r2 the reported tick commits no metadata shortcut'),
    # #1792 r2 (D15): the per-tick type probe is removed, so a watched
    # directory replaced by a plain file stays in directory mode and every
    # tick is obscured (`read_dir` ENOTDIR) — no reload ever again.
    ('mode-change-not-detected', source,
     '        if let Some(directory) = self.directory\n'
     '            && let Ok(meta) = stat_path(&self.path)\n'
     '            && meta.is_dir() != directory\n'
     '        {\n'
     '            self.directory = Some(meta.is_dir());\n'
     '            self.metadata_fingerprint = None;\n'
     '            // #1987 A1: the mode flip changes what the target\'s fingerprint\n'
     '            // covers; a seed sampled in the other mode is not comparable. The\n'
     '            // unknown-read twin (#2122 fix-r1) is folded over the same\n'
     '            // mode-shaped fields, so it goes with it.\n'
     '            self.complete_target_fingerprint = None;\n'
     '            self.content_unknown_twin = None;\n'
     '        }\n',
     '',
     TESTS + 'a_directory_replaced_by_a_file_is_seen',
     '#1792 r2 a directory replaced by a file is seen'),
    # #1792 r2 (D16): an obscured sample never reports against the previous
    # obscured one, so a permanently unreadable sibling entry silences every
    # observable change in the watched directory.
    ('obscured-change-not-reported', source,
     '        let previous = self.obscured_fingerprint.replace(sample);\n'
     '        previous.is_some_and(|previous| previous != sample)',
     '        let previous = self.obscured_fingerprint.replace(sample);\n'
     '        { let _ = previous; false }',
     TESTS + 'a_persistent_obstruction_still_reports_observable_changes',
     '#1792 r2 a persistent obstruction must not silence observable changes'),
    # #1792 r2 (D17): a `read_dir` `NotFound` is counted as obscured again, so
    # a vanished watched directory is not a complete observation and its
    # disappearance is never reported.
    ('read-dir-not-found-counted-obscured', source,
     '                if error.kind() != std::io::ErrorKind::NotFound {\n'
     '                    listing_obscured = true;\n'
     '                }',
     '                listing_obscured = true;',
     TESTS + 'a_removed_directory_is_a_complete_observation',
     '#1792 r2 a vanished directory is a complete observation and reports once'),
    # #1792 r2 (D13, DeepSeek sibling P2): a `NotFound` stat is counted as
    # obscured, so a dangling entry keeps the sample incomplete forever and
    # the metadata fingerprint never commits. The pre-existing
    # `stat-failure-counted-complete` row mutates the same branch the other
    # way and cannot kill the bound test; this row does.
    ('not-found-counted-obscured', source,
     '            if error.kind() == std::io::ErrorKind::NotFound {\n'
     '                SampleStatus::Complete\n'
     '            } else {\n'
     '                SampleStatus::Obscured\n'
     '            }',
     '            if error.kind() == std::io::ErrorKind::NotFound {\n'
     '                SampleStatus::Obscured\n'
     '            } else {\n'
     '                SampleStatus::Obscured\n'
     '            }',
     TESTS + 'missing_entry_is_a_complete_observation',
     '#1792 a dangling entry is a complete observation, not a permanent None'),
    # #1899: the watcher-stop evidence goes back to `JoinHandle::is_finished()`,
    # which tokio 1.48 also reports for a task that died by panic (the harness
    # stores `JoinError::panic` and marks the task complete), so the regression
    # test's `expect_err` fails at its own marker: a panicked watcher is again
    # accepted as a clean stop — #1650's silent hot-reload death.
    ('is-finished-stop-evidence-restored', tests,
     '    match tokio::time::timeout(STOP_TIMEOUT, task).await {\n'
     '        Err(_) => Err(format!(\n'
     '            "the watcher task did not stop within {STOP_TIMEOUT:?}"\n'
     '        )),\n'
     '        Ok(Ok(())) => Ok(()),\n'
     '        Ok(Err(err)) if err.is_panic() => Err(format!("the watcher task panicked: {err}")),\n'
     '        Ok(Err(err)) => Err(format!("the watcher task was cancelled: {err}")),\n'
     '    }',
     '    if task.is_finished() {\n'
     '        Ok(())\n'
     '    } else {\n'
     '        Err("the watcher task did not stop".to_owned())\n'
     '    }',
     TESTS + 'panicked_watcher_task_is_not_a_clean_stop',
     '#1899 a panicked watcher must not look like a clean stop'),
    # #1899: the watcher loop panics on its first poll — the #1650 silent-death
    # path. The drop test must fail instead of passing on a task that is merely
    # "finished": the injected panic kills it at the first liveness check, and
    # the later ones are listed because they are the same defect seen one step
    # apart (`classify` accepts the first marker that shows up). #1920: the old
    # two-in-one drop test is split (`dropping_the_handle_ends_the_task` /
    # `dropping_the_store_ends_the_task`); this row keeps the handle half. The
    # stop-test twin is `watch-loop-panic-injected-before-stop`.
    ('watch-loop-panic-injected', source,
     '    let mut ticker = tokio::time::interval(effective_poll_interval(&options));',
     '    panic!("#1899 injected: simulated watch_loop failure");\n'
     '    let mut ticker = tokio::time::interval(effective_poll_interval(&options));',
     TESTS + 'dropping_the_handle_ends_the_task',
     '#1899 the watcher must still be running before the handle is dropped'),
    # #1920 fix-r1 (Grok sibling#1): the split left the panic death shape
    # bound to the handle half only — the store half's own liveness
    # precondition had no panic mutant, so a watcher-death-by-panic that only
    # the store half would have caught went unguarded. This row restores the
    # store-half marker the old combined row used to list.
    ('watch-loop-panic-injected-store-half', source,
     '    let mut ticker = tokio::time::interval(effective_poll_interval(&options));',
     '    panic!("#1899 injected: simulated watch_loop failure");\n'
     '    let mut ticker = tokio::time::interval(effective_poll_interval(&options));',
     TESTS + 'dropping_the_store_ends_the_task',
     '#1899 the watcher must still be running before the store is dropped'),
    # #1899 review P2-2: a watcher that returns cleanly right after spawn also
    # joins with `Ok(())`, so the drop-the-store evidence only means something
    # once the watcher is proven alive before the drop. This mutant is that
    # counterexample — the silent-death shape #1650 is about, with a clean exit
    # instead of a panic — and it must die at the liveness assertion. #1920:
    # the split keeps this row on the handle half; the stop-test twin is
    # `watch-loop-clean-early-exit-before-stop`.
    ('watch-loop-clean-early-exit', source,
     '    let mut ticker = tokio::time::interval(effective_poll_interval(&options));',
     '    return;\n'
     '    let mut ticker = tokio::time::interval(effective_poll_interval(&options));',
     TESTS + 'dropping_the_handle_ends_the_task',
     '#1899 the watcher must still be running before the handle is dropped'),
    # #1920 fix-r1 (Grok sibling#2): the split's cross-assignment left the
    # store half with the exits-after-first-tick and never-stops shapes only
    # — the spawn-time clean return had no row against it. This row restores
    # the store-half marker the old combined row used to list.
    ('watch-loop-clean-early-exit-store-half', source,
     '    let mut ticker = tokio::time::interval(effective_poll_interval(&options));',
     '    return;\n'
     '    let mut ticker = tokio::time::interval(effective_poll_interval(&options));',
     TESTS + 'dropping_the_store_ends_the_task',
     '#1899 the watcher must still be running before the store is dropped'),
    # #1899 review P2 (boundary): a liveness check taken at spawn time proves
    # nothing about a watcher that returns on its first in-loop tick — still
    # #1650's silent death, one tick later. Each check advances one poll interval
    # first, and this mutant is what that advance is there to catch. #1920: the
    # split moves this row to the store half; fix-r1 adds the handle-half twin
    # below so both halves keep every death shape (the old combined row
    # listed both halves' markers).
    ('watch-loop-exits-after-first-tick', source,
     '                if inner.strong_count() == 0 {\n'
     '                    return;\n'
     '                }\n'
     '                let mut changed = false;',
     '                if inner.strong_count() == 0 {\n'
     '                    return;\n'
     '                }\n'
     '                return;\n'
     '                let mut changed = false;',
     TESTS + 'dropping_the_store_ends_the_task',
     '#1899 the watcher must still be running before the store is dropped'),
    # #1920 fix-r1 (Grok sibling#2): the mirror gap of the row above — the
    # handle half was left with the panic and clean-early-exit shapes only,
    # so a watcher that died right after its first in-loop tick passed the
    # handle-half drop test's liveness window unguarded. This row restores
    # the handle-half marker the old combined row used to list.
    ('watch-loop-exits-after-first-tick-handle-half', source,
     '                if inner.strong_count() == 0 {\n'
     '                    return;\n'
     '                }\n'
     '                let mut changed = false;',
     '                if inner.strong_count() == 0 {\n'
     '                    return;\n'
     '                }\n'
     '                return;\n'
     '                let mut changed = false;',
     TESTS + 'dropping_the_handle_ends_the_task',
     '#1899 the watcher must still be running before the handle is dropped'),
    # #1899 review P2 (sibling): the same mutant against `WatchHandle::stop`,
    # which before #1920 aborted and swallowed the `JoinError` — `weak_count
    # == 0` and "no reload" alone would accept a watcher that died before
    # the stop.
    ('watch-loop-exits-after-first-tick-before-stop', source,
     '                if inner.strong_count() == 0 {\n'
     '                    return;\n'
     '                }\n'
     '                let mut changed = false;',
     '                if inner.strong_count() == 0 {\n'
     '                    return;\n'
     '                }\n'
     '                return;\n'
     '                let mut changed = false;',
     TESTS + 'stop_cancels_pending_reload_and_leaves_no_task',
     '#1899 the watcher must still be running before stop'),
    # #1920: the two #1899 death shapes re-bound to the stop test, where the
    # "stopped" evidence is now `stop()`'s own report — the liveness check
    # fires first, and the #1920 stop-report marker is listed because the same
    # mutation reaching the join (a multi-thread runtime, a future refactor)
    # must be killed by the report assertion too, not only by the check.
    ('watch-loop-panic-injected-before-stop', source,
     '    let mut ticker = tokio::time::interval(effective_poll_interval(&options));',
     '    panic!("#1899 injected: simulated watch_loop failure");\n'
     '    let mut ticker = tokio::time::interval(effective_poll_interval(&options));',
     TESTS + 'stop_cancels_pending_reload_and_leaves_no_task',
     ['#1899 the watcher must still be running before stop',
      '#1920 stop must report the watcher alive until its own abort']),
    # #1920: the clean-exit twin of the row above, on the same reasoning —
    # a watcher that never watched a single tick must not pass the stop test
    # as though `stop` had ended it.
    ('watch-loop-clean-early-exit-before-stop', source,
     '    let mut ticker = tokio::time::interval(effective_poll_interval(&options));',
     '    return;\n'
     '    let mut ticker = tokio::time::interval(effective_poll_interval(&options));',
     TESTS + 'stop_cancels_pending_reload_and_leaves_no_task',
     ['#1899 the watcher must still be running before stop',
      '#1920 stop must report the watcher alive until its own abort']),
    # #1899 review P2 (concurrency): every other row dies before the join, so
    # the timeout arm of `watcher_stop` (and with it the "90 s clears the 60 s
    # poll ceiling" invariant) had no receipt. Removing the owner-gone return
    # makes the watcher keep ticking after `drop(store)`: the suspended clock
    # walks the whole 90 s at no wall-clock cost and the test fails at the
    # clean-stop assertion instead of hanging or being called a clean stop.
    # #1920: the split keeps this row on the store half, whose evidence is
    # the `watcher_stop` join.
    ('watch-loop-never-stops', source,
     '                if inner.strong_count() == 0 {\n'
     '                    return;\n'
     '                }\n'
     '                let mut changed = false;',
     '                let mut changed = false;',
     TESTS + 'dropping_the_store_ends_the_task',
     '#1899 dropping the store must stop the watcher cleanly'),
    # #1920: the stop triage itself regresses — `stop()` aborts, swallows the
    # `JoinError` and unconditionally reports the cancelled shape, the pre-fix
    # body. The hook-armed test then sees a watcher that died by panic reported
    # as a clean stop, and dies at its own marker: the exact #1650 silent
    # hot-reload death the triage exists to surface.
    ('stop-panic-swallowed-again', source,
     '        task.abort();\n'
     '        match task.await {\n'
     '            // Measured on this runtime (tokio 1.48): abort-then-join joins\n'
     '            // `Err(JoinError::Cancelled)` and a task that died by panic joins\n'
     '            // `Err(JoinError::Panic(..))`, so `is_panic()` — checked first,\n'
     '            // like #1899\'s `watcher_stop` — leaves `is_cancelled()` as the\n'
     '            // only other shape.\n'
     '            Err(err) if err.is_panic() => WatchStop::Panicked,\n'
     '            Err(_) => WatchStop::Cancelled,\n'
     '            Ok(()) => WatchStop::Finished,\n'
     '        }',
     '        task.abort();\n'
     '        let _ = task.await;\n'
     '        WatchStop::Cancelled',
     TESTS + 'stop_reports_a_watcher_that_died_by_panic',
     '#1920 stop must report a watcher that died by panic, not a clean stop'),
    # #1920 fix-r1 (DS sibling#1 / concurrency#2 / boundary#2 + Grok
    # boundary#2, four ways): the `Finished` arm had no positive test, so
    # this mutation — relabeling the loop's own clean return (the store was
    # dropped first, the owner check ended the loop) as `Cancelled` —
    # survived every existing row. The new positive test kills it at its own
    # marker.
    ('finished-reported-as-cancelled', source,
     '        task.abort();\n'
     '        match task.await {\n'
     '            // Measured on this runtime (tokio 1.48): abort-then-join joins\n'
     '            // `Err(JoinError::Cancelled)` and a task that died by panic joins\n'
     '            // `Err(JoinError::Panic(..))`, so `is_panic()` — checked first,\n'
     '            // like #1899\'s `watcher_stop` — leaves `is_cancelled()` as the\n'
     '            // only other shape.\n'
     '            Err(err) if err.is_panic() => WatchStop::Panicked,\n'
     '            Err(_) => WatchStop::Cancelled,\n'
     '            Ok(()) => WatchStop::Finished,\n'
     '        }',
     '        task.abort();\n'
     '        match task.await {\n'
     '            // Measured on this runtime (tokio 1.48): abort-then-join joins\n'
     '            // `Err(JoinError::Cancelled)` and a task that died by panic joins\n'
     '            // `Err(JoinError::Panic(..))`, so `is_panic()` — checked first,\n'
     '            // like #1899\'s `watcher_stop` — leaves `is_cancelled()` as the\n'
     '            // only other shape.\n'
     '            Err(err) if err.is_panic() => WatchStop::Panicked,\n'
     '            Err(_) => WatchStop::Cancelled,\n'
     '            Ok(()) => WatchStop::Cancelled,\n'
     '        }',
     TESTS + 'stop_reports_finished_after_loop_returns',
     '#1920 stop must report a watcher that returned on its own as Finished'),
    # #1920 fix-r1 (Grok boundary#1): `stop`'s else arm — the task was
    # already taken out of the handle — used to quietly return `Finished`,
    # relabeling an unobserved death as a clean self-return. The product now
    # debug-asserts the misuse instead; this row removes that assert so the
    # binding test must fail at its own marker.
    ('stop-misuse-silenced-again', source,
     '            debug_assert!(\n'
     '                false,\n'
     '                "#1920 stop after the task was already taken out of the handle"\n'
     '            );\n'
     '            return WatchStop::Finished;',
     '            return WatchStop::Finished;',
     TESTS + 'stop_after_the_task_was_taken_fails_loudly',
     '#1920 stop after the task was taken must not return normally'),
    # #1946: the join window. The baseline claims the in-flight slot
    # synchronously but starts the load on a microtask
    # (`Promise.resolve().then`), so `run_or_wait` hops once before the first
    # claim loads: a peer that lands in that window joins this flight instead
    # of starting a second load, and a same-tick snapshot read still sees the
    # previous generation. Removing the hop loads inline in the claiming poll;
    # the pinning test's first assertion reds (the poll comes back Ready).
    ('join-window-hop-removed', source,
     '                    FlightState::Pending if first && !inner.settled_in_turn() => hop = true,',
     '                    FlightState::Pending if first && !inner.settled_in_turn() => {\n'
     '                        *state = FlightState::Running(std::thread::current().id());\n'
     '                        drop(state);\n'
     '                        return self.execute(inner);\n'
     '                    }',
     TESTS + 'the_join_window_defers_the_load_past_the_claiming_poll',
     '#1946 the claiming poll must hop instead of loading inline'),
    # #1987 A1: the transition tick — the first obscured one after a complete
    # sample, with no folded obscured predecessor — no longer compares the
    # watched config file against its fingerprint from the last complete
    # sample, so an obstruction starting on the same tick as a target rewrite
    # defers the report until the obstruction clears.
    ('obscured-transition-target-not-compared', source,
     '    fn observe_target_on_transition(&self, sample: Option<u64>) -> bool {\n'
     '        match (self.complete_target_fingerprint, sample) {\n'
     '            (Some(previous), Some(current)) => previous != current,\n'
     '            _ => false,\n'
     '        }\n'
     '    }',
     '    fn observe_target_on_transition(&self, sample: Option<u64>) -> bool {\n'
     '        match (self.complete_target_fingerprint, sample) {\n'
     '            (Some(previous), Some(current)) => false,\n'
     '            _ => false,\n'
     '        }\n'
     '    }',
     TESTS + 'obscured_transition_reports_a_same_tick_target_rewrite',
     '#1987 A1 a target rewrite on the obstruction\'s first tick must be reported'),
    # #1987 A2: the startup takes a metadata-only sample before the full one
    # again, so a write landing between the two is absorbed by the full
    # sample while nothing it differs from is ever reported. #2122 P3: the row
    # used to be called ``startup-metadata-shortcut-committed`` — a name (and
    # README description) claiming more than the mutation does: the
    # replacement only restores the two-sample window, the tail still commits
    # no shortcut. Renamed to what it is.
    ('startup-two-sample-window', source,
     '        let (fingerprint, status, target_sample, twin) =\n'
     '            fingerprint(&path, directory, target.as_deref(), true);\n'
     '        // #1987 A2 test hook: fires right after the startup sample. Pre-fix\n'
     '        // it fired between the metadata-only and the full sample; the\n'
     '        // negative control restores that order around it.\n'
     '        startup_sample_gate(&path);\n',
     '        let (_, _, _, _) = fingerprint(&path, directory, target.as_deref(), false);\n'
     '        startup_sample_gate(&path);\n'
     '        let (fingerprint, status, target_sample, twin) =\n'
     '            fingerprint(&path, directory, target.as_deref(), true);\n',
     TESTS + 'a_write_between_the_startup_samples_is_reported',
     '#1987 A2 a write between the startup samples is reported'),
    # #1987 A4: a mixed tick — the metadata sample failed, the folded one
    # succeeded — is routed through the obscured path again, so the change is
    # deferred a tick and the complete folded value pollutes the obscured
    # chain's baseline (false reports on a later readability flap).
    # #2122 F5/P3: the anchor is the two code lines of the first call site,
    # comment-free. #2122 fix-r1: the guard is `== Obscured` now (a
    # content-unknown fold must not enter the obscured chain either); the
    # `if true` mutant still routes every mixed tick into it.
    ('mixed-tick-routed-obscured', source,
     '            if status == SampleStatus::Obscured {\n'
     '                return self.observe_obscured_tick(next, target_sample);\n',
     '            if true {\n'
     '                return self.observe_obscured_tick(next, target_sample);\n',
     TESTS + 'a_mixed_tick_compares_against_the_last_complete_fingerprint',
     '#1987 A4 a mixed tick reports against the last complete fingerprint'),
    # #1987 B5: an obscured startup sample commits its folded value into the
    # complete fingerprint slot (the r1 wrong shape), so every change during
    # the obstruction is invisible and the recovery report disappears.
    ('startup-obscured-committed-complete', source,
     '        let (fingerprint, obscured_fingerprint) = if status == SampleStatus::Obscured {\n'
     '            (None, Some(fingerprint))\n'
     '        } else {\n'
     '            (Some(fingerprint), None)\n'
     '        };',
     '        let (fingerprint, obscured_fingerprint) = if status == SampleStatus::Obscured {\n'
     '            (Some(fingerprint), None)\n'
     '        } else {\n'
     '            (None, Some(fingerprint))\n'
     '        };',
     TESTS + 'a_startup_obstruction_commits_no_complete_fingerprint',
     '#1987 B5 an obscured startup sample commits no complete fingerprint'),
    # #1987 B6: a `read_dir` iteration error mid-listing is swallowed, so a
    # partial entry list is accepted as a complete observation (an obscured
    # tick false-reports the truncated entries as vanished). #2122 P3: the
    # anchor is the error arm alone — the `Ok` arm and the comment between
    # them were copied in verbatim and expired with any comment edit.
    ('read-dir-entry-error-swallowed', source,
     '                        Err(_) => listing_obscured = true,',
     '                        Err(_) => {}',
     TESTS + 'read_dir_iteration_failure_obscures_the_sample',
     '#1987 B6 an obscured sample reports nothing'),
    # #1987 A3/B7: a failed startup probe on the *parent* of a missing config
    # file drops the path again, closing detection for good — the parent arm
    # of `WatchedPath::new`'s stat fallback. #2122 P3: the anchor is the arm
    # alone, not the comment above it.
    ('startup-parent-probe-failure-dropped', source,
     '                    Err(_) => Some(Self::probing(parent, target)),',
     '                    Err(_) => None,',
     TESTS + 'startup_parent_probe_failure_watches_the_parent_dir',
     '#1987 A3 the parent is kept'),
    # #2122 F1: the second obscured tick reports the same obstructed state
    # again — the folded chain compares "any previous" instead of "a
    # different previous". The B6 test's armed budget used to cover only one
    # obscured tick (four entry steps), so its "second obscured tick stays
    # quiet" assertion ran on a complete sample and this mutant survived it;
    # the budget fix is what makes the row kill.
    ('obscured-chain-rereports', source,
     '        let previous = self.obscured_fingerprint.replace(sample);\n'
     '        previous.is_some_and(|previous| previous != sample)',
     '        let previous = self.obscured_fingerprint.replace(sample);\n'
     '        previous.is_some()',
     TESTS + 'read_dir_iteration_failure_obscures_the_sample',
     '#1987 B6 the second obscured tick stays quiet'),
    # #2122 F2: the startup sample commits a fingerprint into the metadata
    # shortcut slot again. The A2 test used to assert `metadata_fingerprint
    # == None` only *after* the reporting `changed()` — where D11 forces
    # `None` on every reporting tick, so the assertion could not fail; it now
    # runs before the first `changed()` and this mutant dies there.
    ('startup-shortcut-committed-again', source,
     '            metadata_fingerprint: None,\n'
     '            obscured_fingerprint,',
     # `fingerprint` here is already the `Option<u64>` this slot holds — the
     # compiling shape of the pre-#1987 startup commit.
     '            metadata_fingerprint: fingerprint,\n'
     '            obscured_fingerprint,',
     TESTS + 'a_write_between_the_startup_samples_is_reported',
     '#1987 A2 the startup sample commits no metadata shortcut'),
    # #2122 F5: the mixed-tick guard used to accept any non-obscured folded
    # sample, so a folded sample whose content read failed (`ContentUnknown`)
    # fell through to the complete path, committed its `read = None` fold and
    # false-reported an untouched config — once on the mixed tick, once more
    # at recovery. #2122 fix-r1: the guard is `== Obscured` in the source now
    # and the content-unknown fold is gated by the unknown-read twin instead;
    # this row anchors that gate — the mutant disables it, restoring the F5
    # fall-through on the mixed path.
    ('mixed-content-tick-committed', source,
     '        if status == SampleStatus::ContentUnknown && self.content_unknown_twin == Some(next) {\n',
     '        if status == SampleStatus::ContentUnknown && false {\n',
     TESTS + 'a_mixed_tick_whose_folded_content_read_fails_reports_nothing',
     '#2122 F5 a mixed tick with an unknown fold reports nothing'),
    # #2122 fix-r1: the twin gate must not fire on a content-unknown sample
    # whose observable state DID move — widened, it swallows the one report a
    # genuinely changed config is owed while the read keeps failing (#1792's
    # contract: the failing tick after a real write reports once).
    ('content-unknown-twin-gate-widened', source,
     '        if status == SampleStatus::ContentUnknown && self.content_unknown_twin == Some(next) {\n',
     '        if status == SampleStatus::ContentUnknown && true {\n',
     TESTS + 'transient_read_failure_is_retried_until_the_content_is_seen',
     '#1792 the first (incomplete) sample still reports the change once'),
    # #2122 fix-r1 (MM boundary#1's sibling): the second call site — the
    # failure landed between the two samples, so the metadata one is clean —
    # had no anchor of its own. Its guard widened back to the F5 first cut,
    # a between-samples content-unknown fold enters the obscured handling,
    # commits its `read = None` shape into the chain and goes quiet on a
    # change it owed a report for.
    ('between-samples-obscured-guard-widened', source,
     '        };\n        if status == SampleStatus::Obscured {\n',
     '        };\n        if status != SampleStatus::Complete {\n',
     TESTS + 'transient_read_failure_is_retried_until_the_content_is_seen',
     '#1792 the first (incomplete) sample still reports the change once'),
    # #2122 fix-r1 (MM concurrency#2): the first call site's guard widened
    # back to the F5 first cut, a mixed content-unknown fold enters the
    # obscured chain, and the next shape flip in that chain false-reports —
    # the two fold shapes are never equal, so they must never be compared.
    ('obscured-chain-mixes-fold-shapes', source,
     '            if status == SampleStatus::Obscured {\n'
     '                return self.observe_obscured_tick(next, target_sample);\n',
     '            if status != SampleStatus::Complete {\n'
     '                return self.observe_obscured_tick(next, target_sample);\n',
     TESTS + 'an_obscured_chain_never_compares_content_unknown_folds',
     '#2122 fix-r1 a content-unknown fold is not compared against the obscured chain'),
    # #2122 fix-r1: every complete sample — a reported one included — commits
    # the unknown-read twin of the state it saw. Without the commit, the first
    # content-unknown tick after a reported change has no baseline to compare
    # against and false-reports the state it just reported.
    ('content-unknown-twin-not-committed', source,
     '            self.content_unknown_twin = Some(twin);\n',
     '            self.content_unknown_twin = None;\n',
     TESTS + 'a_content_unknown_tick_after_a_reported_change_stays_quiet',
     '#2122 fix-r1 the content-unknown tick after the reported change reports nothing'),
    # #2122 fix-r1: a quiet content-unknown tick must still clear the metadata
    # shortcut — otherwise the next tick's identical metadata short-circuits
    # around the read that has to be retried (#1792's retry contract).
    ('content-unknown-shortcut-not-cleared', source,
     '            self.metadata_fingerprint = None;\n'
     '            return false;\n',
     '            return false;\n',
     TESTS + 'a_content_unknown_tick_clears_the_shortcut_so_the_read_is_retried',
     '#2122 fix-r1 the retry tick re-reads the content despite identical metadata'),
    # #2122 fix-r1: the startup sample must seed the twin like any other
    # complete one. Without the seed, the first content-unknown tick after
    # startup has no baseline and false-reports the untouched config — the
    # review's missing grid cell.
    ('startup-unknown-twin-not-seeded', source,
     '            content_unknown_twin: if status == SampleStatus::Complete {\n',
     '            content_unknown_twin: if false {\n',
     TESTS + 'an_untouched_config_with_a_failing_first_content_read_reports_nothing',
     '#2122 fix-r1 the first content-unknown tick with no change reports nothing'),
    # #2122 F4: a truncated listing that never reached the target is read as
    # "the target vanished" — the unobserved arm returns the (empty) target
    # fold instead of `None`, so the transition tick false-reports an
    # untouched config once. The `read-dir-entry-error-swallowed` row cannot
    # catch this: its bound test watches a directory with no target, where
    # the two arms are indistinguishable.
    ('truncated-listing-target-vanished', source,
     '            Some(SampleStatus::Complete) => Some(target_hasher.finish()),\n'
     '            None if !listing_obscured => Some(target_hasher.finish()),\n'
     '            _ => None,',
     '            Some(SampleStatus::Complete) => Some(target_hasher.finish()),\n'
     '            None if !listing_obscured => Some(target_hasher.finish()),\n'
     '            _ => Some(target_hasher.finish()),',
     TESTS + 'a_truncated_listing_that_misses_the_target_reports_nothing',
     '#2122 F4 a transition tick that never reached the target reports nothing'),
]


# specs/negative-controls/README.md: the denominator is a static literal, not
# the runtime table length — dropping a row must fail loudly instead of making
# the round quietly shorter (still reporting N/N killed). #1946 review P2.
EXPECTED_NAMES = (
    'debounce-ceiling-removed',
    'two-hour-debounce-ceiling-removed',
    'poll-ceiling-removed',
    'deadline-add-unchecked',
    'overflowing-debounce-double-restore',
    'metadata-short-circuit-removed',
    'ctime-dropped-from-metadata',
    'ctime-dropped-from-metadata-unit',
    'incomplete-sample-committed',
    'startup-incomplete-committed',
    'stat-failure-counted-complete',
    'read-dir-failure-counted-complete',
    'startup-probe-failure-dropped',
    'startup-recovery-not-reported',
    'recovery-trusts-folded-fingerprint',
    'probe-not-found-fixes-file-mode',
    'report-commits-metadata-shortcut',
    'mode-change-not-detected',
    'obscured-change-not-reported',
    'read-dir-not-found-counted-obscured',
    'not-found-counted-obscured',
    'is-finished-stop-evidence-restored',
    'watch-loop-panic-injected',
    'watch-loop-panic-injected-store-half',
    'watch-loop-clean-early-exit',
    'watch-loop-clean-early-exit-store-half',
    'watch-loop-exits-after-first-tick',
    'watch-loop-exits-after-first-tick-handle-half',
    'watch-loop-exits-after-first-tick-before-stop',
    'watch-loop-panic-injected-before-stop',
    'watch-loop-clean-early-exit-before-stop',
    'watch-loop-never-stops',
    'stop-panic-swallowed-again',
    'finished-reported-as-cancelled',
    'stop-misuse-silenced-again',
    'join-window-hop-removed',
    'obscured-transition-target-not-compared',
    'startup-two-sample-window',
    'mixed-tick-routed-obscured',
    'startup-obscured-committed-complete',
    'read-dir-entry-error-swallowed',
    'startup-parent-probe-failure-dropped',
    'obscured-chain-rereports',
    'startup-shortcut-committed-again',
    'mixed-content-tick-committed',
    'content-unknown-twin-gate-widened',
    'between-samples-obscured-guard-widened',
    'obscured-chain-mixes-fold-shapes',
    'content-unknown-twin-not-committed',
    'content-unknown-shortcut-not-cleared',
    'startup-unknown-twin-not-seeded',
    'truncated-listing-target-vanished',
)


def enforce_denominator():
    names = tuple(row[0] for row in mutations)
    if names != EXPECTED_NAMES:
        missing = [name for name in EXPECTED_NAMES if name not in names]
        extra = [name for name in names if name not in EXPECTED_NAMES]
        raise SystemExit(
            f'#1946 fixed mutation denominator: expected {len(EXPECTED_NAMES)} '
            f'rows in order, found {len(names)} '
            f'(missing={missing}, extra={extra}, '
            f'order_ok={sorted(names) == sorted(EXPECTED_NAMES)})')


def check_anchor(row, original):
    if original.count(row[2]) != 1 or row[2] == row[3]:
        raise ValueError(f'{row[0]}: stale or unchanged anchor (count={original.count(row[2])})')


def mutate(content, scope, replacements):
    for old, new in replacements:
        if content.count(old) != 1 or old == new:
            raise RuntimeError('#1736/#1739/#1792/#1794/#1899/#1920/#1987/#2122 expired or ineffective mutation anchor')
        content = content.replace(old, new, 1)
    return content


def main():
    args = sys.argv[1:]
    enforce_denominator()
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
        print(f'#1736/#1739/#1792/#1794/#1899/#1920/#1987/#2122 {len(mutations)} rows / {distinct} distinct anchors checked')
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
