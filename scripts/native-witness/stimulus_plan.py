"""#1368 E1 input contract. No native injection hooks are implemented here.

The independent declarations mirror six_cells.mts preregistration, not Rust
terminal expectations. A transport request is never a boundary witness.
"""
import copy


# Keep the actual TS fields: variant names alone do not define the stimulus.
CELLS = [
    {'name': 'no-registry-fast', 'registry': False, 'variants': [{'name': 'fast'}]},
    {'name': 'no-registry-slow', 'registry': False, 'variants': [
        {'name': 'status-slow', 'slowStatus': True},
        {'name': 'catch-status-slow', 'slowCatchStatus': True}]},
    {'name': 'no-registry-failure', 'registry': False, 'variants': [
        {'name': 'timeout-status-fails', 'failStatus': 'turn_timeout'},
        {'name': 'catch-status-fails', 'failStatus': 'gateway_submit_failed'}]},
    {'name': 'registry-fast', 'registry': True, 'variants': [{'name': 'fast'}]},
    {'name': 'registry-slow', 'registry': True, 'variants': [
        {'name': 'status-slow-append-fast', 'slowStatus': True},
        {'name': 'status-fast-append-slow', 'slowAppend': 'turn_timeout'},
        {'name': 'both-slow', 'slowStatus': True, 'slowAppend': 'turn_timeout'},
        {'name': 'status-fast-catch-append-slow', 'slowAppend': 'gateway_submit_failed'},
        {'name': 'status-slow-catch-append-slow', 'slowStatus': True, 'slowAppend': 'gateway_submit_failed'},
        {'name': 'catch-status-slow-append-fast', 'slowCatchStatus': True},
        {'name': 'catch-status-slow-append-slow', 'slowCatchStatus': True, 'slowAppend': 'turn_timeout'}]},
    {'name': 'registry-failure', 'registry': True, 'variants': [
        {'name': 'timeout-status-fails-append-fast', 'failStatus': 'turn_timeout'},
        {'name': 'catch-status-fails-append-fast', 'failStatus': 'gateway_submit_failed'},
        {'name': 'timeout-status-fails-append-slow', 'failStatus': 'turn_timeout', 'slowAppend': 'turn_timeout'},
        {'name': 'catch-status-fails-append-slow', 'failStatus': 'gateway_submit_failed', 'slowAppend': 'turn_timeout'},
        {'name': 'timeout-status-fails-catch-append-slow', 'failStatus': 'turn_timeout', 'slowAppend': 'gateway_submit_failed'},
        {'name': 'catch-status-fails-catch-append-slow', 'failStatus': 'gateway_submit_failed', 'slowAppend': 'gateway_submit_failed'},
        {'name': 'timeout-append-fails-status-fast', 'failAppend': 'error:turn_timeout'},
        {'name': 'timeout-status-event-append-fails', 'failAppend': 'agent_status:turn_timeout'},
        {'name': 'catch-status-event-append-fails', 'failAppend': 'agent_status:gateway_submit_failed'},
        {'name': 'catch-append-fails-status-fast', 'failAppend': 'error:gateway_submit_failed'},
        {'name': 'timeout-append-fails-status-slow', 'failAppend': 'error:turn_timeout', 'slowStatus': True},
        {'name': 'catch-append-fails-status-slow', 'failAppend': 'error:gateway_submit_failed', 'slowStatus': True},
        {'name': 'timeout-status-and-catch-append-fail', 'failStatus': 'turn_timeout', 'failAppend': 'error:gateway_submit_failed'},
        {'name': 'catch-status-and-timeout-append-fail', 'failStatus': 'gateway_submit_failed', 'failAppend': 'error:turn_timeout'},
        {'name': 'timeout-append-fails-catch-status-slow', 'failAppend': 'error:turn_timeout', 'slowCatchStatus': True}]},
]
ORDERS = ('unknown-first', 'known-final-first', 'after-complete',
          'known-final-before-timeout-pending')
for cell in CELLS:
    if cell['name'].endswith('-fast'):
        cell['variants'].extend({'name': order, 'order': order} for order in ORDERS)


def event(kind, **fields):
    return {'kind': kind, **fields}


def boundary(name, hook, target, events, before=()):
    """Selectors describe TS target witnesses; native adapters remain absent."""
    return {'name': name, 'hook': hook, 'target': target,
            'events': events, 'before': list(before)}


def plan(case):
    matches = [(cell, variant, phase) for cell in CELLS for variant in cell['variants']
               for phase in ('model', 'tool')
               if case == f"{cell['name']}/{variant['name']}/{phase}"]
    if not matches:
        raise ValueError('#1368 unknown stimulus case: ' + str(case))
    if len(matches) > 1:
        raise ValueError('#1368 duplicate stimulus case: ' + str(case))
    cell, variant, phase = matches[0]
    order = variant.get('order', 'timeout-first')
    prefix = event('client.frame', event={'type': 'tool_call_started' if phase == 'tool' else 'model_request_started'})
    # The producer emits the prefix before waiting. Client consumption follows
    # asynchronously; with a registry even queue.enqueue occurs after the wait.
    emitted = event('registry.append.attempt' if cell['registry'] else 'queue.enqueue',
                    event=prefix['event'])
    loop_wait = event('loop.wait', phase=phase)
    targets = [
        boundary('registry', 'registry.configure', 'gateway runRegistry injection', [], []),
        boundary('phase', 'loop.phase', 'controlled loop wait after the real phase prefix',
                 [emitted, prefix, loop_wait], [(emitted, loop_wait)]),
    ]
    targets[0]['configured'] = cell['registry']
    targets[0]['evidence'] = 'construction receipt + registry applicability; absent file alone is insufficient'
    fire = event('timer.fire')
    close = event('queue.close')
    unknown = event('control.release.unknown')
    final_unknown = event('loop.final', outcome='result_unknown', code='DEADLINE_EXCEEDED')
    known = event('loop.final', outcome='completed')
    status = event('status.attempt', key='turn_timeout')
    if order in ('unknown-first', 'known-final-first'):
        outcome = final_unknown if order == 'unknown-first' else known
        skipped = event('timer.fire.skipped', reason='already cleared by baseline')
        order_events = [outcome, event('timer.clear'), close, skipped]
        edges = [(outcome, close), (close, skipped), (event('timer.clear'), skipped)]
    elif order == 'known-final-before-timeout-pending':
        blocked = event('loop.return.block')
        order_events = [known, blocked, fire, close, event('session.submit.exit')]
        edges = [(known, blocked), (blocked, fire), (fire, close), (close, event('session.submit.exit'))]
    elif order == 'after-complete':
        complete = event('fence.complete')
        order_events = [fire, status, close, complete, event('client.done'), unknown, final_unknown]
        edges = [(fire, status), (status, close), (close, complete),
                 (complete, unknown), (event('client.done'), unknown), (unknown, final_unknown)]
    else:
        order_events = [fire, status, close, unknown, final_unknown]
        edges = [(fire, status), (status, unknown), (unknown, final_unknown)]
        # Slow T status releases unknown while the writer is held; every other
        # timeout-first variant releases it after queue.close (S:371-382).
        edges.append((unknown, close) if variant.get('slowStatus') else (close, unknown))
    targets.append(boundary('order', 'loop.outcome-and-deadline',
                            order, order_events, edges))
    for field, key in (('slowStatus', 'turn_timeout'), ('slowCatchStatus', 'gateway_submit_failed')):
        if variant.get(field):
            blocked = event('status.block', key=key, boundary='eventStore.append inside writer.writeTail')
            release = event('status.release', key=key)
            events = [blocked, release]
            edges = [(blocked, release)]
            if field == 'slowStatus':
                fifo = event('control.status.fifo.checked', queued='gateway_submit_failed', blocked='turn_timeout')
                events.append(fifo)
                edges.extend([(blocked, unknown), (unknown, fifo), (fifo, release)])
            targets.append(boundary(field, 'writer.barrier', 'eventStore.append inside writer.writeTail', events, edges))
    if 'slowAppend' in variant:
        key = variant['slowAppend']
        status_blocked = event('registry.persist.block', key='agent_status:' + key,
                               boundary='persist inside mutationChain and file lock')
        status_release = event('registry.persist.release', key='agent_status:' + key)
        blocked = event('registry.persist.block', key='error:' + key,
                        boundary='persist inside mutationChain and file lock')
        release = event('registry.persist.release', key='error:' + key)
        fifo = event('control.registry.fifo.checked', queued='error:gateway_submit_failed', blocked=key)
        target = boundary('slowAppend', 'registry.persist-barrier',
                          'after memory mutation, before snapshot, inside mutationChain and file lock',
                          [status_blocked, status_release, blocked, release, fifo],
                          [(status_blocked, fifo), (fifo, status_release),
                           (status_release, blocked), (blocked, release)])
        # A failed T status store prevents its registry status append; then
        # the error append owns the first barrier. G status failure is caught
        # by the TS helper and still appends its registry status event.
        if key == 'turn_timeout' and variant.get('failStatus') == 'turn_timeout':
            target['events'] = [blocked, release, fifo]
            target['before'] = [(blocked, fifo), (fifo, release)]
        target['blockedKeys'] = ['agent_status:' + key, 'error:' + key]
        targets.append(target)
    for field, kind, hook, location in (
            ('failStatus', 'status.failure', 'writer.fault', 'eventStore.append inside writer.writeTail'),
            ('failAppend', 'registry.persist.failure', 'registry.persist-fault',
             'after memory mutation, before snapshot, inside mutationChain and file lock')):
        if field in variant:
            entered = event('status.attempt' if field == 'failStatus' else 'registry.persist.enter',
                            key=variant[field])
            failed = event(kind, key=variant[field], error={'code': 'EIO'})
            target = boundary(field, hook, location, [entered, failed], [(entered, failed)])
            target['occurrence'] = 'first-match-only' if field == 'failAppend' else 'matching-status-write'
            targets.append(target)
    return {'case': case, 'registry': cell['registry'], 'phase': phase,
            'variant': copy.deepcopy(variant), 'order': order, 'boundaries': targets}


def plans():
    return [plan(f"{cell['name']}/{variant['name']}/{phase}")
            for cell in CELLS for variant in cell['variants'] for phase in ('model', 'tool')]


def execution(stimulus, evidence=None):
    """s2-s4 adapter seam (#2396): attest the plan against raw product receipts.

    ``evidence`` is the collector's per-case record (``stimulus_driver``).
    Without it the s1 behaviour stands: no target is attested, so neither a
    named request nor matching terminal output authorizes WITNESSED. With it,
    every boundary must be covered by case/run/plan-bound raw receipts; a
    target without its receipts keeps the row NOT_RUN with a precise missing.
    """
    if evidence is None:
        return {'status': 'NOT_RUN', 'reason': '未执行：缺少受控入口及目标边界见证',
                'missing': [item['name'] for item in stimulus['boundaries']],
                'witnesses': [], 'plan': stimulus}
    witnesses, missing = [], []
    for target in stimulus['boundaries']:
        found, absent = _target_witnesses(stimulus, target, evidence)
        if absent:
            missing.append(target['name'] + ': ' + '; '.join(absent))
        else:
            witnesses.append({'target': target['name'], 'evidence': found})
    if missing:
        return {'status': 'NOT_RUN',
                'reason': '未执行：目标边界缺少原始回执',
                'missing': missing, 'witnesses': witnesses, 'plan': stimulus}
    return {'status': 'WITNESSED', 'reason': '受控入口逐目标原始回执齐全',
            'missing': [], 'witnesses': witnesses, 'plan': stimulus}


def _loop(evidence, phase, key=None, site='loop'):
    return [receipt for receipt in evidence.get('loopReceipts', [])
            if receipt.get('site') == site and receipt.get('phase') == phase
            and (key is None or receipt.get('key') == key)]


def _numeric_seq(value):
    """A plain int seq, excluding bool (``isinstance(True, int)`` is True).

    The frozen projection types the field as a plain int
    (``native_evidence.py``); a boolean must never satisfy a seq edge or
    match a release record (#2396 fix-r4, DS boundary P2#2), and neither
    may a float — the projection never emits one, so a float seq is
    synthetic or truncated evidence and fails closed (#2396 fix-r5,
    DS boundary P2#2: the docstring said plain int while the check
    accepted floats).
    """
    return isinstance(value, int) and not isinstance(value, bool)


def _seq_lt(before, after):
    """True only when both receipts carry a numeric seq in strict order.

    A receipt without a seq (synthetic or truncated evidence) never
    attests an ordering edge — the edge lands in `absent` instead of
    crashing the ruler (#2396 fix-r2, anthropic boundary P1).
    """
    a, b = before.get('seq'), after.get('seq')
    return _numeric_seq(a) and _numeric_seq(b) and a < b


def _storage(evidence, site, phase, key):
    """Receipts at one storage boundary.

    A `release` receipt carrying ``bounded: true`` is the product's own
    bound expiring and self-releasing the blocked write — not the
    collector's controlled release — so it never attests the release
    edge (#2396 fix-r1, codex boundary P0 / DS boundary P1).
    """
    return [receipt for receipt in evidence.get('storageReceipts', [])
            if receipt.get('site') == site and receipt.get('phase') == phase
            and receipt.get('key') == key
            and not (phase == 'release' and receipt.get('bounded') is True)]


def _queued(evidence, site):
    return [receipt for receipt in evidence.get('storageReceipts', [])
            if receipt.get('site') == site and receipt.get('phase') == 'queued']


def _frames(evidence, kind, event=None, code=None):
    # This case's frames only: a foreign frame left in the buffer by the
    # previous turn must not witness a phase target or a settlement
    # (#2396 fix-r2, DS sibling P1). Fail-closed on a missing requestId —
    # the collector always writes one.
    request = evidence.get('requestId')
    found = []
    for frame in evidence.get('frames', []):
        if request is None or frame.get('id') != request:
            continue
        if frame.get('type') != kind:
            continue
        if event is not None and frame.get('event') != event:
            continue
        if code is not None and frame.get('code') != code:
            continue
        found.append(frame)
    return found


def _actions(evidence, name, **match):
    return [action for action in evidence.get('controlActions', [])
            if action.get('action') == name
            and all(action.get('detail', {}).get(field) == value
                    for field, value in match.items())]


def _release_action(evidence, seq):
    """The release-storage control record that released one barrier.

    ``detail.seq=True`` equals ``1`` under ``==``, so a boolean must not
    key a release record — a boolean detail never matches (#2396 fix-r5,
    DS boundary P2#2: the plain ``_actions(..., seq=seq)`` match let
    ``True`` stand in for seq 1 and attest the rearm edge).
    """
    return [action for action in _actions(evidence, 'release-storage')
            if _numeric_seq(action.get('detail', {}).get('seq'))
            and action['detail']['seq'] == seq]


def _registry_types(evidence):
    return {event.get('type') for event in evidence.get('registryEvents', [])
            if isinstance(event, dict)}


def _registry_keys(evidence):
    """Registry event keys in the stimulus plan's `registry_event_key` form."""
    keys = set()
    for event in evidence.get('registryEvents', []):
        if not isinstance(event, dict):
            continue
        if event.get('type') == 'agent_status' and event.get('event'):
            keys.add('agent_status:' + event['event'])
        elif event.get('type') == 'error' and event.get('code'):
            keys.add('error:' + event['code'])
    return keys


def _settlement_witnesses(stimulus, evidence):
    """The native timeout-settlement evidence for `timer.fire`/`status.attempt`.

    Under a registry the enqueued timeout frames are dropped (A19-HOST-001),
    so the settlement surfaces as registry events; without one the client
    frame is the raw witness; a held writer barrier adds its own receipts.
    """
    found = []
    found += _storage(evidence, 'writer', 'block', 'turn_timeout')
    found += [{'registryEvent': event} for event in evidence.get('registryEvents', [])
              if isinstance(event, dict) and event.get('type') == 'agent_status'
              and event.get('event') == 'turn_timeout']
    found += _frames(evidence, 'agent_status', event='turn_timeout')
    return found


def _plan_pids(evidence):
    """The pids the plan's own input/wait receipts name (key=planId).

    Final/return loop receipts are this case's only when their pid is in
    this set: a receipt trailing in from the previous case's turn carries
    that turn's pid (#2396 fix-r1, codex boundary P0 / sibling P1 / DS P2).
    """
    plan_id = evidence.get('planId')
    return {receipt.get('pid') for receipt in evidence.get('loopReceipts', [])
            if receipt.get('site') == 'loop' and receipt.get('key') == plan_id
            and receipt.get('phase') in ('input', 'wait')
            and receipt.get('pid') is not None}


def _final_frame(evidence):
    """The final frames of this case's request, never another request's.

    Fail-closed on a missing requestId: the collector always writes one,
    so an evidence record without it attests nothing (#2396 fix-r2, DS
    boundary P2)."""
    request = evidence.get('requestId')
    return [frame for frame in evidence.get('frames', [])
            if frame.get('final')
            and request is not None and frame.get('id') == request]


def _numeric_ms(value):
    """A measured duration in ms: int or float, excluding bool.

    ``timeoutMs`` arrives as a plain int, ``finalElapsedMs`` is a float by
    construction (``stimulus_driver.py`` rounds the measured span) — unlike
    ``seq`` these fields are driver-measured, not projection-typed, so the
    float stays admissible; only a boolean never is (#2396 fix-r5,
    anthropic sibling P2).
    """
    return isinstance(value, (int, float)) and not isinstance(value, bool)


def _timer_cleared(evidence):
    """Native `timer.clear`/`timer.fire.skipped`: the release beat the timer."""
    notes = evidence.get('notes', {})
    return (notes.get('noTimeoutFrameBeforeFinal') is True
            and _numeric_ms(notes.get('timeoutMs'))
            and _numeric_ms(notes.get('finalElapsedMs'))
            and notes['finalElapsedMs'] < notes['timeoutMs'])


def _target_witnesses(stimulus, target, evidence):
    """One plan target against the collected receipts.

    Returns (found, absent): the raw evidence references and the precise
    missing list. Order edges are attested through the receipt sequences and
    the collector's monotonic control-action log, never inferred from
    matching terminal output.
    """
    name = target['name']
    variant = stimulus['variant']
    found, absent = [], []
    if name == 'registry':
        construction = evidence.get('construction', {})
        receipt = construction.get('runRegistry')
        if stimulus['registry']:
            if receipt != 'on':
                absent.append('construction receipt (native run registry on)')
            if evidence.get('registryRecord') is None:
                absent.append('registry applicability (runs.json record for this run)')
            if not evidence.get('registryEvents'):
                absent.append('registry events (runs.json events for this run)')
            found.append({'construction': receipt})
        else:
            if receipt != 'off':
                absent.append('closure receipt (native run registry off)')
            if not evidence.get('runsJsonAbsent'):
                absent.append('closure check (runs.json absent across the case)')
            found.append({'construction': receipt, 'runsJsonAbsent':
                          evidence.get('runsJsonAbsent')})
    elif name == 'phase':
        prefix_type = 'tool_call_started' if stimulus['phase'] == 'tool' else 'model_request_started'
        plan_id = evidence.get('planId')
        inputs, waits = _loop(evidence, 'input', plan_id), _loop(evidence, 'wait', plan_id)
        prefix = _frames(evidence, prefix_type)
        if not inputs:
            absent.append('loop/input receipt (key=' + str(plan_id) + ')')
        if not waits:
            absent.append('loop/wait receipt (key=' + str(plan_id) + ')')
        if not prefix:
            absent.append('client.frame(' + prefix_type + ')')
        if waits and waits[0].get('planPhase') != stimulus['phase']:
            absent.append('loop/wait planPhase=' + str(waits[0].get('planPhase')))
        if inputs and waits and not _seq_lt(inputs[0], waits[0]):
            absent.append('loop input.seq < wait.seq bracket')
        if stimulus['registry']:
            if prefix_type not in _registry_types(evidence):
                absent.append('registry.append.attempt(' + prefix_type + ')')
            found.append({'registryEvent': prefix_type})
        else:
            # Without a registry the delivered client frame is the queue's
            # downstream evidence and the loop/input receipt brackets the
            # emission; there is no further native surface to demand.
            found.append({'clientFrame': prefix_type, 'loopInput': plan_id})
        found += inputs[:1] + waits[:1]
    elif name == 'order':
        absent += _witness_order(stimulus, evidence, found)
    elif name == 'slowStatus':
        blocked = _storage(evidence, 'writer', 'block', 'turn_timeout')
        released = _storage(evidence, 'writer', 'release', 'turn_timeout')
        queued = _queued(evidence, 'writer')
        fifo = _actions(evidence, 'control.status.fifo.checked')
        if not blocked:
            absent.append('status.block(turn_timeout)')
        if not released:
            absent.append('status.release(turn_timeout)')
        if not queued:
            absent.append('control.status.fifo.checked (writer queued receipt)')
        if not fifo:
            absent.append('control.status.fifo.checked action')
        if blocked and queued and not _seq_lt(blocked[0], queued[0]):
            absent.append('status.block.seq < queued.seq (FIFO follower)')
        if queued and released and not _seq_lt(queued[0], released[0]):
            absent.append('queued.seq < status.release.seq')
        # Collector-side edges for the declared (unknown, fifo) and
        # (fifo, release) pairs (#2396 fix-r5, DS sibling P2#2): the
        # receipt edges above attest the product side; these pin the
        # driver's own ordering discipline on its action log — the
        # timeout-first order target already fails closed on a missing
        # release-loop/unknown record, so neither requirement is new.
        release_unknown = _actions(evidence, 'release-loop', stage='outcome',
                                    outcome='unknown')
        if release_unknown and fifo \
                and not release_unknown[0]['monotonic'] < fifo[0]['monotonic']:
            absent.append('control.release.unknown before control.status.fifo.checked')
        if fifo and blocked:
            seq = blocked[0].get('seq')
            if not _numeric_seq(seq):
                absent.append(
                    'control.status.fifo.checked before the status release (seq absent)')
            else:
                rel = _release_action(evidence, seq)
                if not rel:
                    absent.append('control.status.fifo.checked before the status release '
                                  '(release record absent)')
                elif not fifo[0]['monotonic'] < rel[0]['monotonic']:
                    absent.append('control.status.fifo.checked before the status release')
        found += blocked[:1] + released[:1] + queued[:1] + fifo[:1]
        found += _storage(evidence, 'writer', 'complete', 'turn_timeout')[:1]
    elif name == 'slowCatchStatus':
        blocked = _storage(evidence, 'writer', 'block', 'gateway_submit_failed')
        released = _storage(evidence, 'writer', 'release', 'gateway_submit_failed')
        if not blocked:
            absent.append('status.block(gateway_submit_failed)')
        if not released:
            absent.append('status.release(gateway_submit_failed)')
        if blocked and released and not _seq_lt(blocked[0], released[0]):
            absent.append('status.block.seq < status.release.seq')
        found += blocked[:1] + released[:1]
    elif name == 'slowAppend':
        keys = list(target.get('blockedKeys') or [])
        special = variant.get('failStatus') == 'turn_timeout' and variant.get(
            'slowAppend') == 'turn_timeout'
        if special:
            keys = ['error:turn_timeout']
        for key in keys:
            blocked = _storage(evidence, 'registry', 'block', key)
            released = _storage(evidence, 'registry', 'release', key)
            if not blocked:
                absent.append('registry.persist.block(' + key + ')')
            if not released:
                absent.append('registry.persist.release(' + key + ')')
            found += blocked[:1] + released[:1]
        if not _queued(evidence, 'registry'):
            absent.append('control.registry.fifo.checked (registry queued receipt)')
        if not _actions(evidence, 'control.registry.fifo.checked'):
            absent.append('control.registry.fifo.checked action')
        # Product-side sequence edges (#2396 fix-r1): the status append
        # blocks, the error append queues behind it, the status append
        # releases, and only then does the re-armed error barrier block.
        if special:
            # The failed status store prevents its registry status append,
            # so the error append owns the first barrier: the same
            # block→fifo→release seq/order edges apply to it, minus the
            # status-append and rearm edges that do not exist in this
            # single-barrier flow (codex r3 sibling P1#2 — the `not
            # special` gate used to skip every storage-order check).
            base = variant['slowAppend']
            error_block = _storage(evidence, 'registry', 'block', 'error:' + base)
            error_release = _storage(evidence, 'registry', 'release', 'error:' + base)
            queued = _queued(evidence, 'registry')
            if error_block and queued and not _seq_lt(error_block[0], queued[0]):
                absent.append('registry.persist.block.seq < queued.seq (FIFO follower)')
            if queued and error_release and not _seq_lt(queued[0], error_release[0]):
                absent.append('queued.seq < release.seq (FIFO before release)')
            if error_block and error_release \
                    and not _seq_lt(error_block[0], error_release[0]):
                absent.append('error block.seq < release.seq')
            # Collector-side edge on the driver's own action log: the FIFO
            # sighting precedes the controlled release (#2396 fix-r5, DS
            # sibling P1 + boundary P1 — two families: the r4 branch
            # attested block→fifo and block→release but never the declared
            # (fifo, release) edge itself).
            fifo_action = _actions(evidence, 'control.registry.fifo.checked')
            if fifo_action and error_block:
                seq = error_block[0].get('seq')
                if not _numeric_seq(seq):
                    absent.append(
                        'control.registry.fifo.checked before the error release (seq absent)')
                else:
                    rel = _release_action(evidence, seq)
                    if not rel:
                        absent.append('control.registry.fifo.checked before the error release '
                                      '(release record absent)')
                    elif not fifo_action[0]['monotonic'] < rel[0]['monotonic']:
                        absent.append('control.registry.fifo.checked before the error release')
        else:
            base = variant['slowAppend']
            status_block = _storage(evidence, 'registry', 'block', 'agent_status:' + base)
            status_release = _storage(evidence, 'registry', 'release', 'agent_status:' + base)
            error_block = _storage(evidence, 'registry', 'block', 'error:' + base)
            error_release = _storage(evidence, 'registry', 'release', 'error:' + base)
            queued = _queued(evidence, 'registry')
            if status_block and queued and not _seq_lt(status_block[0], queued[0]):
                absent.append('registry.persist.block.seq < queued.seq (FIFO follower)')
            if status_block and status_release \
                    and not _seq_lt(status_block[0], status_release[0]):
                absent.append('registry.persist.block.seq < release.seq (status append)')
            if status_release and error_block \
                    and not _seq_lt(status_release[0], error_block[0]):
                absent.append('status release before the re-armed error block')
            if error_block and error_release \
                    and not _seq_lt(error_block[0], error_release[0]):
                absent.append('error block.seq < release.seq')
            # Collector-side edge: the second barrier is armed before the
            # first is released, so the driver's fifo sighting precedes the
            # re-arm, which precedes the first barrier's release.  The
            # driver always re-arms in this branch (every non-special
            # slowAppend variant has a second barrier), so a missing rearm
            # or a missing matching release-storage control record is
            # incomplete evidence, not an edge to silently skip (#2396
            # fix-r4, codex sibling P1#1 + DS boundary P2#1 — two
            # families).
            fifo_action = _actions(evidence, 'control.registry.fifo.checked')
            rearm = _actions(evidence, 'rearm-storage')
            if not rearm:
                absent.append('rearm-storage (the second barrier armed before the first release)')
            if fifo_action and rearm and not fifo_action[0]['monotonic'] < rearm[0]['monotonic']:
                absent.append('control.registry.fifo.checked before rearm-storage')
            if rearm and status_block:
                seq = status_block[0].get('seq')
                if not _numeric_seq(seq):
                    # The rearm-before-release edge needs the block's numeric
                    # seq to find the matching release; a receipt without one
                    # never attests it — the edge lands in `absent`, the same
                    # fail-closed rule as every other seq edge instead of a
                    # silent skip (#2396 fix-r3, DS concurrency P2).
                    absent.append('rearm-storage before the first registry release (seq absent)')
                else:
                    first_release = _release_action(evidence, seq)
                    if not first_release:
                        absent.append(
                            'rearm-storage before the first registry release (release record absent)')
                    elif not rearm[0]['monotonic'] < first_release[0]['monotonic']:
                        absent.append('rearm-storage before the first registry release')
        found += _queued(evidence, 'registry')[:1]
        found += _actions(evidence, 'control.registry.fifo.checked')[:1]
    elif name == 'failStatus':
        key = variant['failStatus']
        fault = _storage(evidence, 'writer', 'fault', key)
        if not fault:
            absent.append('status.failure(' + key + ', EIO)')
        found += fault[:1]
        if key in ('turn_timeout',) and stimulus['registry'] \
                and 'agent_status:' + key in _registry_keys(evidence):
            absent.append('failed status was still registry-appended (' + key + ')')
    elif name == 'failAppend':
        key = variant['failAppend']
        fault = _storage(evidence, 'registry', 'fault', key)
        if not fault:
            absent.append('registry.persist.failure(' + key + ', EIO)')
        found += fault[:1]
        if key in _registry_keys(evidence):
            absent.append('faulted append is still present in runs.json (' + key + ')')
    return found, absent


def _witness_order(stimulus, evidence, found):
    """Order targets per the five completion orderings (plan.md section 3)."""
    order = stimulus['order']
    variant = stimulus['variant']
    absent = []
    pids = _plan_pids(evidence)

    def plan_bound(receipts):
        return [receipt for receipt in receipts if receipt.get('pid') in pids]

    release_unknown = _actions(evidence, 'release-loop', stage='outcome', outcome='unknown')
    release_known = _actions(evidence, 'release-loop', stage='outcome', outcome='known')
    final_unknown = plan_bound(_loop(evidence, 'final', 'result_unknown'))
    final_completed = plan_bound(_loop(evidence, 'final', 'completed'))
    final_suppressed = plan_bound(_loop(evidence, 'final', 'completed-suppressed'))
    final = _final_frame(evidence)
    settlement = _settlement_witnesses(stimulus, evidence)
    if order in ('unknown-first', 'known-final-first'):
        outcome = final_unknown if order == 'unknown-first' else final_completed
        released = release_unknown if order == 'unknown-first' else release_known
        if not outcome:
            absent.append('loop.final(' + ('result_unknown' if order == 'unknown-first'
                                            else 'completed') + ')')
        if not released:
            absent.append('control.release.' + ('unknown' if order == 'unknown-first'
                                                else 'known'))
        if not _timer_cleared(evidence):
            absent.append('timer.clear (no turn_timeout frame, final before the timer)')
        if not final:
            absent.append('queue.close/client.done (final frame)')
        found += outcome[:1] + released[:1] + final[:1]
        found.append({'timerCleared': _timer_cleared(evidence)})
    elif order == 'known-final-before-timeout-pending':
        # The product's own receipt keys the held generator return as
        # phase=`return`, key=`block` (controlled_loop.rs), not the plan id;
        # the pid binding is what makes it this case's block.
        blocked = plan_bound(_loop(evidence, 'return', 'block'))
        returned = _actions(evidence, 'release-loop', stage='return')
        # After the timeout-decided final the bounded delivery of the buffered
        # turn_completed either lands within the drain budget (`completed`) or
        # is suppressed (`completed-suppressed`); the cell's invariant is the
        # release order, not which of the two the drain window allowed.
        finals = final_completed + final_suppressed
        if not blocked:
            absent.append('loop.return.block')
        if not returned:
            absent.append('loop.return release')
        if evidence.get('notes', {}).get('returnReleaseAfterFinal') is not True:
            absent.append('loop.return release after the timer-decided final')
        if not settlement:
            absent.append('timer.fire settlement')
        if not final:
            absent.append('queue.close (final frame)')
        if not finals:
            absent.append('loop.final(completed|completed-suppressed)')
        found += blocked[:1] + returned[:1] + finals[:1] + final[:1]
    elif order == 'after-complete':
        fence = _loop(evidence, 'complete', evidence.get('runId'), site='fence')
        if not settlement:
            absent.append('timer.fire settlement')
        if not final:
            absent.append('queue.close/client.done (final frame)')
        if not fence:
            absent.append('fence.complete(' + str(evidence.get('runId')) + ')')
        if not release_unknown:
            absent.append('control.release.unknown')
        if not final_unknown:
            absent.append('loop.final(result_unknown)')
        # after-complete: the fence gates the outcome release, so the
        # release must land at or after the final frame (#2396 fix-r1).
        if evidence.get('notes', {}).get('outcomeReleaseAfterFinal') is not True:
            absent.append('outcome release after the fence-gated final')
        found += fence[:1] + release_unknown[:1] + final_unknown[:1] + final[:1]
        found += settlement[:1]
    else:
        if not settlement:
            absent.append('timer.fire/status.attempt settlement')
        if not final:
            absent.append('queue.close (final frame)')
        if not release_unknown:
            absent.append('control.release.unknown')
        if not final_unknown:
            absent.append('loop.final(result_unknown)')
        # timeout-first order edge (#2396 fix-r1, codex sibling P1 / DS
        # boundary P2): a held write (slowStatus/slowAppend) releases the
        # unknown strictly before the final frame — the native final is
        # itself gated behind those barriers (#1370) — while every other
        # variant releases it only once the final is seen.
        expected_after = not (variant.get('slowStatus') or variant.get('slowAppend'))
        if evidence.get('notes', {}).get('outcomeReleaseAfterFinal') is not expected_after:
            absent.append('outcome release ' + ('after' if expected_after else 'before')
                          + ' the final frame (timeout-first)')
        found += settlement[:1] + final[:1] + release_unknown[:1] + final_unknown[:1]
        found.append({'outcomeReleaseAfterFinal':
                      evidence.get('notes', {}).get('outcomeReleaseAfterFinal')})
    return absent

