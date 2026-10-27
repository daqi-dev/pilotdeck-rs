// Calls the fixed TS baseline directly through its real LlmMemoryExtractor.
// The loopback stub records every request; expected result/debug/validation are
// the values the same TS run produced. No expected value is synthesized.
import assert from 'node:assert/strict';
import {existsSync, mkdtempSync, readFileSync, rmSync} from 'node:fs';
import {createRequire} from 'node:module';
import {tmpdir} from 'node:os';
import {dirname, resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {stub} from '../edgeclaw-llm/stub.mts';

export const ENTRY = 'src/context/memory/edgeclaw-memory-core/src/core/skills/llm-extraction.ts';
export const DREAM_ENTRY = 'src/context/memory/edgeclaw-memory-core/src/core/review/dream-review.ts';

const ok = (text: string) => ({
  status: 200,
  body: JSON.stringify({choices: [{message: {content: text}}]}),
});

function jsonSafe(value: any): any {
  if (value === undefined) return null;
  return JSON.parse(JSON.stringify(value));
}

async function transformedDream(baseline: string) {
  const sourcePath = resolve(baseline, DREAM_ENTRY);
  const requireFromBaseline = createRequire(resolve(baseline, 'package.json'));
  const {transformSync} = createRequire(requireFromBaseline.resolve('tsx'))('esbuild');
  const code = transformSync(
    readFileSync(sourcePath, 'utf8')
      + '\nexport { validateExclusiveClusters, validateGeneralProjectMergeGroups };\n',
    {loader: 'ts', format: 'esm', target: 'es2022', sourcefile: sourcePath},
  ).code;
  const executable = code.replace(/from (["'])(\.[^"']+)\1/g, (_: string, quote: string, specifier: string) => {
    let file = resolve(dirname(sourcePath), specifier);
    if (file.endsWith('.js') && existsSync(file.slice(0, -3) + '.ts')) file = file.slice(0, -3) + '.ts';
    return 'from ' + quote + pathToFileURL(file).href + quote;
  });
  return import('data:text/javascript;base64,' + Buffer.from(executable).toString('base64'));
}

const HEADER = (relativePath: string, name: string, description: string, updatedAt: string) =>
  ({relativePath, name, description, updatedAt});

const META = (projectId: string, projectName: string, description: string, status: string,
  updatedAt: string, extra: any = {}) =>
  ({projectId, projectName, description, status, updatedAt, ...extra});

const RECORD = (entryId: string, relativePath: string, type: 'project' | 'feedback',
  name: string, description: string, updatedAt: string, content: string, extra: any = {}) =>
  ({entryId, relativePath, type, scope: 'project', isTmp: false, name, description,
    updatedAt, content, ...extra});

const long = (char: string, count: number) => char.repeat(count);

function specs(): any[] {
  return [
    {
      name: 'planDreamClusters#0', fn: 'planDreamClusters',
      input: {
        kind: 'project',
        headers: [
          HEADER('Project/a.md', '  Alpha   Project  ', long('x', 230) + ' tail', '2026-01-01T00:00:00.000Z'),
          HEADER('Project/b.md', 'Beta', '  one\n two  ', '2026-01-02T00:00:00.000Z'),
          HEADER('Project/c.md', 'Gamma', 'short', '2026-01-03T00:00:00.000Z'),
          HEADER('Project/d.md', 'Delta', 'd', '2026-01-04T00:00:00.000Z'),
        ],
      },
      text: JSON.stringify({
        summary: `  Plan   summary  ${long('s', 400)}`,
        clusters: [
          {member_relative_paths: [' Project/a.md ', 'Project/b.md', 'Project/a.md', 'Unknown.md', 5],
            reason: `  Overlap    facts  ${long('r', 350)}`},
          {member_relative_paths: ['Project/b.md', 'Project/c.md'], reason: 'same project'},
          {member_relative_paths: ['Project/c.md', 'Project/d.md'], reason: 'duplicate'},
          {member_relative_paths: 'Project/a.md'},
          null,
        ],
      }),
    },
    {
      name: 'planDreamClusters#1', fn: 'planDreamClusters',
      input: {
        kind: 'feedback',
        headers: [HEADER('Feedback/a.md', 'A', 'aa', 'U1'), HEADER('Feedback/b.md', 'B', 'bb', 'U2')],
      },
      text: JSON.stringify({clusters: 'not an array'}),
    },
    {
      name: 'planDreamClusters#2', fn: 'planDreamClusters',
      input: {kind: 'project', headers: [HEADER('Project/only.md', 'Only', 'one', 'U')]},
    },
    {
      name: 'planDreamClusters#3', fn: 'planDreamClusters',
      input: {
        kind: 'project',
        headers: [HEADER('Project/a.md', 'A', 'a', 'U1'), HEADER('Project/b.md', 'B', 'b', 'U2')],
      },
      text: 'not json at all',
    },
    {
      name: 'refineDreamCluster#0', fn: 'refineDreamCluster',
      input: {
        kind: 'project',
        records: [
          RECORD('Project/a.md', 'Project/a.md', 'project', ' Alpha ', long('d', 350),
            '2026-01-01', '## Current Stage\nBuild\n', {capturedAt: '2026-01-01', sourceSessionKey: 's'}),
          RECORD('Project/b.md', 'Project/b.md', 'project', 'Beta', '  beta\ndesc  ', '2026-01-02',
            '## Notes\n- n\n', {capturedAt: null, sourceSessionKey: null}),
        ],
      },
      text: JSON.stringify({
        summary: `  Refine   summary ${long('r', 400)}`,
        name: `  Refined    Name  ${long('n', 140)}`,
        description: `  Refined   description ${long('e', 350)}`,
        markdown: '\n\n  ## Summary\nbody\n\n',
      }),
    },
    {
      name: 'refineDreamCluster#1', fn: 'refineDreamCluster',
      input: {
        kind: 'feedback',
        records: [RECORD('Feedback/a.md', 'Feedback/a.md', 'feedback', 'A', 'aa', 'U', 'body', {rule: 'r'})],
      },
      text: JSON.stringify({name: 'N', description: 'D', markdown: '   '}),
    },
    {
      name: 'refineDreamCluster#2', fn: 'refineDreamCluster',
      input: {kind: 'feedback', records: []},
    },
    {
      name: 'planGeneralProjectMetaMerges#0', fn: 'planGeneralProjectMetaMerges',
      input: {
        projectMetas: [
          META('p1', 'P One', long('a', 300), 'active', 'U1',
            {dreamUpdatedAt: 'D1', sourceKind: 'general_local', sourceWorkspacePath: '/ws', sourceProjectId: 'sp'}),
          META('p2', 'P Two', 'desc two', 'done', 'U2', {dreamUpdatedAt: '', sourceKind: '', sourceWorkspacePath: null}),
          META('p3', 'P Three', 'desc three', 'planned', 'U3', {sourceProjectId: false}),
        ],
      },
      text: JSON.stringify({
        summary: ` merge  ${long('m', 400)}`,
        merge_groups: [
          {keeper_project_id: ' p1 ', duplicate_project_ids: [' p2 ', 'p2', '  ', 'p3'], reason: '  same  '},
          {keeper_project_id: '', duplicate_project_ids: ['p1'], reason: 'bad keeper'},
          {keeper_project_id: 'p9', duplicate_project_ids: ['p1']},
          {keeper_project_id: 'p1', duplicate_project_ids: ['p4']},
          null,
        ],
      }),
    },
    {
      name: 'planGeneralProjectMetaMerges#1', fn: 'planGeneralProjectMetaMerges',
      input: {projectMetas: [META('p1', 'P', 'd', 'active', 'U')]},
    },
    {
      name: 'planGeneralProjectMetaMerges#2', fn: 'planGeneralProjectMetaMerges',
      input: {
        projectMetas: [META('p1', 'P1', 'd1', 'active', 'U1'), META('p2', 'P2', 'd2', 'done', 'U2')],
      },
      text: JSON.stringify({
        merge_groups: [
          {keeper_project_id: 'p1', duplicate_project_ids: ' p2 '},
          {keeper_project_id: null, duplicate_project_ids: []},
          7,
        ],
      }),
    },
    {
      name: 'reviewDreamProjectMeta#0', fn: 'reviewDreamProjectMeta',
      input: {
        currentMeta: META('p', '  Fallback Name  ', long('f', 350), '', 'U', {dreamUpdatedAt: 'D'}),
        recentProjectRecords: [RECORD('Project/a.md', 'Project/a.md', 'project', 'A', long('a', 230), 'U', 'body')],
        recentFeedbackRecords: [RECORD('Feedback/a.md', 'Feedback/a.md', 'feedback', 'F', 'desc', 'U', 'body')],
      },
      text: JSON.stringify({
        should_update: ' TRUE ',
        reason: `  reason ${long('r', 400)}`,
        project_name: '   ',
        description: `  ${long('d', 350)}  `,
        status: '   ',
      }),
    },
    {
      name: 'reviewDreamProjectMeta#1', fn: 'reviewDreamProjectMeta',
      input: {
        currentMeta: META('p', 'Fallback', 'fallback desc', 'paused', 'U'),
        recentProjectRecords: [],
        recentFeedbackRecords: [],
      },
      text: JSON.stringify({should_update: 'yes', status: null}),
    },
    {
      name: 'rewriteUserProfile#0', fn: 'rewriteUserProfile',
      input: {
        existingProfile: {files: [{content: `## 身份背景\n- old fact\n${long('x', 4000)}`}]},
        candidates: [
          {type: 'project', body: long('b', 1500), description: '  proj   desc  ', capturedAt: '2026', sourceSessionKey: 's1'},
          {type: 'user', description: '  user   one  ', body: '', profile: 'Profile note', summary: '', capturedAt: '', sourceSessionKey: 's2'},
          {type: 'user', description: 'second', summary: 'Summary note', capturedAt: '2026-02', sourceSessionKey: 's3'},
        ],
      },
      text: JSON.stringify({identity_background_markdown: '```markdown\n## 身份背景\n- Uses Rust, ;\n- 长 fact\n```'}),
    },
    {
      name: 'rewriteUserProfile#1', fn: 'rewriteUserProfile',
      input: {
        existingProfile: null,
        candidates: [{type: 'user', description: 'only', body: '', profile: '', summary: '', capturedAt: '', sourceSessionKey: ''}],
      },
      text: JSON.stringify({identity_background: ['  -  A  fact  ', '**B**', 'a  fact', '  ', 5]}),
    },
    {
      name: 'rewriteUserProfile#2', fn: 'rewriteUserProfile',
      input: {
        existingProfile: null,
        candidates: [{type: 'project', description: 'not a user', body: 'body', capturedAt: 'C', sourceSessionKey: 'S'}],
      },
    },
    {
      name: 'rewriteUserProfile#3', fn: 'rewriteUserProfile',
      input: {
        existingProfile: null,
        candidates: [{type: 'user', description: 'x', body: 'y', capturedAt: 'C', sourceSessionKey: 'S'}],
      },
      text: 'not json at all',
    },
    {
      name: 'rewriteUserProfile#4', fn: 'rewriteUserProfile',
      input: {
        existingProfile: null,
        candidates: [{type: 'user', description: 'x', body: 'y', capturedAt: 'C', sourceSessionKey: 'S'}],
      },
      text: JSON.stringify({identity_background_markdown: null, identity_background: ''}),
    },
    {
      name: 'rewriteUserProfile#5', fn: 'rewriteUserProfile',
      input: {
        existingProfile: null,
        candidates: [{type: 'user', description: 'x', body: 'y', capturedAt: 'C', sourceSessionKey: 'S'}],
      },
      text: JSON.stringify({identity_background_markdown: 42}),
    },
    {
      name: 'rewriteUserProfile#6', fn: 'rewriteUserProfile',
      input: {
        existingProfile: null,
        candidates: [{type: 'user', description: '  edge   case  ',
          body: '## 身份背景\r\n\r\n- **Rust** and *Go* are `tools`.\r\n- Rust，Go；Rust。\r\n- x\r\n',
          capturedAt: '', sourceSessionKey: ''}],
      },
      text: JSON.stringify({
        identity_background_markdown:
          '```MARKDOWN\n###   身份背景   \r\n- **Rust** and *Go* are `tools`.\r\n- Rust，Go；Rust。\r\n- x\r\n```',
      }),
    },
    {
      name: 'planDreamClusters#4', fn: 'planDreamClusters',
      input: {
        kind: 'project',
        headers: Array.from({length: 40}, (_, index) => HEADER(
          `Project/f${index}.md`, `F${index}`, `d${index}`, `U${index}`)),
      },
      text: JSON.stringify({
        summary: 'cap',
        clusters: [{
          member_relative_paths: [
            ...Array.from({length: 40}, (_, index) => `Project/f${index}.md`),
            'Project/f0.md',
            'Unknown.md',
          ],
          reason: 'duplicate',
        }],
      }),
    },
    {
      name: 'reviewDreamProjectMeta#2', fn: 'reviewDreamProjectMeta',
      input: {
        currentMeta: META('p', 'Fallback', 'fallback desc', 'active', 'U'),
        recentProjectRecords: [],
        recentFeedbackRecords: [],
      },
      text: JSON.stringify({
        should_update: 0, reason: 123, project_name: 0, description: null, status: 0,
      }),
    },
    {
      name: 'refineDreamCluster#3', fn: 'refineDreamCluster',
      input: {
        kind: 'project',
        records: [RECORD('Project/a.md', 'Project/a.md', 'project', 'A', 'aa', 'U', 'body')],
      },
      text: JSON.stringify({summary: 123, name: 1, description: null, markdown: false}),
    },
    {
      name: 'planDreamClusters#5', fn: 'planDreamClusters',
      input: {
        headers: [HEADER('Project/a.md', 'A', 'a', 'U1'), HEADER('Project/b.md', 'B', 'b', 'U2')],
      },
      text: JSON.stringify({clusters: []}),
    },
    // fix-r2 P1-1: JSON.parse runs on a JavaScript string, so the inner JSON may
    // hold an unpaired surrogate. Keep both directions and the diagnostic.
    {
      name: 'planDreamClusters#6', fn: 'planDreamClusters',
      input: {
        kind: 'project',
        headers: [HEADER('Project/a.md', 'A', 'a', 'U1'), HEADER('Project/b.md', 'B', 'b', 'U2')],
      },
      text: '{"summary":"' + String.fromCharCode(0xd800) + '","clusters":[]}',
    },
    {
      name: 'planDreamClusters#7', fn: 'planDreamClusters',
      input: {
        kind: 'project',
        headers: [HEADER('Project/a.md', 'A', 'a', 'U1'), HEADER('Project/b.md', 'B', 'b', 'U2')],
      },
      text: '{"summary":"' + String.fromCharCode(0xdc00) + '","clusters":[]}',
    },
    // V8 quotes the offending unpaired surrogate in the SyntaxError message.
    {
      name: 'planDreamClusters#8', fn: 'planDreamClusters',
      input: {
        kind: 'project',
        headers: [HEADER('Project/a.md', 'A', 'a', 'U1'), HEADER('Project/b.md', 'B', 'b', 'U2')],
      },
      text: '{"summary":' + String.fromCharCode(0xd800) + '}',
    },
    // fix-r2 P1-2: ECMAScript `m` anchors also match after U+2028/U+2029.
    {
      name: 'rewriteUserProfile#7', fn: 'rewriteUserProfile',
      input: {
        existingProfile: null,
        candidates: [{type: 'user', description: 'person', body: 'person'}],
      },
      text: JSON.stringify({identity_background_markdown: ['alpha\u2028## beta']}),
    },
    {
      name: 'rewriteUserProfile#8', fn: 'rewriteUserProfile',
      input: {
        existingProfile: null,
        candidates: [{type: 'user', description: 'person', body: 'person'}],
      },
      text: JSON.stringify({identity_background_markdown: ['alpha\u2029- beta']}),
    },
  ];
}

function validateCase(spec: any, result: any) {
  if (spec.fn === 'planDreamClusters' && result && Array.isArray(result.clusters)) {
    return {kind: 'clusters'};
  }
  if (spec.fn === 'planGeneralProjectMetaMerges' && result && Array.isArray(result.mergeGroups)) {
    return {kind: 'merge', metas: spec.input.projectMetas, groups: result.mergeGroups};
  }
  return undefined;
}

export async function collect(baseline: string) {
  assert.equal(process.version, 'v22.22.0');
  const home = mkdtempSync(resolve(tmpdir(), 'pdrs-llm-dream-home-'));
  const oldHome = process.env.HOME;
  const oldDateNow = Date.now;
  const oldRandom = Math.random;
  process.env.HOME = home;
  Date.now = () => 1792972800000;
  Math.random = () => 0.25;
  const {LlmMemoryExtractor} = await import(pathToFileURL(resolve(baseline, ENTRY)).href);
  const dream = await transformedDream(baseline);
  const cases: any[] = [];
  try {
    for (const spec of specs()) {
      const now = 0;
      const steps = spec.text === undefined ? [] : [ok(spec.text)];
      const server = await stub(steps, () => now);
      const config = {
        agents: {defaults: {model: {primary: 'p/m'}}},
        models: {providers: {p: {api: 'chat', baseUrl: `${server.origin}/v1`, apiKey: 'fixture-key', models: [{id: 'm'}]}}},
      };
      const extractor = new LlmMemoryExtractor(config, {});
      const debug: any[] = [];
      const input = {...spec.input, debugTrace: (value: any) => debug.push(value)};
      let outcome: any;
      try {
        let result: any;
        switch (spec.fn) {
          case 'rewriteUserProfile': result = await extractor.rewriteUserProfile(input); break;
          case 'planDreamClusters': result = await extractor.planDreamClusters(input); break;
          case 'refineDreamCluster': result = await extractor.refineDreamCluster(input); break;
          case 'planGeneralProjectMetaMerges': result = await extractor.planGeneralProjectMetaMerges(input); break;
          case 'reviewDreamProjectMeta': result = await extractor.reviewDreamProjectMeta(input); break;
          default: throw Error(`unknown method ${spec.fn}`);
        }
        const validation = validateCase(spec, result);
        if (validation?.kind === 'clusters') {
          validation.result = dream.validateExclusiveClusters(
            result.clusters,
            new Set(spec.input.headers.map((header: any) => header.relativePath)),
            4,
          );
        } else if (validation?.kind === 'merge') {
          validation.result = dream.validateGeneralProjectMergeGroups({
            metas: validation.metas,
            groups: validation.groups,
          });
        }
        outcome = {result: jsonSafe(result), debug: jsonSafe(debug), validation: validation ? jsonSafe(validation) : null};
      } catch (error: any) {
        outcome = {error: {name: String(error?.name), message: String(error?.message)}, debug: jsonSafe(debug)};
      }
      cases.push({name: spec.name, fn: spec.fn, input: spec.input, steps,
        expected: {...outcome, requests: server.requests}});
      await server.close();
    }
  } finally {
    Date.now = oldDateNow;
    Math.random = oldRandom;
    if (oldHome === undefined) delete process.env.HOME;
    else process.env.HOME = oldHome;
    rmSync(home, {recursive: true, force: true});
  }
  return {cases};
}
