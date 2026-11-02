#!/usr/bin/env python3
# anchor-selfcheck: unsupported measured timeline data controls, no source mutation table
"""Recheck all declared witnesses against unmodified six_cells TS output.

For each required event, remove it in memory and require rejection; restore
and recheck. These are data controls, never native execution evidence.
"""
import sys
if not sys.flags.isolated:
    raise SystemExit('run with python3 -I')
sys.dont_write_bytecode = True
import importlib.util
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[5]
spec = importlib.util.spec_from_file_location('stimulus', ROOT / 'scripts/native-witness/stimulus_plan.py')
s = importlib.util.module_from_spec(spec)
spec.loader.exec_module(s)


def matches(row, selector):
    return isinstance(row, dict) and all(
        matches(row.get(key), value) if isinstance(value, dict) else row.get(key) == value
        for key, value in selector.items())


def require_order(target):
    # Registry construction is the sole explicitly unchecked declaration.
    # Validate independently of plan generation, even before skipping events.
    if target['name'] != 'registry' and not target['before']:
        raise ValueError('#1368 missing required order edges: ' + target['name'])


def witnessed(target, timeline):
    require_order(target)
    def positions(selector):
        return [index for index, row in enumerate(timeline) if matches(row, selector)]
    return (bool(target['events'])
            and all(positions(selector) for selector in target['events'])
            and all(positions(left) and positions(right)
                    and min(positions(left)) < min(positions(right)) for left, right in target['before']))


def verify(folder):
    registration = json.loads((folder / 'preregistration.json').read_text())
    if registration['cells'] != s.CELLS or registration['phases'] != ['model', 'tool']:
        raise ValueError('#1368 measured preregistration differs from native declaration')
    if type(registration['repeats']) is not int or registration['repeats'] <= 0:
        raise ValueError('#1368 measured repeats must be a positive integer')
    seen, event_controls, order_controls, empty_before_controls = [], 0, 0, 0
    unchecked = []
    for cell in registration['cells']:
        document = json.loads((folder / (cell['name'] + '.json')).read_text())
        for observation in document['observations']:
            case, repetition = observation['id'].rsplit('/', 1)
            plan = s.plan(case)
            if (observation['variant'] != plan['variant'] or observation['phase'] != plan['phase']
                    or observation['registry']['applicable'] is not plan['registry']):
                raise ValueError('#1368 measured registry/phase/variant mismatch: ' + case)
            timeline = observation['timeline']
            for target in plan['boundaries']:
                require_order(target)
                if not target['events']:
                    unchecked.append({'measurement': observation['id'], 'boundary': target['name'],
                                      'reason': '未执行：缺少可机检的边界事件声明'})
                    continue
                if not witnessed(target, timeline):
                    raise ValueError('#1368 missing measured target: ' + case + '/' + target['name'])
                for selector in target['events']:
                    damaged = [row for row in timeline if not matches(row, selector)]
                    if witnessed(target, damaged):
                        raise ValueError('#1368 missing-event data control SURVIVED: ' + case + '/' + target['name'])
                    event_controls += 1
                for left, right in target['before']:
                    # Move every left match behind the last right match, keeping
                    # the events present so the order check is independently hit.
                    damaged = [row for row in timeline if not matches(row, left)]
                    damaged.extend(row for row in timeline if matches(row, left))
                    if witnessed(target, damaged):
                        raise ValueError('#1368 reversed-order data control SURVIVED: ' + case + '/' + target['name'])
                    order_controls += 1
                if target['name'] != 'registry':
                    damaged = dict(target, before=[])
                    try:
                        witnessed(damaged, timeline)
                    except ValueError as error:
                        if str(error) != '#1368 missing required order edges: ' + target['name']:
                            raise
                    else:
                        raise ValueError('#1368 empty-before data control SURVIVED: ' + case + '/' + target['name'])
                    empty_before_controls += 1
                if not witnessed(target, timeline):
                    raise ValueError('#1368 restored measured target failed: ' + case)
            seen.append(observation['id'])
    expected = [plan['case'] + '/' + str(repeat) for plan in s.plans()
                for repeat in range(1, registration['repeats'] + 1)]
    if seen != expected:
        raise ValueError('#1368 measured denominator/order mismatch')
    return {'status': 'NOT_RUN' if unchecked else 'PASS',
            'uncheckedBoundaries': unchecked,
            'measurements': len(seen), 'cases': len(s.plans()),
            'missingEventControls': event_controls, 'reversedOrderControls': order_controls,
            'emptyBeforeControls': empty_before_controls,
            'scope': 'TS input and boundary validation only; native remains NOT_RUN'}


if __name__ == '__main__':
    print(json.dumps(verify(Path(sys.argv[1])), ensure_ascii=False))
