// #2622: real fixed-baseline transcript-archive calls.
//
// Drives the REAL createLocalGateway delegates for export_session_transcript
// and restore_session_transcript (createLocalGateway.ts export/restore
// delegates + the portable-archive helpers) against real JSONL session
// transcripts on disk: the success shapes (portable text rendering per
// content-block type, synthetic filtering, title rules), every error branch
// the delegates can answer, the written-bytes shape of a restore
// (session_metadata + accepted_input entries, temp-then-rename), and the
// describeServer capability row.
//
// The busy rows hold a REAL submitted turn: the agent loop runs in-process
// (the default PILOTDECK_AGENT_LOOP_TRANSPORT=native profile) against a local
// model endpoint that accepts connections and never responds, so
// router.hasActiveTurn / router.hasSession are observed through the delegates
// exactly as a live CLI session would produce them.
import assert from 'node:assert/strict';
import http from 'node:http';
import {mkdir, mkdtemp, rm, readFile, readdir, writeFile, utimes} from 'node:fs/promises';
import {join, resolve, dirname} from 'node:path';
import {tmpdir} from 'node:os';
import {pathToFileURL} from 'node:url';
import {installFixedClock} from '../_shared/generator_guard.mjs';
import {assertBaseline, baselineSourceHashes} from '../_shared/baseline.mts';
assert.equal(process.version, 'v22.22.0', '#2622 pinned Node');
assert.equal(process.versions.icu, '78.2', '#2622 pinned ICU');
assert.equal(new Intl.Collator().resolvedOptions().locale, 'en-US',
  'run with LANG=en_US.UTF-8 and LC_ALL/LC_MESSAGES unset (got ' +
  `${new Intl.Collator().resolvedOptions().locale})`);
assert.ok(process.env.PDRS_BASELINE, '#2622 baseline');
assert.ok(process.argv[2], '#2622 output');
const baseline = resolve(process.env.PDRS_BASELINE!), output = resolve(process.argv[2]);
const sourceCommit = assertBaseline(baseline);
const sourceSha256 = await baselineSourceHashes(baseline, [
  'src/cli/createLocalGateway.ts',
  'src/gateway/client/InProcessGateway.ts',
  'src/gateway/SessionRouter.ts',
  'src/session/projection/AgentTranscriptProjections.ts',
  'src/session/storage/ProjectSessionStorage.ts',
  'src/session/events/SessionRuntime.ts',
  'src/session/transcript/InMemoryTranscriptWriter.ts',
  'src/session/transcript/JsonlTranscriptWriter.ts',
  'src/model/protocol/clone.ts',
  'src/model/protocol/toolResultContent.ts',
]);
installFixedClock();
const root = await mkdtemp(join(tmpdir(), 'pdrs-2622-'));
const home = join(root, 'home'), project = join(root, 'project');
for (const path of [home, project, join(home, 'builtin')]) await mkdir(path, {recursive: true});
process.env.HOME = home; process.env.PILOT_HOME = home;
process.env.XDG_CONFIG_HOME = join(home, 'xdg'); process.env.XDG_CACHE_HOME = join(home, 'xdg'); process.env.XDG_DATA_HOME = join(home, 'xdg');
process.env.PILOTDECK_STORAGE_ROOT = join(home, 'storage');
// In-process agent loop (the profile default): the turn's model request runs
// in this process and hangs on the held endpoint below.
process.env.PILOTDECK_AGENT_LOOP_TRANSPORT = 'native';
delete process.env.PILOTDECK_SNAPSHOT_ROOT;
process.chdir(project);

// The never-responding model endpoint: a submitted turn stays active until it
// is aborted, which is what makes hasActiveTurn observable through export.
const hold = http.createServer(() => {});
await new Promise<void>((resolveListen) => hold.listen(0, '127.0.0.1', resolveListen));
const port = (hold.address() as {port: number}).port;
await writeFile(join(home, 'pilotdeck.yaml'),
  `schemaVersion: 1\nagent:\n  model: test/mock\nmodel:\n  providers:\n    test:\n      protocol: openai\n      url: http://127.0.0.1:${port}/v1\n      apiKey: test-only\n      models:\n        mock: {}\n`,
  {flag: 'wx'});

const {createLocalGateway} = await import(pathToFileURL(join(baseline, 'src/cli/createLocalGateway.ts')).href);
await mkdir(join(home, 'projects', 'a8-main'), {recursive: true});
await writeFile(join(home, 'projects', 'a8-main', '.cwd'), project + '\n', {flag: 'wx'});
const {getPilotProjectChatDir} = await import(pathToFileURL(join(baseline, 'src/pilot/paths.ts')).href);
const chat = getPilotProjectChatDir(project, home);
await mkdir(chat, {recursive: true});
assert.equal((await readFile(join(dirname(chat), '.cwd'), 'utf8')).trim(), project, '#2622 stable project registration');
const local = createLocalGateway({projectRoot: project, pilotHome: home, builtinSkillsRoot: join(home, 'builtin')});

// Deterministic transcript entries: fixed clock, fixed uuid-shaped ids.
const createdAt = '2026-01-01T00:00:00.000Z';
let sequence = 0;
const entry = (type: string, turnId: string, fields: Record<string, unknown>) => {
  sequence += 1;
  const entryId = `entry-${String(sequence).padStart(4, '0')}`;
  const base = {type, sessionId: 's', turnId, sequence, createdAt, entryId,
    parentEntryId: sequence === 1 ? null : `entry-${String(sequence - 1).padStart(4, '0')}`};
  return {...base, ...fields};
};
const writeSession = async (key: string, lines: unknown[]) => {
  const path = join(chat, `${key}.jsonl`);
  await writeFile(path, lines.map((line) => JSON.stringify(line)).join('\n') + '\n', {flag: 'wx'});
  await utimes(path, new Date(1700000000000), new Date(1700000000000));
  return path;
};

// The rich session: every portable-text content-block branch, the synthetic
// filter, an incomplete-turn durable message (skipped by the projection), and
// a metadata title that survives the 512-character ceiling.
await writeSession('rich', [
  entry('accepted_input', 't1', {messages: [
    {role: 'user', content: [{type: 'text', text: 'Export me'}]},
  ]}),
  entry('assistant_message', 't1', {message: {role: 'assistant', content: [
    {type: 'text', text: 'Working on it'},
    {type: 'tool_call', id: 'c1', name: 'read_file', input: {path: 'a.txt'}},
    {type: 'tool_call', id: 'c2', name: 'search', input: 'plain string input'},
    {type: 'tool_call', id: 'c3', name: 'no_input'},
    {type: 'tool_result_reference', toolCallId: 'c1'},
    {type: 'image', mimeType: 'image/png'},
  ]}}),
  entry('tool_result_message', 't1', {message: {role: 'user', content: [
    {type: 'tool_result', toolCallId: 'c1', content: [
      {type: 'text', text: 'file bytes here'},
      {type: 'image', source: 'base64', mimeType: 'image/png', data: 'AAAA'},
      {type: 'image', source: 'url', mimeType: 'image/png', data: 'https://example.invalid/a.png'},
      {type: 'pdf', mimeType: 'application/pdf', data: 'BBBB', pages: 2},
    ]},
  ]}}),
  entry('turn_result', 't1', {result: {usage: {}, permissionDenials: []}}),
  entry('session_metadata', 't1', {metadata: {title: 'Rich session'}}),
  entry('accepted_input', 't2', {messages: [
    {role: 'user', metadata: {synthetic: true}, content: [{type: 'text', text: 'hidden by the synthetic filter'}]},
  ]}),
  entry('durable_message', 't-never', {message: {role: 'assistant', content: [{type: 'text', text: 'incomplete turn'}]}}),
  entry('turn_result', 't2', {result: {usage: {}, permissionDenials: []}}),
  entry('assistant_message', 't2', {message: {role: 'assistant', content: [{type: 'text', text: 'Done'}]}}),
]);

// No title anywhere: the exported archive must omit the key entirely.
await writeSession('plain', [
  entry('accepted_input', 't1', {messages: [{role: 'user', content: [{type: 'text', text: 'No title'}]}]}),
  entry('turn_result', 't1', {result: {usage: {}, permissionDenials: []}}),
]);

// Titles past the 512-character ceiling are dropped, not truncated; exactly
// 512 characters survive.
await writeSession('long-title', [
  entry('accepted_input', 't1', {messages: [{role: 'user', content: [{type: 'text', text: 'Titled'}]}]}),
  entry('session_metadata', 't1', {metadata: {title: 'x'.repeat(513)}}),
  entry('turn_result', 't1', {result: {usage: {}, permissionDenials: []}}),
]);
await writeSession('boundary-title', [
  entry('accepted_input', 't1', {messages: [{role: 'user', content: [{type: 'text', text: 'Titled'}]}]}),
  entry('session_metadata', 't1', {metadata: {title: 'y'.repeat(512)}}),
  entry('turn_result', 't1', {result: {usage: {}, permissionDenials: []}}),
]);

// Every message renders to empty text: toPortableSessionArchive yields zero
// messages and validatePortableSessionArchive answers the message-count error
// (SESSION_TRANSCRIPT_EMPTY is unreachable — the count check fires first).
await writeSession('empty-text', [
  entry('accepted_input', 't1', {messages: [{role: 'user', content: [{type: 'text', text: ''}]}]}),
  entry('turn_result', 't1', {result: {usage: {}, permissionDenials: []}}),
]);

// A non-string text block: whatever the real render path does with it (JS
// coercion or a TypeError) is the pinned contract.
await writeSession('coerce', [
  entry('accepted_input', 't1', {messages: [{role: 'user', content: [{type: 'text', text: 'x'}]}]}),
  entry('assistant_message', 't1', {message: {role: 'assistant', content: [{type: 'text', text: 5}]}}),
  entry('turn_result', 't1', {result: {usage: {}, permissionDenials: []}}),
]);

// A tool_result image block without data: the flatten reads data.length and
// throws the TypeError the delegate propagates to the RPC boundary.
await writeSession('bad-block', [
  entry('accepted_input', 't1', {messages: [{role: 'user', content: [{type: 'text', text: 'x'}]}]}),
  entry('assistant_message', 't1', {message: {role: 'assistant', content: [
    {type: 'tool_result', toolCallId: 'c1', content: [{type: 'image', mimeType: 'image/png'}]},
  ]}}),
  entry('turn_result', 't1', {result: {usage: {}, permissionDenials: []}}),
]);

// A torn line: severity "error" diagnostics answer SESSION_TRANSCRIPT_INVALID.
{
  const path = join(chat, 'invalid.jsonl');
  await writeFile(path, JSON.stringify(entry('accepted_input', 't1', {messages: []})) + '\nnot json\n', {flag: 'wx'});
  await utimes(path, new Date(1700000000000), new Date(1700000000000));
}

// The busy session's transcript exists before the turn starts, so the
// in-flight rows pin the guard ORDER (busy before restore conflict).
await writeSession('busy-1', [
  entry('accepted_input', 't1', {messages: [{role: 'user', content: [{type: 'text', text: 'warm'}]}]}),
  entry('turn_result', 't1', {result: {usage: {}, permissionDenials: []}}),
]);

const gateway: any = local.gateway;
const router: any = gateway.router;
const clean = (value: any): any => {
  if (typeof value === 'string') return value.split(root).join('<root>');
  if (Array.isArray(value)) return value.map(clean);
  if (value && typeof value === 'object') {
    return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, clean(item)]));
  }
  return value;
};
// The restore-written transcript carries random uuids (entryId, the
// sdk:restore:<uuid> turn id); everything else is deterministic under the
// fixed clock, so only the uuid shapes are masked for byte-stable oracles.
const uuid = /[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/g;
const call = async (method: 'export_session_transcript' | 'restore_session_transcript', params: any) => {
  const invoke = method === 'export_session_transcript'
    ? gateway.exportSessionTranscript.bind(gateway)
    : gateway.restoreSessionTranscript.bind(gateway);
  try {
    return {ok: clean(await invoke(params))};
  } catch (error: any) {
    return {error: clean({code: error.code || 'gateway_request_failed', message: error.message,
      ...(error.validation ? {validation: error.validation} : {})})};
  }
};
const rows: any[] = [];

// --- export rows ------------------------------------------------------------
const archive = {
  schemaVersion: 1,
  format: 'portable_text_messages',
  title: 'Restored title',
  messages: [
    {role: 'user', text: 'Restore me'},
    {role: 'assistant', text: 'Acknowledged'},
  ],
};
for (const [id, params] of [
  ['rich', {sessionKey: 'rich'}],
  ['plain', {sessionKey: 'plain'}],
  ['long-title', {sessionKey: 'long-title'}],
  ['boundary-title', {sessionKey: 'boundary-title'}],
  ['empty-text', {sessionKey: 'empty-text'}],
  ['coerce', {sessionKey: 'coerce'}],
  ['bad-block', {sessionKey: 'bad-block'}],
  ['missing', {sessionKey: 'missing'}],
  ['invalid', {sessionKey: 'invalid'}],
  ['blank-key', {sessionKey: ' '}],
  ['no-key', {archive}],
  ['nonstring-key', {sessionKey: 5}],
  ['nullish-params', null],
] as [string, any][]) {
  rows.push({id: `export.${id}`, params, result: await call('export_session_transcript', params)});
}

// --- restore rows ------------------------------------------------------------
rows.push({id: 'restore.fresh', params: {sessionKey: 'restored', archive},
  result: await call('restore_session_transcript', {sessionKey: 'restored', archive})});
{
  const text = (await readFile(join(chat, 'restored.jsonl'), 'utf8')).replace(uuid, '<uuid>');
  rows.push({id: 'restore.written-bytes', text});
  rows.push({id: 'restore.reexport', result: await call('export_session_transcript', {sessionKey: 'restored'})});
}
const noTitleArchive = {schemaVersion: 1, format: 'portable_text_messages',
  messages: [{role: 'user', text: 'Restore me too'}]};
rows.push({id: 'restore.no-title', params: {sessionKey: 'restored-plain', archive: noTitleArchive},
  result: await call('restore_session_transcript', {sessionKey: 'restored-plain', archive: noTitleArchive})});
rows.push({id: 'restore.written-bytes.no-title',
  text: (await readFile(join(chat, 'restored-plain.jsonl'), 'utf8')).replace(uuid, '<uuid>')});
rows.push({id: 'restore.exists', params: {sessionKey: 'rich', archive},
  result: await call('restore_session_transcript', {sessionKey: 'rich', archive})});
for (const [id, params] of [
  ['blank-key', {sessionKey: ' ', archive}],
  ['no-key', {archive}],
  ['nonstring-key', {sessionKey: 5, archive}],
  ['nullish-params', null],
] as [string, any][]) {
  rows.push({id: `restore.${id}`, params, result: await call('restore_session_transcript', params)});
}

// The five INVALID_SESSION_ARCHIVE texts, one row per branch.
const oneMessage = [{role: 'user', text: 'x'}];
for (const [name, bad] of [
  ['unsupported-shape', {schemaVersion: 2, format: 'portable_text_messages', messages: oneMessage}],
  ['missing-archive', undefined],
  ['null-archive', null],
  ['empty-messages', {schemaVersion: 1, format: 'portable_text_messages', messages: []}],
  ['null-title', {schemaVersion: 1, format: 'portable_text_messages', messages: oneMessage, title: null}],
  ['bad-role', {schemaVersion: 1, format: 'portable_text_messages', messages: [{role: 'system', text: 'x'}]}],
  ['bad-text', {schemaVersion: 1, format: 'portable_text_messages', messages: [{role: 'user', text: 5}]}],
  ['bad-format', {schemaVersion: 1, format: 'other', messages: oneMessage}],
  ['messages-not-array', {schemaVersion: 1, format: 'portable_text_messages', messages: {}}],
] as [string, any][]) {
  const params = {sessionKey: `bad-${name}`, archive: bad};
  rows.push({id: `restore.invalid.${name}`,
    params: {sessionKey: params.sessionKey,
      ...(bad === undefined || bad === null ? {archive: bad} : {archive: '<archive>'})},
    result: await call('restore_session_transcript', params)});
}

// The count and size ceilings. The params are summarized, not echoed.
{
  const tooMany = {schemaVersion: 1, format: 'portable_text_messages',
    messages: Array.from({length: 10001}, () => ({role: 'user' as const, text: 'x'}))};
  rows.push({id: 'restore.invalid.too-many', params: {sessionKey: 'bad-too-many', messages: 10001},
    result: await call('restore_session_transcript', {sessionKey: 'bad-too-many', archive: tooMany})});
  const big = {schemaVersion: 1, format: 'portable_text_messages',
    messages: [{role: 'user', text: 'y'.repeat(2 * 1024 * 1024)}]};
  rows.push({id: 'restore.invalid.too-large', params: {sessionKey: 'bad-too-large', textBytes: 2 * 1024 * 1024},
    result: await call('restore_session_transcript', {sessionKey: 'bad-too-large', archive: big})});
}

// --- busy rows: a real submitted turn against the held endpoint --------------
const waitUntil = async (predicate: () => boolean, label: string) => {
  for (let attempt = 0; attempt < 3000; attempt++) {
    if (predicate()) return;
    await new Promise((resolveTick) => setImmediate(resolveTick));
  }
  throw Error(`#2622 ${label} never became true`);
};
const busyKey = 'busy-1';
const turn = gateway.submitTurn({sessionKey: busyKey, projectKey: project, channelKey: 'test',
  message: 'hold', runId: 'probe-hold-run'});
const consumed = (async () => { for await (const _event of turn) { /* terminal event ends the stream */ } })();
try {
  await waitUntil(() => router.hasActiveTurn(busyKey), 'hasActiveTurn');
  rows.push({id: 'export.busy', result: await call('export_session_transcript', {sessionKey: busyKey})});
  rows.push({id: 'restore.busy', params: {sessionKey: busyKey, archive},
    result: await call('restore_session_transcript', {sessionKey: busyKey, archive})});
} finally {
  await gateway.abortTurn({sessionKey: busyKey, runId: 'probe-hold-run'});
  await consumed;
}
await waitUntil(() => !router.hasActiveTurn(busyKey), 'turn ended');
// The session stays loaded with no active turn: export still works (the
// delegate only checks hasActiveTurn) while restore refuses (hasSession).
rows.push({id: 'restore.loaded-session', params: {sessionKey: busyKey, archive},
  result: await call('restore_session_transcript', {sessionKey: busyKey, archive})});
rows.push({id: 'export.loaded-session', result: await call('export_session_transcript', {sessionKey: busyKey})});

// The chat tree after everything: restores leave only the final transcripts
// behind (temp .restore.tmp files are renamed away).
rows.push({id: 'chat.files', files: (await readdir(chat)).sort()});

// --- capability row ----------------------------------------------------------
const capabilities = (await gateway.describeServer()).capabilities;
assert.equal(capabilities.includes('session_transcript_archive'), true, '#2622 TS advertises the archive name');

const oracle = {sourceCommit, node: process.version, icu: process.versions.icu, sourceSha256,
  capabilities: clean(capabilities), rows};
await mkdir(output, {recursive: true});
await writeFile(join(output, 'oracle.json'), JSON.stringify(oracle, null, 2) + '\n', {flag: 'wx'});
console.log(JSON.stringify({
  node: process.version, icu: process.versions.icu,
  rows: rows.map((row) => ({id: row.id,
    outcome: row.result?.ok !== undefined ? 'ok' : row.result?.error ? row.result.error.code : 'bytes'})),
}, null, 2));
await local.dispose();
process.chdir(tmpdir());
hold.closeAllConnections?.();
await new Promise<void>((resolveClose) => hold.close(() => resolveClose()));
await rm(root, {recursive: true, force: true});
