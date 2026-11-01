#!/usr/bin/env python3
"""#201 controls: compile each mutant, require its bound runtime assertion.

Guards the four registered #201 deviations (A6.2-LEDGER-SEED-WRITE-TIMING-001,
A6.2-LEDGER-RESTORE-INPUT-STRICTNESS-001,
A6.2-LEDGER-ONLINE-ADMISSION-STRICTNESS-001,
A6.2-LEDGER-RESTORE-DOMAIN-SCOPE-001). Run after committing the patch, with no
other writer/build in this worktree: every anchor is checked against HEAD
bytes, every selected case is accounted for, and a mutant only counts as killed
when its bound test fails at its own #201 assertion message.
"""
import argparse
import json
import os
import signal
import subprocess
import sys
from pathlib import Path

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import Case, classify, passed

root = Path(__file__).resolve().parents[5]
ledger = root / 'src/pilotdeck-gateway/src/storage/ledger.rs'
projection = root / 'src/pilotdeck-gateway/src/storage/projection.rs'
adapter = root / 'src/pilotdeck-gateway/src/storage/transcript_adapter.rs'
corrupt_test = 'storage::tests::corrupt_input_strictness_fails_closed_per_registered_entry'
online_test = 'storage::tests::online_admission_strictness_rejects_ts_accepted_inputs'
seed_test = 'storage::tests::real_ts_seed_admission_matches_on_disk_and_online'
domain_test = 'storage::tests::ledger_open_skips_domain_validation_like_the_ts_constructor'

cases = [
    # A6.2-LEDGER-RESTORE-INPUT-STRICTNESS-001: restore tolerates a garbage
    # outcome string instead of fail-closing the open.
    dict(name='restore-unknown-outcome',
         changes=[(ledger,
                   '    .ok_or_else(|| invalid("Invalid terminal outcome."))\n}',
                   '    .ok_or_else(|| invalid("Invalid terminal outcome."))\n'
                   '    .or(Ok(ModuleOutcome::ResultUnknown))\n}')],
         test=corrupt_test,
         assertion='#201 Rust opened TS-tolerated corruption: outcome-unknown-string'),
    # Same entry: restore tolerates an accepted entry without a stream id.
    dict(name='restore-accepted-missing-streamid',
         changes=[(ledger,
                   '            record.stream_id = Some(string(entry, "streamId")?);',
                   '            if let Ok(stream) = string(entry, "streamId") {\n'
                   '                record.stream_id = Some(stream);\n'
                   '            }')],
         test=corrupt_test,
         assertion='#201 Rust opened TS-tolerated corruption: accepted-missing-streamid'),
    # Same entry: restore keeps a string lastAppliedSequence for result_unknown.
    dict(name='restore-unknown-string-sequence',
         changes=[(ledger,
                   '                    _ => return Err(invalid("Invalid unknown terminal sequence.")),',
                   '                    _ => None,')],
         test=corrupt_test,
         assertion='#201 Rust opened TS-tolerated corruption: unknown-string-sequence'),
    # Same entry: restore accepts a truthy non-array messages field.
    dict(name='restore-messages-nonarray',
         changes=[(ledger,
                   '''            let JsValue::Array(messages) = field(entry, "messages") else {
                return Err(invalid(
                    "Restored AgentLoop operation terminal lacks a result projection.",
                ));
            };''',
                   '''            let messages = match field(entry, "messages") {
                JsValue::Array(messages) => messages.clone(),
                _ => Vec::new(),
            };''')],
         test=corrupt_test,
         assertion='#201 Rust opened TS-tolerated corruption: messages-string'),
    # A6.2-LEDGER-ONLINE-ADMISSION-STRICTNESS-001: online terminal() accepts a
    # truthy non-object seed instead of rejecting before any write.
    dict(name='online-seed-string',
         changes=[(projection,
                   '''    let Some(value) = value.filter(|v| truthy(v)) else {
        return Ok(None);
    };''',
                   '''    let Some(value) = value.filter(|v| truthy(v) && v.as_object().is_some()) else {
        return Ok(None);
    };''')],
         test=online_test,
         assertion='#201 string-seed: Rust accepted an online terminal that TS persisted as a deviation'),
    # Same entry: online terminal() accepts outcome=result_unknown.
    dict(name='online-result-unknown-outcome',
         changes=[(ledger,
                   '        if input.resolution.outcome == ModuleOutcome::ResultUnknown {',
                   '        if false && input.resolution.outcome == ModuleOutcome::ResultUnknown {')],
         test=online_test,
         assertion='#201 result-unknown-outcome: Rust accepted an online terminal that TS persisted as a deviation'),
    # A6.2-LEDGER-SEED-WRITE-TIMING-001: replicate the TS order — append the
    # terminal event first, propagate the seed parse error afterwards, so a
    # rejected seed leaves three durable entries like the real TS ledger.
    dict(name='seed-write-timing',
         changes=[(ledger,
                   '''        stable.resolution.seed_state = normalize_seed(serialized_seed.as_ref())?;
        let mut event = id.event("agent_loop_operation_terminal");''',
                   '''        let __mutant_seed = normalize_seed(serialized_seed.as_ref());
        let mut event = id.event("agent_loop_operation_terminal");'''),
                  (ledger,
                   '''        self.sink.append(&id.session_id, &id.turn_id, event)?;
        let record = self.records.get_mut(&id.operation_id).unwrap();''',
                   '''        self.sink.append(&id.session_id, &id.turn_id, event)?;
        stable.resolution.seed_state = __mutant_seed?;
        let record = self.records.get_mut(&id.operation_id).unwrap();''')],
         test=seed_test,
         assertion='Rust must not persist the rejected terminal'),
    # A6.2-LEDGER-RESTORE-DOMAIN-SCOPE-001: the ported validator stops treating
    # tool_call as a domain entry, so the domain-invalid log passes unnoticed.
    dict(name='domain-skip-tool-call',
         changes=[(adapter,
                   '''    "model_stream_event",
    "tool_call",
    "tool_result",''',
                   '''    "model_stream_event",
    "tool_result",''')],
         test=domain_test,
         assertion='#201 the domain-invalid log must fail domain validation'),
]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', type=Path)
    parser.add_argument('selected', nargs='*')
    args = parser.parse_args()
    if not os.environ.get('CARGO_TARGET_DIR'):
        parser.error('CARGO_TARGET_DIR must be supplied by the caller')
    selected = [case for case in cases
                 if not args.selected or case['name'] in args.selected]
    if not selected or set(args.selected) - {case['name'] for case in cases}:
        parser.error('unknown or empty selection')
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    env = dict(os.environ)
    command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
               '-p', 'pilotdeck-gateway', '--lib']

    def stop(signum, frame):
        raise SystemExit(f'interrupted by signal {signum}')

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)

    def run(label, arguments):
        slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=120)
        (out / (label + '.slots.log')).write_text(slot.stdout + slot.stderr)
        try:
            result = run_owned(command + arguments, cwd=root, env=env, timeout=600)
        except subprocess.TimeoutExpired as error:
            raw = ''.join(value.decode(errors='replace') if isinstance(value, bytes) else value or ''
                          for value in (error.stdout, error.stderr))
            (out / (label + '.log')).write_text(raw + f'\n{type(error).__name__}: {error}\n')
            raise
        (out / (label + '.log')).write_text(result.stdout + result.stderr
                                           + f'\nexit={result.returncode}\n')
        return result

    results = []
    for row in selected:
        status, reason = 'INVALID', ''
        arguments = [row['test'], '--', '--exact']
        suite = Case(mutation=row['name'], path=row['changes'][0][0],
                    old=row['changes'][0][1], new=row['changes'][0][2],
                    test=row['test'], assertion=row['assertion'])
        try:
            with mutating_sources(root, [change[0] for change in row['changes']]) as originals:
                original = originals[row['changes'][0][0]].decode()
                for _path, before, _after in row['changes']:
                    if original.count(before) != 1:
                        raise ValueError(f'ANCHOR_EXPIRED {row["name"]}: '
                                         f'{original.count(before)} matches, expected 1')
                if not passed(run(row['name'] + '-positive', arguments), row['test']):
                    raise RuntimeError('positive control did not pass the bound test')
                mutated = original
                for _path, before, after in row['changes']:
                    mutated = mutated.replace(before, after, 1)
                row['changes'][0][0].write_text(mutated)
                compiled = run(row['name'] + '-compile', ['--no-run'])
                if compiled.returncode != 0:
                    raise RuntimeError('mutant failed to compile; not a runtime kill')
                status, reason = classify(run(row['name'] + '-negative', arguments), suite)
            if not passed(run(row['name'] + '-restored', arguments), row['test']):
                status, reason = 'INVALID', 'restored control did not pass the bound test'
        except SystemExit:
            raise
        except Exception as error:
            status, reason = 'INVALID', f'{type(error).__name__}: {error}'
        results.append({'mutation': row['name'], 'test': row['test'],
                        'assertion': row['assertion'], 'status': status, 'reason': reason})
        print(f'{row["name"]}: {status}: {reason}', flush=True)
    # Publish the summary only after every mutation ran and every guarded
    # window restored and re-verified its bytes.
    counts = {state: sum(row['status'] == state for row in results)
              for state in ('KILLED', 'SURVIVED', 'INVALID')}
    payload = json.dumps({'controls': results, 'counts': counts},
                          ensure_ascii=False, indent=2) + '\n'
    temporary = out / 'summary.json.tmp'
    temporary.write_text(payload)
    temporary.replace(out / 'summary.json')
    print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()), flush=True)
    return int(any(row['status'] != 'KILLED' for row in results) or not results)


if __name__ == '__main__':
    sys.exit(main())
