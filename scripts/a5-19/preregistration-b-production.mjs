/** Round 2 additive registration, written before any production-timing measurement.
 * No original registration/checker is changed. Worker must not commit.
 * If Git-before-collection is required for the final evidence: commit this file
 * alone first, recollect with the final apparatus, then commit apparatus/report.
 * This worker's collection must be labelled WORKTREE_REGISTERED_NOT_COMMITTED.
 * k=5 is repetition of each explicit schedule, not a frequency estimate.
 */
export const REPEATS = 5;
export const CASES = Object.freeze([
  ...['single', 'multi'].flatMap(scope => ['end-turn', 'terminal-commit'].map(start => ({
    id: `production-close/${scope}/${start}`, scope, start,
    schedule: [
      'A+B real hello; A target run reaches external loop wait',
      ...(scope === 'multi' ? ['A first session inserted before target; close waits for real first-session abort completion'] : []),
      'B reconnects target using exact A binding; resume_session completes',
      start === 'terminal-commit'
        ? 'start B target new-run at real old terminal registry rename commit (no await added)'
        : 'start B target new-run immediately after real router.endTurn (no await added)',
      'single: invoke close at endTurn; multi: keep first abort draining until target new loop reached',
      'record synchronous disconnect result, live WS inFlight membership, router endTurn and abort target',
      'release external loops; await real old/new WS finals and close completion; read all three planes',
    ],
  }))),
  ...['permission', 'question'].flatMap(kind => ['reconnect-first', 'close-first'].map(order => ({
    id: `production-close/${kind}/${order}`, kind, order,
    schedule: ['A+B hello; A pending external interaction',
      order === 'reconnect-first'
        ? 'B reconnect + resume; A close invokes unchanged synchronous disconnect'
        : 'A close invokes unchanged synchronous disconnect; then B reconnect + resume',
      'A old answer then B new answer; retain consume/resolve and actual abort journals',
      'release external loop; await WS final and real close completion; read all three planes'],
  }))),
]);
export const REQUIREMENTS = Object.freeze([
  'Use production submitTurn itself; disconnect wrapper returns the original synchronous value, never a Promise',
  'No extra wait in submitTurn, disconnectInteraction, router.endTurn or WS finally',
  'Only external loop/interaction waits and the existing real filesystem promises; no sleeps decide ordering',
  'Every case k=5; unchanged rules-b.checkObservation checks each independent three-plane observation',
  'Abort runId outcomes are observations; absence of a new-run hit alone is not a universal reachability proof',
  'Record source/apparatus/registration hashes and raw journals; retain failed attempts separately',
]);
