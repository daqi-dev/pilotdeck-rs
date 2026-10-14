/** Additive schedules, registered before their first measurement.
 * Original preregistration-b.mjs is unchanged. Commit both registrations as
 * commit #1, then recollect the combined manifest. No outcome is prescribed.
 */
export const EXTRA_CASES = Object.freeze([
  ...['same-run', 'distinct-run'].map(order => ({ id: `dual/reattached-${order}`, order: `handoff-${order}`,
    schedule: ['A+B hello', 'A submit held', 'B reconnect exact A binding', 'B submit while A stream still live',
      'B observes its own stream end', 'release A', 'both reread registry', 'record whether B receives later events'] })),
  ...['permission', 'question'].map(kind => ({ id: `dual/${kind}/raced-double`, kind, order: 'raced-double',
    schedule: ['A+B hello', 'A submit pending', 'admit A and B answers before awaiting either response',
      'wait both real responses', 'release loop return', 'read all three planes'] })),
]);
