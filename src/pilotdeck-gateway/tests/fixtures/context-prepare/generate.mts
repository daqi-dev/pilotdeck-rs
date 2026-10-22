// Real TS DefaultContextRuntime prepareForModel corpus.
//
// Runs the pinned baseline's real DefaultContextRuntime, PromptAssembler,
// MessageProjector and InstructionDiscovery against an exclusive temporary
// project tree. Counterpart replay: tests/context_prepare.rs.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import {
  lstat,
  mkdir,
  mkdtemp,
  readFile,
  symlink,
  writeFile,
} from 'node:fs/promises';
import { realpathSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, isAbsolute, join, relative, resolve, sep } from 'node:path';
import { pathToFileURL } from 'node:url';
import { bindSourceTree } from '../../baseline_source_guard.mjs';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE to the fixed TS baseline');
const BASELINE_COMMIT = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const baseline = resolve(process.env.PDRS_BASELINE);
const gitExecutable = realpathSync(
  process.env.PDRS_GIT || execFileSync('which', ['git'], { encoding: 'utf8' }).trim(),
);
const binding = bindSourceTree(baseline, BASELINE_COMMIT, gitExecutable);
const gitEnv = Object.fromEntries(
  Object.entries(process.env).filter(([key]) => !key.startsWith('GIT_')),
);
const git = (...args: string[]) =>
  execFileSync(gitExecutable, ['--no-replace-objects', '-C', baseline, ...args], {
    env: gitEnv,
    maxBuffer: 32 * 1024 * 1024,
  });
assert.equal(
  git('status', '--porcelain', '--untracked-files=no').toString(),
  '',
  'baseline has uncommitted tracked changes',
);

// Array.prototype.sort and localeCompare follow the ICU default locale.
assert.equal(
  new Intl.Collator().resolvedOptions().locale,
  'en-US',
  'generate with LANG=en_US.UTF-8 (see README.md)',
);

// The calibrating implementation is the full relative import closure.
async function resolveSource(absolute: string): Promise<string | undefined> {
  const stem = absolute.endsWith('.js') ? absolute.slice(0, -3) : undefined;
  const candidates = [
    absolute,
    `${absolute}.ts`,
    `${absolute}.tsx`,
    `${absolute}.mts`,
    join(absolute, 'index.ts'),
    ...(stem ? [`${stem}.ts`, `${stem}.tsx`] : []),
  ];
  for (const candidate of candidates) {
    try {
      if ((await lstat(candidate)).isFile()) return candidate;
    } catch {
      // next candidate
    }
  }
  return undefined;
}
async function sourceClosure(entry: string): Promise<string[]> {
  const seen = new Set<string>([entry]);
  const queue = [entry];
  while (queue.length > 0) {
    const relativePath = queue.shift()!;
    const text = await readFile(join(baseline, relativePath), 'utf8');
    const specifiers = [
      ...text.matchAll(/(?:\bfrom\s*|\bimport\s*\(\s*|\bimport\s+)(['"])([^'"]+)\1/g),
    ].map((match) => match[2]);
    for (const specifier of specifiers) {
      if (!specifier.startsWith('./') && !specifier.startsWith('../')) continue;
      const resolved = await resolveSource(join(baseline, dirname(relativePath), specifier));
      assert.ok(resolved, `unresolved baseline import ${specifier} in ${relativePath}`);
      const path = relative(baseline, resolved).split(sep).join('/');
      if (!seen.has(path)) {
        seen.add(path);
        queue.push(path);
      }
    }
  }
  return [...seen].sort();
}
const entry = 'src/context/DefaultContextRuntime.ts';
const closureEntries = [
  entry,
  'src/context/prompt/PromptContributionRegistry.ts',
  'src/context/prompt/registerExtensionPromptContributions.ts',
];
const closureSet = new Set<string>();
for (const closureEntry of closureEntries) {
  for (const path of await sourceClosure(closureEntry)) closureSet.add(path);
}
const closure = [...closureSet].sort();
const sourceSha256: Record<string, string> = {};
for (const path of closure) {
  assert.ok(path.startsWith('src/'), `import closure escaped src/: ${path}`);
  const committed = git('show', `${BASELINE_COMMIT}:${path}`);
  const working = await readFile(join(baseline, path));
  assert.deepEqual(working, committed, `${path} differs from ${BASELINE_COMMIT}`);
  sourceSha256[path] = createHash('sha256').update(committed).digest('hex');
}

const { DefaultContextRuntime } = await import(
  pathToFileURL(join(baseline, entry)).href
);
const { PromptContributionRegistry } = await import(
  pathToFileURL(join(baseline, 'src/context/prompt/PromptContributionRegistry.ts')).href
);
const { registerExtensionPromptContributions } = await import(
  pathToFileURL(
    join(baseline, 'src/context/prompt/registerExtensionPromptContributions.ts'),
  ).href
);
const { PromptCacheCoordinator } = await import(pathToFileURL(join(baseline, 'src/context/cache/PromptCacheCoordinator.ts')).href);
const { InstructionDiscovery } = await import(
  pathToFileURL(join(baseline, 'src/context/instructions/InstructionDiscovery.ts')).href
);

const output = await mkdtemp(
  join(process.argv[2] ? resolve(process.argv[2]) : tmpdir(), 'context-prepare-'),
);
const fixture = '<fixture>';

// ---------------------------------------------------------------- project tree
const invalid = Buffer.concat([
  Buffer.from('deep instructions '),
  Buffer.from([0xff, 0xfe]),
  Buffer.from(' tail\n'),
]);
const files: Record<string, string | Buffer> = {
  'project/PILOTDECK.md': 'root project instructions\r\n',
  'project/.pilotdeck/PILOTDECK.md': 'hidden project instructions\n',
  'project/.pilotdeck/rules/10-first.md': 'first rule\n',
  'project/.pilotdeck/rules/20-second.md': 'second rule\n',
  'project/.pilotdeck/rules/SKILL.md': 'excluded skill\n',
  'project/.pilotdeck/rules/notes.txt': 'excluded text\n',
  'project/.pilotdeck/rules/link-target.txt': 'symlink target content\n',
  'project/.pilotdeck/rules/B.md': 'upper B rule\n',
  'project/.pilotdeck/rules/a.md': 'lower a rule\n',
  'project/.pilotdeck/rules/😀.md': 'astral rule\n',
  'project/.pilotdeck/rules/\uE000.md': 'private use rule\n',
  'project/.pilotdeck/rules/\uFFFD.md': 'replacement-name rule\n',
  'project/sub/PILOTDECK.md': 'sub project instructions\n',
  'project/sub/.pilotdeck/rules/z.md': 'sub rule z\n',
  'project/sub/PILOTDECK.local.md': 'local private instructions\n',
  'project/sub/deep/PILOTDECK.md': invalid,
  'project/sub/deep/.pilotdeck/PILOTDECK.md': '\uFEFF  bom wrapped  \uFEFF\n',
  'project/sub/deep/PILOTDECK.local.md': ' \n\t\n ',
  'project2/PILOTDECK.md': 'outside cwd instructions\n',
  'project-file-rules/PILOTDECK.md': 'file-rules project instructions\n',
  'project-file-rules/.pilotdeck/rules': 'a regular file where a directory is expected\n',
  'home/PILOTDECK.md': 'user global instructions\n',
  'home/rules/b.md': 'user rule b\n',
  'home/rules/a.md': 'user rule a\n',
  'home/rules/SKILL.md': 'excluded\n',
  'managed/PILOTDECK.md': 'managed instructions\n',
  'skills/a/SKILL.md': '# skill a\n',
  'skills/b/SKILL.md': '# skill b\n',
};
const directories = [
  'emptyhome',
  'emptyproject',
  'emptyproject/.pilotdeck/rules',
  'home/rules',
  'managed',
  'project/.pilotdeck/rules/x.md',
  'project2',
  'project-file-rules',
];
// Dirent.kind for a symlink is "symlink", so TS `tryAddRulesDir` never reads
// these even though the name ends in `.md`; the replayed Rust uses
// `DirEntry::file_type()` (lstat) for the same filter.
const symlinks = [
  { path: 'project/.pilotdeck/rules/link.md', target: 'link-target.txt' },
];
async function materialize(root: string): Promise<void> {
  for (const dir of directories) await mkdir(join(root, dir), { recursive: true });
  for (const [path, content] of Object.entries(files)) {
    await mkdir(dirname(join(root, path)), { recursive: true });
    await writeFile(join(root, path), content);
  }
  for (const link of symlinks) {
    await mkdir(dirname(join(root, link.path)), { recursive: true });
    await symlink(link.target, join(root, link.path));
  }
}
await materialize(output);

// ---------------------------------------------------------------- fixtures
const text = (value: string) => ({ type: 'text', text: value });
const user = (...blocks: any[]) => ({ role: 'user', content: blocks });
const assistant = (...blocks: any[]) => ({ role: 'assistant', content: blocks });
const call = (id: string) => ({ type: 'tool_call', id, name: 'lookup', input: { q: id } });
const result = (id: string) => ({
  type: 'tool_result',
  toolCallId: id,
  content: [text(`result ${id}`)],
});
const media = (id: string) => ({
  type: 'media_reference',
  toolCallId: id,
  mediaType: 'image',
  path: `<fixture>/media-${id}.png`,
  mimeType: 'image/png',
});
const compactBoundary = () =>
  user(text('<compact-boundary trigger="auto" preTokens="10">'));
const compactSummary = () =>
  assistant(text('[CONTEXT COMPACTION - REFERENCE ONLY]\nsummary'));

type Extension = {
  commands?: any[];
  skills?: any[];
  mcp?: any[];
  promptContributions?: { name: string; content: string }[];
};
type AbortMode = 'section' | 'variable' | 'memory' | 'memory-pending';
type DynamicText =
  | { kind: 'string'; value: string }
  /** Runtime non-string despite the TS `string` type; `resolveText` rejects it. */
  | { kind: 'non-string'; value: unknown }
  | { kind: 'throw'; message: string }
  /** Abort the step's controller only when the runner is in `when` mode. */
  | { kind: 'abort-when'; value: string; when: AbortMode };
type Contributions = {
  sections?: {
    name: string;
    order: number;
    text?: string;
    dynamic?: DynamicText;
    complete?: boolean;
  }[];
  runtimeContexts?: {
    name: string;
    order: number;
    text?: string;
    dynamic?: DynamicText;
  }[];
  variables?: { name: string; value: string; abort?: boolean }[];
};
type Discovery = {
  projectRoot: string;
  cwd: string;
  pilotHome: string;
  managed?: string | null;
};
type Step = {
  clockMs: number;
  input: any;
  /** Use the previous step's prepared messages as this step's input. */
  messagesFromPrevious?: boolean;
  cacheAction?: 'reset' | 'release' | 'dispose';
  compact?: 'compacted' | 'skipped';
  /**
   * `before`: abort before `prepareForModel`; `section`/`variable`: abort from
   * inside that provider, after it has been called.
   */
  abort?: 'before' | AbortMode;
};
type Case = {
  name: string;
  setup?: {
    surface?: string;
    includeExtensions?: boolean;
    extension?: Extension;
    discovery?: Discovery;
    contributions?: Contributions;
    projectRoot?: string;
    memoryTimeoutMs?: number;
    memory?: { context?: string; error?: string; pending?: boolean; diagnostics?: any[]; abort?: boolean; expectQuery?: string; expectRecentCount?: number; expectProjectRoot?: string };
  };
  steps: Step[];
};

const baseInput = (overrides: any = {}) => ({
  sessionId: 'session',
  turnId: 'turn',
  cwd: '<fixture>/project',
  provider: 'provider',
  model: 'model',
  permissionMode: 'default',
  additionalWorkingDirectories: [],
  messages: [user(text('hello'))],
  tools: [{ name: 'base' }],
  ...overrides,
});
const baseClock = Date.UTC(2026, 0, 2, 3, 4, 5);
const extensionFull: Extension = {
  commands: [
    { name: 'alpha', description: 'Alpha command', argumentHint: '<file>' },
    { name: 'beta', description: '', argumentHint: '' },
    { name: 'gamma' },
  ],
  skills: [
    { name: 'skill-a', description: 'First skill', path: '<fixture>/skills/a/SKILL.md' },
    { name: 'skill-b', path: '<fixture>/skills/b/SKILL.md' },
  ],
  mcp: [
    { serverName: 'zeta', instructions: 'zeta instructions' },
    { serverName: 'Alpha', instructions: '  alpha instructions  ' },
    { serverName: 'ä', instructions: 'umlaut instructions' },
    { serverName: 'empty', instructions: '   ' },
    { serverName: 'x' },
    { serverName: 'amp&"<', instructions: 'escaped server' },
  ],
};

const sevenProjectionMessages = () => [
  user(text('u1')),
  assistant(text('a1')),
  user(text('u2')),
  assistant(call('c1')),
  user(result('c1')),
  assistant(text('a3')),
  user(text('u3')),
];

const cases: Case[] = [
  {
    name: 'basic',
    steps: [{ clockMs: baseClock, input: baseInput() }],
  },
  {
    name: 'web-search-tool',
    steps: [
      {
        clockMs: baseClock,
        input: baseInput({ tools: [{ name: 'base' }, { name: 'web_search' }] }),
      },
    ],
  },
  {
    name: 'web-fetch-tool',
    steps: [
      {
        clockMs: baseClock,
        input: baseInput({ tools: [{ name: 'web_fetch', extra: true }, { name: 'base' }] }),
      },
    ],
  },
  {
    name: 'custom-extensions',
    setup: { includeExtensions: true, extension: extensionFull },
    steps: [
      {
        clockMs: baseClock,
        input: baseInput({
          customSystemPrompt: '  custom prompt  ',
          appendSystemPrompt: '  appended  ',
          permissionMode: 'bypassPermissions',
          runMode: 'ask',
          additionalWorkingDirectories: ['/tmp/extra', '<fixture>/relative'],
        }),
      },
    ],
  },
  {
    name: 'custom-no-extensions',
    setup: { includeExtensions: false, extension: extensionFull },
    steps: [
      {
        clockMs: baseClock,
        input: baseInput({ customSystemPrompt: 'custom only', appendSystemPrompt: 'tail' }),
      },
    ],
  },
  {
    name: 'custom-empty-user-surface',
    setup: { surface: 'user_message', extension: extensionFull },
    steps: [
      {
        clockMs: baseClock,
        input: baseInput({ customSystemPrompt: '   ', appendSystemPrompt: '   ' }),
      },
      {
        clockMs: baseClock,
        input: baseInput({ customSystemPrompt: '   ', appendSystemPrompt: '   ' }),
        messagesFromPrevious: true,
      },
    ],
  },
  {
    name: 'append-only',
    steps: [
      {
        clockMs: baseClock,
        input: baseInput({ appendSystemPrompt: '\n appended tail \n' }),
      },
    ],
  },
  {
    name: 'user-message-surface',
    setup: { surface: 'user_message' },
    steps: [
      {
        clockMs: baseClock,
        input: baseInput({
          messages: [
            user(text('first request')),
            assistant(text('reply')),
            user(text('second request')),
            { ...user(text('synthetic runtime context')), metadata: { synthetic: true, purpose: 'runtime_context' } },
          ],
        }),
      },
      {
        clockMs: baseClock,
        input: baseInput(),
        messagesFromPrevious: true,
      },
    ],
  },
  {
    name: 'projection-truncate',
    steps: [
      {
        clockMs: baseClock,
        input: baseInput({
          maxMessages: 3,
          messages: [
            user(text('u1')),
            assistant(text('a1')),
            user(text('u2')),
            assistant(call('c1')),
            user(result('c1')),
            assistant(text('a3')),
            user(text('u3')),
          ],
        }),
      },
      {
        clockMs: baseClock,
        input: baseInput({
          maxMessages: 5,
          messages: [
            user(text('u1')),
            assistant(text('a1')),
            user(text('u2')),
            assistant(call('c1')),
            user(result('c1')),
            assistant(text('a3')),
            user(text('u3')),
          ],
        }),
      },
      {
        clockMs: baseClock,
        input: baseInput({
          maxMessages: 2,
          messages: [
            user(text('u1')),
            assistant(call('c1')),
            user(result('c1')),
            assistant(text('a2')),
          ],
        }),
      },
    ],
  },
  {
    // JS `maxMessages` is `number`: negative limits take the truncation path
    // and can drop more messages than exist, fractional positive limits read
    // `messages[cutIndex]` as undefined (TypeError), NaN/Infinity never
    // truncate. These values are the measured TS baseline outputs.
    name: 'projection-max-messages-js-number-edge',
    steps: [
      {
        clockMs: baseClock,
        input: baseInput({ maxMessages: -1, messages: sevenProjectionMessages() }),
      },
      {
        clockMs: baseClock,
        input: baseInput({
          maxMessages: { $number: '-0' },
          messages: sevenProjectionMessages(),
        }),
      },
      {
        clockMs: baseClock,
        input: baseInput({
          maxMessages: { $number: 'NaN' },
          messages: sevenProjectionMessages(),
        }),
      },
      {
        clockMs: baseClock,
        input: baseInput({
          maxMessages: { $number: 'Infinity' },
          messages: sevenProjectionMessages(),
        }),
      },
      { clockMs: baseClock, input: baseInput({ maxMessages: { $number: '-Infinity' }, messages: sevenProjectionMessages() }) },
      {
        clockMs: baseClock,
        input: baseInput({ maxMessages: 2.5, messages: sevenProjectionMessages() }),
      },
    ],
  },
  {
    name: 'projection-orphan',
    steps: [
      {
        clockMs: baseClock,
        input: baseInput({
          messages: [
            user(result('gone'), media('gone-media'), text('kept text')),
            assistant(call('matched')),
            user(result('matched'), text('tail')),
          ],
        }),
      },
    ],
  },
  {
    name: 'projection-inject',
    steps: [
      {
        clockMs: baseClock,
        input: baseInput({
          messages: [
            user(text('hi')),
            assistant(call('c1'), text('doing')),
            assistant(text('next')),
            user(text('done')),
          ],
        }),
      },
    ],
  },
  {
    name: 'projection-checkpoint',
    steps: [
      {
        clockMs: baseClock,
        input: baseInput({
          maxMessages: 1,
          messages: [compactBoundary(), compactSummary(), user(text('after checkpoint'))],
        }),
      },
    ],
  },
  {
    name: 'projection-media-visible',
    steps: [
      {
        clockMs: baseClock,
        input: baseInput({
          messages: [
            assistant(call('c1')),
            user(result('c1'), media('c1'), media(''), text('tail')),
          ],
        }),
      },
    ],
  },
  {
    name: 'extension-full',
    setup: { extension: extensionFull, surface: 'system_prompt' },
    steps: [
      {
        clockMs: Date.UTC(2026, 0, 5, 0, 0, 0),
        input: baseInput({
          permissionMode: 'plan',
          runMode: 'plan',
          additionalWorkingDirectories: ['<fixture>/dir one'],
          tools: [{ name: 'web_search' }, { name: 'base' }],
        }),
      },
    ],
  },
  {
    name: 'instructions-full',
    setup: {
      discovery: {
        projectRoot: '<fixture>/project',
        cwd: '<fixture>/project/sub/deep',
        pilotHome: '<fixture>/home',
        managed: '<fixture>/managed',
      },
    },
    steps: [
      {
        clockMs: baseClock,
        input: baseInput({
          cwd: '<fixture>/project/sub/deep',
          messages: [user(text('deep request'))],
        }),
      },
    ],
  },
  {
    name: 'instructions-root',
    setup: {
      discovery: {
        projectRoot: '<fixture>/project',
        cwd: '<fixture>/project',
        pilotHome: '<fixture>/home',
        managed: '<fixture>/managed',
      },
    },
    steps: [{ clockMs: baseClock, input: baseInput({ messages: [user(text('root'))] }) }],
  },
  {
    name: 'instructions-dedupe',
    setup: {
      discovery: {
        projectRoot: '<fixture>/project',
        cwd: '<fixture>/project',
        pilotHome: '<fixture>/home',
        managed: '<fixture>/project',
      },
    },
    steps: [{ clockMs: baseClock, input: baseInput() }],
  },
  {
    name: 'instructions-none',
    setup: {
      discovery: {
        projectRoot: '<fixture>/emptyproject',
        cwd: '<fixture>/emptyproject',
        pilotHome: '<fixture>/emptyhome',
        managed: null,
      },
    },
    steps: [{ clockMs: baseClock, input: baseInput({ cwd: '<fixture>/emptyproject' }) }],
  },
  {
    // `project/.pilotdeck/rules` contains a symlink named `link.md` and a
    // directory named `x.md`; `tryAddRulesDir` filters both by Dirent.kind.
    name: 'instructions-rules-boundaries',
    setup: {
      discovery: {
        projectRoot: '<fixture>/project',
        cwd: '<fixture>/project',
        pilotHome: '<fixture>/emptyhome',
        managed: null,
      },
    },
    steps: [
      {
        clockMs: baseClock,
        input: baseInput({ cwd: '<fixture>/project', messages: [user(text('rules boundaries'))] }),
      },
    ],
  },
  {
    // `.pilotdeck/rules` is a regular file: readDirectory throws ENOTDIR and
    // TS swallows it like a missing directory.
    name: 'instructions-file-rules',
    setup: {
      discovery: {
        projectRoot: '<fixture>/project-file-rules',
        cwd: '<fixture>/project-file-rules',
        pilotHome: '<fixture>/emptyhome',
        managed: null,
      },
    },
    steps: [
      {
        clockMs: baseClock,
        input: baseInput({
          cwd: '<fixture>/project-file-rules',
          messages: [user(text('file rules'))],
        }),
      },
    ],
  },
  {
    // cwd is outside projectRoot: the chain still walks root -> filesystem
    // root -> cwd, so the sibling project2 instructions are loaded last.
    name: 'instructions-outside-cwd',
    setup: {
      discovery: {
        projectRoot: '<fixture>/project',
        cwd: '<fixture>/project2',
        pilotHome: '<fixture>/emptyhome',
        managed: null,
      },
    },
    steps: [
      {
        clockMs: baseClock,
        input: baseInput({ cwd: '<fixture>/project2', messages: [user(text('outside cwd'))] }),
      },
    ],
  },
  {
    name: 'input-boundaries',
    steps: [
      {
        clockMs: baseClock,
        input: baseInput({
          customSystemPrompt: null,
          appendSystemPrompt: null,
          maxMessages: null,
          stepId: 7,
          runtimeContextSurface: null,
          messages: [user(text('dropped by maxMessages=null'))],
        }),
      },
      {
        clockMs: baseClock,
        input: baseInput({
          runtimeContextSurface: 'other',
          stepId: null,
          messages: [],
          tools: [],
        }),
      },
      {
        clockMs: baseClock,
        input: baseInput({
          customSystemPrompt: '',
          stepId: 0,
          runtimeContextSurface: 'user_message',
        }),
      },
    ],
  },
  {
    name: 'contributions',
    setup: {
      discovery: {
        projectRoot: '<fixture>/project',
        cwd: '<fixture>/project',
        pilotHome: '<fixture>/home',
        managed: null,
      },
      extension: {
        promptContributions: [
          { name: 'ext-b', content: 'EB' },
          { name: 'ext-a', content: 'EA' },
        ],
      },
      contributions: {
        sections: [
          { name: 'zeta', order: 5, text: 'Z {{cwd}}' },
          { name: 'alpha', order: 5, text: 'A' },
          { name: 'static', order: 10, text: 'S' },
        ],
        runtimeContexts: [
          { name: 'ctx-b', order: 1, text: 'C {{project}}' },
          { name: 'ctx-a', order: 1, text: 'D' },
        ],
        variables: [{ name: 'project', value: 'P' }],
      },
    },
    steps: [
      {
        clockMs: baseClock,
        input: baseInput({
          tools: [
            { name: 'z', inputSchema: { b: 1 } },
            { name: 'a', description: 'A', inputSchema: { a: 1 }, extra: true },
          ],
        }),
      },
    ],
  },
  {
    name: 'contributions-complete',
    setup: {
      contributions: {
        sections: [{ name: 'complete', order: 0, text: 'COMPLETE {{model}}', complete: true }],
        runtimeContexts: [{ name: 'ctx', order: 0, text: 'X' }],
      },
    },
    steps: [{ clockMs: baseClock, input: baseInput() }],
  },
  {
    name: 'contributions-unknown-variable',
    setup: {
      contributions: {
        sections: [{ name: 'bad', order: 0, text: '{{missing}}' }],
      },
    },
    steps: [{ clockMs: baseClock, input: baseInput() }],
  },
  {
    name: 'contributions-missing-cwd',
    setup: {
      contributions: {
        sections: [{ name: 'bad', order: 0, text: '{{cwd}}' }],
      },
    },
    steps: [{ clockMs: baseClock, input: baseInput({ cwd: undefined }) }],
  },
  {
    name: 'contributions-multiple-complete',
    setup: {
      contributions: {
        sections: [
          { name: 'b', order: 1, text: 'B', complete: true },
          { name: 'a', order: 0, text: 'A', complete: true },
        ],
      },
    },
    steps: [{ clockMs: baseClock, input: baseInput() }],
  },
  {
    name: 'contributions-duplicate-tool',
    setup: { contributions: {} },
    steps: [
      {
        clockMs: baseClock,
        input: baseInput({ tools: [{ name: 'same' }, { name: 'same' }] }),
      },
    ],
  },
  {
    // A rejected dynamic text provider propagates its own error; TS does not
    // wrap it in the `must resolve to a string` message.
    name: 'contributions-dynamic-provider-error',
    setup: {
      contributions: {
        sections: [
          {
            name: 'dynamic',
            order: 0,
            dynamic: { kind: 'throw', message: 'dynamic provider boom' },
          },
        ],
      },
    },
    steps: [{ clockMs: baseClock, input: baseInput() }],
  },
  {
    // Resolving to a non-string is the *only* case with the TypeError text.
    name: 'contributions-dynamic-provider-non-string',
    setup: {
      contributions: {
        runtimeContexts: [
          { name: 'dynamic', order: 0, dynamic: { kind: 'non-string', value: 42 } },
        ],
      },
    },
    steps: [{ clockMs: baseClock, input: baseInput() }],
  },
  {
    // Dynamic providers keep working when they resolve to a string (the
    // `JsValue` conversion is not only on the error path).
    name: 'contributions-dynamic-provider-string',
    setup: {
      contributions: {
        sections: [
          { name: 'dynamic', order: 0, dynamic: { kind: 'string', value: 'DYN {{model}}' } },
        ],
      },
    },
    steps: [{ clockMs: baseClock, input: baseInput() }],
  },
  {
    // TS captures tool schemas before sorting / the multiple-complete check /
    // provider resolution, so the duplicate schema error wins.
    name: 'contributions-tool-capture-first',
    setup: {
      contributions: {
        sections: [
          { name: 'dynamic', order: 0, dynamic: { kind: 'throw', message: 'provider ran too early' } },
          { name: 'b', order: 1, text: 'B', complete: true },
          { name: 'a', order: 2, text: 'A', complete: true },
        ],
      },
    },
    steps: [
      {
        clockMs: baseClock,
        input: baseInput({ tools: [{ name: 'same' }, { name: 'same' }] }),
      },
    ],
  },
  {
    // Same ordering proof without a second early error: a late capture would
    // invoke this provider and surface its thrown message instead.
    name: 'contributions-tool-capture-before-provider',
    setup: {
      contributions: {
        sections: [
          {
            name: 'dynamic',
            order: 0,
            dynamic: { kind: 'throw', message: 'provider ran too early' },
          },
        ],
      },
    },
    steps: [
      {
        clockMs: baseClock,
        input: baseInput({ tools: [{ name: 'same' }, { name: 'same' }] }),
      },
    ],
  },
  {
    // TS checks `abortSignal.throwIfAborted()` at snapshot entry, before
    // captureToolSchemas. The duplicate schemas and the throwing provider must
    // therefore both lose to the abort.
    name: 'contributions-abort-before',
    setup: {
      contributions: {
        sections: [
          {
            name: 'dynamic',
            order: 0,
            dynamic: { kind: 'throw', message: 'provider ran after abort' },
          },
        ],
      },
    },
    steps: [
      {
        clockMs: baseClock,
        abort: 'before',
        input: baseInput({ tools: [{ name: 'same' }, { name: 'same' }] }),
      },
    ],
  },
  {
    // The section provider aborts and still returns a string. With no
    // registered variables there is no later pre-check to rescue the abort, so
    // only `resolveText`'s post-provider check can reject "LEAK".
    name: 'contributions-abort-during-provider',
    setup: {
      contributions: {
        sections: [
          {
            name: 'dynamic',
            order: 0,
            dynamic: { kind: 'abort-when', value: 'LEAK', when: 'section' },
          },
        ],
      },
    },
    steps: [{ clockMs: baseClock, abort: 'section', input: baseInput() }],
  },
  {
    // Static sections resolve, then the variable provider aborts before
    // returning; only the variable-loop post-provider check can reject it.
    name: 'contributions-abort-during-variable',
    setup: {
      contributions: {
        sections: [{ name: 'static', order: 0, text: 'S' }],
        variables: [{ name: 'later', value: 'LATE', abort: true }],
      },
    },
    steps: [{ clockMs: baseClock, abort: 'variable', input: baseInput() }],
  },
  {
    name: 'date-rollover',
    steps: [
      { clockMs: Date.UTC(2026, 0, 1, 23, 59, 59), input: baseInput() },
      { clockMs: Date.UTC(2026, 0, 2, 0, 0, 1), input: baseInput() },
      { clockMs: Date.UTC(2026, 0, 2, 12, 0, 0), input: baseInput() },
      {
        clockMs: Date.UTC(2026, 0, 3, 1, 0, 0),
        input: baseInput({ previewOnly: true }),
      },
      { clockMs: Date.UTC(2026, 0, 3, 1, 0, 0), input: baseInput() },
      { clockMs: Date.UTC(2026, 0, 4, 1, 0, 0), input: baseInput() },
    ],
  },
  {
    name: 'date-checkpoint-refresh',
    steps: [
      { clockMs: Date.UTC(2026, 0, 1, 0, 0, 0), input: baseInput() },
      {
        clockMs: Date.UTC(2026, 0, 3, 0, 0, 0),
        input: baseInput({ messages: [compactBoundary(), compactSummary(), user(text('after'))] }),
      },
      {
        clockMs: Date.UTC(2026, 0, 4, 0, 0, 0),
        input: baseInput({ messages: [compactBoundary(), compactSummary(), user(text('after'))] }),
      },
    ],
  },
  {
    name: 'date-prune',
    steps: [
      { clockMs: Date.UTC(2026, 0, 1, 0, 0, 0), input: baseInput({ messages: [user(text('a')), user(text('b'))] }) },
      { clockMs: Date.UTC(2026, 0, 2, 0, 0, 0), input: baseInput({ messages: [user(text('a')), user(text('b'))] }) },
      { clockMs: Date.UTC(2026, 0, 3, 0, 0, 0), input: baseInput({ messages: [user(text('changed')), user(text('b'))] }) },
    ],
  },
  {
    name: 'permission-run-modes',
    steps: [
      {
        clockMs: baseClock,
        input: baseInput({ permissionMode: 'plan', runMode: 'ask' }),
      },
      {
        clockMs: baseClock,
        input: baseInput({ permissionMode: 'bypassPermissions', runMode: 'plan' }),
      },
      {
        clockMs: baseClock,
        input: baseInput({ permissionMode: 'custom-mode', runMode: 'default' }),
      },
      {
        clockMs: baseClock,
        input: baseInput({ permissionMode: 'default', runMode: '' }),
      },
    ],
  },
];
// Attachments: real builder, injected retrieval contract (no EdgeClaw dependency).
const memoryInput = (extra: any = {}) => baseInput({ ...extra });
for (const [name, memory, extra] of [
  ['text', { context: '\uFEFF  remembered\n detail \uFEFF', expectQuery: ' newest ', expectRecentCount: 3, expectProjectRoot: '<fixture>/memory-root' }, {}],
  ['empty', { context: ' \n\t\uFEFF', diagnostics: [{ code: 'memory_context_empty', severity: 'info', message: 'empty provider context' }] }, {}],
  ['absent', { diagnostics: [{ code: 'memory_disabled', severity: 'warning', message: 'disabled' }] }, {}],
  ['error', { error: 'provider boom' }, {}],
  ['provider-truncated', { context: 'provider prefix…', diagnostics: [{ code: 'memory_context_empty', severity: 'warning', message: 'provider truncated result' }] }, {}],
  ['projected', { context: 'after projection', expectQuery: ' newest ', expectRecentCount: 1 }, { maxMessages: 1 }],
  ['no-user', { context: 'still retrieve', expectQuery: '', expectRecentCount: 0 }, { messages: [] }],
] as const) {
  cases.push({ name: `memory-${name}`, setup: { memory, ...(name === 'text' ? { projectRoot: '<fixture>/memory-root' } : {}) }, steps: [{ clockMs: baseClock, input: memoryInput({ messages: [user(text('old')), assistant(text('reply')), user(text('  '), text(' newest '), text('ignored'))], ...extra }) }] });
}
cases.push({ name: 'memory-user-surface', setup: { surface: 'user_message', memory: { context: 'durable memory' }, discovery: { projectRoot: '<fixture>/project', cwd: '<fixture>/project', pilotHome: '<fixture>/emptyhome' } }, steps: [{ clockMs: baseClock, input: memoryInput() }, { clockMs: baseClock, messagesFromPrevious: true, input: memoryInput() }] });
cases.push({ name: 'memory-timeout', setup: { memory: { pending: true }, memoryTimeoutMs: 2 }, steps: [{ clockMs: baseClock, input: memoryInput() }] });
for (const mode of ['before', 'memory', 'memory-pending'] as const) {
  cases.push({ name: `memory-abort-${mode}`, setup: { memory: { abort: true, context: 'returned while aborting' }, discovery: { projectRoot: '<fixture>/project', cwd: '<fixture>/project', pilotHome: '<fixture>/emptyhome' } }, steps: [{ clockMs: baseClock, abort: mode, input: memoryInput({ protocol: 'anthropic', supportsPromptCache: true }) }, { clockMs: baseClock + 86400000, input: memoryInput() }] });
}
// Stable cwd is intentional: a temp path inside the SHA-256 input cannot be
// normalized after hashing. These cases perform no instruction filesystem IO.
const cacheInput = (extra: any = {}) => baseInput({ cwd: '/pdrs-cache-fixture', customSystemPrompt: 'cache system', protocol: 'anthropic', supportsPromptCache: true, ...extra });
cases.push({ name: 'cache-gating', steps: [
  { clockMs: baseClock, input: cacheInput() },
  { clockMs: baseClock, input: cacheInput({ protocol: 'openai' }) },
  { clockMs: baseClock, input: cacheInput({ supportsPromptCache: false }) },
  { clockMs: baseClock, input: cacheInput({ supportsPromptCache: 'true' }) },
  { clockMs: baseClock, input: cacheInput({ protocol: 'Anthropic' }) },
] });
cases.push({ name: 'cache-generations', steps: [
  { clockMs: baseClock, input: cacheInput() },
  { clockMs: baseClock, input: cacheInput() },
  { clockMs: baseClock, input: cacheInput({ previewOnly: true, messages: [user(text('preview'))] }) },
  { clockMs: baseClock, input: cacheInput() },
  { clockMs: baseClock, input: cacheInput({ messages: [user(text('committed'))] }) },
  { clockMs: baseClock, input: cacheInput({ sessionId: 'other' }) },
  { clockMs: baseClock, cacheAction: 'reset', input: cacheInput({ previewOnly: true, messages: [user(text('committed'))] }) },
  { clockMs: baseClock, input: cacheInput({ messages: [user(text('committed'))] }) },
  { clockMs: baseClock, cacheAction: 'reset', input: cacheInput({ supportsPromptCache: false }) },
  { clockMs: baseClock, input: cacheInput({ messages: [user(text('committed'))] }) },
  { clockMs: baseClock, cacheAction: 'release', input: cacheInput() },
  { clockMs: baseClock, input: cacheInput({ messages: [user(text('different'))] }) },
  { clockMs: baseClock, cacheAction: 'dispose', input: cacheInput({ messages: [user(text('different'))] }) },
  { clockMs: baseClock, compact: 'skipped', input: cacheInput({ messages: [user(text('different'))] }) },
  { clockMs: baseClock, compact: 'compacted', input: cacheInput({ messages: [user(text('different'))] }) },
] });
const sys = (s: string) => ({ role: 'system', content: [text(s)] });
cases.push({ name: 'cache-breakpoint-edges', steps: [
  [], [sys('only')], [user(text('1'))], [user(text('1')), sys('middle'), assistant(text('2')), user(text('3'))],
  [user(text('old')), sys('middle'), user(text('1')), assistant(text('2')), user(text('3')), sys('tail')],
].map(messages => ({ clockMs: baseClock, input: cacheInput({ messages, customSystemPrompt: '', runtimeContextSurface: 'other' }) })) });
cases.push({ name: 'cache-tool-order', steps: [
  [{ name: 'z', inputSchema: { b: 2, a: 1 } }, { name: 'a', inputSchema: {} }, { name: 'a', inputSchema: { z: 1 } }, { name: 'Ä', inputSchema: {} }, { name: 'A', inputSchema: {} }],
  [{ name: 'A', inputSchema: {} }, { name: 'Ä', inputSchema: {} }, { name: 'a', inputSchema: { z: 1 } }, { name: 'a', inputSchema: {} }, { name: 'z', inputSchema: { a: 1, b: 2 } }],
].map(tools => ({ clockMs: baseClock, input: cacheInput({ tools }) })) });
cases.push({ name: 'cache-memory-user-surface', setup: { surface: 'user_message', memory: { context: 'cache memory' } }, steps: [{ clockMs: baseClock, input: cacheInput({ messages: [user(text('old')), assistant(text('reply')), user(text('new'))] }) }] });
assert.equal(cases.length, 58, 'case count');

// ---------------------------------------------------------------- execute
// JSON cannot carry NaN/Infinity/-0. Inputs use {$number: "..."} markers and
// are decoded before the runtime call; recorded inputs keep the marker so the
// Rust replay can rebuild the exact JS number.
function decodeNumbers(value: any): any {
  if (Array.isArray(value)) return value.map(decodeNumbers);
  if (value && typeof value === 'object') {
    if (Object.keys(value).length === 1 && typeof value.$number === 'string') {
      switch (value.$number) {
        case 'NaN': return NaN;
        case 'Infinity': return Infinity;
        case '-Infinity': return -Infinity;
        case '-0': return -0;
      }
    }
    return Object.fromEntries(
      Object.entries(value).map(([key, item]) => [key, decodeNumbers(item)]),
    );
  }
  return value;
}
function encodeNumbers(value: any): any {
  if (typeof value === 'number') {
    if (Object.is(value, -0)) return { $number: '-0' };
    if (Number.isNaN(value)) return { $number: 'NaN' };
    if (value === Infinity) return { $number: 'Infinity' };
    if (value === -Infinity) return { $number: '-Infinity' };
    return value;
  }
  if (Array.isArray(value)) return value.map(encodeNumbers);
  if (value && typeof value === 'object') {
    return Object.fromEntries(
      Object.entries(value).map(([key, item]) => [key, encodeNumbers(item)]),
    );
  }
  return value;
}
function substitute(value: any, root: string): any {
  if (typeof value === 'string') return value.split(fixture).join(root);
  if (Array.isArray(value)) return value.map((item) => substitute(item, root));
  if (value && typeof value === 'object') {
    return Object.fromEntries(
      Object.entries(value).map(([key, item]) => [key, substitute(item, root)]),
    );
  }
  return value;
}
function normalize(value: any, root: string): any {
  if (typeof value === 'string') return value.split(root).join(fixture);
  if (Array.isArray(value)) return value.map((item) => normalize(item, root));
  if (value && typeof value === 'object') {
    return Object.fromEntries(
      Object.entries(value).map(([key, item]) => [key, normalize(item, root)]),
    );
  }
  return value;
}
function resolver(extension: Extension | undefined) {
  return {
    listCommands: () => extension?.commands ?? [],
    listSkills: () => extension?.skills ?? [],
    listPromptContributions: () => extension?.promptContributions ?? [],
    listMcpInstructions: () => extension?.mcp ?? [],
  };
}
// `PromptTextProvider` is `string | ((context) => string | Promise<string>)`
// at the type level, but a plain JS caller can return anything at runtime.
function textProvider(
  spec: { text?: string; dynamic?: DynamicText },
  abortState: { controller?: AbortController; mode?: string },
): any {
  if (!spec.dynamic) return spec.text;
  const dynamic = spec.dynamic;
  return async (context: { abortSignal?: AbortSignal }) => {
    if (dynamic.kind === 'throw') throw new Error(dynamic.message);
    if (dynamic.kind === 'abort-when') {
      // `captureContext` forwards the request signal to providers; aborting the
      // controller here proves the post-provider check observes it.
      assert.equal(
        context.abortSignal,
        abortState.controller?.signal,
        'provider receives the request AbortSignal',
      );
      if (abortState.mode === dynamic.when) abortState.controller!.abort();
      return dynamic.value;
    }
    return dynamic.value;
  };
}

const recordedCases: any[] = [];
for (const spec of cases) {
  const setup = substitute(spec.setup ?? {}, output);
  let clockMs = baseClock;
  const abortState: { controller?: AbortController; mode?: string } = {};
  let promptContributions;
  if (setup.contributions) {
    promptContributions = new PromptContributionRegistry({ name: 'fixture' });
    if (setup.extension?.promptContributions) {
      registerExtensionPromptContributions(promptContributions, resolver(setup.extension));
    }
    for (const section of setup.contributions.sections ?? []) {
      promptContributions.registerSection({
        name: section.name,
        order: section.order,
        text: textProvider(section, abortState),
        complete: section.complete,
      });
    }
    for (const context of setup.contributions.runtimeContexts ?? []) {
      promptContributions.registerRuntimeContext({
        name: context.name,
        order: context.order,
        text: textProvider(context, abortState),
      });
    }
    for (const variable of setup.contributions.variables ?? []) {
      promptContributions.registerVariable(variable.name, async (context: any) => {
        if (variable.abort) {
          assert.equal(
            context.abortSignal,
            abortState.controller?.signal,
            'variable provider receives the request AbortSignal',
          );
          abortState.controller!.abort();
        }
        return variable.value;
      });
    }
  }
  const coordinator = new PromptCacheCoordinator();
  const memoryContractErrors: unknown[] = [];
  const memoryResolver = setup.memory ? {
    async retrieve(input: any) {
      try {
      assert.equal(input.projectRoot, setup.memory.expectProjectRoot ?? setup.projectRoot ?? (spec.name.startsWith('cache-') ? '/pdrs-cache-fixture' : join(output, 'project')));
      assert.equal(input.sessionId, 'session');
      if (setup.memory.expectQuery !== undefined) assert.equal(input.query, setup.memory.expectQuery);
      if (setup.memory.expectRecentCount !== undefined) assert.equal(input.recentMessages.length, setup.memory.expectRecentCount);
      assert.ok(input.signal instanceof AbortSignal);
      } catch (error) { memoryContractErrors.push(error); throw error; }
      if (setup.memory.abort && (abortState.mode === 'memory' || abortState.mode === 'memory-pending')) abortState.controller!.abort();
      if (abortState.mode === 'memory-pending') return await new Promise(() => {});
      if (setup.memory.pending) return await new Promise(() => {});
      if (setup.memory.error) throw new Error(setup.memory.error);
      return { systemContext: setup.memory.context, diagnostics: setup.memory.diagnostics ?? [] };
    },
    async captureTurn() {},
  } : undefined;
  let compactResult = 'skipped';
  const runtime = new DefaultContextRuntime({
    memoryResolver,
    projectRoot: setup.projectRoot,
    memoryRetrievalTimeoutMs: setup.memoryTimeoutMs,
    extension: resolver(setup.extension),
    promptContributions,
    includeExtensionsWithCustomSystemPrompt: setup.includeExtensions ?? false,
    runtimeContextSurface: setup.surface ?? 'system_prompt',
    instructionDiscovery: setup.discovery
      ? new InstructionDiscovery(
          setup.discovery.projectRoot,
          setup.discovery.cwd,
          setup.discovery.pilotHome,
        )
      : undefined,
    now: () => new Date(clockMs),
    promptCacheCoordinator: coordinator,
    compaction: { async autoCompact() { return { type: compactResult }; } } as any,
  });
  const recordedSteps: any[] = [];
  let previousOutput: any;
  for (const step of spec.steps) {
    clockMs = step.clockMs;
    if (setup.discovery?.managed) {
      process.env.PILOTDECK_MANAGED_CONFIG = setup.discovery.managed;
    } else {
      delete process.env.PILOTDECK_MANAGED_CONFIG;
    }
    const input = decodeNumbers(
      substitute(
        step.messagesFromPrevious
          ? { ...step.input, messages: previousOutput.messages }
          : step.input,
        output,
      ),
    );
    const normalizedInput = normalize(encodeNumbers(input), output);
    if (step.compact) { compactResult = step.compact; await runtime.tryAutoCompact({ sessionId: input.sessionId } as any); }
    if (step.cacheAction === 'dispose') runtime.dispose();
    else if (step.cacheAction) coordinator[step.cacheAction](input.sessionId);
    const controller = new AbortController();
    abortState.controller = step.abort ? controller : undefined;
    abortState.mode = step.abort;
    if (step.abort === 'before') controller.abort();
    const prepareInput = step.abort ? { ...input, abortSignal: controller.signal } : input;
    try {
      const result = await runtime.prepareForModel(prepareInput);
      recordedSteps.push({
        clockMs,
        ...(step.abort ? { abort: step.abort } : {}),
        ...(step.cacheAction ? { cacheAction: step.cacheAction } : {}),
        ...(step.compact ? { compact: step.compact } : {}),
        input: normalizedInput,
        output: JSON.stringify(normalize(result, output)),
      });
      previousOutput = result;
    } catch (error) {
      if (step.abort && step.abort !== 'memory' && step.abort !== 'memory-pending') {
        // Pin the real shape, not only the message: the default reason is a
        // DOMException with name=AbortError and legacy code=20.
        assert.ok(error instanceof Error);
        assert.equal(error.name, 'AbortError');
        assert.equal((error as any).code, 20);
        assert.equal(error.message, 'This operation was aborted');
      }
      recordedSteps.push({
        clockMs,
        ...(step.abort ? { abort: step.abort } : {}),
        ...(step.cacheAction ? { cacheAction: step.cacheAction } : {}),
        ...(step.compact ? { compact: step.compact } : {}),
        input: normalizedInput,
        error: error instanceof Error ? error.message : String(error),
      });
    }
  }
  assert.deepEqual(memoryContractErrors, [], `memory contract ${spec.name}`);
  recordedCases.push({
    name: spec.name,
    setup: normalize(setup, output),
    steps: recordedSteps,
  });
}

// The oracle records the real TS environment; the Rust replay injects it so
// prompt parity does not depend on the CI host OS or Node build.
const environment = { platform: process.platform, node: process.version };
const sortedFiles = Object.keys(files)
  .sort()
  .map((path) => ({
    path,
    base64: Buffer.from(files[path]).toString('base64'),
  }));
const oracle = {
  sourceCommit: BASELINE_COMMIT,
  sourceSha256,
  environment,
  directories: [...directories].sort(),
  files: sortedFiles,
  symlinks,
  cases: recordedCases,
};
assert.equal(bindSourceTree(baseline, BASELINE_COMMIT, gitExecutable).sha256, binding.sha256);
await writeFile(join(output, 'oracle.json'), `${JSON.stringify(oracle, null, 2)}\n`);
console.log(JSON.stringify({ output, cases: recordedCases.length }));
