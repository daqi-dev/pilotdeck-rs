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
a refused leader kill (`cleanupBlocked`) retains, and a retained lease cannot be
stolen by a later acquirer.

## Source-mutation controls (after commit)

```sh
python3 -I -B src/pilotdeck-gateway/tests/fixtures/session-domain-lock-release/negative_controls.py \
  --output _tmp/20261026-issue-209/negative/lock-release
```

`negative_controls.py` uses `negative_process.mutating_sources`, so it refuses
files that differ from HEAD.  When launched in the background, wrap it with the
host `sigint-default.py python3 ...` so the child does not inherit a background
shell's `SIGINT=SIG_IGN` (the signal-deferral tests need default delivery).  Run it only after the coordinator commits the
patch; pre-commit it reports `INVALID` (never `SURVIVED`).  It holds the real
`/tmp/pdrs-parity.lock` through the same owner/guard protocol while mutating, so
a concurrent parity run makes the control `INVALID` (busy lock), not a mutation
window.  `--check-anchors` is the read-only preflight; a stale anchor exits 2.

The eight mutants are:

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
- `e1-takeover-ignores-pending` — lets `common.parity_lock` reclaim a lease
  that still has `cleanupPending` (shared protocol row).

Every mutant is compiled before its bound positive runs.  Rows report
`KILLED` / `SURVIVED` / `INVALID` and the run continues to the last selected
row; only an all-`KILLED` batch exits 0.  `new_output_directory` refuses a
non-empty output directory.  If the outer source guard or restoration raises,
`invalidate_completed()` rewrites every already-recorded `KILLED` row to
`INVALID` (the #979 shared semantics) and the exit is 2.
