#!/usr/bin/env python3
"""#1444 steer_turn attachments-merge controls; run after commit.

The #1444 tests live in methods/tests.rs; this runner mutates only
production files that are already on HEAD. Until the #1444 tests themselves
are committed, the shared source guard refuses to run (the whole src tree
must equal HEAD), so the coordinator runs this after the commit lands.

Six mutants (taskbook hard requirement; ledger
A7.2-STEER-ATTACH-001-R2 machine criteria):
  - attachments-rejection-restored: the pre-#1444 explicit rejection
    (STEER_ATTACHMENTS_UNSUPPORTED) is reinstated for non-empty attachments —
    the merge never happens and the live-turn steer errors.
  - boundary-narrowed-to-arrays: `steer_attachments` folds the falsy
    non-nullish spellings (""/0/NaN/false) into the boundary TypeError,
    losing the composer's `!attachments || attachments.length === 0`
    short-circuit (probe a4/a5/a6).
  - allowed-read-files-dropped: the composed `allowedReadFiles` derivation is
    replaced with an always-empty list — a registered lease path no longer
    reaches the steer message (probe a18).
  - inline-path-rerouted: the empty-attachment lane is rerouted through
    `prepare_attachment_turn`, so the raw wire message cells (a11/a16/a17)
    hit the submit-lane string-message guard and error instead of embedding
    the raw value (the residual-1 architecture decision inverts; ledger
    A7.2-STEER-ATTACH-001-R2 keeps the inline path precisely for those
    cells).
  - truthy-non-iterable-accepted (fix-r1, review DS boundary #3): the
    truthy non-iterable rejection arm is reverted to an empty set, so every
    object spelling is accepted — the `{length:2}`/`{length:"0"}` cells
    (probe b3/b4) lose the boundary TypeError the composer's front door
    (`length === 0` on the raw value) still throws for.
  - profile-gate-inline-restored (fix-r1, review MM concurrency #2): the
    profile gate's FS sweep is polled inline on the executor again instead
    of `spawn_blocking` — on the current-thread host runtime a parked
    config read pins the executor and the concurrency probe cannot finish
    before the release write.

Usage: negative_controls.py --check | OUT_DIR [mutation ...]
"""
import os
import sys
from pathlib import Path

root = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_suite import Case, run_cases

STEER = root / 'src/pilotdeck-gateway/src/native_process/methods/steer.rs'
ASSEMBLY = root / 'src/pilotdeck-gateway/src/native_process/assembly.rs'

TEST = 'native_process::methods::tests::'

mutations = [
    # The merge itself: reinstating the old explicit rejection breaks the
    # live-turn merge cell first (probe a13).
    ('attachments-rejection-restored', STEER,
     '        let (content, allowed_read_files) = if attachments.is_empty() {',
     '        if !attachments.is_empty() {\n'
     '            return Err(RequestError {\n'
     '                code: "STEER_ATTACHMENTS_UNSUPPORTED".into(),\n'
     '                message: "attachments are not supported on steer_turn".into(),\n'
     '                details: None,\n'
     '                validation: None,\n'
     '            });\n'
     '        }\n'
     '        let (content, allowed_read_files) = if attachments.is_empty() {',
     TEST + 'steer_turn_merges_attachments_into_content',
     '#1444 attachments merge stays Ok with a live turn'),
    # The composer's own boundary: falsy non-nullish spellings short-circuit
    # to the plain turn (probe a4/a5/a6); narrowing them into the TypeError
    # is the submit-family semantics steer must not inherit.
    ('boundary-narrowed-to-arrays', STEER,
     '        JsValue::String(_) | JsValue::Bool(false) => Ok(Vec::new()),\n'
     '        JsValue::Number(value) if *value == 0.0 || value.is_nan() => Ok(Vec::new()),',
     '        JsValue::String(_) | JsValue::Bool(false) => Err(RequestError::new("attachments is not iterable")),\n'
     '        JsValue::Number(value) if *value == 0.0 || value.is_nan() => Err(RequestError::new("attachments is not iterable")),',
     TEST + 'steer_turn_non_array_attachments_follow_the_composer_boundary',
     # The plain loop dies at its first cell's `unwrap_or_else` panic.
     '#1444 empty string attachments must stay Ok'),
    # The allowedReadFiles derivation: dropping it leaves the registered lease
    # path out of the steer message (probe a18).
    ('allowed-read-files-dropped', STEER,
     '            let allowed_read_files = match prepared.get("allowedReadFiles") {\n'
     '                Some(JsValue::Array(paths)) => paths\n'
     '                    .iter()\n'
     '                    .filter_map(|path| path.as_string().cloned())\n'
     '                    .collect(),\n'
     '                _ => Vec::new(),\n'
     '            };',
     '            let allowed_read_files = Vec::new();',
     TEST + 'steer_turn_registers_leased_attachment_paths',
     '#1444 the lease path is registered in allowedReadFiles'),
    # The inline empty-attachment lane: rerouting it through
    # prepare_attachment_turn subjects the raw message cells (a11/a16/a17)
    # to the submit-lane string guard — the exact residual the #1444 split
    # keeps out of the aligned cells.
    ('inline-path-rerouted', STEER,
     '        let (content, allowed_read_files) = if attachments.is_empty() {',
     '        let (content, allowed_read_files) = if false {',
     TEST + 'steer_turn_message_cells_keep_the_raw_block_and_the_submit_guard',
     '#1444 number message must stay Ok'),
    # The truthy non-iterable rejection: reverting the object arm to an
    # empty set accepts every object spelling (b1/b2 already fold — the
    # mutation only has to stop the TypeError), so the b3/b4 cells
    # (`{length:2}` / `{length:"0"}`) lose the boundary error.
    ('truthy-non-iterable-accepted', STEER,
     '        JsValue::Bool(true) | JsValue::Number(_) | JsValue::Object(_) => {\n'
     '            Err(RequestError::new("attachments is not iterable"))\n'
     '        }\n',
     '        JsValue::Bool(true) | JsValue::Number(_) => {\n'
     '            Err(RequestError::new("attachments is not iterable"))\n'
     '        }\n'
     '        JsValue::Object(_) => Ok(Vec::new()),\n',
     TEST + 'steer_turn_object_attachments_follow_the_length_zero_front_door',
     '#1444 numeric length two must reach the for...of TypeError'),
    # The profile gate's FS sweep: polling it inline on the executor pins the
    # current-thread host for the parked config read; the concurrency probe
    # (10ms sleep after the FIFO rendezvous) cannot finish before the
    # releaser's bounded write.
    ('profile-gate-inline-restored', ASSEMBLY,
     '    let project = options.project.clone();\n'
     '    let home = options.home.clone();\n'
     '    let env = env.clone();\n'
     '    tokio::task::spawn_blocking(move || validate_profile_gate(&project, &home, &env, management))\n'
     '        .await\n'
     '        .map_err(|error| ProxyError::from(error.to_string()))?\n',
     '    validate_profile_gate(&options.project, &options.home, env, management)\n',
     TEST + 'steer_turn_profile_gate_does_not_block_the_serial_host_executor',
     '#1444 the profile gate\'s config load must not block the serial host executor'),
]


def mutate(content, scope, replacements):
    for old, new in replacements:
        if content.count(old) != 1 or old == new:
            raise RuntimeError(f'#1444 expired or ineffective mutation anchor: {content.count(old)} matches')
        content = content.replace(old, new, 1)
    return content


def main():
    if sys.argv[1:] == ['--check']:
        for name, path, old, new, _test, _assertion in mutations:
            mutate(path.read_text(), None, [(old, new)])
        print(f'#1444 {len(mutations)} mutation anchors OK')
        return 0
    if len(sys.argv) < 2:
        raise SystemExit('usage: negative_controls.py --check | OUT [mutation ...]')
    env = dict(os.environ)
    if not env.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
    # `run_cases` installs the receipt protocol per case (negative_suite.py);
    # anything inherited from a caller shell is stale and would turn a real
    # KILLED into INVALID (same pop list as the sibling fixtures, #1616 r3).
    for key in ('PDRS_MUTANT', 'PDRS_NEGATIVE_RECEIPTS',
                'PDRS_NEGATIVE_PARENT_TEST', 'CARGO_TERM_QUIET'):
        env.pop(key, None)
    selected_names = sys.argv[2:]
    if set(selected_names) - {row[0] for row in mutations}:
        raise SystemExit('unknown mutation name')
    selected = [Case(*row) for row in mutations
                if not selected_names or row[0] in selected_names]
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    env.update(CARGO_PROFILE_DEV_DEBUG='line-tables-only',
               CARGO_PROFILE_TEST_DEBUG='line-tables-only')
    return run_cases(root, selected, out, env, mutate, timeout=900, script=__file__)


if __name__ == '__main__':
    raise SystemExit(main())
