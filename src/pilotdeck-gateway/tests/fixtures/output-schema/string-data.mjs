// A cold, default-stack real SDK Client.callTool observation. generate.mjs
// binds these imports to its hash-checked baseline in a temporary directory.
import {Client} from 'pdrs:baseline-client';
import {Server} from 'pdrs:baseline-server';
import {InMemoryTransport} from 'pdrs:baseline-memory';
import {ListToolsRequestSchema, CallToolRequestSchema} from 'pdrs:baseline-types';
const [kind, rawRepeats, subject = 'default', mode] = process.argv.slice(2);
const repeats = Number(rawRepeats);
if (!Number.isSafeInteger(repeats) || repeats < 0) throw Error('invalid repeats');
const patterns = {
  dot: '^.*$', negated: '^[^<>]*$', astralClass: '^[a😀]*$', negatedX: '^[^x]*$',
  property: '^[\\p{L}\\p{Emoji}]*$', mixedGroup: '^(a|😀)*$',
  mixedCounted: '^[a😀]{0,20000000}$', plus: '^[a😀]+$', openCounted: '^[a😀]{1,}$',
  literal: '^a$', boundedClass: '^[a😀]{0,16}$',
  alt: '^(?:a|b)*$', capture: '^(a|b)*$', nested: '^((a)|(b))*c?$',
  look: '^(?:(?:a|b)(?=.))*.?$', cap: '^(ab)*$', optional: '^(?:ab?)*$',
  bounded: '^(?:a|b){1,20000000}$', unicode: '^(😀|😁)*$',
  countedSimple: '^(?:ab){1,20000000}$', countedAtom: '^[ab]{1,40000000}$', lazyCap: '^(ab)*?$',
  lazyAtom: '^[ab]*?$', lazySimple: '^(?:ab)*?$',
  plain: '^[ab]*$', group: '^([ab]*)$', simple: '^(?:ab)*$',
};
const formats = {
  time: ['12:00:00.', '0', 'Z', 'time'],
  dateTime: ['2000-01-01T12:00:00.', '0', 'Z', 'date-time'],
  isoTime: ['12:00:00.', '0', '', 'iso-time'],
  isoDateTime: ['2000-01-01T12:00:00.', '0', '', 'iso-date-time'],
  duration: ['P', '0', 'D', 'duration'],
  hostname: ['', 'a', '', 'hostname'],
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
if (subject === 'mixed') unit = 'a😀';
else if (subject === 'bmp') unit = 'aa';
else if (subject !== 'default') throw Error('invalid subject');
if (mode === 'invalid') suffix += kind === 'negated' ? '<' : kind === 'negatedX' ? 'x' : '\n!';
else if (mode) throw Error('invalid mode');
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
