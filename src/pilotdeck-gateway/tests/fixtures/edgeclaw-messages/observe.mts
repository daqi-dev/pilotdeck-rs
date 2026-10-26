// Calls the fixed TS baseline directly. Every expected value below comes from
// cc7c4e6 `message-utils.ts`; none is synthesized by the Rust side.
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {createRequire} from 'node:module';
import {dirname, join, resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {freezeClock} from '../edgeclaw-file-memory/observe.mts';

export const NOW = '2026-10-22T00:00:00.000Z';
export const HOME = '/fixture/home';
const ROOT = 'src/context/memory/edgeclaw-memory-core/src';
const PRIVATE = [
  'truncate', 'asRecord', 'pushUniqueText', 'extractTextFromObject',
  'extractTextFromContent', 'stripInjectedMemoryContext', 'compactWhitespace',
  'normalizePluginNoiseProbe', 'countPluginStateMarkers',
  'startsWithPluginStateLine', 'lineLooksLikePluginNoise',
  'paragraphLooksLikePluginNoise', 'stripPluginStateScaffolding',
  'normalizeSenderHint', 'collectSenderHints', 'stripUntrustedSenderMetadata',
  'stripQuotedContextBlocks', 'extractCurrentMessageSegment',
  'stripLeadingTimestampPrefix', 'stripAttachmentScaffolding',
  'stripLeadingCurrentMessageEnvelope', 'looksLikeOpaqueSenderLabel',
  'stripLeadingSenderPrefix', 'stripLeadingMentions',
  'stripLeadingMessageEnvelopeLines', 'stripUserNoise',
  'stripAssistantThinking', 'hasToolCallContent', 'shouldSkipUserMessage',
  'parseLeadingSlashCommand', 'parseBangCommand', 'shouldSkipAssistantMessage',
  'normalizeSingleMessage', 'KNOWN_SLASH_COMMANDS',
];

export function enc(v: any): any {
  if (v === undefined) return {$undefined: true};
  if (typeof v === 'number' && (Object.is(v, -0) || !Number.isFinite(v))) {
    return {$number: Object.is(v, -0) ? '-0' : String(v)};
  }
  if (Array.isArray(v)) return v.map(enc);
  if (v && typeof v === 'object') return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, enc(x)]));
  return v;
}

async function transformed(baseline: string) {
  const sourcePath = join(baseline, ROOT, 'message-utils.ts');
  const requireFromBaseline = createRequire(join(baseline, 'package.json'));
  const {transformSync} = createRequire(requireFromBaseline.resolve('tsx'))('esbuild');
  const source = readFileSync(sourcePath, 'utf8');
  const code = transformSync(
    `${source}\nexport { ${PRIVATE.join(', ')} };\n`,
    {loader: 'ts', format: 'esm', target: 'es2022', sourcefile: sourcePath},
  ).code;
  const executable = code.replace(/from (["'])(\.[^"']+)\1/g, (_: string, quote: string, specifier: string) => {
    let file = resolve(dirname(sourcePath), specifier);
    if (file.endsWith('.js') && file.slice(0, -3) + '.ts') file = file.slice(0, -3) + '.ts';
    return 'from ' + quote + pathToFileURL(file).href + quote;
  });
  return import('data:text/javascript;base64,' + Buffer.from(executable).toString('base64'));
}

export async function collect(baseline: string) {
  process.env.HOME = HOME;
  freezeClock();
  const m = await transformed(baseline);
  const cases: any[] = [];
  const add = (group: string, fn: string, args: any[], expected: any) =>
    cases.push({group, fn, args: enc(args), expected: enc(expected)});
  const call = (group: string, fn: string, ...args: any[]) =>
    add(group, fn, args, (m as any)[fn](...args));
  const one = (group: string, fn: string, value: any) =>
    add(group, fn, [value], (m as any)[fn](value));

  // -- extraction -----------------------------------------------------------
  for (const value of [
    '', 'plain', '\\u4e2d\\u6587', '\\uD800x', ' a \n b ',
  ]) one('extract', 'extractTextFromContent', value);
  for (const value of [
    [], ['a', ' b ', 'a', '', ['c', ['a']]],
    [{type: 'text', text: 'a'}, {type: 'TEXT', text: '\\u0042'}],
    [{type: 'message_text', text: 'm'}, 42, null, true],
    [['deep', [['er']]]],
  ]) one('extract', 'extractTextFromContent', value);
  for (const [value, depth] of [
    [{type: 'text', text: 'a'}, 0],
    [{type: 'paragraph', text: 'p'}, 3],
    [{type: 'input_text', text: 'i'}, 6],
    [{type: 'output_text', text: 'o'}, 5],
    [{type: 'toolCall', text: 'skip'}, 0],
    [{type: 'toolResult', text: 'skip'}, 0],
    [{type: 'image', text: 'skip'}, 0],
    [{type: 'input_image', text: 'skip'}, 0],
    [{type: 'file', text: 'skip'}, 0],
    [{type: 'media', text: 'skip'}, 0],
    [{text: 'a', content: 'b', body: 'c', message: 'd', caption: 'e'}, 0],
    [{text: 'a', parts: ['f'], items: {text: 'g'}, blocks: 'h', segments: ['i'], chunks: ['j']}, 0],
    [{text: 'same', content: 'same'}, 0],
    [{type: 'unknown', text: 'x'}, 0],
    [{content: {content: {content: {content: {content: {content: 'six'}}}}}}, 0],
    [{content: {content: {content: {content: {content: {content: 'seven'}}}}}}, 0],
  ]) add('extract', 'extractTextFromObject', [value, depth], (m as any).extractTextFromObject(value, depth));

  // Depth boundary: `depth > 5` is checked on entry to extractTextFromContent
  // (message-utils.ts:133-135), while extractTextFromObject at depth 5 still
  // returns its own `text` (:102-121). A wrapper at depth 5 extracts, one at
  // depth 6 does not, so lowering MAX_CONTENT_EXTRACTION_DEPTH (:13) to 4
  // changes the depth-5 observations.
  const arrays = (inner: any, n: number) => {
    for (let i = 0; i < n; i += 1) inner = [inner];
    return inner;
  };
  const objects = (inner: any, n: number) => {
    for (let i = 0; i < n; i += 1) inner = {content: inner};
    return inner;
  };
  for (const depth of [3, 4, 5, 6, 7]) {
    one('extract', 'extractTextFromContent', arrays(`s${depth}`, depth));
    one('extract', 'extractTextFromContent', arrays({type: 'text', text: `t${depth}`}, depth));
  }
  for (const n of [4, 5, 6, 7]) {
    one('extract', 'extractTextFromContent', objects({type: 'text', text: `o${n}`}, n));
  }

  // -- injected memory context ---------------------------------------------
  const header = 'You are using retrieved ClawXMemory file memories for this turn.';
  const legacy = 'You are using multi-level memory indexes for this turn.';
  const footer = "Treat the above as authoritative prior memory when it is relevant. Prioritize the user's latest request, and do not claim memory is missing or that this is a fresh conversation if the answer is already shown above.";
  const rheader = '## ClawXMemory Recall';
  const rinstruction = 'Use the following retrieved ClawXMemory evidence for this turn.';
  const rfooter = 'Treat the selected evidence above as authoritative historical memory for this turn when it is relevant.';
  const rfinal = 'If the needed answer is already shown above, do not claim that memory is missing or that this is a fresh conversation.';
  for (const value of [
    `${header}\nOLD\n${footer}\nnew`,
    `${header}\nOLD\n${footer}${header}\nOLD2\n${footer}\nnew`,
    `${legacy}\nOLD\n${footer}`,
    `${header}\nmissing footer`,
    `prefix ${header}\nOLD\n${footer} suffix`,
    `${header}\r\nOLD\r\n${footer}\r\nnew`,
    `${rheader}\n${rinstruction}\nEVIDENCE\n${rfooter}\n${rfinal}\n\nSystem: [2026-10-22] note\nSystem: info\nreal`,
    `${rheader}\n${rinstruction}\nEVIDENCE\n${rfooter}\n${rfinal}\nreal`,
    `${rheader}\n${rinstruction}\nEVIDENCE\n${rfooter}\nmissing final`,
    `before\n${rheader}   ${rinstruction}\nEVIDENCE\n${rfooter} ${rfinal}  \nafter`,
    `${header}\nOLD\n${footer}\n${rheader}\n${rinstruction}\nE\n${rfooter}\n${rfinal}\nafter`,
    `  ${header}  OLD  ${footer}  `,
  ]) one('injected', 'stripInjectedMemoryContext', value);

  // -- whitespace / plugin probe -------------------------------------------
  for (const value of [
    '', '  a  ', 'a\r\nb', 'a\n\n\n b', '\uFEFF x \uFEFF', '\u00a0x\u00a0',
    'a\u2028b\u2029c', ' a \n  b  \n\n\n', '\n\n', 'a\rb',
  ]) one('whitespace', 'compactWhitespace', value);
  for (const value of [
    '', '**bold** `code`', '> - * project', 'x\n---y', 'x\u2028---y',
    '\uFEFF  ## Session Status', '> > > a\n\n  b', 'a ** b ` c', '\t-\r\n*x',
  ]) one('probe', 'normalizePluginNoiseProbe', value);

  const pluginValues = [
    '', 'Project state maintained by Agent', 'session status', 'Session Status: active',
    'current git branch: main', 'Current Git Branch :', 'git status: clean',
    'git status summary: 2 files', 'git status summary : x', 'agent: a\nhost: h\nworkspace: w\nos: o',
    'agent: a\nnode: n\nmodel: m', 'session status\nagent: a\nhost: h',
    'project state maintained by x\nsession status\ngit status: y',
    'real content\n\nagent: a\nhost: h\nworkspace: w\nos: o\nnode: n',
    'agent: a\nhost: h\nworkspace: w\nreal paragraph',
    '> **Session Status**\nsome\n> - current git branch: `main`',
    '**Project state maintained by** Codex\nplain text',
    'agent:a host:h workspace:w os:o',
    'not session status but contains it',
    'real\nsession status\nagent: a',
  ];
  for (const value of pluginValues) {
    for (const fn of ['countPluginStateMarkers', 'startsWithPluginStateLine', 'lineLooksLikePluginNoise', 'paragraphLooksLikePluginNoise', 'stripPluginStateScaffolding']) {
      one('plugin', fn, value);
    }
  }

  // -- sender metadata ------------------------------------------------------
  const senderValues = [
    '',
    'Sender (untrusted metadata): ```json\n{"sender":"Alice","sender_id":"A1"}\n```\nhello',
    'Conversation info (untrusted metadata): ```{"sender_label":"Bob"}\n```',
    'sender (UNTRUSTED METADATA): ```json\n{"sender":42,"sender_id":"x"}\n```',
    'Conversation info (untrusted metadata): {"sender":"Carol"} rest',
    'Sender (untrusted metadata): {"sender":"Dan"}\nSender (untrusted metadata): {"sender":"dan"}',
    'Sender (untrusted metadata): ```json\n{bad json}\n```',
    'Sender (untrusted metadata): ```json\n{"sender":"  Alice   Smith  "}\n```',
    'Sender (untrusted metadata): ```json\n{"sender":"a","sender":"b"}\n```',
    'no metadata here',
    'Sender (untrusted metadata): ```json\n{"sender":"Eve"}\n``` tail ``` code',
    'Sender (untrusted metadata): ```json\n{"sender":"A\ud800B"}\n```',
    'Sender (untrusted metadata): ```json\n{"sender":"🚀"}\n```',
  ];
  for (const value of senderValues) {
    for (const fn of ['collectSenderHints', 'stripUntrustedSenderMetadata']) one('sender', fn, value);
  }
  for (const value of [
    '  Alice   Smith  ', 'ALICE', 'A  B', '', '  ', '张三', '\uFEFFX\uFEFF',
    'A\ud800B', '🚀 A',
  ]) one('sender', 'normalizeSenderHint', value);

  // -- quoted context -------------------------------------------------------
  for (const value of [
    '',
    'Chat history since last reply (untrusted metadata): ```json\n{"a":1}\n```\nnew',
    'Replied message (untrusted context): ```\nold\n```\nnew',
    '[Chat messages since your last reply - for context]old\n[Current message - respond to this]new',
    '[chat messages since your last reply - for context]old\n[Current message - respond to this]new',
    '[Chat messages since your last reply - for context]old only',
    'a[Chat messages since your last reply - for context]x[Current message - respond to this]y[Chat messages since your last reply - for context]z[Current message - respond to this]w',
    '[Current message - respond to this]only current',
    '[Chat messages since your last reply - for context][Current message - respond to this]',
  ]) one('quoted', 'stripQuotedContextBlocks', value);

  // -- timestamp / attachment / envelope -----------------------------------
  for (const value of [
    '', 'plain', '[2026-10-22] hi', '[2026-1-2 00:00 GMT] hi', '[UTC] hi', '[utc] hi',
    '  [2026-10-22] hi', '\n[2026-10-22]\nhi', '[no date] hi', '[2026-10-22 00:00]',
    '[2026-10-22]hi', '[12026-1-1] hi', '[2026-13-1] hi', '[2026-1-1] x',
  ]) one('timestamp', 'stripLeadingTimestampPrefix', value);
  for (const value of [
    '', 'hello', '[media attached: image/png] hi', 'x\n[media attached: a] more ]\nTo send an image back, foo\nhello',
    '[media attached: a]\n[media attached: b] tail', 'To send an image back, see this',
    'x\nTo send an image back, foo\ny', 'hello\n<file name="a">content</file>tail',
    '<file a>text</file>', '<files>', 'a\n<files>', 'a\n<file>inline</file>',
    '[media attached: x]\r\nhello', '<file\nmulti>',
  ]) one('attachment', 'stripAttachmentScaffolding', value);
  for (const value of [
    '', 'plain', '[a][b] Label: rest', '[a] [b]: rest', '[a]  : x', '[a] label: first\nsecond',
    `[${'x'.repeat(320)}] ok: yes`, `[${'x'.repeat(321)}] no: yes`,
    `[a] ${'y'.repeat(120)}: yes`, `[a] ${'y'.repeat(121)}: yes`,
    '[a]\nlabel: x', '[a]\n\nlabel: x', '[a]\n\n: x', '[a]  : x', '[a] [b]  c: d',
    '[a][b]: c', '[a] : x', '[a] :', '[a] b:\n\nrest',
  ]) one('envelope', 'stripLeadingCurrentMessageEnvelope', value);

  // -- opaque label / sender prefix ----------------------------------------
  for (const value of [
    '', 'Alice', 'User123', 'user_1', 'a.b@c', 'a b', '张三', 'a'.repeat(121), 'abcd',
    'a-b', 'A B C', '()', '@', 'x1', '1x',
  ]) one('opaque', 'looksLikeOpaqueSenderLabel', value);
  for (const value of [
    'alice: hi', 'Alice: hi', 'U123: hi', 'user: hi', '张三: hi', 'a: b',
    'no colon here', ': leading', `${'x'.repeat(121)}: hi`, 'label:first\nsecond\nthird',
  ]) {
    for (const hints of [[], ['alice'], ['张三']]) {
      add('sender-prefix', 'stripLeadingSenderPrefix', [value, hints], (m as any).stripLeadingSenderPrefix(value, hints));
    }
  }

  // -- mentions / envelope lines -------------------------------------------
  for (const value of [
    '', 'hello', '@alice hello', '  @alice   hello', '@a-b.c @bob  hi',
    '@@x', '@', '@x\n  y', '@x \n\n y', 'text @not-leading',
    '@a\u00a0@b  hi', '@a\uFEFF@b hi', '@a\t@b\n@c x', '@a@b', '@a.',
    '@-a', '@å x', '@a_b-c.d x', '@a\u2028@b x', '@a\r\n@b  tail',
  ]) one('mentions', 'stripLeadingMentions', value);
  for (const value of [
    '', 'plain', '```json\n{"a":1}\n```\nreal',
    '{\n"a":1\n}\nreal', 'Conversation info (untrusted metadata): x\nreal',
    'Conversation info (untrusted metadata)\nreal', 'Chat history since last reply (untrusted x): y\nreal',
    'Replied message (untrusted metadata)\nreal', 'System: [2026-10-22] note\nreal',
    'System: [GMT] note\nreal', '[message_id:abc]\nreal', '[message_id:]\nreal',
    '[x for context]\nreal', '[Current message - respond to this]\nreal',
    '[2026-10-22 channel:12] X: hello\nreal', '[2026-10-22] X: hello\nreal',
    `[a]\n[${'x'.repeat(121)}]: y\nreal`,
    '\n\n```\nreal', '```\n```json\nreal',
  ]) one('envelope-lines', 'stripLeadingMessageEnvelopeLines', value);

  // -- user noise composites ------------------------------------------------
  const userValues = [
    '',
    'hi',
    'Sender (untrusted metadata): ```json\n{"sender":"Alice"}\n```\nAlice: hello',
    'Conversation info (untrusted metadata): ```json\n{"sender":"A"}\n```\n[2026-10-22] A: hi',
    '[Chat messages since your last reply - for context]\nold\n[Current message - respond to this]\nnew',
    'old\n[Chat messages since your last reply - for context]\nold2\n[Current message - respond to this]\nnew',
    'Project state maintained by X\nagent: a\nhost: h\nworkspace: w\nos: o\nactual',
    '[media attached: x]\n<file name="a">payload</file>\nreal',
    '> **Session Status**\n> - current git branch: `main`\nreal',
    '@alice @bob  hello',
    '\uFEFF  [2026-10-22] hi  \uFEFF',
    'A new session was started via /new or /reset.\nRun your Session Startup sequence now',
  ];
  for (const value of userValues) one('user', 'stripUserNoise', value);

  // -- thinking -------------------------------------------------------------
  for (const value of [
    '', 'plain', 'abc</think>answer', '<think>hidden</think>answer',
    '<thinking a=1>hidden</thinking>answer', 'a<thinking>h</thinking>b<think>h</think>c',
    '</thinkx>keep', '<think>only open', '用户的需要：测试\n答案',
    '用户 需要 这是\n答案', '搜索结果 不太理想\n答案', '这搜索 找不到\n答案',
    '第一段\n\n第二段', '用户需要帮助\n\n答案\n\nProject state maintained by X',
    '先想\n\n>\n- agent: a\nhost: h\nworkspace: w\nos: o',
    '  a  \n\n\n b <think>x</think> c ',
    'a<think>one</think>keep<think>two</think>b',
    'a<thinking>one</thinking>keep<thinking>two</thinking>b',
    'a<think>one</think>keep<thinking>two</thinking>b',
    '</think><think>one</think>keep<think>two</think>b',
    '</thinking><thinking>one</thinking>keep<thinking>two</thinking>b',
  ]) one('thinking', 'stripAssistantThinking', value);

  // -- commands / session ---------------------------------------------------
  for (const value of [
    '', 'hello', '/help', '/HELP', '/help ', '/help:', '/help: x', '/help:x',
    '/unknown', '/status now', '/t', '/t:x', '/ send', '/', '/1help',
    '!poll', '!poll x', '! poll', '!  bash something', '!unknown', '!',
    '! stop', '!poll: x', '  /help  ', 'A new session was started via /new or /reset.',
    'A new session was started via /new or /reset.\nRun your Session Startup sequence now',
    'new session was started via /new or /reset.',
  ]) {
    one('commands', 'parseLeadingSlashCommand', value);
    one('commands', 'parseBangCommand', value);
    one('commands', 'isSessionStartupMarkerText', value);
    one('commands', 'isCommandOnlyUserText', value);
  }
  // Every item in the TS whitelist itself: the exact command, the uppercase
  // form with the optional colon, and a trailing unit that must make the
  // command unknown again. Iterating the real Set (instead of a copied list)
  // keeps this fixture covering the whitelist even when TS adds an entry.
  for (const command of m.KNOWN_SLASH_COMMANDS as Set<string>) {
    for (const value of [`/${command}`, `/${command.toUpperCase()}:`, `/${command}x`]) {
      one('commands', 'parseLeadingSlashCommand', value);
    }
  }
  for (const args of [
    [[], 'text'], [[{type: 'toolCall'}], 'text'], [[{type: 'toolCall'}, {type: 'text'}], 'text'],
    [[{type: 'toolResult'}], 'text'], [[{type: 'tool_use'}], 'text'], [[{type: 'tool_result'}], 'text'],
    [[{type: 'other'}], 'text'], [null, 'text'], ['text', 'text'], [[1, null, 'x'], 'text'],
  ] as const) {
    add('tools', 'hasToolCallContent', [args[0]], (m as any).hasToolCallContent(args[0]));
  }
  for (const value of ['', 'hi', '/help', '!poll', 'A new session was started via /new or /reset.',
    'Based on this conversation, generate a short 1-2 word filename slug: x', '  ', '用户需要']) {
    one('skip-user', 'shouldSkipUserMessage', value);
  }
  for (const [raw, content] of [
    [null, ''], [null, 'plain'], [null, '用户需要'], [[{type: 'toolCall'}], 'text'],
    [[{type: 'other'}], 'text'], [null, '搜索结果 不太理想'], [null, '用户需要\nmore'],
    [null, 'line\nline'], [null, '  用户需要  '],
  ] as const) {
    add('skip-assistant', 'shouldSkipAssistantMessage', [raw, content], (m as any).shouldSkipAssistantMessage(raw, content));
  }

  // -- inspect / normalize --------------------------------------------------
  const raws: any[] = [
    null, undefined, 0, 'x', [], {},
    {role: 'user', content: 'hi', id: 'u1'},
    {role: 'assistant', content: 'a', id: 'a1'},
    {role: 'system', content: 's', id: 's1'},
    {role: 'USER', content: 'hi'},
    {role: 'user', content: '  ', id: 'empty'},
    {role: 'user', content: '', id: ''},
    {message: {role: 'user', content: 'nested', id: 'n1'}},
    {role: 'user', content: null, message: {role: 'assistant', content: 'fallback', id: 'n2'}},
    {role: 'assistant', content: [{type: 'text', text: 'visible'}, {type: 'toolCall', id: 't'}]},
    {role: 'assistant', content: [{type: 'toolResult', id: 't'}]},
    {role: 'user', content: [{type: 'text', text: 'u'}], id: 'arr'},
    {role: 'user', type: 'user', toolUseResult: {}, content: 'skip'},
    {role: 'user', type: 'user', toolUseResult: null, content: 'keep'},
    {role: 'user', type: 'user', isMeta: true, content: 'skip'},
    {role: 'user', type: 'user', isMeta: false, content: 'keep'},
    {role: 'user', type: 'user', isVisibleInTranscriptOnly: 1, content: 'skip'},
    {role: 'user', type: 'user', isCompactSummary: 'x', content: 'skip'},
    {role: 'user', type: 'user', isVirtual: {}, content: 'skip'},
    {role: 'user', type: 'assistant', toolUseResult: {}, content: 'keep'},
    {role: 'user', content: 'A new session was started via /new or /reset.'},
    {role: 'user', content: '/help'},
    {role: 'user', content: '!poll'},
    {role: 'user', content: 'Based on this conversation, generate a short 1-2 word filename slug: x'},
    {role: 'user', content: 'Project state maintained by X\nagent: a\nhost: h\nworkspace: w\nos: o'},
    {role: 'user', content: 'Sender (untrusted metadata): ```json\n{"sender":"A"}\n```\n[2026-10-22] A: hello'},
    {role: 'user', content: '[Chat messages since your last reply - for context]\nold\n[Current message - respond to this]\nnew'},
    {role: 'assistant', content: '<think>hidden</think>answer'},
    {role: 'assistant', content: '用户需要帮助\n\nanswer'},
    {role: 'assistant', content: 'plain\nline'},
    {role: 'user', content: '  padded  ', id: 'pad'},
  ];
  const singleOptions = {includeAssistant: true, maxMessageChars: 10};
  for (const raw of raws) {
    one('inspect', 'inspectTranscriptMessage', raw);
    one('inspect', 'isSessionBoundaryMarkerMessage', raw);
    add('normalize-one', 'normalizeSingleMessage', [raw, singleOptions],
      (m as any).normalizeSingleMessage(raw, singleOptions));
  }
  const mixed: any[] = [
    {role: 'user', content: 'first', id: 'u1'},
    {role: 'assistant', content: 'a1', id: 'a1'},
    {role: 'assistant', content: 'a2', id: 'a2'},
    {role: 'user', content: 'second', id: 'u2'},
    {role: 'assistant', content: 'a3', id: 'a3'},
    {role: 'user', content: 'third', id: 'u3'},
    {role: 'assistant', content: 'a4', id: 'a4'},
  ];
  const onlyAssistant: any[] = [
    {role: 'assistant', content: 'a1', id: 'a1'},
    {role: 'assistant', content: 'a2', id: 'a2'},
    {role: 'assistant', content: 'a3', id: 'a3'},
  ];
  const strategies = ['last_turn', 'full_session'] as const;
  const bools = [true, false] as const;
  const maxes = [0, -1, 3, 2.5, NaN, Infinity] as const;
  for (const strategy of strategies) {
    for (const includeAssistant of bools) {
      for (const max of maxes) {
        const options = {includeAssistant, maxMessageChars: max, captureStrategy: strategy};
        add('normalize', 'normalizeMessages', [mixed, options], (m as any).normalizeMessages([...mixed], options));
        add('normalize', 'normalizeMessages', [onlyAssistant, options], (m as any).normalizeMessages([...onlyAssistant], options));
        add('normalize', 'normalizeTranscriptMessage', [mixed[3], includeAssistant, max], (m as any).normalizeTranscriptMessage(mixed[3], {includeAssistant, maxMessageChars: max}));
      }
    }
  }
  add('normalize', 'normalizeMessages', [[], {includeAssistant: true, maxMessageChars: 10, captureStrategy: 'last_turn'}],
    (m as any).normalizeMessages([], {includeAssistant: true, maxMessageChars: 10, captureStrategy: 'last_turn'}));
  add('normalize', 'normalizeMessages', [[null, 'x', {}, {role: 'system', content: 's'}], {includeAssistant: true, maxMessageChars: 10, captureStrategy: 'last_turn'}],
    (m as any).normalizeMessages([null, 'x', {}, {role: 'system', content: 's'}], {includeAssistant: true, maxMessageChars: 10, captureStrategy: 'last_turn'}));


  // -- code-unit edges (lone surrogates, astral pairs, PUA) -----------------
  const odd = ['\ud800', 'a\ud83d', '\udc00x', '🚀x', '\u{f0000}', 'A\u2028B\u2029C'];
  for (const value of odd) {
    for (const fn of [
      'compactWhitespace', 'normalizePluginNoiseProbe', 'stripUserNoise',
      'stripAssistantThinking', 'stripLeadingMessageEnvelopeLines',
      'stripLeadingTimestampPrefix', 'stripAttachmentScaffolding',
      'stripLeadingCurrentMessageEnvelope', 'stripLeadingMentions',
      'extractCurrentMessageSegment', 'stripQuotedContextBlocks',
      'stripUntrustedSenderMetadata', 'stripInjectedMemoryContext',
    ]) one('edge', fn, value);
    one('edge', 'extractTextFromContent', value);
    one('edge', 'shouldSkipUserMessage', value);
    const raw = {role: 'user', content: value};
    add('edge', 'normalizeTranscriptMessage', [raw, true, 2.5],
      (m as any).normalizeTranscriptMessage(raw, {includeAssistant: true, maxMessageChars: 2.5}));
  }
  for (const value of [
    `[${'🚀'.repeat(160)}] ok: yes`,
    `[${'🚀'.repeat(161)}] no: yes`,
    `[a] ${'🚀'.repeat(60)}: yes`,
    `[a] ${'🚀'.repeat(61)}: yes`,
    `[a] ${'x'.repeat(119)}🚀: yes`,
    `[a] ${'x'.repeat(120)}🚀: no`,
    `[🚀] ${'用户'.repeat(60)}: yes`,
  ]) one('edge', 'stripLeadingCurrentMessageEnvelope', value);
  for (const value of [
    '[media attached: x]\rhello',
    'a\rTo send an image back, x\rb',
    '[media attached: a]\u2028[media attached: b] tail',
    'x\r<file a>payload</file>',
  ]) one('edge', 'stripAttachmentScaffolding', value);


  // -- deterministic mixed strings -----------------------------------------
  let seed = 0x5eed1234;
  const next = () => (seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0);
  const alphabet = [
    'a', 'b', 'c', ' ', ':', '[', ']', '(', ')', '@', '!', '/', '-', '*', '\n',
    '\r', '\t', '\uFEFF', '\u00a0', '\u2028', '\ud800', '🚀', '{', '}', '`', '.', '_', '1',
  ];
  for (let index = 0; index < 80; index += 1) {
    let value = '';
    const length = next() % 49;
    for (let step = 0; step < length; step += 1) value += alphabet[next() % alphabet.length];
    for (const fn of [
      'compactWhitespace', 'normalizePluginNoiseProbe', 'stripUserNoise',
      'stripLeadingMessageEnvelopeLines', 'stripLeadingCurrentMessageEnvelope',
      'stripLeadingMentions', 'stripAssistantThinking', 'stripQuotedContextBlocks',
      'stripAttachmentScaffolding', 'stripLeadingTimestampPrefix',
      'stripInjectedMemoryContext', 'extractCurrentMessageSegment',
      'shouldSkipUserMessage', 'isCommandOnlyUserText', 'isSessionStartupMarkerText',
      'parseLeadingSlashCommand', 'parseBangCommand',
    ]) one('edge', fn, value);
    one('edge', 'extractTextFromContent', value);
    add('edge', 'stripLeadingSenderPrefix', [value, []],
      (m as any).stripLeadingSenderPrefix(value, []));
  }

  // -- JS non-Unicode `/i` Canonicalize boundaries --------------------------
  // V8 22 folds Σ/σ/ς and µ/Μ, but not LONG S, KELVIN SIGN, ß/ẞ, İ/i or ı/I.
  // Every ASCII literal pattern below therefore needs ASCII-only folding.
  const boundary = {
    longS: '\u017F', kelvin: '\u212A', dotlessI: '\u0131', dottedI: '\u0130',
    sharpS: '\u00DF', capSharpS: '\u1E9E', capSigma: '\u03A3',
    sigma: '\u03C3', finalSigma: '\u03C2', micro: '\u00B5', mu: '\u03BC',
  };
  for (const [name, value] of Object.entries(boundary)) {
    one('case-fold', 'normalizeSenderHint', `A${value}B${name}`);
  }
  for (const value of ['ΑΣ', 'Σ', 'ΑΣ Α', 'İstanbul', 'STRASSE', 'straße', 'µ/μ']) {
    one('case-fold', 'normalizeSenderHint', value);
  }
  const quotedBoundary = [
    '[Chat messages since your last reply - for context]old[Current message - respond to this]new',
    '[Chat me\u017F\u017Fages since your last reply - for context]old[Current message - respond to this]new',
    '[Chat messages since your last reply - for context]old[Current me\u017F\u017Fage - re\u017Fpond to this]new',
    '[Chat messages since your last reply - for context]old[Current message - respond to thi\u017F]new',
    '[Chat messages since your last reply - for context]old[Current message - respond to thi\u212A]new',
    '[CHAT MESSAGES SINCE YOUR LAST REPLY - FOR CONTEXT]old[CURRENT MESSAGE - RESPOND TO THIS]new',
  ];
  for (const value of quotedBoundary) {
    one('case-fold', 'stripQuotedContextBlocks', value);
    one('case-fold', 'extractCurrentMessageSegment', value);
  }
  for (const value of [
    '[Current message - respond to this]first[Current message - respond to this]last',
    '[Current message - respond to this]only',
    '[Current message - respond to this]',
  ]) one('case-fold', 'extractCurrentMessageSegment', value);
  for (const value of [
    'x\nTo send an image back, foo\ny',
    'x\nTo \u017Fend an image back, foo\ny',
    'x\nTO SEND AN IMAGE BACK, foo\ny',
    '[media attached: x] hi',
    '[med\u0130a attached: x] hi',
    '[MEDIA ATTACHED: x] hi',
  ]) one('case-fold', 'stripAttachmentScaffolding', value);
  for (const value of [
    'session status', '\u017Fession status', 'ses\u017Fion status', 'sess\u0130on status',
    'SESSION STATUS', 'Project state maintained by Agent',
    'Project \u017Ftate maintained by Agent', 'PROJECT STATE MAINTAINED BY AGENT',
    'git status: clean', 'git \u017Ftatus: clean', 'GIT STATUS: CLEAN',
    'agent: a\nhost: h\nworkspace: w\nos: o',
    'agent: a\nho\u017Ft: h\nworkspace: w\nos: o',
    'AGENT: a\nHOST: h\nWORKSPACE: w\nOS: o',
  ]) {
    for (const fn of ['lineLooksLikePluginNoise', 'startsWithPluginStateLine', 'countPluginStateMarkers']) {
      one('case-fold', fn, value);
    }
  }
  for (const value of [
    'System: note\nreal', 'Sy\u017Ftem: note\nreal', '\u017Fystem: note\nreal', 'SYSTEM: note\nreal',
  ]) one('case-fold', 'stripInjectedMemoryContext', value);
  for (const value of [
    '<think>hidden</think>answer', '<thin\u212A>hidden</thin\u212A>answer',
    '<THINK>hidden</THINK>answer', 'a<thin\u212Aing>h</thin\u212Aing>b',
  ]) one('case-fold', 'stripAssistantThinking', value);
  for (const value of [
    'System: [2026-10-22] note\nreal', 'Sy\u017Ftem: [2026-10-22] note\nreal',
    '[x for context]\nreal', '[x for conte\u017Ft]\nreal', '[X FOR CONTEXT]\nreal',
    '[Current message - respond to this]\nreal', '[Current me\u017F\u017Fage - respond to this]\nreal',
    '[message_id:abc]\nreal', '[me\u017F\u017Fage_id:abc]\nreal', '[MESSAGE_ID:abc]\nreal',
  ]) one('case-fold', 'stripLeadingMessageEnvelopeLines', value);
  for (const value of [
    'Sender (untrusted metadata): ```json\n{"sender":"Alice"}\n```\nhello',
    '\u017Fender (untrusted metadata): ```json\n{"sender":"Alice"}\n```\nhello',
    'Sender (untrusted metadata): ```j\u017Fon\n{"sender":"Alice"}\n```\nhello',
    'Sender (untrusted metadata): ```JSON\n{"sender":"Alice"}\n```\nhello',
    'SENDER (UNTRUSTED METADATA): ```json\n{"sender":"Alice"}\n```\nhello',
  ]) one('case-fold', 'collectSenderHints', value);
  for (const value of [
    'A new session was started via /new or /reset.\nRun your Session Startup sequence now',
    'A new session was started via /new or /reset.\nRun your Se\u017F\u017Fion Startup sequence now',
    'A new session was started via /new or /reset.\nRUN YOUR SESSION STARTUP SEQUENCE NOW',
  ]) one('case-fold', 'isSessionStartupMarkerText', value);
  for (const [value, hints] of [
    ['A\u0130lice: hi', ['ali\u0307ce']],
    ['\u017Falice: hi', ['\u017Falice']],
    ['ΑΣ: hi', ['ας']],
    ['Σ: hi', ['σ']],
  ] as const) {
    add('case-fold', 'stripLeadingSenderPrefix', [value, hints],
      (m as any).stripLeadingSenderPrefix(value, hints));
  }

  assert.ok(cases.length > 300, `only ${cases.length} cases`);
  return {cases, clockMs: Date.parse(NOW)};
}
