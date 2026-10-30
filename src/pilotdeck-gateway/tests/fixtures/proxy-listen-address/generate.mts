import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {once} from 'node:events';
import {mkdir, mkdtemp, rm, writeFile} from 'node:fs/promises';
import {join, resolve} from 'node:path';
import {tmpdir} from 'node:os';
import {assertBaseline, baselineSourceHashes} from '../_shared/baseline.mts';

assert.equal(process.version, 'v22.22.0', '#1131 pinned Node');
const baseline = resolve(process.env.PDRS_BASELINE!);
const sourceCommit = assertBaseline(baseline);
const sourceSha256 = await baselineSourceHashes(baseline, [
  'src/gateway/server/GatewayServer.ts', 'src/cli/pilotdeck.ts',
]);
const output = resolve(process.argv[2]);
await mkdir(output, {recursive: true});
const scratch = await mkdtemp(join(tmpdir(), 'pdrs-proxy-listen-'));

async function observe(port: number) {
  const home = join(scratch, String(port));
  await mkdir(home, {recursive: true});
  await mkdir(join(home, 'pilot'), {recursive: true});
  await writeFile(join(home, 'pilot/pilotdeck.yaml'), `agent: {model: fixture/local}
model:
  providers:
    fixture:
      protocol: openai
      url: http://127.0.0.1:1/v1
      apiKey: fixture-only
      models: {local: {}}
telemetry: {enabled: false}
cron: {enabled: false}
`, {flag: 'wx'});
  const child = spawn(process.execPath, [
    '--disable-warning=ExperimentalWarning', '--import',
    join(baseline, 'node_modules/tsx/dist/loader.mjs'),
    join(baseline, 'src/cli/pilotdeck.ts'), 'server', '--port', String(port),
  ], {
    cwd: home,
    env: {PATH: process.env.PATH!, HOME: home, TMPDIR: home, PILOT_HOME: join(home, 'pilot'),
      PILOTDECK_GATEWAY_TOKEN: 'fixture-token', ANALYTICS_ENABLED: 'false'},
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let stdout = '', stderr = '';
  child.stdout.setEncoding('utf8').on('data', data => {stdout += data;});
  child.stderr.setEncoding('utf8').on('data', data => {stderr += data;});
  const exited = once(child, 'exit');
  let timer: ReturnType<typeof setTimeout>;
  try {
    const ready = new Promise<void>((accept, reject) => {
      const check = () => {
        if (/^WebSocket: ws:\/\/127\.0\.0\.1:\d+\/ws\n/m.test(stdout)) accept();
      };
      child.stdout.on('data', check);
      child.once('error', reject);
      child.once('exit', () => reject(new Error('#1131 TS exited before startup')));
      timer = setTimeout(() => reject(new Error('#1131 TS startup deadline')), 15000);
      check();
    });
    await ready;
    const lines = stdout.split('\n').filter(line =>
      line.startsWith('PilotDeck server listening: ') || line.startsWith('WebSocket: '));
    assert.equal(lines.length, 2, '#1131 exactly two address lines');
    const url = new URL(lines[0].slice('PilotDeck server listening: '.length));
    const actual = Number(url.port);
    assert.ok(actual > 0, '#1131 TS actual port is nonzero');
    if (port !== 0) assert.equal(actual, port, '#1131 TS fixed port');
    assert.equal(lines[1], `WebSocket: ws://127.0.0.1:${actual}/ws`, '#1131 TS WS address');
    const response = await fetch(`${url.origin}/health`, {signal: AbortSignal.timeout(3000)});
    assert.equal(response.status, 200, '#1131 TS reported HTTP port is reachable');
    assert.deepEqual(await response.json(), {ok: true}, '#1131 TS health response');
    console.log(JSON.stringify({requestedPort: port, actualPort: actual, stdout, stderr, health: 200}));
    return {actual, lines: lines.map(line => line.replace(`:${actual}`, ':<PORT>'))};
  } catch (error) {
    console.error(JSON.stringify({requestedPort: port, stdout, stderr}));
    throw error;
  } finally {
    clearTimeout(timer!);
    child.kill('SIGKILL');
    await exited;
  }
}

try {
  const ephemeral = await observe(0);
  const fixed = await observe(ephemeral.actual);
  assert.deepEqual(fixed.lines, ephemeral.lines, '#1131 stable output format for both port modes');
  await writeFile(join(output, 'oracle.json'), JSON.stringify({
    sourceCommit, sourceSha256, nodeVersion: process.version,
    addressLines: ephemeral.lines,
  }, null, 2) + '\n', {flag: 'wx'});
} finally {
  await rm(scratch, {recursive: true, force: true});
}
