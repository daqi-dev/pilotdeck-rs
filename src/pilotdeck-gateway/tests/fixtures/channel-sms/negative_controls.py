#!/usr/bin/env python3
"""#1339 compiled controls: compile every switch once, then run the selected
positive, negative and restored cases with negative_suite verdicts. No source
rewriting, no alternate HEAD and no second target directory. Every switch
mutates exactly one sms-channel behavior and is killed by the oracle replay
cell that pins that behavior's TS bytes (11 switches). Run one sample in the
authoring session; the coordinator runs the complete list after the patch is
committed.

`mutating_sources` cannot be used for a pre-commit change (it requires
src/ == HEAD), so the controls are the #996 compile-time switches the
channel already carries; see the diffs ledger for the deviation note.
"""
import argparse
import os
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from _shared import mutant_switch
ROOT = Path(__file__).resolve().parents[5]
FIXTURES = ROOT / 'src/pilotdeck-gateway/tests/fixtures'
SMS = ROOT / 'src/pilotdeck-gateway/src/channel/sms'
PREFIX = 'channel::sms::tests::'
switches = [
    # stop reason replaced by the default: the drain stop of the stop-during-
    # turn cell logs "no reason" instead of the recorded reason.
    ('sms-stop-log-reason', SMS / 'mod.rs', 'stop_during_turn',
     '#1339 sms oracle: stop-during-turn'),
    # TWIML body replaced by the short form: the valid-signature cell's 200
    # response body loses the XML declaration.
    ('sms-twiml-body', SMS / 'mod.rs', 'valid_signature',
     '#1339 sms oracle: valid-signature'),
    # signature gate off: the bad-signature cell is dispatched instead of 403.
    ('sms-signature-gate-off', SMS / 'mod.rs', 'bad_signature',
     '#1339 sms oracle: bad-signature'),
    # empty-From dispatch: the empty-from cell reaches the mapper and turns
    # the TWIML-only record into gateway traffic.
    ('sms-empty-from-dispatch', SMS / 'mod.rs', 'empty_from_and_whitespace_body',
     '#1339 sms oracle: empty-from-and-whitespace-body'),
    # elicitation cancel text replaced: the cancel reply of the
    # elicitation-cancel cell no longer matches TS.
    ('sms-elicit-cancel-text', SMS / 'mod.rs', 'elicitation_cancel',
     '#1339 sms oracle: elicitation-cancel'),
    # active-chat dedupe off: the second message of the active-dedupe cell
    # starts a second turn instead of the status reply.
    ('sms-active-dedupe-off', SMS / 'mod.rs', 'active_dedupe',
     '#1339 sms oracle: active-dedupe'),
    # turn error reply text replaced: the turn-error cell's user-facing
    # failure text no longer matches TS.
    ('sms-error-reply-text', SMS / 'mod.rs', 'turn_error',
     '#1339 sms oracle: turn-error'),
    # in-flight status text replaced: the decide-in-flight-status cell's
    # status reply no longer matches TS.
    ('sms-permission-status-text', SMS / 'permission.rs',
     'permission_decide_in_flight_status',
     '#1339 sms oracle: permission-decide-in-flight-status'),
    # deny confirmation text replaced: the permission-deny cell's confirmation
    # message no longer matches TS.
    ('sms-permission-confirmation-text', SMS / 'permission.rs', 'permission_deny',
     '#1339 sms oracle: permission-deny'),
    # /new command off: the new-session cell keeps the general session key
    # instead of minting the s_ uuid session.
    ('sms-mapper-new-command', SMS / 'mapper.rs', 'new_session',
     '#1339 sms oracle: new-session'),
    # tool-failure fragment dropped: the turn-reply-fragments cell's rendered
    # body loses the "⚠️ ... failed" fragment.
    ('sms-render-failure-fragment', SMS / 'render.rs', 'turn_reply_fragments',
     '#1339 sms oracle: turn-reply-fragments'),
]
GUARDED = [SMS / name for name in (
    'mod.rs', 'gateway.rs', 'mapper.rs', 'render.rs', 'elicitation.rs',
    'permission.rs', 'sender.rs', 'tests.rs',
)] + [
    ROOT / 'src/pilotdeck-gateway/src/channel/mod.rs',
    ROOT / 'src/pilotdeck-gateway/src/mutants.rs',
    ROOT / 'src/pilotdeck-gateway/src/lib.rs',
    ROOT / 'src/pilotdeck-gateway/tests/common/baseline.rs',
    ROOT / 'src/pilotdeck-gateway/Cargo.toml', ROOT / 'src/Cargo.toml', ROOT / 'src/Cargo.lock',
    FIXTURES / '_shared/mutant_switch.py', FIXTURES / 'negative_suite.py', FIXTURES / 'negative_process.py',
    Path(__file__), Path(__file__).with_name('oracle.json'),
    Path(__file__).with_name('generate.mts'), Path(__file__).with_name('README.md'),
]

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check', action='store_true')
    parser.add_argument('output', nargs='?')
    parser.add_argument('names', nargs='*')
    args = parser.parse_args()
    try:
        rows = [dict(zip(('name', 'path', 'test', 'anchor'), row)) for row in switches]
        chosen = mutant_switch.selected(rows, args.names)
        mutant_switch.preflight(rows)
        if args.check:
            if args.output or args.names:
                raise ValueError('--check does not take output or names')
            print(f'{len(rows)} switch anchors and bindings checked')
            return 0
        if not args.output or not os.environ.get('CARGO_TARGET_DIR'):
            raise ValueError('NEW_OUTPUT_DIR and caller CARGO_TARGET_DIR are required')
        output = Path(args.output).resolve()
        output.mkdir(parents=True, exist_ok=False)
        return mutant_switch.run(ROOT, output, chosen,
            mutant_switch.clean_environment(os.environ), guarded=GUARDED,
            prefix=PREFIX, script=Path(__file__), requested=args.names,
            test_timeout=480)
    except (ValueError, OSError) as error:
        print(f'preflight: {error}', file=sys.stderr)
        return 2

if __name__ == '__main__':
    sys.exit(main())
