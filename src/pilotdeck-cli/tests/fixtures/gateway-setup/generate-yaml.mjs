// Recompute the checked-in input cases against the fixed baseline's yaml.
import { createRequire } from 'node:module';
import { readFileSync, writeFileSync, mkdirSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { resolve, join } from 'node:path';
import { createHash } from 'node:crypto';
const baseline = resolve(process.argv[2] ?? '');
const output = resolve(process.argv[3] ?? '');
if (process.argv.length !== 4) throw new Error('Usage: generate-yaml.mjs <baseline> <new-output-dir>');
const expected = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const head = execFileSync('git', ['-C', baseline, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).trim();
if (head !== expected || process.version !== 'v22.22.0' || process.versions.icu !== '78.2')
  throw new Error(`Baseline/Node/ICU mismatch: ${head} ${process.version} ${process.versions.icu}`);
const require = createRequire(join(baseline, 'package.json'));
const yaml = require('yaml');
if (require('yaml/package.json').version !== '2.9.0') throw new Error('Expected yaml 2.9.0');
const options = { lineWidth: 0, singleQuote: false };
const load = file => JSON.parse(readFileSync(new URL(file, import.meta.url), 'utf8'));
const publish = (file, data) => writeFileSync(join(output, file), JSON.stringify(data, null, 2) + '\n', { flag: 'wx' });
mkdirSync(output, { recursive: false });
const provenance = { baseline: head, node: process.version, icu: process.versions.icu, yamlVersion: '2.9.0', inputs: {} };
for (const file of ['yaml-corpus.json', 'graph-corpus.json', 'leading-space.json'])
  provenance.inputs[file] = createHash('sha256').update(readFileSync(new URL(file, import.meta.url))).digest('hex');
const values = load('yaml-corpus.json');
publish('yaml-corpus.json', { node: process.version, icu: process.versions.icu, yamlVersion: '2.9.0', options,
  cases: values.cases.map(({ id, value }) => ({ id, value, yaml: yaml.stringify(value, options) })) });
const adapter = () => ({ enabled: true, appId: 'fixture-app', appSecret: 'fixture-secret', connectionMode: 'stream', domainName: 'feishu' });
const graphs = load('graph-corpus.json').cases.map(({ id, yaml: input }) => {
  const result = { id, yaml: input };
  try {
    const value = yaml.parse(input);
    Object.assign(result, { type: typeof value, isArray: Array.isArray(value), constructor: value?.constructor?.name ?? null,
      output: yaml.stringify(value, options) });
    try {
      const config = value ?? {};
      if (!config.adapters) config.adapters = {};
      config.adapters.feishu = adapter();
      result.afterFeishuWrite = yaml.stringify(config, options);
    } catch (error) { result.writeError = { name: error.name, message: error.message }; }
  } catch (error) { result.error = { name: error.name, message: error.message }; }
  return result;
});
publish('graph-corpus.json', { node: process.version, icu: process.versions.icu, yamlVersion: '2.9.0', cases: graphs });
const { input } = load('leading-space.json');
const config = yaml.parse(input);
config.adapters = { feishu: adapter() };
publish('leading-space.json', { node: process.version, icu: process.versions.icu, yamlVersion: '2.9.0', input, expected: yaml.stringify(config, options) });
publish('provenance.json', provenance);
