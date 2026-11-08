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
CASE_BUDGET_SECONDS = 8.0
MILESTONE_SECONDS = 1.5
POLL_SECONDS = 0.02

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
    def prepare(self, case, index, plan, run_id):
        """Arm the per-case plan before the turn is submitted."""
        plan_id = f'{case}#{index}'
        atomic_write(self.loop_dir / 'loop.json', json.dumps({
            'id': plan_id, 'phase': plan['phase'], 'order': plan['order'],
            'boundMs': LOOP_BOUND_MS}))
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
                                  'order': plan['order']})
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
        prefix_type = PREFIX_TYPES[plan['phase']]
        state['notes']['timeoutMs'] = timeout_ms
        started = time.monotonic()
        end = min(deadline, started + CASE_BUDGET_SECONDS)
        while time.monotonic() < end:
            frame = client.receive_poll(POLL_SECONDS)
            if frame is not None:
                state['received'].append(frame)
                event = frame.get('event') if isinstance(frame, dict) else None
                kind = event.get('type') if isinstance(event, dict) else None
                if state['prefixIndex'] is None and kind == prefix_type:
                    state['prefixIndex'] = len(state['received']) - 1
                if frame.get('id') == request_id and frame.get('final'):
                    state['final'] = frame
                    if 'finalAt' not in state:
                        # The timer.clear observation needs the submit→final
                        # elapsed time, not just the ordering of the frames.
                        state['finalAt'] = time.monotonic()
                        state['notes']['finalElapsedMs'] = round(
                            (state['finalAt'] - started) * 1000.0, 3)
            self._scan()
            self._settle_milestones(plan)
            if self._turn_complete(plan):
                break
            if state['final'] is not None:
                # Every real outcome trigger is satisfied at or before the
                # final frame — the native final is itself gated behind the
                # barrier releases (#1370) — so a trigger still missing a
                # milestone after final is a stuck choreography, not a slow
                # one. Bounded grace only; the honest result either way is
                # the same precise missing.
                end = min(end, state['finalAt'] + MILESTONE_SECONDS)
            # Every receipt-gated storage step necessarily precedes the final
            # frame (the native final is gated behind the held durable writes,
            # #1370), so final + a released outcome means the choreography has
            # interacted with every boundary it is going to. Trailing receipts
            # (loop final, K-P return) get the bounded top-up below.
            if self._final_seen() and state['outcome'] is not None:
                break
        # A loop final receipt can trail the last frame by a few polls; give
        # it the same bounded top-up as any other milestone.
        topup = min(deadline, time.monotonic() + MILESTONE_SECONDS)
        while time.monotonic() < topup and not self._turn_complete(plan):
            client.receive_poll(POLL_SECONDS)
            self._scan()
            self._settle_milestones(plan)
        if state['prefixIndex'] is None:
            state['missed'].append('phase:client.frame(' + prefix_type + ')')
        self._observe_orders(plan)
        return {'received': state['received'], 'actions': state['actions'],
                'notes': state['notes'], 'missed': state['missed']}

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

    def _loop_final(self):
        found = self._receipts('loop', site='loop', phase='final')
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
        # 2. K-P return release after the timer-decided final frame.
        if (plan['order'] == 'known-final-before-timeout-pending'
                and not state['returnReleased'] and state['outcome'] == 'known'
                and self.loop_receipt('return', 'block')
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
            self._release_storage(step['blockSeq'])
            step['phase'] = 'second-arm'
            second = second_append_key(variant)
            if second is not None:
                # Written immediately after the release, while the product's
                # 10 ms release poll still separates the released write from
                # the queued one (plan.md section 1).
                state['arms'] = json.loads(json.dumps(state['arms']))
                state['arms']['registry']['barrier'] = {
                    'id': f"{state['case']}#{state['index']}-r2", 'key': second,
                    'boundMs': STORAGE_BOUND_MS}
                atomic_write(self.storage_dir / 'stimulus.json', json.dumps(state['arms']))
                self._action('rearm-storage', state['arms']['registry'])
                return
            step['phase'] = 'done'
        if step['phase'] == 'second-arm':
            second = second_append_key(variant)
            blocked = self.storage_receipt('registry', 'block', second) if second else []
            if blocked:
                self._release_storage(blocked[-1]['seq'])
                step['phase'] = 'done'

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
        return True

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
            and isinstance(frame.get('event'), dict)
            and (frame['event'].get('type') == 'agent_status'
                 and frame['event'].get('event') == 'turn_timeout'
                 or frame['event'].get('type') == 'error'
                 and frame['event'].get('code') == 'turn_timeout')
            for frame in state['received'])
        state['notes']['prefixIndex'] = state['prefixIndex']
        state['notes']['finalIndex'] = next(
            (i for i, frame in enumerate(state['received'])
             if isinstance(frame, dict) and frame.get('final')), None)

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
