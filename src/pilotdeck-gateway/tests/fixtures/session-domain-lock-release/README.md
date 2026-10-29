# session-domain-lock-release

#209 E1 controls for the gated release of `/tmp/pdrs-parity.lock` by
`scripts/gateway-parity/session_domain.py`.

The shared lease contract is the same as #990 A2 (`run_round.py` +
`common.py`) and #1015 A1/B2: a run registers every process group it starts,
defers SIGINT/SIGTERM across spawn/registration and cleanup, and removes a pgid
from `common.LIVE_GROUPS` only after the group probe proves absence **and** the
leader is reaped **and** no cleanup step failed.  If any of those is unproven,
`common.parity_lock` publishes `owner.json.cleanupPending` and keeps the lease;
dead-pid/expired takeover must not reclaim it.

The gate lives in a dedicated `allStopped` field.  The frozen scoring fields
(`processTreeCleanup`, `cleanupBlocked`, `processGroupProbe`, `psCensus`) keep
exactly the writes `origin/main` performs: a verified probe is *not* downgraded
because the lease is retained (#209 E1 P0-1).  A cleanup helper exception is
recorded as `cleanupRaised` and re-raised, so the existing FAIL/BLOCKED contract
decides the run instead of an optimistic report.

## Positive controls (no source mutation)

```sh
python3 -I -B src/pilotdeck-gateway/tests/fixtures/session-domain-lock-release/test_lock_release.py -v
```

Each test uses its own temporary lease root, so the real
`/tmp/pdrs-parity.lock` is never touched.  `run_fixture` is driven with a fake
child for the failure injections and once with a real empty process group
(`python3 -I -c pass`) to prove the gate also releases when the host probe
returns `VERIFIED`.

Covered cells: spawn/registration under blocked signals, cleanup under blocked
signals, verified empty group releases, `GROUP_NOT_EMPTY` retains with explicit
`cleanupPending`, probe `OSError` retains, a leader that was not reaped retains,
a refused leader kill (`cleanupBlocked`) retains while `processTreeCleanup`
stays `VERIFIED` (main's scoring), a cleanup helper that raises after the leader
settled records `cleanupRaised`, keeps the probe's `VERIFIED` value, propagates
the error and retains, an `EXITED`-zombie refused kill records `cleanupBlocked`
and retains (main's unconditional refused branch, fail-closed), and a retained
lease cannot be stolen by a later acquirer.

A retained lease and the frozen report status are deliberately independent:
when `allStopped` is false but `processTreeCleanup` is `VERIFIED`, the gateway
report keeps `status=PASS` (`cleanup_status` only maps `processTreeCleanup`)
while the retained evidence is split by file: `report.json.cleanupPending`,
`launch.json.ownedGroupsRemaining` and the lock directory's
`owner.json.cleanupPending` (`launch.json` has no `cleanupPending`), and the
shared lease stays retained.  That combination is intentional (#209 E1): the
frozen scoring fields must match `origin/main`, and lock lifecycle is expressed
only by `allStopped`/`cleanupPending`, never by rewriting a scoring field.

`RunnerInvalidationControls` drives the mutation runner itself.  Per-row
results are kept in memory; `summary.json` is published exactly once, after
`mutating_sources` restored and byte-verified every guarded file.  Nothing in
the target directory is deleted or renamed before `new_output_directory`
admits it: a directory holding an old `summary.json` — alone or together with
other evidence — gets the normal non-empty refusal (#953) with every byte
preserved, so a mistyped `--output` cannot destroy another run's result; and a
successfully admitted directory is always empty, so a previous success cannot
be inherited either (#209 E1 r4 P0).  A guard failure after the last row
publishes `INVALID`; a `KeyboardInterrupt` or
`SystemExit` during the guard exit voids the same rows and is then re-raised;
a SIGINT inside that void publish leaves no summary at all (missing means
failure, never the old all-KILLED file); and the guard set must carry both
protected sources plus the delivered runner with exactly one `mutating_sources`
call.

## Source-mutation controls (after commit)

```sh
python3 -I -B src/pilotdeck-gateway/tests/fixtures/session-domain-lock-release/negative_controls.py \
  --output _tmp/20261026-issue-209-e1/fix-r3/negative/lock-release
```

`negative_controls.py` uses `negative_process.mutating_sources`, so it refuses
files that differ from HEAD (the default `--baseline HEAD`; the flag exists for
a coordinator rehearsal on a committed tree).  `mutating_sources` acquires the
real `/tmp/pdrs-parity.lock` itself — the runner has no second, outer lease.
When launched in the background, wrap it with the host
`sigint-default.py python3 ...` so the child does not inherit a background
shell's `SIGINT=SIG_IGN` (the signal-deferral tests need default delivery).
Run it only after the coordinator commits the patch; pre-commit it reports
`INVALID` (never `SURVIVED`).  `mutating_sources` holds the real
`/tmp/pdrs-parity.lock` through the same owner/guard protocol while mutating, so
a concurrent parity run makes the control `INVALID` (busy lock), not a mutation
window.  `--check-anchors` is the read-only preflight; a stale anchor exits 2.

The fifteen mutants are:

- `e1-release-ungated` — deletes the group's registration even when cleanup is
  unverified.
- `e1-spawn-not-deferred` — removes the deferred-signal window around
  `Popen` + registration.
- `e1-cleanup-not-deferred` — removes it from the cleanup path.
- `e1-group-not-registered` — never adds the pgid to `LIVE_GROUPS`.
- `e1-probe-error-as-verified` — publishes a probe `OSError` as `VERIFIED` and
  drops the error.
- `e1-leader-not-reaped-verified` — reports `VERIFIED` when the leader was not
  reaped.
- `e1-cleanup-blocked-ignored` — lets a recorded cleanup error pass the release
  gate.
- `e1-cleanup-raised-ignored` — lets a recorded cleanup-helper exception pass
  the release gate.
- `e1-exited-refused-exempted` — puts back the `EXITED`-zombie exemption, so a
  refused kill of an unreaped leader releases the lease.
- `e1-takeover-ignores-pending` — lets `common.parity_lock` reclaim a lease
  that still has `cleanupPending` (shared protocol row).
- `e1-allstopped-optimistic` — publishes `allStopped=True` although the owned
  group was not proven stopped; the bound probe-OSError test must fail at
  `#209 probe OSError must not report allStopped`.
- `e1-scoring-downgrade-restored` — puts the P0-1 downgrade back
  (`processTreeCleanup=NOT_VERIFIED` whenever the lease is retained); the bound
  test must fail at `#209 P0-1 a verified probe must keep main scoring`.
- `e1-runner-baseexception-narrowed` — narrows the runner's guard exit back to
  `except Exception`, so a cancellation skips the void publish and leaves no
  final summary; the bound test must fail at `#209 P0-2 a cancellation after
  the last row must not keep KILLED`.
- `e1-runner-publish-before-verify` — puts the pre-fix order back: `record()`
  publishes `summary.json` while the guarded restore is still unverified, so a
  SIGINT in the later void publish leaves the all-KILLED file; the bound test
  must fail at `#209 P0-3 an interrupt during the void publish must not leave a
  readable KILLED summary`.
- `e1-runner-retire-before-admission` — puts the pre-admission retirement back
  (`--output`'s `summary.json` is unlinked before `new_output_directory` runs),
  so pointing at another run's evidence directory destroys its summary and is
  only then refused; the bound test must fail at `#209 P0-4 a pre-existing
  evidence directory must be refused with every byte preserved`.

Every mutant is compiled before its bound positive runs.  Rows report
`KILLED` / `SURVIVED` / `INVALID` and the run continues to the last selected
row; only an all-`KILLED` batch exits 0.  `new_output_directory` refuses a
non-empty output directory.  Per-row results stay in memory, so the disk holds
no final `summary.json` while the guarded restore is unverified; the file is
published once after `mutating_sources` restores and byte-verifies every
guarded path.  If the outer source guard or restoration raises — including
`KeyboardInterrupt` / `SystemExit` during restore/re-verify — the shared
`negative_suite.invalidate_completed()` voids every already-recorded `KILLED`
row, the unexecuted rows are added, an `INVALID` summary is published
(#979/#1037), and the `Exception` path exits 2 while a cancellation is
re-raised.  If that void publish is itself interrupted, no `summary.json`
exists: a missing summary is the fail-closed result, never the previous
all-KILLED one.
