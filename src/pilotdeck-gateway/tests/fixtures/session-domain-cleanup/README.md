# session-domain-cleanup

#164 controls for the ps-free process-group cleanup probe in
`scripts/gateway-parity/session_domain.py`.

## Run in this session (does not mutate source)

```sh
python3 -I src/pilotdeck-gateway/tests/fixtures/session-domain-cleanup/check_cleanup.py \
  --output _tmp/20261026-issue-164/negative/behavior --mode all
```

`clean` is the empty-group positive control; `leak` leaves a bounded `sleep 20`
grandchild after the launcher exits; `ignore` leaves a grandchild that ignores
SIGTERM (and writes a ready marker after installing `SIG_IGN`; the launcher
waits for it for at most 10 s before exiting) to exercise the bounded SIGKILL
escalation; `blocked` denies both `killpg` and the leader `kill`, pins
`BLOCKED`/non-PASS and separately exercises the generic `OSError` branch;
`eperm-live-member` denies only `killpg` while a real grandchild is alive and
pins `GROUP_NOT_EMPTY`/non-PASS (the independent group enumeration must find it,
so EPERM alone can never become VERIFIED); `identity-lost` forces the "already
reaped" branch and pins `NOT_VERIFIED` with zero `killpg` calls;
`census-unavailable` removes the group enumeration and pins `BLOCKED`/non-PASS
instead of empty; `census-snapshot` makes a member appear between two complete
group snapshots and pins `GROUP_NOT_EMPTY`/non-PASS; `census-descendant` pins
the stable pair that includes the descendant; `census-unstable` pins an
unproven (None) census when the two snapshots disagree; `census-truncated`
feeds a full libproc buffer and pins `None`, never a partial list;
`census-enumeration` makes enumeration fail and pins `BLOCKED`; `pid-blocked`
denies `kill(pid, 0)` and pins the three-state `blocked` as "cannot exclude
alive" (never gone); `linux-filtered-proc` simulates hidepid's visible-only
`/proc` on Linux and pins `BLOCKED` before any census read or signal, never
`VERIFIED`/`PASS`; `signal-entry`
pins that the single signal entry refuses a reaped leader for both group and
direct-pid sends while still sending to a live held child; `reap-state` forces
`run_fixture` to see `REAPED` and pins no bare leader kill plus `NOT_VERIFIED`;
`runtime-negative` replaces only the probe result with `VERIFIED` at runtime
and shows the leaky fixture would then look accepted, while the real probe still
reports `GROUP_NOT_EMPTY`; `status` pins `VERIFIED -> PASS` and
`GROUP_NOT_EMPTY/BLOCKED/NOT_VERIFIED -> PASS_WITH_CLEANUP_UNVERIFIED`;
`wiring` drives `session_domain.main()`'s real cleanup tail with an unverified
group and pins `status` / `executionStatus` / `freezeStatus` and exit 2;
`batch-aggregation` pins the batch status order, `run_all()`/`main()` status and
exit codes, calibrate's `PASS_WITH_SKIPS -> BLOCKED / exit 2` mapping, and the
source negative-control batch: a restored positive with platform skips must make
that batch `PASS_WITH_SKIPS` / exit 1, never `PASS` / exit 0.
`aggregation-audit` statically enumerates every top-level status/exit-code
computation in the E1 apparatus and fails a registered point that does not call
`common.aggregate_status()`, an unregistered new point, or a `int(...)` exit
computation.  #892 adds two layers: a directly returned summary dict (a `status`
key in the returned dict or a dict element of a returned tuple) marks the
function as an aggregation point that must be registered, and every registered
point must route each return path's status slot, status literal, and exit-code
computation through the shared function instead of computing its own verdict
inline.  The per-return-path check follows the dominant real-code shape — the
summary dict is built into a local `report`/`summary`/`result` name and returned
by that name — and it only accepts a written status literal when it is
fail-closed (`BLOCKED`/`FAIL`/`NOT_RUN`); `'PASS'` written straight into a slot
is a bypass even though it is an `ast.Constant`.  A status slot that is a local
variable is followed back through every assignment in the function: if any
assignment computes the value inline instead of taking it from the shared call,
the slot is flagged.  The same function is enumerated under any variable name
(`payload = {'status': ...}; return payload`), including annotated assignments
and element-wise tuple targets.  Every return arm is judged on its own, so
`0 if cond else common.aggregate_status(...)[1]` cannot be excused by the shared
call in the other arm.  A bare `return`, a literal `0`, and `raise
SystemExit(0)` are optimistic exits and are flagged; a non-zero exit literal
(`return 2` for BLOCKED, `return 3` for NOT_RUN) stays legal because it cannot
claim a clean run.  The audit self-tests twenty probes: a direct `{'status': ...}`
return, an inline `0 if ... else 1` branch, a legal shared pair, a summary dict
returned through a local name, a bare `return 0`, an optimistic conditional arm,
a bare `return`, a summary dict returned under an arbitrary name, a status slot
filled from a locally computed variable, a `raise SystemExit(0)`, a clean
verdict written straight into a `report['status']` slot, an explicit
`return None`, a `raise SystemExit(None)`, an exit code relayed through a local
variable, a status slot filled from a local dict subscript, a local
`aggregate_status` lookalike, a `report.update({'status': 'PASS'})` write, a
duplicate `status` key, and a nested `def main`.  Registered points also
write their report incrementally instead of returning the dict, so every
`x['status'] = ...` assignment in a registered point is judged by the same rule:
a `'PASS'` / `'PASS_WITH_SKIPS'` literal (directly or through a local variable)
and an inline `all(...passed...)` are flagged; relays, shared calls and
non-batch labels such as `'PENDING_REVIEW'` stay legal.
Fixed in r3, no longer boundaries: an explicit `return None` / `return False`
from a registered point is an optimistic exit (only ``_guard._failure``, listed
in ``_SENTINEL_POINTS``, keeps ``None`` as its "propagate unchanged" sentinel),
a duplicate `status` key inside one summary dict is judged by every value, a
`{**literal}` expansion is walked, and a `status` / `verdict` key nested inside
another dict is read where the summary is returned or passed on.  Known
boundaries, not covered by this check: the module-level `raise
SystemExit(main())` that no function contains, and a status key built by
`dict(**kwargs)` or a comprehension.  The
exit mapping is the one
registered order: `PASS=0`, `PASS_WITH_SKIPS/FAIL=1`, `BLOCKED=2`, `NOT_RUN=3`.
Every process mode checks the live process group and the grandchild PID after
the run.  On non-Darwin the Darwin-only positives (`clean`, `leak`, `ignore`,
`eperm-live-member`, `runtime-negative`) are recorded as `passed: null` with an
explicit `skipped` reason instead of a false FAIL: the non-Darwin probe is
`BLOCKED` before any census, so their positive expectation is unprovable there.
The fixture summary is pessimistic: any executed failure or malformed result is
`FAIL`; all executed pass with any skip is `PASS_WITH_SKIPS` (exit 1); only no
skips and all executed pass is `PASS` (exit 0).  A skip is never a pass.

The probe waits for the launcher with `waitid(WNOWAIT)` and only reaps after the
group probe and any signals, so a bare pgid is never used after it could be
reused. macOS enumerates the group directly with
`libproc.proc_listpids(PROC_PGRP_ONLY, pgid, ...)`, not a global pid snapshot
followed by per-pid `getpgid`, so a member that forks a same-group descendant
and exits between snapshot and query cannot be missed; two complete snapshots
must agree. Non-darwin has no equivalent completeness proof (`/proc` can be
filtered by `hidepid`/permissions), so `probe_process_group` reports `BLOCKED`
before reading a census or sending a signal; `linux-filtered-proc` pins that
boundary. A libproc return count equal to the capacity is truncation: the
implementation allocates slack and accepts only a strictly short, 4-byte
aligned write, growing while full; it never returns a partial group. Enumeration
failure, truncation, a changing group, or a denied leader probe all record
`BLOCKED` and send no signal. Every actual signal goes through the
`HeldProcessGroup` entry, which re-checks `waitid(WNOWAIT)` immediately before
sending and refuses unless the leader is `RUNNING` or `EXITED` (unreaped
zombie); signal-0 observations are non-delivering only. `run_fixture` reads
`launch.json` only after that cleanup (a truncated file becomes
`fixtureReadError` instead of skipping cleanup) and always saves the audit. The
controls never signal a pid after the leader was reaped: a residual group is
reported as a failure and the fixture grandchild is bounded by its own lifetime.

`VERIFIED` covers the launcher's process group and only up to the observation
instant of the last complete group snapshot: two equal snapshots prove no
uncounted live member at those instants. A descendant forked after the last
snapshot is outside that instant (an observation boundary, not an incomplete
census read as empty). A descendant that calls `setsid`/`detached` leaves the
group and is a registered P2 boundary, not part of this proof; this round does
not claim to detect that escape.

## Run after commit (source mutation controls)

`negative_controls.py` uses `negative_process.mutating_sources`, so it refuses
to run until the #164 change is committed and the named sources are byte-identical
to HEAD. It mutates the named source one anchor at a time (`session_domain.py`,
`controls/session_domain.py`, this `check_cleanup.py`, or its own
`negative_controls.py`), requires the
behavior check to fail **for that mutation's own expected fields**, restores the
bytes, and finally requires the restored source to pass. A stale anchor exits 2
(`ANCHOR_EXPIRED`), never `SURVIVED`. Environment/startup failures are `INVALID`
and map to `BLOCKED` / exit 2 through the shared aggregator; they are never
counted as killed. A probe `signalError`/`probeError`
that the mutation did not declare is `INVALID` too, so a denied group signal
cannot satisfy a "the wrong signal was sent" mutation. Every mutation runs
before the summary is decided. No cargo is launched.

The batch top-level status/exit code is the sole
`common.aggregate_status()` result over all mutants and the restored positive:
`KILLED -> PASS`, `SURVIVED -> FAIL`, `INVALID -> BLOCKED`, and the restored
summary status as-is.  A restored `PASS_WITH_SKIPS` (non-Darwin platform skips)
therefore makes the batch `PASS_WITH_SKIPS` / exit 1, never `PASS` / exit 0.

```sh
python3 -I src/pilotdeck-gateway/tests/fixtures/session-domain-cleanup/negative_controls.py \
  --output _tmp/20261026-issue-164/negative/source
```

Run after the coordinator commits the patch. `--baseline` only accepts `HEAD`;
worktree snapshots and alternate trees must not bypass the pristine-source guard.
`--only NAME` (repeatable) runs just the named mutants and still runs the
restored positive. `MUTATIONS` is a literal `(name, source, old, new)` table
read by #820's anchor selfcheck; per-mutant mode/judge metadata lives in
`JUDGES`, keyed by the same names. The current table has 40 mutants:
`empty-group-probe-removed`, `cleanup-status-write-removed`,
`top-level-mapping-relaxed`, `sigterm-removed`, `blocked-branch-removed`,
`sigkill-escalation-removed`, `top-level-wiring-relaxed`,
`eperm-census-ignored`, `unavailable-census-as-empty`, `reap-guard-removed`,
`snapshot-race-accepted`, `unstable-census-accepted`, `truncation-accepted`,
`enumeration-failure-empty`, `signal-gate-removed`, `pid-blocked-as-gone`,
`linux-visible-census-trusted`, `batch-status-relaxed`,
`fixture-summary-relaxed`, `calibrate-controls-skip-accepted`,
`aggregation-exit-bypassed`, `summary-dict-bypass`,
`aggregation-branch-exit-inline`, `summary-name-bypass`, `bare-exit-zero`,
`status-slot-relayed`, `system-exit-zero-bypass`, `bare-return-exit`,
`conditional-arm-exit`, `calibrate-bare-zero`,
`calibrate-status-write-pass`, `restored-skip-accepted`,
`controls-return-none`, `controls-system-exit-none`,
`calibrate-exit-code-relayed`, `controls-status-slot-subscript`,
`controls-aggregate-lookalike`, `calibrate-status-update`,
`retirement-duplicate-status-key`, `controls-nested-same-name`.  The batch-related
five revert the pessimistic batch aggregation, the fixture skip summary, the
calibrate skipped-controls mapping, the controls `main()` shared exit call
(`aggregation-audit`), and the source negative-control final aggregation
(drops the restored positive child).  `summary-dict-bypass` replaces the
`run_all()` shared aggregation with a nested function returning a summary
dict directly (the #892 enumeration and per-branch audits must catch it);
`aggregation-branch-exit-inline` rewrites one `main()` exit branch to compute
its own code while the other still shares (the per-branch slot audit must
catch it); `summary-name-bypass` writes `'PASS'` into `run_all()`'s summary
dict that is returned by name (the name-resolution and literal rules must
catch it, and no unregistered point may appear); `bare-exit-zero` replaces one
`main()` exit path with a bare `return 0` while the other still shares (the
optimistic-exit rule must catch it); `status-slot-relayed` computes `status`
inline right before the summary dict is built (the variable-following rule must
catch it); `system-exit-zero-bypass` replaces one `main()` exit path with
`raise SystemExit(0)`; `bare-return-exit` replaces it with a bare `return`;
`conditional-arm-exit` hides the optimistic value inside one arm of a
conditional; `calibrate-bare-zero` puts the same optimistic exit on the
calibrate entry point (`session_domain.py:main`, a different registered point,
so the rule is not proven only on the controls file); and
`calibrate-status-write-pass` writes `report['status'] = 'PASS'` incrementally,
with no Return involved, so the assignment scan is proven on its own.  The
eight `fix-r3` mutants each pin one of the false-pass vectors the r2 reviews
found, so no rule is proven only by an in-file self-test: `controls-return-none`
returns `None` from a registered `main`, and `controls-system-exit-none` raises
`SystemExit(None)` (both exit 0);
`calibrate-exit-code-relayed` binds the exit code inline and returns the name;
`controls-status-slot-subscript` fills the slot from a local dict;
`controls-aggregate-lookalike` shadows the shared function with a local one;
`calibrate-status-update` writes the slot through `update()`;
`retirement-duplicate-status-key` repeats the `status` key with a clean value
second; `controls-nested-same-name` nests a second `def main`.  The
restored-positive mutant binds `check_cleanup.py --mode batch-aggregation`'s
`negativeRestored.skipStatus/skipReturncode`, not an unrelated panic/timeout;
`aggregation-exit-bypassed` binds `aggregation-audit`'s
`points.controls-main.shared` and its dedicated first failure message.

`pgrep-before.txt` / `pgrep-after.txt` are collected around the mutations using
the unique `session-domain-cleanup/launcher.py` pattern (bounded `timeout=5`);
the fixture never matches or signals another user's `sleep`.

`ts_group_probe.mjs` (Node v22.22.0) bounds `waitExit` to 10 s, `waitFile` to
5 s and `settle` to 10 s. It registers the leader's `exit` listener and promise
immediately after spawn, before any await. Every actual signal goes through
`signalHeldGroup` / `signalHeldChild`, which refuse unless the leader is still a
live, unreaped direct child; `term` sends SIGTERM to `-pgid` only while that
gate holds, `term-early` deterministically waits for the leader's exit and
pins `signalRefused=leader-exit-observed`, and `leak` only observes a
short-lived grandchild's natural exit after the `exit` event reaped the leader.
A fake already-exited entry pins the gate refusal without calling `process.kill`.

`ts_group_probe_negative.mjs` copies the probe to a temp file, removes only the
ownership gate, replaces the real senders with a recorder, and requires the
mutated copy to fail (`KILLED`). It never signals a real process and never
touches the repository source.
