// A cold, default-stack real SDK Client.callTool observation. generate.mjs
// binds these imports to its hash-checked baseline in a temporary directory.
import {Client} from 'pdrs:baseline-client';
import {Server} from 'pdrs:baseline-server';
import {InMemoryTransport} from 'pdrs:baseline-memory';
import {ListToolsRequestSchema, CallToolRequestSchema} from 'pdrs:baseline-types';
const [kind, rawRepeats] = process.argv.slice(2);
const repeats = Number(rawRepeats);
if (!Number.isSafeInteger(repeats) || repeats < 0) throw Error('invalid repeats');
const patterns = {
  alt: '^(?:a|b)*$', capture: '^(a|b)*$', nested: '^((a)|(b))*c?$',
  look: '^(?:(?:a|b)(?=.))*.?$', cap: '^(ab)*$', optional: '^(?:ab?)*$',
  bounded: '^(?:a|b){1,20000000}$', unicode: '^(😀|😁)*$',
  countedSimple: '^(?:ab){1,20000000}$', countedAtom: '^[ab]{1,40000000}$', lazyCap: '^(ab)*?$',
  lazyAtom: '^[ab]*?$', lazySimple: '^(?:ab)*?$',
  plain: '^[ab]*$', group: '^([ab]*)$', simple: '^(?:ab)*$',
};
const formats = {
  email: ['', 'a.', 'a@example.com', 'email'],
  uri: ['x:', '/a', '', 'uri'],
  uriReference: ['x:', '/a', '', 'uri-reference'],
  uriTemplate: ['', '{a}', '', 'uri-template'],
  jsonPointer: ['', '/a', '', 'json-pointer'],
  fragment: ['#', '/a', '', 'json-pointer-uri-fragment'],
  relative: ['0', '/a', '', 'relative-json-pointer'],
  byte: ['', 'YWJj', '', 'byte'],
  url: ['https://', 'a-', 'a.example.com/', 'url'],
};
let prefix = '', unit = kind === 'unicode' ? '😀😁' : 'ab', suffix = '';
let rule = {type: 'string', pattern: patterns[kind]};
if (formats[kind]) {
  [prefix, unit, suffix] = formats[kind];
  rule = {type: 'string', format: formats[kind][3]};
} else if (!patterns[kind] && kind !== 'key' && kind !== 'additionalKey' && kind !== 'skippedKey') throw Error('invalid kind');
const key = ['key', 'additionalKey', 'skippedKey'].includes(kind);
const schema = key
  ? {type: 'object', patternProperties: {'^(?:a|b)*$': kind === 'key' ? {type: 'number'} : {}},
    ...(kind === 'additionalKey' ? {additionalProperties: false} : {})}
  : {type: 'object', properties: {s: rule}};
const text = prefix + unit.repeat(repeats) + suffix;
const data = key ? {[text]: 1} : {s: text};
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
try { await client.callTool({name: 'probe', arguments: {}}); }
catch (error) { observed = {sdkError: {code: error.code, message: error.message}}; }
await client.close();
await server.close();
console.log(JSON.stringify({schema, data: null,
  stringData: {prefix, unit, suffix, repeats, length: text.length, target: key ? 'key' : 'value'},
  ...observed,
}));
