// One cold SDK Client.callTool per process, using linked in-memory transports.
// The generator binds these static imports to the hash-checked baseline in /tmp.
import {Client} from 'pdrs:baseline-client';
import {Server} from 'pdrs:baseline-server';
import {InMemoryTransport} from 'pdrs:baseline-memory';
import {ListToolsRequestSchema, CallToolRequestSchema} from 'pdrs:baseline-types';
const [shape, rawDepth, mode] = process.argv.slice(2);
const depth = Number(rawDepth);
if (!Number.isSafeInteger(depth) || depth < 0) throw new Error('invalid depth');
const props = {type: 'object', properties: {a: {$ref: '#'}}};
const schemas = {
  properties: props,
  patternProperties: {type: 'object', patternProperties: {'^a$': {$ref: '#'}}},
  additionalProperties: {type: 'object', additionalProperties: {$ref: '#'}},
  anyOf: {type: 'object', anyOf: [props]},
  oneOf: {type: 'object', oneOf: [props]},
  allOf: {type: 'object', allOf: [props]},
  not: {type: 'object', not: {not: props}},
  if: {type: 'object', if: props, then: {type: 'object'}, else: {type: 'object'}},
  then: {type: 'object', if: {required: ['a']}, then: props},
  else: {type: 'object', if: {not: {required: ['a']}}, then: {type: 'object'}, else: props},
  ref: {type: 'object', definitions: {
    node: {type: 'object', properties: {a: {$ref: '#/definitions/alias'}}},
    alias: {$ref: '#/definitions/node'},
  }, $ref: '#/definitions/node'},
  items: {type: 'object', properties: {a: {$ref: '#/definitions/node'}},
    definitions: {node: {type: 'array', items: {$ref: '#/definitions/node'}}}},
  wideAnyOf: {type: 'object', anyOf: Array.from({length: 512}, () => props)},
};
let schema = schemas[shape];
// Wide recipes retain all static branches and dependency code-generation inputs.
const wide = /^(anyOf|oneOf|allOf|if|dependencies)(128|256|512)$/.exec(shape);
if (wide) {
  const [, kind, rawWidth] = wide, width = Number(rawWidth);
  if (kind === 'anyOf') schema = {type: 'object', anyOf: Array.from({length: width}, () => props)};
  if (kind === 'oneOf') schema = {type: 'object', oneOf: [props, ...Array.from({length: width - 1}, () => ({type: 'number'}))]};
  if (kind === 'allOf') schema = {type: 'object', allOf: [props, ...Array.from({length: width - 1}, () => ({not: {type: 'number'}}))]};
  if (kind === 'if') schema = {type: 'object', if: {oneOf: [{type: 'object'}, ...Array.from({length: width - 1}, () => ({type: 'number'}))]}, then: props};
  if (kind === 'dependencies') schema = {...props, dependencies: Object.fromEntries(Array.from({length: width}, (_, i) => [`x${i}`, ['y']]))};
}
const comparison = ['const', 'enum', 'uniqueItems'].includes(shape);
if (!schema && !comparison) throw new Error('invalid shape');
const arrays = shape === 'items';
let cursor = arrays ? [] : {};
let data = arrays ? {a: cursor} : cursor;
for (let i = 0; i < depth; i++) {
  const child = arrays ? [] : {};
  if (arrays) cursor.push(child); else cursor.a = child;
  cursor = child;
}
if (comparison) {
  const cloneChain = () => {
    let value = {};
    for (let i = 0; i < depth; i++) value = {a: value};
    return value;
  };
  schema = {type: 'object'};
  if (shape === 'const') schema.const = cloneChain();
  if (shape === 'enum') schema.enum = [cloneChain()];
  if (shape === 'uniqueItems') {
    schema.properties = {a: {uniqueItems: true}};
    data = {a: [data, cloneChain()]};
  }
}
if (wide?.[1] === 'dependencies') {
  for (let node = data; node; node = node.a) {
    node.y = true;
    for (let i = 0; i < Number(wide[2]); i++) node[`x${i}`] = true;
  }
}
// Leaf corruption is a data negative control for the generator's safety assertion.
if (mode === 'invalid-leaf') {
  if (arrays) cursor.push(7); else cursor.a = 7;
}
const client = new Client({name: 'oracle', version: '1'});
const server = new Server({name: 'oracle', version: '1'}, {capabilities: {tools: {}}});
const [ct, st] = InMemoryTransport.createLinkedPair();
server.setRequestHandler(ListToolsRequestSchema, () => ({tools: [
  {name: 'probe', inputSchema: {type: 'object'}, outputSchema: schema},
]}));
server.setRequestHandler(CallToolRequestSchema, () => ({content: [], structuredContent: data}));
await server.connect(st);
await client.connect(ct);
await client.listTools();
let observed = {};
// The optional warm control uses 1000 completed real calls before observing.
if (mode === 'warm1000') {
  for (let i = 0; i < 1000; i++) {
    try { await client.callTool({name: 'probe', arguments: {}}); } catch {}
  }
}
try {
  await client.callTool({name: 'probe', arguments: {}});
} catch (error) {
  observed = {sdkError: {code: error.code, message: error.message}};
}
await client.close();
await server.close();
console.log(JSON.stringify(comparison
  ? {schema: null, data: null, comparisonData: {kind: shape, depth}, ...observed}
  : {schema, data: null, recursiveData: {shape, depth}, ...observed}));
