import { createRequire } from 'node:module';
import { resolve } from 'node:path';
import { writeFileSync } from 'node:fs';
const [baseline, output] = process.argv.slice(2);
const { Parser, CST } = createRequire(import.meta.url)(resolve(baseline, 'node_modules/yaml'));
const yaml = '? [a, b]\n: [c, d]\n? e\n: [f, g]\n';
const cases = [];
for (const mode of ['skip', 'remove', 'index', 'break', 'next', 'next-skip', 'next-break', 'next-remove']) {
  const doc = [...new Parser().parse(yaml)][0];
  const events = [];
  const event = (phase, path) => events.push({ phase, path });
  CST.visit(doc, (item, path) => {
    event('enter', path);
    if (mode === 'break' && path.length === 2) {
      item.value.source = 'A';
      return CST.visit.BREAK;
    }
    if (path.length === 1 && path[0][1] === 0) {
      if (mode === 'skip') return CST.visit.SKIP;
      if (mode === 'remove') return CST.visit.REMOVE;
      if (mode === 'index') return 2;
      if (mode.startsWith('next')) return (item, path) => {
        event('after-key', path);
        if (mode === 'next-skip') return CST.visit.SKIP;
        if (mode === 'next-break') return CST.visit.BREAK;
        if (mode === 'next-remove') return CST.visit.REMOVE;
        return (item, path) => { event('exit', path); };
      };
    }
  });
  cases.push({ mode, yaml, events, source: CST.stringify(doc) });
}
writeFileSync(output, JSON.stringify({ node: process.version, yaml: '2.9.0', cases }, null, 2) + '\n');
