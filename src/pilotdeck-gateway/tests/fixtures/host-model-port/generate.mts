import assert from 'node:assert/strict';
import { realpathSync, mkdirSync, mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve } from 'node:path';
import { bindSourceTree, sourceClosureSha256 } from '../../baseline_source_guard.mjs';
import { record } from './record.mts';
assert.equal(new Intl.Collator().resolvedOptions().locale, 'en-US');
assert.ok(process.env.PDRS_BASELINE);
assert.ok(process.env.PDRS_GIT);
assert.ok(process.argv[2]);
const baseline = resolve(process.env.PDRS_BASELINE);
const commit = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const git = realpathSync(process.env.PDRS_GIT);
const sourceCommit = bindSourceTree(baseline, commit, git).ref;
// The composition under test and everything its behavior crosses: the two
// model-invoker adapters, the sidecar bindTurn projection, the recorder and
// the transcript stack, the router and the model runtime it routes over.
const sourceSha256 = sourceClosureSha256(baseline, commit, git, [
  'src/agent/modules/llm/routerModelInvokerAdapter.ts',
  'src/agent/modules/llm/durableModelInvokerPort.ts',
  'src/agent/modules/transport/sidecarHostModulePorts.ts',
  'src/agent/modules/transport/sidecarDefaultModuleDispatcher.ts',
  'src/agent/modules/transport/sidecarToolContext.ts',
  'src/agent/session/AgentSessionEventRecorder.ts',
  'src/agent/runtime/modelContextWindow.ts',
  'src/router/RouterRuntime.ts',
  'src/model/ModelRuntime.ts',
  'src/model/streaming/streamModel.ts',
  'src/session/transcript/JsonlTranscriptWriter.ts',
  'src/session/events/SessionRuntime.ts',
  'src/session/persistence/JsonlSessionPersistence.ts',
]);
// The recording needs a private scratch directory for the transcripts.
const scratch = mkdtempSync(`${tmpdir()}/pdrs-host-model-port-`);
const cases = await record(baseline, scratch);
const output = resolve(process.argv[2]);
mkdirSync(output, { recursive: true });
writeFileSync(
  resolve(output, 'oracle.json'),
  JSON.stringify({ sourceCommit, sourceSha256, ...cases }, null, 2) + '\n',
  { flag: 'wx' },
);
