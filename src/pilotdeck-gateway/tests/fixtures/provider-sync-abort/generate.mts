// #415: capture the real provider catch after a rejected promise and same-stack abort.
import assert from 'node:assert/strict';
import { mkdir, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';
assert.equal(process.versions.node.split('.')[0], '22', 'generate with Node 22');
assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE');
assert.ok(process.argv[2], 'provide an output directory');
const baseline = resolve(process.env.PDRS_BASELINE);
const sourceCommit = assertBaseline(baseline);
const sources = ['src/model/streaming/streamModel.ts', 'src/network/fetch.ts'];
const sourceSha256 = await baselineSourceHashes(baseline, sources);
const { streamModel } = await import(pathToFileURL(join(baseline, sources[0])).href);
const cases = [];
for (const protocol of ['anthropic', 'openai', 'openai-responses', 'google']) {
  const controller = new AbortController();
  const events = [], retries = [];
  let calls = 0;
  const reject = () => {
    calls++;
    const promise = Promise.reject(new TypeError('fixture transport failure'));
    controller.abort();
    return promise;
  };
  const provider = { id: 'p', protocol, url: 'http://example.test', apiKey: 'fixture', headers: {},
    models: { m: { id: 'm', capabilities: { supportsStreaming: true, maxOutputTokens: 4096 } } },
    retry: { streamMaxRetries: 1 } };
  let observed;
  try {
    for await (const event of streamModel({ provider: 'p', model: 'm', messages: [] },
      { providers: { p: provider } }, { fetch: reject, signal: controller.signal,
        onRetryProgress: r => retries.push(r),
        googleClientFactory: () => ({ models: { generateContentStream: reject } }) })) events.push(event);
    observed = { delivery: 'ok', events, retries: retries.length };
  } catch (error) {
    observed = { delivery: 'throw', name: error.name, message: error.message, events, retries: retries.length };
  }
  assert.equal(calls, 1, 'must reach the real provider and reject once');
  cases.push({ protocol, observed });
}
assertBaseline(baseline);
assert.deepEqual(await baselineSourceHashes(baseline, sources), sourceSha256);
await mkdir(resolve(process.argv[2]), { recursive: true });
await writeFile(join(resolve(process.argv[2]), 'oracle.json'), JSON.stringify({ sourceCommit, sourceSha256, cases }, null, 2) + '\n');
