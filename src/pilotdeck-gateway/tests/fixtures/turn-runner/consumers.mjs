import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';

export function verifyConsumers(rows, paths) {
  const tests=paths.map(path=>readFileSync(path,'utf8')).join('\n').replace(/\s+/g,'');
  for(const row of rows) if(row.consumption?.test) {
    const name=row.consumption.test;
    assert.ok(tests.includes(`fn${name}(`) || tests.includes(`case!(${name},`), `missing Rust consumer: ${name}`);
  }
}
