#!/usr/bin/env python3
"""Run one selected #855 pump-order mutation per invocation on a committed, idle product tree.

`gate-order` attaches the pump before the #832 durable gates (each of the three
gate scenarios must catch it on its own); `wake-recheck` removes the post-wake
`stopped` re-check at live.rs:885; `final-overtakes` lets the final skip the
reserved-slot predecessor it must publish behind. A mutant only counts as
killed when the named test itself panicked with its own #855 marker: a compile
failure, a gate/cleanup timeout or any other test's panic does not.
"""
import os
from pathlib import Path
import re
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
assert os.environ.get('CARGO_TARGET_DIR'), 'caller must set CARGO_TARGET_DIR'
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=False)
path = root / 'src/pilotdeck-gateway/src/runtime/turn/live.rs'
mutations = {
    'gate-order': (
        '''                // No connection or execute can start until both durable gates commit.
                if execute_wait.await.is_err() {
                    return Ok(ExecuteResult::Aborted { terminal: None });
                }
                let client = tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => return Ok(ExecuteResult::Aborted { terminal: None }),
                    client = client => client?,
                };
                pending_host.handoff();
                let identity = request.identity.clone();
                handler
                    .with_pump(
                        client.execute_cancellable(request, handler.clone(), cancellation),
                        identity,
                    )
                    .await''',
        '''                // No connection or execute can start until both durable gates commit.
                let identity = request.identity.clone();
                let gated = handler.clone();
                let gated_request = request;
                let gated_cancellation = cancellation;
                let gated_pending_host = pending_host;
                handler
                    .with_pump(
                        Box::pin(async move {
                            if execute_wait.await.is_err() {
                                return Ok(ExecuteResult::Aborted { terminal: None });
                            }
                            let client = tokio::select! {
                                biased;
                                _ = gated_cancellation.cancelled() => return Ok(ExecuteResult::Aborted { terminal: None }),
                                client = client => client?,
                            };
                            gated_pending_host.handoff();
                            client
                                .execute_cancellable(gated_request, gated.clone(), gated_cancellation)
                                .await
                        }),
                        identity,
                    )
                    .await''',
        'gates_pump_',
        [('gates_pump_reject_never_starts_the_pump', '#855 [reject]'),
         ('gates_pump_cancel_never_starts_the_pump', '#855 [cancel]'),
         ('gates_pump_open_pins_the_start_order', '#855 [open]')],
    ),
    'wake-recheck': (
        '                    if self.forwarding.lock().unwrap().stopped || Instant::now() < self.forwarding.lock().unwrap().deadline() { continue; }',
        '                    if Instant::now() < self.forwarding.lock().unwrap().deadline() { continue; }',
        'host_pump_final_orders_behind_the_reserved_slot',
        [('host_pump_final_orders_behind_the_reserved_slot',
          '#855: the probe batch must publish')],
    ),
    'final-overtakes': (
        '''        let delivery = {
            let mut forwarding = self.forwarding.lock().unwrap();
            if final_event {
                forwarding.reserve()
            } else {''',
        '''        let delivery = {
            let mut forwarding = self.forwarding.lock().unwrap();
            if final_event {
                let mut overtaking = forwarding.reserve();
                overtaking.previous = None;
                overtaking
            } else {''',
        'host_pump_final_orders_behind_the_reserved_slot',
        [('host_pump_final_orders_behind_the_reserved_slot',
          '#855: the final must stay parked')],
    ),
}
name = sys.argv[2]
old, new, test_filter, checks = mutations[name]

# libtest captures a panic as `thread '<full test path>' (<id>) panicked at
# <file>:<line>:<col>:` followed by the message block up to a blank line.
PANIC = re.compile(
    r"^thread '(?P<thread>[^']+)'(?: \(\d+\))? panicked at (?P<where>\S+):\n"
    r"(?P<body>.*?)(?=\n\n|\Z)",
    re.M | re.S,
)


def named_test_killed(log, test, marker):
    """Return None when the named test panicked with its own marker.

    Everything else — a compile failure, a gate or cleanup timeout, another
    test's panic — comes back as the reason this does not count as a kill.
    """
    if 'test result: FAILED' not in log:
        return 'no FAILED test result (compile failure or harness timeout)'
    blocks = [
        match for match in PANIC.finditer(log)
        if match['thread'] == test or match['thread'].endswith(f'::{test}')
    ]
    if not blocks:
        return f'no panic block for {test}'
    if any(marker in match['body'] for match in blocks):
        return None
    return f'{test} panicked without its marker {marker!r}'


def run(label):
    slot = wait_for_slot(root / 'scripts/cargo-slots.sh')
    (out / f'{label}.slots.log').write_text(slot.stdout + slot.stderr)
    result = run_owned(
        ['cargo', 'test', '--offline', '-q', '-p', 'pilotdeck-gateway',
         '--lib', test_filter],
        cwd=root / 'src', env=os.environ.copy(), timeout=900)
    (out / f'{label}.log').write_text(result.stdout + result.stderr)
    return result


with mutating_sources(root, [path]) as snapshots:
    content = snapshots[path].decode()
    assert content.count(old) == 1, (name, content.count(old))
    green = run('positive')
    assert green.returncode == 0, 'positive run must be green'
    # The filter names exactly the checked tests; anything filtered away (or a
    # wrong filter) would make the kill checks below vacuous.
    passed = re.search(r'test result: ok\. (\d+) passed; 0 failed', green.stdout)
    assert passed and int(passed.group(1)) == len(checks), \
        f'positive run must pass all {len(checks)} named tests'
    path.write_text(content.replace(old, new))
    red = run('mutant')
    log = red.stdout + red.stderr
    assert red.returncode != 0 and 'test result: FAILED' in log, \
        'must fail a test assertion, not compilation'
    for test, marker in checks:
        reason = named_test_killed(log, test, marker)
        assert reason is None, f'{name} did not kill {test}: {reason}'
assert run('restored').returncode == 0, 'restored run must be green'
print(f'PASS {name}: positive / {len(checks)} named kills / restored')
