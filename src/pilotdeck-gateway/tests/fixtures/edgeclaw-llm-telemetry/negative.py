#!/usr/bin/env python3
"""#868 integration controls; run after commit, with caller CARGO_TARGET_DIR.

All selected mutants are summarized, including failed build/positive/restoration.
--check-anchors is read-only. Stale anchors exit 2, never SURVIVED.
The shared suite's runner targets --lib, so this integration adapter reuses its
Case/passed helpers, adds no-run preflight and exact panic-block classification.
"""
import json
import os
from pathlib import Path
import re
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import Case, passed

root = Path(__file__).resolve().parents[5]
source = root / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/llm/transport.rs'
request = '''        self.runtime.track_feature_loop_stage(model_event(
            &selection,
            input,
            "model_request",
            false,
        ));
'''
auth = '''        let env = |name: &str| self.runtime.env(name);
        let api_key = match resolve_api_key(&self.config, &selection.provider, &env, None) {
            Ok(key) => key,
            Err(_) => {
                let auth = self
                    .runtime
                    .resolve_auth(&selection.provider, &self.config)
                    .await?;
                resolve_api_key(&self.config, &selection.provider, &env, auth.as_ref())
                    .map_err(|e| LlmError::new(e.message))?
            }
        };
'''
response = '''        self.runtime.track_feature_loop_stage(model_event(
            &selection,
            input,
            "model_response",
            false,
        ));
'''
cases = [
    ('request-after-auth', source, request + auth, auth + request, 'request_precedes_auth'),
    ('extract-tracked', source, '        extract_text(&api, &payload)',
     '        extract_text(&api, &payload).inspect_err(|error| self.runtime.track_error(error, model_event(&selection, input, "model_request", true)))',
     'response_precedes_extract_without_tracking_parse_failures'),
    ('response-after-extract', source, response + '        extract_text(&api, &payload)',
     '        let extracted = extract_text(&api, &payload)?;\n' + response + '        Ok(extracted)',
     'response_precedes_extract_without_tracking_parse_failures'),
    ('omit-retrieval', source, '["retrieve", "retrieval", "recall"]', '["retrieve", "omitted", "recall"]', 'six_phase_labels'),
    ('omit-dream', source, 'label.contains("dream")', 'false', 'six_phase_labels'),
    ('omit-rewrite', source, 'label.contains("rewrite")', 'false', 'six_phase_labels'),
    ('omit-capture', source, 'label.contains("capture")', 'false', 'six_phase_labels'),
    ('omit-extract', source, 'label.contains("extract")', 'false', 'six_phase_labels'),
    ('omit-index', source, '    } else {\n        "index"\n    };', '    } else {\n        "capture"\n    };', 'six_phase_labels'),
    ('error-category', source, '"model_request_error".into()', '"model_response_error".into()', 'only_final_request_failure_is_tracked'),
    ('error-stage-shape', source, 'model_event(&selection, input, "model_request", true)',
     'model_event(&selection, input, "model_request", false)', 'only_final_request_failure_is_tracked'),
    ('omit-final-error', source, '            self.runtime\n                .track_error(error, model_event(&selection, input, "model_request", true));',
     '            let _ = error;', 'only_final_request_failure_is_tracked'),
    ('stage-outcome', source, 'event.insert("outcome".into(), "success".into());',
     'event.insert("outcome".into(), "failure".into());', 'six_phase_labels'),
    ('metadata-label', source, 'metadata.insert("requestLabel".into(), string(&input.request_label));',
     'metadata.insert("requestLabel".into(), "wrong-label".into());', 'six_phase_labels'),
]
messages = {
    'request_precedes_auth': '#868 request precedes auth',
    'response_precedes_extract_without_tracking_parse_failures': '#868 response precedes extract and excludes parse failures',
    'six_phase_labels': '#868 all six phase labels',
    'only_final_request_failure_is_tracked': '#868 only final request failure is tracked',
}
cells = {
    'request-after-auth': 'auth-success',
    'extract-tracked': 'extract-chat-0',
    'response-after-extract': 'extract-chat-0',
    'omit-retrieval': 'telemetry-phase-retrieval',
    'omit-dream': 'telemetry-phase-DREAM',
    'omit-rewrite': 'telemetry-phase-rewrite',
    'omit-capture': 'telemetry-phase-capture',
    'omit-extract': 'telemetry-phase-extract',
    'omit-index': 'telemetry-phase-index',
    'error-category': 'chat-attempt-0-event-1',
    'error-stage-shape': 'chat-attempt-0-event-1',
    'omit-final-error': 'chat-attempt-0-event-1',
    'stage-outcome': 'telemetry-phase-retrieval',
    'metadata-label': 'telemetry-phase-retrieval',
}


def anchors():
    stale = [(name, path.read_text().count(old)) for name, path, old, _, _ in cases
             if path.read_text().count(old) != 1]
    if stale:
        for name, count in stale:
            print(f'STALE ANCHOR {name}: {count} matches', file=sys.stderr)
        raise SystemExit(2)


def classify(result, case):
    log = result.stdout + result.stderr
    if passed(result, case.test):
        return 'SURVIVED', 'bound test passed'
    blocks = re.findall(r'^---- (.+) stdout ----\n(.*?)(?=^---- |^failures:|\Z)', log, re.M | re.S)
    if (result.returncode == 101 and 'could not compile' not in log
            and re.search(r'^running 1 test$', log, re.M)
            and f'test {case.test} ... FAILED' in log
            and 'test result: FAILED. 0 passed; 1 failed;' in log
            and len(blocks) == 1 and blocks[0][0] == case.test
            and re.search(r"^thread '" + re.escape(case.test) + r"'(?: \(\d+\))? panicked at .+:\d+:\d+:$", blocks[0][1], re.M)
            and re.search(r'^assertion .* failed: ' + re.escape(case.assertion) + r'$', blocks[0][1], re.M)):
        return 'KILLED', 'bound test failed at its dedicated assertion'
    return 'INVALID', 'build error, wrong panic, timeout, or missing bound assertion'


def main():
    anchors()
    if sys.argv[1:] == ['--check-anchors']:
        print(f'{len(cases)} unique anchors; no mutation')
        return 0
    if not os.environ.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be supplied by caller')
    if len(sys.argv) < 2:
        raise SystemExit('usage: negative.py OUT_DIR [NAME ...] | --check-anchors')
    selected = set(sys.argv[2:])
    if not selected <= {row[0] for row in cases}:
        raise SystemExit('unknown mutation')
    chosen = [Case(name, path, old, new, test, messages[test] + ': ' + cells[name])
              for name, path, old, new, test in cases if not selected or name in selected]
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    command = ['cargo', 'test', '--offline', '--manifest-path', str(root / 'src/Cargo.toml'),
               '-p', 'pilotdeck-gateway', '--test', 'edgeclaw_llm_telemetry']

    def run(label, args):
        slot = wait_for_slot(root / 'scripts/cargo-slots.sh')
        with (out / f'{label}.slots.log').open('x') as log:
            log.write(slot.stdout + slot.stderr)
        try:
            result = run_owned(command + args, cwd=root, env=dict(os.environ), timeout=600)
        except Exception as error:
            with (out / f'{label}.log').open('x') as log:
                if isinstance(error, subprocess.TimeoutExpired):
                    for text in (error.stdout, error.stderr):
                        log.write(text.decode(errors='replace') if isinstance(text, bytes) else text or '')
                log.write(f'\n{type(error).__name__}: {error}\n')
            raise
        with (out / f'{label}.log').open('x') as log:
            log.write(result.stdout + result.stderr)
        return result

    results = []
    try:
        with mutating_sources(root, [source]) as originals:
            for case in chosen:
                state, reason = 'INVALID', 'positive failed'
                try:
                    if passed(run(case.mutation + '-before', [case.test, '--', '--exact']), case.test):
                        source.write_text(originals[source].decode().replace(case.old, case.new))
                        if run(case.mutation + '-compile', ['--no-run']).returncode != 0:
                            reason = 'mutant did not compile'
                        else:
                            state, reason = classify(run(case.mutation, [case.test, '--', '--exact']), case)
                except Exception as error:
                    state, reason = 'INVALID', f'{type(error).__name__}: {error}'
                finally:
                    source.write_bytes(originals[source])
                try:
                    if not passed(run(case.mutation + '-restored', [case.test, '--', '--exact']), case.test):
                        state, reason = 'INVALID', reason + '; restored positive failed'
                except Exception as error:
                    state, reason = 'INVALID', reason + f'; restore test: {error}'
                results.append({'mutation': case.mutation, 'status': state, 'reason': reason})
                print(f'{case.mutation}: {state}: {reason}', flush=True)
    except Exception as error:
        # A guard/restore failure prevents further mutation but accounts for all rows.
        results = [{'mutation': c.mutation, 'status': 'INVALID', 'reason': f'source guard: {error}'} for c in chosen]
    counts = {state: sum(r['status'] == state for r in results) for state in ('KILLED', 'SURVIVED', 'INVALID')}
    with (out / 'summary.json').open('x') as log:
        json.dump({'controls': results, 'counts': counts}, log, indent=2)
        log.write('\n')
    print(json.dumps({'counts': counts, 'controls': results}), flush=True)
    return int(any(r['status'] != 'KILLED' for r in results))


if __name__ == '__main__':
    raise SystemExit(main())
