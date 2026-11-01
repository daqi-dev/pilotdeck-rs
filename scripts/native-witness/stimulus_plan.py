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


def execution(stimulus):
    """s2-s4 adapter seam: no target is attested by the current text provider.

    Future adapters must supply case/run-bound raw boundary receipts, including
    order/FIFO witnesses. Neither a named request nor matching terminal output
    authorizes WITNESSED. Keeping every target pending is intentional in s1.
    """
    return {'status': 'NOT_RUN', 'reason': '未执行：缺少受控入口及目标边界见证',
            'missing': [item['name'] for item in stimulus['boundaries']],
            'witnesses': [], 'plan': stimulus}
