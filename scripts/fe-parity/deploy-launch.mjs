// Repository-owned B deployment: same native supervisor as D, frozen backend.
import { spawn } from 'node:child_process';
import { readFile, writeFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { baseline, root, requireUI, ownedPath } from './env.mjs';
import { prepareRuntime, runtimeConfig } from './real-launch.mjs';
import { assertRealGitBin } from './real-git.mjs';
import { assertDeployExecutionEnv, deployCell, profileSource, deployPorts, deployRunRoot } from './deploy-matrix.mjs';

export async function deploymentConfig(cell, runDir) {
  const { parse } = requireUI('yaml');
  const profile = parse(await readFile(profileSource(cell), 'utf8'));
  const native = runtimeConfig('ready', runDir);
  // Preserve the baseline's module selection and settings. Replace historical
  // machine paths, public host and model endpoints with our isolated runtime.
  Object.assign(profile, { agent: { ...profile.agent, model: 'smoke/operator' },
    model: { providers: { smoke: { ...native.model.providers['e2e-openai'], models: { operator: { capabilities: { supportsToolUse: true } } } } } },
    webui: { ...profile.webui, runtime: { ...profile.webui?.runtime, ...native.webui.runtime,
      host: '127.0.0.1', serverPort: deployPorts.server, vitePort: deployPorts.vite } },
    telemetry: { enabled: false } });
  if (cell.profile === 'replacement') profile.modules.knowledge.endpoint = `http://127.0.0.1:${deployPorts.knowledge}`;
  if (cell.blocked) throw new Error(cell.blocked);
  return profile;
}
export async function prepareDeployment(cell, runDir) {
  if (cell.blocked) throw new Error(cell.blocked);
  const runtime = { ...await prepareRuntime('ready', runDir, { runRoot: deployRunRoot }),
    cell: cell.id, profile: cell.profile, ports: deployPorts };
  await writeFile(runtime.configPath, JSON.stringify(await deploymentConfig(cell, runDir), null, 2) + '\n');
  await writeFile(resolve(runDir, 'runtime.json'), JSON.stringify(runtime, null, 2) + '\n');
  await writeFile(resolve(runDir, 'deployment.json'), JSON.stringify({ cell: cell.id, profile: cell.profile,
    sourceProfile: profileSource(cell), configPath: runtime.configPath, ports: deployPorts }, null, 2) + '\n', { flag: 'wx' });
  return runtime;
}
async function main() {
  assertDeployExecutionEnv(process.env);
  const cell = deployCell(process.env.FE_B_CELL), runDir = ownedPath(process.env.PILOTDECK_E2E_RUN_DIR);
  if (cell.blocked) throw new Error(cell.blocked);
  await assertRealGitBin(process.env);
  // D's fresh HOME, project registration and workspace seed, without rm/copy.
  await prepareDeployment(cell, runDir);
  let replacement, stopping = false;
  if (cell.profile === 'replacement') {
    await writeFile(process.env.REPLACEMENT_KNOWLEDGE_EVIDENCE_PATH, '', { flag: 'wx' });
    // Import the actual baseline conformance process; no fabricated API data.
    replacement = spawn(process.execPath, [resolve(baseline, '../products/pilotdeck-staffdeck-sop/fixtures/replacement-knowledge-runtime.mjs')],
      { cwd: root, env: process.env, stdio: 'inherit' });
    replacement.once('error', error => { console.error(error); process.kill(process.pid, 'SIGTERM'); });
    replacement.once('close', () => { if (!stopping) process.kill(process.pid, 'SIGTERM'); });
  }
  const { createRuntimeSupervisor } = await import(pathToFileURL(resolve(baseline, 'server/webRuntimeSupervisor.js')));
  const supervisor = createRuntimeSupervisor({ mode: 'dev', cwd: baseline, requestFile: resolve(runDir, 'restart.request'),
    spawnImpl: (command, args, options) => {
      assertDeployExecutionEnv(options.env);
      const ownedArgs = args[0] === resolve(baseline, 'node_modules/vite/bin/vite.js')
        ? [resolve(root, 'scripts/fe-parity/deploy-vite.mjs')] : args;
      return spawn(command, ownedArgs, options);
    } });
  // Runtime group cleanup is owned by runner even on restart/error.
  for (const signal of ['SIGINT', 'SIGTERM']) process.on(signal, () => { stopping = true; replacement?.kill(signal); });
  supervisor.run();
}
if (process.argv[1] === new URL(import.meta.url).pathname) await main();
