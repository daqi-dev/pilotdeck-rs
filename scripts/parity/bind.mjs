// Source (copied before editing):
// /Users/da/ws/PilotDeck-rust-program-20261009/build/CORE01-astra-deadline-closure-a1/harness/formal-bind.mjs
// Source sha256: c3aa2b71ae5dbd9bf64af7a3f2e7746b326abec8f75a814e41696979fdf1f2aa
// Record actual spawn identity only. Selection uses production environment variables;
// this wrapper NEVER replaces a command, args, options, or stdio.
import fs from 'node:fs';
import path from 'node:path';
import cp from 'node:child_process';
import { syncBuiltinESMExports } from 'node:module';
import { pathToFileURL } from 'node:url';
import { createHash } from 'node:crypto';

const baseline = process.env.PDRS_BASELINE;
const side = process.env.PDRS_SIDE;
const expected = process.env.PDRS_EXPECTED_SHA256;
const binary = process.env.PILOTDECK_AGENT_LOOP_SIDECAR_COMMAND;
const tsSidecar = path.join(baseline, 'dist/src/cli/pilotdeck-agent-loop-sidecar.js');
if (process.env.PARITY_MODE !== 'sidecar' || !['ts', 'rust'].includes(side) || !expected) {
  throw new Error('identity adapter requires sidecar mode, side and expected hash');
}
const digestFile = (p) => createHash('sha256').update(fs.readFileSync(p)).digest('hex');
const observations = [];
const originalSpawn = cp.spawn;
cp.spawn = function (command, args = [], options = {}) {
  const child = originalSpawn.call(this, command, args, options);
  if (command === binary || args.includes(tsSidecar)) {
    const candidate = side === 'rust' ? command : tsSidecar;
    observations.push({
      command, args: [...args], pid: child.pid,
      sha256: digestFile(candidate),
      matches: side === 'rust'
        ? command === binary && args.length === 1 && args[0] === tsSidecar
        : command === process.execPath && args.length === 1 && args[0] === tsSidecar,
    });
  }
  return child;
};
syncBuiltinESMExports();

// The Original parity adapter overwrites the COMMAND env with process.execPath.
// Use its supported production module binding to select Rust without altering
// the adapter or spawn. Keep the same positional sidecarPath as deployment.
if (side === 'rust') {
  const scenario = JSON.parse(process.env.PARITY_SCENARIO_JSON);
  const ctx = scenario.limits?.maxContextTokens ?? 65536;
  const out = scenario.limits?.maxOutputTokens ?? 8192;
  const max = scenario.limits?.maxContextMessages;
  const config = path.join(process.env.TMPDIR, `${scenario.scenarioId}.${process.env.PARITY_INVOCATION_ID}.yaml`);
  const yaml = `schemaVersion: 1
agent:
  model: parity/deterministic
  maxContextTokens: ${ctx}
  maxOutputTokens: ${out}${max ? `\n  maxContextMessages: ${max}` : ''}
model:
  providers:
    parity:
      protocol: openai
      url: ${JSON.stringify(process.env.PARITY_MOCK_BASE_URL)}
      apiKey: parity-test
      models:
        deterministic:
          capabilities:
            supportsToolUse: true
            maxContextTokens: ${ctx}
            maxOutputTokens: ${out}
telemetry:
  enabled: false
modules:
  agentLoop:
    enabled: true
    implementationId: pilotdeck-agent-loop
    contract: pilotdeck.agent-loop/v1
    transport: module-stdio-v2
    command: ${JSON.stringify(binary)}
    args: [${JSON.stringify(tsSidecar)}]
    methods: [execute]
`;
  fs.writeFileSync(config, yaml, { flag: 'wx' });
  process.env.PILOTDECK_CONFIG_PATH = config;
}

await import(pathToFileURL(path.join(baseline, 'tools/agent-loop-parity/adapters/pilotdeck_gateway_impl.mjs')));
const trace = process.env.PARITY_TRACE_OUT;
const rows = fs.readFileSync(trace, 'utf8').trim().split('\n').map(JSON.parse);
const last = rows.at(-1);
const sha = side === 'rust' ? digestFile(binary) : digestFile(tsSidecar);
const valid = observations.length > 0 && observations.every(o => o.matches && o.pid && o.sha256 === expected) && sha === expected;
fs.appendFileSync(trace, JSON.stringify({
  kind: 'harness.proof', scenarioId: last.scenarioId, q: last.q,
  invocationId: last.invocationId, sequence: last.sequence + 1,
  state: 'sidecar_identity', side, sha256: sha, expectedSha256: expected,
  spawnObserved: valid, observations,
}) + '\n');
if (!valid) throw new Error('sidecar identity did not match the sealed candidate');
