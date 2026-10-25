// Calls the fixed TS baseline directly. Every expected value in oracle.json
// comes from these calls; no value is synthesized by the Rust side.
import {existsSync, readFileSync} from 'node:fs';
import {createRequire} from 'node:module';
import {dirname, join, resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {freezeClock} from '../edgeclaw-file-memory/observe.mts';

export const NOW = '2026-10-22T00:00:00.000Z';
export const HOME = '/fixture/home';
const ROOT = 'src/context/memory/edgeclaw-memory-core/src';

export function enc(v: any): any {
  if (v === undefined) return {$undefined: true};
  if (typeof v === 'number' && (Object.is(v, -0) || !Number.isFinite(v))) return {$number: Object.is(v, -0) ? '-0' : String(v)};
  if (Array.isArray(v)) return v.map(enc);
  if (v && typeof v === 'object') return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, enc(x)]));
  return v;
}

function clean(v: any): any {
  const cwd = process.cwd();
  if (typeof v === 'string') return v.split(cwd).join('$CWD');
  if (Array.isArray(v)) return v.map(clean);
  if (v && typeof v === 'object') return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, clean(x)]));
  return v;
}

async function transformedService(baseline: string) {
  const sourcePath = join(baseline, ROOT, 'service.ts');
  const requireFromBaseline = createRequire(join(baseline, 'package.json'));
  const {transformSync} = createRequire(requireFromBaseline.resolve('tsx'))('esbuild');
  const code = transformSync(
    readFileSync(sourcePath, 'utf8')
      + '\nexport { parseRawRecallSections, normalizeCaseTraceContextPreview, looksLikeRawRecallContext };\n',
    {loader: 'ts', format: 'esm', target: 'es2022', sourcefile: sourcePath},
  ).code;
  const executable = code.replace(/from (["'])(\.[^"']+)\1/g, (_: string, quote: string, specifier: string) => {
    let file = resolve(dirname(sourcePath), specifier);
    if (file.endsWith('.js') && existsSync(file.slice(0, -3) + '.ts')) file = file.slice(0, -3) + '.ts';
    return 'from ' + quote + pathToFileURL(file).href + quote;
  });
  return import('data:text/javascript;base64,' + Buffer.from(executable).toString('base64'));
}

export async function collect(baseline: string) {
  // Freeze and isolate HOME before general-projects.ts is first imported so
  // the ambient GENERAL_WORKSPACE_DIR constant is deterministic.
  process.env.HOME = HOME;
  freezeClock();
  const load = (relative: string, query = '') =>
    import(pathToFileURL(join(baseline, ROOT, relative)).href + query);
  const text = await load('core/utils/text.ts');
  const id = await load('core/utils/id.ts');
  const general = await load('core/general-projects.ts');
  const trace = await load('core/trace-i18n.ts');
  const service = await transformedService(baseline);
  const cases: any[] = [];
  const add = (fn: string, args: any[], expected: any) =>
    cases.push({fn, args: enc(args), expected: enc(clean(expected))});

  // -- text.ts --------------------------------------------------------------
  const trunc: Array<[string, number]> = [
    ['', 5], ['abc', 0], ['abc', -1], ['abc', 3], ['abcd', 3], ['产品🚀x', 2],
    ['\ud800abc', 1], ['a'.repeat(120), 100], ['产品', 2.5], ['abc', NaN],
    ['abc', Infinity], ['abc', -Infinity],
  ];
  for (const [value, max] of trunc) add('truncate', [value, max], text.truncate(value, max));

  // Values here are literal backslash escapes, exercised as data.
  const escaped: Array<[string, boolean]> = [
    ['', false], ['plain', true], ['\\u4e2d\\u6587', false], ['\\uD83D\\uDE80', false],
    ['\\uD800', false], ['\\uD800x\\uDC00', false], ['\\n\\r\\t\\"\\\\', true],
    ['\\n', false], ['\\\\n', true], ['\\\\u0041', false], ['\\uZZZZ', false],
    ['a\\u0000b', false],
  ];
  for (const [value, flag] of escaped) {
    add('decodeEscapedUnicodeText', [value, flag], text.decodeEscapedUnicodeText(value, flag));
  }
  const nested = {a: '\\u0041\\n', b: ['\\u4e2d', 42, null, true], c: {d: '\\uD800'}};
  for (const flag of [false, true]) {
    add('decodeEscapedUnicodeValue', [nested, flag], text.decodeEscapedUnicodeValue(nested, flag));
  }
  add('decodeEscapedUnicodeValue', [undefined, true], text.decodeEscapedUnicodeValue(undefined, true));
  add('decodeEscapedUnicodeValue', [null, false], text.decodeEscapedUnicodeValue(null, false));

  const normalized: string[] = [
    '', '  a  b  ', 'a\n\t b', '\u00a0\u3000\ufeff x \u2028 y', '产品  🚀',
    '\ud800  x  \ud801', '\r\n', 'a\rb',
  ];
  for (const value of normalized) add('normalizeText', [value], text.normalizeText(value));

  const matches: Array<[string, string]> = [
    ['', ''], ['abc', 'abc'], ['ABC', 'abc'], ['alpha', 'alphabet'], ['pha', 'alphabet'],
    ['alpha beta', 'alpha x beta'], ['alpha beta', 'nothing'], ['产品', '产品 指南'],
    ['🚀', '产品🚀x'], ['a b c', 'c b a'], ['İ', 'i̇stanbul'], ['STRASSE', 'straße'],
  ];
  for (const [query, value] of matches) add('scoreMatch', [query, value], text.scoreMatch(query, value));

  const raw: string[] = ['{"a":1,"b":[2]}', '[1,2]', 'null', 'bad', '"\\ud800"', '', '{"a":'];
  for (const value of raw) add('safeJsonParse', [value, {fallback: true}], text.safeJsonParse(value, {fallback: true}));

  // -- id.ts ---------------------------------------------------------------
  for (const value of ['', 'abc', '产品🚀', '\ud800', 'a'.repeat(200)]) {
    add('hashText', [value], id.hashText(value));
  }
  add('nowIso', [], id.nowIso());
  for (const [sessionKey, timestamp, payload] of [
    ['s', '2026-10-22T00:00:00.000Z', 'payload'],
    ['', 't', 'p'], ['会话🚀', 't\ud800', 'p\ud801'],
  ] as const) {
    add('buildL0IndexId', [sessionKey, timestamp, payload], id.buildL0IndexId(sessionKey, timestamp, payload));
  }
  for (const [timestamp, ids] of [
    ['t', ['b', 'a', 'B']], ['t', []], ['t\ud800', ['\ud800', 'a']],
  ] as const) {
    add('buildL1IndexId', [timestamp, [...ids]], id.buildL1IndexId(timestamp, [...ids]));
  }
  add('buildL2TimeIndexId', ['2026-10-22'], id.buildL2TimeIndexId('2026-10-22'));
  for (const key of ['ProjectKey', 'ÜBER', '产品']) {
    add('buildL2ProjectIndexId', [key], id.buildL2ProjectIndexId(key));
    add('buildFactId', [key], id.buildFactId(key));
  }
  add('buildLinkId', ['l0', 'id1', 'l1', 'id2'], id.buildLinkId('l0', 'id1', 'l1', 'id2'));
  add('buildLinkId', ['', '\ud800', 'x', ''], id.buildLinkId('', '\ud800', 'x', ''));

  // -- general-projects.ts -------------------------------------------------
  for (const value of [
    '/a/b/../c/./d//e/', 'relative/x/../y', '', '.', './x', '/..', '//a///b', 'x/../y/../z',
  ]) {
    add('normalizeWorkspacePath', [value], general.normalizeWorkspacePath(value));
  }
  add('generalWorkspaceDir', [], general.GENERAL_WORKSPACE_DIR);
  for (const value of [
    '/fixture/home/.pilotdeck', '/fixture/home/.pilotdeck/.', '/fixture/home/.pilotdeck/../.pilotdeck',
    '/fixture/home/other', '/fixture/home/.pilotdeck2',
  ]) {
    add('isGeneralWorkspaceDir', [value], general.isGeneralWorkspaceDir(value));
    add('getWorkspaceMemoryMode', [value], general.getWorkspaceMemoryMode(value));
  }
  add('buildExternalProjectLogicalId', ['/fixture/ws', 'proj-1'], general.buildExternalProjectLogicalId('/fixture/ws', 'proj-1'));
  add('buildExternalProjectLogicalId', ['/fixture/ws/../ws', '项目'], general.buildExternalProjectLogicalId('/fixture/ws/../ws', '项目'));
  for (const relative of ['dir/file.md', 'dir\\file.md', '../x y.md', '']) {
    add('buildExternalRecordId', ['/fixture/ws', relative], general.buildExternalRecordId('/fixture/ws', relative));
  }
  for (const value of [
    'external:abc:dir/file.md', 'external:abc:dir\\file.md', 'external:  key  :  path  ',
    'external::x', 'external:abc:', 'external:abc', 'other:abc:x', 'external:abc: spaced \\ path ',
  ]) {
    add('parseExternalRecordId', [value], general.parseExternalRecordId(value));
    add('isExternalRecordId', [value], general.isExternalRecordId(value));
  }

  // Empty HOME: homedir() wins even when empty, so the constant is relative
  // and every comparison resolves it against process.cwd().
  process.env.HOME = '';
  const empty = await load('core/general-projects.ts', '?empty-home');
  add('generalWorkspaceDirEmptyHome', [], empty.GENERAL_WORKSPACE_DIR);
  for (const value of ['.pilotdeck', './.pilotdeck/', '/fixture/home/.pilotdeck']) {
    add('isGeneralWorkspaceDirEmptyHome', [value], empty.isGeneralWorkspaceDir(value));
    add('getWorkspaceMemoryModeEmptyHome', [value], empty.getWorkspaceMemoryMode(value));
  }
  process.env.HOME = HOME;

  // -- trace-i18n.ts -------------------------------------------------------
  add('traceI18n', ['trace.key', 'fallback'], trace.traceI18n('trace.key', 'fallback'));
  add('traceI18n', ['trace.key', 'fallback', [[]]], trace.traceI18n('trace.key', 'fallback', []));
  add('traceI18n', ['key', 'fb', ['x', 1, 1.5, -0, NaN, Infinity]],
    trace.traceI18n('key', 'fb', 'x', 1, 1.5, -0, NaN, Infinity));
  // Array.prototype.toString renders null/undefined elements as empty strings.
  // `add` records the rest-argument list, so each array argument is wrapped once.
  const arrayArg = [null, undefined, 1, [2, null], {a: 1}, true];
  add('traceI18n', ['key', 'fb', [arrayArg]], trace.traceI18n('key', 'fb', arrayArg));
  add('traceI18n', ['key', 'fb', [[null]]], trace.traceI18n('key', 'fb', [null]));
  const nestedArg = [[undefined]];
  add('traceI18n', ['key', 'fb', [nestedArg]], trace.traceI18n('key', 'fb', nestedArg));

  // -- service.ts recall ---------------------------------------------------
  const recallBlocks: string[] = [
    '',
    '### [user]\nAlice\n### [project_meta]\nMeta\n### [project]\nProject\n### [feedback]\nFeedback',
    '## ClawXMemory Recall\nroute=auto\nTreat these file memories as the authoritative long-term memory for this turn when relevant.\n### [user]\n  indented  \n\nnext\n### [project]\n\n### [project]\nsecond',
    '### [project_meta]\r\nA\r\nB\r\n### [user]\r\nC',
    '### [unknown]\nignored\n### [user]\nkept',
    '### [user]\n  \n\n',
    '### [user]\nA\nroute=secret\nB',
  ];
  for (const value of recallBlocks) {
    add('parseRawRecallSections', [value], service.parseRawRecallSections(value));
    add('buildMemoryRecallSystemContext', [value], service.buildMemoryRecallSystemContext(value));
  }
  add('buildMemoryRecallSystemContext', ['  \n route=x \n  '], service.buildMemoryRecallSystemContext('  \n route=x \n  '));
  for (const value of [
    undefined, '', '   ', 'route=auto', 'a\nroute=auto', 'a\rroute=auto',
    'a\u2028route=auto', 'a\u2029### [user]', ' ### [user]', '### [user]\n',
    '### [project_meta]', 'x### [project]', 'ROUTE=x',
  ]) {
    add('looksLikeRawRecallContext', [value], service.looksLikeRawRecallContext(value));
  }
  const records: any[] = [
    {retrieval: {contextPreview: '### [user]\nAlice\n### [project]\nP'}},
    {a: 1, retrieval: {x: 1, contextPreview: '### [feedback]\nF\n### [user]\nU', y: 2}, b: 2},
    {retrieval: {contextPreview: 'not raw'}},
    {retrieval: {contextPreview: ''}},
    {retrieval: {}},
    {retrieval: null},
    {other: 1},
  ];
  for (const record of records) {
    add('normalizeCaseTraceContextPreview', [record], service.normalizeCaseTraceContextPreview(record));
  }
  return {cases, clockMs: Date.parse(NOW)};
}
