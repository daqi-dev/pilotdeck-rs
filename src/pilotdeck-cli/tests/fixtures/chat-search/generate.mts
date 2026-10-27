// PDRS_BASELINE=<fixed baseline> node --import <baseline>/node_modules/tsx/dist/loader.mjs <this file> <output dir>
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdir, writeFile, mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { assertBaseline, baselineSourceHashes } from '../../../../pilotdeck-gateway/tests/fixtures/_shared/baseline.mts';
assert.ok(process.env.PDRS_BASELINE); assert.ok(process.argv[2]);
assert.match(process.version, /^v22\./);
const baseline = resolve(process.env.PDRS_BASELINE);
const sourceCommit = assertBaseline(baseline);
const sources = ['src/cli/commands/chatSearch.ts', 'src/session/search/NodeSessionSearchPort.ts',
  'src/session/search/formatChatHistorySearch.ts', 'src/pilot/paths.ts',
  'src/session/storage/SessionList.ts', 'src/session/storage/SessionLiteReader.ts',
  'src/session/storage/ProjectSessionStorage.ts', 'src/session/metadata/SessionMetadataStore.ts'];
const sourceSha256 = await baselineSourceHashes(baseline, sources);
const here = dirname(fileURLToPath(import.meta.url));
const root = await mkdtemp(join(tmpdir(), 'pdrs-chat-generate-'));
const project = join(root, 'project');
const pilot = join(root, 'pilot');
const entry = (type: string, sessionId: string, extra: any) => JSON.stringify({ type, sessionId,
  turnId: 'turn', sequence: 1, createdAt: '2026-01-02T13:04:00.000Z', ...extra });
const content = (text: string) => ({ role: 'user', content: [{ type: 'text', text }] });
const files: Record<string,string> = {
  'pilot/projects/fixture-chat-project/.cwd': '/fixture/chat-project',
  'pilot/projects/fixture-chat-project/chats/a-long-session-123456.jsonl': [
    entry('session_metadata', 'a-long-session-123456', { metadata: { title: 'Custom title', aiTitle: 'AI title' } }),
    '', 'not json',
    entry('accepted_input', 'a-long-session-123456', { messages: [content('  Hello   WORLD\n next line  '), content('second block')] }),
    entry('assistant_message', 'a-long-session-123456', { message: content('hello reply 42') }),
    entry('durable_message', 'a-long-session-123456', { message: content('durable HELLO') }),
    entry('tool_result', 'a-long-session-123456', { message: content('hello ignored') }),
    entry('accepted_input', 'a-long-session-123456', { messages: [content('x'.repeat(85) + ' needle ' + 'z'.repeat(100))] }),
    entry('accepted_input', 'a-long-session-123456', { createdAt: '', messages: [content('emoji 😀 中文 İ Σ hello')] }),
  ].join('\r\n') + '\r\n',
  'pilot/projects/fixture-chat-project/chats/b.jsonl': [
    entry('accepted_input', 'b', { createdAt: 'invalid date', messages: [content('hello last prompt')] }),
    entry('session_metadata', 'b', { metadata: { aiTitle: 'Generated title' } }),
  ].join('\n'),
  'pilot/projects/fixture-chat-project/chats/always-on-report-hidden.jsonl': entry('accepted_input', 'always-on-report-hidden', { messages: [content('hello hidden')] }),
  'pilot/projects/fixture-chat-project/chats/empty.jsonl': '',
  'pilot/projects/fixture-chat-project/chats/not-jsonl.txt': 'hello',
  'pilot/projects/other/chats/c.jsonl': entry('accepted_input', 'c', { messages: [content('hello other project')] }),
};
const edges = process.argv[3] === 'edges';
if (edges) Object.assign(files, {
  'pilot/projects/edge/chats/snippet.jsonl': entry('accepted_input','snippet',{ messages: [content('😀'.repeat(40) + 'odd' + '😀'.repeat(45))] }),
  'pilot/projects/edge/chats/unicode.jsonl': entry('accepted_input', 'unicode', { messages: [content('😀 中文 ١ K ſ')] }),
  'pilot/projects/edge/chats/linebreak.jsonl': entry('accepted_input', 'linebreak', { messages: [content('a\u2028b')] }),
  'pilot/projects/edge/chats/raw.jsonl': [
    entry('session_metadata', 'raw', { metadata: { title: 'First title' } }),
    '', 'broken',
    JSON.stringify({ type: 'accepted_input', sessionId: 'raw', messages: [content('edge raw record')] }),
    entry('session_metadata', 'raw', { metadata: { title: '  Latest title  ' } }),
  ].join('\r'),
  'pilot/projects/edge/chats/patch.jsonl': [
    entry('session_metadata', 'patch', { metadata: { title: 'First title' } }),
    JSON.stringify({ type: 'accepted_input', sessionId: 'patch', messages: [content('edge patch record')] }),
    entry('session_metadata', 'patch', { metadata: { title: '  Latest title  ' } }),
  ].join('\n'),
});
const cases: Array<{id:string, argv:string[], tz?:string}> = (edges ? [
  ['unit-one', ['search','^.$','--regex','--session','unicode']],
  ['utf16-units', ['search','^.. ','--regex','--session','unicode']],
  ['surrogate-snippet', ['search','odd','--session','snippet']],
  ['unit-two', ['search','😀','--regex','--session','unicode']],
  ['ascii-digit', ['search','\\d','--regex','--session','unicode']],
  ['ascii-word', ['search','\\w','--regex','--session','unicode']],
  ['kelvin', ['search','k','--regex','--session','unicode']],
  ['long-s', ['search','s','--regex','--session','unicode']],
  ['space', ['search','\\s','--regex','--session','unicode']],
  ['identity-escape', ['search','\\q','--regex','--session','unicode']],
  ['dot-linebreak', ['search','a.b','--regex','--session','linebreak']],
  ['raw-order-lines', ['search','edge','--session','raw']],
  ['metadata-patch', ['search','edge','--session','patch']],
  ['missing-session', ['search','edge','--session','missing']],
].map(([id, argv]) => [id, [...argv as string[], '--json','--project','/edge']]) : [
 ['usage', []], ['unknown', ['bogus']], ['empty', ['search']], ['flags-only', ['search','--json','--limit','3']],
 ['text', ['search','hello']], ['json', ['search','hello','--json']],
 ['case-sensitive', ['search','hello','--case-sensitive','--json']],
 ['role-user', ['search','hello','--role','user','--json']], ['role-assistant', ['search','hello','--role','assistant','--json']],
 ['role-invalid', ['search','hello','--role','tool','--json']],
 ['all', ['search','hello','--all-projects']], ['all-json', ['search','hello','--all-projects','--json']],
 ['limit', ['search','hello','--limit','1']], ['fraction-limit', ['search','hello','--limit','1.5','--json']],
 ['zero-limit', ['search','hello','--limit','0','--json']], ['negative-limit', ['search','hello','--limit','-2','--json']],
 ['nan-limit', ['search','hello','--limit','NaN','--json']], ['hex-limit', ['search','hello','--limit','0x2','--json']],
 ['infinite-limit', ['search','hello','--limit','Infinity','--json']],
 ['session', ['search','hello','--session','b','--json']],
 ['internal-session', ['search','hello','--session','always-on-report-hidden','--json']],
 ['no-result', ['search','absent']], ['snippet', ['search','needle','--json']],
 ['multiword', ['search','hello','WORLD','--json']], ['unknown-flag', ['search','hello','--bogus','reply','--json']],
 ['missing-role', ['search','hello','--role','--json']], ['repeat-limit', ['search','hello','--limit','2','--limit','1','--json']],
 ['regex', ['search','h.llo','--regex','--json']], ['regex-lookbehind', ['search','(?<=hello )reply','--regex','--json']],
 ['regex-backref', ['search','(l)\\1','--regex','--json']], ['regex-alternation', ['search','reply|durable','--regex','--json']],
 ['regex-invalid-class', ['search','[','--regex']], ['regex-invalid-group', ['search','(','--regex']],
 ['regex-invalid-repeat', ['search','*','--regex']], ['regex-invalid-range', ['search','[z-a]','--regex']],
 ['unicode', ['search','İ','--json']], ['literal-dot', ['search','h.llo','--json']],
 ['pilot-home-token', ['search','--pilot-home','missing','--json']],
 ['empty-project', ['search','hello','--project','','--json']],
]).map(([id, argv]) => ({id: id as string, argv: [...argv as string[], '--project', '/fixture/chat-project']}));
if (edges) {
  const reviewFile = (id: string, rows: string[]) => {
    files[`pilot/projects/review/chats/${id}.jsonl`] = rows.join('\n');
  };
  const row = (id: string, text: string, extra = {}) => entry('accepted_input', id, {messages:[content(text)], ...extra});
  const add = (id: string, argv: string[], tz?: string) => cases.push({id, argv:['search',...argv,'--project','/review'], ...(tz ? {tz} : {})});
  reviewFile('expansion', [row('expansion', 'İ'.repeat(70) + 'x tail')]);
  add('lower-expansion', ['x','--session','expansion','--json']);
  reviewFile('own-title', [entry('session_metadata','own-title',{metadata:{title:'Own title'}}),row('OTHER-ID','title lookup')]);
  reviewFile('OTHER-ID', [entry('session_metadata','OTHER-ID',{metadata:{title:'Other indexed title'}})]);
  reviewFile('missing-id', [JSON.stringify({type:'accepted_input',messages:[content('title lookup')]})]);
  reviewFile('miss-title', [row('unindexed-entry','title lookup')]);
  add('title-entry-key', ['title lookup','--all-projects','--json']);
  reviewFile('lone', [entry('session_metadata','lone',{metadata:{title:'Title \ud800'}}),row('lone','lone \ud800 hello',{createdAt:'date \ud800'}),row('id\ud800','lone id hello')]);
  for (const mode of ['json','text']) add(`lone-${mode}`, ['hello','--session','lone',...(mode === 'json' ? ['--json'] : [])]);
  add('lone-regex', ['\\ud800','--regex','--session','lone','--json']);
  add('lone-not-replacement', ['�','--session','lone','--json']);
  reviewFile('boundary',[row('boundary','éx'),row('boundary','ax'),row('boundary','x')]);
  add('ascii-boundary', ['\\bx','--regex','--session','boundary','--json']);
  reviewFile('date', [row('date','clock pm',{createdAt:'2026-01-02T13:04:00.000Z'}),row('date','clock midnight',{createdAt:'2026-01-02T00:04:00.000Z'}),row('date','clock local midnight',{createdAt:'2026-01-01T16:04:00.000Z'})]);
  for (const tz of ['UTC','Asia/Shanghai','America/New_York']) add(`clock-${tz.replaceAll('/','-')}`,['clock','--session','date'],tz);
  reviewFile('numbers', Array.from({length:30}, (_,i)=>row('numbers',`numeric ${i}`)));
  for (const [i, value] of ['1_000','0x1_0',' 12 ','1e3','','0b11','Infinity','0o3','+0x3'].entries())
    add(`number-${i}`, ['numeric','--limit',value,'--session','numbers','--json']);
  // Error transcripts live in another pilot home: existing --all-projects
  // cases must retain their original input and expected bytes.
  const errorCase = (id: string, rows: string[]) => {
    files[`error-pilot/projects/errors/chats/${id}.jsonl`] = rows.join('\n');
    cases.push({id, argv:['search','probe','--session',id,'--json',
      '--pilot-home','../error-pilot','--project','/errors']});
  };
  const malformed = [['x',String.raw`\x`],['q',String.raw`\q`],
    ['unicode',String.raw`\u12`],['tab','\t'],['nul','\0'],['utf16',String.raw`😀é\q`]];
  for (const position of ['head','tail']) for (const type of ['session_metadata','ordinary'])
    for (const field of ['title','createdAt']) for (const [kind, value] of malformed) {
      const id = `decode-${position}-${type}-${field}-${kind}`;
      const bad = `{"type":"${type}","${field}":"${value}"}`;
      const good = row(id,'probe');
      errorCase(id, position === 'head' ? [bad,good] : [good,' '.repeat(70000),bad]);
    }
  // createdAt is decoded only after a nonempty summary exists. Metadata slow
  // recovery can produce that summary even when the preview cannot see it.
  errorCase('decode-no-summary', ['{"createdAt":"\\q"}']);
  errorCase('decode-slow-summary', ['{"createdAt":"\\q"}',
    JSON.stringify({type:'session_metadata',padding:' '.repeat(70000),metadata:{title:'Recovered'},endPadding:' '.repeat(70000)})]);
  errorCase('decode-field-order', ['{"type":"session_metadata","aiTitle":"\\q"}',
    '{"type":"session_metadata","title":"\\u12"}']);
  const assertionPatterns = ['\\b*','\\b?','\\B{2}','^*','$+','(?<=a)+','(?<!a)?',
    '(?=a)+','(?!a){2}','(?:(?<=a))+','a{2,1}','x{2,1}',
    'a{2147483647,2147483646}'];
  for (const [i, pattern] of assertionPatterns.entries()) for (const sensitive of [false,true])
    add(`assertion-regex-${i}-${sensitive}`, [pattern,'--regex','--session','patterns',
      '--json',...(sensitive ? ['--case-sensitive'] : [])]);
  const subjects = ['Z','[','&','b','x{,3}','\x01','\0','k','xy','aaa','3','-','xzz','zz','u'.repeat(41),'u{41}','a','éx','ax','x','\ud800','é','h','_'];
  reviewFile('patterns', subjects.map(text => row('patterns',text)));
  const patterns = ['[Z-a]','[[]','[a&&b]','[a--b]','x{,3}','\\1','\\0','\\k','(?<a>x)(?<a>y)','a{1000}{1000}{1000}','[\\d]','[\\w-]','[\\s\\S]','\\xzz','\\u{41}','[a-\\d]','(?i)','(?i:a)','[a\\1]','\\8','(x)\\1','(?<name>x)\\k<name>'];
  for (const [i, pattern] of patterns.entries()) for (const sensitive of [false,true])
    add(`review-regex-${i}-${sensitive}`, [pattern,'--regex','--session','patterns','--limit','100','--json',...(sensitive ? ['--case-sensitive'] : [])]);
  // Keep resource probes isolated so all original all-projects inputs stay put.
  files['bounded-pilot/projects/bounded/chats/probe.jsonl'] = [
    row('probe','Z [ & b 3 - x é K ſ _ 😀'),
    row('probe','\t\n\r \u00a0\u1680\u2000\u200a\u2028\u2029\u202f\u205f\u3000\ufeff'),
    row('probe','\ud7ff\ud800\udfff\ue000\uffff'),
  ].join('\n');
  const isolated = (id: string, pattern: string, sensitive: boolean) =>
    cases.push({id, argv:['search',pattern,'--regex','--session','probe','--json',
      '--pilot-home','../bounded-pilot','--project','/bounded',
      ...(sensitive ? ['--case-sensitive'] : [])]});
  files['bounded-pilot/projects/bounded/chats/literal.jsonl'] = row('literal','a'.repeat(130000));
  for (const sensitive of [false,true]) cases.push({id:`bounded-literal-${sensitive}`,
    argv:['search','a'.repeat(65000)+'b','--session','literal','--json',
      '--pilot-home','../bounded-pilot','--project','/bounded',
      ...(sensitive ? ['--case-sensitive'] : [])]});
  for (const sensitive of [false,true]) {
    isolated(`bounded-class-${sensitive}`, `[${String.raw`\S`.repeat(65000)}]`, sensitive);
    // Both positive and complemented escapes, folding, negative classes and
    // intervals crossing the surrogate remapping boundaries use actual TS.
    for (const [i, pattern] of [String.raw`[\D]`, String.raw`[\W]`, String.raw`[\s]`,
      String.raw`[^\S]`, String.raw`[^\D]`, String.raw`[^\W]`,
      String.raw`[\0-\uffff]`, String.raw`[\ud7ff-\ue000]`,
      String.raw`[Z-a\d\s]`].entries())
      isolated(`interval-class-${i}-${sensitive}`, pattern, sensitive);
  }

}

// Reproduce the disclosed giant-quantifier divergence without adding it to
// the passing parity matrix or changing any accepted comparison rules.
if (process.argv[3] === 'giant-quantifier') cases.splice(0,cases.length,
  {id:'giant-quantifier',argv:['search','a{2147483648,2147483647}','--regex','--all-projects','--json']});

try {
  await mkdir(project, {recursive:true});
  for (const [relative, text] of Object.entries(files)) {
    const path = join(root,relative); await mkdir(dirname(path),{recursive:true});
    await writeFile(path,text.replaceAll('{runtime}',root));
  }
  const raw: Array<{id:string, stdout:Buffer, stderr:Buffer}> = [];
  const results = cases.map(test => {
    const out = spawnSync(process.execPath, ['--import',join(baseline,'node_modules/tsx/dist/loader.mjs'),join(here,'driver.mts'),...test.argv],
      {cwd:project,env:{PATH:process.env.PATH,HOME:join(root,'home'),PILOT_HOME:pilot,PDRS_BASELINE:baseline,
        TZ:test.tz ?? 'UTC',LANG:'C',LC_ALL:'C',NO_COLOR:'1'},timeout:30000});
    assert.ifError(out.error); assert.equal(out.signal,null);
    raw.push({id:test.id,stdout:out.stdout,stderr:out.stderr});
    // Exactly the D0 comparison_view prefix exception; all raw bytes retained.
    const stderr = out.stderr.toString().replace(/^\(node:[0-9]+\) ExperimentalWarning: SQLite is an experimental feature and might change at any time\n\(Use `node --trace-warnings \.\.\.` to show where the warning was created\)\n/, '');
    return {...test,exit:out.status,stdout:out.stdout.toString().replaceAll(root,'{runtime}'),stderr:stderr.replaceAll(root,'{runtime}')};
  });
  assertBaseline(baseline); await baselineSourceHashes(baseline,sources);
  const oracle = {provenance:{sourceCommit,sourceSha256,node:process.version},files,cases:results};
  const output = resolve(process.argv[2]); await mkdir(output,{recursive:true});
  for (const capture of raw) {
    await mkdir(join(output,'raw',capture.id),{recursive:true});
    await writeFile(join(output,'raw',capture.id,'stdout.bin'),capture.stdout,{flag:'wx'});
    await writeFile(join(output,'raw',capture.id,'stderr.bin'),capture.stderr,{flag:'wx'});
  }
  const bytes = JSON.stringify(oracle,null,2)+'\n'; await writeFile(join(output,'oracle.json'),bytes,{flag:'wx'});
  console.log(createHash('sha256').update(bytes).digest('hex'));
} finally { await rm(root,{recursive:true,force:true}); }
