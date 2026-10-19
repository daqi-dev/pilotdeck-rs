// Real fixed-baseline permission decisions; no matching or priority logic is
// reimplemented here. Run from the repo root (Node 22, baseline tsx loader):
//
// PDRS_BASELINE=/abs/PilotDeck node --import /abs/PilotDeck/node_modules/tsx/dist/loader.mjs \
//   src/pilotdeck-gateway/tests/fixtures/permission/generate.mts /abs/output-parent
//
// Each run creates its own mkdtemp child under the output parent. Scenarios
// use synthetic POSIX paths, so the oracle is portable and never touches disk.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { realpathSync } from 'node:fs';
import { lstat, mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, relative, resolve, sep } from 'node:path';
import { pathToFileURL } from 'node:url';
import { bindSourceTree } from '../../baseline_source_guard.mjs';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE');
assert.ok(process.argv[2], 'provide an output parent directory');
const baseline = resolve(process.env.PDRS_BASELINE);
const outputParent = resolve(process.argv[2]);
const commit = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const git = realpathSync(
  process.env.PDRS_GIT || execFileSync('which', ['git'], { encoding: 'utf8' }).trim(),
);
const gitEnv = Object.fromEntries(
  Object.entries(process.env).filter(([key]) => !key.startsWith('GIT_')),
);
const runGit = (...args: string[]) =>
  execFileSync(git, ['--no-replace-objects', '-C', baseline, ...args], {
    env: gitEnv,
    maxBuffer: 32 * 1024 * 1024,
  });
const assertTrackedClean = () => {
  const changes = runGit('status', '--porcelain', '--untracked-files=no').toString();
  assert.equal(changes, '', `baseline has uncommitted changes to tracked files:\n${changes}`);
};
const sourceCommit = bindSourceTree(baseline, commit, git).ref;
assertTrackedClean();

const sources = [
  'src/permission/decision/PermissionRuntime.ts',
  'src/permission/policy/matchPermissionRule.ts',
  'src/permission/protocol/types.ts',
];
// Hash the relative-import closure, not just the entry files (same rule as
// workspace-snapshot/async-hooks). Bare and node: specifiers are external.
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
      // try the next candidate
    }
  }
  return undefined;
}
const closure = new Set<string>(sources);
for (const queue = [...sources]; queue.length > 0; ) {
  const file = queue.shift()!;
  const text = await readFile(join(baseline, file), 'utf8');
  for (const [, , specifier] of text.matchAll(
    /(?:\bfrom\s*|\bimport\s*\(\s*|\bimport\s+)(['"])([^'"]+)\1/g,
  )) {
    if (!specifier.startsWith('./') && !specifier.startsWith('../')) continue;
    const resolved = await resolveSource(join(baseline, dirname(file), specifier));
    assert.ok(resolved, `unresolved baseline import ${specifier} in ${file}`);
    const path = relative(baseline, resolved).split(sep).join('/');
    if (!closure.has(path)) {
      closure.add(path);
      queue.push(path);
    }
  }
}
const sourceSha256: Record<string, string> = {};
for (const file of [...closure].sort()) {
  assert.ok(file.startsWith('src/'), `import closure escaped src/: ${file}`);
  const committed = runGit('show', `${commit}:${file}`);
  assert.deepEqual(await readFile(join(baseline, file)), committed, `${file} differs from ${commit}`);
  sourceSha256[file] = createHash('sha256').update(committed).digest('hex');
}

const load = (path: string) => import(pathToFileURL(join(baseline, path)).href);
const { isPermissionMode, emptyPermissionRuleSet, createDefaultPermissionContext } = await load(
  'src/permission/protocol/types.ts',
);
const { matchPermissionRule } = await load('src/permission/policy/matchPermissionRule.ts');
const { PermissionRuntime } = await load('src/permission/decision/PermissionRuntime.ts');
const locale = new Intl.Collator().resolvedOptions().locale;
assert.equal(locale, 'en-US', 'generate with LANG=en_US.UTF-8 (see README.md)');
// The cwd:"" scenarios resolve relative inputs against the real process cwd.
// Their recorded booleans are machine-independent as long as that cwd is not
// the filesystem root; assert it instead of silently recording "/" behavior.
assert.notEqual(resolve('.'), '/', 'cwd:"" scenarios require a non-root process cwd');

const CWD = '/ws/project';
const EXTRA = '/ws/extra';
const PLAN = '/ws/project/.pilotdeck/plans';
const clone = (value: any) => JSON.parse(JSON.stringify(value));
const rule = (
  source: string,
  behavior: string,
  toolName: string,
  pattern?: string,
  force?: boolean,
) => ({
  source,
  behavior,
  toolName,
  ...(pattern !== undefined ? { pattern } : {}),
  ...(force !== undefined ? { force } : {}),
});
const rules = (allow: any[] = [], deny: any[] = [], ask: any[] = []) => ({ allow, deny, ask });
const context = (extra: any = {}) => ({
  mode: 'default',
  rules: rules(),
  cwd: CWD,
  additionalWorkingDirectories: [],
  canPrompt: false,
  bypassAvailable: true,
  ...extra,
});

const modeValues = ['default', 'plan', 'bypassPermissions', 'acceptEdits', 'Plan', '', null, 5, true];
const modes = modeValues.map((value, index) => ({
  name: `mode-${index}`,
  value,
  expected: isPermissionMode(value),
}));

const defaultOptions = [
  { name: 'minimal', options: { cwd: CWD } },
  {
    name: 'full',
    options: {
      cwd: CWD,
      mode: 'plan',
      canPrompt: true,
      policyCanPrompt: false,
      bypassAvailable: true,
      acceptEdits: false,
      additionalWorkingDirectories: [EXTRA],
      planDirectoryPath: PLAN,
      rules: { allow: [rule('session', 'allow', 'read_file')], ask: [rule('policy', 'ask', 'bash')] },
    },
  },
  { name: 'omit-empty-plan-and-accept', options: { cwd: CWD, acceptEdits: undefined, planDirectoryPath: '' } },
  { name: 'policy-true-is-omitted', options: { cwd: CWD, policyCanPrompt: true } },
];
const defaults = defaultOptions.map(({ name, options }) => ({
  name,
  options,
  expected: clone(createDefaultPermissionContext(options as any)),
}));
assert.equal(defaults[0].expected.canPrompt, false);
assert.equal(defaults[1].expected.policyCanPrompt, false);

// ---- matchPermissionRule ------------------------------------------------
const matches: any[] = [];
function match(
  name: string,
  permissionRule: any,
  toolName: string,
  input: any,
  permissionContext?: any,
) {
  const expected = matchPermissionRule(permissionRule, toolName, input, permissionContext);
  matches.push({
    name,
    rule: clone(permissionRule),
    toolName,
    ...(input !== undefined ? { input } : {}),
    ...(permissionContext !== undefined ? { context: permissionContext } : {}),
    expected,
  });
}
match('exact', rule('user', 'allow', 'read_file'), 'read_file', { file_path: 'a' }, context());
match('exact-case-mismatch', rule('user', 'allow', 'read_file'), 'Read_file', { file_path: 'a' }, context());
match('wildcard-suffix', rule('user', 'allow', 'read_*'), 'read_file', { file_path: 'a' }, context());
match('wildcard-case-mismatch', rule('user', 'allow', 'Read_*'), 'read_file', { file_path: 'a' }, context());
match('wildcard-all', rule('user', 'allow', '*'), 'anything', undefined, context());
match('wildcard-stops-at-newline', rule('user', 'allow', '*'), 'a\nb', undefined, context());
match('bash-exact-trimmed', rule('user', 'allow', 'bash', 'git status'), 'bash', { command: '  git status  ' }, context());
match('bash-newline-boundary', rule('user', 'allow', 'bash', '*'), 'bash', { command: 'a\nb' }, context());
match('bash-colon-star', rule('user', 'allow', 'bash', 'git:*'), 'bash', { command: 'git status' }, context());
match('bash-non-string-command', rule('user', 'allow', 'bash', '*'), 'bash', { command: 5 }, context());
match('bash-missing-command', rule('user', 'allow', 'bash', '*'), 'bash', {}, context());
match('bash-exact-mismatch', rule('user', 'allow', 'bash', 'git status'), 'bash', { command: 'git push' }, context());
match('bash-multiline-exact', rule('user', 'allow', 'bash', 'a\nb'), 'bash', { command: 'a\nb' }, context());
match('file-absolute-pattern', rule('user', 'allow', 'read_file', `${CWD}/src/*.rs`), 'read_file', { file_path: 'src/a.rs' }, context());
match('file-star-does-not-cross-directory', rule('user', 'allow', 'read_file', '*.rs'), 'read_file', { file_path: 'src/a.rs' }, context());
match('file-relative-pattern', rule('user', 'allow', 'read_file', 'src/*'), 'read_file', { file_path: 'src/a.rs' }, context());
match('file-backslash-pattern', rule('user', 'allow', 'read_file', 'src\\*.rs'), 'read_file', { file_path: 'src/a.rs' }, context());
match('file-nul-path', rule('user', 'allow', 'read_file', '*'), 'read_file', { file_path: 'src/a\u0000b' }, context());
match('file-path-fallback', rule('user', 'allow', 'read_file', 'src/*'), 'read_file', { file_path: null, filePath: 'src/a.rs' }, context());
match('file-path-non-string-does-not-fall-back', rule('user', 'allow', 'read_file', 'src/*'), 'read_file', { file_path: 5, filePath: 'src/a.rs' }, context());
match('file-path-trimmed', rule('user', 'allow', 'read_file', 'src/*'), 'read_file', { file_path: '  src/a.rs  ' }, context());
match('write-no-pattern-inside', rule('user', 'allow', 'write_file'), 'write_file', { file_path: 'src/a.rs' }, context());
match('write-no-pattern-outside', rule('user', 'allow', 'write_file'), 'write_file', { file_path: '../a.rs' }, context());
match('write-no-pattern-additional-root', rule('user', 'allow', 'write_file'), 'write_file', { file_path: `${EXTRA}/a.rs` }, context({ additionalWorkingDirectories: [EXTRA] }));
match('write-no-pattern-lexical-dotdot', rule('user', 'allow', 'write_file'), 'write_file', { file_path: '../project/src/a.rs' }, context());
match('read-no-pattern-outside-ignores-workspace', rule('user', 'allow', 'read_file'), 'read_file', { file_path: '../a.rs' }, context());
match('write-pattern-takes-precedence', rule('user', 'allow', 'write_file', '/etc/*'), 'write_file', { file_path: '/etc/passwd' }, context());
match('unknown-tool-pattern-ignored', rule('user', 'allow', 'web_fetch', 'never-matches'), 'web_fetch', { url: 'x' }, context());
match('send-attachment-path-pattern', rule('user', 'allow', 'send_attachment', 'src/*'), 'send_attachment', { file_path: 'src/a.rs' }, context());
match('rule-wildcard-with-bash-pattern', rule('user', 'allow', '*', 'git*'), 'bash', { command: 'git log' }, context());
match('empty-pattern-write-inside', rule('user', 'allow', 'write_file', ''), 'write_file', { file_path: 'src/a.rs' }, context());
match('empty-pattern-write-outside', rule('user', 'allow', 'write_file', ''), 'write_file', { file_path: '../a.rs' }, context());
match('file-pattern-case-sensitive', rule('user', 'allow', 'read_file', 'SRC/*'), 'read_file', { file_path: 'src/a.rs' }, context());
match('unicode-lone-surrogate-path', rule('user', 'allow', 'read_file', 'src/\ud800/*'), 'read_file', { file_path: 'src/\ud800/a' }, context());
match('multiple-wildcards', rule('user', 'allow', 'read_file', 'src/*/a*'), 'read_file', { file_path: 'src/x/abc' }, context());
match('regex-metacharacters-literal', rule('user', 'allow', 'bash', 'a+b?'), 'bash', { command: 'a+b?' }, context());
match('regex-metacharacters-do-not-match', rule('user', 'allow', 'bash', 'a+b?'), 'bash', { command: 'aab' }, context());
match('write-pattern-outside-workspace-matches', rule('user', 'allow', 'write_file', `${EXTRA}/*`), 'write_file', { file_path: `${EXTRA}/a.rs` }, context({ additionalWorkingDirectories: [EXTRA] }));
match('edit-file-path-pattern', rule('user', 'allow', 'edit_file', `${CWD}/src/*`), 'edit_file', { file_path: 'src/a.rs' }, context());
match('write-no-pattern-camel-filePath-inside', rule('user', 'allow', 'write_file'), 'write_file', { filePath: 'src/a.rs' }, context());
match('write-no-pattern-camel-filePath-outside', rule('user', 'allow', 'write_file'), 'write_file', { filePath: '../a.rs' }, context());
// cwd "" is an empty path segment: TS `path.join("", "secret.env")` is
// "secret.env", then `path.resolve` makes it `<process cwd>/secret.env`.
match('file-cwd-empty-relative', rule('user', 'allow', 'read_file', '/*/secret.env'), 'read_file', { file_path: 'secret.env' }, context({ cwd: '' }));
match('file-cwd-empty-not-root', rule('user', 'allow', 'read_file', '/secret.env'), 'read_file', { file_path: 'secret.env' }, context({ cwd: '' }));
match('file-cwd-dot-relative', rule('user', 'allow', 'read_file', '/*/secret.env'), 'read_file', { file_path: 'secret.env' }, context({ cwd: '.' }));
match('file-cwd-relative-relative', rule('user', 'allow', 'read_file', '/*/secret.env'), 'read_file', { file_path: 'secret.env' }, context({ cwd: 'rel/path' }));
match('write-no-pattern-cwd-empty-inside', rule('user', 'allow', 'write_file'), 'write_file', { file_path: 'src/a.rs' }, context({ cwd: '' }));
match('write-no-pattern-cwd-empty-outside', rule('user', 'allow', 'write_file'), 'write_file', { file_path: '../a.rs' }, context({ cwd: '' }));

assert.equal(matches.find((m) => m.name === 'wildcard-case-mismatch')!.expected, false);
assert.equal(matches.find((m) => m.name === 'bash-newline-boundary')!.expected, false);
assert.equal(matches.find((m) => m.name === 'write-no-pattern-outside')!.expected, false);
assert.equal(matches.find((m) => m.name === 'file-pattern-case-sensitive')!.expected, false);
for (const name of ['file-cwd-empty-relative', 'file-cwd-dot-relative', 'file-cwd-relative-relative']) {
  assert.equal(matches.find((m) => m.name === name)!.expected, true, name);
}
// Negative control: treating cwd "" as "/" would turn this into a match.
assert.equal(matches.find((m) => m.name === 'file-cwd-empty-not-root')!.expected, false);
assert.equal(matches.find((m) => m.name === 'write-no-pattern-cwd-empty-inside')!.expected, true);
assert.equal(matches.find((m) => m.name === 'write-no-pattern-cwd-empty-outside')!.expected, false);

// ---- PermissionRuntime.decide ------------------------------------------
const decisions: any[] = [];
function decide(name: string, permissionContext: any, toolSpec: any, input: any, toolCallId = 'call-1') {
  const calls = { count: 0 };
  const tool: any = {
    name: toolSpec.name,
    isReadOnly: () => toolSpec.readOnly ?? false,
    checkPermissions: async () => {
      const index = calls.count++;
      return toolSpec.checks?.[index];
    },
  };
  if (toolSpec.kind !== undefined) tool.kind = toolSpec.kind;
  const runtime = new PermissionRuntime();
  const decision = runtime.decide(
    tool,
    input,
    { permissionContext } as any,
    toolCallId,
  );
  return decision.then((value: any) => {
    decisions.push({
      name,
      context: permissionContext,
      tool: {
        name: toolSpec.name,
        ...(toolSpec.kind !== undefined ? { kind: toolSpec.kind } : {}),
        readOnly: toolSpec.readOnly ?? false,
        checks: toolSpec.checks ?? [],
      },
      ...(input !== undefined ? { input } : {}),
      toolCallId,
      expected: { decision: clone(value), calls: calls.count },
    });
  });
}

const askReason = { type: 'mode', mode: 'default', message: 'scripted ask' };
const toolAsk = { type: 'ask', reason: askReason, request: { toolCallId: 'old', toolName: 'old', inputSummary: 'old', reason: askReason, options: [] } };

const plan = context({ mode: 'plan', canPrompt: true });
const bypass = context({ mode: 'bypassPermissions', canPrompt: true });
const prompts = context({ canPrompt: true });
const noPrompts = context({ canPrompt: false });

const pending = [
  decide('default-readonly-allowed', prompts, { name: 'read_file', readOnly: true }, { file_path: 'src/a.rs' }),
  decide('default-write-asks', prompts, { name: 'write_file', readOnly: false }, { file_path: 'src/a.rs' }),
  decide('default-write-prompts-disabled', noPrompts, { name: 'write_file', readOnly: false }, { file_path: 'src/a.rs' }),
  decide('plan-readonly-allowed', plan, { name: 'read_file', readOnly: true }, { file_path: 'src/a.rs' }),
  decide('plan-bash-denied-message', plan, { name: 'bash', readOnly: false }, { command: `${'x'.repeat(119)}😀tail` }),
  decide('plan-write-denied-message', plan, { name: 'write_file', readOnly: false }, { file_path: 'src/a.rs' }),
  decide('plan-directory-markdown-allowed', context({ mode: 'plan', planDirectoryPath: PLAN }), { name: 'write_file', readOnly: false, kind: 'filesystem' }, { file_path: '.pilotdeck/plans/x.md' }),
  decide('plan-directory-uppercase-markdown-allowed', context({ mode: 'plan', planDirectoryPath: PLAN }), { name: 'write_file', readOnly: false, kind: 'filesystem' }, { file_path: '.pilotdeck/plans/X.MD' }),
  decide('plan-directory-outside-denied', context({ mode: 'plan', planDirectoryPath: PLAN }), { name: 'write_file', readOnly: false, kind: 'filesystem' }, { file_path: 'other/x.md' }),
  decide('plan-user-allow-plan-directory', context({ mode: 'plan', planDirectoryPath: PLAN, rules: rules([rule('user', 'allow', 'write_file')]) }), { name: 'write_file', readOnly: false, kind: 'filesystem' }, { file_path: '.pilotdeck/plans/x.md' }),
  decide('plan-user-allow-non-plan-directory', context({ mode: 'plan', planDirectoryPath: PLAN, rules: rules([rule('user', 'allow', 'write_file')]) }), { name: 'write_file', readOnly: false, kind: 'filesystem' }, { file_path: 'src/a.md' }),
  decide('plan-user-allow-readonly', context({ mode: 'plan', rules: rules([rule('user', 'allow', 'read_file')]) }), { name: 'read_file', readOnly: true }, { file_path: 'src/a.rs' }),
  decide('plan-tool-ask-readonly-allowed', context({ mode: 'plan', canPrompt: true }), { name: 'read_file', readOnly: true, checks: [toolAsk] }, { file_path: 'src/a.rs' }),
  decide('plan-tool-ask-write-denied', context({ mode: 'plan', canPrompt: true }), { name: 'write_file', readOnly: false, checks: [toolAsk] }, { file_path: 'src/a.rs' }),
  decide('plan-tool-deny-wins', plan, { name: 'write_file', readOnly: false, checks: [{ type: 'deny', reason: { type: 'safety', message: 'blocked' }, message: 'blocked by tool' }] }, { file_path: 'src/a.rs' }),
  decide('bypass-allows-unhandled', bypass, { name: 'write_file', readOnly: false }, { file_path: 'src/a.rs' }),
  decide('bypass-overrides-tool-ask', bypass, { name: 'bash', readOnly: false, checks: [toolAsk] }, { command: 'rm -rf x' }),
  decide('bypass-skips-ask-rule-prompt', context({ mode: 'bypassPermissions', canPrompt: true, rules: rules([], [], [rule('user', 'ask', 'bash')]) }), { name: 'bash', readOnly: false }, { command: 'rm -rf x' }),
  decide('bypass-keeps-policy-ask', context({ mode: 'bypassPermissions', canPrompt: true, rules: rules([], [], [rule('policy', 'ask', 'bash')]) }), { name: 'bash', readOnly: false }, { command: 'rm -rf x' }),
  decide('policy-ask-prompts-disabled', context({ canPrompt: false, rules: rules([], [], [rule('policy', 'ask', 'bash')]) }), { name: 'bash', readOnly: false }, { command: 'x' }),
  decide('policy-can-prompt-false-denies', context({ canPrompt: true, policyCanPrompt: false, rules: rules([], [], [rule('session', 'ask', 'bash')]) }), { name: 'bash', readOnly: false }, { command: 'x' }),
  decide('session-allow-beats-user-deny', context({ rules: rules([rule('session', 'allow', 'write_file')], [rule('user', 'deny', 'write_file')]) }), { name: 'write_file', readOnly: false }, { file_path: 'src/a.rs' }),
  decide('project-deny-beats-session-allow', context({ rules: rules([rule('session', 'allow', 'write_file')], [rule('project', 'deny', 'write_file')]) }), { name: 'write_file', readOnly: false }, { file_path: 'src/a.rs' }),
  decide('user-deny-beats-user-allow', context({ rules: rules([rule('user', 'allow', 'write_file')], [rule('user', 'deny', 'write_file')]) }), { name: 'write_file', readOnly: false }, { file_path: 'src/a.rs' }),
  decide('ask-rule-before-session-allow', context({ canPrompt: true, rules: rules([rule('session', 'allow', 'bash')], [], [rule('user', 'ask', 'bash')]) }), { name: 'bash', readOnly: false }, { command: 'x' }),
  decide('policy-ask-before-session-allow', context({ canPrompt: true, rules: rules([rule('session', 'allow', 'bash')], [], [rule('policy', 'ask', 'bash')]) }), { name: 'bash', readOnly: false }, { command: 'x' }),
  decide('session-mcp-force-ask-survives-bypass', context({ mode: 'bypassPermissions', canPrompt: true, rules: rules([], [], [rule('session', 'ask', 'mcp__server', undefined, true)]) }), { name: 'mcp__server', readOnly: false }, {}),
  decide('non-session-force-ask-bypassed', context({ mode: 'bypassPermissions', canPrompt: true, rules: rules([], [], [rule('user', 'ask', 'mcp__server', undefined, true)]) }), { name: 'mcp__server', readOnly: false }, {}),
  decide('session-force-non-mcp-bypassed', context({ mode: 'bypassPermissions', canPrompt: true, rules: rules([], [], [rule('session', 'ask', 'bash', undefined, true)]) }), { name: 'bash', readOnly: false }, { command: 'x' }),
  decide('allow-rule-write-inside', context({ rules: rules([rule('user', 'allow', 'write_file', `${CWD}/src/*`)]) }), { name: 'write_file', readOnly: false }, { file_path: 'src/a.rs' }),
  decide('allow-rule-write-outside-no-match', prompts, { name: 'write_file', readOnly: false, checks: [] }, { file_path: '../a.rs' }),
  decide('deny-wildcard-case-sensitive', context({ rules: rules([], [rule('user', 'deny', 'Read*')]) }), { name: 'read_file', readOnly: true }, { file_path: 'src/a.rs' }),
  decide('deny-wildcard-matches', context({ rules: rules([], [rule('user', 'deny', 'read_*')]) }), { name: 'read_file', readOnly: true }, { file_path: 'src/a.rs' }),
  decide('accept-edits-inside-workspace', context({ acceptEdits: true }), { name: 'write_file', readOnly: false }, { file_path: 'src/a.rs' }),
  decide('accept-edits-additional-root', context({ acceptEdits: true, additionalWorkingDirectories: [EXTRA] }), { name: 'edit_file', readOnly: false }, { file_path: `${EXTRA}/a.rs` }),
  decide('accept-edits-outside-workspace', context({ acceptEdits: true }), { name: 'write_file', readOnly: false }, { file_path: '../a.rs' }),
  decide('accept-edits-write-denied-directory', context({ acceptEdits: true }), { name: 'write_file', readOnly: false }, { file_path: '.git/config' }),
  decide('accept-edits-bypass-outside-workspace', context({ mode: 'bypassPermissions', acceptEdits: true }), { name: 'write_file', readOnly: false }, { file_path: '/tmp/a' }),
  decide('accept-edits-read-tool-not-edits', context({ acceptEdits: true }), { name: 'read_file', readOnly: true }, { file_path: 'src/a.rs' }),
  decide('accept-edits-notebook', context({ acceptEdits: true }), { name: 'edit_notebook', readOnly: false }, { notebook_path: 'src/n.ipynb' }),
  decide('tool-allow-with-updated-input', prompts, { name: 'write_file', readOnly: false, checks: [{ type: 'allow', reason: { type: 'tool', toolName: 'write_file', message: 'ok' }, updatedInput: { file_path: 'src/b.rs' } }] }, { file_path: 'src/a.rs' }),
  decide('tool-cancel-passthrough', prompts, { name: 'write_file', readOnly: false, checks: [{ type: 'cancel', reason: { type: 'runtime', message: 'cancelled' }, message: 'cancelled' }] }, { file_path: 'src/a.rs' }),
  decide('tool-passthrough-falls-through', prompts, { name: 'write_file', readOnly: false, checks: [{ type: 'passthrough' }] }, { file_path: 'src/a.rs' }),
  decide('tool-unknown-type-asks-runtime', prompts, { name: 'write_file', readOnly: false, checks: [{ type: 'weird' }] }, { file_path: 'src/a.rs' }),
  decide('tool-ask-normalized-request', prompts, { name: 'write_file', readOnly: false, checks: [toolAsk] }, { file_path: 'src/a.rs' }),
  decide('tool-ask-prompts-disabled', noPrompts, { name: 'write_file', readOnly: false, checks: [toolAsk] }, { file_path: 'src/a.rs' }),
  decide('session-allow-tool-deny-wins', context({ rules: rules([rule('session', 'allow', 'write_file')]) }), { name: 'write_file', readOnly: false, checks: [{ type: 'deny', reason: { type: 'safety', message: 'no' }, message: 'no' }] }, { file_path: 'src/a.rs' }),
  decide('session-allow-tool-ask-ignored', context({ canPrompt: true, rules: rules([rule('session', 'allow', 'write_file')]) }), { name: 'write_file', readOnly: false, checks: [toolAsk] }, { file_path: 'src/a.rs' }),
  decide('summary-undefined-input', prompts, { name: 'write_file', readOnly: false }, undefined),
  decide('summary-object-input', prompts, { name: 'write_file', readOnly: false }, { b: 1, a: '中文', list: [1, null, 'x'] }),
  decide('summary-long-surrogate', prompts, { name: 'write_file', readOnly: false }, '😀'.repeat(300)),
  decide('summary-array-input', prompts, { name: 'write_file', readOnly: false }, [1, 2, 3]),
  decide('policy-allow-rule', context({ rules: rules([rule('policy', 'allow', 'write_file')]) }), { name: 'write_file', readOnly: false }, { file_path: 'src/a.rs' }),
  decide('deny-beats-tool-allow', context({ rules: rules([], [rule('user', 'deny', 'write_file')]) }), { name: 'write_file', readOnly: false, checks: [{ type: 'allow', reason: { type: 'tool', message: 'ok' } }] }, { file_path: 'src/a.rs' }),
  decide('deny-before-ask', context({ rules: rules([], [rule('user', 'deny', 'bash')], [rule('user', 'ask', 'bash')]) }), { name: 'bash', readOnly: false }, { command: 'x' }),
  decide('deny-rule-before-bypass', context({ mode: 'bypassPermissions', rules: rules([], [rule('project', 'deny', 'bash')]) }), { name: 'bash', readOnly: false }, { command: 'x' }),
  // cwd "" / relative cwd: empty segments are skipped, then the path is
  // resolved against the real process cwd. These pin the deny, plan-directory
  // and acceptEdits paths that all share resolveInput.
  decide('deny-rule-cwd-empty', context({ cwd: '', rules: rules([], [rule('user', 'deny', 'read_file', '/*/secret.env')]) }), { name: 'read_file', readOnly: true }, { file_path: 'secret.env' }),
  decide('deny-root-pattern-cwd-empty-no-match', context({ cwd: '', rules: rules([], [rule('user', 'deny', 'read_file', '/secret.env')]) }), { name: 'read_file', readOnly: true }, { file_path: 'secret.env' }),
  decide('plan-directory-cwd-empty-relative', context({ cwd: '', mode: 'plan', planDirectoryPath: '.' }), { name: 'write_file', readOnly: false, kind: 'filesystem' }, { file_path: 'x.md' }),
  decide('accept-edits-cwd-empty', context({ cwd: '', acceptEdits: true }), { name: 'write_file', readOnly: false }, { file_path: 'x' }),
  decide('first-matching-allow-rule-wins', context({ rules: rules([rule('user', 'allow', 'read_file', '/*/secret.env'), rule('user', 'allow', 'read_file', '*')]) }), { name: 'read_file', readOnly: true }, { file_path: 'secret.env' }),
  decide('invalid-mode-acceptedits', context({ mode: 'acceptEdits', canPrompt: true }), { name: 'write_file', readOnly: false }, { file_path: 'src/a.rs' }),
  decide('invalid-mode-dontask', context({ mode: 'dontAsk', canPrompt: true }), { name: 'write_file', readOnly: false }, { file_path: 'src/a.rs' }),
  decide('invalid-mode-empty', context({ mode: '', canPrompt: true }), { name: 'write_file', readOnly: false }, { file_path: 'src/a.rs' }),
  decide('invalid-mode-plan-case', context({ mode: 'Plan', canPrompt: true }), { name: 'write_file', readOnly: false }, { file_path: 'src/a.rs' }),
  decide('session-mcp-force-ask-survives-no-prompt', context({ canPrompt: false, rules: rules([], [], [rule('session', 'ask', 'mcp__server', undefined, true)]) }), { name: 'mcp__server', readOnly: false }, {}),
  decide('accept-edits-additional-root-write-denied', context({ acceptEdits: true, canPrompt: true, additionalWorkingDirectories: [EXTRA] }), { name: 'write_file', readOnly: false, kind: 'filesystem' }, { file_path: `${EXTRA}/.git/x` }),
];
await Promise.all(pending);
decisions.sort((left, right) => (left.name < right.name ? -1 : left.name > right.name ? 1 : 0));

assert.equal(decisions.find((s) => s.name === 'session-allow-beats-user-deny')!.expected.decision.type, 'allow');
assert.equal(decisions.find((s) => s.name === 'project-deny-beats-session-allow')!.expected.decision.type, 'deny');
assert.equal(decisions.find((s) => s.name === 'ask-rule-before-session-allow')!.expected.decision.type, 'ask');
assert.equal(decisions.find((s) => s.name === 'session-mcp-force-ask-survives-bypass')!.expected.decision.type, 'ask');
assert.equal(decisions.find((s) => s.name === 'bypass-overrides-tool-ask')!.expected.decision.type, 'allow');
assert.equal(decisions.find((s) => s.name === 'accept-edits-inside-workspace')!.expected.decision.reason.message, 'SDK acceptEdits allows workspace file tool write_file.');
assert.equal(decisions.find((s) => s.name === 'summary-long-surrogate')!.expected.decision.request.inputSummary.length, 503);
assert.equal(decisions.find((s) => s.name === 'summary-undefined-input')!.expected.decision.request.inputSummary, 'undefined');
assert.equal(decisions.find((s) => s.name === 'deny-rule-cwd-empty')!.expected.decision.type, 'deny');
assert.equal(decisions.find((s) => s.name === 'deny-root-pattern-cwd-empty-no-match')!.expected.decision.type, 'allow');
assert.equal(decisions.find((s) => s.name === 'accept-edits-cwd-empty')!.expected.decision.reason.message, 'SDK acceptEdits allows workspace file tool write_file.');
assert.equal(decisions.find((s) => s.name === 'plan-directory-cwd-empty-relative')!.expected.decision.reason.message, 'Plan mode allows writing markdown plans under the plan directory.');
assert.equal(decisions.find((s) => s.name === 'first-matching-allow-rule-wins')!.expected.decision.reason.rule.pattern, '/*/secret.env');
assert.equal(decisions.find((s) => s.name === 'invalid-mode-acceptedits')!.expected.decision.type, 'ask');
assert.equal(decisions.find((s) => s.name === 'invalid-mode-plan-case')!.expected.decision.reason.mode, 'Plan');
assert.equal(decisions.find((s) => s.name === 'session-mcp-force-ask-survives-no-prompt')!.expected.decision.type, 'ask');
assert.equal(decisions.find((s) => s.name === 'accept-edits-additional-root-write-denied')!.expected.decision.type, 'ask');

const oracle = {
  sourceCommit,
  sourceSha256,
  locale,
  counts: { matches: matches.length, decisions: decisions.length, modes: modes.length, defaults: defaults.length },
  modes,
  defaults,
  matches,
  decisions,
};
const text = `${JSON.stringify(oracle, null, 2)}\n`;
for (const forbidden of [baseline, outputParent, realpathSync(tmpdir())]) {
  assert.ok(!text.includes(forbidden), `oracle leaked a machine path: ${forbidden}`);
}
assertTrackedClean();

const output = await mkdtemp(join(outputParent, 'pdrs-permission-'));
const owner = await lstat(output);
assert.ok(owner.isDirectory());
await writeFile(join(output, 'oracle.json'), text, { flag: 'wx' });
console.log(join(output, 'oracle.json'));
