#!/usr/bin/env python3
"""#1438: the async-hook registration bridge must stay load-bearing.

Five mutations walk the bridge backwards from its consumer. The first three
revert one link each of the #1438 wiring to the pre-issue degraded state — the
state where ``hook_async_result`` could only ever answer ``unknown`` because
nothing in production ever called ``AsyncHookRegistry::register``:

* ``assembly-sink-install-skipped`` — ``build_async_hook_registry`` stops
  installing the sink into the InteractionHooks resource (the injection point
  the issue fixed). Every binding then keeps the default
  ``on_async_registered: None`` and async outputs fall back to the local
  pending list.
* ``hooks-bind-handler-install-skipped`` — the resource still carries the
  sink, but ``bind_with_options`` stops bridging it into the per-turn handler,
  so the wiring is dead one level lower.
* ``runtime-registration-conflict-swallowed`` — the handler is installed and
  forwards the registration, but its error no longer propagates (the trait's
  pre-#1438 infallible shape): an ``ASYNC_HOOK_INVOCATION_CONFLICT`` on a
  restarted run would be swallowed and the dispatch would wrongly succeed.

The last two guard the issue's second half — the steer-family behavior tests
the issue added alongside the wiring:

* ``mailbox-too-late-rejection-removed`` — the SteerMailbox's too-late guard
  stops rejecting a drained (offered) item, so the round-trip test's negative
  half would wrongly report a successful cancel.
* ``turn-gateway-abort-interception-removed`` — the ``abort_turn``
  interception in the turn gateway stops matching, dropping the method onto
  the methods registry the interception exists to shield.

Every row's kill oracle is the bound test's own receipt-wired assertion (the
#1891 precedent: text and pass/fail condition unchanged, so the kill is the
exact site, not timing). Rows run the bound test only (exact mode, one target
each); the positive and restored phases pin that the un-mutated bytes pass the
same test. The table is HEAD-guarded: run it after committing the candidate,
from a clean tree, with ``CARGO_TARGET_DIR`` supplied. ``--check`` verifies
every anchor against the working tree; a stale anchor is an error exit, never
a SURVIVED verdict.
"""
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import matrix_main

ROOT = Path(__file__).resolve().parents[5]
GATEWAY = ROOT / 'src/pilotdeck-gateway/src'
STEER_ASSEMBLY = GATEWAY / 'native_process/assembly/steer.rs'
INTERACTION_HOOKS = GATEWAY / 'runtime/project/interaction_hooks.rs'
HOOKS_RUNTIME = GATEWAY / 'runtime/hooks/runtime.rs'
TURN_GATEWAY = GATEWAY / 'runtime/turn/gateway.rs'
STEER_MAILBOX = GATEWAY / 'runtime/steer.rs'

MUTATIONS = [
    # The assembly-side injection point: the registry build hands itself to
    # the hooks resource. Skipping it reverts the whole bridge to
    # on_async_registered: None (local pending fallback).
    ('assembly-sink-install-skipped', STEER_ASSEMBLY,
     '    if let Some(hooks) = interaction_hooks {\n'
     '        hooks.set_async_hook_sink(Arc::new(GatewayAsyncHookSink {',
     '    if let Some(hooks) =\n'
     '        None::<crate::runtime::project::interaction_hooks::InteractionHooks>\n'
     '    {\n'
     '        hooks.set_async_hook_sink(Arc::new(GatewayAsyncHookSink {',
     [('native_process::assembly::steer::tests::'
       'production_hook_dispatch_registers_async_output_in_the_gateway_registry',
       '#1438 a bridged async output must not enter the local pending list')]),
    # The resource-side install: bind_with_options bridges the installed
    # sink into the per-turn handler only under this condition.
    ('hooks-bind-handler-install-skipped', INTERACTION_HOOKS,
     '        if options.on_async_registered.is_none()\n'
     '            && let Some(sink) = state.async_hook_sink.clone()\n'
     '        {',
     '        if false\n'
     '            && options.on_async_registered.is_none()\n'
     '            && let Some(sink) = state.async_hook_sink.clone()\n'
     '        {',
     [('native_process::assembly::steer::tests::'
       'production_hook_dispatch_registers_async_output_in_the_gateway_registry',
       '#1438 a bridged async output must not enter the local pending list')]),
    # The propagation link: TS throws ASYNC_HOOK_INVOCATION_CONFLICT out of
    # HookRuntime.run (InProcessGateway.ts:724-729, HookRuntime.ts:170); the
    # fallible handler's `?` is the only channel. Swallowing the error
    # restores the silent pre-#1438 shape.
    ('runtime-registration-conflict-swallowed', HOOKS_RUNTIME,
     '                                invocation_id: invocation_id.clone(),\n'
     '                                timeout_ms: r#async.timeout_ms,\n'
     '                            })?;',
     '                                invocation_id: invocation_id.clone(),\n'
     '                                timeout_ms: r#async.timeout_ms,\n'
     '                            });',
     [('native_process::assembly::steer::tests::'
       'production_async_hook_conflict_fails_the_dispatch',
       '#1438 a conflicting invocation id must fail the dispatch')]),
    # The steer-family behavior pins' guard: without the too-late rejection a
    # drained (offered) item could still be "cancelled" — the round-trip
    # test's negative half would go green.
    ('mailbox-too-late-rejection-removed', STEER_MAILBOX,
     '                if pending\n'
     '                    .as_ref()\n'
     '                    .is_some_and(|entry| entry.lock().unwrap().state != DeliveryState::Pending)\n'
     '                    || (pending.is_none() && state.seen.contains(&item_id))\n'
     '                {',
     '                if false\n'
     '                    && pending\n'
     '                        .as_ref()\n'
     '                        .is_some_and(|entry| entry.lock().unwrap().state != DeliveryState::Pending)\n'
     '                    || (pending.is_none() && state.seen.contains(&item_id))\n'
     '                {',
     [('native_process::methods::tests::'
       'cancel_steer_round_trip_empties_the_mailbox_and_reports_too_late_after_drain',
       '#1438 a drained item can no longer be cancelled')]),
    # The interception the #1891 fix-r2 review left as the sole abort_turn
    # implementation: disabling it drops the method onto the registry.
    ('turn-gateway-abort-interception-removed', TURN_GATEWAY,
     '        if method.equals_str("abort_turn") {',
     '        if method.equals_str("abort_turn-disabled") {',
     [('runtime::turn::gateway::tests::'
       'abort_turn_request_is_intercepted_before_the_methods_registry',
       '#1438 abort_turn must not reach the erroring mock registry')]),
]


def main():
    return matrix_main(ROOT, __file__, '', MUTATIONS,
                       passed_count=1, child_count=0,
                       paths=[STEER_ASSEMBLY, INTERACTION_HOOKS, HOOKS_RUNTIME,
                              STEER_MAILBOX, TURN_GATEWAY],
                       exact=True)


if __name__ == '__main__':
    raise SystemExit(main())
