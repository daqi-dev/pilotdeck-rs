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
SystemExit(0)` are optimistic exits and are flagged.  The same provenance rules
judge the argument of `sys.exit(...)`, `os._exit(...)` and `raise
SystemExit(...)`, the bare `raise SystemExit` included (it raises with no
argument, so the process exits 0), and a boolean exit expression (`return not
ok`) or a comparison is an inline exit code.  The statement form and the
`return sys.exit(0)` form share one judgement, because a `return` of an exit
call never returns either; an arm of an `if` / boolean argument that is a
literal zero (`sys.exit(0 if cond else 1)`) is inline on its own, even though
the whole argument is not a literal (`fix-r7`).  A returned conditional or
boolean is expanded into its arms, so ``return {'status': 'PASS'} if cond else
report`` and ``return payload if cond else report`` are judged slot by slot like
a plain return, and ``report.update(**name)`` / ``report.update(name)`` /
``report |= name`` whose payload this function never built writes slots the
audit cannot see, so it fails closed (`fix-r7`, extended in `fix-r9`; a dict
literal, a locally built dict and a helper call stay legal, and the
`_bound_update` probe pins that side).  An exit-code name with no local binding
is judged fail-closed on the same reasoning as the status side, and so is a
local relayed from it (`code = EXIT_OK; return code`, `fix-r10`), while an
exit-code delegation through an attribute callee or through a name bound to the
helper is followed instead of skipped (`fix-r10`).  A clean status written
through a call result (`report['status'] = report.get('status', 'PASS')`) is
flagged like the literal it hides (`fix-r10`), and a registered point that
neither returns nor exits at all is flagged `no return or exit path`
(`fix-r10`).  Since `fix-r11` the delegation family judges the helper's own
body instead of only its value returns: a bare `return` or a body that simply
ends hands `None` back (`SystemExit(None)` is exit 0), a `sys.exit(0)` or
`raise SystemExit` written in statement position is the same exit, and a
module-level helper reached through a local (`code = _exit_code(report)`) is
judged like the direct call.  An augmented `report |= {'status': 'PASS'}`
enumerates the function even when it is the only status evidence
(`fix-r11`); its payload call is resolved in the file the function lives in,
exactly like every other delegation judgement (`fix-r15`).  An exit call is a
status/exit computation of its own, so a function whose only evidence is
`sys.exit(...)`, `os._exit(...)`, bare `exit(...)` or `quit(...)` is enumerated
even when the argument carries no status word, and a tuple target
(`report['status'], code = 'PASS', 0`) writes the same slot as the plain
subscript form (`fix-r15`).  A point that ends in an ordinary `raise` has an exit path —
a propagating error is not an optimistic exit, so it is not reported as
`no return or exit path` (`fix-r11`).  `fix-r12` closes the two holes the r11
reviews opened in that family: the receiver of `|=` / `update()` may be a
*parameter* (a report handed in is still a summary being written), and an
`|=` on a parameter only counts when the payload carries status evidence, so a
set union (`owned |= children`) stays a normal statement.  A call is a proven
slot source only when its callee is one of the registered points (its own
return path is audited there) or routes through the shared call itself; every
other call — `report['status'] = _clean()`, `report.update(_payload(report))` —
fails closed (`fix-r12`).  A helper whose only exit is a plain `raise` is not
an optimistic exit either, so the delegation family no longer reports it as
`inline exit code` (`fix-r12`).  Enumeration is symmetric across the two exit
families: every `sys.exit(...)` / `os._exit(...)` / `exit(...)` / `quit(...)`
call *and* every `raise SystemExit` / `raise SystemExit(...)`, whatever the
argument, is an exit-code computation of its own (`fix-r15`, `fix-r16`); an
attribute head (`helpers.cleanup_status(...)`) never inherits the `(rel, name)`
registration of the file it is written in, because the attribute may name any
module's member (`fix-r16`).  The registries that answer "is this call a proven
slot source" are asserted disjoint by the self test: a forwarding wrapper must
name the registered point it forwards to, and an exempt point that still
supplies a slot must carry its measured face as a reason (`fix-r16`).  A non-zero exit literal on its own
(`return 2` for BLOCKED, `return 3` for NOT_RUN) is not an optimistic exit and
stays legal because it cannot claim a clean run; a *pair* that hands back a
status dict with a literal code (`return {'status': 'BLOCKED'}, 2`) is still a
batch computation and is flagged, because the pair must come from the shared map
(`fix-r6` pins both sides).  The audit self-tests one hundred and three probe functions: one hundred and two inside `probe892` in
`check_cleanup.py` plus `_silent`, which is parsed and asserted outside that
function.  `probe892` is the authoritative list for the one hundred and two (the nine helpers
it defines — `_exit_code`, `_exit_a`, `_exit_b`, `_relay_zero`, `_mixed_code`,
`_bare_bail`, `_decoy_code`, `_exit_bare`,
`_exit_statement` — are leaves reached through a probe, not probes themselves;
`_raising_only` is asserted directly, not through a leaf).  Every rule below has a flagged probe and, where the shape is legal,
an accepted one: the original twenty-two covered the direct summary, the inline
`0 if ... else 1` branch, the legal shared pair, a summary returned through a local
name, a bare `return 0` / `return`, an optimistic conditional arm, a status slot
filled from a locally computed variable, `raise SystemExit(0)` / `SystemExit(None)`
/ `return None`, a clean verdict written into a `report['status']` slot, an exit
code relayed through a local variable, a status slot filled from a local dict
subscript, a local `aggregate_status` lookalike, a `report.update({'status':
'PASS'})` write, a duplicate `status` key, a `{**literal}` expansion, and a zero
initializer every following path overwrites (must stay legal).  The `fix-r5`
probes added thirteen: a zero branch that returns *before* the shared rebind, a
two-name relay, an `int(...)` computation, a zero read out of a local dict, a
nested helper returning a constant zero, a `report['verdict']` write, an alias
cycle, `sys.exit(0)`, a walrus write, an unresolvable name, a delegated exit, a
rebuilt summary, and a function whose only status evidence is a
`report.update(...)` write.  The `fix-r6` probes added nine: a boolean exit
expression, an exit code relayed into `sys.exit(...)`, a bare `raise
SystemExit`, a two-hop delegation chain, a decoy shared call beside a `return 0`,
an unproven subscript written into the slot, a summary keyed `verdict`, and the
legal / illegal pair around a non-zero exit literal.  The `fix-r7` probes added
five: a summary dict in a conditional arm, a conditional arm returning a *bound*
summary, an `update(**name)` expansion the function never built, a zero arm
inside an exit call's argument, and an exit call in `return` position.  The
`fix-r8` probes added three: an annotated `report: dict = {**report, ...}`
rebuild (the Assign branch's unpack re-judge did not reach the `AnnAssign`
shape), a bare `verdict = 'PASS'` name write (the write family accepted only the
`status` spelling), and `return bool(x)` as an exit code.  The `fix-r18`
probes added nine: a module-level `bail = sys.exit` alias, a function-local
`import sys as s` alias, a function-local `from sys import exit as bail`
alias, an alias bound inside a module-level `try:` block, an attribute status
write `report.status = 'PASS'`, and the four remaining write spellings
`report.__setattr__` / `dict.__setitem__(report, ...)` /
`operator.setitem(report, ...)` / `builtins.setattr(report, ...)`.
A `code = 0` initializer is therefore judged by
reachability, not by the presence of a shared assignment somewhere else in the
function: only a later statement that rebinds the name from the shared call on
every fall-through path (same block, both `if` arms, a `with` body, or a `try`
body whose handlers also rebind) makes it an initializer rather than an
optimistic exit, and a `return` / `SystemExit` between the two ends that
reachability proof (`fix-r5`), so an early exit out of a zero branch is flagged
even when a shared rebind follows it.  A delegated exit code is followed to the
leaf with a cycle guard: a return arm that calls a same-module function whose
returns do not all route their own code through the shared map, and one of which
can be a zero arm, is inline.  The exemption is per return, not per mention (a
decoy `common.aggregate_status(...)` call whose result is thrown away does not
excuse the `return 0` beside it), and only module-level defs are lookup
candidates, so a nested `def` of the same name cannot shadow the helper a
registered point really calls.  An incremental write whose
value is a name with no local binding is judged fail-closed, exactly like the
returned-dict path.  Registered points also
write their report incrementally instead of returning the dict, so every
`x['status'] = ...` assignment in a registered point is judged by the same rule:
a `'PASS'` / `'PASS_WITH_SKIPS'` / `'PASS_WITH_CLEANUP_UNVERIFIED'` literal
(directly or through a local variable) and an inline `all(...passed...)` are
flagged; the two halves are complement sets of `STATUS_WORDS`, so a new batch
word cannot join neither side (`fix-r9`); relays, shared calls and
non-batch labels such as `'PENDING_REVIEW'` stay legal.  That label allowance
is a *write*-path rule only: a summary dict that is *returned* with a status key
is judged element-wise, and any value that is neither a shared call nor a relay
is flagged, so a returned intermediate label would be a false FAIL today (no
healthy point returns one; `fix-r6` keeps the two paths separate on purpose).
Fixed in r3, no longer boundaries: an explicit `return None` / `return False`
from a registered point is an optimistic exit (only ``_guard._failure``, listed
in ``_SENTINEL_POINTS``, keeps ``None`` as its "propagate unchanged" sentinel),
a duplicate `status` key inside one summary dict is judged by every value, a
`{**literal}` expansion is walked, and a `status` / `verdict` key nested inside
another dict is read where the summary is returned or passed on.  Known
boundaries, not covered by this check: the module-level `raise
SystemExit(main())` that no function contains, a status key built by
`dict(**kwargs)`, by a comprehension, by a bare splat of an opaque name
(`return {**payload}` — `_dict_has_status` only reads `Constant` keys), or
through a non-constant subscript (`key = 'status'; report[key] = 'PASS'` — the
write family matches a ``Constant`` slice only), a third top-level summary
spelling
(`overall`, `outcome`) — the key family is exactly the two spellings in
`_STATUS_KEYS`, so an unrecognised top-level key yields no marker, no
aggregation point and no failure until the spelling is added there, and one assignment shape that a static
check cannot separate from healthy code — a *fresh* dict literal written into a
name that also feeds the shared call (`result = {'status': 'PASS'}; return
common.aggregate_status(result['status'])[1]`).  That is exactly the healthy
per-case verdict in `scripts/gateway-parity/controls/session_domain.py:main`,
and the identical shape with a batch summary is a hardcoded PASS; the audit
therefore judges every other write (subscript, `update`, `|`, walrus,
unresolvable name, a keyword expansion `update(**name)`, a dict rebuilt with
`{**name}`) but exempts the fresh literal
for such a name.  The batch status of both registered `main` points is instead
pinned at runtime: the published status must equal what the one shared
aggregation returns for the observed cell statuses, and a `--case all` replay
with one failing case must come out `FAIL`/exit 1 (`caseFailure`), so a clean
status hardcoded into the shared call's argument list is caught even though no
slot shows it.  The exit-code reachability judgement errs
strict: an overwrite that can be skipped (`for` / `while` bodies, `match` cases)
does not count, so an initializer overwritten only inside a loop is still
flagged — a false FAIL on unusual code, never a false PASS.  The
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
restored positive.  The batch holds the host-wide parity lease for its whole
run, so it keeps that hold inside the apparatus's own 900 s run bound
(`LEASE_BUDGET_SECONDS`), restored positive and kill grace included: when the
remaining budget can no longer absorb one check plus that tail, the rest of the
batch is recorded `INVALID` (which aggregates to BLOCKED, never to a kill) and
the run says to continue with `--only` (`fix-r9`, tightened in `fix-r10` after
MiniMax concurrency r7 P1-1 / GLM boundary r9 P1-1).  The bound does not remove
the pre-existing race with a suite that is already waiting on the same lease
with its own 30 s deadline; shrinking the lease to the rewrite window needs the
shared `mutating_sources` change tracked in #1719. `MUTATIONS` is a literal `(name, source, old, new)` table
read by #820's anchor selfcheck; per-mutant mode/judge metadata lives in
`JUDGES`, keyed by the same names. The current table has 331 mutants:
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
`retirement-duplicate-status-key`, `controls-nested-same-name`,
`calibrate-shared-excuse`, `calibrate-status-setdefault`,
`calibrate-status-update-kwarg`, `calibrate-early-return-branch`,
`calibrate-two-hop-zero`, `calibrate-int-call-code`,
`calibrate-dict-code-zero`, `calibrate-nested-def-zero`,
`calibrate-verdict-write`, `calibrate-sys-exit-zero`,
`calibrate-walrus-status`, `calibrate-rebuilt-summary`,
`calibrate-boolean-exit`, `calibrate-sys-exit-relay`,
`calibrate-system-exit-bare`, `calibrate-delegated-chain`,
`calibrate-container-slot`, `calibrate-alias-cycle`,
`calibrate-verdict-dict`, `calibrate-clean-argument`,
`calibrate-conditional-summary`, `calibrate-update-expansion`,
`calibrate-exit-conditional`, `calibrate-return-exit-call`,
`calibrate-ann-rebuilt`, `calibrate-bare-verdict`, `calibrate-bool-exit`,
`calibrate-bool-relay`, `calibrate-unbound-exit-name`,
`calibrate-update-positional`, `calibrate-augassign-update`,
`calibrate-tuple-verdict`, `calibrate-cleanup-word`,
`calibrate-unbound-exit-relay`, `calibrate-no-exit-path`,
`calibrate-delegated-attribute`, `calibrate-update-unregistered`,
`calibrate-walrus-exit`,
`calibrate-helper-bare-return`, `calibrate-helper-exit-call`,
`calibrate-augassign-unregistered`, `calibrate-opaque-clean-write`,
`calibrate-opaque-update-payload`, `calibrate-param-augassign`,
`calibrate-param-update`, `calibrate-helper-local-relay`,
`calibrate-helper-bare-sysexit`, `calibrate-attr-head-callee`,
`calibrate-mixed-branch-helper`, `calibrate-cross-module-name`,
`calibrate-exempt-callee-write`, `calibrate-param-augassign-opaque`,
`calibrate-quit-exit`, `calibrate-partial-raise-point`,
`calibrate-augassign-cross-file-call`, `calibrate-exit-call-unregistered`,
`calibrate-tuple-status-target`, `calibrate-attr-collision`,
`calibrate-bare-raise-exit`, `calibrate-alias-sysexit`,
`calibrate-alias-system-exit`, `calibrate-setattr-status`,
`calibrate-dunder-setitem-status`, `calibrate-local-import-alias`,
`calibrate-module-alias-binding`, `calibrate-attr-status`,
`calibrate-dunder-setattr-status`, `calibrate-unbound-setitem-status`,
`calibrate-builtins-setattr-status`, `calibrate-operator-setitem-status`,
`calibrate-try-assign-alias`, `calibrate-attr-exit-code`,
`calibrate-subscript-exit-code`, `calibrate-attr-code-relay`,
`calibrate-try-else-completes`, `calibrate-exit-call-attribute`,
`calibrate-exit-call-subscript`, `calibrate-shadowed-common`,
`calibrate-computed-status-write`, `calibrate-local-exit-alias-chain`,
`calibrate-computed-status-key`, `calibrate-computed-status-key-write`,
`calibrate-shadowed-common-local`, `calibrate-shadowed-receiver-local`,
`calibrate-shadowed-common-global`, `calibrate-shadowed-receiver-global`,
`calibrate-shadowed-receiver-poison-fn`,
`calibrate-shadowed-receiver-same-fn`, `calibrate-redefined-loader`,
`calibrate-conditional-def`, `calibrate-decorated-source`,
`calibrate-nested-def-rebind`, `calibrate-walrus-rebind`,
`calibrate-dynamic-globals-write`, `calibrate-sys-modules-poison`,
`calibrate-shadowed-global-loader`, `calibrate-module-walrus`,
`calibrate-dynamic-receiver-write`, `calibrate-setattr-shared-write`,
`calibrate-exec-nonliteral`, `calibrate-negative-process-redefine`,
`calibrate-decorated-guard-load`, `calibrate-direct-attribute-write`,
`calibrate-dict-mutation-keys`, `calibrate-sys-modules-wildcard`,
`calibrate-single-definition-dynamic`, `calibrate-dynamic-shared-name`,
`calibrate-builtin-alias-setattr`, `calibrate-vars-dict-receiver`,
`calibrate-loop-key-rebind`, `calibrate-definition-header-walrus`,
`calibrate-sys-modules-chain-poison`, `calibrate-walrus-receiver-local`,
`calibrate-dict-namespace-update`, `calibrate-dict-namespace-augassign`,
`calibrate-dunder-setattr-receiver`, `calibrate-object-setattr-unbound`,
`calibrate-dict-alias-write`, `calibrate-import-call-modules`,
`calibrate-dict-update-unbound`, `calibrate-dict-setdefault-unbound`,
`calibrate-dict-pop-unbound`, `calibrate-dict-setitem-unbound`,
`calibrate-helper-setattr`, `calibrate-operator-setitem`,
`calibrate-getattr-dunder-write`, `calibrate-getattr-builtin-write`,
`calibrate-partial-write`, `calibrate-partial-handle`,
`calibrate-tuple-attribute-write`, `calibrate-eval-literal-write`,
`calibrate-exec-receiver-dict`, `calibrate-locals-write`,
`calibrate-namespace-unbound-update`, `calibrate-builtins-type-alias`,
`calibrate-dict-alias-augassign`, `calibrate-walrus-receiver-write`,
`calibrate-alias-chain-reverse`, `calibrate-sm-pair-value`,
`calibrate-sm-loop-rebind`, `calibrate-sm-unbound-update`,
`calibrate-getattr-dict-subscript`, `calibrate-builtins-subscript-setattr`,
`calibrate-getattr-import-alias`, `calibrate-builtins-module-alias`,
`calibrate-builtins-getattr-subscript`, `calibrate-builtins-subscript-vars`,
`calibrate-write-handle-name`, `calibrate-write-handle-getattr`,
`calibrate-write-handle-attrgetter`, `calibrate-star-setattr`,
`calibrate-star-forward`, `calibrate-unpack-receiver`,
`calibrate-for-receiver`, `calibrate-default-receiver`,
`calibrate-with-receiver`, `calibrate-with-key-rebind`,
`calibrate-globals-tuple-target`, `calibrate-sm-callable-setitem`,
`calibrate-sm-mapping-augassign`, `calibrate-builtins-alias-subscript`,
`calibrate-getattr-dynamic-call`, `calibrate-globals-alias-augassign`,
`calibrate-status-getattr-alias`, `calibrate-global-walrus-rebind`,
`calibrate-unpack-star-target`, `calibrate-unpack-star-value`,
`calibrate-unpack-iter-for`, `calibrate-with-handle-name`,
`calibrate-getattr-handle-name`, `calibrate-getattr-handle-dict`,
`calibrate-nonlocal-common-rebind`, `calibrate-builtin-last-binding`,
`calibrate-partial-prebound`, `calibrate-partial-name-status`,
`calibrate-builtins-chain-four`,
`calibrate-closure-default`, `calibrate-global-handoff`,
`calibrate-star-from-name`, `calibrate-star-target-rest`,
`calibrate-handle-annassign`, `calibrate-handle-argument`,
`calibrate-ordered-binding`, `calibrate-partial-update-prebound`,
`calibrate-partial-setdefault-prebound`,
`calibrate-builtins-chain-fiftyone`, `calibrate-loop-backedge-alias`,
`calibrate-sameline-restore`, `calibrate-shared-seen-paths`,
`calibrate-decorator-lambda-scope`, `calibrate-nonlocal-walrus-withdraw`,
`calibrate-handle-list-literal`, `calibrate-handle-dict-literal`,
`calibrate-handle-yield`, `calibrate-getattribute-handover`,
`calibrate-handle-dictcomp-escape`, `calibrate-handle-listcomp-escape`,
`calibrate-handle-aliased-escape`,
`calibrate-getattribute-write-attribute`,
`calibrate-getattr-const-subscript-write`, `calibrate-match-capture-helper`,
`calibrate-comprehension-write-helper`, `calibrate-mixed-alias-binding`,
`calibrate-handle-container-alias`, `calibrate-handle-mapping-write`,
`calibrate-list-const-subscript-write`,
`calibrate-comprehension-filter-write`,
`calibrate-match-capture-receiver`,
`calibrate-match-capture-setattr-receiver`,
`calibrate-getattribute-attr-spelling`,
`calibrate-getattribute-name-spelling`, `calibrate-for-wrapped-container`,
`calibrate-for-target-after-loop`, `calibrate-for-slice`,
`calibrate-for-copy`, `calibrate-comp-result-receiver`,
`calibrate-for-target-rebind-if`, `calibrate-for-target-after-match`,
`calibrate-comp-dict-result`, `calibrate-comp-name-iter`,
`calibrate-for-target-return`, `calibrate-for-target-alias`,
`calibrate-starred-const-subscript-write`, `calibrate-comp-handle-relay`,
`calibrate-for-target-finally-rebind`, `calibrate-for-target-closure`,
`calibrate-for-target-shadowed-fresh`, `calibrate-for-target-with-alias`,
`calibrate-for-target-except-alias`, `calibrate-for-target-name-iter`,
`calibrate-comp-handle-relay-for`, `calibrate-comp-handle-relay-chain`,
`calibrate-for-target-assign-call`, `calibrate-for-target-def-default`,
`calibrate-for-target-readonly-call-leak`,
`calibrate-for-target-nested-param`,
`calibrate-for-target-compare-call-leak`,
`calibrate-for-target-outer-shadow-fresh`,
`calibrate-for-target-outer-shadow-print`,
`calibrate-for-target-ambiguous-literal`,
`calibrate-for-target-comp-element-call`,
`calibrate-for-target-call-attribute`,
`calibrate-for-target-call-attribute-alias`,
`calibrate-for-target-false-filter-write`,
`calibrate-for-target-class-body-clear`, `calibrate-for-target-with-suppress`,
`calibrate-for-target-class-body-shadow`,
`calibrate-for-target-lambda-default`, `calibrate-for-target-class-body-def`,
`calibrate-for-target-method-self`,
`calibrate-for-target-conditional-literal-rebind`,
`calibrate-for-target-enclosing-binding-after-use`,
`calibrate-for-target-tuple-partial-alias`,
`calibrate-for-target-comp-partial-shadow`,
`calibrate-for-target-assert-msg`,
`calibrate-for-target-enclosing-call-between-rebinds`,
`calibrate-for-target-class-body-compound`,
`calibrate-for-target-class-method-closure`,
`calibrate-for-target-class-body-shadowed-constructor`,
`calibrate-for-target-module-import-alias`,
`calibrate-for-target-class-body-copy-alias`,
`calibrate-for-target-nested-class-method`, `calibrate-for-target-conditional-rebind`,
`calibrate-for-target-loop-back-edge`, `calibrate-for-target-same-line-rebind`,
`calibrate-for-target-class-block-alias`, `calibrate-for-target-nested-copy-alias`,
`calibrate-for-target-module-call-before-rebind`,
`calibrate-for-target-module-self-assignment`,
`calibrate-for-target-module-loop-first-use`,
`calibrate-for-target-duplicated-unpack-target`,
`calibrate-for-target-module-call-site-loop-edge`,
`calibrate-for-target-default-value-call`,
`calibrate-for-target-assignment-back-edge`,
`calibrate-for-target-handle-conditional-overwrite`,
`calibrate-for-target-scoped-handle-receiver`,
`calibrate-for-target-shadowed-constructor`,
`calibrate-for-target-handle-container-copy`,
`calibrate-status-bound-handle-alias-chain`,
`calibrate-status-three-arg-handle-live`,
`calibrate-status-getattr-alias-callee`,
`calibrate-for-target-nested-container-copy`,
`calibrate-status-alias-copied-before-overwrite`,
`calibrate-for-target-container-filled-after-rebind`,
`calibrate-status-imported-getattr-factory`,
`calibrate-for-target-alias-of-container-copy`,
`calibrate-status-builtins-module-alias`,
`calibrate-status-factory-import-after-assignment`,
`calibrate-status-factory-import-conditional-overwrite`,
`calibrate-status-builtins-alias-self-assign`,
`calibrate-status-builtins-subscript-copy`.
The batch-related
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
second; `controls-nested-same-name` nests a second `def main`.  The three
`fix-r4` mutants pin what the r3 reviews found and the review of this round
required to stay separate: `calibrate-shared-excuse` writes `code = 0` into one
`session_domain.py:main` suite branch while the other branch still calls the
shared function (the reachability rule must flag it, and the zero initializer
above the `try` must stay legal); `calibrate-status-setdefault` writes the slot
through `setdefault()`; `calibrate-status-update-kwarg` passes the slot as a
keyword argument to `update()`.  The nine `fix-r5` mutants pin the vectors the
r4 reviews found.  Six of them are written as an extra bypass branch *in front
of* the untouched shared exit/rebind in `session_domain.py:main` (`A_EXIT`), so
the healthy fall-through keeps its shared assignment.  Note what that does
*not* buy: any extra `return` inside the guarded `try` makes the `code = 0`
initializer above it reachable, so those mutants also trip the initializer rule
and their judges must name their own rule's message (`calibrate-two-hop-zero`
originally named the initializer's `code` instead of its own `relay`, corrected
in `fix-r9` after MiniMax boundary r7 P2-2).  The other three r5 mutants replace
the summary status write (`B_STATUS`) itself, which is the write being pinned
there.  All of them
--- like the `fix-r6` mutants below --- use a synthetic `report.get('special')`
guard.  Those guards are *static* triggers: no real report carries a `special`
key, so the mutants prove that the rule fires on the shape, not that the branch
is reachable at runtime: `calibrate-early-return-branch` returns the zero code before the shared
rebind; `calibrate-two-hop-zero` relays `0` through two locals;
`calibrate-int-call-code` computes the code with `int(...)` into a local;
`calibrate-dict-code-zero` reads a zero back out of a local dict;
`calibrate-nested-def-zero` returns a nested helper's constant zero;
`calibrate-verdict-write` writes the clean verdict into `report['verdict']`;
`calibrate-sys-exit-zero` calls `sys.exit(0)`; `calibrate-walrus-status` writes
`(status := 'PASS')`; `calibrate-rebuilt-summary` rebuilds the summary with
`{**report, 'status': 'PASS'}`.  The eight `fix-r6` mutants pin what the r5
reviews found: `calibrate-boolean-exit` returns `not ok`; `calibrate-sys-exit-relay`
passes a local dict's zero to `sys.exit(...)`; `calibrate-system-exit-bare` raises
`SystemExit` with no argument; `calibrate-delegated-chain` delegates the exit code
twice (`_exit_a` -> `_exit_b`); `calibrate-container-slot` fills the slot from an
unproven subscript; `calibrate-alias-cycle` feeds the slot through a swap cycle;
`calibrate-verdict-dict` builds and prints a summary keyed `verdict` (the
enumeration side of the key family); `calibrate-clean-argument` hardcodes the
clean status into the shared call's *argument* list
(`common.aggregate_status('PASS', cleanup['status'])`), which no slot can see —
it is pinned at runtime instead, by the `caseFailure` row of
`--mode batch-aggregation` (a failing case must reach the batch status).  The first six replace the one shared exit
line in `controls/session_domain.py:main` (`CT_EXIT`); `calibrate-verdict-dict`
prepends a function to that module, and `calibrate-clean-argument` is pinned at
runtime instead (see above).  The
four `fix-r7` mutants pin the return-path shapes the r6 reviews found still
silent: `calibrate-conditional-summary` returns `{'status': 'PASS'}` from one arm
of a conditional, `calibrate-update-expansion` passes an unbound name through
`update(**extra)`, `calibrate-exit-conditional` calls `sys.exit(0 if ... else
1)`, and `calibrate-return-exit-call` writes `return sys.exit(0)`.  All four
replace the one shared exit line in `controls/session_domain.py:main`
(`CT_EXIT`).  The three `fix-r8` mutants pin the remaining write/exit spellings:
`calibrate-ann-rebuilt` rebuilds the summary through an annotated assignment,
`calibrate-bare-verdict` writes `'PASS'` into a bare `verdict` name, and
`calibrate-bool-exit` returns `bool(...)`.  The six `fix-r9` mutants pin the
relay and write spellings: `calibrate-bool-relay` relays `bool(...)` through a
local (the relay arm used to know only `int`), `calibrate-unbound-exit-name`
returns a name with no local binding, `calibrate-update-positional` passes the
payload to `update()` positionally, `calibrate-augassign-update` writes it with
`|=`, `calibrate-tuple-verdict` writes `'PASS'` through a tuple target, and
`calibrate-cleanup-word` writes the cleanup-verification word into a status
slot.  The five `fix-r10` mutants pin the r9-round gaps: `calibrate-unbound-exit-relay`
relays an unbound `EXIT_OK` through a local, `calibrate-no-exit-path` drops the
return *and* keeps a shared call so the point is registered but never exits,
`calibrate-delegated-attribute` delegates to `helpers.exit_code(report)`,
`calibrate-update-unregistered` gives an unregistered function an opaque
`update()` payload, and `calibrate-walrus-exit` relays a walrus-bound `int()`
code (the r9 round proved that spelling only with an in-file probe).  The three
`fix-r11` mutants pin the r10-round gaps: `calibrate-helper-bare-return` returns
a module helper that ends in bare `return` (the caller's process exits 0 one
call away, and the helper is a real module function so the delegation arm sees
it rather than failing closed on an unknown name),
`calibrate-helper-exit-call` puts `raise SystemExit(0)` in statement position
inside a nested helper, and `calibrate-augassign-unregistered` gives an
unregistered function `report |= {'status': 'PASS'}`.  The seven `fix-r13`
mutants pin the r12-round gaps: `calibrate-helper-local-relay` stores `0` in a
helper local and returns it, `calibrate-helper-bare-sysexit` raises a bare
`SystemExit` (which `SystemExit(None)` makes a zero exit), `calibrate-attr-head-callee`
takes a status slot from `helpers._clean()` — an attribute-headed callee the
fail-closed call rule used to wave through — `calibrate-mixed-branch-helper`
raises on one branch and falls off the end on the other,
`calibrate-cross-module-name` defines a lookalike of a registered point's name
in another module, `calibrate-exempt-callee-write` uses an exempted-but-unaudited
callee as a slot source, and `calibrate-param-augassign-opaque` applies `|=` to
a parameter with an opaque payload.  The two `fix-r14` mutants pin the
r13-round gaps: `calibrate-quit-exit` exits through the other site builtin,
`quit(0)`, which raises the same `SystemExit` as `exit(0)` and used to reach no
rule at all, and `calibrate-partial-raise-point` registers a point that raises
on one branch and falls off the end on the other — the point-level reachability
rule is now the same one the helper arm uses.  The three `fix-r15` mutants pin the
r14-round gaps: `calibrate-augassign-cross-file-call` applies `|=` to a summary
with a call payload whose callee is a local lookalike of a registered point in
*another* file (the enumeration had dropped `module`/`rel`, so the payload was
trusted by bare name), `calibrate-exit-call-unregistered` gives an unregistered
point `sys.exit('BLOCKED')` as its only evidence (the raise family enumerated a
status payload, the call family enumerated nothing), and
`calibrate-tuple-status-target` writes `report['status'], code = 'PASS', 0` (the
enumeration only knew the plain subscript target, while the slot layer has
expanded tuple targets since `fix-r9`).  The two `fix-r16` mutants pin the r15-round gaps:
`calibrate-attr-collision` writes a status through an **attribute** callee
(`helpers.cleanup_status(audit)`) inside the very file that registers
`cleanup_status` — the bare attribute name matched the `(rel, name)` registry, so
the write looked proven (GLM boundary r15 P1-1); `calibrate-bare-raise-exit`
gives an unregistered point `raise SystemExit(0)` as its only evidence, the
optimistic spelling the raise family skipped while the exit-call family already
enumerated every call (MiniMax sibling r15 P1-1).  The four `fix-r17` mutants pin the r16-round gaps:
`calibrate-alias-sysexit` gives an unregistered point `import sys as s` plus
`s.exit(0)` as its only evidence (the exit family matched the literal names
`sys.exit` / `os._exit` / `exit` / `quit`, so every alias spelling walked past
all of them — GLM boundary r16 P0-1); `calibrate-alias-system-exit` does the
same through `from builtins import SystemExit as SE; raise SE(0)`; and
`calibrate-setattr-status` / `calibrate-dunder-setitem-status` write a clean
status into a registered point's summary through `setattr(report, 'status',
'PASS')` / `report.__setitem__('status', 'PASS')`, the two spellings the write
family (`update` / `setdefault` / `|=`) never read (GLM boundary r16 P1-1).
Alias resolution is one resolver (`_exit_target`) used by all three predicate
families, fed from three places: module-scope `import ... as` / `from ...
import ... as` including a `try:`-guarded optional import
(`_module_exit_aliases`), the function's own bindings (`_binding_exit_aliases`,
e.g. `bail = sys.exit`), and the function's own `import` statements -- the
shared `_import_exit_aliases` helper reads the same statements from either
scope, so a function-local `import sys as s` no longer resolves `s` as an
unknown bare name (`fix-r18`).

The seven `fix-r18` mutants pin the r17-round gaps:
`calibrate-local-import-alias` gives an unregistered point a *function-local*
`import sys as s` plus `s.exit(0)` (the module walk read direct children only
and the binding map never collected `import` statements, so the shape was
invisible -- GLM boundary r17 P0-1); `calibrate-module-alias-binding` binds
`bail = sys.exit` at module level and calls `bail(0)` (GLM sibling r17 P1-1);
`calibrate-attr-status` writes `report.status = 'PASS'` and
`calibrate-dunder-setattr-status` writes it through
`report.__setattr__('status', 'PASS')` (neither the enumeration nor the slot
layer had an attribute branch -- MiniMax sibling r17 P1-1); and
`calibrate-unbound-setitem-status` / `calibrate-builtins-setattr-status` /
`calibrate-operator-setitem-status` write the slot through the unbound
three-argument spellings `dict.__setitem__(report, ...)` /
`builtins.setattr(report, ...)` / `operator.setitem(report, ...)`
(GLM sibling r17 P2-2).

The five `fix-r19` mutants pin the r18-round gaps:
`calibrate-try-assign-alias` binds `bail = sys.exit` inside a module-level
`try:` block and calls `bail(0)` — the shape `_guard.py` itself uses — which
the module-level assignment pass missed because it walked direct children only
(GLM boundary r18 P1-1); `calibrate-attr-exit-code` /
`calibrate-subscript-exit-code` / `calibrate-attr-code-relay` hand an exit code
back as `helpers.EXIT_OK`, as `CODES[policy]`, and as a local name assigned
from an attribute, three spellings the return arm had no branch for, so the
value was judged by no rule at all (GLM boundary r18 P0-1); and
`calibrate-try-else-completes` delegates to a helper whose `except` handler
falls off its own end while `else` returns, which the old `_body_completes`
folded into one flag so the `else` branch cancelled the completing handler
(MiniMax boundary r18 P0-1).  An exit code is now judged the same way whether
it is spelled as a bare name, an attribute or a subscript: an origin no local
binding can prove is fail-closed (`calibrate-subscript-exit-code`,
`calibrate-exit-call-subscript`), and a subscript whose base routes through
the shared call stays legal -- the skip `_exit_call_violation` makes for
`_calls_aggregate`, which the healthy tree's own
`common.aggregate_status(...)[1]` spellings exercise on every run (GLM sibling
r20 P2-3, a stale name pointed at no registered control).

The restore window is shielded by `fix-r18`: `terminate_cleanly` raises
`SystemExit` from the signal handler so the restore `finally` unwinds, and a
raise landing on a bytecode boundary *inside* the write-back loop exited
mid-restore and left a mutant in the tree, which the next run read as a real
kill (MiniMax concurrency r17 P1-1).  `negative_process.restore_critical()`
makes the handler record the signum instead (`defer_signal_exit`), and the
pending exit is taken once after the restore and its byte verification
(`take_deferred_signal`).  `fix-r19` moved the protection into the window
itself: a consumer-side call can only protect the one fixture that remembers to
make it, so `restore_critical()` now swaps the SIGINT / SIGTERM / SIGHUP /
SIGQUIT handlers for its own recorder while the window is open and puts them
back afterwards (a handler that is `SIG_IGN` stays ignored, and a handler
installed *inside* the window survives it).  That closes a plain Ctrl-C, for
which no fixture installed a handler at all, and every sibling handler on those
four signals -- including one that raises `SystemExit` directly -- because the
window owns SIGINT / SIGTERM / SIGHUP / SIGQUIT while it is open, whatever was
installed before (MiniMax sibling r18 P1-2 / GLM sibling r18 P1-3).  The
deferral protocol is checked deterministically on every import of
`negative_controls.py`, and the real-signal path is covered by nine
`test_negative_process.RestoreWindowSignals` cases: SIGINT with the default and
with a fixture handler, SIGTERM with a fixture handler, SIGHUP with the default
handler, SIGQUIT with the default handler (``^\`` is catchable like the rest;
GLM sibling r20 P2-4), the first of two signals inside one window winning
(anthropic concurrency r20 P2-1), an ignored signal staying ignored, a failing
body not leaking the deferred signum (MiniMax boundary r19 P2), and the
handlers being put back afterwards (the signal/processor matrix is not fully
crossed -- the signal/processor combinations the fixture actually exercises are
the ones listed); the arbitrary bytecode boundary is not reproducible by
construction and is recorded as such in the PR.

`fix-r20` closed four r19 review findings, each with its own mutation:
`calibrate-exit-call-attribute` / `calibrate-exit-call-subscript` show that the
r19 attribute/subscript rule had reached the `return` arm only, so
`sys.exit(helpers.EXIT_OK)` and `raise SystemExit(CODES[policy])` were judged by
no rule at all (GLM sibling r19 P1 / MiniMax boundary r19 P0);
`calibrate-shadowed-common` rebinds `common` to a `SimpleNamespace` stub, which
the spelling-based `_is_shared_call` accepted as the shared aggregator (GLM
boundary r19 P1-1) -- a file now keeps that trust only when it binds the name
through the apparatus loader (`_load('common', ...)`), and a file that rebinds
it fails the audit outright instead of silently losing its points;
`calibrate-computed-status-write` writes `'PA' + 'SS'`, the same literal to the
interpreter but not a `Constant` node, which neither the write family nor
`_status_literals` could see (GLM boundary r19 P1-2); and
`calibrate-local-exit-alias-chain` is the two-hop `bail = sys.exit; other =
bail` alias the function-level pass resolved only in a favourable source order
(MiniMax boundary r19 P1).  `_git` also gained its own session and a 120 s hard
timeout: it runs inside the shielded restore window, where an interrupt cannot
land, so a blocked git would have held the window open forever (MiniMax
concurrency r19 P1).

`fix-r21` closed the three r20 P1 findings, each with its own mutation:
`_status_key` now judges every *key-side* spelling of a status word through
the same `_folded_string` the value side already used, so `{'sta' + 'tus':
'PASS'}` and `report['sta' + 'tus'] = 'PA' + 'SS'` reach all eight key
judgement sites the plain `Constant` spelling did
(`calibrate-computed-status-key` / `calibrate-computed-status-key-write`, GLM
sibling r20 P1-1); and trust withdrawal for a shadowed `common` moved from
module scope into each function body -- a function-scope
`common = SimpleNamespace(...)`, over the parameter itself included, a
locally rebound relay receiver (`md = <stub>` then
`md.common.aggregate_status`), and a `global common` rebind that poisons the
module binding file-wide all lose the shared-call credit
(`calibrate-shadowed-common-local` / `calibrate-shadowed-receiver-local` /
`calibrate-shadowed-common-global`, GLM sibling r20 P1-2 / boundary r20 P1-1).
The two shapes the healthy tree legitimately keeps are registered in
`PROVEN_SHARED_SOURCES` (negative_controls.py's committed-HEAD loader and
pristine-common loader) and the loader spelling, and `check_cleanup.main`'s
`global md` is carved out because it rebinds a receiver, not a shared module.
The same round hardened the harness on four r20 P2s: SIGQUIT joined
`_SHIELDED_SIGNALS` (with its own `RestoreWindowSignals` case), the deferred
signum is now keep-first so a second signal never overwrites the first (with
the two-signal case), `_git`'s timeout arm group-kills and collects boundedly
like `run_owned`, and the restored-positive check applies the same strict
`is True` / `is None` predicate on non-Darwin that Darwin always did.

`fix-r22` repaired the two `test_restore.py` real-guard anchors the r18
re-indentation of the restore loop had gone stale on: the write anchor kept the
old 20-space depth, which matched the 24-space line as a substring and spliced
the `try` mid-line (CI `IndentationError`), and the head anchor kept 28 spaces
against a 32-space f-string (`ValueError`, stale anchor).  Both anchors now
follow the source's real indentation, and the #1110 preflight runs clean.

`fix-r23` closed the three r22 P0/P1 findings, each with its own mutation or
test: receiver-relay trust now has a module/global layer (`_withdrawn_receivers`)
-- a relay receiver (`md.common.aggregate_status`) keeps the shared-call credit
only while every module-scope binding of it is a proven loader or a registered
proven source, so `md = <stub>` at module scope, an unrelated
`def _poison(): global md; md = <stub>`, and `global md; md = <stub>` inside the
registered point itself all lose the relay file-wide
(`calibrate-shadowed-receiver-global` / `-poison-fn` / `-same-fn`, GLM boundary
r22 P0-1; `load_session_domain` is registered in `PROVEN_SHARED_SOURCES`, and
the `md = None` placeholder initializer cannot serve a verdict so it does not
withdraw); the apparatus loader is no longer callee-blind -- a name keeps the
loader role only while every module-scope binding of it is the `<name>.load`
attribute, so a `def _load(name, path)` inserted after `_load = _guard.load`
keeps the call spelling while returning a stub and now fails the audit
(`calibrate-redefined-loader`, GLM boundary r22 P1-2; `_guard.py`'s own
`def load` stays the one sanctioned definition); and the harness batch loop now
registers SIGQUIT with `terminate_cleanly` too, not only SIGTERM/SIGHUP
(`_install_terminate_handlers`, anthropic sibling r22 P1 / GLM boundary r22 P2
taken at the higher grade; two `test_negative_process.TerminateHandlerRegistration`
cases prove both directions at runtime -- with the registration a SIGQUIT
outside the window unwinds through `finally` and exits 128+SIGQUIT, without it
the same signal kills in place and the `finally` never runs).

`fix-r24`+ replaced the "trust the definition name" proof with a module
binding-graph proof: `_proven_source_names` / `_loader_names` /
`_function_shared_names` / `_withdrawn_receivers` now read
`_module_scope_binding_map`, so a name keeps the shared-call credit only while
every binding of it is the registered definition or the apparatus loader.
`fix-r25` closed the r24 review's P0/P1 shapes, each with its own mutation:
a same-named `def` inside a module-level `if`/`try`
(`calibrate-conditional-def`), a decorator that replaces the function object
(`calibrate-decorated-source`), a nested `def` and a walrus target rebinding a
proven source inside a function (`calibrate-nested-def-rebind` /
`calibrate-walrus-rebind`), a literal `globals()['<registered>'] = <stub>`
(`calibrate-dynamic-globals-write`), a `sys.modules['_guard']` pre-seed
(`calibrate-sys-modules-poison`), and a `global common` rebind routed through a
locally shadowed loader (`calibrate-shadowed-global-loader`).

`fix-r26` closed the r25 review's P0/P1 shapes.  Every module-scope binding
form now reaches the graph: a module-level walrus or `match` capture
(`calibrate-module-walrus`; `_module_extra_bindings`), a dynamic write into the
module namespace from inside a function
(`calibrate-dynamic-receiver-write`; `_dynamic_write_names` walks the whole
file, and `_withdrawn_receivers` consumes it), its one-hop-away spellings --
`globals()`/`vars()` aliases, `globals().update({...})`,
`setattr(<module object>, ...)` and an `exec` whose source is not a literal
(`calibrate-exec-nonliteral`) -- and the relay receiver's shared *attribute*
(`calibrate-setattr-shared-write`, resolved through the literal dict the key
comes from so the apparatus's own `for name, value in patched.items()` control
is not withdrawn).  The `sys.modules` rules accept the aliases a real file can
use (`import sys as _s`, `_s = sys`, `from sys import modules as _m`), and a
pre-seed only counts as harmless when its value is a *proven* module.  Imports
inside `if TYPE_CHECKING:` bind nothing.  Two cross-file rules were missing
their target file entirely: `PROVEN_SHARED_DEFINITIONS` now holds the defining
file of a cross-file proven source to the same single-undecorated-def shape
(`calibrate-negative-process-redefine`), and `_guard.py` must keep exactly one
undecorated `load` (`calibrate-decorated-guard-load`).

`fix-r27` closed the r26 review's P0/P1 shapes.  The five per-rule alias
collectors became one resolver: `_object_kinds` is a fixpoint over every
`Assign`/`AnnAssign`/walrus binding and over `import sys as _s` /
`from sys import modules` / `from builtins import setattr as _sa`, and yields
the kinds `sys`, `sysmods`, `namespace`, `module`, `builtins` and
`fn:<builtin>`; `_dynamic_write_names`, `_sys_modules_write_values`,
`_receiver_attribute_writes`, `_is_namespace_object`, `_is_module_object` and
`_callable_name` all read it, so a spelling taught to one rule reaches the
others (GLM concurrency r26 P1-1 / DeepSeek sibling r26 P1-5).  The receiver's
shared attribute is withdrawn by the plain `recv.<shared> = <stub>`
(`calibrate-direct-attribute-write`), by `recv.__dict__[<key>]` /
`vars(recv)[<key>]` (`calibrate-vars-dict-receiver`), through an alias root
(`m = md`) and through a `from builtins import setattr` spelling
(`calibrate-builtin-alias-setattr`); a `for` target that is reassigned inside
the loop body no longer supplies the key (`calibrate-loop-key-rebind`), and a
literal key dict mutated after its binding is unbounded rather than stale
(`calibrate-dict-mutation-keys`).  `sys.modules[<name>] = ...` resolves the key
through the enclosing `for` loop's literal pair sequence --
`for name, previous in (('common', saved), ...)` restores each entry from the
value paired with *its own* key, which is why the harness's own restore stays
green -- while a key that cannot be resolved lands on the `'*'` sentinel and
that sentinel is now a real verdict
(`calibrate-sys-modules-wildcard`); the poisoning pass is a fixpoint, so a key
pre-seeded from another pre-seeded key's read no longer launders itself
(`calibrate-sys-modules-chain-poison`).  A literal `exec` or `globals()` write
reaches `_single_definition` (`calibrate-single-definition-dynamic`),
`_module_shared_names` (`calibrate-dynamic-shared-name`) and `_loader_names`,
and the single-definition rule accepts only a `def`/`async def`, never a
`class`.  Function-local rebinds now include walrus targets and `match`
captures (`calibrate-walrus-receiver-local`), and a definition *header* --
decorators, defaults, annotations, bases -- is walked in the enclosing scope,
so a walrus there counts (`calibrate-definition-header-walrus`).

`fix-r28` closed the r27 self-audit's write spellings.  r27 resolved the
builtin `setattr` and the two subscript roots, so every *other* spelling of the
same write still answered green: the namespace mapping's own mutators
(`recv.__dict__.update(common=<stub>)` /
`setdefault` / `pop` / `clear`, `calibrate-dict-namespace-update`), the
in-place replacement `recv.__dict__ |= {'common': <stub>}`
(`calibrate-dict-namespace-augassign`), the bound method
`recv.__setattr__('common', <stub>)` (`calibrate-dunder-setattr-receiver`), the
unbound `object.__setattr__(recv, 'common', <stub>)` and
`dict.__setitem__(mapping, 'common', <stub>)` (`calibrate-object-setattr-unbound`),
and a name bound to the mapping itself
(`_d = recv.__dict__; _d['common'] = <stub>`, `calibrate-dict-alias-write`).
One resolver `_setattr_arguments` now feeds both the receiver rule and the
module-namespace rule, `namespace_dict_roots` follows `__dict__` / `vars()` /
their aliases, and `_expr_kinds` resolves `__import__('sys')` /
`importlib.import_module('builtins')` to the same module kinds as the import
statement (`calibrate-import-call-modules`).

`fix-r29` closed the *unbound* spelling of the same mapping writes.  r28 read
only the bound `receiver.method(...)` form, so `dict.update(md.__dict__,
common=<stub>)` (`calibrate-dict-update-unbound`),
`dict.update(md.__dict__, {'common': <stub>})`, `dict.setdefault(md.__dict__,
'common', <stub>)` (`calibrate-dict-setdefault-unbound`), `dict.pop(
md.__dict__, 'common', <stub>)` (`calibrate-dict-pop-unbound`) and
`__builtins__.dict.update(md.__dict__, common=<stub>)` stayed green, and so did
a helper module's `attr.setattr(md, 'common', <stub>)`
(`calibrate-helper-setattr`).  `dict.__setitem__(md.__dict__, 'common',
<stub>)` and `builtins.dict.__setitem__(md.__dict__, 'common', <stub>)` were
*already* red in r28 -- its non-bound three-argument branch covers them -- and
`calibrate-dict-setitem-unbound` pins that path rather than closing a new hole.

`fix-r30` closed the spellings the r29 resolver itself still missed, each
measured on the r29 head with `_tmp`-style probe modules (before: PASS, after:
FAIL).  The receiver rule now decodes every call target through one resolver
(`_write_call_target`): `operator.setitem(md.__dict__, 'common', <stub>)`
(`calibrate-operator-setitem`), `getattr(md, '__setattr__')('common', <stub>)`
(`calibrate-getattr-dunder-write`), `getattr(builtins, 'setattr')(md, 'common',
<stub>)` (`calibrate-getattr-builtin-write`), `functools.partial(setattr,
md)('common', <stub>)` (`calibrate-partial-write`) and a *named* partial handle
(`calibrate-partial-handle`, which fails the whole receiver set closed because
the target is no longer nameable) and the dict-valued `__builtins__` the
fixture sees when it runs as `__main__`
(`__builtins__['setattr'](md, 'common', <stub>)`,
`calibrate-builtins-subscript-setattr`).  Tuple assignment targets are walked leaf by
leaf, so `md.common, _x = None, None` (`calibrate-tuple-attribute-write`) is the
plain attribute write, `exec(src, md.__dict__)` / `eval(src, md.__dict__)`
(`calibrate-exec-receiver-dict`) writes the receiver's mapping with no AST node
to read, and a reverse-order alias chain needs a real fixpoint instead of four
rounds (`calibrate-alias-chain-reverse`).  The mapping-root rule decodes the
call spelling too, so `getattr(md, '__dict__')['common'] = <stub>` and the alias
form `_d = getattr(md, '__dict__'); _d['common'] = <stub>` are the same write as
`md.__dict__['common']` (`calibrate-getattr-dict-subscript`).  The module-namespace side gained
`locals()` (a `locals()['common'] = <stub>` binding at module scope,
`calibrate-locals-write`), the unbound `dict.update(globals(), ...)`
(`calibrate-namespace-unbound-update`), a namespace alias `|=` replacement
(`g = globals(); g |= {...}`, folded into `calibrate-globals-alias-augassign`),
`from builtins import dict as _bd` (`calibrate-builtins-type-alias`), a walrus
receiver (`calibrate-walrus-receiver-write`) and a literal `eval` source, which
withdraws the whole file (`calibrate-eval-literal-write`).  The `sys.modules`
pre-seed pass reads the *actual* right-hand side of a loop-keyed write instead
of always substituting the loop pair's value, honours a key the loop body
rebinds, and accepts the unbound `dict.update(sys.modules, ...)` /
`operator.setitem(sys.modules, ...)` spellings (`calibrate-sm-pair-value`,
`calibrate-sm-loop-rebind`, `calibrate-sm-unbound-update`); a removal
(`sys.modules.pop`) is not a write and stays ignored.  The status ruler
`_call_written_values` follows the same alias and unbound teachings
(`sa = setattr; sa(report, 'status', ...)`, `dict.setdefault(report, ...)`,
`getattr(report, '__setitem__')(...)`, `__builtins__['setattr'](report, ...)`),
pinned by four probes in the in-file
detector self-test rather than by mutants.  The proven-source import check now
requires *every* import of a name to come from a proven module (`all`, not
`any`).
One resolver `_mapping_method_call` now names the target for both spellings --
`_builtin_type_name` decides whether the receiver is the builtin type (a bare
`dict`/`type`, `builtins.dict`, `__builtins__.dict`) and, if so, takes the
first argument as the mapping -- and `_setattr_arguments` accepts any
`<expr>.setattr(target, key, value)` wrapper.  A `del md.__dict__['common']`
is still out of scope: deleting the attribute makes the audit itself raise, so
it cannot turn a real bypass into a green report.

`fix-r31` closed the last literal-name holdout in that same family: the read side
(`_getattr_literal`, which feeds `_expr_kinds` and `namespace_dict_roots`) judged
only a callee spelled exactly `getattr`, while the write-target rule already went
through the alias resolver.  Measured on the r30 head (before: PASS, after: FAIL):
`from builtins import getattr as _g; _g(md, '__dict__')['common'] = <stub>`
(`calibrate-getattr-import-alias`), `import builtins as _b;
_b.getattr(md, '__dict__')['common'] = <stub>` (`calibrate-builtins-module-alias`),
`__builtins__['getattr'](md, '__dict__')['common'] = <stub>`
(`calibrate-builtins-getattr-subscript`) and
`__builtins__['vars'](md)['common'] = <stub>`
(`calibrate-builtins-subscript-vars`).  `_callable_name` now decodes the
dict-valued `__builtins__['<builtin>']` callee, and both read-side call sites
pass the alias map in.  A bare `builtins.getattr(...)` with no `import builtins`
is not a program and stays unresolved.

`fix-r32` closed the fail-closed gaps the two r31 reviewers measured, each with
a mutant: a bound managed write kept in a name (`f = md.__setattr__;
f('common', <stub>)`, `calibrate-write-handle-name` / `-getattr`), a handle built
by `operator.attrgetter('__setattr__')(md)` (`calibrate-write-handle-attrgetter`),
a target hidden in a star argument or forwarded from `*args`/`**kwargs`
(`calibrate-star-setattr`, `calibrate-star-forward`), a receiver handed on
through an unpacked target, a `for` target, a parameter default or a
`with ... as` target (`calibrate-unpack-receiver`, `-for-receiver`,
`-default-receiver`, `-with-receiver`), a loop key rebound by `with ... as`
(`calibrate-with-key-rebind`), a namespace write through an unpacked subscript
target (`calibrate-globals-tuple-target`), a `sys.modules` write through an
aliased `operator.setitem` or an in-place `|=` (`calibrate-sm-callable-setitem`,
`calibrate-sm-mapping-augassign`), a builtin read through an alias of the
`__builtins__` table (`calibrate-builtins-alias-subscript`), a call through
`getattr` with a non-literal attribute name (`calibrate-getattr-dynamic-call`),
the aliased-namespace `|=` replacement the r30 note named but no mutant pinned
(`calibrate-globals-alias-augassign`), and the status ruler's `g = getattr`
alias (`calibrate-status-getattr-alias`).  The alias walk that feeds the
receiver fixpoint is now the same `_alias_assignments` table the object-kind
resolver uses, and the three `__builtins__['<name>']` decoders share one
`_builtins_subscript_literal` helper.


`fix-r33` closed the gaps the two r32 reviewers measured, each with a mutant
except the two over-withdrawal directions, which a probe pair pins instead
(before: the clean file FAILs, after: PASS, with `restoredPass` re-checking the
restored tree): a walrus on a `global`-declared receiver
(`calibrate-global-walrus-rebind`), a nested function that rebinds an enclosing
receiver through `nonlocal` (`calibrate-nonlocal-common-rebind`, read by the
status ruler), a receiver handed through a star on either side of an unpacking
(`calibrate-unpack-star-target`, `calibrate-unpack-star-value`), through
`for m in iter((md,))` (`calibrate-unpack-iter-for`) or through a context
manager held in a name (`calibrate-with-handle-name`), a builtin name shadowed
after its first binding (`calibrate-builtin-last-binding`), a
`functools.partial(<write helper>, report)` whose pre-bound arguments decide the
arity, both inline and through a name (`calibrate-partial-prebound`,
`calibrate-partial-name-status`), a dynamic `getattr` handle kept in a name or
used as a mapping (`calibrate-getattr-handle-name`,
`calibrate-getattr-handle-dict`), and an alias chain four hops long
(`calibrate-builtins-chain-four`; the three-hop cap in `_builtin_receiver` /
`_resolved_write_helper` / `_name_resolves_to_builtins` became a `seen` guard
in `fix-r34`, so the walk is bounded by the chain's own length and
`calibrate-builtins-chain-fiftyone` pins that).
The two reversed directions: `getattr(obj, <dynamic>)(...)` now withdraws the
object the handle was taken from rather than every receiver in the file, and
the receiver alias table is keyed by scope (`(scope, name)`), so `_x = md` in
one function no longer merges with `_x = object()` in another.

`fix-r34` closed the six r33 codex P0s and its three P1s.  Bindings are now
read at the line that uses them (`_bindings_at`), so a name rebound *after* the
call no longer excuses the call (`calibrate-ordered-binding`).  A closure
default and a `global` hand-off both carry the receiver into the inner function
(`calibrate-closure-default`, `calibrate-global-handoff`), a star on either
side of an unpacking is resolved through names and rest targets
(`calibrate-star-from-name`, `calibrate-star-target-rest`), a write handle
reaches a call through an annotated assignment or as an argument
(`calibrate-handle-annassign`, `calibrate-handle-argument`), and
`functools.partial` keeps the pre-bound arguments it was given for `update`
and `setdefault` (`calibrate-partial-update-prebound`,
`calibrate-partial-setdefault-prebound`).  The two over-withdrawal directions
are probes, not mutants, because the clean file is what must change: a nested
function that declares `nonlocal md` without ever writing it keeps the outer
receiver trusted, and reading a dynamic attribute
(`getattr(md, name)`) is not a write.  A dynamic `getattr` handle only
withdraws when it reaches a call, a name, a return or a star
(`_handle_escapes`), which is also what keeps the harness's own
`{name: getattr(md, name) for name in patched}` restore loop from withdrawing
`md`; and an unknown-iterable loop target shadows the name it binds instead of
inheriting the loop-free binding.

The full 216-mutant batch at r33 also left two survivors, and they were the
mutants that were wrong rather than the ruler: `calibrate-sm-callable-setitem`
and `calibrate-sm-mapping-augassign` poisoned `sys.modules['_guard']` inside
*this* fixture, where `_guard` is never read out of `sys.modules` and the
static `common` binding stays the shared module, so `PASS` was the correct
verdict.  Both now spell the same write in `controls/session_domain.py`, the
file that does read `_guard` from `sys.modules`, and are judged by the
`rebinds the shared aggregator module` failure (`fix-r34`).

Known boundaries stay open and are recorded here so they are not re-reported
as new (the r34-measured constructs, each with its minimal spelling and probe
exit code, are tabulated in `## Known boundaries` at the end of this file): an
exit alias rebound inside a *called helper* is not resolved (the
helper's body is walked for exit calls with the module-level aliases plus its
own bindings, so `bail = sys.exit` in the helper itself is seen, but a name
bound in a third function and passed in is not); an exit spelled as an
attribute of an object that is not one of `sys` / `os` / `builtins` / `posix`
(`helpers.exit(0)`) is not treated as a process exit; the write family resolves
`setattr` / `setitem` spellings including a *name* bound to one
(`from operator import setitem as put; put(report, 'status', 'PASS')` is read
through the alias graph since fix-r30), and a write handle produced by
`operator.attrgetter('__setattr__')(md)` since fix-r32; an
alias chain longer than the hops `_exit_target` follows is not resolved; a
`Starred` element inside a returned tuple is judged only through the tuple's
other arms; an augmented assignment on a *parameter* is only judged by its own
payload (a `|=` set-union of two parameters is not treated as an aggregation
point); a `{**payload}` expansion whose names all resolve locally is not
followed into the payload's contents; an **attribute receiver**
(`self.report.update(payload)` / `self.report |= payload`) is only a summary
write when the payload carries a status literal of its own — the audited tree
has no classes, so an opaque payload on an attribute is not treated as a
summary write either way; `setattr(<module object>, <non-literal key>)` still
withdraws every name in the file rather than resolving the key, and a
`setattr` whose key comes from a dict the resolver cannot see is the same
whole-file withdrawal; `negative_process.py` is not in the
`aggregation_audit` scan set at all -- only the one registered name is checked
there; and the shielded restore window keeps one deferred signum, so a second
signal arriving inside the same window is dropped rather than queued.  The
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

## Known boundaries

`aggregation-audit` is a static approximation, not a proof.  **Its guarantee is
the set of spellings registered in this README and in `negative_controls.py`;
anything outside that set needs runtime observation instead of a static PASS.**
A green `--mode all` therefore means "none of the registered shapes is present",
never "no unaggregated write exists".

The construct below was measured against the ruler at
`1aad2b626e3db5c27a950489ce7db3beace3312e` (r34) and rechecked after
`fix-r35`; it is **still** not detected.  Repro: copy the tree to a
detached worktree at that commit and run
`BASE_TREE=<tree> python3 -I _tmp/20261106-issue-892/r34-review-probes/probe.py`
(probe + raw output archived with PR #1634).  `rc=0` = the ruler reported PASS,
i.e. missed the write; `rc=1` = caught.  Every injection is wrapped in
`if False:` (the same convention `negative_controls.py` uses), and each case
against the injected tree is a full `--mode aggregation-audit` run.

| ID | Shape **not** covered | Minimal spelling | Probe | rc |
|---|---|---|---|---|
| H | **False FAIL, not a miss**: a starred target is bound to *every* element of the right-hand side, including the ones already consumed by the leading target, so `rest[0]` is treated as `md` and an unrelated object is withdrawn. | `a, *rest = (md, object())`<br>`rest[0].common = object()` | `H-star-overbind-false-fail` | 1 |

Two boundaries are accepted deliberately by `fix-r1` round 12, both fail-closed
(they can only over-report, never let a write through):

- **A call between two exclusive literals is not modelled.**  `items = ()`,
  `def early(): for x in items: ...`, `early()`, then `items = (md,)` is still
  reported although nothing is withdrawn at runtime: without call-site
  modelling every literal binding of the enclosing scope stays a candidate.
  Probe `r12-p0-1-enclosing-late-rebind-clean` stays `caught=True` by design;
  the sibling `r12-p0-1-enclosing-late-rebind` (`items = (md,)`, `early()`,
  then `items = ()`) really does write and must stay `caught=True` too.
- **The supersede floor applies only inside the literal's own scope.**  An
  inner-scope literal no longer erases an *outer* scope's bindings; an outer
  binding stays a candidate for an inner function, which is the round-11 rule.

### Closed by `fix-r35` (#1953)

Nine constructs measured at the same r34 head were **not** detected there.  The
r35 resolver changes detect all nine, and each now has a `calibrate-*` mutant
that was `SURVIVED` before the fix and `KILLED` after (raw evidence archived with
PR #1953; re-derive the "before" state by copying the tree to a detached
worktree at `1aad2b626e3db5c27a950489ce7db3beace3312e` with the current
`negative_controls.py` in place and running `python3 -I negative_controls.py
--output <dir> --only <name>`).

| ID | Shape | Mutant |
|---|---|---|
| A | Binding that reaches the write through a **loop back edge**.  `_bindings_at` filters bindings by text line only, so a binding written *after* the call site is invisible on the next iteration. | `calibrate-loop-backedge-alias` |
| B | **Same physical line**: the call and a later rebind share a line, so the line-only comparison let the rebind excuse the call. | `calibrate-sameline-restore` |
| C | Two **sibling alias paths** to the same binding: one `seen` set shared across them made the second path return `None`. | `calibrate-shared-seen-paths` |
| D | Nested lambda **inside a decorator expression**, evaluated in the enclosing scope but attributed to the decorated function's scope. | `calibrate-decorator-lambda-scope` |
| E | A `nonlocal` name rebound through a **walrus** (or a match capture) was not withdrawn as a write. | `calibrate-nonlocal-walrus-withdraw` |
| F1 | A dynamic write handle stored in a **list literal** never reached the escape test. | `calibrate-handle-list-literal` |
| F2 | A dynamic write handle stored in a **dict literal** never reached the escape test. | `calibrate-handle-dict-literal` |
| F3 | A dynamic write handle handed out through **`yield`** never reached the escape test. | `calibrate-handle-yield` |
| G | **`__getattribute__`** read by a non-literal name hands over the `__dict__`; it was not in the read-handover set. | `calibrate-getattribute-handover` |

### Closed by `fix-r1` (#1953 review round)

The same two reviews measured five more shapes at the `fix-r35` head; each now
has a `calibrate-*` mutant that is `KILLED` (raw evidence archived with PR
#1953; `--only <name>` re-runs one).

| Shape | Mutant |
|---|---|
| **N**: a write handle carried out by a **dict comprehension** (`{k: getattr(md, k) for k in (n,)}`, then `hs['__setattr__'](...)`) kept the receiver trusted. | `calibrate-handle-dictcomp-escape` |
| **N2**: the **list-comprehension** spelling of the same escape. | `calibrate-handle-listcomp-escape` |
| **N2b**: the handle is laundered out of the container into a plain name first (`f = hs[0]; f('common', <stub>)`); the alias fixpoint carries the roots across the assignment. | `calibrate-handle-aliased-escape` |
| **O**: a **`match` pattern capture** (`case h:`) is a binding the resolver never saw, so `h(report, 'status', 'PASS')` stayed invisible. | `calibrate-match-capture-helper` |
| **P**: a name with **both** a non-write and a write binding made the resolution non-unique and it was dropped instead of failing closed. | `calibrate-mixed-alias-binding` |
| A write performed **inside a comprehension element** is an expression, not a statement, so the per-statement `unshared_status_slots` scan never reached it. | `calibrate-comprehension-write-helper` |

Two narrow positives pin the exemptions that `fix-r1` added for the false FAILs
it had introduced: `getattr(md, '__getattribute__')('__doc__')` (and the
constant-subscript spelling `_keys = ('__doc__',); getattr(md, _keys[0])`) is a
plain read and must stay clean, while the same shapes naming a **write**
attribute still turn the gate red --
`calibrate-getattribute-write-attribute` and
`calibrate-getattr-const-subscript-write`.

### Closed by `fix-r1` round 2 (codex r1b)

The first `fix-r1` head closed the eight constructs above but the same review
found four ways around the new rules; each now has its own mutant.

| Shape | Mutant |
|---|---|
| **N/N2 alias**: the whole **container** is handed to a second name (`other = hs; other[0](...)`); the fixpoint only followed subscript/call wrappers. | `calibrate-handle-container-alias` |
| **N/N2 mapping write**: the handle is not only *called* -- `hs[0]['common'] = <stub>` (or `hs[0].common = ...`) writes the object it was taken from. | `calibrate-handle-mapping-write` |
| **M hole**: a **list** literal was folded like a tuple, so `keys = ['__doc__']; keys[0] = '__setattr__'; getattr(md, keys[0])(...)` read as a pure `__doc__` read. Only immutable tuples are folded now. | `calibrate-list-const-subscript-write` |
| Comprehension **filter / iterable**: the write sat in `if <write>` or in the iterated expression, neither of which was scanned. | `calibrate-comprehension-filter-write` |

Round 2 also narrowed two rules the first head over-reached on, both pinned by
in-file self-tests that run on every `aggregation-audit`:
a **comprehension target** no longer leaks into the enclosing function's
bindings (`[h for h in (operator.setitem,)]; h = lambda *a: None;
h(report, 'status', 'PASS')` is clean -- probe `N13`), and a bare write-handle
name resolves its target from the first positional argument instead of
withdrawing every receiver in the file.

### Closed by `fix-r1` round 3 (GLM sibling/concurrency r1b)

The second head still let three receiver-side spellings through; each is now a
mutant and was measured `rc=0` before the change, `rc=1` after.

| Shape | Mutant |
|---|---|
| **`match` capture as receiver**: `match md: case h: h.common = <stub>` (and `setattr(h, ...)`). The capture table existed for the *helper* rules but not for the table the receiver rules read; a comprehension target bound to a literal element (`[h for h in (md,)]`) had the same gap. | `calibrate-match-capture-receiver`, `calibrate-match-capture-setattr-receiver` |
| **`__getattribute__` attribute spelling**: `md.__getattribute__('__dict__')['common'] = <stub>` reaches the same mapping as `getattr(md, '__getattribute__')('__dict__')`, which `namespace_dict_roots` already decoded. | `calibrate-getattribute-attr-spelling` |
| **Wrapped iteration**: `for h in list(hs): h(...)` -- the call position decoded `list`/`tuple`/`sorted`/`iter`/`reversed` wrappers, the iteration position only took a bare name. | `calibrate-for-wrapped-container` |

### Closed by `fix-r1` round 4 (codex r2)

The third head left one false PASS and carried two false FAILs the earlier
rounds had introduced.

| Shape | Mutant / self-test |
|---|---|
| **Non-literal `__getattribute__` name**: `_key = '__dict__'; md.__getattribute__(_key)['common'] = <stub>` -- the attribute spelling only decoded a literal argument, so the hand-over under a name stayed trusted. An attribute name that is not provably a pure read now fails closed, matching r35's rule for non-literal names. | `calibrate-getattribute-name-spelling` |
| **Comprehension target scope (false FAIL)**: `h = object(); [h for h in (md,)]; h.common = <stub>` -- a comprehension target is local to its comprehension, so it must not enter the enclosing alias table. The write *inside* the comprehension is judged against the literal elements instead, so `[setattr(h, 'common', <stub>) for h in (md,)]` is still caught. | self-tests `comp_receiver_probe` / `comp_receiver_scope_probe` |
| **Iterated container, pure read (false FAIL)**: `for h in hs: print(h)` -- iterating only hands the elements to the loop target; withdrawing on the iteration itself made clean code fail. The loop target's own call / store sites decide now. | self-test `iterated_read_probe` |
| Non-literal `__getattribute__` name (positive side) | self-test `getattribute_name_probe` |

Also measured at the same head, tracked in #1954 rather than accepted here (they
are P2-level and narrower than the constructs above): `from operator import attrgetter as _ag;
_ag('__setattr__')(md)('common', object())` (probe `I-attrgetter-fromimport`,
rc=0 — the from-import alias list does not carry `attrgetter`); an `attrgetter`
handle registered through an annotated assignment rather than a bare `Assign`
(`J-attrgetter-annassign`, rc=0); and `a, *rest = (None, md.__dict__);
rest[0]['common'] = object()` (`K-namespace-dict-subscript`, rc=0 —
`namespace_dict_roots` does not descend a `Subscript` the way `receiver_roots`
now does).  Separately, an alias chain of ~1,100 hops exhausts the Python
recursion limit instead of resolving (codex's measurement, not independently
reproduced here); the README's "bounded by the chain's own length" statement
holds only below the interpreter's recursion limit.

### Closed by `fix-r1` round 5 (GLM boundary r2)

The fourth head left two iteration-position false PASSes, a container-relay
false PASS and one false FAIL of the constant folding.

| Shape | Mutant / self-test |
|---|---|
| **Loop target used after the loop**: `for h in hs: pass` then `h('common', <stub>)` -- the target outlives the loop, so the use-point scan has to cover the statements that follow it in the same scope. | `calibrate-for-target-after-loop` |
| **Narrow iteration-position decoding**: `for h in hs[:]` and `for h in hs.copy()` copy the container but hand out the same handles. The call and iteration positions now share one decoder. | `calibrate-for-slice` / `calibrate-for-copy` |
| **Comprehension result container**: `_g = [h for h in (md,)]; _g[0].common = <stub>` -- the comprehension target is local, the list it builds is not; a relay comprehension's result carries the element roots. | `calibrate-comp-result-receiver` |
| **Negative constant subscript (false FAIL)**: `keys = ('__doc__',); getattr(md, keys[-1])` -- a negative index is a position from the end, not an unknown name. The literal arrives as `UnaryOp(USub, Constant)`, so the fold decodes it before indexing. | self-test `negative_subscript_probe` |
| Loop target read after the loop (false FAIL guard): `for h in hs: pass` then `print(h)` reads without writing. | self-test `iterated_after_read_probe` |

### Closed by `fix-r1` round 6 (codex r3)

The round-5 use-point scan walked the parent chain through
`body`/`orelse`/`finalbody`, so a use placed in a `match` case, an `except`
handler or a `finally` after the loop was never reached; it also stopped at
any rebinding, so `if cond: h = None` ended the scan while the handle was
still live. The comprehension relay branch knew neither a `DictComp` nor a
name-bound literal iterable.

| Shape | Mutant / self-test |
|---|---|
| **Conditional rebinding of the loop target**: `if False: h = None` then `h('common', <stub>)` -- only a *sibling* rebinding to a provably fresh value ends the scan. | `calibrate-for-target-rebind-if` |
| **Use in a `match` case after the loop**: the scan now walks the whole scope in source order instead of the parent chain. | `calibrate-for-target-after-match` |
| **Dict-comprehension result container**: `_g = {0: h for h in (md,)}; _g[0].common = <stub>`. | `calibrate-comp-dict-result` |
| **Name-bound literal iterable**: `items = (md,); _g = [h for h in items]`. | `calibrate-comp-name-iter` |
| Fresh rebinding after the loop (false FAIL guard): `h = object(); h()` builds a new object, so the old handle is gone and `object()`/`list()`/`dict()`/... count as fresh; `getattr(md, name)` does not. | self-test `loop_rebind_fresh_probe` |
| Conditional rebinding keeps the handle (self-test of the fix above). | self-test `loop_rebind_if_probe` |
| Comprehension result only read (false FAIL guard): `_g = [h for h in (md,)]; print(len(_g))`. | self-test `comp_result_pure_probe` |

### Closed by `fix-r1` round 7 (GLM boundary r3 / sibling r3)

| Shape | Mutant / self-test |
|---|---|
| **The loop target escapes through `return`**: the caller receives the write handle. | `calibrate-for-target-return` |
| **The loop target escapes through an alias**: `f = h; f('common', <stub>)`; the scan follows second names and drops them again when they are rebound to a fresh value. | `calibrate-for-target-alias` |
| **Starred element in a folded tuple**: `keys = ('__doc__', *more)` has no static length, so `keys[-2]` may be the attacker's `'__setattr__'`; the fold refuses instead of resolving to the wrong element. | `calibrate-starred-const-subscript-write` |
| **Comprehension relaying a handle container**: `g = [h for h in hs]; g[0]('common', <stub>)` -- `hs` is not a literal, its roots come from the alias table. | `calibrate-comp-handle-relay` |
| Statically false filter (false FAIL guard): `g = [h for h in (md,) if False]` yields nothing, so the result relays no receiver. | self-test `comp_filter_false_probe` |
| Alias kept for a pure read (false FAIL guard): `f = h; print(f)`. | self-test `loop_alias_read_probe` |
| Slice / copy iterated for a read only (false FAIL guards). | self-tests `slice_read_probe` / `copy_read_probe` |
| Returned loop target, aliased call and starred fold (self-tests of the four mutants above). | self-tests `loop_return_probe`, `loop_alias_probe`, `starred_keys_probe`, `comp_handle_relay_probe` |

The wrapper-name family (`iter`/`next`/`tuple`/`list`/`sorted`/`reversed`/
`set`/`frozenset`) is one constant shared by the iteration decoder and the
literal-element decoder; the subscript decoder also unwraps `hs[:]` and
`hs.copy()` now, so `hs.copy()[0]('common', <stub>)` is caught.

### Closed by `fix-r1` round 8 (codex r4 five P0 / three P1)

Round 7 enumerated the *escaping* spellings and review found four more, so the
audit now walks the scope's statements in execution order and keeps a name in
the tracked set unless it is provably fresh; a tracked name counts as a use
unless it sits in one of the few positions that only read it.  The five P0
shapes therefore close structurally rather than one spelling at a time.

| Shape | Mutant / self-test |
|---|---|
| **Rebind control never reaches**: `raise` then `h = None`, then `finally: h('common', <stub>)`. | `calibrate-for-target-finally-rebind` |
| **Closure captures the loop target**: `def _inner(): h('common', <stub>)` after the loop. | `calibrate-for-target-closure` |
| **Shadowed constructor name**: `object = lambda: hs[0]; h = object(); h('common', <stub>)` hands the handle back. | `calibrate-for-target-shadowed-fresh` |
| **`with`-statement binding**: `with nullcontext(h) as f: f('common', <stub>)`. | `calibrate-for-target-with-alias` |
| **Exception round-trip**: `raise Exception(h)` / `except Exception as e: f = e.args[0]; f(...)`. | `calibrate-for-target-except-alias` |
| **`for` over a name-bound literal**: `items = (hs[0],); for h in items`. | `calibrate-for-target-name-iter` |
| **Relayed container iterated by `for`**: `g = [h for h in hs]; for f in g: f(...)`. | `calibrate-comp-handle-relay-for` |
| **Relayed through two comprehensions**: `g = [h for h in hs]; j = [f for f in g]; j[0](...)`. | `calibrate-comp-handle-relay-chain` |
| Comparison is a pure read (false FAIL guard): `return h is None`. | self-test `loop_return_compare_probe` |
| Rebind in a statically taken branch (false FAIL guard): `if True: h = lambda: None; h()`. | self-test `loop_rebind_block_probe` |
| Relay through a statically empty comprehension (false FAIL guard). | self-test `comp_relay_empty_probe` |
| Nested definition that rebinds the name locally (false FAIL guard). | self-test `loop_local_shadow_probe` |
| The six new escape shapes above, replayed in memory. | self-tests `loop_finally_rebind_probe`, `loop_closure_probe`, `loop_shadowed_fresh_probe`, `loop_with_alias_probe`, `loop_except_alias_probe`, `comp_relay_chain_probe` |

### Closed by `fix-r1` round 9 (codex r5 five P0 / three P1, GLM r5)

Round 8's block-ordered scan still modelled *where* a statement runs.  Round 9
replaces it with one flat, source-ordered walk over the scope (plus a
`_certain_statements` gate that only lets a provably executed statement clear a
tracked name), and widens the use test so that every position that is not
provably a read counts as a hand-over.  Each row below is a shape the two
reviewers reported; the clean counterparts are pinned as in-file self-tests
because a mutant can only prove the gate fires, never that it stays quiet.

| Shape | Mutant / self-test |
|---|---|
| **Assignment right-hand side**: `x = [h('common', <stub>)]`. | `calibrate-for-target-assign-call` |
| **Definition header**: `def inner(x=h('common', <stub>)): ...`. | `calibrate-for-target-def-default` |
| **Read-only builtin, unknown callee**: `print(helper(h))`. | `calibrate-for-target-readonly-call-leak` |
| **Nested parameter does not shield the outer name**: `def inner(): def unused(h): ...` then `h(...)`. | `calibrate-for-target-nested-param` |
| **Comparison operand**: `if sink(h) == 0: ...`. | `calibrate-for-target-compare-call-leak` |
| **Constructor shadowed in an enclosing scope**: module-level `object = lambda: hs[0]`, then `h = object()`. | `calibrate-for-target-outer-shadow-fresh` |
| **Read-only builtin shadowed in an enclosing scope**: `print = sink`, then `lambda: print(h)`. | `calibrate-for-target-outer-shadow-print` |
| **Ambiguous literal binding**: `items = (); items = (md,)`, then iterate and write. | `calibrate-for-target-ambiguous-literal` |
| **Comprehension element** (round 8 skipped the whole comprehension): `[h('common', <stub>) for j in (md,)]`. | `calibrate-for-target-comp-element-call` |
| Bare handle in a truth test is a read (false FAIL guard): `if h:` / `while h:`. | self-test `round 9: a bare handle in a truth test` |
| Attribute load is a read (false FAIL guard): `name = h.__name__`. | self-test `round 9: an attribute read on the handle` |
| Nested parameter / class-body binding really shadows (false FAIL guards). | self-tests `round 9: a parameter of a nested definition`, `round 9: a class-body binding` |
| Parameter shadows a module-level literal (false FAIL guard). | self-test `round 9: a parameter that shadows a module-level literal` |
| `del h` ends the name instead of using it (false FAIL guard). | self-test `round 9: a deleted loop target` |
| Statically false comprehension filter (false FAIL guard). | self-test `round 9: a comprehension whose filter can never hold` |

### Closed by `fix-r1` round 10 (codex r6 five P0 / four P1, GLM boundary r6)

Round 9's flat scan is kept; round 10 fixes the places where it confused a
*namespace* with a scope and a *certain* statement with a reachable one.

| Shape | Mutant / self-test |
|---|---|
| **Attribute load that forwards the handle**: `h.__call__(<stub>)`, `f = h.__call__`. | `calibrate-for-target-call-attribute`, `calibrate-for-target-call-attribute-alias` |
| **Statically false filter still evaluates what precedes it**: `[m for m in (md,) if setattr(m, 'common', <stub>) if False]`. | `calibrate-for-target-false-filter-write` |
| **Class-body rebinding clears the outer handle**: `class C: h = object()` then `h(...)`. | `calibrate-for-target-class-body-clear` |
| **`with` body is not provably executed** (the context manager may raise). | `calibrate-for-target-with-suppress` |
| **Class body shadows a read-only builtin for its own expressions**. | `calibrate-for-target-class-body-shadow` |
| **Lambda default writes at creation time**: `f = lambda x=h('common', <stub>): None`. | `calibrate-for-target-lambda-default` |
| **Class-body `def` binds its name in the class namespace**, not the enclosing function. | `calibrate-for-target-class-body-def` |
| **The bound object behind a method**: `x = h.__self__; x.common = <stub>`. | `calibrate-for-target-method-self` |
| Literal bound in another scope does not reach the loop (false FAIL guard). | self-test `round 10: a literal bound in another function` |
| Comprehension target shadows the handle (false FAIL guard). | self-test `round 10: a comprehension target that shadows the handle` |
| `finally` rebinding is provably executed (false FAIL guard). | self-test `round 10: a rebinding in a finally block` |
| Tuple unpack copies the handle instead of handing it over (false FAIL guard). | self-test `round 10: a tuple unpack that copies the handle` |
| Assertion message is only formatted on failure (false FAIL guard). **Withdrawn in round 11**: the object survives in `AssertionError.args`. | `calibrate-for-target-assert-msg` |

### Closed by `fix-r1` round 11 (codex r7 five P0 / one P1)

Round 10's scope model is kept; round 11 fixes five places where it proved too
much from too little, plus one pre-existing survivor.

| Shape | Mutant / self-test |
|---|---|
| **A literal rebinding that may not run**: `items = (md,); if flag: items = (); for x in items: x.common = <stub>`. | `calibrate-for-target-conditional-literal-rebind` |
| **Enclosing-scope binding written after the use's line**: `def early(): for x in items: ...` then `items = (md,)`. | `calibrate-for-target-enclosing-binding-after-use` |
| **One alias must not skip the rest of the right-hand side**: `f, x = h, [h('common', <stub>)]`. | `calibrate-for-target-tuple-partial-alias` |
| **Shadowing one tracked name must not clear the others**: `f = h; [f('common', <stub>) for h in (None,)]`. | `calibrate-for-target-comp-partial-shadow` |
| **The assertion message carries the object**: `try: assert False, h except AssertionError as e: e.args[0](...)`. | `calibrate-for-target-assert-msg` |
| **Class-body rebinding shadows the outer handle for later statements** (false FAIL guard). | self-test `round 11: a class body that rebinds the handle name before the call` |
| **`from builtins import setattr as alias` is the builtin** (pre-existing survivor). | `calibrate-builtin-alias-setattr` |

### Closed by `fix-r1` round 12 (codex r8 four P0 / two P1, GLM sibling r8 P1)

Round 12 fixes four places where the round-11 hand-over model still handed a
write handle to the checker's own bookkeeping, plus two false FAILs and one
pre-existing miss on the status side.

| Shape | Mutant / self-test |
|---|---|
| **A later inner-scope literal must not erase an outer scope's candidates**: the supersede floor now applies only inside the literal's own scope. | `calibrate-for-target-enclosing-call-between-rebinds`; self-test `a later literal rebinding in the enclosing scope` (round 12) |
| **A class-body write inside a compound statement**: `class _C: if flag: h('common', <stub>)`. | `calibrate-for-target-class-body-compound` |
| **A method that closes over the outer handle**: `class _C: h = lambda *a: None` then `def m(self): h('common', <stub>)`. | `calibrate-for-target-class-method-closure` |
| **A class body that shadows the constructor it calls**: `class _C: object = lambda: hs[0]; h = object(); h('common', <stub>)`. | `calibrate-for-target-class-body-shadowed-constructor` |
| **A literal rebound in the enclosing scope *after* the call**: `items = (md,); def early(): for x in items: ...; early(); items = ()`. Fail-closed, see Known boundaries. | `calibrate-for-target-enclosing-call-between-rebinds` |
| **A nested unpack copies instead of handing over** (false FAIL guard). | self-test `round 12: a nested unpack that only copies the handle` |
| **`from builtins import setattr as alias` in the statement's own scope is the builtin**, while a parameter of the same name is not (pre-existing miss on the status side). | self-test `round 12: a parameter typed by another function's import` |

### Closed by `fix-r1` round 13 (codex r9 five P0 / three P1)

Round 13 fixes the import-alias resolution on both sides and the parts of the
class-body hand-over the round-12 rewrite still dropped.

| Shape | Mutant / self-test |
|---|---|
| **A write helper imported at module scope and called inside a function**: `from builtins import setattr as w` at module level, `w(md, 'common', <stub>)` in a body. | `calibrate-for-target-module-import-alias`; self-test `round 13: a builtin write reached through a module-level import alias` |
| **A call before a later rebinding**: `from builtins import setattr as w; w(md, ...); w = print` still writes. | self-test `round 13: a later rebinding after the call` |
| **A non-write import before the call** (false FAIL guard): `from builtins import print as w` after the write import makes `w(...)` a print. | self-test `round 13: a non-write import before the call` |
| **A class body that copies the handle**: `_f = _h; _f('common', <stub>)`, including inside a compound statement. | `calibrate-for-target-class-body-copy-alias`; self-test `round 13: a class body that copies the handle into another name` |
| **A nested class whose method closes over the outer handle**: `class C: h = lambda *a: None; class D: def m(self): h('common', <stub>)`. | `calibrate-for-target-nested-class-method`; self-test `round 13: a nested class whose method closes over the outer handle` |
| **A method body reads the handle with the builtin `print`** (false FAIL guard): the class namespace does not reach into a method. | self-test `round 13: a method that reads the handle with the builtin print` |
| **Status side: a module-level import alias**, a call before the rebinding, and the two false-FAIL directions. | self-tests `round 13: a module-level import alias`, `a call before the rebinding`, `a later non-write import`, `a rebinding before the call`, `a parameter that shadows the module alias` |

### Closed by `fix-r1` round 14 (codex r10 four P0 / one P1)

Round 13 decided which binding a use reads by source line alone. Round 14
makes it a flow question on both sides: an event keeps its column, whether it
is conditional and the loops it sits inside.

| Shape | Mutant / self-test |
|---|---|
| **A rebinding inside a branch does not erase an earlier write helper**: `from builtins import setattr as w; if flag: from builtins import print as w; w(md, ...)` really writes when `flag` is false. | `calibrate-for-target-conditional-rebind`; self-test `round 14: a conditional rebinding of the write alias` |
| **A loop back edge puts a textually later binding in reach**: `from builtins import print as w; for ...: w(md, ...); from builtins import setattr as w` writes on the second iteration. | `calibrate-for-target-loop-back-edge`; self-test `round 14: a write alias rebound after the call inside its loop` |
| **Two bindings on one line are ordered by column**, not by walk order. | `calibrate-for-target-same-line-rebind`; self-tests `round 14: a write import rebound after the call on the same line`, `a non-write import before the call on the same line` |
| **A class-body block hands the alias on**: `class C: if True: f = _h` followed by `f('common', <stub>)` after the block. | `calibrate-for-target-class-block-alias`; self-test `round 14: a class body that copies the handle inside a block` |
| **A nested function hands the alias on**: `def g(): if True: f = _h` then `f('common', <stub>)`, called from the enclosing function. | `calibrate-for-target-nested-copy-alias`; self-tests `round 14: a nested function that copies the handle before the call`, `a nested function that only reads the copy` |
| **An unconditional non-write import after the write import** (false FAIL guard): at module scope the last binding before the call is what the body reads. | self-test `round 14: an unconditional non-write import after the write import` |

### Closed by `fix-r1` round 15 (codex r14 four P0 / one P1, GLM sibling r14 P1)

Round 14 resolved an enclosing binding by "the last unconditional binding in
the module", which is only right when nothing is known about when the
enclosing function runs.  Round 15 reads the module binding at the module-scope
call site, resolves the right-hand side of an assignment whose value names a
write helper, and builds a scope's flow marks before the first lookup.  It
also stops a copy from surviving an unconditional overwrite.

| Shape | Mutant / self-test |
|---|---|
| **A module call that ran before a later rebinding**: `from builtins import setattr as w; def f(): w(md, ...); f(); from builtins import print as w` still writes. | `calibrate-for-target-module-call-before-rebind`; self-tests `round 15: a module call before the write alias is rebound` (aggregation), `round 15: a module call before the write alias is rebound` (status) |
| **A self assignment keeps the write helper**: `from builtins import setattr as w; w = w; w(md, ...)`. | `calibrate-for-target-module-self-assignment`; self-tests `round 15: a module self assignment of the write alias` (both sides) |
| **A conditional assignment of the helper**: `from builtins import print as w; if flag: w = setattr; w(report, 'status', 'PASS')` writes when `flag` is true. | self-test `round 15: a conditional assignment of the write helper` |
| **The first lookup in a module scope already sees its loop back edge**: `from builtins import print as w; for ...: w(md, ...); from builtins import setattr as w`. | `calibrate-for-target-module-loop-first-use`; self-test `round 15: a module loop whose first use sits on the back edge` |
| **A copy that is certainly overwritten is no alias** (false FAIL guard): `f = _h; f = lambda *a: None; f('common', <stub>)` calls the fresh object. | self-test `round 15: a copy that is certainly overwritten` |
| **A non-write rebinding after the call on the same line** (false FAIL guard, status side). | self-test `round 15: a non-write rebinding after the call on the same line` |
| **An unconditional non-write import after the module write import** (false FAIL guard): with no call site the round-14 rule stands. | self-test `round 15: an unconditional non-write import after the module write import` |

Known boundary: an assignment whose right side is an *attribute*
(`w = builtins.setattr`) still resolves to no helper, and a nested function's
enclosing call site is read from the module-level definition it sits in, so a
call made from a sibling function body is not modelled.  Both directions keep
the round-14 answer.

### Closed by `fix-r1` round 16 (codex r15 four P0)

Round 15 read a module-scope call site's binding at the call, but the *shape*
of a binding was still taken from the name alone: a bound write handle
(`h = getattr(md, '__setattr__')`) was spelled as the unbound helper, so the
receiver was lost and the write was not attributed.  Round 16 keeps the
receiver in the kind (`fn:write-handle:<name>`), lets a certain overwrite by a
fresh value clear it, and treats the module scope as a binding scope with call
positions on loop back edges and in definition headers.

| Shape | Mutant / self-test |
|---|---|
| **A duplicated unpack target that ends on the handle**: `h = getattr(md, '__setattr__'); f, f = None, h; f('common', ...)` writes. | `calibrate-for-target-duplicated-unpack-target`; self-test `round 16: a duplicated unpack target that ends on the handle` |
| **A module call site on a loop back edge**: `from builtins import print as w; def f(): w(md, ...); for ...: f(); from builtins import setattr as w`. | `calibrate-for-target-module-call-site-loop-edge`; self-test `round 16: a module call site on a loop back edge` |
| **A call in a definition header default** (evaluated in the module scope): `def g(x=f())` where `f` calls the write alias. | `calibrate-for-target-default-value-call`; self-test `round 16: a call in a definition header default` |
| **An assignment on a loop back edge**: `for ...: x = w; x(md, ...); from builtins import setattr as w`. | `calibrate-for-target-assignment-back-edge`; self-test `round 16: an assignment on a loop back edge` |
| **A bound handle that is certainly overwritten** (false FAIL guard): `h = getattr(md, '__setattr__'); h = lambda *a: None; h('common', ...)` calls the fresh object. | self-test `round 16: a bound handle that is certainly overwritten` |

The bound-handle family was missing at every head since round 14 while
`origin/main` caught it; the four mutants above are red at round 15 and green
at round 16 (probe `P0-1..P0-4`: `withdrawn=[]` before, `['md']` after).

Known boundary: a nested function's module call site is read from its own
definition, so a call made from a sibling function body is not modelled; where
the tool cannot tell, it reports the write (fail closed) instead of trusting
the attribute.  A write handle taken from a receiver that is not a bare name
(`getattr(md.inner, '__setattr__')`, `md[0].__setattr__`) carries no receiver
tag, so a later two-argument call on it is not attributed; the write lands on a
sub-object of the shared module, which the audit does not treat as a write to
`md` itself — the same boundary as the direct spelling `md.inner.__setattr__(…)`
(sibling r16 P2-4, registered as a known boundary; r17 leaves it unchanged).

### Closed by `fix-r1` round 17 (codex r16 four P0 / boundary r16 two P0)

Round 16 asked "is this name still the handle?" of the single positionally
latest binding, with the receiver tags pooled per file.  Three shapes answered
yes while a real write still happened, and two clean shapes answered no.

| Shape | Mutant / self-test |
|---|---|
| **A conditional overwrite does not clear the handle**: `h = getattr(md, '__setattr__'); if flag: h = lambda *a: None; h('common', ...)` still writes on the other arm. | `calibrate-for-target-handle-conditional-overwrite`; self-tests `round 17: a bound handle overwritten only in an if arm`, `round 17: a bound handle overwritten in a try body` |
| **A sibling function's same-named binding is not this binding**: two functions bind `h`, one to another object and one to `md`; the receiver tags are now resolved per scope instead of pooled. | `calibrate-for-target-scoped-handle-receiver`; self-tests `round 17: a bound handle another function binds elsewhere`, `round 17: a bound handle only another function binds to the receiver` |
| **A constructor shadowed in the same scope is not the builtin**: `object = lambda: h; h = object()` re-binds the handle, so "fresh value" no longer clears it. | `calibrate-for-target-shadowed-constructor`; self-test `round 17: a bound handle recreated by a shadowed constructor` |
| **A handle re-bound to a non-handle name** (false FAIL guard): `h = getattr(md, '__setattr__'); h = print` calls something else. | self-test `round 17: a bound handle rebound to a known non-handle` |
| **A bound handle the status ruler must still follow**: `f = getattr(report, '__setattr__'); f('status', 'PASS')` and the attribute spelling. | self-tests `round 17: a status write through a by-name bound handle`, `round 17: a status write through an attribute bound handle` |
| **Certainty is block ancestry, not line order** (false FAIL guard): a duplicated unpack target that ends fresh, and a relayed handle that is certainly overwritten. | self-tests `round 17: a duplicated unpack target that ends fresh`, `round 17: a relayed handle that is certainly overwritten` |

The same family, one round later: the three mutants above are green at round
16 and red at round 17 (probe `if` / sibling-scope / shadowed-constructor:
`withdrawn=[]` before, `['md']` after).  Codex r16 P0-3 and P0-4 are one fix
(the receiver tags resolved per scope), so three mutants close four reports.

### Closed by `fix-r1` round 18 (codex r17 two P0 / one P1, GLM sibling r17 P2)

Round 17 still read a container *copy* as a fresh value, decoded only a
directly bound status handle, and ran the freshness guard on two-argument
calls alone.

| Shape | Mutant / self-test |
|---|---|
| **A constructor copy of a handle container still hands the handles over**: `h = [getattr(md, k) for k in (n,)]; h = list(h); for f in h: f('common', ...)` keeps the write live. | `calibrate-for-target-handle-container-copy` |
| **The status write survives a name-to-name hand-over**: `g = getattr(report, '__setattr__'); f = g; f('status', 'PASS')` (also the single-hop and attribute spellings) is the same write. | `calibrate-status-bound-handle-alias-chain` |
| **A three-argument alias that still holds `setattr` is not cleared** (false FAIL guard): only a certain fresh overwrite ends it; `h = setattr; h(report, 'status', 'PASS')` is still flagged. | `calibrate-status-three-arg-handle-live` |
| **A constructor copy of an ordinary list stays fresh** (false FAIL guard): `xs = [1, 2]; xs = list(xs); for x in xs: print(x)` writes nothing. | matrix case `R2-container-copy-clean` |
| **An overwritten three-argument alias writes nothing** (false FAIL guard): `h = setattr; h = lambda *a: None; h(report, 'status', 'PASS')`. | matrix case `R4-three-arg-alias-clean` |

The three mutants are green at round 17 and red at round 18; the two clean
shapes are red at round 17 and green at round 18 (see the round-18 matrix in
the PR).

### Closed by `fix-r1` round 19 (GLM boundary r17 one P0 / one P1, codex r18 two P0 / two P1, GLM boundary r18 one P0)

The status ruler decoded only the literal `getattr` spelling, so an aliased
handle factory lost the write; the round-17 block table judged a binding in a
`finally` body (or the taken arm of a statically decided `if`) as "may not
have run", so a clean file was flagged.  Round 18's own guards then had three
holes of their own: the container copy was decoded one level deep (a nested or
sliced copy read as fresh), the by-name handle chain was cut at eight hops,
and it read every binding a name ever had instead of the one in force.

| Shape | Mutant / self-test |
|---|---|
| **The handle factory may reach the call under another name**: `ga = getattr; f = ga(report, '__setattr__'); f('status', 'PASS')` (also `ga = builtins.getattr`) is the same status write. | `calibrate-status-getattr-alias-callee`; self-tests `round 19: a status write through an aliased getattr factory`, `... through an aliased builtins.getattr factory` |
| **A factory alias that is not `getattr` writes nothing** (false FAIL guard): `ga = dict; f = ga(report, '__setattr__')`. | self-test `round 19: a status write through a factory alias that is not getattr` |
| **A nested or sliced copy still carries the handles**: `hs2 = list(tuple(hs))` / `list(hs[:])` / `list(iter(hs))` / `list(hs.copy())`. | `calibrate-for-target-nested-container-copy`; self-tests `round 19: a handle container copied through a nested constructor`, `... through a slice` |
| **A long name chain is still a handle** (false PASS guard): nine hops of `a1 = a0`. | self-test `round 19: a status write through a nine-hop name chain` |
| **The binding in force decides, not the history** (false FAIL guard): `g = report.__setitem__; g = print; f = g`. | self-tests `round 19: a status handle certainly overwritten by another name`, `... overwritten only in an if arm` |
| **A container certainly replaced before the copy no longer carries** (false FAIL guard): `hs = [...]` handles; `hs = [lambda *a: None]`; `hs2 = list(hs)`. | self-tests `round 19: a handle container certainly replaced before the copy`, `... replaced only in an if arm before the copy` |
| **A handle certainly overwritten in a `finally` block is gone** (false FAIL guard): `h = getattr(md, '__setattr__'); try: ... finally: h = lambda *a: None; h('common', ...)`. | self-tests `round 19: a handle certainly overwritten in a finally block`, `... in a statically true if` |
| **A handle overwritten in an ordinary branch stays live** (fail-closed): `if object(): h = lambda *a: None` may not run. | self-test `round 19: a handle overwritten in an ordinary branch` |

### Closed by `fix-r1` round 20 (codex r19 two P0 / two P1)

Round 19's "binding in force" was read at the final call rather than at the
assignment that handed the value on, and the `getattr` spelling read the whole
binding history plus any attribute called `getattr`.  Both directions leaked:
a handle copied before its source was overwritten stayed green, and a clean
file using `other.getattr(...)` or an overwritten factory alias went red.  The
container relay had the mirror-image hole: a name rebound to a fresh empty
list and then filled in place was judged by the empty list alone.

| Shape | Mutant / self-test |
|---|---|
| **The handle was copied before its source name was overwritten**: `g = report.__setitem__; f = g; g = print; f('status', 'PASS')` writes the status slot. | `calibrate-status-alias-copied-before-overwrite`; self-tests `round 20: a status write through an alias copied before the overwrite`, `... through a two-name relay before the overwrite` |
| **A container filled in place after a rebinding still carries**: `hs = [...]; hs = []; hs.append(getattr(md, '__setattr__')); copy = list(hs)`. | `calibrate-for-target-container-filled-after-rebind`; self-tests `round 20: a handle container filled in place after a rebinding`, `... extended in place after a rebinding` |
| **A factory alias overwritten before the call writes nothing** (false FAIL guard). | self-test `round 20: a status write through a factory alias overwritten before the call` |
| **A `getattr` attribute on another object writes nothing** (false FAIL guard): `other.getattr(report, '__setitem__')`. | self-test `round 20: a status write through a getattr on another object` |
| **A shadowed `getattr` name writes nothing** (false FAIL guard): `getattr = lambda ...` in the function. | self-test `round 20: a status write through a shadowed getattr name` |
| **A container emptied before the copy no longer carries** (false FAIL guard). | self-tests `round 20: a handle container emptied and copied`, `... certainly replaced and copied` |

### Closed by `fix-r1` round 21 (GLM r19 one P0 / one P1 / two false FAILs)

Round 20 resolved every hop at its own assignment but still asked the
*per-name history* for container hops, so a chain that had certainly replaced
the handles one hop upstream read as carried; the `getattr` factory was also
only recognised as the bare builtin name, not an import alias or a
module-level alias.

| Shape | Mutant / self-test |
|---|---|
| **The factory reaches the call through an import alias**: `from builtins import getattr as ga; f = ga(report, '__setattr__'); f('status', 'PASS')`. | `calibrate-status-imported-getattr-factory`; self-test `round 21: a status write through an imported getattr factory` |
| **The factory is a module-level alias**: `ga = getattr` at module level, used inside `main`. | self-test `round 21: a status write through a module-level getattr alias` |
| **An imported factory that is not `getattr` writes nothing** (false FAIL guard): `from builtins import dict as ga`. | self-test `round 21: a status write through an imported factory that is not getattr` |
| **A copy of a replaced container is fresh, and stays fresh through a name alias**: `hs = [...]; hs = [lambda *a: None]; c = list(hs); x = c; for h in x: h('common', ...)`. | self-tests `round 21: a handle container copied then aliased by a fresh name`, `... replaced and copied twice` |
| **Aliasing a copy that still carries keeps the handles** (false PASS guard). | `calibrate-for-target-alias-of-container-copy`; self-test `round 21: a handle container still carrying when aliased` |

### Closed by `fix-r1` round 22 (codex r21 three P0s / GLM r21 two P0s and two P1s)

Round 21 taught the status ruler the import alias of the factory but read the
import table only when the assignment table was empty, and `_builtins_module`
never learned the module's own import alias.

| Shape | Mutant / self-test |
|---|---|
| **The builtins module itself is imported under another name**: `import builtins as bi; f = bi.getattr(report, '__setattr__'); f('status', 'PASS')`. | `calibrate-status-builtins-module-alias`; self-tests `round 22: a status write through an imported builtins module alias`, `... module-level builtins module alias` |
| **An assignment of another value precedes the factory import**: `ga = print; from builtins import getattr as ga; f = ga(...)`. | `calibrate-status-factory-import-after-assignment`; self-test `round 22: a status write through an assignment before the factory import` |
| **A conditional overwrite does not erase the factory import**: `from builtins import getattr as ga; if flag: ga = dict`. | `calibrate-status-factory-import-conditional-overwrite`; self-test `round 22: a status write through a conditionally overwritten factory import` |
| **A certain overwrite still ends the spelling** (false FAIL guard): `from builtins import getattr as ga; ga = print`, and `import builtins as bi; bi = other`. | self-tests `round 22: a status write through a certainly overwritten factory import`, `... certainly replaced builtins module alias` |
| **A certain overwrite ends the `builtins[...]` table reading** (false FAIL guard, GLM sibling r21 P1-1). | `_name_resolves_to_builtins` now reads `_values_in_force`; self-tests `round 23: a subscript write through a copied builtins module name` (true) and `... a certainly replaced module name` (false), mutant `calibrate-status-builtins-subscript-copy` |

### Closed by `fix-r1` round 23 (codex r22 two P1s / GLM r22 one P0 and one P1)

Round 22 gave every binding of a `builtins` alias its own event, but
keyed the events by the statement position, let the module-end binding
reach calls that ran before the import, and shadowed the no-op `bi = bi`.

| Shape | Mutant / self-test |
|---|---|
| **A harmless second alias of one import statement** (false FAIL guard): `import builtins as bi, os as bi; f = bi.getattr(...)`. | self-test `round 23: a harmless second alias of the same import statement`; each alias keeps its own column |
| **A builtins alias imported after every call** (false FAIL guard): `import json as bi; def main(): bi.getattr(...)`; `main(...)`; `import builtins as bi`. | self-test `round 23: a builtins alias imported after every call of the function`; `_binding_event_at(call_only=True)` |
| **A self-assigned module alias keeps the module reading**: `import builtins as bi; bi = bi; f = bi.getattr(...)`. | `calibrate-status-builtins-alias-self-assign`; self-test `round 23: a self-assigned builtins module alias` |

### Closed by `fix-r1` round 24 (codex r23 one P0 and one P1 / GLM boundary r23 one P0 and one P1)

Round 23 trusted the module-scope call list as complete and read a
`bi = bi` copy without looking at what happened to `bi` in between.

| Shape | Mutant / self-test |
|---|---|
| **A function that also escapes as a value runs after the later import**: `import json as bi; def main(): ...`; `main(...)`; `_cbs.append(main)`; `import builtins as bi`; `for cb in _cbs: cb(...)`. | self-tests `round 24: a builtins alias imported after a callback registration`, `... before a nested-scope call`; `_module_value_uses` keeps `call_only` off. No table mutant: the shape needs the import at module scope and the write inside a function, which one `(old, new)` replacement cannot express; the end-to-end repro is in the PR body (#1953) |
| **Direct calls only stay clean** (false FAIL guard): the round-23 shape still reads as no write. | self-test `round 24: a builtins alias imported after direct calls only` |
| **A copy of an already replaced alias is not the module** (false FAIL guard): `import builtins as bi; bi = Safe; bi = bi; f = bi.getattr(...)`. | self-test `round 24: a self-assigned builtins alias after a certain replacement`; `_resolve_builtins_module_copies` reads `events[name]` |

### Closed by `fix-r1` round 25 (codex r24 two P0s and one P1)

Round 24 treated "the module calls this function somewhere" as "the call
list is complete", and counted a plain store of the function name as
handing the function out.

| Shape | Mutant / self-test |
|---|---|
| **A conditional module import still leaves the module reading in force**: `import json as bi; if flag: import builtins as bi; bi = bi; f = bi.getattr(...)`. | self-test `round 25: a conditionally imported builtins alias before a self-assigned copy`; `_resolve_builtins_module_copies` only lets a binding past the last *reachable* module binding take the reading away |
| **The function is reached through a module-level alias**: `saved = main; import builtins as bi; saved(...)`. | self-test `round 25: a builtins alias reached through a module-level alias call`; `_module_alias_calls` relays the call sites |
| **The function is reached through `globals()`**: `globals()['main'](...)` produces no `ast.Name`, so the call list is not exhaustive. | self-test `round 25: a builtins alias reached through globals()`; `_module_open_calls` turns `call_only` off |
| **A name stored over but never read hands nothing out** (false FAIL guard): `main(...); main = None; import builtins as bi`. | self-test `round 25: a module name stored over but never read`; `_module_value_uses` counts `ast.Load` only |
| **A copy after a certain replacement is not the module** (false FAIL guard): round-24 shape kept green. | self-test `round 25: a builtins alias copied after a certain replacement` |

No table mutant: like round 24, these shapes need a module-scope trigger
plus a write inside a function, which one `(old, new)` replacement cannot
express. The end-to-end repro and the per-shape detector probes are in the
PR body (#1953).

### Closed by `fix-r1` round 26 (codex r25 two P0s and three P1s)

Round 25 resolved an alias to its final module-end binding, treated
`globals()`-style lookups as the only way a call escapes the name list, and
counted a binding inside `if False:` as a live import.

| Shape | Mutant / self-test |
|---|---|
| **The function is reached through an unpacked hand-off**: `a, b = main, print; import builtins as bi; a(...)`. | self-test `round 26: a builtins alias reached through an unpacked hand-off`; `_plain_alias_pairs` pairs the elements |
| **The function is reached through a dynamic-lookup alias**: `lookup = globals; import builtins as bi; lookup()['main'](...)`. | self-test `round 26: a builtins alias reached through a dynamic-lookup alias`; `_module_open_aliases` closes over the heads |
| **The function is reached through an attribute call**: `sys.modules['__main__'].main(...)`. | self-test `round 26: a builtins alias reached through an attribute call`; `_spelled_callee` records the site in `events.attr_calls` |
| **The function is reached through a literal `getattr` call**: `getattr(X, 'main')(...)`. | self-test `round 26: a builtins alias reached through a literal getattr call`; same site list |
| **A certain rebind before the call hands nothing off** (false FAIL guard): `a = main; a = lambda ...; import builtins as bi; a(...)`. | self-test `round 26: a module alias certainly rebound before the call`; `_module_alias_sources` keeps the last certain binding |
| **An unrelated `getattr` before the import is not a call site** (false FAIL guard): `getattr(object(), 'x')` next to a later import. | self-test `round 26: an unrelated module-level getattr before a later import`; `getattr` left `_OPEN_CALL_HEADS` |
| **An unrelated `getattr` after the import is not a call site** (false FAIL guard). | self-test `round 26: an unrelated getattr after the later import` |
| **A `getattr` inside a function the module never calls is not a call site** (false FAIL guard). | self-test `round 26: a getattr inside a function the module never calls`; `_module_open_calls` walks module scope only |
| **A builtins import inside a statically dead branch is not a binding** (false FAIL guard): `saved = main; if False: import builtins as bi; saved(...)`. | self-test `round 26: a builtins import inside a statically dead branch`; `_dead_line_ranges` / `_dead_shadowed` |

### Closed by `fix-r1` round 27 (codex r26 four P0s)

Round 26 dropped every binding when the resolving position was unknown,
split one dead branch into one range per statement, treated a
`globals()['main']` lookup position as the call position, and resolved an
alias only against the bindings textually before the call.

| Shape | Mutant / self-test |
|---|---|
| **A live import next to an unrelated dead branch stays live**: `import builtins as bi; ...; if False: x = 1`, no visible call of `main`. | self-test `round 27: a live import next to an unrelated dead branch`; `_dead_shadowed` only drops bindings that sit inside a dead range |
| **An import and the call inside the same dead branch still pair up**: `if False: import builtins as bi; main(...)`. | self-test `round 27: a builtins import and the call in the same dead branch`; one range per branch |
| **A dynamically looked-up function is a call site where it is called**: `held = globals()['main']; import builtins as bi; held(...)`. | self-test `round 27: a dynamically looked-up function called after a later import`; `_dynamic_result_names` adds the carrier name to `_module_open_calls` |
| **A binding on the loop back edge is a candidate**: `for ...: a(...); import builtins as bi; a = main`. | self-test `round 27: an alias bound after the call on a loop back edge`; `_module_alias_sources` follows the shared loop context |

Like rounds 24 and 25, these shapes need a module-scope trigger plus a
write inside a function, which one `(old, new)` replacement cannot express;
the end-to-end repro and the per-shape detector probes are in the PR body
(#1953).
### Closed by `fix-r1` round 28 (GLM boundary r26 one P0, three P1s; GLM sibling r26 two P1s)

Boundary found the round-27 back edge (its P0) and three holes around the
dynamic-head and dead-branch tables; sibling found the same two hand-off
forms missing on the `builtins`-module copy table and on the open heads.

| Shape | Mutant / self-test |
|---|---|
| **A dead branch's spans carry columns**: a live binding on the same line as a one-line `if False: ...` body is not swallowed. | no source-level self-test: a simple statement cannot share a line with the `if`, so the shape is exercised by a unit probe in the PR body (`_dead_line_ranges` `(2, 2)` versus `((2, 4), (2, 10))`) |
| **The function is reached through an attribute-form head**: `_bi.eval('main(...)')`. | self-test `round 28: a builtins alias reached through an attribute-form head`; one head table for the name and attribute spellings |
| **A bare attribute spelled like a head is not a call site** (false FAIL guard): `_cfg.vars` without a call. | self-test `round 28: a bare attribute spelled like a head is not a call site` |
| **A module-level walrus hands the function over**: `if (_saved := main) or flag: ...`. | self-test `round 28: a module-level walrus hands the function over` |
| **An annotated assignment hands the function over**: `_saved: object = main`. | self-test `round 28: an annotated assignment hands the function over` |
| **An unpacked copy of the builtins module**: `_bi, other = _bi, None`. | self-test `round 28: an unpacked copy of the builtins module`; the copy table reads the per-target source |
| **A dead import does not restore the builtins reading** (false FAIL guard): `import builtins as bi; bi = Safe; if False: import builtins as bi; bi = bi`. | self-test `round 28: a dead import does not restore the builtins reading` |
| **An open alias rebound before the call is not a head** (false FAIL guard): `lookup = globals; lookup = print; lookup(x)`. | self-test `round 28: an open alias rebound before the call is not a head`; the head is resolved at the call site |
| **An alias pair before a `Starred` target still pairs**: `a, *rest = main, print, x`. | self-test `round 28: an alias pair before a Starred target` |

Like rounds 24 to 27, these shapes need a module-scope trigger plus a write
inside a function, which one `(old, new)` replacement cannot express; the
end-to-end repro and the per-shape detector probes are in the PR body
(#1953).

### Closed by `fix-r1` round 29 (codex r27 two P0s, two P1s; GLM boundary r27 three P1s, one P2; GLM sibling r27 two P1s, one P2)

Codex found the function-scope copy builder handing the AST node where the
resolver expects a name (P0), the self-copy `lookup = lookup` hiding the head
(P0), the carrier never expiring (P1), and a nested dead branch counted as an
outer one (P1).  GLM found the carrier family still reading only
`ast.Assign` (P1), the `Starred` pairing missing one end (P1), the
attribute-spelled `getattr` factory unrecognised (P1), `while False:` not
treated as dead (P2), and a carrier taken from a rebound alias still counted
as a site (P2).

| Shape | Mutant / self-test |
|---|---|
| **A function-local copy of the builtins module**: `import builtins as bi; bi = bi` inside the function. | self-test `round 29: a function-local copy of the builtins module`; the function-scope copy builder shares `_alias_pairs` instead of storing the AST node |
| **A self-copy of a dynamic head alias keeps the head**: `lookup = globals; lookup = lookup; import builtins as bi; lookup()['main'](...)`. | self-test `round 29: a self-copy of a dynamic head alias keeps the head`; `_module_alias_sources` keys its guard on `(name, position)` and resolves a self-copy strictly before itself |
| **A carrier rebound before the call is not a site** (false FAIL guard): `f = globals()['main']; f = lambda ...; import builtins as bi; f(...)`. | self-test `round 29: a dynamic-lookup carrier rebound before the call`; `_dynamic_result_in_force` resolves the in-force binding at the call site |
| **A nested dead branch import is out of reach of the outer one** (false FAIL guard): `if False:\n    if False:\n        import builtins as bi\n    main(...)`. | self-test `round 29: a nested dead branch import is out of reach of the outer one`; `_dead_shadowed` requires every containing span to cover the query |
| **An annotated assignment carries a looked-up function**: `_held: object = globals()['main']`. | self-test `round 29: an annotated assignment carries a looked-up function`; the carrier table reads the same binding shapes as `_alias_pairs` |
| **A module walrus carries a looked-up function**: `if (_held := globals()['main']) or flag: ...`. | self-test `round 29: a module walrus carries a looked-up function` |
| **An attribute-form head in the carrier value**: `_held = _bi.vars()['main']`. | probe in the PR body (`carrier_attr_head`, old/mid `False` -> new `True`); `_contains_dynamic_lookup` accepts `ast.Attribute` heads |
| **A `Starred`-first target still pairs the tail element**: `*_, a = 0, main`. | self-test `round 29: a Starred-first target still pairs the tail element`; `_plain_alias_pairs` aligns the determinable tail from the end |
| **A value-side `Starred` keeps the prefix pairs**: `a, b = main, *rest`. | probe in the PR body (`starred_value`) |
| **An attribute-spelled `getattr` factory hands the function over**: `mod.getattr(mod, 'main')(...)`. | self-test `round 29: an attribute-spelled getattr factory hands the function over`; `_spelled_callee` matches `Attribute.attr == 'getattr'` |
| **A `while False:` import does not restore the module reading** (false FAIL guard). | self-test `round 29: a while-False import does not restore the module reading`; `_dead_line_ranges` covers `ast.While` with a false constant |
| **A carrier built from a rebound alias is not a site** (false FAIL guard): `lookup = globals; lookup = print; held = lookup(); import builtins as bi; held(...)`. | self-test `round 29: a carrier built from a rebound alias is not a site`; the alias table is sorted into source order before the late dynamic pass |

Like rounds 24 to 28, these shapes need a module-scope trigger plus a write
inside a function, which one `(old, new)` replacement cannot express; the
end-to-end repro and the per-shape detector probes are in the PR body
(#1953).

### Closed by `fix-r1` round 30 (codex r28 two P0s, one P1; GLM sibling r28 one P1, one P2)

Codex found the round-29 ordering fix itself leaving the late dynamic pass
holding stale list indices (P0), the carrier self-copy resolving to itself
instead of the binding before it (P0), and a bare annotation `x: int`
crashing the detector because it walked a `None` value (P1).  GLM found the
function-side copy resolution still ignoring the dead spans the module-side
twin already passed (P1), and two shapes that lived only in the PR body
(P2).

| Shape | Mutant / self-test |
|---|---|
| **The late dynamic pass marks the binding it inspected, not the index it was queued with** (false FAIL guard): sort the table into source order, then run the late pass. | self-test `round 30: a carrier marked inside a conditional is not the later rebind`; the late list holds the entry object and finds it by identity |
| **A carrier self-copy keeps the looked-up value**: `held = globals()['main']; held = held; import builtins as bi; held(...)`. | self-test `round 30: a carrier self-copy keeps the looked-up value`; `_dynamic_result_in_force` resolves a self-copy strictly before itself |
| **A bare annotation binds nothing** (false FAIL guard): `saved: object` with no right-hand side. | self-test `round 30: a bare annotation binds nothing`; the binding walk skips annotations without a value instead of inspecting `None` |
| **A dead function-local import does not decide a live copy** (false FAIL guard): `if False: import builtins as bi` inside the function, then `bi = bi`. | self-test `round 30: a dead function-local import does not decide a live copy`; the function-side copy resolution receives the dead spans |
| **An attribute-form head carries a looked-up function**: `_held = _bi.vars()['main']`. | self-test `round 30: an attribute-form head carries a looked-up function` (was a PR-body probe only) |
| **A value-side `Starred` still pairs the prefix element**: `a, b = main, *[print]`. | self-test `round 30: a value-side Starred still pairs the prefix element` (was a PR-body probe only) |

Like rounds 24 to 29, these shapes need a module-scope trigger plus a write
inside a function, which one `(old, new)` replacement cannot express; the
end-to-end repro and the per-shape detector probes are in the PR body
(#1953).

### Closed by `fix-r1` round 31 (codex r29 one P0; GLM r29 three roles on the same P1, one boundary P1, two P3)

Codex and three GLM roles independently hit the same hole: the round-30
self-copy recursion carried no visited set, so a self-copy inside a loop
re-entered itself through the loop back-edge clause -- which ignores
`strict` on purpose -- until the audit died with `RecursionError` (false
FAIL).  GLM boundary also found a dead-branch carrier deciding a live
self-copy (false FAIL introduced by round 30).

| Shape | Mutant / self-test |
|---|---|
| **A self-copy inside a loop is resolved once** (false FAIL guard): `held = globals()['main']; held = lambda: ...; for _ in (0,): held = held; held(x)`. | self-test `round 31: a self-copy inside a loop is resolved once`; `_dynamic_result_in_force` keeps a `(name, position, strict)` visited set, the guard `_module_alias_sources` already had |
| **A dead carrier does not decide a live self-copy** (false FAIL guard): `if False: held = globals()['main']` then a live `held = held`. | self-test `round 31: a dead carrier does not decide a live self-copy`; the carrier resolution now receives `_dead_line_ranges` and drops bindings that a live query cannot reach |

Like rounds 24 to 30, these shapes need a module-scope trigger plus a write
inside a function, which one `(old, new)` replacement cannot express; the
end-to-end repro and the per-shape detector probes are in the PR body
(#1953).

### Closed by `fix-r1` round 32 (GLM r30 sibling P1-1 and boundary P1-1 on the same hole, boundary P1-2 a false PASS, sibling P2-1 a missing positive)

Round 31 taught the carrier resolver about statically dead branches but left
its twin `_module_alias_sources` without the filter, so a dead alias still
decided a live hand-off (false FAIL, two roles independently).  The same dead
alias kept the function in `_module_value_uses`, which turned `call_only` off
and reported the clean module as a write even after the alias table was
fixed -- the second half of the same chain.  GLM boundary also found the
walrus callee: `(_g := held)(report)` is neither a `Name` nor an `Attribute`,
so no branch of `_module_open_calls` saw the call and a real carrier write
stayed green (false PASS, the direction that matters for a ruler).

| Shape | Mutant / self-test |
|---|---|
| **A dead alias does not hand the function over** (false FAIL guard): `if False: saved = main`, then a live `saved(object())` after `import builtins as bi`. | self-test `round 32: a dead alias does not hand the function over`; `_module_alias_sources` takes the dead spans and all three call sites pass them |
| **A dead alias is not a value use** (false FAIL guard): the same binding used to keep `main` in `_module_value_uses`, which disabled `call_only` for the whole function. | self-test `round 32: a dead alias does not hand the function over`; `_module_value_uses` drops loads that sit inside `_dead_line_ranges` |
| **A dead open alias is not a site** (false FAIL guard): `if False: lk = globals`, then a live `lk(object())`. | self-test `round 32: a dead open alias is not a site`; the alias branch of `_module_open_calls` resolves through the dead-aware table |
| **A walrus callee reaches the carrier** (false PASS guard): `held = globals()['main']; import builtins as bi; (_g := held)(object())`. | self-test `round 32: a walrus callee reaches the carrier`; `_module_open_calls` unwraps an `ast.NamedExpr` callee to its value |
| **A live self-copy inside a loop keeps the carrier** (regression guard): the round-31 recursion guard must not prune the live case. | self-test `round 32: a live self-copy inside a loop keeps the carrier`; the loop call stays an open site |
| **A live alias hand-off keeps the site** (control): `saved = main` outside any dead branch. | self-test `round 32: a live alias hand-off`; both the alias table and the value-use set keep `main` |

Like rounds 24 to 31, these shapes need a module-scope trigger plus a write
inside a function, which one `(old, new)` replacement cannot express; the
end-to-end repro and the per-shape detector probes are in the PR body
(#1953).

### Closed by `fix-r1` round 33 (self-check of the 331-mutant table: three `calibrate-match-capture-*` mutants came back SURVIVED)

The full table at the round-32 head reported three SURVIVED mutants in the
`match` family, and the reason was not a missed write: the audit died.
`ast.match_case` carries no position, and both binding walks read
`node.lineno` off it -- `_module_binding_events` for a module-scope `match`,
`_imported_write_aliases` for one inside a function.  The crash arrived as
`#164-fix-r7 aggregation-audit: AttributeError: 'match_case' object has no
attribute 'lineno'`, which is not the failure the mutants assert, so all three
were recorded as survivors.

| Shape | Mutant / self-test |
|---|---|
| **A module-scope `match` capture keeps its binding** (crash guard): `match operator.setitem: case h: h(...)`. | self-test message `... crashed on a match capture with no position (round 33)`; `_match_case_position` falls back to the pattern, then to the first body statement |
| **A function-local `match` capture keeps its binding** (crash guard): the same shape inside a `def`. | self-test message `... lost a match capture (round 33)`; the function-side branch passes the same position instead of the case node |
| **A wildcard case binds nothing** (negative): `case _:` with no body capture. | self-test message `... bound a wildcard case (round 34)`; `_match_capture_names` returns no name for a bare `MatchAs()` |

Like rounds 24 to 32, these shapes need a module-scope trigger plus a write
inside a function, which one `(old, new)` replacement cannot express; the
end-to-end repro and the per-shape detector probes are in the PR body
(#1953).

### Closed by `fix-r1` round 34 (codex r33 P0-1 / P0-2, GLM r33 sibling P1-1 / P2-2 / P2-3, boundary P1-1 / P1-2)

Round 32 narrowed the audit in two places and both cuts went one step too far.
`_module_value_uses` dropped *every* load inside a dead span, so a dead
`register(main)` stopped counting as a hand-off and `call_only` came back on --
a false PASS in the direction that matters, since the dead code is analysed on
purpose here (the negative controls inject their mutants inside `if False:`).
Only the hand-off the alias table already resolves through its own dead filter
may be dropped: the source of a plain-name binding.  The walrus reading added
in round 32 reached `_module_open_calls` and nothing else, so
`(_g := sd.main)(report)` and `(_g := getattr(sd, 'main'))(report)` never
entered the attribute call table -- the same false PASS, one table over.  Round
33 fixed two of the three `match_case` walks; the third one sits on the
receiver path and still read `node.lineno` off a case node.

| Shape | Mutant / self-test |
|---|---|
| **A dead escape is still a value use** (false PASS guard): `import builtins as bi` then `if False: register(main)`. | self-test message `... dropped a dead escape as a value use (round 34)`; `_module_value_uses` only drops the source of a plain-name binding inside a dead span |
| **A dead alias is still not a value use** (round-32 guard kept): `if False: saved = main`, then a live `saved(object())`. | self-test message `... kept a dead alias as a value use (round 32)`, still green |
| **A walrus attribute call reaches the table** (false PASS guard): `(_g := sd.main)(report)` and `(_g := getattr(sd, 'main'))(report)`. | self-test messages `... lost the walrus attribute call (round 34)` / `... lost the walrus getattr call (round 34)`; `_callee_expr` unwraps the `NamedExpr` for every caller, nested walrus included |
| **A walrus plain call reaches the alias table** (false PASS guard): `saved = main; (_g := saved)(report)`. | self-test message `... lost the walrus plain call (round 34)`; `_module_call_positions` records the unwrapped callee |
| **A walrus callee is still a carrier site** (round-32 guard, now counted): `held = globals()['main']; (_g := held)(object())`. | self-test message `... lost the walrus callee site (round 32)` now asserts the site count (a plain non-empty check was a tautology: `globals()` is itself a site) |
| **A match capture does not crash the receiver walk** (crash guard): `from builtins import setattr as _sa`, a `match` capture, then `_sa(md, 'common', object())`. | self-test messages `... crashed on a match capture in the receiver walk (round 34)` / `... lost a receiver withdrawn through a match (round 34)`; `_scope_import_events` passes the case position instead of the case node |
| **A wildcard case binds nothing** (negative): `case _:` with no body capture. | self-test message `... bound a wildcard case (round 34)` |

Like rounds 24 to 33, these shapes need a module-scope trigger plus a write
inside a function, which one `(old, new)` replacement cannot express; the
end-to-end repro and the per-shape detector probes are in the PR body
(#1953).

### Closed by `fix-r1` round 35 (codex r34 P0-1 / P0-2 / P1, GLM boundary r34 #1 / #2, sibling r34 P3-2)

Round 34's three registration walks for `match` captures asked the *Match
statement's* flow marks whether the capture ran, and a top-level
`match 0: case [_sa]:` answered "unconditional" -- the capture then erased
`from builtins import setattr as _sa` even though `0` never matches a sequence
pattern (false PASS).  The dead-span drop in `_module_value_uses` still
collected *every* load under an alias statement's value once any pair matched,
so a partial pairing escaped loads the alias table never resolves:
`if False: _f, _x = md, _register(main)` dropped the `main` escape, and the
chained target `if False: _a = _obj.slot = main` dropped the load the
attribute write consumes.  A bare head spelling (`globals`) was counted as an
open site unconditionally, so `globals = lambda: {...}; held = (_g :=
globals)()['main']` called the *lambda* and still reported a write (false
FAIL).

| Shape | Mutant / self-test |
|---|---|
| **An unmatched match capture is conditional** (false PASS guard): `from builtins import setattr as _sa`, `match 0: case [_sa]:`, then `_sa(md, 'common', object())`. | self-test message `... let an unmatched match capture erase the write helper (round 35)`; all three registration walks force `conditional=True` for a capture (site 3 spells it directly, sites 1-2 pass `force_conditional`) |
| **A dead partial-pairing escape is a value use** (false PASS guard): `if False: _f, _x = md, _register(main)`. | self-test message `... dropped a dead partial-pairing escape as a value use (round 35)`; `_alias_source_nodes` returns only the value nodes that pair with a plain-name target |
| **A dead chained-target escape is a value use** (false PASS guard): `if False: _a = _obj.slot = main`. | self-test message `... dropped a dead chained-target escape as a value use (round 35)`; a target that consumes without pairing (attribute/subscript) keeps every load of that statement |
| **A dead paired element is still dropped** (round-32 guard kept): `if False: _f, _x = main, _register(other)`. | self-test message `... kept a dead alias as a value use (round 32)`/`... kept a dead paired alias as a value use (round 35)`; only elements that actually pair are dropped |
| **A shadowed head is not an open site** (false FAIL guard): `globals = lambda: {'main': lambda x: None}; held = (_g := globals)()['main']; import builtins; held(object())`. | self-test message `... kept a shadowed head as an open site (round 35)`; `_head_shadowed` proves the shadow only for a binding that certainly ran, is in force, and is provably inert (`_provably_inert`: a lambda with no call, or a literal); the late pass in `_module_alias_bindings` now re-resolves every entry and overwrites both directions |
| **An unshadowed walrus-callee head stays a site** (round-32/34 guard kept): `held = (_g := globals)()['main']; import builtins; held(object())`. | self-test message `... lost the unshadowed walrus-callee site (round 35)` |
| **The site count is exactly two** (r34 boundary P3-2): the walrus-callee fixture has exactly two sites. | the round-32 count assertion now pins `!= 2` instead of `< 2`, so an extra site fails too |
| **A nested walrus callee spells the attribute** (r34 sibling P3-2): `(_g := (_h := getattr)(sd, 'main'))(report)`. | `_spelled_callee` reads the inner callee through `_callee_expr` like every other call table |

Like rounds 24 to 34, these shapes need a module-scope trigger plus a write
inside a function, which one `(old, new)` replacement cannot express; the
end-to-end repro and the per-shape detector probes are in the PR body
(#1953).

### Closed by `fix-r1` round 36 (codex r35 P0-1..P0-5 / P1-1 / P1-2, GLM r35 sibling P1-1 / P2-2, boundary r35 #1 / #2)

Round 35 gated the bare head spelling behind `_head_shadowed`, and every one
of these findings is a way that gate (or its neighbours) answered wrong.
`_provably_inert` accepted *any* call-free lambda, so `globals = lambda: vars`
still handed out the builtin (false PASS; the r35 self-test only covered a
literal-dict body).  The binding table never registered `import` / `del`, so
`from builtins import globals` and `del globals` after a shadow left the stale
shadow in force.  `_flow_marks` marked a walrus inside a `BoolOp` tail or an
`IfExp` body unconditional, so `report is None and (globals := lambda: {})`
shadowed the builtin outright.  `_contains_dynamic_lookup` was a plain
`ast.walk`, so a lambda parameter and a deferred body read the *module* binding
table: `held = (lambda globals: globals('main'))(eval)` counted the parameter
as the module's shadow.  `_alias_source_nodes` accepted any target shape, so a
leaf-target escape (`_a, _obj.slot = main, other`) and the walrus argument /
target escapes (`_register(_a := main)`, `_obj.slot = (_a := main)`) were
dropped as value uses.  A sole irrefutable capture (`match x: case _sa:`) was
forced conditional like any other match, withdrawing a receiver it really
binds (false FAIL).  The alias branch of the open-call walk resolved the
terminal head but never asked whether *it* is shadowed: `lk = globals` after
an inert shadow still reported a site.  On the receiver side the write decoders
read the raw callee, so `(_g := _f)(md, 'common', <stub>)` vanished (runtime
differential re-verified, false PASS).

| Shape | Mutant / self-test |
|---|---|
| **A call-free lambda over a head is not inert** (false PASS guard): `globals = lambda: vars` / `lambda: sys.modules[__name__].__dict__`, then the double-call chain. | self-test message `... kept a call-free lambda over a head as inert (round 36)`; `_inert_literal_expr` is a recursive whitelist (lambdas with inert defaults and body, constants, literal containers) -- anything else fails closed |
| **A re-imported head is back in force** (false PASS guard): `globals = lambda: {...}; from builtins import globals`, then the call chain. | self-test message `... let a reimported head stay shadowed (round 36)`; `_module_alias_bindings` registers `Import`/`ImportFrom` head names as dynamic, non-inert entries |
| **A deleted head is back in force** (false PASS guard): `globals = lambda: {...}; del globals`, then the call chain. | self-test message `... let a deleted head stay shadowed (round 36)`; `ast.Delete` head names register the same way |
| **A short-circuit walrus does not shadow** (false PASS guard): `report is None and (globals := lambda: {})`, then `held = (_g := globals)()['main']`. | self-test message `... kept a short-circuit walrus shadow (round 36)`; `_flow_marks` marks `BoolOp` values[1:] and `IfExp` body/orelse tails conditional |
| **A lambda parameter is not the module's shadow** (false PASS guard): `globals = lambda: {...}; held = (lambda globals: globals('main'))(eval)`. | self-test message `... kept a lambda parameter as a head shadow (round 36)`; `_contains_dynamic_lookup` walks with the local names lambdas and comprehensions bind |
| **A deferred lambda body is a carrier outright** (false PASS guard): `globals = lambda: {...}; held = (lambda: globals()['main'])()`. | self-test message `... kept a deferred lambda body on a shadowed head (round 36)`; a deferred body treats a head spelling as a carrier, fail-closed |
| **A leaf-target escape is a value use** (false PASS guard): `if False: _a, _obj.slot = main, other`. | self-test message `... dropped a leaf-target escape as a value use (round 36)`; `_leaf_targets_plain` gates pairing at the leaf level |
| **A walrus-argument escape is a value use** (false PASS guard): `if False: _register(_a := main)`. | self-test message `... dropped a walrus-argument escape as a value use (round 36)`; a walrus not bound as a statement of its own is not a plain hand-off |
| **A walrus-target escape is a value use** (false PASS guard): `if False: _obj.slot = (_a := main)`. | self-test message `... dropped a walrus-target escape as a value use (round 36)`; the value escapes through the bigger expression |
| **An irrefutable capture keeps its binding** (false FAIL guard): a sole `case _sa36:` over any subject, then `_sa36(md, 'common', object())`. | self-test message `... kept a receiver written through an irrefutable capture (round 36)`; `_irrefutable_capture` (sole case, no guard, bare `MatchAs` chain) keeps the capture certain |
| **An alias hop to a shadowed head is not a site** (false FAIL guard): `globals = lambda: {...}; lk = globals; held = lk()['main']`. | self-test message `... kept an alias hop to a shadowed head as a site (round 36)`; both alias branches (`_module_open_calls`, `_contains_dynamic_lookup`) ask `_head_shadowed` about the terminal head |
| **A walrus callee on the receiver side writes** (false PASS guard): `_f = md.__setattr__; (_g := _f)(md, 'common', object())`. | self-test message `... lost the walrus-callee receiver write (round 36)`; `_write_call_target` and `_container_callee` unwrap the `NamedExpr` callee, and the relay walk reads through `_callee_expr` |
| **The r35 site assertion actually checks membership** (r35 P1-2): the walrus-callee fixture's two sites must include the `_held` call. | `_m35_walrus_head` compares `site[0]` against the call position -- the sites are `(position, loops)` pairs, so the r35 bare-position membership check was a tautology |

Like rounds 24 to 35, these shapes need a module-scope trigger plus a write
inside a function, which one `(old, new)` replacement cannot express; the
end-to-end repro and the per-shape detector probes are in the PR body
(#1953).

### Closed by `fix-r1` round 37 (codex r36 P0-1..P0-5 / P1-1..P1-6, all eleven)

Round 36 pinned the alias hop on the *call position*, so `lk = globals`
captured before a later shadow still resolved the builtin at the position
`lk` was read -- the round-36 check asked `_head_shadowed` about the alias
name at the wrong spot (false PASS).  A `match` capture binds the subject's
*value* at the statement, but the three registration walks resolved the
captured name before the subject was processed, so `match _sa: case _sa:`
erased an imported write helper it really rebinds (false PASS).  `del`
with a tuple target and `from builtins import *` never registered, so both
left a stale shadow in force (false PASS).  `_container_base` read the raw
`node.func`, so `next((_h := iter)(hs))` and `(_g := hs.copy)()[0](...)`
returned `None` for the whole chain and dropped the write (false PASS).
On the false-FAIL side: `lambda: -1` was not provably inert while `lambda: 1`
was; a statically certain short-circuit (`True and (globals := ...)`) still
got the conditional mark; a class-body `from builtins import globals`
shadowed the module binding it never touches; a comprehension target
shadowed its own first iterable; `collect()` read the raw callee so a
walrus callee freshly rebound to a no-op still withdrew its receiver; and
the per-child recursion in `scan` crashed on a 1100-deep expression.

| Shape | Mutant / self-test |
|---|---|
| **A head captured before a shadow is resolved at the read** (false PASS guard): `lk = globals; globals = lambda: {'main': lambda x: None}; held = lk()['main']`. | self-test message `... kept a head captured before a shadow out of the site table (round 37)`; `_module_alias_head_reads` returns `(head, read-position)` pairs and both twin branches (`_module_open_calls`, `_contains_dynamic_lookup`) test shadows at the read position |
| **A match capture rebinds from the subject** (false PASS guard): `from builtins import setattr as _sa; match _sa: case _sa:`, then `_sa(md, 'common', object())` inside a function. | self-test message `... lost the write helper a match capture rebinds from the subject (round 37)`; all three registration walks append the capture to `pending` with the subject node, resolved against the bindings in force at the statement |
| **A tuple delete unshadows the head** (false PASS guard): `globals = lambda: {'main': lambda x: None}; del (globals,)`, then the call chain. | self-test message `... kept a tuple delete from unshadowing the head (round 37)`; `_deleted_names` recurses through `Tuple`/`List` elements |
| **A star import unshadows the head** (false PASS guard): `globals = lambda: {'main': lambda x: None}; from builtins import *`, then the call chain. | self-test message `... kept a star import from unshadowing the head (round 37)`; the `ImportFrom` branch of `_head_binding_names` returns every head on `*` |
| **A walrus wrapper callee still carries the container** (false PASS guard): `hs = [getattr(md, k) for k in ('__setattr__',)]; next((_h := iter)(hs))('common', object())`. | self-test message `... lost the receiver behind a walrus wrapper callee (round 37)`; `_container_base` reads the callee through `_callee_expr` |
| **A walrus copy-wrapper subscript still carries the container** (false PASS guard): `hs = [getattr(md, k) for k in ('__setattr__',)]; (_g := hs.copy)()[0]('common', object())`. | self-test message `... lost the receiver behind a walrus copy-wrapper subscript (round 37)`; same `_callee_expr` unwrap on the container base |
| **A signed-literal lambda stays inert** (false FAIL guard): `globals = lambda: {'main': lambda x: -1}`, then the call chain -- no site. | self-test message `... kept an inert signed-literal lambda as a head shadow (round 37)`; `_inert_literal_expr` recurses through `USub`/`UAdd`/`Invert`/`Not` |
| **A statically certain short-circuit walrus shadows outright** (false FAIL guard): `True and (globals := lambda: {'main': lambda x: None}); held = (_g := globals)()['main']`. | self-test message `... kept a statically certain short-circuit walrus as a conditional shadow (round 37)`; `_static_truth` via `ast.literal_eval` drives the `BoolOp`/`IfExp` marks |
| **A class-body import does not shadow the module** (false FAIL guard): `class _C37: from builtins import globals` at module scope, then the call chain -- no site. | self-test message `... let a class-body import shadow the module binding (round 37)`; `_module_alias_bindings` skips class spans unless the name is declared `global` there |
| **A comprehension target does not shadow its first iterable** (false FAIL guard): `globals = lambda: [0]; held = [lambda x: None for globals in globals()][0]`, then the call -- no site. | self-test message `... let a comprehension target shadow its own first iterable (round 37)`; `scan` builds the comprehension scope incrementally, iterable first, target after |
| **A walrus callee freshly rebound to nothing stops withdrawing** (false FAIL guard): `_f = setattr; _f = lambda *a: None; (_g := _f)(md, 'common', object())`. | self-test message `... withdrew a receiver on a walrus callee freshly rebound to nothing (round 37)`; `collect()` reads every callee through `_callee_expr` |
| **A deep expression does not crash the walk** (crash guard): `held = 1 + 1 + ... (1100 operands)`. | self-test message `... let a deep expression crash the site walk (round 37)`; `scan` keeps an explicit stack instead of per-child recursion |

Like the earlier rounds, these shapes need a module-scope trigger plus a
write inside a function, which one `(old, new)` replacement cannot express;
the end-to-end repro and the per-shape detector probes are in the PR body
(#1953).

### Closed by `fix-r1` round 38 (codex r37 P0-1..P0-7 / P1-1, all eight; GLM r37 boundary P0-1, concurrency P0-1, sibling P1-1, boundary P1-2, sibling P2-2 / P2-4, boundary P2-3 / concurrency P2-3 / simplify P2-1)

Round 37 settled an `IfExp` through `_static_truth`, but its spelling ("is
not False" / "is not True") let an *unknown* test keep both arms certain:
`... if flag else ...` runs each arm conditionally, in both directions.
`_static_truth` itself answered the *syntax* through `ast.literal_eval`
where the runtime builds the object with the module's own binding --
`set = lambda: True; set() or (globals := ...)` read truthy through the
rebound `set` and marked the walrus certain (false PASS).  The inert walk
recursed per node, so a 1100-deep unary chain in a lambda body took the
whole audit down.  The class span started at the `class` header, so the
bases and keywords -- which run in the *enclosing* scope -- were read as
class-local, and a `global` statement nested under an `if` in the class body
never registered.  The dynamic-lookup scan stopped at the comprehension
branch instead of descending into the target, and its alias branch dropped
`loops`, so a back-edge rebind inside the loop never reached the resolver.
`_irrefutable_capture` proved the *pattern* always binds but never asked
whether the *subject* is resolvable, so a walrus / `IfExp` / `BoolOp`
subject cleared an imported write helper through a value the resolver never
made (false PASS).  `_container_base` / `_container_callee` trusted the
wrapper *spelling*, so a locally rebound `iter = lambda hs: ...` still
descended into its argument and flagged a clean module (false FAIL).  The
receiver-side scans read the raw callee, so walrus callees dropped the
dynamic-`attrgetter` relay and the `getattr` decode.  A star import
unshadowed heads the source never exports (`from json import *`), while a
builtins star must restore the public heads yet still skip underscore
names.  Comprehension parts after the first iterable run once per item, so
a walrus in an `if` clause over an empty iterable never runs -- but a
non-empty literal iterable keeps it certain.  The sources and head-reads
alias resolvers were line-for-line twins (every fix landed twice) and the
per-hop recursion crashed on a deep chain.

| Shape | Mutant / self-test |
|---|---|
| **An unknown IfExp test runs each arm conditionally** (false PASS guard): `flag = report is None; _x = (globals := lambda: {'main': lambda x: None}) if flag else None`, then the call chain. | self-test `... let an unknown IfExp test keep both arms certain (round 38)`; only the arm a literal test settles keeps the parent certainty, with two counter-guards (`... dropped the certain IfExp arm a literal-True test settles` / `... orelse a literal-False test settles`) pinning that the certain arm is not dropped either |
| **A call the module rebound is not a literal** (false PASS guard): `set = lambda: True; set() or (globals := lambda: {'main': lambda x: None})`, then the call chain. | self-test `... trusted literal_eval on a call the module had rebound (round 38)`; `_static_truth` returns None for any expression containing a Call, with the falsy-literal counter-guard `... lost the certain tail a falsy literal keeps running (round 38)` |
| **A deep unary lambda shadow is inert without crashing** (crash guard): `globals = lambda: ----…1` (1100 unary ops), then the call chain. | self-tests `... let a deep unary literal crash the inert walk (round 38)` and `... lost the inertness of a deep unary lambda shadow (round 38)`; `_inert_literal_expr` walks an explicit stack (same rule as `_flow_marks`) |
| **Class bases run in the enclosing scope** (false PASS guard): `saved = globals; globals = lambda: {...}; class C(*((globals := saved) and ())):` -- the shadow is gone and the site must appear. | self-test `... read a class-header walrus as a class-local binding (round 38)`; the class span starts at the first body statement, not the `class` header |
| **A class-body binding does not clear the module shadow** (false FAIL guard): `class C: globals = saved` after a shadow -- no site. | self-test `... let a class-body binding clear the module shadow (round 38)` |
| **A `global` nested in the class body declares the module binding** (false PASS guard): `class C:` with `if False: global globals` above `globals = saved` -- the shadow is gone. | self-test `... missed a global statement nested in the class body (round 38)`; the Global collection walks the class body's statement tree, skipping nested scopes |
| **A comprehension target is read on the way to the store** (false PASS guard): `held = [value for globals()['value'] in (holder.get('main'),)][0]`. | self-test `... dropped the carrier a comprehension target reads (round 38)`; the scan appends the target expression before the target names bind |
| **An exotic match subject stays conditional** (false PASS guard): `match (_sa := _sa):` / `(_sa if True else None)` / `(_sa or None):` with `case _sa:`, then `_sa(md, 'common', object())` -- the helper must withdraw. | self-test `... let an exotic match subject clear a real write helper (round 38)`; a capture keeps the certain reading only over a plain name or a provably inert subject (`_capture_subject_resolvable`, and `_helper_from_value` unwraps the walrus subject), with the inert-subject counter-guard `... kept a helper through a capture bound to an inert subject (round 38)` |
| **A rebound wrapper spelling is not the builtin** (false FAIL guard): `iter = lambda hs: (h for h in (lambda *a: None,)); next((_h := iter)(hs))('common', object())` -- the receiver must not withdraw. | self-test `... trusted a wrapper spelling the scope had rebound (round 38)`; `_container_base` / `_container_callee` take a `wrapper_fresh` guard (`_freshly_rebound(..., allow_container=False)`) at every scoped call site |
| **A walrus callee relays a dynamic attrgetter** (false PASS guard): `_f = operator.attrgetter(k); _h = (_g := _f)(md); _h('common', object())`. | self-test `... lost the receiver a walrus callee handed a dynamic attrgetter (round 38)`; the first scan, the assign-value scan and the dynamic-handle consumption all read the callee through `_callee_expr` |
| **A walrus `getattr` callee takes its receiver** (false PASS guard): `_f = (_q := getattr)(md, k); _f('common', object())`. | self-test `... lost the receiver a walrus getattr callee took (round 38)`; `_callable_name` unwraps the `NamedExpr` callee once for every decode table |
| **A star import unshadows only what the source exports** (false PASS guards ×3): `from json import *` keeps the shadow; `from builtins import *` restores the public heads; a shadowed `__import__` stays shadowed through a builtins star. | self-tests `... let a json star import unshadow a head json never exports (round 38)` / `... lost the builtin a builtins star import restores (round 38)` / `... let a builtins star import unshadow the underscore __import__ (round 38)`; `_head_binding_names` answers the public head spellings for `builtins` and `[]` otherwise |
| **Comprehension parts run once per item** (false PASS + false FAIL guards): `[x for x in () if (globals := lambda: {...})]` never runs the walrus; over `(0,)` it certainly runs. | self-tests `... kept a comprehension-if walrus certain over an empty iterable (round 38)` / `... dropped the certain walrus a non-empty literal iterable runs (round 38)`; `_static_nonempty` answers literal-container emptiness, and a part keeps the parent certainty only while every iterable so far provably yields an item with no unknown filter in front |
| **A loop back-edge alias rebind is visible to the scan** (false PASS guard): `for _ in (0, 1): held = lk()['main']; lk = globals`. | self-test `... dropped the carrier a loop back-edge alias rebinds (round 38)`, counter-guard `... kept a site behind an alias that never reaches a head (round 38)`; the alias branch of `_contains_dynamic_lookup` passes `loops` to the resolver |
| **A deep alias chain resolves without recursion** (crash guard): the backward chain `a1200 = globals; a1199 = a1200; ...; a1 = a2`, then `held = a1()['main']`. | self-tests `... let a deep alias chain crash the head resolver (round 38)` / `... lost the head a deep alias chain reaches (round 38)` / control `... kept a site behind a deep chain that never reaches a head (round 38)`; `_module_alias_head_reads` keeps an explicit `(name, position, strict, seen)` worklist and `_module_alias_sources` is that walk reduced to names -- one algorithm, one place |

One r37 finding is closed as *not a bug*: GLM boundary#5, a `MatchOr` of two
bare captures (`case _sa | _sb:`).  `ast.parse` accepts the pattern, but real
compilation rejects it (`SyntaxError`), re-confirmed by `exec`-ing the probe
shape -- the shape can never run, so no registration path can mis-answer it
(probe `probe-r38-pre2.py`, runtime block, in the PR evidence).

The star-import trade-off, stated once: a non-builtin module that genuinely
re-exports a head spelling under `*` stays unregistered, because the export
list of an arbitrary module is statically unknowable; the local shadow stays
in force and the site behind it stays unreported.  `builtins` is the one
source whose public exports are known (and its star still skips underscore
names, so `__import__` cannot come back that way).  **Round 39 reversed the
first half of this trade-off** (see the round-39 section below): an unknown
star may re-export the head, so the shadow is no longer provable and the
site stays -- over-report, chosen because the miss was a false PASS.

### Closed by `fix-r1` round 39 (codex r38 C-P0-1..C-P0-7 / C-P1-1..C-P1-3; GLM r38 boundary P0-1 / P1-2, concurrency P1-1 / P1-2 / P2-3 / P2-4; sibling G-s2 / G-s3, probe 2026-11-09)

Round 38 severed the container walk at *any* rebound wrapper spelling.  A
**passthrough** wrapper -- `iter = lambda hs: (h for h in hs)` -- hands the
very handles back, so severing hid a real write (false PASS, C-P0-1); the
guard also asked only the innermost scope, so a harmless module-scope
wrapper severed the walk for the function scope below it (false FAIL,
C-P1-3).  `_wrapper_fresh` now walks the scope chain and severs only on a
unique, certainly-executed **lambda whose body never reads one of its own
parameters** -- the one shape that provably cannot pass the container
through.  A match subject that is a plain **name** cleared an imported
helper through a value the resolver never made (`held = _sa if True else
None; match held: case _sa:`, C-P0-3b); `set()` subjects read certain
through `ast.literal_eval` accepting the call syntax (C-P0-3a) --
`_capture_subject_resolvable` now accepts only a provably inert subject,
and `_provably_inert` (like `_static_truth`) rejects any `ast.Call` in the
tree, because `set()` is built with the module's own binding.  Starred
displays counted **elements** as items: `[*()]`, `(*(),)`, `{*()}`,
`{**{}}` all read provably nonempty and a walrus filter behind them stayed
certain (C-P0-4) -- `_static_nonempty` answers unknown for a display built
only from `Starred` elements / `None` keys.  An **unconsumed genexp** never
runs any part past its first iterable (PEP 289), so a walrus filter inside
it stayed certain (C-P0-5) -- the comprehension marks now defer everything
but `gen0.iter` for a `GeneratorExp`.  A `global` under an `except:`
handler or a `case:` body was dropped whole (`ast.ExceptHandler` /
`ast.match_case` are not `ast.stmt`, C-P0-6a/b).  The class span started at
`node.body[0].lineno`, but a decorator of `body[0]` sits on an earlier line
and runs in the class namespace, so its walrus fell outside the span and
read as a module binding (C-P1-1) -- the span now starts at the earliest
decorator.  A provably-true filter (`if True`) downgraded the parts behind
it (C-P1-2) -- `_static_truth` now decides.  The attrgetter factory behind
a walrus (`(_g := operator.attrgetter)(k)(md)`) left the raw `NamedExpr`
callee unread (C-P0-7), and walrus `partial` / aliased `__import__` callees
missed their kinds (G-s2) -- the decode reads the factory through
`_callee_expr`, `__import__` joined the callable kinds, and
`_import_literal` decodes the callee.  Three whole-module visits
(`_sys_modules_write_values`, `_module_import_binding_counts`,
`_module_import_sources`) and `_container_base` still recursed per child /
per link, so parser-deep expressions and subscript chains crashed the audit
(concurrency P1-2); the self-copy recursion in `_dynamic_result_in_force`
and the deep-call recursion in `_expr_kinds` crashed it too (P2-3) -- the
first three are explicit stacks now, `_container_base` a plain loop, and
the two unstackable recursions answer fail-closed (`True` / unknown) on
`RecursionError`.  `from .builtins import *` names a package sibling, not
builtins (P2-4) -- every builtins check now also requires `level == 0`.
The two `RecursionError` guards arm only the **outermost** entry (a module
global busy flag): the resolver's own recursion re-enters through the public
wrapper, and an intermediate catch let each caller re-descend into its other
sub-expressions -- one near-limit try and traceback build per level, minutes
of pure exception churn on the 2000-deep call chain.  The cross-scope half
of C-P1-3 needed two more pieces: the scope chain spells the module level
`None` while the binding and block tables key it as the tree, and a use
inside a nested scope never appears in the outer level's block map -- the
guard now reads the module level through the tree key and compares the
binding against the **statement that defines the nested scope** (a binding
certain before the `def` ran before the function object existed, hence
before any call into it; anything later keeps the descent, fail-closed).

**Display element relay (round 39, macmini r36b table survivor, 2026-11-09).**
The r36b interim negative-control table left exactly one survivor on macmini
(`calibrate-for-target-name-iter`), reproduced locally on the pre-fix r39 head
(python 3.14): `_hs = [getattr(md, k) for k in (_n,)]` then
`_items = (_hs[0],)`; `for _h in _items: pass`; `_f = _h`; `_f('common', ...)`.
The container fixpoint decoded a comprehension relay and a bare-name handover
but not a **display whose elements take handles out of a known container**,
so `_items` never entered the alias table and the iteration stayed invisible.
The fixpoint now also collects, for a tuple/list/set display, each element's
source container (a bare name registered in the table, or the container a
`_container_callee` subscript/wrapper spelling names) and carries its roots
onto the display's target -- same hand-over, same fixpoint, no new consumer.

| Shape | Self-test |
|---|---|
| **A passthrough wrapper still carries** (false PASS guard): `iter = lambda hs: (h for h in hs); next((_h := iter)(hs))('common', object())` -- the receiver must withdraw. | `... severed the walk at a passthrough wrapper (round 39)` |
| **A harmless module-scope wrapper severs** (false FAIL guard): module-level `iter = lambda hs: (h for h in (lambda *a: None,))`, used inside the function -- no withdrawal. | `... followed the walk through a harmless module-scope wrapper (round 39)` (the round-38 row `... trusted a wrapper spelling the scope had rebound (round 38)` stays green) |
| **A star import restores a shadowed helper** (false PASS guard): `from os.path import join as setattr; from builtins import *; setattr(md, 'common', object())`. | `... let a star import leave a shadowed helper in force (round 39)` |
| **A relative star is not builtins** (false PASS guard): `from .builtins import *` with an underscore head (`__import__`) shadowed -- the site stays, fail-closed. | `... read a relative star import as builtins and kept an underscore head shadowed (round 39)` |
| **An unknown star may re-export the head** (false PASS guard, supersedes the round-38 json row): `import json; globals = lambda: {...}; from json import *` -- the site stays. | `... let an unknown star import unshadow a head the source may re-export (round 39)` (the `_m38_star_json` row, message rewritten) |
| **A name / `set()` match subject stays conditional** (false PASS guard): `held = _sa if True else None` / `held = set()`, `match held: case _sa:` -- the helper must withdraw. | `... read a match capture over "<subject>" as certain (round 39)` |
| **Starred displays may be empty** (false PASS guard): `[x for x in [*()] if (globals := ...)]` (also `(*(),)`, `{*()}`, `{**{}}`) -- the site stays. | `... proved a starred display "<display>" nonempty (round 39)` |
| **An unconsumed genexp defers everything but its first iterable** (false PASS guard): `held = (x for x in (0,) if (globals := ...))` with nothing consuming it -- the site stays. | `... kept a genexp walrus certain though nothing consumes it (round 39)` |
| **A consumed genexp keeps the site** (documented over-report): the same genexp inside `list(...)` -- the site stays, because a consumer whitelist is unsound (`list` is shadowable). | `... resolved a genexp consumer it cannot prove (round 39)` |
| **A `global` under except / case registers** (false PASS guard): `class C:` with `except Exception: global globals` or `case 0: global globals` above `globals = saved`. | `... dropped a global under except-handler/match-case (round 39)` |
| **A walrus attrgetter factory takes its object** (false PASS guard): `(_g := operator.attrgetter)(_k)(md)`. | `... lost the receiver a walrus factory callee handed a dynamic attrgetter (round 39)` |
| **A walrus partial callee writes** (false PASS guard): `(_g := functools.partial)(setattr, md)`. | `... lost the receiver a walrus partial callee wrote (round 39)` |
| **An aliased walrus `__import__` binds the module kind** (false PASS guard): `from builtins import __import__ as _imp; _s = (_g := _imp)('sys')`. | `... lost the module kind an aliased walrus __import__ binds (round 39)` |
| **Deep expressions / chains do not crash** (crash guards): a 1100-term sum through the three whole-module visits; a 2000-deep subscript chain and a 2000-deep call chain through the decoders; a 1100-step self-copy chain through the head resolver. | `... let a deep expression crash a whole-module visit (round 39)` / `... let a deep subscript/call chain crash the decoders (round 39)` / `... let a self-copy chain crash the dynamic resolver (round 39)` |
| **A decorator walrus is class-local** (false FAIL guard): `class C:` with `@((globals := saved) and (lambda f: f))` above `def f(): pass` -- the module shadow stays. | `... swallowed a decorator walrus into the class span (round 39)` |
| **A provably true filter never downgrades** (false FAIL guard): `[(globals := ...) for x in (0,) if True]` -- the walrus is certain. | `... downgraded a walrus behind a provably true filter (round 39)` |
| **A display of taken-out handles still relays** (false PASS guard): `hs = [getattr(md, k) for k in (n,)]`, `items = (hs[0],)` (also `[hs[0]]`, `(hs,)`, `items[0]` after the loop), `for f in items: ...`, `f('common', object())` -- the receiver must withdraw. | `... lost a handle relayed through a display before the iteration (round 39)` |
| **A call-shaped display cannot prove a head shadow** (false PASS guard): `set = lambda: __import__; __import__ = set(); s = __import__('sys')` -- `ast.literal_eval` parses the `set()` syntax but the runtime builds it with the module's own binding, so the rebind hands back the true head and the site stays. | `... let a call-shaped display prove a head shadow (round 39)` |

Two recursion families are unreachable in practice and stay recursive by
design: deep **tuple / del-target** nesting, because the CPython parser caps
parenthesis nesting first (measured: >200 parses, 250 raises `SyntaxError`;
probe `probe-r39-pre.py` in the PR evidence), and the deep `list(zN)` copy
chain, which did not reproduce at 600/700/900/1100 copies on either head
(GLM concurrency r38 #3 -- no fix without a reproducer).

### Closed by `fix-r1` round 40 (codex r39 C-P0-1..C-P0-8 / C-P1-1..C-P1-3; GLM r39 boundary P0-1 / P0-2 / P1; sibling P1 / P2-2 / P2-3 / P2-4; probe 2026-11-09)

The round-39 wrapper sever asked only whether the lambda's body read its own
parameters, so a wrapper **closing over the container** (`iter = lambda x: hs`)
severed the walk while passing the handles through (C-P0-1 / GLM P0-1), and a
**parameter anywhere in the use chain** shadowing the wrapper binding imported
the module scope's harmlessness into the function (GLM P0-2).  The sever test is
now a free-variable analysis -- loads minus the names the body itself binds
(targets, walrus, nested parameters via `_shadowed_parameters`) -- with a
chain-level pre-check that any shadowed parameter on the way down keeps the
walk.  The round-39 display relay carried the container's roots onto the
display's target but no further: a **plain alias or a `list()` copy taken after
the display** (`first = (hs[0],); items = first`) lost the handle (C-P0-2 /
C-P0-2b), a display whose elements came from a **comprehension relay** swallowed
a later element selection (C-P0-3), and a **nested** display decoded only one
level (C-P0-4).  Two r39 regressions are pinned as false-FAIL guards: a display
built after the container was **certainly overwritten** carried the stale roots
(C-P1-1), and a **clean element** of a display inherited its sibling's handle
(C-P1-2) -- `_display_selection_roots` resolves the latest certain binding and
the constant-subscript selection path (outer-first slices applied reversed,
constant-key dict lookup, int index incl. negative) before a call off a display
subscript counts; local scope only, an outer-scope display selection stays
fail-closed (declared trade-off).  A **dict display** relays both its keys and
its values (GLM boundary P1).  An unknown-module **star import restores a
tracked alias** it re-exports (C-P0-5) -- the four event tables append a
restore event for any tracked name with an earlier helper event, and the
module alias table its own import-style entry.

The import family: an aliased `__import__` **re-bound to the bare head**
replaced the alias' kind set instead of unioning it (C-P0-6) -- kinds now
accumulate across the module's bindings; the module name rides the `name=`
**keyword** and the first positional slot (C-P0-7); and a
*certain* rebind to a **private-dict lambda** kept the import kind forever
(C-P1-3, the r39 regression) -- `_import_refused_calls` builds a dedicated
module-scope event table (imports of the builtin itself record `None`;
assignments record their value node; an unknown-module star before the call
keeps every name live) and `_import_refused_calls` refuses the decode when
the latest certain binding plus the later conditional / loop-back candidates
are all provably not the builtin (a lambda or `def` that reads no
import-capable name).  The callee kind computation rode every partial /
attrgetter branch of `_expr_kinds_inner`, making a chained `f()()()...` cost
2^(n+1)-1 calls (C-P0-8) -- the callee's kinds are computed once and reused.
The receiver write walk recursed per child, so a **left-deep expression in a
tracked scope** (1100-term sum) crashed the audit (GLM sibling P1, crash
family) -- the walk is an explicit stack now, and the flattening renamed the
inner `_handle_receivers` result so it can no longer shadow the receiver set
the root lookups intersect with (a `set & NoneType` crash the battery caught).
The mapping-method and getattr-spelling scans read their callee through the
**walrus unwrap** (GLM sibling P2-2 / P2-3), and a *relative*
`from .sys import exit` is not `sys.exit` (P2-4).  C-P1-4 (the
`_provably_inert` Call rejection over-reporting clean call displays) is the
declared r39 fail-closed trade-off and is unchanged.

| Shape | Self-test |
|---|---|
| **A wrapper closing over the container still carries** (false PASS guard): `iter = lambda x: _hs; _h = next(iter(_hs))` -- the receiver must withdraw. | `... severed the walk at a wrapper closing over the container (round 40)` |
| **A chain parameter shadow hides nothing** (false PASS guard): module `iter = lambda x: 1`, `def _f(report, iter)` -- the receiver must withdraw. | `... let a chain parameter shadow hide a real wrapper (round 40)` |
| **An alias after a display still relays** (false PASS guard): `first = (hs[0],); items = first` (also `items = list(first)`) then iterate and call -- withdraw. | `... dropped a handle alias taken after a display (round 40)` / `... dropped a list() copy of a taken-out handle (round 40)` |
| **A comprehension relay does not swallow a display selection** (false PASS guard): `items = ([h for h in _mx], hs[0]); items[1]('common', ...)` -- the selected element's receiver withdraws. | `... let a comprehension relay swallow a display element selection (round 40)` |
| **A nested display decodes all the way** (false PASS guard): `items = ((hs[0],),); items[0][0]('common', ...)` -- withdraw. | `... stopped a nested display decode at the first level (round 40)` |
| **A stale display carries nothing** (false FAIL guard): `hs = [lambda *a: None]` written after the take-out, display then iterate and call -- no withdrawal. | `... re-propagated a stale handle through a display (round 40)` |
| **A clean element does not inherit** (false FAIL guard): `items = (lambda *a: None, hs[0]); items[0]('common', ...)` -- no withdrawal. | `... let a clean display element inherit its sibling handle (round 40)` |
| **A dict display relays keys and values** (false PASS guard): `items = {hs[0]: 1}` iterated, or `items = {'k': hs[0]}; items['k']('common', ...)` -- withdraw. | `... ignored a dict display key/value (round 40)` |
| **A star import restores a tracked alias** (false PASS guard): `from builtins import setattr as _sa; _sa = lambda *a: None; from exported import *` -- the write must withdraw. | `... kept a shadowed helper in force behind a star import (round 40)` |
| **A rebind to the bare head unions kinds** (false PASS guard): `from builtins import setattr as imp; imp = __import__; s = imp('sys')` -- the module write decodes. | `... let a kind set replace, not union, on an aliased import rebind (round 40)` |
| **The module name rides `name=`** (false PASS guard): `s = imp(name='sys')` -- decodes. | `... missed the module name in an import keyword argument (round 40)` |
| **A certain private-dict rebind is refused** (false FAIL guard): `imp = lambda name: SimpleNamespace(modules={})` -- no module write reported. | `... kept reporting an import the alias no longer performs (round 40)` |
| **The callee kind recursion stays linear** (hang guard): a 30-deep `f()()()...` chain through `_object_kinds` under a 10 s bound. | `... let the callee kind recursion go exponential (round 40)` |
| **A deep expression in a tracked scope does not crash** (crash guard): a 1100-term sum through the receiver write walk. | `... let a deep expression crash the receiver write walk (round 40)` |
| **A walrus mapping-method callee writes** (false PASS guard): `(_g := sys.modules.update)({'_guard': object()})`. | `... lost a mapping-method write behind a walrus callee (round 40)` |
| **A walrus unbound setitem callee writes** (false PASS guard): `(_g := dict.__setitem__)(sys.modules, '_guard', object())` -- only `_mapping_method_call`'s own callee unwrap carries this spelling, so the row red-proves that fix site alone. | `... lost an unbound setitem write behind a walrus callee (round 40)` |
| **A walrus getattr spelling hands the handle** (false PASS guard): `(_g := getattr)(md, '__setattr__')`. | `... lost a handle behind a walrus getattr spelling (round 40)` |
| **A relative import is not `sys.exit`** (false FAIL guard): `from .sys import exit as bail` -- no exit alias. | `... read a relative import as the sys exit alias (round 40)` |

### Closed by `fix-r1` round 41 (codex r40 C-P0-1..C-P0-12 / C-P1-1 / C-P1-2; GLM r40 boundary P0-1 / P2-2 / P2-3, concurrency P1, sibling P2; probe 2026-11-09)

The import family was rebuilt around one module-scope event walk.  The
round-40 walk still recursed per child, and its crash guard only reached an
`Assign` -- a **module-level deep expression in an `if` test or a bare
`Expr`** crashed the whole kinds computation (codex C-P0-1 / GLM concurrency
P1) -- the walk is an explicit stack now (declared as the round-40 trade-off
closure).  The walk also skipped **assignment right-hand sides, `while`
tests and `def`/`class` name events** (codex C-P0-3 / GLM boundary P0-1):
an RHS or while-test walrus that restores the builtin alias, and a rebind
through a module-scope `def` name, were all invisible -- values, tests and
def/class names are events now, and a `class` event is provably not the
import while a `def` is judged by its body.  The **capability test** read
only bare names, so the builtin behind a **tuple unpack, an `if` expression
or a boolean operator** (C-P0-2), behind a **`builtins.__import__`
delegation** or a thunk **returning the module itself** (C-P0-4) was judged
incapable and the real `sys.modules` write went invisible -- the value walk
descends containers and conditionals, attribute delegation onto a
`builtins`-kinded base counts, and a lambda/def whose returns provably name
`sys`/`builtins` counts.  A **factory call** over a def whose every return
is provably clean no longer over-reports (C-P1-2).  Star semantics moved into
`live()` proper: a star **behind the call inside a loop** is live on the
back edge (C-P0-5 / GLM boundary P2-2), a **dead** star (dead-shadowed or in
a dead branch) restores nothing, and a star **superseded by a later certain
rebind** drops out of force (C-P1-1) -- both r40 false reports the battery
pins.  The chosen binding is **position-based** now (the stack walk's event
order is not source order).

The display family: a **Starred element before the selected index** shifts
it, and a `**` merge or a **dynamic dict key** can override the selected
key (C-P0-6 / C-P0-7) -- both make the selection path give up (fail-closed,
the receiver withdraws); a **mutating call between the take-out and the
call** (`reverse`, `pop`, subscript writes; `_display_mutated_between`)
invalidates the cached selection (C-P0-8); a **comprehension element**
relays through `_relay_handle_roots` (C-P0-10) and a **dictcomp key** is a
taken root like any other (GLM sibling P2).  The wrapper sever test is a
**free-name analysis** -- `_free_name_loads` computes per-scope free names,
so a wrapper whose only free name is the container keeps the walk alive
while everything its own scopes bind stays shadowed (C-P0-9).  The
iter-literal and `__getattribute__` reads take their callee through the
walrus unwrap (GLM sibling P2), as do the status-write decoders
`_call_written_values` and `_partial_bound_arguments` -- a walrus `setattr`
or `functools.partial` callee no longer loses the `status` write
(C-P0-11 / C-P0-12).

| Shape | Self-test |
|---|---|
| **A deep module-level expression does not crash the kinds walk** (crash guard): a 1100-term sum in an `if` test and a bare `Expr`. | `... let a deep module-level expression in an if test / a bare expression crash the kinds walk (round 41)` |
| **A container or conditional value keeps the alias capable** (false PASS guard): `imp, = (__import__,)`, `imp = __import__ if True else None`, `imp = __import__ or None` -- the module write decodes. | `... refused an import alias restored through a tuple unpack / an if expression / a boolean operator (round 41)` |
| **An assignment right-hand or while-test walrus restores the alias** (false PASS guard): `_ = (imp := __import__)('sys')`, `while (imp := __import__) and False:` then `s = imp('sys')`. | `... dropped a real import behind an assignment right-hand walrus / a while-test walrus (round 41)` |
| **Delegation and module-returning thunks stay capable** (false PASS guard): `imp = lambda name: b.__import__(name)`, `imp = lambda name: sys`. | `... dropped a real import behind a builtins delegation / a module-returning thunk (round 41)` |
| **A star behind the call in a loop is live on the back edge** (false PASS guard): `while flag: s = imp('sys'); from loopstar import *`. | `... dropped a real import behind a star back edge (round 41)` |
| **A def rebind, a dead star, a superseded star and a factory do not report** (false FAIL guard): `def imp(name): return None`; `if False: from exported import *`; `from exported import *` then `imp = lambda`; `imp = _factory()` over clean returns -- no module write. | `... kept reporting an import behind a module-scope def rebind / a dead star / a star superseded by a certain rebind / a factory over a clean lambda (round 41)` |
| **A starred shift moves the selected index** (false PASS guard): `items = (*hs, lambda *a: None); items[1](...)` -- withdraw. | `... lost a receiver behind a starred shift of the selected index (round 41)` |
| **A `**` merge or dynamic key overrides the selection** (false PASS guard): `items = {'k': lambda, **{'k': hs[0]}}`, `items = {'k': lambda, k2: hs[0]}` -- withdraw. | `... lost a receiver behind a dict merge override / a dynamic key override (round 41)` |
| **A mutation between take-out and call invalidates the selection** (false PASS guard): `items.reverse()` / `items.pop(0)` then `items[0](...)` -- withdraw.  (The no-argument `pop()` spelling was the round-41 row's own error: it removes the *last* element and cannot move a leading selection -- the round-42 battery carries it as a clean twin.) | `... lost a receiver behind a reverse mutation / a pop mutation between take-out and call (round 41)` |
| **A comprehension element and a dictcomp key relay** (false PASS guard): `items = ([h for h in hs],); items[0][0](...)`; `d = {h: None for h in hs}` iterated -- withdraw. | `... lost a receiver behind a comprehension display element / a dictcomp key relay (round 41)` |
| **A walrus iter or `__getattribute__` callee hands the receiver** (false PASS guard): `for h in (_i := iter)((md,)): h.common = x`; `(_g := md.__getattribute__)('__dict__')`. | `... lost a receiver behind a walrus iter callee / a walrus getattribute callee (round 41)` |
| **A wrapper whose only free name is the container still carries** (false PASS guard): `iter = lambda x: ((lambda x: None), (h for h in x))[1]` -- the receiver must withdraw. | `... severed the walk at a wrapper whose only free name is the container (round 41)` |
| **A walrus setattr or partial callee writes the status** (false PASS guard): `(_s := setattr)(r, 'status', 'PASS')`; `(_p := functools.partial)(setattr, r, 'status')('PASS')` -- the write decodes. | `... lost a status write behind a walrus setattr callee / a walrus partial callee (round 41)` |

### Closed by `fix-r1` round 42 (codex r41 C-P0-1..C-P0-9 / C-P1-1..C-P1-5; GLM r41 boundary P0-1 / P1#2, sibling P2#2 / P2#3; probe 2026-11-10)

Every finding was probe-reproduced against the round-41 ruler and the r40
baseline before any fix (probe log: `_tmp/20261109-issue-1953-revival/probe-r42-postfix.log`
in the main repo; the "r41" column there is the round-41 commit, the "r42"
column the fixed ruler).  Two reviewer claims were refuted by measurement and
closed without code: the in-loop direct back-edge mutation (GLM boundary
P0-1's primary shape -- a `for` statement's own subtree containment already
catches it; the real hole was the closure variant) and the deep-tuple parse
crash (the parser caps right-nested tuples far below the recursion limit on
Python 3.14) -- the iterative rewrite still subsumes both defensively.

The import family.  A **`class` judged by its `__new__`** (codex C-P0-1):
`class imp: def __new__(cls, name): return sys` hands the module back and
the round-41 `ClassDef` event proved nothing -- the capability test now walks
the `__new__` body (decorator/metaclass keeps the conservative capable
answer).  A **factory rebound after its `def`** (C-P0-2): a decorator, an
import alias (`from functools import partial as factory`) and a `global`
rebind each left the round-41 `factory_returns` resolving a stale def --
`factory_returns` now refuses decorated defs and any name rebound anywhere in
the module (`_name_rebound_beyond`: `global`, import aliases including `*`,
same-named def/class, module-scope for/with targets and except names).  A
**call whose only carrier is the `while` test** (C-P0-3): `_flow_marks`
gives a while-test *expression* the outer loops, so the test call lost the
loop context the r41 back-edge rule needed -- the event and call walks now
derive loop context from their own walk (test/body inner, orelse and the
one-shot for-iterable outer), marks only contribute conditionality.  A
**walrus in a def default or a class base** (C-P0-4): headers evaluate at
module scope and never entered the event table -- they do now.  A **starred
unpack** (`imp, = (*(__import__,),)`, C-P0-5a) and a **lambda body that is
a walrus or an if-expression over a known module** (C-P0-5b/c) complete the
capable-value bins; the capability evaluation itself is a **worklist** now
(C-P0-9a, GLM boundary P1#2): a 600-deep factory chain, a 1100-deep
lambda chain (`_free_name_loads`, C-P0-9c) and a 1100-deep if-expression
each crashed the whole audit on clean files, and a non-node operand (the
`None` in `__import__ or None`) fell through the worklist and decided for
the capable name queued behind it.  The false-report side: a **tuple
unpack onto another name**, an **if-expression / boolean operator onto a
clean lambda** (C-P1-1), an **uncalled lambda's walrus** (C-P1-2) and the
**loop-boundary star shapes** (C-P1-3 -- the else-clause star after an
in-loop call, the body star after a for-iterable call) no longer report.

The display family.  The wrapper sever test dropped the **first
generator's free name** (`[h for h in h]` reads its iterable before binding
-- C-P0-6a) and **lambda defaults** now keep the outer shadow (C-P0-6b);
`_free_name_loads` is a worklist.  `_display_mutated_between` grew the
mutation classes the window missed (C-P0-7, GLM boundary P0-1): **closure
mutations** (a nested def or lambda containing a display-mutating call,
convicted only when a call to it sits in the window), **alias mutations**
(`_alias = _items; _alias.reverse()` -- an alias fixpoint over whole-scope
name assignments), **unbound `list.reverse(_items)`** and **for-target
subscript writes** (`for _items[0] in (...)`).  Precision on `pop`: a
no-argument `pop()` removes the *last* element and cannot move a leading
selection, a literal `pop(k)` is priced against the leading index (C-P1-4;
the round-41 battery row itself carried the wrong spelling -- now
`pop(0)`, with the `pop()` clean twin).  `_relay_handle_roots` now checks
`_carries_at` before reusing a handle-container history bucket, so a
comprehension element or dictcomp key relayed from a container rebound to
clean lambdas no longer withdraws (C-P1-5).

The callee sites.  Eight scan/decoder sites read their callee through
`_callee_expr` (C-P0-8): `_callee_names`, the `_nested_def_exits_zero`
guard, `_delegated_exit`, the `_exit_target` caller, `_proven_callee`,
`_unproven_expansion` (which also crashed on a walrus callee),
`_unproven_update_on_summary` and the inline `update`/`setdefault`
status check -- `(_u := report.update)(payload)` and `(_e := sys.exit)(0)`
keep their markers.  `_dict_binding_keys` unwraps the callee too and
counts `pop`/`popitem`/`clear`/`__setitem__`/`__delitem__` (GLM sibling
P2#3): a walrus `update` or `__setitem__` callee no longer keeps a dict's
keys bounded.

Known residuals (follow-up issue, none reviewer-flagged as unfixed):
the **positional star rule** keeps reporting the three else-clause /
for-iterable shapes' *trailing* call (C-P1-3's readback) -- a star the call
can still see by position conservatively re-exports any name, and the
runtime export list is not statically knowable; a **closure defined after
the binding and never called** still withdraws (pre-round-42 behaviour,
conservative direction); and a **module-scope for/with target** rebinding
a factory name registers as a factory-resolution blocker but not as a
live() event.  The round-41 negative-control driver's interlock note
("no strict subset of the import family's hunks reverses into runnable
code") was overstrong (codex r41 C-P2-1): the builtins-delegation hunk
reverts independently -- the round-42 driver splits that family so each
subset carries its own red proof.

| Shape | Self-test |
|---|---|
| **A class `__new__` returning the module keeps the alias capable** (false PASS guard): `class imp: def __new__(cls, name): return sys`. | `... dropped a real import behind a class __new__ returning the module (round 42)` |
| **A factory rebound after its def keeps resolving** (false PASS guard): decorated, import-aliased and `global`-rebound factories over `factory(__import__)`. | `... dropped a real import behind a decorated factory / an import-aliased factory rebind / a global factory rebind (round 42)` |
| **A while-test call carrier keeps its loop context** (false PASS guard): `while (s := imp('sys')) and (imp := __import__): ...`. | `... dropped a real import behind a while-test call carrier (round 42)` |
| **A walrus in a def default or class base is a module-scope event** (false PASS guard): `def _g(x=(imp := __import__))`, `class _C(*((imp := __import__) and ()))`. | `... dropped a real import behind a walrus in a def default / a walrus in a class base (round 42)` |
| **A starred unpack, a lambda-body walrus / if-expression and an `or` over a literal keep the alias capable** (false PASS guard): `imp, = (*(__import__,),)`; `imp = lambda name: (m := sys)`; `imp = lambda name: sys if True else None`; `imp = __import__ or None`; `imp = __import__ if True else None`. | `... dropped a real import behind a starred unpack of the builtin / a lambda body walrus over the module / a lambda body if-expression over the module / an or whose right operand is a literal / an if-expression whose true arm is the builtin (round 42)` |
| **A tuple unpack onto another name, a decided arm onto a clean lambda and an uncalled walrus do not report** (false FAIL guard): `spare, imp = (__import__, clean)`; `imp = clean if True else __import__`; `imp = clean or __import__`; `f = lambda: (imp := __import__)`. | `... kept reporting an import behind a tuple unpack onto another name / an if-expression onto a clean lambda / a boolean operator onto a clean lambda / a walrus inside an uncalled lambda (round 42)` |
| **A loop-boundary star does not leak into the loop** (false FAIL guard): an in-loop call under a for/else or while/else star, a for-iterable call under a body star -- the in-loop call stays refused. | `... kept an in-loop call live behind a for/else star after an in-loop call / a while/else star after an in-loop call / a body star after a for-iterable call (round 42)` |
| **A first-generator free name and a lambda-default capture keep the wrapper** (false PASS guard): `iter = lambda x: (h for h in x)`; `iter = lambda x: (lambda x=x: (h for h in x))()`. | `... lost a receiver behind a first-generator free name / a lambda-default capture wrapper (round 42)` |
| **Closure, alias, unbound and for-target mutations invalidate the selection** (false PASS guard): `def _mutate(): items.reverse()` called in the window; `alias = items; alias.reverse()`; `list.reverse(items)`; `for items[0] in (...)`. | `... lost a receiver behind a closure mutation defined before the binding / an alias mutation / an unbound list.reverse / a for-target subscript write (round 42)` |
| **A `pop()` of the last element and relays from clean-rebound containers do not withdraw** (false FAIL guard): `items.pop()`; a comprehension element / dictcomp key over `_hs` rebound to clean lambdas. | `... withdrew a clean receiver behind a pop of the last element / a comprehension element after a clean rebind / a dictcomp key after a clean rebind (round 42)` |
| **A walrus `report.update` / `sys.exit` callee keeps its marker** (false PASS guard): `(_u := report.update)(payload)`; `(_e := sys.exit)(0)`. | `... lost the write-status marker behind a walrus report.update callee / the exit-call-status marker behind a walrus sys.exit callee (round 42)` |
| **A walrus update callee with an unproven payload is still an inline status write** (false PASS guard): `(_u := report.update)(payload)` inside a registered point, judged by the same `_unproven_expansion` arm as the plain spelling -- the negctl round-42 driver reverts the `unshared_status_slots` callee hunk alone, and the generic call branch (which already unwraps the callee since round 41) still reads a clean dict literal but not an unproven name, so the probe carries an unproven parameter. | `... did not flag an unproven update payload behind a walrus callee (round 42)` |
| **A walrus `update` / `__setitem__` callee unbounds dict keys** (false PASS guard): `(_u := d.update)({'common': 1})`; `(_u := d.__setitem__)('common', 1)`. | `... kept dict keys bounded behind a walrus update callee / __setitem__ callee (round 42)` |
| **Deep factory chains, lambda chains and if-expressions do not crash** (crash guard): a 600-deep factory chain (also clean -- no import reported), a 1100-deep lambda chain, a 1100-deep if-expression. | `... let a deep factory chain crash the import walk / let a deep factory chain report an import / let a deep lambda chain crash the free-name walk / let a deep if-expression crash the capability test (round 42)` |

### Closed by `fix-r1` round 43 (GLM r42 boundary#1..#3 / sibling#1..#6; anthropic r42 boundary#1; probe 2026-11-10)

Every finding was probe-reproduced against the round-42 ruler before any fix
(probe logs: `_tmp/20261109-issue-1953-revival/` -- `probe-r43-postfix.log`
carries the pre/post table, `probe-antb1.log` the nested-scope family,
`probe-s3.py` the unpack-projection discriminator; the "r41" column is the
round-41 commit kept as a second witness, the current column the fixed
ruler).  One P0 claim was refuted by measurement and closed without that
half of the fix: GLM boundary#1 asserted the `pop`/`insert` pricing reads a
*raw negative* `leading` constant and under-prices (false PASS) -- but
`ast.parse` never yields `Constant(-1)` for `x[-1]`; the slice is
`UnaryOp(USub, Constant(1))`, which every `isinstance(slice, ast.Constant)`
check already declined, so the pre-fix ruler was blanket-conservative
(probe: every negative-selection shape read R=True, no false PASS possible).
The real defect was the mirror -- **false reports on every clean
negative-selection display** (`items[-1]` with no mutation, `pop(0)` or
`insert(0, ...)` before it) -- now fixed by pricing the trailing selection
against its end anchor.

The display family.  A negative literal subscript is now read as a
**trailing selection** anchored at `literal_len - j`: a removal at index
`k` moves it iff `k >= anchor`, an insertion iff `k > anchor` (GLM
boundary#1, probe b1a/b1b/b1e/b1f/b1g).  `mutates_display` prices
`pop()` / `pop(k)` / `insert(k, x)` / `append` / `extend` (the last two
never move a positive leading selection) and normalizes negative `k`
(GLM sibling#4a); a whole-target literal `del items[k]` takes the same
pricing (sibling#4c).  The **mutators alias fixpoint** (GLM boundary#2)
collects `alias = mutator` names so `_m = _mutate; _m()` convicts.  The
**`__new__` inheritance** family (GLM boundary#3): a base-resolvable class
is judged through its parent's own `__new__` (`class_node` resolver inside
`_import_refused_calls`, single-event/not-rebound guards mirroring
`factory_returns`, cycle-guarded) -- an inherited module-returning builder
(one or two levels) and an assigned `__new__ = staticmethod(<builder>)`
spelling are the import, an inherited *clean* builder stays clean, and an
unresolvable or rebound base keeps the conservative standing.

The context and callee family.  The **`while` test carries the back edge**
in both loop-context helpers (GLM sibling#1): `_flow_marks` and
`_module_call_positions` give the test subtree the loop's id (it
re-evaluates after every body iteration), while the `for` iterable, the
`else` blocks and the one-shot positions keep the outer loops.  The
receiver scan unwraps the callee (sibling#2: `(_g :=
md.common.aggregate_status)(report)` keeps its receiver).  The write-helper
event tables **project tuple unpacks per name** through
`_project_unpack_targets` at all three sites (sibling#3, probe
`probe-s3.py`: `spare, w = (clean, setattr)` with a prior import alias
dropped the write-status marker).  `_dict_binding_keys` follows **aliased
and unbound receiver spellings** (`e = d; e.update(...)`,
`dict.update(d, ...)`, `dict.__setitem__(d, ...)`, `operator.setitem`,
sibling#5).  The exit-raise and return-int marker checks unwrap their
callee (sibling#6; anthropic sibling#1: `raise (_e := SystemExit)(0)`,
`return (_i := int)('PASS')`).  `_name_rebound_beyond` no longer pierces
nested scopes (anthropic boundary#1): a nested-only
`from x import factory` / `def factory` / `factory += 1` leaves the
module-level name un-rebound (only `global` keeps the whole-tree read), so
a legitimate factory keeps its refused-call standing; the same fixpoint
collects `AugAssign` targets.

Known residuals (follow-up issue): `remove` / `popitem` / `dict.update`
mutations are still un-priced (conservative withdraw, GLM sibling#4 tail);
a `pop()` or `del` that removes the *selected* element itself still
withdraws -- the trailing anchor prices `k == anchor` as a move because
the selection then reads a neighbour the ruler does not re-check (the
b1d/b1i conservative-mirror pair, false-report direction); the mutators
`ast.walk` in `_display_mutated_between` still pierces nested scopes
(anthropic P3#2, conservative direction); the unbound-branch type list in
`mutates_display` is a hardcoded builtin set (GLM sibling#7, P3); the
battery's `imp`-call filter reads its callee through `_callee_expr`
(anthropic P3#3, fixed in passing); and the module_copies source of the
event tables keeps the round-26 plain-alias reading (deliberately not
extended to projected unpack elements).

| Shape | Self-test |
|---|---|
| **An inherited `__new__` returning the module keeps the import standing** (false PASS guard): `class _B: def __new__(cls, name): return sys` then `class imp(_B)`, one or two levels deep; `__new__ = staticmethod(lambda cls, name: sys)`. | `... dropped the import standing behind an inherited __new__ returning the module / a two-level inherited __new__ returning the module / an assigned __new__ builder (round 43)` |
| **An unresolvable or rebound base keeps the conservative standing** (floor lock): `class imp(_B)` over an imported `_B`; `_B = dict` after the class. | `... dropped the import standing behind an unresolvable imported base / a base name rebound after its class (round 43)` |
| **An inherited clean `__new__`, a base cycle and a nested-only factory import do not report** (false FAIL guard): `class _B` returning `SimpleNamespace(modules={})`; `class imp(_A) / class _A(_B) / class _B(_A)` (the guard must terminate); a helper containing `from functools import partial as factory` over a clean factory. | `... kept reporting an import behind an inherited clean __new__ / a self-referential base cycle / a nested-only import of the factory name (round 43)` |
| **Tail mutations move a trailing selection** (false PASS guard): `pop()` / `del items[2]` / `insert(3, hs[0])` then `items[-1](...)` -- withdraw. | `... lost a receiver behind a pop() moving the handle into a trailing selection / an insert past the tail into a trailing selection / a del moving the handle into a trailing selection (round 43)` |
| **An alias call of a mutating closure convicts** (false PASS guard): `_m = _mutate; _m()` with `_mutate` reversing the display. | `... lost a receiver behind an alias call of a mutating closure (round 43)` |
| **Head mutations and no-op aliases do not withdraw** (false FAIL guard): `pop(0)` / `insert(0, None)` before `items[-1]`; a bare `items[-1]`; `pop(-1)` removing the handle before `items[0]`; `append(None)` / `extend([None])` / `del items[1]` before `items[0]`; `_m = _noop; _m()`. | `... withdrew a clean receiver behind a head pop before a trailing selection / a head insert before a trailing selection / a bare trailing selection / a negative-argument pop of the handle / an append after a leading selection / an extend after a leading selection / a del of the handle before a leading selection / an alias call of a no-op closure (round 43)` |
| **The `while` test carries the loop context, the `else` block does not** (structural guard, `_flow_marks`): the test call subtree and walrus, and the body, carry the while id; the else block keeps the outer loops. | `... lost / leaked the while loop context of the while-test call subtree / the while-test walrus / the while body / the while else block (round 43)` |
| **The module call positions carry the same loop semantics** (structural guard, `_module_call_positions`): while-test and body calls carry the while id; for-iterable, for-else and while-else calls keep the outer loops. | `... lost / leaked the loop context of the _w43t / _w43b / _w43e / _f43i / _f43b / _f43e call (round 43)` |
| **A walrus callee on a stub receiver keeps the receiver** (false PASS guard): `(_g := md.common.aggregate_status)(report)` over a module-scope stub `md`. | `... lost a receiver behind a walrus callee on a module-scope stub receiver (round 43)` |
| **An unpack rebinding onto the write helper keeps the marker, onto a clean callee does not grow one** (both directions, GLM sibling#3): `spare, w = (clean, setattr)` then `w(report, 'status', 'PASS')`; the mirror `spare, w = (setattr, clean)`. | `... lost the write-status marker behind an unpack rebinding onto the write helper / kept the write-status marker behind an unpack rebinding onto a clean callee (round 43)` |
| **Aliased and unbound dict receivers unbind the keys** (false PASS guard): `e = d; e.update(...)`; `dict.update(d, ...)`; `dict.__setitem__(d, ...)`. | `... kept dict keys bounded behind an aliased receiver update / an unbound dict.update receiver / an unbound dict.__setitem__ receiver (round 43)` |
| **Walrus exit-raise and return-int callees keep their markers** (false PASS guard): `raise (_e := SystemExit)(0)`; `return (_i := int)('PASS')`. | `... lost the raise-exit-status marker behind a walrus exit callee / the return-int-status marker behind a walrus int callee (round 43)` |

### Closed by `fix-r1` round 44 (GLM r43 boundary#1..#5 / sibling#1..#3 / sibling#6 tail; anthropic r43 sibling#1/#2; probe 2026-11-10)

Every finding was probe-reproduced against the round-43 ruler before any fix
(`_tmp/20261109-issue-1953-revival/`, `probe-r44.log` pre-fix /
`probe-r44-after.log` post-fix; the "r42" column is the round-42 commit kept
as a second witness, the "r43" column the fixed ruler).  One P3 claim was
refuted by measurement and closed without a fix: anthropic boundary#1
asserted the ClassDef branch of `_import_alias_events` is dead code -- but
the events table records the ClassDef target (probe ant-b1), so
`class_node(_B)` resolves.

The composite-pricing family (GLM boundary#1, P0).  Each priced mutation
read the ORIGINAL literal length, so two individually-safe events composite
into a moving one: `[_hs[0], L, L]` with a trailing selection (`pop(1)` and
`pop(1)` each price "not moving" against the original anchor) removes the
selected element at runtime.  The selection is now **simulated over the
priced event stream**: events are collected in source order and replayed
against the current length (`ln`), the selected element index (`pos`) is
tracked (`k == pos` convicts, `k < pos` shifts `pos`, insertions `p <= pos`
shift it up), negative indices normalize against the current `ln`, and the
final verdict compares `pos` with the current trailing anchor / leading
position.  Single-event verdicts are reproduced exactly (the invariant
`pos == ln - trailing` holds across every non-convicting event).  A
**loop the binding does not run in** re-executes an event without
re-running the binding (`_repeated_between`), so a carried priced event
convicts -- fail-closed: the loop that runs it only once reports too (the
`for _ in (0,):` clean twin withdraws; anything not statically priceable
stays a mutation, the standing contract).  The **mutator collection** now
takes any mutating call in a def/lambda body (`price=False`), so a
loop-carried or call-count-unknown mutator closes conservatively.

The alias-closure family.  `_name_alias_closure(scope, seeds)` is the one
shared fixpoint for display names, mutators and dict receivers (GLM
boundary#2/#3): it reads every binding spelling -- `AnnAssign`, a bare
walrus anywhere in an expression, multi-target `a = b = x`, and unpack
projection through `_local_bound_values` -- and deliberately skips
`AugAssign` (`f += d` is not an alias).  It replaces the three
single-target-Assign-only fixpoints.

The rebind family (GLM boundary#4).  `_name_rebound_beyond` now reads
module-level `match` captures (`MatchAs` / `MatchStar` names,
`MatchMapping.rest`) and a bare `del name` (ast.Delete Name targets), so a
captured or deleted base keeps the conservative import standing.

The walk-sync family.  `_module_open_calls` (the third loop-context walk,
GLM sibling#1) and `_name_bindings` (the fourth, GLM sibling#2) now carry
the canonical For/While semantics round 43 gave `_flow_marks` and
`_module_call_positions`: the `for` iterable and the `else` blocks stay
outer, the bodies and the `while` test carry the loop id.  The old blanket
`inner` kept a phantom open-call site behind a loop-body shadow (probe s1)
and let a for-else binding share the loop id with the body use (probe s2).

The exit-marker family (anthropic sibling#1/#2).  `_exit_code_shaped` and
`_inline_exit_call` read their callee through the shared `_callee_expr`
walrus unwrap: a ternary arm `(_i := int)('PASS') if c else 0` keeps the
exit shape, and the relay `code = (_i := int)('PASS')` keeps the `int` /
`bool` spelling.  The battery's `_m43_positions` presence check (GLM
boundary#5) makes a dropped call fail its own assertion instead of
satisfying the `want=None` shape.  The `__new__` Assign check reads the
class body's direct statements only (GLM sibling#3 tail): a method-local
`__new__ = ...` is that method's own local name, not the class builder.
Three inline negative-int normalizers now call the shared `_constant_int`
(GLM sibling#3).  The For-only loop contexts of `_sys_modules_write_values`
and `_receiver_attribute_writes` carry an explicit exemption comment
(GLM sibling#6): their only consumer is For-target key resolution, which
stays conservative without the While/async branches.

Known residuals (follow-up issue): the `remove` / `popitem` /
`dict.update` pricing, the b1d/b1i conservative mirror, the mutators
`ast.walk` piercing and the hardcoded builtin type list carry over from
round 43; the loop-carried clean twin is a deliberate false-report
trade-off documented above; the width guard is `O(chain length x tree)`
(the same order as the fixpoint it replaced); the hardcoded
mutating-attribute tuples and the bare-walrus raise-exit spelling are
unchanged; the fifth/sixth walks stay For-only by the recorded exemption.

| Shape | Self-test |
|---|---|
| **Composite mutations move a selection** (false PASS guard): `pop(1); pop(1)`, `pop(1); insert(2, hs[0])`, `del items[1]; del items[1]`, and a loop-carried `for _ in (0, 1): pop(1)` before `items[-1]` -- withdraw. | `... lost a receiver behind a pop+pop composite removing the selected element / a pop+insert composite crossing the anchor / a del+del composite through the Delete spelling / a loop-carried pop executed twice (round 44)` |
| **A loop-carried pop executed once still withdraws** (fail-closed floor lock): `for _ in (0,): pop(1)` before `items[-1]`. | `... lost a receiver behind a loop-carried pop executed once (fail-closed standing) (round 44)` |
| **Net-zero composites do not withdraw** (false FAIL guard): `pop(1); insert(1, None)` and `pop(0); insert(0, None)` before `items[-1]`. | `... withdrew a clean receiver behind a net-zero pop+insert composite / a before-anchor pop(0)+insert(0) composite (round 44)` |
| **Every alias spelling of a dict receiver unbinds the keys** (false PASS guard): `e: dict = d`, `(_e := d)`, `e = f = d`, `e, x = (d, 1)` with a write through the alias. | `... kept dict keys bounded behind an AnnAssign alias receiver write / a walrus alias receiver write / a multi-target alias receiver write / an unpack alias receiver write (round 44)` |
| **A writeless alias keeps the keys bounded** (false FAIL guard): `e: dict = d` with no write through it. | `... withdrew dict keys behind a writeless alias (round 44)` |
| **Every alias spelling of a mutating closure convicts** (false PASS guard): `(_m := _mutate)`, `_m: object = _mutate`, `_m = _n2 = _mutate`, `_m, _x = (_mutate, 1)` then `_m()`; and a walrus alias of the display itself `(_n2 := _items); _n2.reverse()`. | `... lost a receiver behind a walrus alias call of a mutating closure / an AnnAssign alias call of a mutating closure / a multi-target alias call of a mutating closure / an unpack alias call of a mutating closure / a walrus alias of the mutated display itself (round 44)` |
| **Match captures and del rebind the base name** (floor lock): `case [_B]`, `case {'k': _B}`, `case [*_B]`, `case {'k': _v, **_B}` over a clean `_B` with a `_Bad` subject; `del _B` used or re-imported. | `... dropped the import standing behind a match value capture rebinding the base / a match mapping capture rebinding the base / a match star capture rebinding the base (fail-closed) / a match mapping-rest capture rebinding the base (fail-closed) / a del rebind used without a re-import (fail-closed) / a del rebind followed by a re-import (round 44)` |
| **An unrelated match capture does not report** (false FAIL guard): `case [_other]` over a clean `_B`. | `... kept reporting an import behind an unrelated match capture (round 44)` |
| **A method-local `__new__` assign is not the class builder; the direct class-body assign is** (both directions): `def _m(self): __new__ = staticmethod(...)` vs a direct `__new__ = staticmethod(...)` in the class body. | `... kept the import standing behind a method-local __new__ assign / dropped the import standing behind a direct class-body __new__ assign (round 44)` |
| **The third walk carries the canonical loop semantics** (structural guard, `_module_open_calls`): for-iter and both `else` calls stay outer; the for body, the while test and the while body carry the loop id. | `... lost the open-call site at (line, col) entirely / read the open-call site at (line, col) with the wrong loop context (round 44)` |
| **The fourth walk carries the same semantics** (structural guard, `_name_bindings`): a for-else binding shares no loop id with the body call; the body call keeps the loop id. | `... leaked the loop id onto a for-else binding / lost the loop id of a for body call (round 44)` |
| **Walrus exit callees keep shape and spelling** (false PASS guard): `(_i := int)('PASS') if c else 0` shaped; `code = (_i := int)('PASS')` / `code = (_i := bool)(x)` spelled; the `len` mirror stays unshaped. | `... lost the exit shape behind a walrus int callee / kept the exit shape behind a walrus len callee / lost the int spelling behind a walrus callee / lost the bool spelling behind a walrus callee (round 44)` |

### Closed by `fix-r1` round 45 (GLM r44 boundary#1 P0 / concurrency#1 / boundary#2 / sibling#1..#3; anthropic r44 sibling#1..#3 / sibling#5; probe 2026-11-10)

Every finding was probe-reproduced against the round-44 committed ruler
(`probe-r45.log`; the "r44" column there is the round-44 commit, the "r43"
column the round-43 commit kept as a second witness, the "tree" column the
fixed ruler, and every dirty row carries a runtime oracle line).  One probe
expectation was corrected by measurement and no ruler change came of it: the
clean *assigned* `__new__` builder over-reports exactly the way round 43
registered for the Assign spelling (`c-control`), so the AnnAssign extension
inherits the same conservative trade-off rather than widening the
value-analyzed spelling set.

The evaluation-order family (GLM boundary#1, P0).  The round-44 composite
replay collected events in *source* order, so `_items.insert(2,
_items.pop(0))` replayed the outer insert first and laundered the move --
at runtime the inner (argument) call runs first, and `IfExp` evaluates its
test before either arm.  The simulation now walks each statement through
`_eval_order_nodes`, an explicit-stack **post-order** traversal (children
before parents, `field`-order left-to-right) that skips nested scope *bodies*
while still descending their definition headers.  The window itself was
scoped at the same time (GLM concurrency#1): it reads only the scope's own
statements (`_scoped_nodes`), because a nested scope's body runs at its
*invocation*, not its definition -- a never-called closure's body events
were a false report (probe `f-nested-def`); an invocation still convicts
through the mutator collection, the standing fail-closed floor.

The double-count fix (found by the `d-for-else` probe).  Round 44 iterated
`ast.walk(scope)` for statements and `ast.walk(statement)` for nodes, so an
event inside a compound statement entered the replay once as part of its
enclosing compound statement and once as its own outer-loop statement --
two clean `remove(1)`-safe events composited into a conviction.  Round 43
was immune (its per-event check was idempotent).  The events list is now
deduplicated (`priced`, keyed on `id(node)`).

The else-block family (GLM sibling#2 + boundary#3; anthropic sibling#2).
`_repeated_between` now pushes For/While `orelse` children with the `carried`
flag, not `inner`: the `else` block runs once after the loop, so its single
clean event carries the outer verdict (it used to ride the loop's inner
flag and report), while a composite inside it still convicts.  The dead
`repeated.add(id(node))` for def/lambda/class statements is gone.

The alias-closure family (GLM sibling#1 + anthropic sibling#1, P1, two
lineages).  `_name_alias_closure` additionally reads a **whole-subject match
capture** (`match _items: case _n2:` -- the walrus-unwrapped Name subject
joins the seeds, so `MatchAs(pattern=None, name=...)` captures it) and a
**for/comprehension target over a literal Tuple/List whose every element is
a seed Name** (`for _n2 in (_items,):`).  The mutator *definitions* were
extended at the same time (anthropic sibling#1): the seeding reads
`_local_bound_values` (AnnAssign, multi-target, unpack projection) and the
walrus `(_g := lambda: ...)` spelling, so every binding spelling of a
mutating lambda closes.  Nested-element captures (`case [_n3]`) and
non-literal or mixed iterables stay outside the closure by design.

The `__new__` spelling family (GLM boundary#2; anthropic sibling#3).  The
class-body capability walk reads every binding spelling, not only the
single-Name `ast.Assign`: AnnAssign, tuple/multi-target, the bare walrus,
and `from m import f as __new__` all take the conservative-capable
assignment reading round 43 registered for the Assign spelling (a clean
*assigned* builder over-reports -- the registered trade-off; only the
`def __new__` spelling is judged through its own body).  The walk itself is
a direct `value.body` traversal, not `_scoped_nodes`: that generator yields
nested definition *headers* only, so the `def __new__` statement itself
would never surface (caught by the round-42/43 battery rows, which the
first draft of this fix regressed before the body walk was restored).

The walrus-callee family (GLM sibling#3; anthropic sibling#3).  Eight more
`.func` reads route through the shared `_callee_expr` walrus unwrap:
`_has_passed_aggregate`, `_loads_shared_module`, `_loop_target_used_as_handle`,
`_fresh_value`, the read-only walk, `_proven_source`, `_is_shared_call` and
`_assigns_clean_status`.  The rebind family closes its tuple spelling at
the same time (anthropic sibling#5): `del (_B, _x)` rebinds the base name
through `_target_names`, matching the round-44 bare-`del` fix.

Known residuals (follow-up issue): a callback-passed mutator (`map(_mutate,
...)`) is not an invocation for the mutator collection; nested-element
match captures and non-literal for iterables stay outside the alias
closure (the battery pins both); comprehension later-parts keep their
conservative reading; the walk consolidation (GLM/anthropic simplify#1),
the battery dedup between the self-test and the 331-mutant table, lazy
event computation, the fourth inline `USub` normalizer, the dead
`if not priceable: return True` branch and the `_unwrap_named` micro-helper
are unchanged from round 44.

| Shape | Self-test |
|---|---|
| **A same-statement composite replays in evaluation order** (false PASS guard): `insert(2, pop(0))` in one statement, and `insert(2, h) if pop(0) else None` (the `IfExp` test runs first) -- withdraw. | `... lost a receiver behind a same-statement insert(2, pop(0)) composite / an IfExp test-first composite (round 45)` |
| **The clean twins stay clean, deduped** (false FAIL guard): `insert(1, pop(0))`; a never-called closure body; a single clean pop inside an `if` branch (an event enters the replay once, not once per enclosing statement). | `... withdrew a clean receiver behind a same-statement insert(1, pop(0)) composite / a never-called closure body between binding and use / a single clean pop inside an if branch (deduped window) (round 45)` |
| **A loop `else` block runs once** (false FAIL guard / false PASS guard): a for-else or while-else single clean pop does not withdraw; a for-else two-pop composite still withdraws. | `... withdrew a clean receiver behind a for-else single clean pop / a while-else single clean pop (round 45)`; `... lost a receiver behind a for-else two-pop composite (round 45)` |
| **A whole-subject match capture and a literal-seed for target join the alias closure** (false PASS guard): `match _items: case _n2: _n2.reverse()`; `for _n2 in (_items,): _n2.reverse()`; and a match-capture mutator alias `match _mutate: case _m: _m()` -- withdraw. | `... lost a receiver behind a whole-subject match capture mutation / a for-target over a single-seed literal / a match-capture mutator alias call (round 45)` |
| **A writeless or element capture stays outside** (false FAIL guard): `case _n2:` without a mutation; `case [_n3]`; a for target over a mixed literal. | `... withdrew a clean receiver behind a writeless match capture / a nested-element match capture (binds an element) / a for target over a mixed literal (round 45)` |
| **A match-capture dict receiver write unbinds the keys** (false PASS guard): `match d: case e:` with `e['common'] = 1` reads unbounded; the writeless capture keeps `{'x'}`. | `... kept dict keys bounded behind a match-capture receiver write (round 45)`; `... withdrew dict keys behind a writeless match capture (round 45)` |
| **Every binding spelling of a mutating lambda seeds the mutators** (false PASS guard): AnnAssign, walrus, multi-target and unpack projections of `lambda: _items.pop(1)` -- withdraw; a read-only AnnAssign lambda stays clean. | `... lost a receiver behind an AnnAssign lambda mutator / a walrus lambda mutator / a multi-target lambda mutator / an unpack lambda mutator (round 45)`; `... withdrew a clean receiver behind a read-only AnnAssign lambda (round 45)` |
| **Every binding spelling of the class-body `__new__` keeps the import standing** (false PASS guard): AnnAssign, tuple-target, walrus, `from m import f as __new__`, and the clean *assigned* builder (the registered conservative standing) -- registered; a clean `def __new__` stays value-analyzed. | `... dropped the import standing behind an AnnAssign __new__ / a tuple-target __new__ / a walrus __new__ / an imported __new__ / a clean AnnAssign __new__ builder (conservative standing) (round 45)`; `... kept the import standing behind a clean def __new__ builder (value-analyzed) (round 45)` |
| **The walrus callee keeps its reading at the six helper sites** (false PASS guard / false FAIL guard): `(_a := all)(r.passed ...)`, `(_c := md.common.aggregate_status)(report)`, `(_l := load)('x')`, `(_s := str)('a')`, `(_h := h)()`, `(_p := load_pristine_common)()` all read; the `len`/`other`/`open`/pure-read mirrors do not. | `... lost the aggregate marker / the shared call / the guard loader / the fresh value / the loop-target handle use / the proven source behind a walrus callee (round 45)`; the four negative mirrors each carry their own `kept ... behind a walrus non-X / pure read` message (round 45) |
| **A tuple-target `del` rebinds the base name** (floor lock): `del (_B, _x)` before `class imp(_B)` keeps the conservative import standing. | `... dropped the import standing behind a tuple-target del rebind (round 45)` |

### Closed by `fix-r1` round 46 (GLM r45 boundary-rerun#1..#3 / concurrency#1..#4 / sibling#1/#2/#5; anthropic r45 boundary#1..#8 / sibling#1..#5; probe 2026-11-10)

Every finding was probe-reproduced against the round-45 committed ruler
before any fix (frozen procedure: `probe-r46.log` holds the pre-fix
columns -- the "r45" column there is the round-45 commit `c0f24ca86`,
the "r44" column the round-44 commit `72f6f67ee` kept as a second
witness, and every dirty row carries a runtime oracle;
`probe-r46-after3.log` holds the post-fix state, re-run byte-identical
after the last battery-round fix).  Two reviewer claims did not
reproduce and were rejected *with* that probe evidence: the
closure-store shape (`a-store-closure`) already convicts at round 45
(the display store guard fires without the window walk -- the claim's
false-PASS premise was the window, the ruler never needed it), and the
double-pop Assign (`b-assign-dirty2`) already convicts at round 45 --
it is runtime-clean but the selection *address changes*, which the
registered address-change conservative floor convicts by design (the
same standing the round-44 section registered; the probe claim text
itself notes the floor).

The scope-execution family (GLM concurrency#1a; anthropic boundary#1).
A *class* body executes at its `class` statement, but `_scoped_nodes`
never yields the `ClassDef` node itself (it pushes only the header
expressions), so round 45 lost every class-body event the round-44
`ast.walk` window used to price -- a false PASS regression.  The window
now walks `_window_statements`, the same walk with one rule changed: a
`ClassDef` child enters the stream as a statement, so its body descends
right where the `class` line sits (a nested `def`/`lambda` inside the
class body keeps the headers-only rule).  An immediately-invoked lambda
runs its body at the call: `_eval_order_nodes` descends a
Lambda-callee's body after the call's argument expressions (CPython
evaluates defaults, then the argument list, then the body).

The mutator net (GLM concurrency#1b; GLM sibling#2).  A wrapper that
only *calls* a mutator stayed outside `mutators` while its inner call
sat in a nested scope the window never walks -- a false PASS.  The
seeding is now a transitive fixpoint: `_body_calls_mutator` closes
wrapper definitions, `_body_mutates` reads non-Call stores and deletes
on display names (Assign/AugAssign/AnnAssign/Delete targets and For
targets through `_target_hits_display`), binding values unwrap
`NamedExpr` (`_g = (_h := lambda: ...)` seeds both names), a for target
bound to a mutating lambda over a literal Tuple/List iterable seeds the
target names, and `_name_alias_closure` runs inside the fixpoint so
alias chains feed back.

The evaluation-order family, second wave (GLM concurrency#2; GLM
boundary-rerun#2; anthropic sibling#1/concurrency#1/boundary#2).  The
AST *field* order is not the evaluation order on the remaining
composite spellings: an assignment evaluates its value before the
target's subscript components (`Assign._fields` lists targets first),
an `AnnAssign`'s value runs before its target while its *annotation*
is not evaluated at the statement at all (PEP 649 lazy annotations --
kept out of the order), a dict display interleaves each key with its
value (`Dict._fields` lists all keys then all values; `**`-unpacking
keys are `None` and skipped), and a `for` evaluates its iterable
before its target.  `_eval_order_nodes` spells all of these as the
runtime order; the mirror Assign (runtime-clean under value-first
order) stopped reporting at the same time.

The fork family (GLM concurrency#3; anthropic boundary#5).  The
round-44/45 replay ran *every* priced event in the window, so the
unexecuted arm of an `if`/`else` (or a short-circuited boolop right
side) could cancel the executed arm's move -- a false PASS.
`_conditional_arm_chains` maps every node of the scope to its chain of
`(fork, arm)` placements; the replay enumerates the cartesian product
of arm choices and convicts when any *executable* path convicts.  An
`if` with an `else` carries exactly one arm ({0, 1}); without one it
also carries the no-arm choice ({0, -1}); an `IfExp` always evaluates
exactly one arm; a `match` carries the no-case-matched choice; a
`BoolOp` operand at index i runs only when the chain reached it
(choice >= i; operand 0 always runs).  `Try` arms keep the registered
conservative read (all handlers replayed); match *patterns* cannot hold
calls or deletes, so only guards sit inside an arm.  The combination
count is capped at 16 -- beyond it the verdict is `True` (fail-closed).
A first draft of this walk iterated `IfExp.body`, which is a single
expression node, and crashed the whole audit on the round-45 `IfExp`
battery row; the battery caught it before commit and the walk now has
an `IfExp`-specific branch.

The post-use truncation (anthropic boundary#4).  The window filtered at
*statement* granularity, so an event after the use inside the use's own
compound statement entered the replay and could cancel the move -- a
false PASS.  Every priced event now carries its own position: an event
at or after `use_pos` never enters the replay.

The del dedup completion (GLM concurrency#4 + sibling#1 in both
lineages; anthropic sibling#2).  The round-45 `priced` guard covered
only Call events; a nested `del items[k]` entered the replay once per
enclosing compound statement (a false report).  The removal append
carries the same guard.

The for-target repetition (anthropic boundary#6).  `_repeated_between`
never pushed the `for` target, so its per-iteration subscript events
were priced once -- a false PASS.  The target now re-runs like body
events unless the iterable is a one-element literal (which keeps the
linear single-iteration read); an empty, longer or non-literal iterable
takes the conservative `inner` flag.

The match-capture spellings (GLM boundary-rerun#3; anthropic
sibling#3).  A toplevel `case <pattern> as _n2:` binds the *whole
subject* even under a sub-pattern, and every `MatchOr` arm that
captures the whole subject does the same; the round-45 closure read
only bare `MatchAs(pattern=None)`.  `_toplevel_match_captures`
unwraps both spellings for the alias closure (nested-element captures
stay outside, as registered).  The class-body `__new__` walk reads the
same shapes (anthropic sibling#4: a class-body `match` binds `__new__`
through its capture) and now descends def/lambda/class *headers* in the
class body (anthropic boundary#8: a walrus default
`def f(x=(__new__ := staticmethod(...)))` binds the class attribute;
the block comment always promised "header expressions still run").

The last two bare `.func` reads (GLM sibling#5, fixed the same round as
a 15-minute shape): `_module_load_call` and `_iterable_dict_keys` route
through the shared `_callee_expr` walrus unwrap.

Known residuals (follow-up issue): a callback-passed mutator
(`map(_mutate, ...`) is still not an invocation; nested-element match
captures and non-literal for iterables stay outside the alias closure
(pinned by battery); comprehension later-parts keep their conservative
reading; `literal_seed_iterable` returns a vacuous True on an *empty*
literal (over-report direction only); Try arms and cross-case match
guard composites stay conservative; events inside the use's own
subscript path truncate with the same-statement nuance;
`_unbounded_write`'s `_expr_kinds` and `_getattribute_pure_read`'s
parent chain do not unwrap `NamedExpr` (false-FAIL direction); PEP 649
laziness can still price a phantom annotation event (false-FAIL
direction); `_target_names` returns no name for a Starred for-target;
Call keyword/starred source-order mixing is unverified against CPython;
the round-45 simplify debt (walrus-unwrap dedup helper, comment phrase,
two-pass walk merge, `_body_mutates` layering) is unchanged.

| Shape | Self-test |
|---|---|
| **Scope-execution spellings enter the window** (false PASS guard): a class-body composite, an indirect wrapper (`_outer()` calling `_mutate()`), an immediately-invoked mutating lambda, a closure body storing into a display subscript, and a for-target bound to a mutating lambda -- withdraw. | `... lost a receiver behind a class-body composite between binding and use / an indirect mutator call through a wrapper / an immediately-invoked mutating lambda / a closure body storing a handle into a display subscript / a for-target bound to a mutating lambda over a literal (round 46)` |
| **The scope-execution clean twins stay clean** (false FAIL guard): a compensating class-body composite, a wrapper calling a clean reader, an immediately-invoked clean lambda, a closure body storing into another name's subscript, and a for-target clean reader. | `... withdrew a clean receiver behind a compensating class-body composite / a wrapper calling a clean reader / an immediately-invoked clean lambda / a closure body storing into another name's subscript / a for-target bound to a clean reader lambda (round 46)` |
| **Composite spellings replay in evaluation order** (false PASS guard): Assign value-before-target, AnnAssign value-before-target, Dict pair-interleaved, For iter-before-target, a For target re-evaluated every iteration, and the double-pop Assign address change (the registered conservative floor) -- withdraw. | `... lost a receiver behind an Assign value-before-target composite / an AnnAssign value-before-target composite / a Dict pair-interleaved composite / a For iter-before-target composite / a For target re-evaluated every iteration / a double-pop Assign address change (conservative floor) (round 46)` |
| **The evaluation-order clean twins stay clean** (false FAIL guard): the same three shapes at the clean index, and the mirror Assign whose value-first order removes the handle before the use (the round-45 false report on this spelling is gone). | `... withdrew a clean receiver behind an Assign composite keeping the use clean / a Dict pair-interleaved composite keeping the use clean / a For iter-before-target composite keeping the use clean / a mirror Assign whose value-first order stays clean (round 46)` |
| **Mutually exclusive regions fork the replay** (false PASS guard): a statically-taken if arm with a cancelling else arm, a statically-taken else arm composite, a boolop right side that never runs, and a trailing pop after the use inside its own statement -- withdraw. | `... lost a receiver behind a statically-taken if arm with a cancelling else arm / a statically-taken else arm composite / a boolop right side that never runs / a trailing pop after the use inside its statement (round 46)` |
| **The fork clean twins stay clean** (false FAIL guard): mutually exclusive arms each pricing safe alone, and a trailing pop after a clean move at the use. | `... withdrew a clean receiver behind mutually exclusive arms each pricing safe alone / a trailing pop after a clean move at the use (round 46)` |
| **A nested `del` enters the replay once** (false FAIL guard / false PASS guard): a single `del _items[1]` inside an `if` does not withdraw; two distinct dels and a walrus-wrapped lambda mutator called twice still withdraw; a walrus-wrapped reader lambda stays clean. | `... withdrew a clean receiver behind a single nested del / a walrus-wrapped reader lambda (round 46)`; `... lost a receiver behind two distinct nested dels / a walrus-wrapped lambda mutator called twice (round 46)` |
| **A sub-pattern `as` capture binds the whole subject** (false PASS guard): `case [*_] as _n2: _n2.reverse()`, a `MatchOr` arm capture, and a sub-pattern match-capture mutator alias call -- withdraw; the capture used only to read stays clean. | `... lost a receiver behind a sub-pattern whole-subject match capture mutation / a MatchOr whole-subject capture mutation / a sub-pattern match-capture mutator alias call (round 46)`; `... withdrew a clean receiver behind a sub-pattern match capture used only to read (round 46)` |
| **A class-body match capture and a header walrus bind `__new__`** (false PASS guard): `match staticmethod(_mk): case __new__:` and `def f(x=(__new__ := staticmethod(...)))` keep the import standing; the plain-attribute controls do not. | `... dropped the import standing behind a match-captured __new__ / a walrus-default __new__ in a class-body def header (round 46)`; `... kept the import standing behind a match capture on a plain attribute / a walrus default on a plain attribute (round 46)` |
| **The last two helper sites unwrap a walrus callee** (false FAIL guard): `(_g := sys.modules.get)('_guard')` stays a proven load; `(_d := d.keys)()` reads the dict's keys. | `... lost a proven module load behind a walrus callee / withdrew dict keys behind a walrus callee (round 46)` |

### Closed by `fix-r1` round 47 (GLM r46 boundary#1/#2 / concurrency#1/#2 / sibling#1/#2/#3; anthropic r46 boundary#1..#6; probes 2026-11-10)

Every finding was probe-reproduced against the round-46 committed ruler
before any fix (frozen procedure: `probe-r47.log` holds the pre-fix
columns -- the "r46" column there is the round-46 commit `acfd050dc`,
the "r45" column the round-45 commit `c0f24ca86`, and every dirty row
carries a runtime oracle; `probe-r47-final.log` holds the post-fix
state, re-run after the battery caught a regression mid-round, below).
One reviewer claim did not reproduce and was rejected *with* that probe
evidence: the use-in-an-Assign-target shape (`_d[_items[-1](...)] =
_items.insert(3, _hs[0])`, GLM boundary#1 + anthropic boundary#1)
already convicts at round 46 -- an Assign whose *target* holds the use
rides the registered conservative target-subscript floor (the target
store is itself a display mutation, so the selection declines before the
window is ever consulted), and the claim's premise (the window's
after-use filter decides the shape) was wrong; its clean twin's
over-report is the same standing floor, present since round 45.

The element-store registration family (the false PASS under every
insertion spelling).  The container fixpoint registered
`handle_containers` only from *binding values* -- `items = [hs[0], L]`
-- so a container bound clean that later *grows* a handle through
`items.append(z)` / `insert` / a subscript store `items[k] = z` / a
For-target write `for items[k] in hs:` / `items += [z]` / `extend`
stayed invisible forever: when a window mutation later made the
selection decline, the whole-display fallback was empty and the receiver
walked clean (all four conditional findings traced here, not to the
fork model).  `_register_element_stores` registers each such store the
way a binding of the same value would, through `_carried_handle_roots`
(the shared chain a *value* carries), `_store_base_names` (the base
names a subscript store writes through) and `_store_receiver_names`
(the symmetric name class: `_name_alias_closure` follows value hops
forward only, so a store through `_a` after `_a = items` never reached
`items`; the union over the scope's bare-name binding pairs answers
both directions).

The fork family, second wave (anthropic boundary#2/#3/#4).  The round-46
replay ran Try arms together (an unexecuted handler's `pop` cancelled
the executed body's `append`), put a failed match guard's pop on its
own arm so no path saw guard(i) + body(j>i) -- the fall-through both
run -- and knew neither the chained-comparison tail (`1 > 2 <
items.pop()`, the pop never runs) nor the assert message (it only runs
on failure).  A `Try` body and `finally` always run while at most one
handler -- or the `else` block, on the no-arm choice -- joins them; a
guard rides its own fall-through fork keyed apart from the body fork;
a comparison with *n* ops forks like a `BoolOp` (comparator *i* runs
for choice >= i); an `assert` forks its message on the test failing.

The evaluation-order after-use cut (anthropic boundary#6, probe
p1b-reverse; keyword-then-starred, probe p1c-info).  Source position is
not execution order inside one statement: an Assign target evaluates
after its value, and a keyword argument written before a starred
argument evaluates after it.  When the use itself lives in the priced
statement the cut compares eval-order indices; the direct convictions
(mutator-name calls, unpriceable mutations) take the same cut (GLM
concurrency#2, probe p7-direct: a `_mut()` call after the use inside
its arm cannot move what the use reads).

The remaining spellings: a one-element tuple/list iterable whose only
element is `Starred` (`for _d[items.pop(1)] in (*_src,)`) iterates an
unknown number of times, so the target re-runs (GLM boundary#2 +
anthropic boundary#5, probe p2-starred); a `del a[1], a[1]` keys its
`priced` dedup on the *target*, not the Delete statement, so the second
removal priced (GLM concurrency#1, probe p3-del-multi); a class body
inside a loop re-runs its statements (GLM sibling#1, probe
p8-class-loop) and the module call tables now record a class-body call
at its `class` statement and carry the loop id on a for-target call
(GLM sibling#2/#3, unit probes u-class-site/u-fortarget-loops).

The mid-round regression the battery caught before commit: the first
draft of the after-use gate hoisted the `repeated` check above the
mutation determination, so *any* call inside a loop -- including the
round-46 clean twin's for-target bound to a clean reader lambda --
convicted without ever being a mutation.  The `repeated`/`after_use`
pair now sits inside every mutation path (mutator-name, unpriceable,
priced) and a call that mutates nothing falls through untouched.

Known residuals (follow-up issue): the use-in-Assign-target shape keeps
its registered conservative floor (over-report direction only); a
`Try` interrupted mid-body (the body ran half its events before the
handler) over-reports; the mutator-name conviction is not fork-aware
(a mutator call on an unexecuted arm convicts -- over-report
direction); `extend` of an empty literal over-reports (the pre-existing
pricing quirk); a clean rebind leaves stale container history that a
later dynamic path still reads conservatively; starred/keyword
*dynamic* arguments (`items.append(*args)`) are not statically
analyzable; `_flow_marks` still pushes a plain for-target without the
loop's flow mark (unchanged from round 46).

| Shape | Self-test |
|---|---|
| **Element stores register the container** (false PASS guard): an append of a bound handle, an append through an alias, an unbound `list.append`, an insert, a subscript store, a for-target write, an augmented append, an extend of a tracked container -- all into a clean-bound container -- withdraw. | `... lost a receiver behind an append of a bound handle into a clean container / an append through an alias of the container / an unbound list.append store / an insert of a handle into a clean container / a subscript store of a handle into a clean container / a for-target write of a handle into a clean container / an augmented append of a handle into a clean container / an extend of a tracked container into a clean container (round 47)` |
| **The element-store clean twins stay clean** (false FAIL guard): an append/insert/subscript store/for-target write/augmented append of a clean lambda, and a handle append narrowed to a clean leading element. | `... withdrew a clean receiver behind an append of a clean lambda / a handle append narrowed to a clean leading element / an insert of a clean lambda / a subscript store of a clean lambda / a for-target write over a clean literal / an augmented append of a clean lambda (round 47)` |
| **Forked tails no longer cancel** (false PASS guard): a try-body append cancelled by an unexecuted handler pop, a comparison tail pop that short-circuits away, an assert message pop that never runs, and a match guard fall-through composite -- withdraw. | `... lost a receiver behind a try body append cancelled by an unexecuted handler pop / a comparison tail pop that short-circuits away / an assert message pop that never runs / a match guard fall-through composite (round 47)` |
| **The fork clean twins stay clean** (false FAIL guard): try arms each pricing safe alone, a comparison tail pop after a safe insert, an assert message pop after a safe insert, and a match guard with two head pops. | `... withdrew a clean receiver behind try arms each pricing safe alone / a comparison tail pop after a safe insert / an assert message pop after a safe insert / a match guard with two head pops (round 47)` |
| **Evaluation order inside one statement** (false PASS guard / false FAIL guard): a keyword-then-starred pair convicts (the starred insert runs first); a mirror value-side use after a target-side insert stays clean. | `... lost a receiver behind a keyword-then-starred evaluation-order pair (round 47)`; `... withdrew a clean receiver behind a mirror value-side use after a target-side insert (round 47)` |
| **The remaining spellings** (false PASS guard): a starred one-element for-iterable double pop, a del statement's second same-index target, and a class body re-run inside a loop -- withdraw; a mutator call after the use inside its arm stays clean (the after-use cut gates the direct convictions). | `... lost a receiver behind a starred one-element for-iterable double pop / a del statement's second same-index target / a class body re-run inside a loop (round 47)`; `... withdrew a clean receiver behind a mutator call after the use inside its arm (round 47)` |
| **A del statement's second head-index target stays clean** (false FAIL guard): `del _items[0], _items[0]` keeps the trailing element a lambda, and a class body outside any loop prices once. | `... withdrew a clean receiver behind a del statement's second head-index target / a class body outside any loop (round 47)` |
| **The module call tables carry class-body and for-target sites** (unit): a class-body call records its `(3, 4)` position with no loop id; a for-target call inside a module loop carries the loop id. | `... dropped the class-body call site from the position table / dropped the loop id from a for-target call site (round 47)` |

### Closed by `fix-r1` round 48 (GLM r47 boundary#1..#3 / concurrency#1..#3 / sibling#3; anthropic r47 boundary battery row; probes 2026-11-10)

Every finding was probe-reproduced against the round-47 committed ruler
`ec67bc358` before any fix (frozen procedure: `probe-r48.log` holds the
pre-fix columns -- the "r47" column there is the round-47 commit, the
"r46" column `acfd050dc` -- and `probe-r48b.log` the spelling-family
extension rows; every dirty row carries a runtime oracle).  One reviewer
claim did not reproduce and was rejected *with* that probe evidence:
the value-side use / target-side store mirror
(`_items[0] = _items[-1]('common', object())`, GLM sibling#1) reads
clean at round 47 with its runtime twin clean too (`s1-valueside-use-
store-clean`, R = False = runtime) -- the use evaluates before the
store runs, so the selection's element really is the clean lambda.

The keyword-form store spellings (GLM boundary#1, P0).  The r47
registration family scanned only positional arguments, so a keyword
`_d.update(common=_hs[0])` -- and its `**{'common': ...}` unfolding,
which compiles to the same keywords -- never registered the dict, and
the whole r47 chain (mutation -> decline -> whole-display fallback)
fell through to an empty table (a false PASS on the exact family r47
claimed closed).  The keyword values join the positional ones in
`_register_element_stores` (`update` is the store family's only
kwargs-taking spelling).

The dict augmented merge (GLM boundary#2 + concurrency#3, an r47
incompleteness carried from r46).  `_d |= {'common': _hs[0]}` merges in
place, but the r47 AugAssign pricing branch gated itself on `not
display_is_dict` and the target loop only matches subscripts, so the
merge left *no* event and no conviction.  A dict display has no element
price, so the window now declines the selection outright (the same
conservative read as every other unpriceable dict write); the fixpoint
registers the receiver only when the merged literal carries a handle,
which keeps the clean twin clean.

The guard on a never-matching pattern (GLM concurrency#1 -- an r47
*regression*, r46 convicted it).  `match 0:` under `case 1 if
<events>` never evaluates the guard -- the literal pattern fails first
-- yet the r47 fall-through fork ran those events on every path, and
their insertion cancelled the executed arm's removal (a false PASS).
Statically unmatchable literal patterns (a constant subject against a
constant `MatchValue`/`MatchSingleton` it compares unequal to, with
`==` semantics -- `match 1:` does take `case True`) now ride the case
arm itself, which cannot run either; every other shape (non-constant
subjects, sequence/class/mapping/or patterns, attribute values) keeps
the conservative fall-through fork.

The alias augmented grow (GLM concurrency#2).  The fixpoint's AugAssign
arm registered only the target name through `_carry`, so `_a = _items;
_a += [_hs[0]]` registered `_a` while the caller's fallback looks
`_items` up and stays empty.  The grow is in place, so every name of the
target's object registers together -- the name-target arm now goes
through `_register_container_names` (the symmetric name class), the
same read a store through an alias gets.

The remaining spellings (GLM boundary#3 + sibling#3): a static starred
argument `_items.append(*[_hs[0]])` unpacks its literal before the
call (`_carried_handle_roots` unwraps `ast.Starred`; only *dynamic*
starred arguments stay residual); `operator.setitem(_items, 1, _hs[0])`
/ `operator.iadd(_items, [_hs[0]])` name the display first the way the
unbound type spelling does -- both the mutation gate in
`mutates_display` (whose entry now admits the operator store trio;
`delitem` joins them) and the registration walk read them, and
`builtins.list.append(_items, z)` is the unbound spelling behind one
attribute hop on both sides.  The r47 battery's for-site self-test
asserts its two recorded sites exist before `all()` (GLM boundary#4:
an empty table answered `all()` True).

Known residuals (follow-up issue; unchanged from round 47 unless
noted): the use-in-Assign-target conservative floor, a `Try`
interrupted mid-body, the not-fork-aware mutator-name conviction, and
`extend` of an empty literal all over-report; stale container history
survives a clean rebind; *dynamic* starred/keyword arguments remain
unanalyzable (the *static* starred literal closed this round); the
guardfall fork stays independent of the match fork in the combination
enumeration (unexecutable combinations can only over-report -- GLM
boundary#5); `_scoped_nodes` class-body walrus, `_name_bindings`
for-target spellings and the For-only `AsyncFor` walks at the two
module-table sites remain (GLM sibling#2, not probe-reproduced this
round).

| Shape | Self-test |
|---|---|
| **The keyword-form store spellings register** (false PASS guard): a keyword `update`, a `**`-unpacked `update`, an unbound `dict.update(d, ...)` and a dict `\|=` merge of a handle -- withdraw. | `... lost a receiver behind a keyword-form dict update of a handle / a **-unpacked dict update of a handle / an unbound keyword-form dict update of a handle / a dict augmented merge of a handle (round 48)` |
| **The keyword-form clean twins stay clean** (false FAIL guard): the same four spellings storing a clean lambda. | `... withdrew a clean receiver behind a keyword-form dict update of a clean lambda / a **-unpacked dict update of a clean lambda / an unbound keyword-form dict update of a clean lambda / a dict augmented merge of a clean lambda (round 48)` |
| **A guard on a never-matching pattern no longer launders** (false PASS guard / false FAIL guard): the guard's insert cannot cancel the executed pop; the clean fall-through twin stays clean. | `... lost a receiver behind a guard on a never-matching pattern (round 48)`; `... withdrew a clean receiver behind a never-matching guard with a clean fall-through (round 48)` |
| **An aliased augmented grow registers the name class** (false PASS guard / false FAIL guard): `_a += [handle]` registers `_items` too; the clean lambda twin stays clean. | `... lost a receiver behind an aliased augmented append of a handle (round 48)`; `... withdrew a clean receiver behind an aliased augmented append of a clean lambda (round 48)` |
| **The static starred and qualified spellings register** (false PASS guard): `append(*[handle])`, `operator.setitem`, `operator.iadd` and `builtins.list.append` of a handle -- withdraw. | `... lost a receiver behind a starred-literal append of a handle / an operator.setitem store of a handle / an operator.iadd grow of a handle / a builtins-qualified append of a handle (round 48)` |
| **The static starred and qualified clean twins stay clean** (false FAIL guard): the same four spellings plus the alias and unbound appends of a clean lambda. | `... withdrew a clean receiver behind a starred-literal append of a clean lambda / an operator.setitem store of a clean lambda / an operator.iadd grow of a clean lambda / a builtins-qualified append of a clean lambda / an alias append of a clean lambda / an unbound append of a clean lambda (round 48)` |
| **The for-site self-test asserts its sites** (unit): the module call table must still record both `_f` sites with their loop ids. | `... dropped the loop id from a for-target call site (round 47)` |

### Closed by `fix-r1` round 49 (GLM r48 boundary#1 P0 / sibling#1 P1 / sibling#2 P1 / sibling#3..#5 P2; probes 2026-11-10)

Every finding was probe-reproduced against the round-48 committed ruler
`ae2c8fb0f` before any fix (`probe-r49.log` holds the pre-fix columns --
the "r47" column there is `ec67bc358`, showing the same miss predates
r48, the review's own precondition -- and every row carries a runtime
oracle).  The P0 was confirmed: all four import spellings read R = False
while their run times poisoned the relay.

The import spellings of the store family (GLM boundary#1, P0 -- the same
family the r47 keyword-form P0 opened, so this round is *structural* per
the long-tail closure rule, judgment 1: a second recurrence of the
escape family must close by one change that kills the family, not by
another literal addition).  Both gates read the qualified spellings off
the literal attribute chain -- `callee.value.id == 'operator'` /
`'builtins'` -- so `import operator as _op; _op.setitem(_items, 1,
_hs[0])`, `from operator import setitem [as _si]; setitem(...)`,
`import builtins as _b; _b.list.append(_items, z)` and `from builtins
import list as _l; _l.append(_items, z)` missed the mutation window
*and* the registration walk at once: zero events, zero registration, a
false PASS on the family the round-48 diff claimed closed while its
residual list only registered the dynamic kwargs/starred forms.  The
fix reads the import bindings once
(`_import_binding_origins`: `import x as y` -> module `x`, `from x
import f [as g]` -> `x.f`, scope subtree plus the module body) and
feeds both gates from it: the callee's base resolves to the operator
module / the builtins module / the from-imported type, and a bare-Name
callee resolves to a from-imported operator store function.  The
literal names keep their reading (the map only adds spellings), which
is why every pre-existing audit row is untouched.

The operator word lists consolidated (one place, both gates):
`_OPERATOR_STORE_FUNCS` (`setitem`/`iadd`/`iconcat`/`ior` plus the
module's own dunder aliases `__setitem__`/`__iadd__`/`__iconcat__`/
`__ior__`, verified against `dir(operator)`) with
`_OPERATOR_DISPLAY_MUTATORS` widening it by the deleters
(`delitem`/`__delitem__` -- a deletion mutates but never stores a
handle, so the registration walk does not admit it).

`operator.ior` (GLM sibling#1, P1): the module spelling of `d |= {...}`
was in none of the three admission points while `__ior__` sat in
`_MAPPING_METHODS` -- now the mutation window, the registration walk and
the mapping decoder (`_mapping_method_call`, which names its target
first the way `operator.setitem` does) all read it.  The in-place dunder
spellings `_items.__iadd__([...])` / `_d.__ior__({...})` /
`operator.__setitem__(...)` join `_MUTATING_DISPLAY_ATTRS` /
`_ELEMENT_STORE_ATTRS` the same way (GLM sibling#3, P2).

The never-matching patterns widened (GLM sibling#2, P1): a constant
subject also never matches a sequence or mapping pattern (PEP 634 -- a
constant is never a sequence or a mapping, `str`/`bytes` included) or an
`or` pattern whose every alternative is unmatchable, so those guards
ride the case arm instead of the fall-through fork -- the same wash the
round-48 `c1` probe proved for `MatchValue`.  Class patterns and
attribute values stay conservative.

The immutable augmented rebind (GLM sibling#4, P2 -- **rejected**,
`probe-r49c.log`): the review claimed the r48 name-class registration
poisoned the alias of an immutable `+=` rebind (`_t = (lambda,); _a =
_t; _t += (_hs[0],)`) as an r48 regression over an "r47-correct"
plain-`_carry` reading.  The probe shows both of the review's own forms
disagree with that premise: the iteration form (`for _h in _a:`) is
clean in r47, r48 and r49 alike, and the subscript form (`_a[0](...)`)
over-reports in r47 exactly as in r48 -- the conviction there comes
from the pre-existing window floor (`_display_mutated_between`
declining a stale selection; "over-report beats a hidden write"), not
from the registration arm, and an outer-display relay form shows no
difference between the two readings either.  No narrowing was made --
an unverifiable change is worse than the documented fail-closed floor;
the over-report itself is registered as a residual below.

Wording corrections riding the round: the r48 claim "`update` is the
store family's only kwargs-taking spelling" is wrong (`setdefault`
takes one too -- the code always merged all keywords, GLM boundary#2),
and the r48 residual "dynamic starred arguments stay unanalyzable"
understates the register side: a *tracked container's* dynamic starred
argument already registers through the `Name` branch (GLM boundary#3)
-- the residual is now worded to untracked/non-literal dynamic starred
arguments.

Known residuals (follow-up issue; unchanged from round 48 unless
noted): the use-in-Assign-target conservative floor, a `Try`
interrupted mid-body, the not-fork-aware mutator-name conviction, and
`extend` of an empty literal all over-report; stale container history
survives a clean rebind; *untracked or non-literal* dynamic
starred/keyword arguments remain unanalyzable (a tracked container's
dynamic starred argument registers since r48 -- GLM r48 boundary#3);
the guardfall fork stays independent of the match fork in the
combination enumeration (unexecutable combinations can only
over-report); `_scoped_nodes` class-body walrus, `_name_bindings`
for-target spellings and the For-only `AsyncFor` walks at the two
module-table sites remain; an import binding a name later rebinds to
another object still reads as the module (the registered
`_name_alias_closure`-style over-approximation, honest spellings pinned
by the clean twins); the `operator.__setitem__`-on-a-receiver spelling
in `_mapping_method_call` keeps its pre-r49 target read (not asked, no
residual registered before); a tuple/str display's `+=` rebind makes a
*subscript* use of its alias over-report through the window floor (the
selection declines on the augmented rebind and falls back to the
whole-display roots -- pre-existing since the floor's introduction,
probe-r49c, not the registration arm the rejected sibling#4 named); an
outer display relaying a container grown by ``+=`` through a name
alias misses the handle (``_t = [..]; _a = _t; _t += [hs]; _outer =
(_a,); _outer[0][-1](...)`` -- the same shape grown by ``append``
instead convicts, so the gap is the outer-display element roots not
reading the AugAssign-registered carrying; pre-existing, probe-r49c
``s4-list-alias-outer-display-dirty``, r47/r48/r49 read identically).

| Shape | Self-test |
|---|---|
| **The import spellings of the store family register** (false PASS guard): `import operator as _op`, `from operator import setitem [as _si]`, `import builtins as _b` and `from builtins import list as _l` storing a handle -- withdraw. | `... lost a receiver behind an import-aliased operator.setitem store of a handle / a from-imported operator.setitem store of a handle / a from-import-aliased operator.setitem store of a handle / an import-aliased builtins-qualified append of a handle / a from-imported type append of a handle (round 49)` |
| **The import-spelling clean twins stay clean** (false FAIL guard): the same spellings storing a clean lambda. | `... withdrew a clean receiver behind an import-aliased operator.setitem store of a clean lambda / a from-imported operator.setitem store of a clean lambda / an import-aliased builtins-qualified append of a clean lambda / a from-imported type append of a clean lambda (round 49)` |
| **`operator.ior` and the dunder spellings register** (false PASS guard): `operator.ior(_d, {...})` (plain and import-aliased), `operator.__setitem__`, `_items.__iadd__([...])` and `_d.__ior__({...})` of a handle -- withdraw. | `... lost a receiver behind an operator.ior dict merge of a handle / an import-aliased operator.ior dict merge of a handle / an operator dunder-alias setitem store of a handle / an in-place dunder append of a handle / an in-place dunder dict merge of a handle (round 49)` |
| **The ior/dunder clean twins stay clean** (false FAIL guard): the same spellings merging a clean lambda. | `... withdrew a clean receiver behind an operator.ior dict merge of a clean lambda / an in-place dunder append of a clean lambda / an in-place dunder dict merge of a clean lambda (round 49)` |
| **The never-matching sequence / mapping / or guards no longer launder** (false PASS guard / false FAIL guard): the guard's insert cannot cancel the executed pop; the clean fall-through twin stays clean. | `... lost a receiver behind a guard on a never-matching sequence pattern / a guard on a never-matching mapping pattern / a guard on an all-failing or-pattern (round 49)`; `... withdrew a clean receiver behind a never-matching sequence guard with a clean fall-through (round 49)` |
| **A tuple `+=` rebind's alias stays clean** (false FAIL guard): `_t += (handle,)` rebinds, so iterating the alias reads the old clean object. | `... withdrew a clean receiver behind a clean lambda iterated off a tuple alias after a tuple += (round 49)` |

### Closed by `fix-r1` round 50 (DS r49 boundary#1 P0 / boundary#2 P0 / concurrency#1 P0 / concurrency#2 P0 / concurrency#3 P0 / sibling#1 P1 / sibling#2 P1 / sibling#5 P1; probes 2026-11-10)

Every finding was probe-reproduced against the round-49 committed ruler
`9a1b4cf8a` before any fix (`probe-r49d.log` holds the pre-fix columns,
every row carrying a runtime oracle; the r49 star cell there was vacuous
-- a `from operator import *` inside a `def` is a SyntaxError, so the
probe's module-level rerun is the real evidence).  The structural second
layer: r49 fixed the *identification key* (literal attribute chain ->
binding resolution), r50 fixes the *value domain* -- the scope model kept
one origin per name (the module read only `tree.body` direct children,
a function scope took `ast.walk(scope)` with overwrite), so nine holes
shared the single root cause `_import_binding_origins`' scope semantics.
**Fix type: structural** -- one change of the scope model's value domain
closes the family, not another literal addition; the residual-judgment
two questions: r49's hole was the value domain (single value -> missed
shadowing and layers), r48's hole was the identification key (literal
chain -> binding resolution) -- two different layers, so round 50 does
not repeat round 49's shape.

`_import_binding_layers` builds `{layer: {name: set(origins)}}` in one
`_walk_scoped` pass -- a *layer* is the module body or one function's
own body, compound blocks and class bodies counting where they stand,
a nested `def`/`lambda` body a layer of its own -- plus the
function-parent map.  `_import_binding_origins(scope)` then resolves
each name through the chain `[scope, parent, ..., None]`: every binding
of the *innermost layer that binds the name* joins the answer as a
union, so shadowing is decided by the reference's layer, not by walk
order.  The placements this closes: a module-level `try`-wrapped import
(b1a, DS boundary#1a), an outer import used inside a closure (b1b, DS
boundary#1b), both same-name shadowing variants -- the nested def's
clean import no longer clobbers the outer dirty binding and the nested
dirty import is no longer missed (b1c, DS boundary#1c/concurrency#1) --
the star import (b2, DS boundary#2/concurrency#3) and the class-body
import (c2, DS concurrency#2).  Star imports are module-level-only in
the CPython grammar (a `from x import *` inside a `def` or class body
is a SyntaxError), so the star traverses the two module-level
placements; the operator star binds the store/display function names
(`operator.__all__` verified: `setitem` in it, no underscore names),
the builtins star binds the unbound type names, and any other source
fails closed the same way (the r39 precedent).

The matrix the battery actually traverses: three import forms
(`import x as y` / `from x import f` / `from x import *`) x four
placements (module top level, a module-level `try`, the function's own
body or a nested def of it, a class body -- in-function and at module
level) x the shadowing pairs (outer clean/inner dirty, outer dirty/
inner clean, both dirty in the alias-vs-alias shape) x operator/builtins
(`probe-r50.py` runs the full 51-cell matrix against the runtime
oracle, `probe-r50-after.log`: 51 cells, 0 mismatches; star
module-level-only per the grammar).

The module-layer walk root (`ast.walk(None)`, pre-existing latent
crash): r49 *already* crashed on every module-level store spelling it
recognized (`_items.append(_hs[0])`, `operator.setitem`, a subscript
store -- with the receiver-establishing call present; the corpus never
trips it, which is why fifty rounds stayed green) while silently
false-PASSing the alias spellings.  The union rewrite routes the
module-layer shapes into the same path, so round 50 fixes the root:
`_name_alias_closure` takes the walk root (module scope spells `None`
in the scoped walk -- the root is the module tree the caller hands in)
and the two scope walks inside the registration path read the same way.
The r49 probe's c2 cell omitted the `md.common.aggregate_status(...)`
call entirely, so its `False` conflated the origins hole with the
missing receiver registration; the round-50 battery rows carry the
call and pin the end-to-end conviction.

`_dict_binding_keys` (DS sibling#1, P1): the in-place merge
`operator.ior(_d, {...})` adds keys the literal binding never saw --
plain, import-aliased and the unbound `dict.__ior__(d, {...})` -- and
the walk's origins are now the union of every layer (an alias bound
inside a function reads the same as the module-level one).  The
from-imported `setitem` writing `md.__dict__` (DS sibling#5, P1) and
the star-imported spelling join the mapping-write family the plain
module spelling already read.  `_sys_modules_write_values` (DS
sibling#4 via boundary#3, P1) keeps the `ior`-merged values: the
`operator.ior(sys.modules, {...})`, import-aliased and receiver-bound
`sys.modules.__ior__({...})` spellings each record their written
values the plain `setitem` spelling already did.  `case [_x] as _w`
(DS sibling#2, P1): a `MatchAs`-wrapped never-matching pattern never
evaluates its guard either -- the wrapper's inner pattern is asked the
same unmatchability question (a bare `case _ as w` wildcard capture
still matches; its pattern is `None`).

Known residuals (follow-up issue; unchanged from round 49 unless
noted): the use-in-Assign-target conservative floor, a `Try`
interrupted mid-body, the not-fork-aware mutator-name conviction,
`extend` of an empty literal and the tuple/str display's `+=` rebind
alias all over-report; stale container history survives a clean rebind;
*untracked or non-literal* dynamic starred/keyword arguments remain
unanalyzable; the guardfall fork stays independent of the match fork;
`_scoped_nodes` class-body walrus, `_name_bindings` for-target
spellings and the For-only `AsyncFor` walks remain; an import binding
a name later rebinds to another object still reads as the module
(position within a layer is not modeled -- the registered
over-approximation, honest spellings pinned by the clean twins); the
`operator.__setitem__`-on-a-receiver spelling in `_mapping_method_call`
keeps its pre-r49 target read and the dunder spellings
(`operator.__setitem__` / `operator.__ior__`) stay outside the
target-first decode of the mapping decoder (registered, kept for the
audit-identical guarantee); the round-50 arbitration probes
(`probe-r50c.log`, run after the round-50 commit) pinned the same
residual on the `sys.modules` consumer side -- the dunder
`operator.__ior__(sys.modules, ...)` / `operator.__setitem__(
sys.modules, ...)` spellings drop the write entirely (the
`_expr_is_sys_modules` gate fails on the `operator`-module target the
pre-r49 read names, before anything records) and a starred
`operator.ior(*_pair)` records nothing, both riding the registered
statements above; the star-imported bare `ior(sys.modules, ...)`
spelling joins them (the module-level star binds the name, the
write-side decode misses it); and the file-wide union below holds only
when the walk root is the module tree -- a consumer handing the
enclosing function node as the tree (`_receiver_attribute_writes` into
`_iterable_dict_keys`) misses the module layer's imports, which is the
root cause the star-side false PASS rides -- all folded into the
follow-up family; `_dict_binding_keys`' origin union is
file-wide by design when the walk root is the module tree
(over-approximation consistent with the walk);
an outer display relaying a `+=`-grown container through a name alias
misses the handle (pre-existing, probe-r49c).

| Shape | Self-test |
|---|---|
| **The scope-model placements register** (false PASS guard): a try-wrapped module import, a module-level star, an outer import used inside a nested def, both same-name shadowing variants and a class-body import (in-function and at module level) storing a handle -- withdraw. | `... lost a receiver behind a try-wrapped module import of operator / a module-level star import of operator / an outer import used inside a nested def / an outer dirty import shadowed by a nested clean import / an outer clean import shadowed by a nested dirty import / a class-body import of operator inside the function (round 50)`; the module rows: `... a module-level class-body import with the use in the class body / a try-wrapped module import with a module-level class-body use / a module-level star import with a module-level class-body use (round 50)` |
| **The placement clean twins stay clean** (false FAIL guard): the same placements storing a clean lambda; the outer-clean/inner-dirty shadow use at the outer layer stays clean (the layer, not the subtree, decides). | `... withdrew a clean receiver behind a try-wrapped module import of a clean store / a module-level star import of a clean store / an outer clean import shadowed by a nested dirty import / a class-body import of a clean store (round 50)`; the module twin: `... a module-level class-body import of a clean store (round 50)` |
| **The `ior` merged keys read unbounded** (false PASS guard): `operator.ior(_d, {...})` (plain and import-aliased) and `dict.__ior__(_d, {...})` merged-key iteration -- withdraw. | `... lost a receiver behind an operator.ior dict merge iterated for keys / an import-aliased operator.ior dict merge iterated for keys / an unbound dict.__ior__ dict merge iterated for keys (round 50)` |
| **The from-imported / star mapping writes register** (false PASS guard): `from operator import setitem as _si; _si(md.__dict__, 'common', ...)` and the star-imported spelling -- withdraw. | `... lost a receiver behind a from-imported setitem mapping write of a handle / a star-imported setitem mapping write of a handle (round 50)` |
| **The `MatchAs`-wrapped never-matching guards no longer launder** (false PASS guard / false FAIL guard): `case [_x] as _w if ...` and `case 0 \| 2 as _w if ...` ride the case arm; the clean fall-through twin stays clean. | `... lost a receiver behind a guard on a MatchAs-wrapped never-matching sequence pattern / a guard on a MatchAs-wrapped all-failing or-pattern (round 50)`; `... withdrew a clean receiver behind a MatchAs-wrapped guard with a clean fall-through (round 50)` |
| **The star mapping write of an untracked key stays clean** (false FAIL guard): `setitem(md.__dict__, 'other', ...)` under a star import. | `... withdrew a clean receiver behind a star-imported setitem mapping write of an untracked key (round 50)` |
| **`sys.modules` keeps the `ior`-merged values** (false PASS guard): `operator.ior(sys.modules, {...})`, its import-aliased spelling and `sys.modules.__ior__({...})`. | `... dropped the written value behind an operator.ior write into sys.modules / an import-aliased operator.ior write into sys.modules / a receiver-bound __ior__ write into sys.modules (round 50)` |
