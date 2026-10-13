#!/usr/bin/env node
// A3 · gateway 协议 schema 正负例校验（ajv · draft-07）
//
// 用法：
//   node scripts/gateway-parity/validate_fixtures.mjs [--schema <path>] [--fixtures <path>]
//
// 消费 scripts/gateway-parity/fixtures/gateway-protocol.fixtures.json：
//   frames.positive   每条必须通过根 schema（WsGatewayFrame）
//   frames.negative   每条必须被拒绝（含 run_get 语义：ok:true 省略 result 的正例在 positive 里）
//   definitions[]     逐条按指定 definition 校验，expect: pass|fail——覆盖按方法导出的内联载荷、
//                     开放联合（GatewayChannelKey 收自定义串）/ 关闭枚举（GatewayMode / GatewayWsClientName）
//
// 另做两道机判（与 check_schema.py A9 互为镜像，分别守 node/python 两侧）：
//   * x-pilotdeck.rootTypes 全部有 definition
//   * 快照 payloadAliases 的别名全部有 definition
// 任一不符 → 非零退出。

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import Ajv from 'ajv';

const here = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(here, '..', '..');
const argv = process.argv.slice(2);
const argOf = (flag) => {
  const i = argv.indexOf(flag);
  return i >= 0 ? argv[i + 1] : undefined;
};
const schemaPath = argOf('--schema') ?? path.join(repoRoot, 'schema', 'gateway-protocol.schema.json');
const fixturesPath = argOf('--fixtures') ?? path.join(here, 'fixtures', 'gateway-protocol.fixtures.json');

const schema = JSON.parse(fs.readFileSync(schemaPath, 'utf8'));
const snapshot = JSON.parse(fs.readFileSync(path.join(repoRoot, 'schema', 'gateway-methods.snapshot.json'), 'utf8'));
const fixtures = JSON.parse(fs.readFileSync(fixturesPath, 'utf8'));
const ajv = new Ajv({ strict: false }); // draft-07；strict 关掉只为容忍 x-pilotdeck-* 注解关键字
const definitions = schema.definitions ?? schema.$defs ?? {};

const failures = [];
const judge = (label, expectPass, actualPass, errors) => {
  const ok = expectPass === actualPass;
  console.log(`[${ok ? 'PASS' : 'FAIL'}] ${label}（期望${expectPass ? '通过' : '拒绝'}，实际${actualPass ? '通过' : '拒绝'}）`
    + (ok || !errors ? '' : ` —— ${errors}`));
  if (!ok) failures.push(label);
};

// 帧级正负例：根 schema
const validateFrame = ajv.compile(schema);
let idx = 0;
for (const frame of fixtures.frames?.positive ?? []) {
  const ok = validateFrame(frame);
  judge(`frame.positive[${idx}] ${JSON.stringify(frame).slice(0, 90)}`, true, ok, ajv.errorsText(validateFrame.errors));
  idx += 1;
}
idx = 0;
for (const frame of fixtures.frames?.negative ?? []) {
  const ok = validateFrame(frame);
  judge(`frame.negative[${idx}] ${JSON.stringify(frame).slice(0, 90)}`, false, ok, '负例竟通过校验');
  idx += 1;
}

// definition 级正负例：按名编译子 schema
const subValidators = new Map();
for (const case_ of fixtures.definitions ?? []) {
  const name = case_.definition;
  if (!subValidators.has(name)) {
    if (!definitions[name]) {
      failures.push(`definition ${name} 不存在`);
      console.log(`[FAIL] definition ${name} 不存在`);
      subValidators.set(name, null);
      continue;
    }
    subValidators.set(name, ajv.compile({ $ref: `#/definitions/${name}`, definitions }));
  }
  const v = subValidators.get(name);
  if (!v) continue; // 缺 definition 已计 FAIL
  const ok = v(case_.data);
  judge(`definition ${name} ${JSON.stringify(case_.data).slice(0, 70)} 期望 ${case_.expect}`,
    case_.expect === 'pass', ok, ajv.errorsText(v.errors));
}

// roots 齐备（镜像 check_schema.py A9）
const rootTypes = schema['x-pilotdeck']?.rootTypes;
if (!Array.isArray(rootTypes) || !rootTypes.includes('WsGatewayFrame')) {
  failures.push('x-pilotdeck.rootTypes 异常');
  console.log('[FAIL] x-pilotdeck.rootTypes 异常');
} else {
  const missing = rootTypes.filter((n) => !definitions[n]);
  console.log(`[${missing.length ? 'FAIL' : 'PASS'}] rootTypes ${rootTypes.length} 个全部有 definition`);
  if (missing.length) failures.push(`rootTypes 缺 definition: ${missing.join(', ')}`);
}
const aliases = (snapshot.payloadAliases ?? []).flatMap((e) => [e.input, e.result].filter(Boolean));
const aliasMissing = aliases.filter((n) => !definitions[n] || !rootTypes?.includes(n));
console.log(`[${aliasMissing.length ? 'FAIL' : 'PASS'}] 快照 payloadAliases ${aliases.length} 条全部有 definition 且列入 rootTypes`);
if (aliasMissing.length) failures.push(`payloadAliases 缺失: ${aliasMissing.join(', ')}`);

if (failures.length) {
  console.error(`\nvalidate_fixtures: ${failures.length} 项 FAIL`);
  for (const f of failures) console.error(`  - ${f}`);
  process.exit(1);
}
console.log('\nvalidate_fixtures: 全部正负例与机判通过');
