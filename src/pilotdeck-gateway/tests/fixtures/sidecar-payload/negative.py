#!/usr/bin/env python3
"""One selected mutant per invocation is recommended; run after committing sources."""
import os
import re
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
if not os.environ.get('CARGO_TARGET_DIR'):
    sys.exit('CARGO_TARGET_DIR must be supplied by the caller')
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=False)
gateway = 'src/pilotdeck-gateway/src/'
mutants = {
    'embedded-drop': (gateway + 'runtime/turn/live.rs', 'pilotdeck-gateway',
        'non_final_event_non_object_payload_is_protocol_error',
        'if !final_event && payload.as_ref().and_then(JsValue::as_object).is_none() {',
        'if false && !final_event && payload.as_ref().and_then(JsValue::as_object).is_none() {'),
    'wire-lax': ('src/pilotdeck-protocol/src/module.rs', 'pilotdeck-gateway',
        'sidecar_payload_wire_matches_ts', ' || !record(get("payload"))', ''),
    'final-skip-inner': (gateway + 'runtime/turn/live.rs', 'pilotdeck-gateway',
        'final_event_payload_shape_is_left_to_inner_handler',
        'let applied = self.inner.event(event);',
        'let applied = if final_event { Box::pin(async { Ok(()) }) as HostFuture<()> } else { self.inner.event(event) };'),
    'module-error-shape': (gateway + 'host/modules/event.rs', 'pilotdeck-gateway',
        'sidecar_payload_module_matches_ts',
        'Sidecar host event does not match the active turn.', 'wrong malformed event error'),
    'bridge-stop': ('src/pilotdeck-transport/src/notifications.rs', 'pilotdeck-transport',
        'sidecar_payload_bridge_continues_after_known_failure',
        'pending.and_then(|pending| pending.wait(&event_control))',
        'pending.and_then(|pending| pending.wait(&event_control)).and_then(|response| { if response.boundary.value().get("ok") == Some(&JsValue::Bool(false)) { Err(PortError::TransportBeforeDispatch { message: "known rejection".into() }) } else { Ok(response) } })'),
}
for route, test in [('wire-event', 'wire'), ('host-bridge', 'module'), ('embedded-reader', 'embedded_reader')]:
    old = f'"name":"missing","route":"{route}"'
    mutants[f'omit-{route}'] = ('src/pilotdeck-gateway/tests/fixtures/sidecar-payload/oracle.json',
        'pilotdeck-gateway', f'sidecar_payload_{test}_matches_ts', old, old.replace(route, 'omitted'))
selected = sys.argv[2:] or list(mutants)
assert selected and all(name in mutants for name in selected), 'unknown/empty selection'
def log(name, text):
    with (out / name).open('x') as target:
        target.write(text)
def run(name, package, test):
    slots = wait_for_slot(root / 'scripts/cargo-slots.sh')
    log(name + '.slots.log', slots.stdout + slots.stderr)
    result = run_owned(['cargo', 'test', '--manifest-path', 'src/Cargo.toml', '-p', package,
                        '--lib', test, '--', '--nocapture'], cwd=root, env=os.environ.copy(), timeout=900)
    log(name + '.log', result.stdout + result.stderr)
    return result
for name in selected:
    relative, package, test, old, new = mutants[name]
    # First prove the named test exists and passes, never count compile failure.
    green = run(name + '-before', package, test)
    assert green.returncode == 0 and re.search(r'test result: ok\. 1 passed; 0 failed;', green.stdout), green.stdout + green.stderr
    with mutating_sources(root, [relative]) as originals:
        source = originals[Path(relative)].decode()
        assert source.count(old) == 1, (name, source.count(old))
        (root / relative).write_text(source.replace(old, new, 1))
        red = run(name + '-mutant', package, test)
        assert red.returncode == 101 and f'{test} ... FAILED' in red.stdout and 'test result: FAILED' in red.stdout, red.stdout + red.stderr
    restored = run(name + '-restored', package, test)
    assert restored.returncode == 0 and re.search(r'test result: ok\. 1 passed; 0 failed;', restored.stdout), restored.stdout + restored.stderr
    print(f'{name}: KILLED by test assertion; restored PASS', flush=True)
