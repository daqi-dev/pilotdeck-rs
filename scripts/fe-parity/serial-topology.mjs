import { createHash } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { baseline, requireUI } from './env.mjs';

const digest = value => createHash('sha256').update(value).digest('hex');
const id = row => JSON.stringify([row.file.replaceAll('\\', '/'), row.title]);
const catalog = JSON.parse(await readFile(new URL('./pending-orchestration.json', import.meta.url), 'utf8'));
// Pin source topology, not a hand-maintained list of cascade identities.
const hashes = Object.freeze({
  'real/files.spec.mjs': '6b9c38ef856bd9458d2140c4c071d4ab4c7c267c23b8336f05f55907b25f56e4',
  'real/session.spec.mjs': '77c077c0a70dac026b972c6dda2c337b200fb99fb52228068899d94948d680f8',
  'real/settings.spec.mjs': '9a61e9d1387d4b97b322be1410f07306059d26a6d40f56a1ccb433a6282380f2',
  'real/skills.spec.mjs': '5c9a301ba150257c9664cd56c81b98129452e6978d39b9a96a314327eb2d281d',
  'real/onboarding.spec.mjs': 'a947335d75c04c3d71e048b51a678122ea7c56b467ce70942d19ba1861b799f7',
});
export async function readSerialTopology(input = catalog, readSource = file => readFile(resolve(baseline, 'e2e', file), 'utf8')) {
  if (digest(JSON.stringify(input)) !== 'bbf6bcffdf7a0ec493e628035a231d997cf2f05b2aace2ddc9ad05600455e227') throw new Error('Serial registration catalog drift');
  const ts = requireUI('typescript'), entries = [];
  for (const [file, hash] of Object.entries(hashes)) {
    const source = await readSource(file);
    if (digest(source) !== hash) throw new Error(`Serial source drift: ${file}`);
    const ast = ts.createSourceFile(file, source, ts.ScriptTarget.Latest, true), configurations = [], registrations = [];
    const visit = node => {
      if (ts.isCallExpression(node)) {
        const callee = node.expression.getText(ast);
        if (callee === 'test.describe.configure') {
          if (!ts.isExpressionStatement(node.parent) || node.parent.parent !== ast) throw new Error(`Unsupported serial scope: ${file}`);
          const mode = node.arguments[0]?.properties?.find(prop => prop.name?.getText(ast) === 'mode');
          if (mode?.initializer?.text !== 'serial') throw new Error(`Expected frozen serial mode: ${file}`);
          configurations.push(ast.getLineAndCharacterOfPosition(node.getStart(ast)).line + 1);
        } else if (callee.startsWith('test.describe')) throw new Error(`Unsupported nested describe: ${file}`);
        if (callee === 'test') registrations.push({ line: ast.getLineAndCharacterOfPosition(node.getStart(ast)).line + 1, title: node.arguments[0] });
      }
      ts.forEachChild(node, visit);
    };
    visit(ast);
    if (configurations.length !== 1) throw new Error(`Expected one frozen serial group: ${file}`);
    for (const row of input.tests.filter(row => row.file === file)) {
      const line = Number(row.source.registration.split(':').at(-1));
      const registration = registrations.find(item => item.line === line);
      if (!registration || row.source.serial !== `ui/e2e/${file}:${configurations[0]}`
        || ts.isStringLiteral(registration.title) && registration.title.text !== row.title) throw new Error(`Serial registration location/title drift: ${id(row)}`);
      entries.push(Object.freeze({ file, title: row.title, line, order: input.tests.indexOf(row),
        serialGroup: row.source.serial, registration: row.source.registration, sourceSha256: hash }));
    }
  }
  return Object.freeze(entries);
}
export const SERIAL_TOPOLOGY = await readSerialTopology();
const byId = new Map(SERIAL_TOPOLOGY.map(row => [id(row), row]));
export const serialLocation = row => byId.get(id(row));
export function serialSuccessors(inventory, predecessor) {
  const location = serialLocation(predecessor);
  return location ? inventory.filter(row => {
    const candidate = serialLocation(row);
    return candidate?.serialGroup === location.serialGroup && (candidate.line > location.line
      || candidate.line === location.line && candidate.order > location.order);
  }) : [];
}
