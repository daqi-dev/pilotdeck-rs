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
pins `GROUP_NOT_EMPTY`/non-PASS (the independent census must find it, so EPERM
alone can never become VERIFIED); `identity-lost` forces the "already reaped"
branch and pins `NOT_VERIFIED` with zero `killpg` calls; `census-unavailable`
removes the live-member census and pins `BLOCKED`/non-PASS instead of empty;
`reap-state` forces `run_fixture` to see `REAPED` and pins no bare
`child.kill()` plus `NOT_VERIFIED`;
`runtime-negative` replaces only the probe result with `VERIFIED` at runtime
and shows the leaky fixture would then look accepted, while the real probe still
reports `GROUP_NOT_EMPTY`; `status` pins `VERIFIED -> PASS` and
`GROUP_NOT_EMPTY/BLOCKED/NOT_VERIFIED -> PASS_WITH_CLEANUP_UNVERIFIED`;
`wiring` drives `session_domain.main()`'s real cleanup tail with an unverified
group and pins `status` / `executionStatus` / `freezeStatus` and exit 2.
Every process mode checks the live process group and the grandchild PID after
the run.

The probe waits for the launcher with `waitid(WNOWAIT)` and only reaps after the
group probe and any signals, so a bare pgid is never used after it could be
reused.  `run_fixture` reads `launch.json` only after that cleanup (a truncated
file becomes `fixtureReadError` instead of skipping cleanup) and always saves
the audit.  The controls never signal a pid after the leader was reaped: a
residual group is reported as a failure and the fixture grandchild is bounded by
its own lifetime.  `VERIFIED` covers the launcher's process group only; a
descendant that calls `setsid`/`detached` leaves that group and is a registered
P2 boundary, not part of this proof.

## Run after commit (source mutation controls)

`negative_controls.py` uses `negative_process.mutating_sources`, so it refuses
to run until the #164 change is committed and the named source is byte-identical
to HEAD. It mutates `session_domain.py` one anchor at a time, requires the
behavior check to fail **for that mutation's own expected fields**, restores the
bytes, and finally requires the restored source to pass. A stale anchor exits 2
(`ANCHOR_EXPIRED`), never `SURVIVED`. Environment/startup failures are `INVALID`
(exit 3) and are never counted as killed; a probe `signalError`/`probeError`
that the mutation did not declare is `INVALID` too, so a denied group signal
cannot satisfy a "the wrong signal was sent" mutation. Every mutation runs
before the summary is decided. No cargo is launched.

```sh
python3 -I src/pilotdeck-gateway/tests/fixtures/session-domain-cleanup/negative_controls.py \
  --output _tmp/20261026-issue-164/negative/source
```

A pre-commit rehearsal can pass `--baseline <tree-ish>`; the default is `HEAD`.
`--only NAME` (repeatable) runs just the named mutants and still runs the
restored positive.  `MUTATIONS` is a literal `(name, source, old, new)` table
read by #820's anchor selfcheck; per-mutant mode/judge metadata lives in
`JUDGES`, keyed by the same names.

`pgrep-before.txt` / `pgrep-after.txt` are collected around the mutations using
the unique `session-domain-cleanup/launcher.py` pattern (bounded `timeout=5`);
the fixture never matches or signals another user's `sleep`.

`ts_group_probe.mjs` (Node v22.22.0) bounds `waitExit` to 10 s, `waitFile` to
5 s and `settle` to 10 s, and registers its spawned leader before any await.
It never signals a pid it does not hold: `term` sends SIGTERM to `-pgid` only
while the leader is a live, unreaped direct child, and `leak` only observes a
short-lived grandchild's natural exit after the leader's `exit` event reaped it.
