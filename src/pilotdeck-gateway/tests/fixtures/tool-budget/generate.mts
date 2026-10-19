// Real-TS oracle for ToolResultBudget + DefaultContextRuntime.applyToolResults.
// Run from the repo root with the fixed baseline's tsx loader; see README.md.
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import {
  chmod,
  lstat,
  mkdir,
  mkdtemp,
  readFile,
  readdir,
  readlink,
  rm,
  writeFile,
} from 'node:fs/promises';
import { realpathSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, isAbsolute, join, relative, resolve, sep } from 'node:path';
import { pathToFileURL } from 'node:url';
import assert from 'node:assert/strict';
import { bindSourceTree, sourceClosureSha256 } from '../../baseline_source_guard.mjs';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE to the fixed TS baseline');
assert.ok(process.argv[2], 'provide the fixture directory');
const BASELINE_COMMIT = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
const gitExecutable = realpathSync(
  process.env.PDRS_GIT || execFileSync('which', ['git'], { encoding: 'utf8' }).trim(),
);
const bound = bindSourceTree(baseline, BASELINE_COMMIT, gitExecutable);
const sourceCommit = bound.ref;
const sources = [
  'src/context/budget/ToolResultBudget.ts',
  'src/context/DefaultContextRuntime.ts',
];
const sourceSha256 = sourceClosureSha256(baseline, sourceCommit, gitExecutable, sources);
const locale = new Intl.Collator().resolvedOptions().locale;
assert.equal(locale, 'en-US', 'generate with LANG=en_US.UTF-8 (see README.md)');
assert.notEqual(
  typeof process.geteuid === 'function' ? process.geteuid() : -1,
  0,
  'the EACCES fixture must be generated as a non-root user',
);

const { ToolResultBudget } = await import(
  pathToFileURL(join(baseline, sources[0])).href
);
const { DefaultContextRuntime } = await import(
  pathToFileURL(join(baseline, sources[1])).href
);
const { countTokens } = await import(
  pathToFileURL(join(baseline, 'src/context/budget/tokenizer.ts')).href
);

const scratch = await mkdtemp(join(tmpdir(), 'pdrs-tool-budget-'));
const sha256 = (bytes) => createHash('sha256').update(bytes).digest('hex');

function text(value) {
  return { type: 'text', text: value };
}
function user(content) {
  return { role: 'user', content };
}
function toolResult(toolCallId, content, extra = {}) {
  return { type: 'tool_result', toolCallId, ...extra, content };
}
function image(data, mimeType, extra = {}) {
  return { type: 'image', source: 'base64', data, mimeType, ...extra };
}
function pdf(data, mimeType, extra = {}) {
  return { type: 'pdf', data, mimeType, ...extra };
}
function audio(data, mimeType, extra = {}) {
  return { type: 'audio', source: 'base64', data, mimeType, ...extra };
}
function normalize(value, root) {
  if (typeof value === 'string') return value.split(root).join('<fixture>');
  if (Array.isArray(value)) return value.map((item) => normalize(item, root));
  if (value && typeof value === 'object') {
    return Object.fromEntries(
      Object.entries(value).map(([key, item]) => [key, normalize(item, root)]),
    );
  }
  return value;
}

async function setup(caseRoot, spec) {
  if (spec.blockingFile) {
    await writeFile(join(caseRoot, spec.blockingFile), 'not a directory\n');
  }
  for (const file of spec.files ?? []) {
    await mkdir(dirname(join(caseRoot, file.path)), { recursive: true });
    await writeFile(join(caseRoot, file.path), file.text);
  }
  if (spec.deniedDir) {
    await mkdir(join(caseRoot, spec.deniedDir), { recursive: true });
    await chmod(join(caseRoot, spec.deniedDir), 0o555);
  }
}

async function release(caseRoot, spec) {
  if (spec.deniedDir) await chmod(join(caseRoot, spec.deniedDir), 0o755);
}

async function snapshot(root) {
  const entries = [];
  async function walk(rel) {
    const names = (await readdir(join(root, rel), { withFileTypes: true }))
      .map((entry) => entry.name)
      .sort();
    for (const name of names) {
      const path = rel ? `${rel}/${name}` : name;
      const absolute = join(root, path);
      const stat = await lstat(absolute);
      if (stat.isSymbolicLink()) {
        entries.push({ path, kind: 'symlink', target: await readlink(absolute) });
      } else if (stat.isDirectory()) {
        entries.push({ path, kind: 'dir' });
        await walk(path);
      } else {
        const bytes = await readFile(absolute);
        const item = { path, kind: 'file', size: bytes.length, sha256: sha256(bytes) };
        const roundTrip = Buffer.from(bytes.toString('utf8'), 'utf8');
        if (roundTrip.equals(bytes) && bytes.length <= 4096) {
          item.text = bytes.toString('utf8');
        }
        entries.push(item);
      }
    }
  }
  await walk('');
  return entries;
}

const cases = [];
let stepCount = 0;

// Each case owns a fresh ToolResultBudget, exactly like a fresh session bundle.
async function runCase(spec) {
  const caseRoot = join(scratch, spec.name);
  const toolResultsDir = join(
    caseRoot,
    spec.toolResultsDir ?? (spec.layout === 'plain' ? 'spill' : 'tool-results/session'),
  );
  if (spec.blockingFile) {
    await mkdir(caseRoot, { recursive: true });
  } else {
    await mkdir(dirname(toolResultsDir), { recursive: true });
  }
  await setup(caseRoot, spec);
  const budget = new ToolResultBudget({
    toolResultsDir,
    ...(spec.budget ?? {}),
  });
  const runtime = new DefaultContextRuntime({
    toolResultBudget: budget,
    compaction: {},
  });
  const steps = [];
  try {
    for (const step of spec.steps) {
      const input = {
        sessionId: 'fixture-session',
        turnId: step.turnId ?? 'turn-1',
        messages: [{ role: 'assistant', content: [text('prior message')] }],
        ...step.input,
      };
      const result = await runtime.applyToolResults(input);
      if (step.expectFirstBlockType) {
        assert.equal(
          result.appendedMessages[0].content[0].type,
          step.expectFirstBlockType,
          `sampled boundary misplaced for ${spec.name}`,
        );
      }
      steps.push({
        input: normalize(input, scratch),
        outputJson: JSON.stringify(normalize(result, scratch)),
        filesJson: JSON.stringify(normalize(await snapshot(caseRoot), scratch)),
      });
      stepCount += 1;
    }
  } finally {
    await release(caseRoot, spec);
  }
  cases.push({
    name: spec.name,
    toolResultsDir: normalize(toolResultsDir, scratch),
    budget: spec.budget ?? {},
    ...(spec.layout ? { layout: spec.layout } : {}),
    ...(spec.toolResultsDir ? { toolResultsDirRelative: spec.toolResultsDir } : {}),
    ...(spec.blockingFile ? { blockingFile: spec.blockingFile } : {}),
    ...(spec.files ? { files: spec.files } : {}),
    ...(spec.deniedDir ? { deniedDir: spec.deniedDir } : {}),
    ...(spec.requiresUnprivileged ? { requiresUnprivileged: true } : {}),
    steps,
  });
}

const longText = 'hello world '.repeat(20); // 240 UTF-8 bytes
const tokenText = 'The quick brown fox jumps over the lazy dog. '.repeat(20);
const tokenCount = countTokens(tokenText);

// Mirror of ToolResultBudget.ts:432-458 so the fixture can place the exact
// estimate and estimate-1 on both sides of the inline/spill boundary. The
// real TS run below decides the actual output; `expectFirstBlockType` fails
// generation if this replica ever drifts.
function sampleTextForTokenEstimate(text) {
  const budget = 6_000;
  if (text.length <= budget * 3) return [text];
  const middleStart = Math.max(0, Math.floor((text.length - budget) / 2));
  return [
    text.slice(0, budget),
    text.slice(middleStart, middleStart + budget),
    text.slice(-budget),
  ];
}
function estimateTokens(text, byteLength) {
  if (byteLength <= 40_000) return countTokens(text);
  let sampledBytes = 0;
  let sampledTokens = 0;
  for (const sample of sampleTextForTokenEstimate(text)) {
    sampledBytes += Buffer.byteLength(sample, 'utf8');
    sampledTokens += countTokens(sample);
  }
  if (sampledBytes === 0) return 0;
  return Math.ceil((sampledTokens / sampledBytes) * byteLength);
}

// 'ab' + '中😀' places a surrogate pair across the 6,000-unit sample cut.
const sampledText = 'ab' + '中😀'.repeat(7_314); // 51,200 bytes / 21,944 units
const sampledTextLarge = 'ab' + '中😀'.repeat(21_942); // 153,596 bytes / 65,828 units
const sampledEstimate = estimateTokens(sampledText, Buffer.byteLength(sampledText, 'utf8'));
const sampledEstimateLarge = estimateTokens(
  sampledTextLarge,
  Buffer.byteLength(sampledTextLarge, 'utf8'),
);
assert.ok(Buffer.byteLength(sampledText, 'utf8') > 40_000);
assert.ok(Buffer.byteLength(sampledTextLarge, 'utf8') > 40_000);
assert.ok(sampledEstimate > 0 && sampledEstimateLarge > 0);

await runCase({
  name: 'within-budget',
  budget: { maxResultSizeChars: 200, maxResultSizeTokens: 10_000, previewBytes: 12_000 },
  steps: [
    {
      input: {
        toolResultMessage: user([toolResult('call-small', [text('short tool result')])]),
      },
    },
  ],
});

await runCase({
  name: 'exact-char-threshold',
  budget: { maxResultSizeChars: 20, maxResultSizeTokens: 1_000 },
  steps: [
    {
      input: {
        toolResultMessage: user([toolResult('call-equal', [text('A'.repeat(20))])]),
      },
    },
  ],
});

await runCase({
  name: 'over-char-spill',
  budget: { maxResultSizeChars: 100, maxResultSizeTokens: 1_000, previewBytes: 12_000 },
  steps: [
    {
      input: {
        toolResultMessage: user([toolResult('call-spill', [text(longText)])]),
      },
    },
  ],
});

await runCase({
  name: 'exact-token-threshold',
  budget: { maxResultSizeChars: 100_000, maxResultSizeTokens: tokenCount },
  steps: [
    {
      input: {
        toolResultMessage: user([toolResult('call-token-equal', [text(tokenText)])]),
      },
    },
  ],
});

await runCase({
  name: 'over-token-spill',
  budget: { maxResultSizeChars: 100_000, maxResultSizeTokens: tokenCount - 1 },
  steps: [
    {
      input: {
        toolResultMessage: user([
          toolResult('call-token-spill', [text(tokenText)], { isError: false }),
        ]),
      },
    },
  ],
});

// >40,000-byte results take the sampled token estimate. Each text is tested at
// its exact estimate (inline, `<=`) and one below (spill, `>`), with the
// sample cuts crossing a surrogate pair.
for (const [suffix, value, estimate] of [
  ['50k', sampledText, sampledEstimate],
  ['150k', sampledTextLarge, sampledEstimateLarge],
]) {
  for (const [side, budget, expected] of [
    ['inline', estimate, 'tool_result'],
    ['spill', estimate - 1, 'tool_result_reference'],
  ]) {
    await runCase({
      name: `sampled-token-${suffix}-${side}`,
      budget: { maxResultSizeChars: 200_000, maxResultSizeTokens: budget, previewBytes: 100 },
      steps: [
        {
          input: {
            toolResultMessage: user([toolResult(`call-sampled-${suffix}`, [text(value)])]),
          },
          expectFirstBlockType: expected,
        },
      ],
    });
  }
}

await runCase({
  name: 'mixed-multiple',
  budget: { maxResultSizeChars: 50, maxResultSizeTokens: 1_000, previewBytes: 100 },
  steps: [
    {
      input: {
        toolResultMessage: user([
          toolResult('call-inline', [text('tiny')]),
          text('message level text'),
          toolResult('call-error', [text('B'.repeat(300))], { isError: true }),
        ]),
      },
    },
  ],
});

await runCase({
  name: 'media-blocks',
  budget: { maxResultSizeChars: 100, maxResultSizeTokens: 1_000, previewBytes: 12_000 },
  steps: [
    {
      input: {
        toolResultMessage: user([
          toolResult('call-media', [
            text('prefix'),
            image('A'.repeat(300), 'image/png', { bytes: 12_345, detail: 'high' }),
            image('B'.repeat(10), 'image/png'),
            pdf('C'.repeat(300), 'application/pdf', { bytes: 999, pages: 2 }),
          ]),
        ]),
      },
    },
  ],
});

await runCase({
  name: 'small-malformed-media',
  budget: { maxResultSizeChars: 100, maxResultSizeTokens: 1_000 },
  steps: [
    {
      input: {
        toolResultMessage: user([
          toolResult('call-small-media', [
            // Small image without mimeType: TS returns before reading it.
            { type: 'image', source: 'base64', data: 'B'.repeat(10) },
            // Audio inside a tool_result is not part of isToolResultMediaBlock.
            audio('A'.repeat(300), 'audio/wav'),
          ]),
        ]),
      },
    },
  ],
});

await runCase({
  name: 'image-with-pages',
  budget: { maxResultSizeChars: 8, maxResultSizeTokens: 1_000 },
  steps: [
    {
      input: {
        toolResultMessage: user([
          toolResult('call-image-pages', [
            image('Q'.repeat(64), 'image/png', { pages: 3 }),
          ]),
        ]),
      },
    },
  ],
});

await runCase({
  name: 'pdf-no-mime',
  budget: { maxResultSizeChars: 8, maxResultSizeTokens: 1_000 },
  steps: [
    {
      input: {
        toolResultMessage: user([
          // PDF's extensionForMedia branch never reads mimeType, so an
          // oversized PDF without one still replaces (preview says undefined).
          toolResult('call-pdf-no-mime', [pdf('P'.repeat(64), undefined as any)]),
        ]),
      },
    },
  ],
});

await runCase({
  name: 'supplemental-media',
  budget: { maxResultSizeChars: 100, maxResultSizeTokens: 1_000 },
  steps: [
    {
      input: {
        toolResultMessage: user([toolResult('call-main', [text('main result')])]),
        supplementalMessages: [
          {
            toolCallId: 'call-img',
            message: user([image('D'.repeat(250), 'image/jpeg', { detail: 'low' })]),
            extra: 'dropped by the TS map shape',
          },
          {
            toolCallId: 'call-audio',
            message: user([audio('E'.repeat(250), 'audio/wav')]),
          },
        ],
      },
    },
  ],
});

await runCase({
  name: 'supplemental-missing-toolcall-small',
  budget: { maxResultSizeChars: 1_000, maxResultSizeTokens: 1_000 },
  steps: [
    {
      input: {
        toolResultMessage: user([toolResult('call-main-small', [text('main result')])]),
        supplementalMessages: [
          // No toolCallId: TS never reads it for a small, non-media message.
          { message: user([text('small supplemental without a call id')]) },
        ],
      },
    },
  ],
});

await runCase({
  name: 'supplemental-missing-toolcall-media',
  budget: { maxResultSizeChars: 1, maxResultSizeTokens: 1_000 },
  steps: [
    {
      input: {
        toolResultMessage: user([toolResult('call-main-media', [text('primary body')])]),
        supplementalMessages: [
          // Oversized media reaches scopedToolResultKey and throws there.
          { message: user([image('I'.repeat(120), 'image/png')]) },
        ],
      },
    },
  ],
});

await runCase({
  name: 'supplemental-failure-keeps-primary',
  budget: { maxResultSizeChars: 1, maxResultSizeTokens: 1_000 },
  steps: [
    {
      input: {
        toolResultMessage: user([toolResult('call-primary', [text('primary body')])]),
        supplementalMessages: [
          {
            toolCallId: 'call-good',
            message: user([image('G'.repeat(120), 'image/png')]),
          },
          {
            toolCallId: 'call-bad',
            // mimeType is absent on purpose: TS throws from `.split`.
            message: user([{ type: 'image', source: 'base64', data: 'H'.repeat(120) }]),
          },
        ],
      },
    },
  ],
});

await runCase({
  name: 'turn-id-null',
  budget: { maxResultSizeChars: 1_000, maxResultSizeTokens: 1_000 },
  steps: [
    {
      input: {
        // null (not undefined) reaches safePathPart, which calls null.trim.
        turnId: null,
        toolResultMessage: user([toolResult('call-null-turn', [text('inline body')])]),
      },
    },
  ],
});

await runCase({
  name: 'turn-id-null-unused',
  budget: { maxResultSizeChars: 1_000, maxResultSizeTokens: 1_000 },
  steps: [
    {
      input: {
        turnId: null,
        toolResultMessage: user([]),
        supplementalMessages: [{ toolCallId: 'call-null-unused', message: user([text('kept')]) }],
      },
    },
  ],
});

await runCase({
  name: 'empty-and-nonuser',
  budget: { maxResultSizeChars: 1, maxResultSizeTokens: 1_000 },
  steps: [
    {
      input: {
        toolResultMessage: user([toolResult('call-empty', [])]),
      },
    },
    {
      input: {
        toolResultMessage: {
          role: 'assistant',
          content: [toolResult('call-assistant', [text('F'.repeat(200))])],
        },
      },
    },
  ],
});

await runCase({
  name: 'multibyte-emoji',
  budget: { maxResultSizeChars: 10, maxResultSizeTokens: 1_000, previewBytes: 100 },
  steps: [
    {
      input: {
        toolResultMessage: user([toolResult('call-emoji', [text('😀'.repeat(30))])]),
      },
    },
  ],
});

await runCase({
  name: 'multibyte-cjk',
  budget: { maxResultSizeChars: 10, maxResultSizeTokens: 1_000, previewBytes: 100 },
  steps: [
    {
      input: {
        toolResultMessage: user([toolResult('call-cjk', [text('中文测试'.repeat(30))])]),
      },
    },
  ],
});

await runCase({
  name: 'multibyte-surrogate-cut',
  budget: { maxResultSizeChars: 10, maxResultSizeTokens: 1_000, previewBytes: 100 },
  steps: [
    {
      input: {
        toolResultMessage: user([
          toolResult('call-surrogate', [text(`a${'😀'.repeat(50)}b`)]),
        ]),
      },
    },
  ],
});

await runCase({
  name: 'preview-overlap',
  budget: { maxResultSizeChars: 1, maxResultSizeTokens: 1_000, previewBytes: 2_040 },
  steps: [
    {
      input: {
        toolResultMessage: user([toolResult('call-overlap', [text('A'.repeat(2_500))])]),
      },
    },
  ],
});

await runCase({
  name: 'json-extension',
  budget: { maxResultSizeChars: 50, maxResultSizeTokens: 1_000 },
  steps: [
    {
      input: {
        toolResultMessage: user([
          toolResult('call-json', [text(`  {"a":1,"b":"${'x'.repeat(200)}"}`)]),
        ]),
      },
    },
  ],
});

await runCase({
  name: 'plain-layout',
  layout: 'plain',
  budget: { maxResultSizeChars: 5, maxResultSizeTokens: 1_000 },
  steps: [
    {
      input: {
        toolResultMessage: user([toolResult('call-plain', [text('plain layout body')])]),
      },
    },
  ],
});

await runCase({
  name: 'repeat-apply-cache',
  budget: { maxResultSizeChars: 5, maxResultSizeTokens: 1_000 },
  steps: [
    {
      input: {
        toolResultMessage: user([toolResult('call-repeat', [text('repeat body')])]),
      },
      turnId: 'turn-repeat',
    },
    {
      input: {
        toolResultMessage: user([toolResult('call-repeat', [text('repeat body')])]),
      },
      turnId: 'turn-repeat',
    },
    {
      input: {
        toolResultMessage: user([toolResult('call-repeat', [text('repeat body')])]),
      },
      turnId: 'turn-other',
    },
  ],
});

await runCase({
  name: 'existing-alias',
  budget: { maxResultSizeChars: 5, maxResultSizeTokens: 1_000 },
  files: [{ path: 'tool-results/refs/result-0001.txt', text: 'occupied alias\n' }],
  steps: [
    {
      input: {
        toolResultMessage: user([toolResult('call-alias', [text('alias body')])]),
      },
    },
  ],
});

await runCase({
  name: 'invalid-dir',
  blockingFile: 'blocker',
  toolResultsDir: 'blocker/session',
  budget: { maxResultSizeChars: 1, maxResultSizeTokens: 1_000 },
  steps: [
    {
      input: {
        toolResultMessage: user([toolResult('call-notdir', [text('blocked body')])]),
      },
    },
  ],
});

await runCase({
  name: 'unwritable-dir',
  deniedDir: 'denied',
  toolResultsDir: 'denied/session',
  requiresUnprivileged: true,
  budget: { maxResultSizeChars: 1, maxResultSizeTokens: 1_000 },
  steps: [
    {
      input: {
        toolResultMessage: user([toolResult('call-denied', [text('denied body')])]),
      },
    },
  ],
});

try {
  const oracle = {
    sourceCommit,
    sourceSha256,
    node: process.version,
    locale,
    counts: { cases: cases.length, steps: stepCount },
    cases,
  };
  const json = `${JSON.stringify(oracle, null, 2)}\n`;
  for (const leak of [scratch, output, baseline, process.cwd(), tmpdir()]) {
    assert.ok(!json.includes(leak), `oracle.json leaks a generation path: ${leak}`);
  }
  await writeFile(join(output, 'oracle.json'), json);
  console.log(`oracle.json: ${cases.length} cases, ${stepCount} steps`);
} finally {
  await rm(scratch, { recursive: true, force: true });
}
