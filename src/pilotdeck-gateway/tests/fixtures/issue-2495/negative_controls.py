#!/usr/bin/env python3
"""#2495: the note-pass and hasInspectable-accumulation mutations.

Usage: negative_controls.py --check
       negative_controls.py [--compile-only] NEW_OUTPUT_DIR [mutation ...]
Every mutation must compile, fail at its bound nonce receipt, and pass again
after restoration. Source bytes must equal HEAD; no alternate baseline exists.
The three domain rows pin the #2495 boundary the TS probe fixed
(`_tmp/20261026-issue-2495/probe/probe.out`): hasInspectable is a separate
`attachments.some(...)` over the whole attachment list (never gated by the
note dedup, and counting an unregistered entry whose spelling is a member of
allowedReadFiles), while the note line itself still dedups on the lexical
spelling. The membership row pins the gate the whole pass hangs on, the
always-true row the conversion direction, and the dedup-dropped row the
#1446/#1321 note-line dedup the refactor must not lose.
"""
import argparse
import hashlib
import json
import os
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[5]
FIXTURES = Path(__file__).resolve().parents[1]
SERVICES = ROOT / 'src/pilotdeck-gateway/src/native_process/services.rs'
TESTS = ROOT / 'src/pilotdeck-gateway/src/native_process/services_tests.rs'
sys.path.insert(0, str(FIXTURES))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import ReceiptRun, invalidate_completed, judge, passed

mutations = [
    # The #2495 bug itself: the accumulation moved back behind the note
    # dedup continue, so a second entry with the same lexical spelling but
    # a different inspectable stops counting.
    ('dedup-gating-restored', SERVICES,
     '''        has_inspectable |= is_read_file_inspectable(entry);
        if !seen.insert(lexical.clone()) {
            continue;
        }''',
     '''        if !seen.insert(lexical.clone()) {
            continue;
        }
        has_inspectable |= is_read_file_inspectable(entry);''',
     'native_process::services_tests::note_dedup_keeps_second_entry_inspectable',
     "#2495 the deduped second entry's inspectable must still select the read_file form, got: {note}"),
    # The hasInspectable domain only: unregistered entries stop counting
    # even when their spelling is a member of allowedReadFiles.
    ('inspectable-registered-domain-restored', SERVICES,
     '        has_inspectable |= is_read_file_inspectable(entry);',
     '        has_inspectable |= entry.channel_key && is_read_file_inspectable(entry);',
     'native_process::services_tests::unregistered_colliding_spelling_counts_for_guidance',
     "#2495 the unregistered colliding entry's inspectable must still select the read_file form, got: {note}"),
    # The note-line domain: the whole pass restricted to channel-registered
    # entries, so a colliding unregistered spelling loses its line too.
    ('note-domain-registered-only', SERVICES,
     '''        if !allowed.contains(&lexical) {
            continue;
        }''',
     '''        if !allowed.contains(&lexical) || !entry.channel_key {
            continue;
        }''',
     'native_process::services_tests::unregistered_colliding_spelling_gets_note_line',
     '#2495 the unregistered entry naming the registered realpath spelling must get its own note line, got: {note}'),
    # The membership gate the whole pass hangs on: without it every path
    # attachment gets a line, registered or not (#1469's upload fixture).
    ('membership-gate-dropped', SERVICES,
     '''        if !allowed.contains(&lexical) {
            continue;
        }''',
     '''        if false {
            continue;
        }''',
     'native_process::services_tests::binary_container_info_diagnostic_gating',
     '#1469 only the registered binary container keeps a note line, got: {note}'),
    # The conversion direction: the accumulation stuck at true keeps the
    # read_file form even when nothing inspectable is left.
    ('inspectable-always-true', SERVICES,
     '        has_inspectable |= is_read_file_inspectable(entry);',
     '        has_inspectable = true;',
     'native_process::services_tests::note_dedup_both_non_inspectable_keeps_conversion_form',
     '#2495 two non-inspectable entries must keep the conversion form, got: {note}'),
    # The #1446/#1321 note-line dedup the refactor must not lose.
    ('note-dedup-dropped', SERVICES,
     '        if !seen.insert(lexical.clone()) {',
     '        if false {',
     'native_process::services_tests::note_name_cleaning_and_dedupe',
     '#1321 duplicate normalized paths must produce exactly one note line, got: {note}'),
]


def test_body(test, texts=None):
    if texts is None:
        texts = {TESTS: TESTS.read_text()}
    # Top-level test functions: a column-zero closing brace ends this
    # function, so a sibling's assertion cannot satisfy it.
    pattern = r'^(?:async )?fn ' + re.escape(test.rsplit('::', 1)[-1]) + r'\([^\n]*\).*?^}'
    bodies = re.findall(pattern, texts[TESTS], re.M | re.S)
    return bodies[0] if len(bodies) == 1 else ''


def check_binding_scope():
    fixture = 'fn target() {\n}\nfn sibling() {\n assert!(false, "only sibling");\n}\n'
    if 'only sibling' in test_body('target', {TESTS: fixture}):
        raise RuntimeError('#2495 binding checker accepted sibling assertion')
    if 'only sibling' not in test_body('sibling', {TESTS: fixture}):
        raise RuntimeError('#2495 binding checker lost its own assertion')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check', action='store_true')
    parser.add_argument('--compile-only', action='store_true')
    parser.add_argument('output', nargs='?')
    parser.add_argument('selected', nargs='*')
    args = parser.parse_args()
    check_binding_scope()
    stale = []
    for name, path, old, new, test, assertion in mutations:
        text = path.read_text()
        body = test_body(test)
        if (text.count(old) != 1 or old == new or
                not body or f'"{assertion}"' not in body):
            stale.append(name)
    if stale:
        raise SystemExit('#2495 stale anchor/test/assertion bindings: ' + ', '.join(stale))
    if args.check:
        print(f'#2495 {len(mutations)} unique anchors and assertion bindings: OK')
        return 0
    selected = [row for row in mutations if not args.selected or row[0] in args.selected]
    if not selected or (args.selected and len(selected) != len(args.selected)):
        raise SystemExit('#2495 unknown or repeated mutation')
    if not args.output or not os.environ.get('CARGO_TARGET_DIR'):
        raise SystemExit('#2495 output and caller CARGO_TARGET_DIR required')
    output = Path(args.output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    env = dict(os.environ)
    for key in [ReceiptRun.ENV, ReceiptRun.PARENT_ENV, 'PDRS_MUTANT', 'LIVE_E2E_MUTANT']:
        env.pop(key, None)
    guarded = [SERVICES, TESTS, Path(__file__).resolve(),
               FIXTURES / 'negative_process.py',
               FIXTURES / 'negative_suite.py',
               ROOT / 'src/pilotdeck-gateway/src/lib.rs']
    results = []
    guarded_sha256 = {}

    def run(label, run_env, test, *, compile_only=False):
        slot = wait_for_slot(ROOT / 'scripts/cargo-slots.sh', timeout=600)
        (output / f'{label}.slots.log').write_text(slot.stdout + slot.stderr)
        command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
                   '-p', 'pilotdeck-gateway', '--lib']
        command += ['--no-run'] if compile_only else [test, '--', '--exact', '--include-ignored']
        (output / f'{label}.command.json').write_text(json.dumps(command) + '\n')
        result = run_owned(command, cwd=ROOT, env=run_env, timeout=900)
        (output / f'{label}.log').write_text(result.stdout + result.stderr)
        return result

    try:
        with mutating_sources(ROOT, guarded) as originals:
            guarded_sha256 = {str(path.relative_to(ROOT)): hashlib.sha256(originals[path]).hexdigest() for path in guarded}
            for name, path, old, new, test, assertion in selected:
                item = dict(name=name, test=test, assertion=assertion,
                            status='INVALID', reason='not run', compiled=False)
                try:
                    if not args.compile_only and not passed(run(name + '-positive', env, test), test):
                        raise RuntimeError('positive bound test failed')
                    path.write_text(originals[path].decode().replace(old, new, 1))
                    if run(name + '-compile', env, test, compile_only=True).returncode:
                        raise RuntimeError('mutant did not compile; not a runtime kill')
                    item['compiled'] = True
                    if args.compile_only:
                        item.update(status='COMPILED', reason='mutated source compiled')
                    else:
                        receipt = ReceiptRun(output, name, [(test, assertion)])
                        result = run(name + '-negative', receipt.environment(env), test)
                        killed, reason, hits = judge(result, '', [(test, assertion)], receipt)
                        item.update(status='KILLED' if killed else ('SURVIVED' if passed(result, test) else 'INVALID'),
                                    reason=reason, hits=hits)
                except Exception as error:
                    item.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
                finally:
                    path.write_bytes(originals[path])
                try:
                    if not args.compile_only:
                        if not passed(run(name + '-restored', env, test), test):
                            raise RuntimeError('restored bound test failed')
                        item['restoredPass'] = True
                except Exception as error:
                    item.update(status='INVALID', reason=item['reason'] + f'; restored: {error}')
                results.append(item)
                print(f'{name}: {item["status"]}: {item["reason"]}', flush=True)
    except Exception as error:
        reason = f'source guard: {type(error).__name__}: {error}'
        invalidate_completed(results, reason)
        for name, _, _, _, test, assertion in selected[len(results):]:
            results.append(dict(name=name, test=test, assertion=assertion,
                                status='INVALID', reason=reason, compiled=False))
    counts = {state: sum(row['status'] == state for row in results)
              for state in ['KILLED', 'SURVIVED', 'INVALID', 'COMPILED']}
    temporary = output / 'summary.json.tmp'
    temporary.write_text(json.dumps(
        {'controls': results, 'counts': counts, 'guardedSha256': guarded_sha256}, indent=2) + '\n')
    temporary.replace(output / 'summary.json')
    print(f'summary: KILLED={counts["KILLED"]}, SURVIVED={counts["SURVIVED"]}, '
          f'INVALID={counts["INVALID"]}', flush=True)
    print(json.dumps({'controls': results, 'counts': counts}), flush=True)
    success = 'COMPILED' if args.compile_only else 'KILLED'
    return int(any(row['status'] != success for row in results))


if __name__ == '__main__':
    raise SystemExit(main())
