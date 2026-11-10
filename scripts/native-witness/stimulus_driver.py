"""#2396 native collector adapter for the s2-s4 product stimulus entries.

Drives the #1368 per-case plans against the real product entries merged in
#1369 (run registry construction), #1370 (storage barriers and faults) and
#1371 (controlled loop), and collects the raw receipts the attestation in
``stimulus_plan.execution`` scores. No receipt is fabricated here: the driver
only writes control files (atomically, tmp + rename), polls product-owned
witness directories and records its own control actions.

Startup-frozen resolution compatibility (see `_tmp` plan.md section 1):
loop.json is rewritten between cases, so the sidecar that turn N spawns
freezes plan N at its own startup; the gateway's startup-frozen copy only
gates ``is_planned``. The registry switch is process-wide, so one capture
carries exactly one on/off setting and the 72-case manifest is still driven
in full for both settings (two captures, union attribution off-tree).
"""
import copy
import json
import os
from pathlib import Path
import time


class StimulusIncomplete(Exception):
    """A structural stimulus problem that must fail the capture closed."""


# Loop plan bound and storage barrier bounds: the driver must always be able
# to out-wait the product's own bound, and a lost collector must never wedge
# the product. Milestone waits stay well under the product bounds.
LOOP_BOUND_MS = 8000
STORAGE_BOUND_MS = 8000
MILESTONE_SECONDS = 1.5
POLL_SECONDS = 0.02
# The per-barrier case budget must out-wait the product's own storage bound
# plus the final top-up: with 8.0 == 8000 ms the budget raced the bound, so a
# barrier whose bound expired was cut off before the self-release could be
# observed (#2396 fix-r1, codex concurrency/DS boundary). Cases whose variant
# appends twice stack two barrier lifetimes, so the budget counts each barrier
# (#2396 fix-r2, DS boundary P2).
CASE_BUDGET_SECONDS = STORAGE_BOUND_MS / 1000.0 + 2 * MILESTONE_SECONDS


def case_budget_seconds(variant):
    """The case budget, scaled by the number of storage barriers it stacks.

    Every armed barrier can hold its whole bound: the writer barrier gates
    the status write, the registry barrier gates the append, and a re-armed
    registry barrier gates the error append, so the worst-case wall clock is
    their sum (#2396 fix-r3, DS boundary/sibling/concurrency P2 — three
    families; the old count ignored the writer site and gave 3-barrier
    variants like both-slow only 2).  Counting them serially is the
    conservative direction: the budget can only stretch the window, never
    truncate a slow case into a missing final.
    """
    barriers = (1 if (variant.get('slowStatus') or variant.get('slowCatchStatus')) else 0) \
        + (1 if variant.get('slowAppend') else 0) \
        + (1 if second_append_key(variant) is not None else 0)
    return max(1, barriers) * CASE_BUDGET_SECONDS

PREFIX_TYPES = {'model': 'model_request_started', 'tool': 'tool_call_started'}


def atomic_write(path, text):
    """Publish a control file the way the product reads it: tmp + rename."""
    path = Path(path)
    temporary = path.with_name(path.name + '.tmp')
    temporary.write_text(text)
    os.replace(temporary, path)


def storage_arms(case, index, variant):
    """The per-case stimulus.json arms (plan.md section 6).

    One barrier or one fault per site, exactly like the product's SiteConfig.
    Keys stay the #1368 plan keys; where the native timeout settlement never
    performs that write the arm simply stays inert and the target keeps a
    precise missing (plan.md section 5).
    """
    tag = f'{case}#{index}'
    writer = {}
    if variant.get('slowStatus'):
        writer['barrier'] = {'id': tag + '-w1', 'key': 'turn_timeout',
                             'boundMs': STORAGE_BOUND_MS}
    elif variant.get('slowCatchStatus'):
        writer['barrier'] = {'id': tag + '-w1', 'key': 'gateway_submit_failed',
                             'boundMs': STORAGE_BOUND_MS}
    if variant.get('failStatus'):
        writer['fault'] = {'id': tag + '-wf', 'key': variant['failStatus']}
    registry = {}
    append = variant.get('slowAppend')
    if append:
        # A faulted turn_timeout status write never reaches its registry
        # status append, so the error append owns the barrier (plan special
        # case); every other variant blocks the status append first.
        key = 'error:turn_timeout' if (append == 'turn_timeout'
                                       and variant.get('failStatus') == 'turn_timeout') \
            else 'agent_status:' + append
        registry['barrier'] = {'id': tag + '-r1', 'key': key, 'boundMs': STORAGE_BOUND_MS}
    if variant.get('failAppend'):
        registry['fault'] = {'id': tag + '-rf', 'key': variant['failAppend']}
    return {'writer': writer, 'registry': registry}


def second_append_key(variant):
    """The re-armed registry barrier key of a slowAppend cell, if any."""
    append = variant.get('slowAppend')
    if not append:
        return None
    if append == 'turn_timeout' and variant.get('failStatus') == 'turn_timeout':
        return None  # the single-barrier plan special case
    return 'error:' + append


class StimulusDriver:
    """Per-case choreography plus raw receipt collection.

    ``client`` only needs ``receive_poll(seconds) -> frame | None`` (the
    collector's WireClient provides it); every real frame keeps the recorder
    contract because only the collector records rows.
    """

    def __init__(self, loop_dir, storage_dir, *, registry_on):
        self.loop_dir = Path(loop_dir)
        self.storage_dir = Path(storage_dir)
        self.registry_setting = 'on' if registry_on else 'off'
        for directory in (self.loop_dir, self.storage_dir):
            directory.mkdir(parents=True, exist_ok=True)
        self._active = None
        self._stderr_seen = ''

    def environment(self):
        """The three s2-s4 entries for the gateway process (sidecars inherit)."""
        return {'PILOTDECK_NATIVE_RUN_REGISTRY': self.registry_setting,
                'PILOTDECK_NATIVE_STORAGE_STIMULUS': str(self.storage_dir),
                'PILOTDECK_NATIVE_LOOP_STIMULUS': str(self.loop_dir)}

    # ------------------------------------------------------------------
    # control files
    # ------------------------------------------------------------------
    def prepare(self, case, index, plan, run_id, *, require_provider=False):
        """Arm the per-case plan before the turn is submitted."""
        plan_id = f'{case}#{index}'
        loop_plan = {'id': plan_id, 'phase': plan['phase'], 'order': plan['order'],
                     'boundMs': LOOP_BOUND_MS}
        if require_provider:
            loop_plan['requireProvider'] = True
        atomic_write(self.loop_dir / 'loop.json', json.dumps(loop_plan))
        atomic_write(self.storage_dir / 'stimulus.json',
                    json.dumps(storage_arms(case, index, plan['variant'])))
        self._active = {
            'case': case, 'index': index, 'plan': plan, 'planId': plan_id,
            'runId': run_id,
            'arms': storage_arms(case, index, plan['variant']),
            'loopSnapshot': self._witness_names(self.loop_dir),
            'storageSnapshot': self._witness_names(self.storage_dir),
            'received': [], 'final': None, 'prefixIndex': None,
            'actions': [], 'notes': {},
            'loopReceipts': [], 'storageReceipts': [],
            'outcome': None, 'returnReleased': False,
            'storageSteps': {}, 'missed': [],
        }
        self._action('arm-loop', {'planId': plan_id, 'phase': plan['phase'],
                                  'order': plan['order'],
                                  **({'requireProvider': True} if require_provider else {})})
        self._action('arm-storage', self._active['arms'])
        return plan_id

    def disarm(self):
        """Disarm storage arms so a stale barrier can never hit the next case."""
        if self._active is not None:
            self._action('disarm-storage', {})
        atomic_write(self.storage_dir / 'stimulus.json', '{}')

    # ------------------------------------------------------------------
    # receipt scanning
    # ------------------------------------------------------------------
    @staticmethod
    def _witness_names(directory):
        witness = Path(directory) / 'witness'
        try:
            return {item.name for item in witness.iterdir()}
        except FileNotFoundError:
            return set()

    def _scan(self):
        """Collect receipts published since the previous scan."""
        state = self._active
        for directory, snapshot, sink, key in (
                (self.loop_dir, state['loopSnapshot'], state['loopReceipts'], 'loop'),
                (self.storage_dir, state['storageSnapshot'], state['storageReceipts'], 'storage')):
            witness = directory / 'witness'
            try:
                names = {item.name for item in witness.iterdir()}
            except FileNotFoundError:
                continue
            for name in sorted(names - snapshot):
                snapshot.add(name)
                try:
                    value = json.loads((witness / name).read_text())
                except (OSError, ValueError):
                    # A half-written or unreadable receipt is re-read next
                    # tick; it never silently disappears from the evidence.
                    snapshot.discard(name)
                    continue
                if isinstance(value, dict):
                    value['file'] = name
                    sink.append(value)
        return state

    def _receipts(self, kind, **match):
        state = self._active
        found = []
        for receipt in state['loopReceipts'] if kind == 'loop' else state['storageReceipts']:
            if all(receipt.get(field) == value for field, value in match.items()):
                found.append(receipt)
        return found

    def loop_receipt(self, phase, key):
        return self._receipts('loop', site='loop', phase=phase, key=key)

    def _plan_bound(self, phase, key):
        """Loop receipts at one phase/key bound to this plan's pids."""
        pids = self._plan_pids()
        return [receipt for receipt in self._receipts('loop', site='loop', phase=phase,
                                                       key=key)
                if receipt.get('pid') in pids]

    def fence_receipt(self, phase, key):
        return self._receipts('loop', site='fence', phase=phase, key=key)

    def storage_receipt(self, site, phase, key):
        return self._receipts('storage', site=site, phase=phase, key=key)

    # ------------------------------------------------------------------
    # control actions
    # ------------------------------------------------------------------
    def _action(self, name, detail):
        self._active['actions'].append(
            {'action': name, 'detail': detail, 'monotonic': round(time.monotonic(), 6)})

    def _release_loop(self, stage, outcome=None):
        state = self._active
        document = {'id': state['planId'], 'stage': stage}
        if outcome is not None:
            document['outcome'] = outcome
        atomic_write(self.loop_dir / 'release.json', json.dumps(document))
        self._action('release-loop', document)

    def _release_storage(self, seq):
        atomic_write(self.storage_dir / 'release.json', json.dumps({'seq': seq}))
        self._action('release-storage', {'seq': seq})

    # ------------------------------------------------------------------
    # per-turn choreography (plan.md section 3)
    # ------------------------------------------------------------------
    def drive(self, case, plan, client, request_id, deadline, timeout_ms=None):
        """Run one turn: multiplex client frames and product receipts."""
        state = self._active
        if state is None or state['case'] != case:
            raise StimulusIncomplete('#2396 drive without prepare: ' + str(case))
        if self.registry_setting == 'off' and plan['variant'].get('slowAppend'):
            state['notes']['unavailableStorageActions'] = ['registry slowAppend (capture registry off)']
        prefix_type = PREFIX_TYPES[plan['phase']]
        state['notes']['timeoutMs'] = timeout_ms
        state['requestId'] = request_id
        state['prefixType'] = prefix_type
        started = time.monotonic()
        state['startedAt'] = started
        end = min(deadline, started + case_budget_seconds(plan.get('variant')))
        while time.monotonic() < end:
            self._ingest(client.receive_poll(POLL_SECONDS))
            self._scan()
            self._settle_milestones(plan)
            if self._turn_complete(plan):
                break
            if state['final'] is not None and self._storage_actions_complete(plan):
                # Once configured storage actions are issued, preserve the
                # original bounded final/outcome grace. A reliable hard close
                # may precede durable I/O; pending storage arms still own the
                # original case budget, not this shorter final grace.
                end = min(end, state['finalAt'] + MILESTONE_SECONDS)
            # A wire final cannot cancel a configured second barrier. Only
            # stop this loop after its actual control actions are complete;
            # trailing loop/return receipts retain the same bounded top-up.
            if (self._final_seen() and state['outcome'] is not None
                    and self._storage_actions_complete(plan)):
                break
        # A loop final receipt can trail the last frame by a few polls; give
        # it the same bounded top-up as any other milestone. Frames arriving
        # during the top-up are ingested through the same path as the main
        # loop (#2396 fix-r1: the return value used to be dropped, losing a
        # final that landed just past the case budget).
        topup = min(deadline, time.monotonic() + MILESTONE_SECONDS)
        while time.monotonic() < topup and not self._turn_complete(plan):
            self._ingest(client.receive_poll(POLL_SECONDS))
            self._scan()
            self._settle_milestones(plan)
        if state['prefixIndex'] is None:
            state['missed'].append('phase:client.frame(' + prefix_type + ')')
        self._observe_orders(plan)
        return {'received': state['received'], 'actions': state['actions'],
                'notes': state['notes'], 'missed': state['missed']}

    def _ingest(self, frame):
        """Record one client frame (shared by the drive and top-up loops).

        The final-frame capture binds to this case's request id, so a frame
        keyed to another request — e.g. one left in the socket buffer by the
        previous case — is received but never mistaken for this case's
        final (#2396 fix-r1).
        """
        if frame is None:
            return
        state = self._active
        state['received'].append(frame)
        event = frame.get('event') if isinstance(frame, dict) else None
        kind = event.get('type') if isinstance(event, dict) else None
        if (state['prefixIndex'] is None and kind == state['prefixType']
                and frame.get('id') == state.get('requestId')):
            state['prefixIndex'] = len(state['received']) - 1
        if frame.get('id') == state.get('requestId') and frame.get('final'):
            state['final'] = frame
            if 'finalAt' not in state:
                # The timer.clear observation needs the submit→final
                # elapsed time, not just the ordering of the frames.
                state['finalAt'] = time.monotonic()
                state['notes']['finalElapsedMs'] = round(
                    (state['finalAt'] - state['startedAt']) * 1000.0, 3)

    def _wait_receipt_seen(self):
        return bool(self.loop_receipt('wait', self._active['planId']))

    def _outcome_trigger(self, plan):
        """The milestone that gates the outcome release (plan.md section 3).

        Native order note: for slowStatus/slowAppend the TS ``waitClose``
        gate would deadlock, because the native final frame is itself gated
        behind the held durable write (#1370 replay). The barrier block
        receipt is the native equivalent of reaching the held boundary.
        """
        variant = plan['variant']
        if plan['order'] in ('unknown-first', 'known-final-first',
                             'known-final-before-timeout-pending'):
            return self._wait_receipt_seen()
        if plan['order'] == 'after-complete':
            return self._final_seen() and bool(
                self.fence_receipt('complete', self._active['runId']))
        if variant.get('slowStatus'):
            return bool(self.storage_receipt('writer', 'block', 'turn_timeout'))
        if variant.get('slowAppend'):
            return bool(self._first_registry_block(plan))
        return self._final_seen()

    def _first_registry_block(self, plan):
        arms = self._active['arms']
        arm = arms['registry'].get('barrier')
        if not arm:
            return []
        return self.storage_receipt('registry', 'block', arm['key'])

    def _final_seen(self):
        return self._active['final'] is not None

    def _plan_pids(self):
        """The pids the plan's own input/wait receipts name (key=planId).

        A trailing receipt from the previous case's turn — published after
        this case's directory snapshot was taken — carries that turn's pid,
        so final/return receipts are only this case's when their pid is in
        this set (#2396 fix-r1, codex boundary/sibling).  A receipt without
        a pid never names one either — the same `pid is not None` guard the
        ruler side already has (#2396 fix-r3, DS sibling P2).
        """
        state = self._active
        return {receipt.get('pid') for receipt in state['loopReceipts']
                if receipt.get('site') == 'loop' and receipt.get('key') == state['planId']
                and receipt.get('phase') in ('input', 'wait')
                and receipt.get('pid') is not None}

    def _loop_final(self):
        pids = self._plan_pids()
        found = [receipt for receipt in self._receipts('loop', site='loop', phase='final')
                 if receipt.get('pid') in pids]
        return found[-1] if found else None

    def _settle_milestones(self, plan):
        """Apply the choreography rules; bounded waits never block others."""
        state = self._active
        variant = plan['variant']
        # 1. Outcome release.
        if state['outcome'] is None and self._outcome_trigger(plan):
            outcome = {'unknown-first': 'unknown', 'after-complete': 'unknown'}.get(
                plan['order'], 'unknown')
            if plan['order'] == 'known-final-first':
                outcome = 'known'
            if plan['order'] == 'known-final-before-timeout-pending':
                outcome = 'known'
            self._release_loop('outcome', outcome)
            state['outcome'] = outcome
        # 2. K-P return release after the timer-decided final frame. The held
        # return is this case's only when its pid matches the plan's own
        # input/wait receipts (#2396 fix-r1 cross-case binding).
        if (plan['order'] == 'known-final-before-timeout-pending'
                and not state['returnReleased'] and state['outcome'] == 'known'
                and self._plan_bound('return', 'block')
                and self._final_seen()):
            self._release_loop('return')
            state['returnReleased'] = True
        # 3. slowStatus: FIFO witness, then release the held writer write.
        # The follower key is whatever the native settlement writes next
        # (#1370 measured `turn_result`; the TS name has no native
        # counterpart, so the first queued receipt is the raw witness).
        self._storage_step(
            plan, 'slowStatus', ('writer', 'block', 'turn_timeout'),
            lambda: self._first_queued('writer'),
            'control.status.fifo.checked')
        # 4. slowCatchStatus: release the catch-status barrier once blocked.
        # No FIFO witness here: TS waits for a gsf follower the native
        # settlement never writes (plan.md section 5), so the release is not
        # gated on a follower the product cannot produce.
        self._storage_step(
            plan, 'slowCatchStatus', ('writer', 'block', 'gateway_submit_failed'),
            None, None)
        # 5. slowAppend: FIFO witness, release, re-arm the second key, release.
        self._append_steps(plan)

    def _first_queued(self, site):
        """The first FIFO follower receipt at a site, by sequence."""
        queued = self._receipts('storage', site=site, phase='queued')
        return [min(queued, key=lambda receipt: receipt['seq'])] if queued else []

    def _storage_step(self, plan, name, block_match, fifo_source, fifo_action):
        """One barrier family: optional FIFO witness, then the release."""
        state = self._active
        if name not in plan['variant']:
            return
        site, phase, key = block_match
        step = state['storageSteps'].setdefault(name, {'queued': None, 'seq': None})
        if step['seq'] is not None:
            return
        blocked = self.storage_receipt(site, phase, key)
        if not blocked:
            return
        if state['outcome'] is None:
            return  # TS order: the release never precedes the unknown.
        if fifo_source is None:
            self._release_storage(blocked[-1]['seq'])
            step['seq'] = blocked[-1]['seq']
            return
        queued = fifo_source()
        if queued:
            if fifo_action:
                self._action(fifo_action, {'site': site, 'queuedKey': queued[0]['key'],
                                           'blockedKey': key, 'blockSeq': blocked[-1]['seq']})
            self._release_storage(blocked[-1]['seq'])
            step['seq'] = blocked[-1]['seq']
            step['queued'] = queued[0]['key']

    def _append_steps(self, plan):
        """slowAppend: block, FIFO, release, re-arm the second key, release."""
        state = self._active
        variant = plan['variant']
        if 'slowAppend' not in variant:
            return
        arm = state['arms']['registry'].get('barrier')
        if arm is None:
            return
        step = state['storageSteps'].setdefault('slowAppend', {'phase': 'first'})
        if step['phase'] == 'first':
            blocked = self.storage_receipt('registry', 'block', arm['key'])
            if not blocked:
                return
            step['blockSeq'] = blocked[-1]['seq']
            step['phase'] = 'fifo'
        if step['phase'] == 'fifo':
            if state['outcome'] is None:
                return  # TS order: releaseUnknown precedes the append release.
            queued = self._first_queued('registry')
            if not queued:
                return
            self._action('control.registry.fifo.checked',
                         {'queuedKey': queued[0]['key'], 'blockedKey': arm['key'],
                          'blockSeq': step['blockSeq']})
            step['queued'] = queued[0]['key']
            second = second_append_key(variant)
            if second is not None:
                # The second barrier is armed BEFORE the first is released:
                # the product re-reads stimulus.json on every append, so the
                # queued error append must already see the new barrier when
                # the release lets the status append complete. The first
                # waiter self-holds its own seq/bound and is unaffected by
                # the swap (#2396 fix-r1, codex concurrency P1).
                state['arms'] = copy.deepcopy(state['arms'])
                state['arms']['registry']['barrier'] = {
                    'id': f"{state['case']}#{state['index']}-r2", 'key': second,
                    'boundMs': STORAGE_BOUND_MS}
                atomic_write(self.storage_dir / 'stimulus.json', json.dumps(state['arms']))
                self._action('rearm-storage', state['arms']['registry'])
                self._release_storage(step['blockSeq'])
                step['lastRelease'] = {'key': arm['key'], 'seq': step['blockSeq']}
                step['phase'] = 'second-arm'
                return
            self._release_storage(step['blockSeq'])
            step['lastRelease'] = {'key': arm['key'], 'seq': step['blockSeq']}
            step['phase'] = 'done'
        if step['phase'] == 'second-arm':
            second = second_append_key(variant)
            blocked = self.storage_receipt('registry', 'block', second) if second else []
            if blocked:
                self._release_storage(blocked[-1]['seq'])
                step['lastRelease'] = {'key': second, 'seq': blocked[-1]['seq']}
                step['phase'] = 'done'

    def _release_observed(self, site, key, block_seq):
        return any(type(receipt.get('seq')) is int and receipt['seq'] > block_seq
                   and 'bounded' not in receipt
                   for receipt in self.storage_receipt(site, 'release', key))

    def _storage_actions_complete(self, plan):
        """Wire final is not an I/O fence (#2498). Do not disarm an owed arm.

        This gates collection only: no missing boundary is certified here.
        Registry-off remains explicitly unobservable to the unchanged judge.
        """
        steps = self._active['storageSteps']
        for name, key in (('slowStatus', 'turn_timeout'),
                          ('slowCatchStatus', 'gateway_submit_failed')):
            if plan['variant'].get(name):
                seq = steps.get(name, {}).get('seq')
                if seq is None or not self._release_observed('writer', key, seq):
                    return False
        if plan['variant'].get('slowAppend') and self.registry_setting == 'on':
            step = steps.get('slowAppend', {})
            released = step.get('lastRelease')
            if (step.get('phase') != 'done' or released is None
                    or not self._release_observed('registry', released['key'], released['seq'])):
                return False
        return True

    def _turn_complete(self, plan):
        state = self._active
        if state['final'] is None:
            return False
        if state['outcome'] is None:
            return False
        if self._loop_final() is None:
            return False
        if plan['order'] == 'known-final-before-timeout-pending' and not state['returnReleased']:
            return False
        return self._storage_actions_complete(plan)

    def _observe_orders(self, plan):
        """Record the driver-side ordering observations the attestation uses."""
        state = self._active
        actions = state['actions']
        releases = [a for a in actions if a['action'] == 'release-loop'
                    and a['detail'].get('stage') == 'outcome']
        state['notes']['outcomeReleaseAfterFinal'] = bool(
            releases and state.get('finalAt') is not None
            and releases[0]['monotonic'] >= state['finalAt'])
        # K-P: the held generator return is released only after the timer
        # decided (the final frame), so the completion receipt that follows
        # the delivery can never describe a pre-timeout completion.
        returns = [a for a in actions if a['action'] == 'release-loop'
                   and a['detail'].get('stage') == 'return']
        state['notes']['returnReleaseAfterFinal'] = bool(
            returns and state.get('finalAt') is not None
            and returns[0]['monotonic'] >= state['finalAt'])
        state['notes']['noTimeoutFrameBeforeFinal'] = not any(
            isinstance(frame, dict)
            and frame.get('id') == state.get('requestId')
            and isinstance(frame.get('event'), dict)
            and (frame['event'].get('type') == 'agent_status'
                 and frame['event'].get('event') == 'turn_timeout'
                 or frame['event'].get('type') == 'error'
                 and frame['event'].get('code') == 'turn_timeout')
            for frame in state['received'])
        state['notes']['prefixIndex'] = state['prefixIndex']
        state['notes']['finalIndex'] = next(
            (i for i, frame in enumerate(state['received'])
             if isinstance(frame, dict) and frame.get('final')
             and frame.get('id') == state.get('requestId')), None)

    # ------------------------------------------------------------------
    # evidence assembly
    # ------------------------------------------------------------------
    def construction(self, stderr_text):
        """Parse the product's own construction receipts from stderr.

        The parsed values are what the product actually froze, not the
        collector's request: the no-registry closure evidence is the product's
        own ``native run registry off`` line, so a file-absence check can never
        stand in for it.
        """
        receipts = [line for line in stderr_text.split('\n')
                    if line.startswith('native ')]

        def suffix(prefix):
            line = next((item for item in receipts if item.startswith(prefix)), None)
            return line[len(prefix):].strip() or None if line is not None else None

        return {
            'runRegistry': suffix('native run registry'),
            'storage': suffix('native storage stimulus on'),
            'loop': suffix('native loop stimulus on'),
            'stderrReceipts': receipts,
            # The gateway prints this line once and every sidecar at its own
            # startup, so this counts both; the per-case plan binding is the
            # loop input/wait receipts keyed by planId, never this count.
            'loopStimulusReceipts': sum(
                1 for line in receipts if line.startswith('native loop stimulus on')),
        }

    def evidence(self, case, run_id, request_id, drive_result, stderr_text,
                 registry_events, registry_record, runs_json_absent):
        """Assemble the per-case raw evidence record for the attestation."""
        state = self._active
        if state is None or state['case'] != case:
            raise StimulusIncomplete('#2396 evidence without a driven case: ' + str(case))
        # Receipts can trail the last frame by a few polls; harvest once more
        # so the evidence never omits a receipt the product already published.
        self._scan()
        received = drive_result['received'] if drive_result else state['received']
        frames = []
        for frame in received:
            if not isinstance(frame, dict):
                continue
            event = frame.get('event')
            kind = event.get('type') if isinstance(event, dict) else None
            extra = {}
            if isinstance(event, dict):
                for field in ('event', 'code'):
                    if field in event:
                        extra[field] = event[field]
            frames.append({'type': kind, 'final': bool(frame.get('final')),
                           'id': frame.get('id'), **extra})
        loop = [dict(receipt) for receipt in state['loopReceipts']]
        storage = [dict(receipt) for receipt in state['storageReceipts']]
        return {
            'planId': state['planId'], 'case': case, 'runId': run_id,
            'requestId': request_id,
            'construction': self.construction(stderr_text),
            'frames': frames,
            'loopReceipts': loop, 'storageReceipts': storage,
            'registryEvents': registry_events,
            'registryRecord': registry_record,
            'runsJsonAbsent': runs_json_absent,
            'controlActions': drive_result['actions'] if drive_result else state['actions'],
            'notes': drive_result['notes'] if drive_result else state['notes'],
            'missed': list(state['missed']),
        }
