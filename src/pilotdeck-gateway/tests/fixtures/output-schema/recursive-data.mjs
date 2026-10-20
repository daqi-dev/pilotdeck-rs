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
  items: {type: 'object', properties: {a: {$ref: '#/definitions/node'}},
    definitions: {node: {type: 'array', items: {$ref: '#/definitions/node'}}}},
  wideAnyOf: {type: 'object', anyOf: Array.from({length: 512}, () => props)},
};
let schema = schemas[shape];
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
