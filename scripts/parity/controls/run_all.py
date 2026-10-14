#!/usr/bin/env python3
"""Offline adversarial controls; never invokes a sidecar or the parity runner."""
import argparse
from copy import deepcopy
import json
from pathlib import Path
import shutil
import sys
import tempfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import g03_compare as G

DEFAULT_SOURCE = G.WORKTREE / '_tmp/20261014-exp001/parity-run1'


def rows(*records):
    return [dict(scenarioId='pure_text', q='control', sequence=i, **r) for i, r in enumerate(records)]


def rec(kind, **kw):
    return dict(kind=kind, **kw)


def terminal(**kw):
    return rec('terminal', **dict(outcome='completed', stopReason='completed', durableStopReason='completed', **kw))


def resequence(records):
    for i, r in enumerate(records):
        r['sequence'] = i
        r.setdefault('scenarioId', 'pure_text')
        r.setdefault('q', 'control')
    return records


def rule_pairs():
    # Valid -> invalid witnesses for each concrete G03 predicate. Synthetic
    # records use Original schema; directory-level controls below use real data.
    t = terminal()
    cases = {}
    a = rows(rec('durable.status', event='max_turns_reached'), rec('agent.status', event='max_turns_reached'), t)
    cases['E1'] = (a, resequence([a[1], a[0], a[2]]))
    a = rows(rec('durable.status', event='context_budget'), rec('context.budget', used=1), t)
    cases['E1v_context_budget_durable_before_visible'] = (a, resequence([a[1], a[0], a[2]]))
    a = rows(rec('tool.start', name='read_file', toolCallId='budget-tool'), rec('model.response'), rec('durable.status', event='max_budget_reached'), rec('agent.status', event='max_budget_reached'),
             rec('operation.terminal', outcome='failed'), rec('durable.state', durableStatusCount=1),
             rec('terminal', outcome='failed', code='agent_max_budget_reached', stopReason='max_budget'))
    b = deepcopy(a); b[0], b[1] = b[1], b[0]; resequence(b)
    cases['E2'] = (a, b)
    a = rows(rec('compact.boundary', compactionId='compact-1'), rec('durable.compaction_completed', compactionId='compact-1'),
             rec('model.request', afterCompactionId='compact-1'), t)
    cases['E3'] = (a, resequence([a[1], a[0], a[2], a[3]]))
    a = rows(rec('durable.steer', itemId='item-1'), rec('steer.applied', itemId='item-1'), rec('model.request'), t)
    cases['E4'] = (a, resequence([a[1], a[0], a[2], a[3]]))
    a = rows(rec('cancel.requested'), rec('tool.finish', name='read_file', toolCallId='cancel-tool', success=True), rec('cancel.acknowledged'), rec('terminal', outcome='cancelled', resultType='aborted', durableStopReason='aborted_streaming', stopReason='aborted_streaming'))
    b = deepcopy(a); b[1], b[2] = b[2], b[1]; resequence(b)
    cases['E5'] = (a, b)
    a = rows(rec('durable.status', event='turn_timeout'), rec('operation.terminal', outcome='result_unknown'), rec('terminal', outcome='failed'))
    b = deepcopy(a); b[1]['outcome'] = 'completed'
    cases['E6'] = (a, b)
    a = rows(rec('operation.terminal', outcome='completed'), rec('durable.state'), t)
    b = deepcopy(a[1:]); resequence(b)
    cases['E7'] = (a, b)
    a = rows(rec('operation.terminal', outcome='completed'), terminal())
    b = deepcopy(a); b[0]['outcome'] = 'result_unknown'
    cases['E8'] = (a, b)
    a = rows(rec('model.request'), rec('sidecar.lifecycle', state='parent_closed'), t)
    cases['E9'] = (a, resequence([a[1], a[0], a[2]]))
    a = rows(rec('model.response', content='first'), rec('model.response', content='second'), t)
    cases['E10'] = (a, resequence([a[1], a[0], a[2]]))
    a = rows(rec('model.response', content='first'), t)
    b = deepcopy(a); b.insert(1, deepcopy(b[0])); resequence(b)
    cases['E11'] = (a, b)
    a = rows(rec('model.request', modelView={'messages': []}), rec('tool.call', name='lookup', toolCallId='call-one'),
             rec('model.request', modelView={'messages': [{'role': 'tool', 'toolCallId': 'call-one', 'content': 'result'}]}), t)
    b = deepcopy(a); b[2]['modelView']['messages'] = []
    cases['E12'] = (a, b)
    # Moving a producer or actor must not make an event disappear into a
    # different stream. Also verify harmless cross-channel relocation passes.
    a = rows(rec('model.request'), rec('user.output', text='first'), t)
    b = deepcopy(a); b[0]['agentScope'] = 'child'
    cases['G03_channels'] = (a, b)
    return cases


def verdict_summary(result):
    return {k: result[k] for k in ('verdict', 'complete', 'passed')}


def run_controls(source):
    from controls.multimodal import run_controls as multimodal_controls

    cases = {}
    for rule, (good, bad) in rule_pairs().items():
        good, bad = deepcopy(good), deepcopy(bad)
        resequence(good); resequence(bad)
        positive = G.compare_records('pure_text', good, good)
        negative = G.compare_records('pure_text', good, bad)
        cases[rule] = {'ok': positive['verdict'] == 'PASS' and negative['verdict'] == 'FAIL' and rule in negative['triggered_rules'],
                       'positive': positive['verdict'], 'negative': negative['verdict'], 'triggered_rules': negative['triggered_rules']}
    # The intended relaxation: cross-channel interleaving alone is harmless.
    good = rows(rec('model.request'), rec('user.output', text='same'), terminal())
    reordered = resequence([deepcopy(good[1]), deepcopy(good[0]), deepcopy(good[2])])
    cases['cross_channel_positive'] = {'ok': G.compare_records('pure_text', good, reordered)['verdict'] == 'PASS'}
    source = G.artifact_dir(source)
    with tempfile.TemporaryDirectory(prefix='pdrs-controls-', dir=G.WORKTREE / '_tmp') as tmp:
        left, right = Path(tmp) / 'left', Path(tmp) / 'right'
        left.mkdir(); right.mkdir()
        for sid in G.scenario_catalog():
            name = f'{sid}.pilotdeck-current-sidecar.jsonl'
            shutil.copyfile(source / name, left / name)
            shutil.copyfile(source / name, right / name)
        positive = G.compare_directories(left, right)
        cases['real_self_positive'] = dict(ok=positive['verdict'] == 'PASS', **verdict_summary(positive))
        lp, rp = [d / 'pure_text.pilotdeck-current-sidecar.jsonl' for d in (left, right)]
        original = lp.read_text()
        def write(p, records):
            p.write_text(''.join(json.dumps(r) + '\n' for r in records))
        raw = [json.loads(line) for line in original.splitlines()]
        changed = deepcopy(raw)
        next(r for r in changed if r['kind'] == 'model.request')['modelView']['systemPrompt'] += '\nINJECTED_SEMANTIC_DIFFERENCE'
        write(rp, changed)
        result = G.compare_directories(left, right)
        cases['semantic_injection'] = dict(ok=result['verdict'] == 'FAIL' and 'G03_channels' in result['per_scenario']['pure_text']['triggered_rules'], **verdict_summary(result))
        rp.unlink()
        result = G.compare_directories(left, right)
        cases['missing_scenario'] = dict(ok=result['verdict'] == 'FAIL' and not result['complete'], **verdict_summary(result))
        # Valid JSONL prefix, then malformed cut in the middle of a record.
        for name, data in [('truncated_prefix', ''.join(json.dumps(r) + '\n' for r in raw[:10])), ('truncated_json', original[:-20])]:
            rp.write_text(data)
            result = G.compare_directories(left, right)
            cases[name] = dict(ok=result['verdict'] == 'FAIL', **verdict_summary(result))
        lp.write_text(''); rp.write_text('')
        result = G.compare_directories(left, right)
        cases['both_empty_traces'] = dict(ok=result['verdict'] == 'FAIL', **verdict_summary(result))
        changed = deepcopy(raw)
        end = next(r for r in changed if r['kind'] == 'terminal')
        end.update(outcome='failed', code='SAME_BROKEN_ERROR', stopReason='failed', durableStopReason='failed')
        write(lp, changed); write(rp, changed)
        result = G.compare_directories(left, right)
        cases['both_same_error'] = dict(ok=result['verdict'] == 'FAIL' and 'trace_health' in result['per_scenario']['pure_text']['triggered_rules'], **verdict_summary(result))
        empty = Path(tmp) / 'empty'; empty.mkdir()
        result = G.compare_directories(empty, empty)
        cases['both_empty_directories'] = dict(ok=result['verdict'] == 'FAIL' and not result['complete'], **verdict_summary(result))
    multimodal = multimodal_controls(source)
    cases.update(multimodal)
    negative_names = list(rule_pairs()) + ['semantic_injection', 'missing_scenario', 'truncated_prefix', 'truncated_json']
    negative_names += [name for name in multimodal if name != 'multimodal_set_positive']
    return {'ok': all(c['ok'] for c in cases.values()),
            'negative_controls': all(cases[n]['ok'] for n in negative_names),
            'both_broken': all(cases[n]['ok'] for n in ('both_empty_traces', 'both_same_error', 'both_empty_directories')),
            'positive_controls': all(cases[n]['ok'] for n in ('real_self_positive', 'cross_channel_positive', 'multimodal_set_positive')),
            'source': str(source), 'cases': cases}


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--source', type=Path, default=DEFAULT_SOURCE)
    ap.add_argument('--json', type=Path, default=G.WORKTREE / '_tmp/controls.json')
    args = ap.parse_args()
    try:
        result = run_controls(args.source)
    except (OSError, ValueError, TypeError, KeyError) as error:
        result = {'ok': False, 'negative_controls': False, 'both_broken': False, 'error': str(error)}
    args.json.parent.mkdir(parents=True, exist_ok=True)
    args.json.write_text(json.dumps(result, ensure_ascii=False, indent=2) + '\n')
    print(json.dumps(result, ensure_ascii=False))
    return 0 if result['ok'] else 1


if __name__ == '__main__':
    sys.exit(main())
