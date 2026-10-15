/** Registered before the new four-cell collection; historical runs were post-hoc. */
export const SIDECAR_FOUR = Object.freeze({
  cases: ['sidecar/no-registry/fast/model', 'sidecar/no-registry/fast/tool',
    'sidecar/registry/fast/model', 'sidecar/registry/fast/tool'],
  repeats: 5,
  sequence: ['target Module call and host barrier', 'pause prefix consumer', 'fire host timeout',
    'observe queue close', 'real cancel request and raw child ACK', 'release host port',
    'observe actual operation terminal', 'wait gateway final queue close', 'collect three planes', 'cleanup owned child'],
  outcomes: 'Record actual terminal outcome/code/sequence; no expected outcome is prescribed.',
  clock: 'Fresh gateway deadline clock per run; retain sent absolute deadline and fixed semantic timestamps.',
  pass: 'Each exact case has five observations; full declared semantic projection stable in each group; raw read checks pass; cleanup errors fail.',
});
