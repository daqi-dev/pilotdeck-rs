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
`JUDGES`, keyed by the same names. The current table has 238 mutants:
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
`retirement-duplicate-status-key`, `nested-status-slot-bypass`,
`nested-constant-conditional-bypass`, `nested-not-false-conditional-bypass`,
`nested-boolop-value-bypass`, `nested-is-literal-bypass`,
`nested-shared-call-boolop-bypass`,
`calibrate-status-write-is-literal-bypass`,
`calibrate-exit-is-literal-bypass`, `summary-status-shared-call-bypass`,
`summary-unpack-shared-call-bypass`, `summary-name-relay-shared-call-bypass`,
`calibrate-exit-name-is-literal-bypass`, `controls-nested-same-name`,
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
`calibrate-builtins-chain-fiftyone`.
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

The constructs below were measured against the ruler at
`1aad2b626e3db5c27a950489ce7db3beace3312e` (r34) and are **not** detected.  They
are recorded here so they are not re-reported as new.  Repro: copy the tree to a
detached worktree at that commit and run
`BASE_TREE=<tree> python3 -I _tmp/20261106-issue-892/r34-review-probes/probe.py`
(probe + raw output archived with PR #1634).  `rc=0` = the ruler reported PASS,
i.e. missed the write; `rc=1` = caught.  Every injection is wrapped in
`if False:` (the same convention `negative_controls.py` uses), and each case
against the injected tree is a full `--mode aggregation-audit` run.

| ID | Shape **not** covered | Minimal spelling | Probe | rc |
|---|---|---|---|---|
| A | Binding that reaches the write through a **loop back edge**.  `_bindings_at` filters bindings by text line only, so a binding written *after* the call site is invisible on the next iteration. | `dict = builtins.dict`<br>`for i in (0, 1):`<br>`    if i: dict.update({'status': 'PASS'})`<br>`    dict = report` | `A-loop-backedge` | 0 |
| B | **Same physical line**: the call and a later rebind share a line, and only the line number is compared, so the rebind excuses the call. | `dict = report; dict.update({'status': 'PASS'}); dict = builtins.dict` | `B-sameline-restore` | 0 |
| C | Two **sibling alias paths** to the same binding.  The recursive resolvers share one `seen` set across `or`-ed candidates, so the second path to an already-visited binding returns `None`; `found` becomes `{setattr, None}` and `len(found) != 1` drops the write. | `a = setattr`<br>`h = a`<br>`h = a`<br>`h(report, 'status', 'PASS')` | `C-shared-seen` | 0 |
| D | Nested lambda **inside a decorator expression**.  `_walk_scoped` assigns the decorator lambda to the decorated function's scope, so a capture of `md` is resolved against the inner function's unrelated `x = object()`. | `def outer():`<br>`    x = md`<br>`    @((lambda y=x: (lambda f: (setattr(y, 'common', object()), f)[1]))())`<br>`    def inner(): x = object()`<br>`outer()` | `D-decorator-lambda` | 0 |
| E | A `nonlocal` name rebound through a **walrus**.  `_local_bound_names` collects plain assignments only, so `nonlocal common` plus `(common := ...)` is not seen as a write. | `def middle():`<br>`    def inner():`<br>`        nonlocal common`<br>`        (common := types.SimpleNamespace(aggregate_status=lambda *a: ('PASS', 0)))`<br>`    inner()`<br>`middle()` | `E-nonlocal-walrus` | 0 |
| F | A dynamic write handle handed through a **container or a generator**.  `_handle_escapes` inspects the immediate parent node only; a handle stored in a list/tuple/dict or produced by `yield` never reaches a call. | `n = '__setattr__'`<br>`hs = [getattr(md, n)]; hs[0]('common', object())`<br>— or —<br>`hs = {'x': getattr(md, n)}; hs['x']('common', object())`<br>— or —<br>`def handles(): yield getattr(md, n)`<br>`next(handles())('common', object())` | `F1-handle-list`<br>`F2-handle-dict`<br>`F3-handle-yield` | 0<br>0<br>0 |
| G | **`__getattribute__` as the managed attribute name.**  The known-string allowlist covers `__setattr__` / `__setitem__` / `setattr` / `setitem`, but not the read that yields a writable `__dict__`. | `_pdrs_attr_name = '__getattribute__'`<br>`getattr(md, _pdrs_attr_name)('__dict__')['common'] = object()` | `G-getattribute-named` | 0 |
| H | **False FAIL, not a miss**: a starred target is bound to *every* element of the right-hand side, including the ones already consumed by the leading target, so `rest[0]` is treated as `md` and an unrelated object is withdrawn. | `a, *rest = (md, object())`<br>`rest[0].common = object()` | `H-star-overbind-false-fail` | 1 |

Also measured at the same head, tracked in #1954 rather than accepted here (they
are P2-level and narrower than A–H): `from operator import attrgetter as _ag;
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

`#1652` adds the recursive layer the two #892 layers could not see: a summary
face nested inside a returned dict — `{'status': 'BLOCKED', 'face': {'status':
'PASS'}}` — reads clean to the top-level slot scan, because the inner `status`
key never becomes a slot of its own.  The recursive walk collects status/verdict
keys in dict values (including `**` expansions), list/tuple/set elements,
`IfExp` arms and `BoolOp` values inside a returned summary of a registered
point, and judges them by the same provenance rules as the top level: a relay
(`_relays_status` / `_calls_aggregate`) or any arm folding into a fail-closed
literal (`BLOCKED`/`FAIL`/`NOT_RUN`/…) is legal, an unconditional clean literal
is a `nested status slot` bypass.  A *constant* conditional is not a branch
(fix-r2, anthropic boundary P0): `'PASS' if True else 'FAIL'` is judged by the
arm the interpreter can take alone, in both `_exit_arms` and the nested walk —
the dead fail-closed arm no longer excuses the live clean one.  fix-r4 (DS
boundary P0) grew the constant fold to the whole literal grammar —
``not False``, ``1 == 1``, ``True and True`` and ``'x' in 'abc'`` are constant
by the interpreter's own semantics and ``ast.literal_eval`` rejected exactly
those, so each spelling kept its dead fail-closed arm; a test with a runtime
operand (a name, a call, a subscript) keeps both arms, and a pure-literal test
outside the foldable grammar produces *no* certified arm in `_exit_arms`
(fail-closed) while the nested walk keeps both sides visible.  A `Starred`
literal spread (`[*[{'status': 'PASS'}]]`) is walked into its payload (fix-r2,
DS boundary observation); spreading a name stays opaque.  fix-r5 (DS boundary
P0/P1) extended the same interpreter semantics to the value side and the
identity operator: a `BoolOp` *value* follows short-circuit evaluation, so
``'PASS' or 'FAIL'`` certifies only the clean operand (the dead fail-closed
operand never runs — the mirror of the constant-conditional rule), a runtime
operand keeps every later operand reachable, and ``is``/``is not`` never folds
(two freshly parsed AST nodes are never the same object while CPython may
dedup equal constants in one code object — ``1.0 is 1.0`` is `True` at runtime,
measured), so an identity test fails closed with no certified arm; the
arithmetic fold also grew bounds (exponents, shift widths and string repeats
past fixed caps resolve to nothing) so an adversarial literal cannot hang or
OOM the audit.  fix-r6 (DS + MiniMax r5, both families) corrected three
directions of that settlement.  First, the shared-call exemption now follows
the same short-circuit reachability: a `common.aggregate_status` operand the
interpreter can never reach (`'PASS' or common.aggregate_status(…)`) is dead
spelling, not a relay, and may not exempt the nested slot — the exemption is
judged on the exit arms only (`_nested_slot_fail_closed`, MiniMax boundary
P0, probe-verified).  Second, "no certified arm" is *not* "no clean write /
no zero exit": the write, exit-call, return and nested-slot presence scans
all treat an unresolvable identity test as cannot-rule-out, because at
runtime the identity spelling takes the clean arm (DS boundary P1;
`_assigns_clean_status`, `_helper_exits_zero`, `_exit_provenance`, the
unshared-slot element loop).  Third, the collectors walk the same reachable
arms the value fold certifies — `_nested_status_slots`,
`_returned_summary_dicts` and `_returned_names` no longer expand dead BoolOp
operands into nested slots / returned summaries (false red), with an
unresolved test falling back to every operand so the walk can only
over-collect (MiniMax sibling P1).  The arithmetic bounds were re-cut onto
the *folded result's* size (`_FOLD_MAX_BITS` / `_FOLD_MAX_ITEMS`, both
1 000 000): fix-r5 judged each node's own operands, so a nested chain
(`((2**64)**64)**64`) stayed inside every per-node cap while its result grew
without one, and `bytes`/`list`/`tuple` repetition was not bounded at all;
the caps are pre-checked before each op, so an under-cap chain can never
exceed them overall, and a single node whose folded result is under the cap
now computes (`2 ** 100` is a 101-bit int, not a hang) — the fold's
`RecursionError` also resolves to the unresolved sentinel at every level
instead of collapsing the whole audit (MiniMax concurrency P2).  The
string fold (`_folded_string`) grew the same result-size caps on
concatenation and repetition.  Names, calls and
comprehensions stay opaque
to the recursion — a Name is resolved by the write families, a callee by the
delegation registries, and a comprehension body is its own scope; walking them
here would double-judge the shapes those families already own.  Thirteen probes
pin it: a clean status inside a returned face, one inside a list element of a
returned face, one behind a constant condition, one behind each literal-grammar
spelling (``not False`` / a comparison / a boolean chain), one behind an
unresolvable identity test (``1.0 is 1.0``), the short-circuit pair
(``'PASS' or 'FAIL'`` and ``'FAIL' and 'PASS'``), one inside a starred
literal spread, and the legal trio — an `IfExp` face that keeps its fail-closed
arm, a bare fail-closed literal, and a `BoolOp` value with a runtime operand
beside its fail-closed operand.  The `nested-status-slot-bypass`,
`nested-constant-conditional-bypass`, `nested-not-false-conditional-bypass`,
`nested-boolop-value-bypass`, `nested-is-literal-bypass` and
`nested-shared-call-boolop-bypass` mutants
(the retirement report's `cleanupAudit` face hard-written to `PASS`, the
same face written as `'PASS' if True else 'FAIL'`, as
`'PASS' if not False else 'FAIL'`, as `'PASS' or 'FAIL'`, as
`'PASS' if 1.0 is 1.0 else 'FAIL'`, and as
`'PASS' or common.aggregate_status('PASS')`) pin the
detector from the mutation side, with the write/exit identity spellings
pinned by `calibrate-status-write-is-literal-bypass`
(`report['status'] = 'PASS' if 1.0 is 1.0 else 'FAIL'`) and
`calibrate-exit-is-literal-bypass` (`sys.exit(0 if 1.0 is 1.0 else 1)`)
in the same family.  fix-r7 (DS + MiniMax r6, both families, probe-verified)
closed four more gaps in that settlement.  First, the *top-level* summary
slot scan had kept the full-tree `_calls_aggregate` exemption the nested layer
lost in fix-r6, so `{'status': 'PASS' or common.aggregate_status(overall)}`
still passed at the top level; the exemption there is now judged on the exit
arms only, and a `Dict` literal in `**` unpack position is judged recursively
as the summary level it merges into — an arm-based rule alone does not see
the dict literal itself (`summary-status-shared-call-bypass` and
`summary-unpack-shared-call-bypass` pin both spellings).  Second,
`_exit_name_violation` — the fifth `_exit_arms` consumer — treated an empty
arm set as *no violation* where every other consumer fails closed; an
unresolvable test now reads as `conditional` (cannot rule out), and
`calibrate-exit-name-is-literal-bypass` pins the name-relay spelling
(`sys.exit(code)` where `code = 0 if 1.0 is 1.0 else 1`, which never matches
a `Constant` node and so dodged the optimistic-name rule).  Third, the census
row-side pid check compared only `is None`, so a bool or float pid (`True ==
1`, `7.0 == 7`) matched a declared int through `==`; `_retirement_real_pid`
(int and not bool) is now applied to every row pid and to the declared
comparison, and the declared-side `untrustworthy` set projects unhashable
entries through the same hashable wrapper before sorting — an unhashable
declared element used to raise `TypeError` out of the report builder and
collapse the whole 16-face retirement report into a `ROUND_EVIDENCE BLOCKED`
stub (every r6 role block, both families).  Fourth, the executed facet
keyed its rounds by the raw `round` label, so two verdicts sharing one label
silently dropped a round; the facet now deduplicates (`round-01#2`, …) and
the generator test pins it.  The `_folded_string` fold grew the same
wrapper as `_literal_test_value` — any `ArithmeticError`, `TypeError`,
`ValueError`, `MemoryError` or `RecursionError` resolves to nothing instead
of collapsing the audit — plus a width pre-check on `%` formatting, because
`'%2000000000s' % 'x'` *succeeds* at runtime by allocating ~2 GB rather
than raising.

fix-r8 (DS + MiniMax r7, both families, probe-verified) closed the round
after.  First, the relay family — `_name_relays` / `_container_relays` — had
kept the full-tree `_calls_aggregate` exemption that fix-r7 closed for the
direct spellings: a name bound to `'PASS' or common.aggregate_status(…)`
still read as shared-derived and exempted the slot it relayed into
(`summary-name-relay-shared-call-bypass` pins the spelling; a container's
dict binding is now judged by its own status slots, slot by slot, because an
arm split cannot see inside a dict literal).  Second, `_retirement_real_pid`
accepted `0`, negative and oversized ints — `{'rows': [{'pid': 0}],
'declared': [0]}` read as a fully established census — so it now requires
`0 < pid <= 2**31 - 1`, the controls' class-2 row-side and gateway-pid reads
go through the same predicate, and `_retirement_hashable` projects a bool to
its repr so `{True} | {1}` cannot collapse in the control attributions.
Third, the format folds: the fix-r7 width scan ate digit zeros as flag
characters (`'%100000000s'` read width 1), and `_literal_test_value_fold`
had no `%` bound at all (explicit width, `'%*s'` star width and
`'%.500000000e'` precision all materialised the result first); both folds
now accumulate every digit run of each spec into one max, resolve a star
width from the int elements of the tuple, and check the bound before the
interpreter is asked to build anything.  Fourth, int `Add`/`Sub` gained the
result-size cap every other int op already kept — at-cap operands chained
by `+` summed past `_FOLD_MAX_BITS` — and `_literal_test_value`'s wrapper
joined the five-exception family `_folded_string` got in fix-r7 (its
container-literal branches have no `try` of their own).  The in-file
aggregation-audit self-tests pin every one of these shapes, bounded
formats included.

fix-r9 (DS r8, probe-verified) added no mutants — the face-side fixes are
pinned by `test_safety` (probe-status domain, gateway-port domain, the
`/ws` frame-dict requirement, the witness pid ceiling) and the fold fixes
by the in-file aggregation-audit self-tests, both the established r6-r8
precedent.  The fold work: the container branches gained the *total*
running bound (`_folded_size`, judged per element with an early exit, so
`['x' * 999999, …]` piles can no longer sum to n × the cap — measured: a
~1.5 KB source folding to a ~100 MB value peaked ~150 MB RSS pre-fix and
folds to UNRESOLVED at ~51 MB RSS post-fix), the negative-int exponent
comment was corrected to the measured
behavior (CPython resolves `int ** negative-int` as a float power with no
large intermediate; an oversized left raises `OverflowError` into the
fail-closed except — the r8 comment's "raises" claim was wrong, and two
self-tests pin it), bytes `%` formats joined the str bound through
`_format_max_width` (a 1:1 latin-1 read; `b'%100000000s' % b'x'` no longer
materialises), and the duplicated width scan was extracted into that one
shared helper.  The enumeration list above also gained the
`summary-name-relay-shared-call-bypass` row the r8 table add had missed
(DS boundary r8 P3 — count 238 was already right).

fix-r10 (DS r9 P0/P1 + anthropic r9 P2/P3, probe-verified) also added no
mutants — same pinning split.  Two DS findings landed in `_folded_size`:
first, the total bound was still quadratic, because list repetition shares
one folded child object — `([(['x'] * 999999)] * 999999,)` (a ~40-byte
source) put ~10^12 virtual nodes behind ONE element's `_folded_size` call
and the caller's early exit only fires after that call returns (measured
>20 s; the fold path itself >25 s).  The traversal now runs under one
shared work budget (`_FOLD_SIZE_WORK_CAP = _FOLD_MAX_ITEMS * 4` child
visits, one budget list for the whole container loop, so a pile of
zero-size elements — `[''] * 999999` × 5, where the running total never
grows — still aborts); an exhausted budget raises `_FoldedSizeOverflow`
inside, returns `None`, and the callers read that as an
`_LITERAL_TEST_UNRESOLVED` verdict.  Second, ints counted "1 each" in
containers while a legal single int is `1 << 999999` — 1200 literal
elements of it (~15.6 KB of
source) folded to ~200 MB RSS with the total reading only 1200.  Ints now
count `max(1, bit_length())` bits (bools floor to 1; the arithmetic fold
already used that convention), so a single at-cap int still folds.  The
aggregation-audit self-tests pin the bomb shape, the int pile, the
zero-size pile, and the still-folding controls.  anthropic r9's boundary
P2 (the port-domain matrix pinned 0/True/70000 but not the 65536 boundary
itself) joined `test_safety`'s two matrices, and its sibling P3 wording
(the r9 README/docstring mixed "~100 KB" sources and mismatched RSS
numbers) is unified to the measured source/fold/RSS triple above.

fix-r11 (DS r10 concurrency P0 + sibling/boundary P1 — one mechanism,
three roles; probe-verified) again added no mutants.  The r10 work budget
was still drawn *per container literal*: each nesting level re-walked the
same shared folded child on a fresh allowance, so
`'[' * 198 + "[''] * 999999" + ']' * 198` (621 bytes of source) folded
for a measured 17.2 s and twelve such tests ran 197.5 s — past the 180 s
`CHECK_TIMEOUT` (the CPython parser caps nesting at ~199, so one
expression caps at ~17 s, but a file of them scales linearly).  One budget
now belongs to the whole top-level fold: the fold creates it when the
wrapper passes `None` and threads it through every recursive call, so a
nesting level re-walking the shared child draws from the same allowance
and the depth shape aborts (measured post-fix: 0.27 s per shape, 12 ×
d=198 in 3.3 s).  The self-tests pin the depth shape (seven
single-element wrappers → `UNRESOLVED`), the deep-small control (nesting
depth alone never trips the budget), the five-entry dict pile, the
at-cap dict control, and — after the first variant run showed a
per-dict-literal budget *surviving* the pile pin (the enclosing
sequence's size walk re-charges the dict anyway) — the cumulative shape
`([''] * 999999, {'a': [''] * 850000, 'b': [''] * 850000})`: a nested
dict is walked twice on one allowance (per entry while folding, then as
the enclosing element's size), so ~4.4e6 visits fail closed while a dict
that re-drew folds on ~2.7e6.  The int pile got its own pin message
because its interception is the *total* bound, not the budget.  DS r10's
sibling/boundary P2s joined `test_safety` as test-only pins over the
unchanged production domains (65535 top-valid on both port-matrix paths,
census and witness pid 2**31/2**31-1), and the P3 comment fixes reworded
the budget's docstring scope and the `/ws` hit predicate's wire-outcome
condition — no behavior to mutate.
fix-r12 (DS r11 concurrency P1 + P2 — the pass budget, probe-verified)
again added no mutants.  The r10/r11 budgets still belonged to *one
top-level fold*: `_exit_arms` starts a fresh fold per IfExp test and per
BoolOp operand (and `_helper_exits_zero` folds up to three summary values),
so per-fold caps bounded nothing at the file level — every fold re-drew a
fresh 4M allowance and ~778 of them (~11 KB of crafted source, one measured
at 0.2314 s) ran the 180 s `CHECK_TIMEOUT` down.  One shared
`_AUDIT_PASS_BUDGET` cell (`_AUDIT_PASS_WORK_CAP = 4 × _FOLD_SIZE_WORK_CAP`,
comfortably above one legal at-cap fold plus a file's ordinary audit work)
now belongs to each audited file in the scan loop; while it is active the
fold mint reuses it instead of drawing fresh, and an exhausted cell resolves
`_LITERAL_TEST_UNRESOLVED` — the arm callers read that fail-closed (an
empty arm set excuses nothing).  The BoolOp value fold also became lazy in
the same round: it folds operands in order and stops at the settling one,
the mirror of the interpreter's own short-circuit (the eager form folded
and retained ~292 MB of folded strings from a 5.4 KB source before the
reduce), while an unresolvable operand *before* the settling one still
rejects the whole value.  The self-tests pin the pass budget (a later fold
on a shared reduced allowance fails closed), the short-circuit, the
pre-settle rejection, and the file-loop wiring (`audit_pass_files == 0` is
itself a failure); four revert mutants (eager BoolOp, mint ignoring the
cell, activation removed, UNRESOLVED check removed) are killed by the named
pins.

fix-r13 (DS r12 sibling#1 + concurrency#1 P1, both roles independently;
boundary#2 materialisation P1 + sibling#2 string-fold P2 same family)
extended the pass budget to the two faces r12 left unbilled.  First, the
cell's scope ended with the file loop, but the registered-point loop after
it (`unshared_status_slots`) folds return leaves through `_exit_arms` too —
each on a freshly minted 4M, so a registered point carrying ~200
``return <pile> and 0`` leaves (8 KB of source, one leaf measured at
0.9125 s) re-attained the 180 s face.  The points phase now draws its own
cell (pinned by the `audit_pass_tail` wiring check), `aggregation_audit`
became a thin ``try/finally`` wrapper so an exception escaping the scan can
no longer leak a half-spent module-level cell, and a non-UTF-8 file in the
scan directory resolves to a recorded failure instead of an escaped
`UnicodeDecodeError`.  Second, nothing debited the *interpreter's* copy
work: a BinOp materialisation (`[''] * 999999` builds an ~8 MB list before
any cap is consulted) was free while the pass cell stayed full, so ~300k
materialising operands (~3.3 MB of source) timed out on copies alone.  The
three materialising families (sequence×int repetition either way, sequence
+ sequence, ``%`` formatting) are now charged in the same units the
container branches pay, before the op runs; int results stay on the bit
caps, which already bound them by result size.  Third, `_folded_string`
(the other half of the shared-implementation pair with the value fold)
mints from the same cell and charges its quadratic ``+`` chain.  The
self-tests pin the materialisation (second 1e6-unit BinOp on a reduced
allowance fails closed), the string fold (second 1e6-char repetition
returns None), and the points wiring; four revert mutants (billing
removed, points activation removed, string-fold budget removed, and the
production `<= 599` status bound relaxed to `<= 600`) are killed by the
named pins and the `test_safety` 600-as-NO_HIT pin that joined the same
round.
fix-r14 (DS r13 boundary#1 = concurrency#1 P0, both roles independently,
probe-verified) closed the two-fold reading of the string fold's None.
The budget-exhaustion branches and the five-exception wrapper now return
the module sentinel `_FOLD_STRING_UNRESOLVED`, propagated by the BinOp
recursion *before* the operand guards (a stopped operand must not fall
into the "is not a constant literal" face, and `len()` on the sentinel
would raise into the wrapper and collapse the stop back into the same
fail-open None); the interpreter's own `%` failure still returns None —
an expression that crashes at runtime can never write a status.  The
three consumers (`_status_key`, `_status_literals`, `_assigns_clean_status`)
resolve the sentinel conservatively (a status key, a status literal, a
possible clean write): over-collecting flags the function, the fail-closed
direction.  Self-tests added: the unit polarity pin (under a spent cell
`'sta' + 'tus'` and `'PA' + 'SS'` read as status evidence by all three
consumers), the end-to-end padding pin (17 `report['x' * 999999] = 1`
pads, then a rogue unregistered function writing `report['sta' + 'tus'] =
'PA' + 'SS'` must still enter `found`), the propagation pin (a stopped
Mod must surface as the sentinel, not as an audit crash), and per-branch
billing pins for the string Add/Mod and value Add/Mod families the r13
Mult pin did not cover; the file-loop and points wiring lines now bind the
cell by identity, so an `= None` that keeps the activation counter is a
red.  Fourteen revert mutants (consumer reverts, propagation removal,
per-branch exhaustion-to-None, both wiring downgrades, billing deletions,
and the `session_domain.py` floor relaxed to 99) are killed by the named
pins and the `test_safety` 99-as-NO_HIT pin that joined the same round.
fix-r15 (DS r14 concurrency#1 P0, probe-verified) closed the last
fail-open face of the string fold: the Mod size-cap rejection was
returning None on an *over-estimated* bound — a precision conversion
(`'%.6s'`) truncates at runtime, so a 1e6-char operand can produce the
exact word `'status'` while the fold refuses to compute it, and the
consumers read that refusal as negative evidence with no padding needed.
The Mod rejection now stops the fold as the sentinel
`_FOLD_STRING_UNRESOLVED` (the same fail-closed polarity as budget
exhaustion); the Add and Mult size rejections keep their plain None —
their bounds are exact, so the result is provably too long to equal any
status word.  The three consumers' sentinel polarity now lives in one
place, `_folded_status_hit` (whose docstring pins the one consumer that
must NOT use it: `_nested_slot_fail_closed` keeps the bare `in`, where
False is the safe direction).  The end-to-end padding pin now derives
its pad count from the cap family (`_AUDIT_PASS_WORK_CAP //
(_FOLD_MAX_ITEMS - 1) + 1`) instead of a hand-written 17 and asserts the
cell was actually spent, so a raised cap turns a vacuous pass into a
red.  Self-tests added: the unit face (an oversized `'%.6s'` operand
folds to the sentinel while a small one still folds to the exact word)
and two end-to-end rogues (a precision-truncated status key and status
value must each enter `found`); the oversized-format pin moved its
expectation to the sentinel.  `controls/session_domain.py`'s single-case
handler now records a MemoryError as a verdict.json FAIL (pinned
statically in test_safety — #166 blocks executing a mutated registered
file, so the revert mutant is killed by reading the source).  Five
revert mutants (the Mod rejection back to None, the oversized-format
pin's expectation reverted, the helper's sentinel arm removed, the
padding count without the +1, and the controls MemoryError dropped) are
killed by the named pins.

fix-r16 (anthropic r15 boundary#1 P0, probe-verified) closed the
one-level-down escape of the string fold: the Add and Mult size
rejections return None on *exact* bounds, which is sound only while the
oversized result is consumed directly.  Under an outer `%` a precision
conversion truncates, so "too long" carries no information about the
formatted result and the oversized point only has to move one level
down: a one-over Add or Mult on the right (`'%.6s' % ('sta' + 'tus' +
'x' * 999995)` is `'status'` at runtime), a zero-padded precision spec
that keeps the format string oversized while the conversion stays short
(the left operand), or a constant tuple feeding precision-0 conversions
that emit nothing — every one a fully constant expression whose refusal
used to read as negative evidence.  Any operand the fold cannot resolve
under `%` now stops the fold as the sentinel
`_FOLD_STRING_UNRESOLVED` (the same fail-closed polarity as budget
exhaustion and the r15 size refusal).  A JoinedStr conversion with a
format spec on a constant value (`f'{"statusxx":.6}'` is `'status'` at
runtime) stops the fold as the sentinel too, instead of the fail-open
None.  Self-tests added: the six-shape unit pin (any unresolvable-`%`
shape folding to anything but the sentinel is a red), the six-rogue
end-to-end pin (one scan, one function per shape, each runtime
status/PASS write must enter `found`), and the polarity pin
(`_nested_slot_fail_closed` keeps the bare `in` on purpose — a stopped
fold must not exempt a nested status slot there, so a tidying pass that
swaps it for `_folded_status_hit` turns the `[0]`-budget cell probe
red).  `controls/session_domain.py`'s single-case handler now records
a RecursionError as a verdict.json FAIL alongside the fix-r15
MemoryError (pinned statically in test_safety).  Six revert mutants
(the Mod guard weakened to both-None, the JoinedStr sentinel disabled,
the shapes pin's expectation reverted, the writes pin's direction
reverted, the bare `in` swapped for the helper, and the controls
RecursionError dropped) are killed by the named pins.

fix-r17 (DS boundary#1 + sibling#1 r16 P0/P1, two roles, probe-verified
— `fix-r17-audit/probe-r17-p0.out` S1/S2/S3): the r16 sentinel only
covered bare `ast.Constant` interpolation values, so a foldable constant
expression inside an f-string was still the fail-open None —
`f'{"sta" + "tus"}'` is `'status'` at runtime, its `:.6`-spec form moves
the r16 truncation point one level down, and an outer f-string dropped
the inner `%` sentinel exactly the way the outer `%` did before r16; a
fourth shape was self-found while probing (`f'{"status"!r}'` folded the
untransformed value while the runtime is `"'status'"`).  The
`FormattedValue` branch now folds the interpolated expression
recursively (str joins, sentinel propagates, None stays genuinely
runtime) and stops the fold as the sentinel under any format spec or
repr/ascii conversion.  Pins: the positive fold pin
(`_fsr3_fold` must reach `'status'`), the three-shape sentinel pin, and
the three-rogue end-to-end enumeration pin.  The expectation/native
`run_control.py` runners' catch sets regained `MemoryError` +
`RecursionError` (they had drifted from the controls entry twice) and a
new test_safety static pin (`test_fixture_runner_catch_sets_match_controls`)
keeps all three files in lockstep.  Seven revert mutants (the recursion
reverted, the conversion guard dropped, the fold pin's expectation
reverted, the writes pin's direction reverted, the sentinel propagation
swallowed, and both shims' RecursionError dropped) are killed by the
named pins.

fix-r18 (DS boundary#1 + sibling#1; MM boundary#1 + concurrency#1 r17
P0/P1, two families, four roles, probe-verified —
`fix-r18-audit/probe-r18-p0.out` F1-F6): the fold grammar returned the
fail-open None for every node outside Constant/JoinedStr/BinOp, so a
constant key built with a constant IfExp/BoolOp (`f'{"status" if True
else "x"}'`, `'status' or "x"`) read as "not a status key" and the rogue
write escaped enumeration; the key side also lacked the value side's
`ast.walk` constant fallback, so the runtime-grammar Subscript shapes
(`'status'[:6]`) escaped too.  Two layers: the fold's final return now
stops as the sentinel for non-runtime IfExp/BoolOp (the only
string-producing operators outside the grammar — a first draft folded
every non-runtime grammar miss into the sentinel and failed the healthy
controls `verdict = {}` baseline, so non-string shapes keep the None
miss); `_status_key` gains the mirrored `ast.walk` constant fallback
with the `**spread` None key kept a miss.  Pins: `_fsr4_shapes` (four
constant shapes must stop the fold), `_fsr4_keys` (two Subscript shapes
must read as status keys), `_fsr4_writes` (six rogues must be
enumerated, the genuinely-runtime `report[name]` must stay out).  The
test_safety lockstep pin now compares full catch sets (members read via
`ast.unparse`) instead of MemoryError/RecursionError membership.  Ten
revert mutants (both layers, four pin directions, the set-equality pin
direction, and one dropped member per catch-set file) are killed by the
named pins.

fix-r19 (DS r18 boundary#1 P0 + sibling#1 P2, probe-verified —
`fix-r19-audit/probe-r19-p0.out` G1-G7): the r18 branch judged a whole
IfExp/BoolOp subtree with `_test_is_runtime`, so a dead-arm Name or a
runtime test turned a conditional that IS one constant string
(`('sta' + 'tus') if True else name`, `('sta' + 'tus') or name`) back
into the fail-open None — a folded status word escaped both the key and
the value side — while an all-constant NON-string conditional
(`{} if True else {}`) read the sentinel and could flag a healthy
function.  Two halves: the fold now resolves per reachable arm
(`_exit_arms` — a constant test keeps the taken arm, a settled BoolOp
stops at its settling operand; a single arm folds as itself, an
unresolvable constant test keeps the sentinel, a runtime branch keeps
None); and a new word-aware `_conditional_status_word` makes
`_status_key` / `_status_literals` treat a runtime conditional whose
reachable arm folds to a status word (or stops the fold) as a hit —
folded non-status words and genuinely runtime arms stay misses.  The
`_fsr4_shapes` pin now demands the exact 'status' fold, and new
`_fsr5_*` pins cover the fold faces, key/value faces with negative
controls, and seven end-to-end rogues with three negative controls.
Ten revert mutants are killed by the named pins.

fix-r20 (DS r19 boundary#1 P0 + sibling#1/#2 P1 + sibling#3 P2(b) +
simplify#1 P2(a), probe-verified — `fix-r20-audit/probe-r20-p0.out`
PB1/PB2/PB3/S1/S2K/S2V/S2F; the anthropic r19 boundary#1 P0 claim was
arbitrated by a four-case probe and refuted): `_exit_arms` flattening
DROPPED a sub-expression whose constant-but-unresolvable test
(`1.0 is 1.0`) certifies no arm — the surviving arms read as the
complete answer while the dropped branch could still carry the runtime
value (PB1 even folded the deterministic WRONG string 'x'; the miss was
non-monotonic: nesting B's runtime value set is a superset of control
A's, yet detection went True→False).  The fix is deliberately
consumer-side: a new `_exit_uncertain(node)` companion walk is consulted
only by the two consumers that must not trust a partial arm set —
`_folded_string_fold` answers the sentinel (never a wrong string) and
`_assigns_clean_status` fails closed — because making `_exit_arms`
itself propagate the empty set upward would LOSE the surviving arms'
detection for the 15+ presence-scan consumers (`_calls_aggregate` and
friends read [] as "certifies nothing").  `_assigns_clean_status` also
gains the word-aware arm rule (the third folded-word consumer: the
write path may not disagree with the return path), and
`_conditional_status_word` gains a per-operand BinOp branch (Subscript
deliberately not a recursion face — the walk fallback's literal-slice
over-collection is the r18-established rule); the key and value side now
share one `_spelling_hit` three-step pipeline.  Pins `_fsr5b_*` (fold
sentinel, key/value faces with negative controls, fail-closed
over-collection, the write-policy faces, six end-to-end rogues with one
negative); a first S3 pin draft did not isolate the arm-loop polarity
(mutant M7 survived through the BinOp branch's own operand fold), the
f-string sentinel-interpolation arm is the isolating face.  Twelve
revert mutants are killed (`fix-r20-audit/mutneg-r20-variants.out`).

fix-r21 (DS r20 concurrency#1, runtime-arbitrated to P0 under the
reviewer's pre-registered escalation clause — probe-verified,
`fix-r21-audit/probe-r21-p0.out` + `e2e-recursion-{before,after}.out`): a
no-paren conditional chain (`'PASS' if md else 'PASS' if md else … 'FAIL'`)
has no tokenizer bracket limit to stop it — the parser accepts depth 5000+
while the flattening walks' frame budgets end far earlier (`_exit_arms`
around 990, `_exit_uncertain` from 499, one generator frame per level) — so
a registered point whose status write nests that deep crashed the whole
audit: the RecursionError escaped the parse point's
`(OSError, SyntaxError, UnicodeError)` tuple and the run collapsed into a
single control_failure line, losing every other point's verdict.  The fix
mirrors the `_folded_string` wrapper polarity on three surfaces:
`_assigns_clean_status` guards both its walks (a walk that cannot finish
certifies nothing about absence — the same fail-closed True as an empty arm
set), the points loop wraps its analysis calls in one boundary (the affected
point is recorded with `unsharedSlots: ['recursion-depth-exceeded']` and the
table keeps every other point's verdict), and the parse tuple gains
RecursionError (defense in depth: no no-bracket parse shape raises it on
this interpreter, `parse-shapes-probe.out`).  Pins `_fsr6_depth` (600
isolates the `_exit_uncertain` guard, 1200 the `_exit_arms` guard — both
must answer True, never raise); four revert mutants are killed, plus an
injection-only control that must keep the fail-closed line and lose it only
under the mutant (`mutneg-r21-variants.out`).

fix-r22 (DS r21 boundary#1/concurrency#1/#2/sibling#1 — all five
non-P3 findings runtime-arbitrated and confirmed, probe-verified,
`fix-r22-audit/probe-r22-arb.out` + `probe-r22-e2e.out`): a
resolvable-constant outer shell (`('x' if flag else ('sta'+'tus') if
(1.0 is 1.0) else 'y')) if 1 else 'z'`) re-opened the r19/r20
partial-arm-set escape — `_exit_uncertain` answered False for the
outer test without recursing into the taken arm while `_exit_arms`
flattened into it, so both the status-key face and the clean-status
value face escaped enumeration.  Four violation consumers
(`_exit_name_violation`, `_exit_provenance`, `_helper_exits_zero`,
`unshared_status_slots` return elements) likewise read a surviving
partial arm set as the complete answer.  The scan stage
(`scan_aggregation_tree` recursion family) had no boundary at all —
a 1200-deep no-paren chain in a return position collapsed the whole
audit into one RecursionError line — and the two parse tuples let a
60k-unary chain's `MemoryError('Parser stack overflowed')` through to
the same collapse.  The fix recurses the taken/orelse arm in
`_exit_uncertain`'s non-runtime branch, wires all four consumers to
consult it (RecursionError fails closed the same way), wraps the
per-file scan block in a `(RecursionError, MemoryError)` boundary
(the file is recorded `cannot scan … (scan stage)` and skipped), and
adds `MemoryError` to both parse tuples (`RecursionError` also joins
the proven-definitions arm).  The write path's hand-mirrored
three-step spelling pipeline is extracted into `_walk_word_hit`
(simplify#1).  The `_fsr6` raised-recursionlimit isolation gap
(concurrency#3) is registered, not changed: the reviewer-suggested
'FAIL'-arm shape measures as a false red on healthy trees under
`setrecursionlimit(20000)` (`probe-r22-fsr6.out`), so the 'PASS' arms
stay and the trade-off is documented in-repo.  New `_fsr7` pins cover
the constant-shell key/value/plain faces, the four consumer wirings,
and the rogue enumeration.  Ten revert mutants are killed
(`fix-r22-audit/mutneg-r22-variants.out`), including the scan
boundary and the parse tuple arms with injected killer shapes.

fix-r23 (DS boundary r22 P0, probe-verified): the routes-through-the-map
exemption in `_helper_exits_zero` ran as a whole-tree scan before the
empty-arm and uncertainty checks, so a helper whose return merely
MENTIONED the shared aggregate in an arm the flattener drops
(``(1.0 is 1.0) and (0 if flag else common.aggregate_status(rows)[0])``)
was exempted while the runtime always took the clean literal.  The
exemption is now judged on the exit arms only — the fix-r7 semantics
the slot scan already uses — and only after the empty/partial set has
failed closed.  `ValueError` joins both parse tuples (NUL-byte sources
report as SyntaxError on 3.12+ and as ValueError on <=3.11; both now
answer with the same per-file line).  New `_fsr8` pins cover both top
shapes, the partial-arm mention, and the live/mixed no-over-collection
faces; revert mutants are killed (`mutneg-r23.out`).

fix-r24 (DS r23 boundary#1 P0 + sibling#1/#2 P1 + P2x4, probe-verified): the
r23 any-arm exemption polarity exempted a whole return whenever ANY reachable
arm mentioned the shared aggregate, so a mixed arm set (`0 if flag else
common.aggregate_status(rows)[0]`, flattening to `[0, agg[0]]`) carried its
clean constant arm past the zero-exit judgement.  The exemption is now
per-arm: `judged = [arm for arm in arms if not _inevitably_shared(arm)]`,
with the new `_inevitably_shared` predicate True only for the shared call
itself, a Subscript whose `.value` recursively routes through it, or an
IfExp/BoolOp whose certified arms (`_exit_arms`, empty set False) all route
through the map -- dead operands, dead arms and constant shells keep being
judged.  The `_exit_argument_violation` exemption is judged on the exit
arms only (`status_exempt`, RecursionError -> False fail-closed; it was a
whole-argument mention scan before -- a dead operand `'PASS' or <shared
call>` suppressed the verdict); three mention-based checks (the
`_proven_callee` proof and its two siblings) now require `_is_shared_call(call)`; the `_exit_arms` call inside
`_helper_exits_zero` is wrapped RecursionError -> fail-closed True
(function-level kill only: the r22 scan-stage boundary fires first at every
depth that would reach it, `probe-r24-m19-depth{,2}.out` swept 300..1200);
both parse sites share the `_PARSE_FAILURES` constant.  New `_fsr8` pins
(six: five positive, one live-relay negative) and `_fsr9` pins (seven) cover
every face; revert mutants are killed (`mutneg-r24.out`, first-run 14/18
record `mutneg-r24-run1.out` kept alongside).

fix-r25 (DS r24 boundary#1 P0, human verdict 2026-11-09 — structural
direction (a), single-direction polarity): the shared-arm-set exemption
is now fail-closed end to end.  `_inevitably_shared` consults
`_exit_uncertain` before the surviving arms (`_exit_arms` silently
drops branches whose pure-literal test it cannot resolve), so a
PARTIAL arm set certifies no more than the empty one; the relay family
(slot exemption, exit-argument status exemption, name/container relay
trust) moved onto one predicate, `_armwise_shared`, which requires
every certified arm to relay a verdict or inevitably route through the
map — one shared arm no longer excuses `{'status': 'PASS' if flag else
<shared>[0]}` and its name-bound twin; `_helper_exits_zero` returns
True when a judged arm is an attribute/subscript composite (the
point-level canon already fails closed on that spelling — one source
may not keep two verdicts through a helper hop); `_delegated_exit`
gains the `_is_shared_call` guard its module-level twin has carried
since #892 (a helper returning the aggregated value itself is the
proof, not a delegation to judge); the remaining `_exit_arms` call
sites gained RecursionError wraps (function-level kill via
`wrap-driver.py`; the element/BoolOp wraps are registered-not-e2e —
`_nested_status_slots` raises first at every reachable depth, same
call as r24 M19).  Twelve new pins (two `_fsr8`, ten `_fsr9`)
including the two container-relay sites; the `_fsr9` count above is
corrected 7 -> 8 (`_fsr9_shared_summary`, r24 simplify#4) -> 18 this
round.  Revert mutants are killed (`mutneg-r25.out` 31/31; the
first-run 30/31 record `mutneg-r25-run1.out` is kept alongside — the
M15 anchor matched mid-indent and its mutant died with
IndentationError, the same harness-error class the r24 first run
had).  Evidence `_tmp/20261109-1355-r25/` (matrix 80 cells
0 RED after, baseline 23 RED before — 负控先证红; r24 probe
F1-F5 0 of 7 faces confirmed; real-tree aggregation-audit
self-check PASS with verdicts byte-identical to the r24 head).

fix-r26 (dual-lineage review of the r25 head, P0x1 + P1x4 + P2x2,
probe-verified): the single-direction polarity migration is complete.
`_nested_slot_fail_closed` — the one exemption consumer the r25 verdict
(a) never reached — is now the canonical `_armwise_shared` predicate with
the nested layer's fail-closed literal arm kept (a shared arm certifies
only itself; an empty/partial/un-walkable arm set certifies nothing), so
`{'cleanupAudit': {'status': 'PASS' if flag else <shared>[0]}}` can no
longer keep a second verdict its top-level twin lost.  The delegation
family's guards judge with the caller's own file-trust view:
`_function_shared_view(function, context)` threads the points loop's
per-file `(shared, rel, loaders, proven, poisoned)` context through
`_delegated_exit`/`_module_delegated_exit`/`_helper_exits_zero`/
`_exit_name_violation`/`_exit_provenance`, so a helper that locally
rebinds `common` is not the shared map (the r24 fail-closed verdict
returned) while a registered proven loader rebinding keeps its shared
call — the healthy `negative_controls.main` shape stays green (the
first cut of this fix phantom-flagged it; the control is pinned by the
real-tree byte-compare).  The `_helper_exits_zero` composite-arm check
no longer trusts a status face it cannot see into (a helper returning
`common.BATCH_EXIT_CODES['PASS']` reads exactly like its inline twin),
and the `_exit_name_violation` name hop carries the same per-arm
provenance judgement the helper hop got in r25 (`code =
helpers.EXIT_OK if flag else 1; sys.exit(code)` no longer falls
through).  The recursion family fails closed everywhere it recurses:
the `_armwise_shared` tail and `_inevitably_shared`'s subscript and
IfExp/BoolOp folds wrap (a deep legal chain certifies nothing instead
of raising — the r25 :1876 registration described this wrap but the
code never carried it), `seen` is isolated per sibling arm (a twin-arm
relay is not "already traced"), and `_returned_summary_dicts` /
`_nested_status_slots` over-collect on un-walkable chains (their
ownership stays the registered residual band — measured on this
interpreter the collector walk is never shallower than `_exit_arms`
for the same tree, so their kill point is the :7790 point-level
boundary, e2e-proven at 900-deep; at 1200-deep the parser's own
family raises first and the file-level boundary fires).  The
`_status_literals or _has_passed_aggregate` pair lives once as
`_status_face` (seven consumer sites), a machine check (AST-based —
the first-cut regex fired on the fix-r24/r25 comments that describe
the retired any-arm shape) fails the audit if this source ever regains
an any-arm `_inevitably_shared` exemption consumer, and the
`_fsr9_dm_helper` fixture is renamed `_r26_dm_helper` so the pin
count below greps clean.  Six new `_fsr9` pins (nested mixed/partial,
rebound delegation, helper status face, name-hop composite, sibling
seen), the `_r26_deep_faces` recursion pin, and the
`_aggregate_status_slot` enumeration pin (the pair's aggregate half
was a mutneg survivor before it); the `_fsr9` count above is corrected
18 -> 24 this round.  Revert mutants are killed (`mutneg-r26.out`
12/13; M13 — the helper-scope context threading — is a registered
survivor: its judgement is the module-level guard's, killed by M02,
and no non-fabricated fixture-level face exists for it).  Evidence
`_tmp/20261026-issue-1355/fix-r26/` (rebuilt on this tree — the r25
evidence directory does not exist here; matrix 56 cells 0 RED after,
baseline 9 RED before — 负控先证红; probes 21 with 12 red on head, 0
control-red, 0 unexpected after; real-tree aggregation-audit
self-check PASS with verdicts byte-identical to the r25 head;
test_safety 158/158; check_frozen --base dcc110f5f PASS 30).

fix-r27 (anthropic r26-c boundary#1 P0, probe-verified before/after —
`_tmp/20261110-1355-r26-harvest/probe-p0-mult.out`): the Mult fold
accepted only ``str * Constant-int`` — the commuted ``1 * 'tus'`` and
the constant-expression counts (``'tus' * +1``, ``'tus' * (2 - 1)``)
fell outside that grammar into the fail-open None, and
``report['sta' + 1 * 'tus'] = 'PASS'`` escaped the status-write
enumeration on both the key and the value side (``'PA' + 1 * 'SS'`` the
same) — the one-value-two-meanings gap r14 closed for budget exhaustion
and r18 for the grammar, on the multiplication face.  The fold is
symmetric now (whichever side folded to a string is the sequence) and
the count is judged with `_literal_test_value` threaded on the same
budget: a runtime count operand keeps the plain None of the contract
(r19: the runtime half is word-aware and lives in
`_conditional_status_word`); a constant non-int count makes the product
raise at runtime and keeps the plain None; a pure-literal count the fold
cannot resolve stops as the r14 sentinel, never the fail-open None.
New `_fsr10` pins: three fold shapes (all must fold to 'status'),
three polarity pins (runtime None / non-int None / unresolved
sentinel), and four e2e rogues (three key-side, one value-side) that
must all be enumerated.  The revert mutant is killed
(`mutneg-r27.out` M01 back to the old one-sided grammar, pin-bound,
battery fires on the real-tree audit; fixture restored sha-verified).
Real-tree aggregation-audit self-check PASS with verdicts
byte-identical to the r26 merged head (`agg-audit-r27.out`);
test_safety 162/162; three `check_frozen --base origin/main` suites
PASS unchanged (no registered file touched, `--write` a no-op).
Evidence `_tmp/20261110-1355-r26-harvest/` (probe-p0-mult.out,
probe-p0-mult-after.out, mutneg-r27.out, agg-audit-r27.out,
test-safety-r27.tail.out).

fix-r28 (anthropic r27-b sibling#1 P1 + DS r27-d boundary#1 P0, both
probe-verified before/after — `_tmp/20261110-1355-r26-harvest/
probe-p1-fstring.out`, `probe-p0-subscript.out`): two remaining faces
of the same one-value-two-meanings family.  (1) The r17
FormattedValue recursion folded string expressions, but a constant
*scalar* interpolation outside that grammar (`f'{(114+1):c}tatus'`,
`f'{+115:c}tatus'`) landed in the fail-open None while the runtime key
is exactly `'status'` (format(115, 'c') == 's') — the interpolation is
judged with `_literal_test_value` now: a runtime operand keeps the
plain None (r19), an unresolvable pure-literal tree is the r14
sentinel, a scalar constant joins as `str(value)` (exact under no
spec; the spec/conversion check still stops that as the sentinel), and
a folded container — constant but whose `str()` order is not
certifiable across processes (set/dict iteration) — stops as the
sentinel too.  (2) The r27 Mult-count gate and the new interpolation
gate both ran `_test_is_runtime` BEFORE the literal fold, and
`ast.Subscript` is runtime grammar there, so a constant-semantics count
(`'tus' * (1, 1)[0]`, `[1][0]`) took the fail-open None while
`report['sta' + 'tus' * (1, 1)[0]]` wrote the exact `'status'` — the
gates use `_RUNTIME_TEST_NODES_STRICT` (Subscript/Slice removed) now,
and the literal fold fail-closes the subscript shapes it does not fold
to the sentinel; a subscript on a Name base still keeps the plain None.
New `_fsr11`/`_fsr12` pins: fold faces (three f-string shapes, three
subscript shapes), polarity pins (runtime None / unresolved sentinel /
container sentinel / subscript-on-Name None), and seven e2e rogues
that must all be enumerated.  The revert mutants are killed
(`mutneg-r28.out` M01 back to the fail-open interpolation, M02 back to
the `_test_is_runtime` gate; both pin-bound, battery fires on the
real-tree audit; fixture restored sha-verified).  Real-tree
aggregation-audit self-check PASS with verdicts byte-identical to the
r26 merged head (`agg-audit-r28.out`, `agg-audit-r28-b.out`);
test_safety 162/162; three `check_frozen --base origin/main` suites
PASS unchanged (no registered file touched, `--write` a no-op).
Evidence `_tmp/20261110-1355-r26-harvest/` (probe-p1-fstring.out,
probe-p1-fstring-after.out, probe-p0-subscript.out,
probe-p0-subscript-after.out, mutneg-r28.out, agg-audit-r28*.out,
test-safety-r28.out).
fix-r29 (anthropic r28 boundary#1 P0 + DS r28-b boundary#1 P0, sibling#1/#2
P1, simplify#2 P2-as-P1, sibling#4 P2 — all probe-verified before/after in
`_tmp/20261110-1355-r26-harvest/`): the third face of the
one-value-two-meanings family, plus the pid booleans on the witness side.
(1) One set, no strict copy: the r28 gates kept `_RUNTIME_TEST_NODES_STRICT`
as a private copy of the state-dependent set minus Subscript/Slice, so a node
added to the main set (fix-r28's own `NamedExpr`-style growth) was missing
from the gate copy and the new grammar landed in the fail-open None
(`probe-p0-strictset.out`). The gates and `_test_is_runtime` now read the one
`_RUNTIME_TEST_NODES` set. (2) The fold tail: JoinedStr/FormattedValue/
Starred/Subscript/Slice values are decided by their subtree — when no
state-dependent node appears anywhere in the expression the value is
constant-semantics, so the fold's final gate hands it to
`_literal_test_value` and fail-closes what it cannot fold to the r14
sentinel (`'sta' + 'tus'[:3]` writes the exact `'status'`, `probe-p1-foldtail.out`);
a subtree with a Name (or any state-dependent node) keeps the plain None of
the runtime contract. (3) The conditional classification: a dead branch
guarded by a constant-semantics condition (`'FAIL' if (1, 1)[0] else 'PASS'`)
was exempted from the nested-slot fail-close because the walker saw the
Subscript test as runtime grammar — `_nested_slot_fail_closed` now judges the
arms with the folded condition, so the dead arm's `'FAIL'` is no longer
creditable as the fail-closed verdict while the live arm writes. New `_fsr13`
pins: six sentinel fold faces (`'sta' + 'tus' * (f'' == '')`, the Starred
count, the f-string format-spec join, the subscripted slice, `'PA' +
'SS'[:2]`, the tuple select), three runtime polarity faces (Name count /
Name slice / Name-keyed dict — all plain None), the conditional faces
(constant-subscript condition exempts nothing, Name condition keeps the
exemption), and three e2e rogues plus the nested dead-branch e2e through
`unshared_status_slots`. `f'{3.5}'` / `f'{3.5!s}'` join `_fsr11` (a float
constant joins the fold as `str(value)`, '3.5' both ways). The revert
mutants are killed (`mutneg-r29.out` M03 set-collapse, M04 fold tail, M05
conditional classification; pin-bound, battery fires on the real-tree
audit; fixture restored sha-verified). Real-tree aggregation-audit
self-check PASS (`agg-audit-r29.out` — byte-identical to the r26 merged
head before the r29 test pins; the final state with the two new
test_safety pins re-ran green). Witness side: `audit_native` gates every
pid-carrying field with `_retirement_real_pid` (process ppid, the exec
event pid/ppid, the egress ownerPid, the contract's launcherPid) so a
forged `True`/`1.0` no longer passes `== 1`, the class-4 control's mutated
`rows` read goes through `_retirement_list_rows`, the frame-id read reuses
`_retirement_usable_frame_id`, and `native_evidence.checked_evidence`
compares the three witness groups with `same_json`. test_safety grows two
pins (`test_audit_native_rejects_boolean_pids_on_every_witness_field`,
`test_forwarding_control_normalizes_a_corrupt_rows_sample_before_injection`)
for 164/164; this round touches three registered files, so gateway-parity
`check_frozen --write` re-registers the hashes (event
`frozen/20261108-1355-r1.md` fix-r29 section).

fix-r30 (DS r29 boundary#1 P1 + concurrency#1 P2-arbitrated-to-P1 +
sibling#1 P1, anthropic sibling#2/boundary#2-adjacent — both lineages
hit the same gaps; probes in `_tmp/20261110-1355-r26-harvest/`): the
container *content* the r29 gates did not charge, the resource-exhaustion
classification, and the connections counts. (1) The `%` fold's payload
bound and the Mult debit charged the *outer* container only —
`'%s' % ([['x' * 999999]] * 999)` folded a ~999 MB string from ~30 bytes
of source (probe-r30-fold.out: 999,004,995 chars pre-fix, sentinel in
< 1 ms post-fix; DS scenario ② re-checked with the exact spelling in
probe-r30-ds2.out). A container right operand now pays its recursive
`_folded_size` plus the rendered separators in the payload bound, and a
materialised container's Mult debit is the *max* of its element count
(the traversal bound the #892 `[''] * 999999` pin charges — a content-only
debit would zero out empty-string elements and unpin it) and its folded
content. (2) MemoryError/RecursionError join BLOCKED_APPARATUS_ERRORS and
carry their own `APPARATUS_RESOURCES` failure rule: apparatus resource
exhaustion is the apparatus' own BLOCKED/2, never a judged-product FAIL/1
with `failure.rule='EVIDENCE'` (the pre-fix inner handler contradicted the
outer handler and retirement_report, which already classified both as
BLOCKED). (3) The connections counts are gated with
`_retirement_real_count` (the count twin of the pid/port realness rules:
a plain non-negative int is all `len()` can emit; `True == 1` and
`1.0 == 1` are corrupt artifacts) on both the audit comparison and the
loader's malformed-document gate. Witness side:
`native_evidence.session_windows` gates the transcript-open pid with the
`type(...) is int` real-pid rule (both reviewers hit it), the three
native-witness negative anchors re-anchor to the `same_json` spelling r29
left in `native_evidence.py` (they were STALE and refused to run), and
same-json-surfaces grows four witness-replay revert rows (processes /
execWitness / networkWitness / transcriptWitness, 22 cases) bound to four
new `SameJsonSurfaces` pins each with an equal-type control. test_safety
grows the boundary values the pid table lacked (0, 2**31, 1.0 on the
event/launcher channels) plus five connections channels on a witnessed
variant, and the suite-level resource-exhaustion pin, for 169/169; this
round touches three registered files, so gateway-parity
`check_frozen --write` re-registers the hashes (event
`frozen/20261108-1355-r1.md` fix-r30 section).
