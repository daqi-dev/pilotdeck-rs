// Equivalent seed + native supervisor, without baseline launch's rm or Vite writes.
import { spawn } from 'node:child_process';
import { mkdir, writeFile, realpath, symlink } from 'node:fs/promises';
import { dirname, resolve, sep } from 'node:path';
import { pathToFileURL } from 'node:url';
import { baseline, root, realRunRoot, realPorts, REAL_TEST_KEY, ownedPath } from './env.mjs';

export function runtimeConfig(mode, runDir) {
  if (!['ready', 'onboarding'].includes(mode)) throw new Error('Invalid real mode');
  const center = `http://127.0.0.1:${realPorts.mock}`;
  const providers = mode === 'onboarding' ? {} : Object.fromEntries([
    ['e2e-openai', 'openai', center + '/v1', 'gpt-4.1'],
    ['e2e-anthropic', 'anthropic', center + '/v1', 'claude-sonnet-4.6'],
    ['e2e-google', 'google', center, 'gemini-2.5-flash'],
  ].map(([id, protocol, url, model]) => [id, { protocol, url, apiKey: '${PILOTDECK_E2E_API_KEY}', models: { [model]: {} } }]));
  return { schemaVersion: 1, ...(mode === 'ready' ? { agent: { model: 'e2e-openai/gpt-4.1' } } : {}), model: { providers },
    webui: { runtime: { host: '127.0.0.1', serverPort: realPorts[mode].server, vitePort: realPorts[mode].vite,
      databasePath: resolve(runDir, 'auth.db'), workspacesRoot: resolve(runDir, 'workspace') } } };
}
export async function prepareRuntime(mode, runDir) {
  ownedPath(runDir);
  if (!runDir.startsWith(realRunRoot + sep) || await realpath(dirname(runDir)) !== dirname(runDir)) throw new Error('Runtime path must be a fresh directory under canonical real-run');
  await mkdir(runDir); // never delete/reuse a previous runtime
  const workspacePath = resolve(runDir, 'workspace'), project = resolve(workspacePath, 'e2e-project');
  await mkdir(project, { recursive: true }); await mkdir(resolve(runDir, 'home'));
  await mkdir(resolve(workspacePath, 'general'), { mode: 0o700 });
  await mkdir(resolve(runDir, 'bin'));
  await symlink(process.execPath, resolve(runDir, 'bin/node'));
  const configPath = resolve(runDir, 'pilotdeck.yaml');
  // JSON is valid YAML; no runtime YAML dependency, same launch schema and defaults.
  await writeFile(configPath, JSON.stringify(runtimeConfig(mode, runDir), null, 2) + '\n');
  await writeFile(resolve(project, 'README.md'), '# F2 isolated workspace\nE2E-FILE-SEED\n');
  await writeFile(resolve(project, 'sample.ts'), 'export const marker = "E2E-FILE-SEED";\n');
  await writeFile(resolve(project, 'SKILL.md'), '---\nname: e2e-seed\ndescription: F2 isolated seed skill\n---\n\n# Seed skill\n');
  // A workspace folder is not a registered project. Use the native ID encoder
  // and registration marker, as addProjectManually does (no session fabrication).
  const { createProjectId } = await import(pathToFileURL(resolve(baseline, 'server/utils/pilotPaths.js')));
  const projectId = createProjectId(project);
  const registrationPath = resolve(runDir, 'home/projects', projectId);
  await mkdir(registrationPath, { recursive: true });
  await writeFile(resolve(registrationPath, '.cwd'), project, 'utf8');
  const runtime = { runDir, configPath, workspacePath, projectPath: project, projectId, registrationPath, mode, ports: realPorts[mode] };
  await writeFile(resolve(runDir, 'runtime.json'), JSON.stringify(runtime, null, 2) + '\n');
  return runtime;
}
export function runtimeArgs(args) {
  return args[0] === resolve(baseline, 'node_modules/vite/bin/vite.js')
    ? [resolve(root, 'scripts/fe-parity/real-vite.mjs')] : args;
}
async function main() {
  const mode = process.env.PILOTDECK_E2E_MODE, runDir = ownedPath(process.env.PILOTDECK_E2E_RUN_DIR);
  if (process.env.PILOTDECK_E2E_API_KEY !== REAL_TEST_KEY) throw new Error('Invalid test credential');
  await prepareRuntime(mode, runDir);
  const { createRuntimeSupervisor } = await import(pathToFileURL(resolve(baseline, 'server/webRuntimeSupervisor.js')));
  const supervisor = createRuntimeSupervisor({ mode: 'dev', cwd: baseline,
    requestFile: resolve(runDir, 'restart.request'),
    spawnImpl: (command, args, options) => {
      return spawn(command, runtimeArgs(args), options);
    },
  });
  supervisor.run();
}
if (process.argv[1] === new URL(import.meta.url).pathname) await main();
