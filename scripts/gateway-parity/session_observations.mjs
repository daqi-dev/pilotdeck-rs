/** E1 adapter: reuse A5.2a's rules verbatim; never define another scorer. */
import { readFileSync } from 'node:fs';
import { checkObservation, compareObservers, runControls } from '../a5-19/observation_rules.mjs';

const input = JSON.parse(readFileSync(process.argv[2], 'utf8'));
try {
  let controls;
  if (input.pair) {
    if (!compareObservers(...input.pair)) {
      throw Object.assign(new Error('observer projections differ'), { rule: 'OBSERVER_DIFFERENCE' });
    }
  } else {
    if (!Array.isArray(input.observations) || !input.observations.length) {
      throw Object.assign(new Error('no observations'), { rule: 'EMPTY_OBSERVER' });
    }
    for (const observation of input.observations) checkObservation(observation);
    if (input.controls) {
      const source = id => {
        const value = input.observations.find(o => o.id === id);
        if (!value) throw Object.assign(new Error(`missing control source: ${id}`), { rule: 'CONTROLS' });
        return value;
      };
      controls = runControls(source('no-registry-fast/fast/model/1'), source('registry-fast/fast/model/1'),
        source('no-registry-failure/timeout-status-fails/model/1'),
        source('registry-failure/catch-append-fails-status-fast/model/1'));
    }
  }
  console.log(JSON.stringify({ status: 'PASS', count: input.pair ? 2 : input.observations.length, controls }));
} catch (error) {
  console.log(JSON.stringify({ status: 'FAIL', rule: error.rule ?? 'MALFORMED_OBSERVER', detail: String(error) }));
  process.exitCode = 1;
}
