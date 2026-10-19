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
type DynamicText =
  | { kind: 'string'; value: string }
  /** Runtime non-string despite the TS `string` type; `resolveText` rejects it. */
  | { kind: 'non-string'; value: unknown }
  | { kind: 'throw'; message: string };
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
  variables?: { name: string; value: string }[];
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
};
type Case = {
  name: string;
  setup?: {
    surface?: string;
    includeExtensions?: boolean;
    extension?: Extension;
    discovery?: Discovery;
    contributions?: Contributions;
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
assert.equal(cases.length, 37, 'case count');

// ---------------------------------------------------------------- execute
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
function textProvider(spec: { text?: string; dynamic?: DynamicText }): any {
  if (!spec.dynamic) return spec.text;
  const dynamic = spec.dynamic;
  return async () => {
    if (dynamic.kind === 'throw') throw new Error(dynamic.message);
    return dynamic.value;
  };
}

const recordedCases: any[] = [];
for (const spec of cases) {
  const setup = substitute(spec.setup ?? {}, output);
  let clockMs = baseClock;
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
        text: textProvider(section),
        complete: section.complete,
      });
    }
    for (const context of setup.contributions.runtimeContexts ?? []) {
      promptContributions.registerRuntimeContext({
        name: context.name,
        order: context.order,
        text: textProvider(context),
      });
    }
    for (const variable of setup.contributions.variables ?? []) {
      promptContributions.registerVariable(variable.name, async () => variable.value);
    }
  }
  const runtime = new DefaultContextRuntime({
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
    promptCacheCoordinator: {
      createPlan: () => undefined,
      reset() {},
      release() {},
    },
    compaction: {},
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
    const input = substitute(
      step.messagesFromPrevious
        ? { ...step.input, messages: previousOutput.messages }
        : step.input,
      output,
    );
    const normalizedInput = normalize(input, output);
    try {
      const result = await runtime.prepareForModel(input);
      recordedSteps.push({
        clockMs,
        input: normalizedInput,
        output: JSON.stringify(normalize(result, output)),
      });
      previousOutput = result;
    } catch (error) {
      recordedSteps.push({
        clockMs,
        input: normalizedInput,
        error: error instanceof Error ? error.message : String(error),
      });
    }
  }
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
