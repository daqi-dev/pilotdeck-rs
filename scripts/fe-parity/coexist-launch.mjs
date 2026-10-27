// #927 coexistence runtime entrypoint: the baseline's own supervisor in
// start-built mode over the deployment copy (server + frozen TS gateway, no
// Vite), so SPA requests really hit the patched app.get('*') catchall.
import { spawn } from 'node:child_process';
import { mkdir, realpath, writeFile } from 'node:fs/promises';
import { dirname, resolve, sep } from 'node:path';
import { pathToFileURL } from 'node:url';
import { baseline, realPorts, REAL_TEST_KEY, ownedPath } from './env.mjs';
import { assertCoexistExecutionEnv, coexistCopyRoot, coexistPorts, coexistRunRoot, PHASES } from './coexist-deploy.mjs';
import { assertRealGitBin } from './real-git.mjs';

export function coexistRuntimeConfig(phase, runDir) {
  if (!PHASES.includes(phase)) throw new Error('Invalid coexist phase');
  const center = `http://127.0.0.1:${realPorts.mock}`;
  const providers = Object.fromEntries([
    ['e2e-openai', 'openai', center + '/v1', 'gpt-4.1'],
    ['e2e-anthropic', 'anthropic', center + '/v1', 'claude-sonnet-4.6'],
    ['e2e-google', 'google', center, 'gemini-2.5-flash'],
  ].map(([id, protocol, url, model]) => [id, { protocol, url, apiKey: '${PILOTDECK_E2E_API_KEY}', models: { [model]: {} } }]));
  // The invalid phase points agent.model at an undeclared provider, which the
  // server reports as configuration=invalid (ModelConfigurationErrorScreen).
  return { schemaVersion: 1, agent: { model: phase === 'invalid' ? 'ghost/undeclared-model' : 'e2e-openai/gpt-4.1' },
    model: { providers },
    webui: { runtime: { host: '127.0.0.1', serverPort: coexistPorts.server, vitePort: coexistPorts.vite,
      databasePath: resolve(runDir, 'auth.db'), workspacesRoot: resolve(runDir, 'workspace') } } };
}
export async function prepareCoexistRuntime(phase, runDir) {
  ownedPath(runDir);
  if (!runDir.startsWith(coexistRunRoot + sep) || await realpath(dirname(runDir)) !== dirname(runDir)) {
    throw new Error('Runtime path must be a fresh directory under canonical coexist-run');
  }
  await mkdir(runDir, { recursive: true });
  const workspacePath = resolve(runDir, 'workspace'), project = resolve(workspacePath, 'e2e-project');
  await mkdir(project, { recursive: true }); await mkdir(resolve(runDir, 'home'));
  await mkdir(resolve(workspacePath, 'general'), { mode: 0o700 });
  const configPath = resolve(runDir, 'pilotdeck.yaml');
  // JSON is valid YAML; same launch schema and defaults as the D suite.
  await writeFile(configPath, JSON.stringify(coexistRuntimeConfig(phase, runDir), null, 2) + '\n');
  await writeFile(resolve(project, 'README.md'), '# F3 coexistence isolated workspace\nE2E-FILE-SEED\n');
  const { createProjectId } = await import(pathToFileURL(resolve(baseline, 'server/utils/pilotPaths.js')));
  const projectId = createProjectId(project);
  const registrationPath = resolve(runDir, 'home/projects', projectId);
  await mkdir(registrationPath, { recursive: true });
  await writeFile(resolve(registrationPath, '.cwd'), project, 'utf8');
  const runtime = { runDir, configPath, workspacePath, projectPath: project, projectId, registrationPath, phase, ports: coexistPorts };
  await writeFile(resolve(runDir, 'runtime.json'), JSON.stringify(runtime, null, 2) + '\n');
  return runtime;
}
async function main() {
  assertCoexistExecutionEnv(process.env);
  const phase = process.env.FE_COEXIST_PHASE, runDir = ownedPath(process.env.PILOTDECK_E2E_RUN_DIR);
  if (process.env.PILOTDECK_E2E_API_KEY !== REAL_TEST_KEY) throw new Error('Invalid test credential');
  await assertRealGitBin(process.env);
  await prepareCoexistRuntime(phase, runDir);
  // Import the supervisor from the copy: the deployment must be self-sufficient.
  const { createRuntimeSupervisor } = await import(pathToFileURL(resolve(coexistCopyRoot, 'ui/server/webRuntimeSupervisor.js')));
  const supervisor = createRuntimeSupervisor({ mode: 'start-built', cwd: resolve(coexistCopyRoot, 'ui'),
    requestFile: resolve(runDir, 'restart.request'),
    spawnImpl: (command, args, options) => {
      assertCoexistExecutionEnv(options.env);
      return spawn(command, args, options);
    },
  });
  supervisor.run();
}
if (process.argv[1] === new URL(import.meta.url).pathname) await main();
