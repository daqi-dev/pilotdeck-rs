// Additional Node v22 / yaml@2.9.0 oracles. Convert even with parse errors:
// YAMLSet's nodeClass survives TAG_RESOLVE_FAILED and still merges keys.
import { readFileSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
const root = join(resolve(process.argv[2]), 'node_modules/yaml');
if (Number(process.versions.node.split('.')[0]) !== 22) throw Error('Use Node v22');
if (JSON.parse(readFileSync(join(root, 'package.json'))).version !== '2.9.0') throw Error('Use yaml@2.9.0');
const { parseDocument } = await import(pathToFileURL(join(root, 'dist/index.js')));
function graph(value) {
  const seen = new Map(), nodes = [];
  function visit(v) {
    if (v === null) return {type:'null'};
    if (typeof v === 'string') return {type:'string', json:JSON.stringify(v)};
    if (typeof v === 'number') return {type:'number', value:Object.is(v,-0)?'-0':String(v)};
    if (typeof v !== 'object') return {type:typeof v, value:String(v)};
    if (seen.has(v)) return {ref:seen.get(v)};
    const id = nodes.length; seen.set(v,id); nodes.push(null);
    const entries = Object.entries(v).map(([k,v])=>[JSON.stringify(k),visit(v)]);
    nodes[id] = {type:Object.prototype.toString.call(v), entries,
      ...(v instanceof Date ? {iso:v.toISOString()}:{}),
      ...(v instanceof Map ? {map:[...v].map(([k,v])=>[visit(k),visit(v)])}:{}),
      ...(v instanceof Set ? {set:[...v].map(visit)}:{})};
    return {ref:id};
  }
  return {root:visit(value),nodes};
}
const diagnostic = e => ({name:e.name,code:e.code,message:e.message,pos:e.pos,...(e.linePos?{linePos:e.linePos}:{})});
const inputs = [
  '!!set {<<: {a: null}, b: null}',
  '[&a {x: 1}, !!set {<<: *a, z: null}]',
  '!!set {<<: [{x: 1}, {x: 2, z: 3}], z: null}',
  '!!set {z: null, <<: {x: 1, z: 2}}',
  '!!set {<<: {x: 1}, <<: {z: 2}}',
  '!!set {<<: {<<: {x: 1}, z: 2}}',
  '[&a !!set {x: null}, !!set {<<: *a, z: null}]',
  '[&a !!set {<<: {x: 1}}, !!set {<<: *a}]',
  '!!set {!!merge <<: {x: 1}, z: null}',
  '!!set {"<<": {x: 1}}',
  '!!set {!!str <<: {x: 1}}',
  '!!set {<<: null}',
  '!!set {<<: 7}',
  '!!set {<<: [7]}',
  '!!set {<<: *missing}',
  '!!set {a: 1}',
  '!!set {a: !!null null}',
  '!!set {a: null # comment\n}',
  '!!set {? [x, z]: 1}',
  '!!set {<<: {.nan: 1, -0: 2, 0: 3}}',
  '!!set {<<: {? [x, z]: 1}, ? [x, z]: null}',
  '!!set [x, z]',
];
const cases=[];
for (const version of ['1.1', '1.2']) for (const [i,body] of inputs.entries()) {
  const yaml=`%YAML ${version}\n---\n${body}\n`;
  const doc=parseDocument(yaml,{prettyErrors:false});
  const pretty=parseDocument(yaml,{prettyErrors:true});
  let conversion;
  const warn=process.emitWarning; process.emitWarning=()=>{};
  try { conversion={value:graph(doc.toJSON())}; }
  catch(e) { conversion={error:{name:e.name,message:e.message}}; }
  finally { process.emitWarning=warn; }
  cases.push({id:`set-merge-${version}-${i}`,yaml,errors:doc.errors.map(diagnostic),
    warnings:doc.warnings.map(diagnostic),prettyErrors:pretty.errors.map(diagnostic),
    prettyWarnings:pretty.warnings.map(diagnostic),...conversion});
}
writeFileSync(process.argv[3],JSON.stringify(cases,null,2)+'\n');
console.log(`${cases.length} set/merge oracles`);
