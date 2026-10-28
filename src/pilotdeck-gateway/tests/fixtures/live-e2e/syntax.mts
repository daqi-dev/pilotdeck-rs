import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {createRequire} from 'node:module';
import {join} from 'node:path';
assert.equal(process.version, 'v22.22.0', '#969 syntax: Node version');
const ts = createRequire(join(process.env.PDRS_BASELINE!, 'package.json'))('typescript');
const result = ts.transpileModule(readFileSync(process.argv[2], 'utf8'), {
  reportDiagnostics:true, compilerOptions:{target:ts.ScriptTarget.ESNext, module:ts.ModuleKind.ESNext},
});
assert.equal(result.diagnostics?.length ?? 0, 0, '#969 syntax: mutant must compile');
