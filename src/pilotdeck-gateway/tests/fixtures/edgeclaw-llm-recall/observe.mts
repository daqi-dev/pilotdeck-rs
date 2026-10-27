// Calls the fixed TS baseline directly. Every expected request body, result
// and debug record in oracle.json comes from these calls.
import assert from 'node:assert/strict';
import {mkdtempSync, rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {pathToFileURL} from 'node:url';
import {stub} from '../edgeclaw-llm/stub.mts';

export const ENTRY = 'src/context/memory/edgeclaw-memory-core/src/core/skills/llm-extraction.ts';
export const NOW = '2026-10-22T00:00:00.000Z';
export const HOME = '/fixture/home';

export function enc(v: any): any {
  if (v === undefined) return {$undefined: true};
  if (typeof v === 'number' && (Object.is(v, -0) || !Number.isFinite(v))) {
    return {$number: Object.is(v, -0) ? '-0' : String(v)};
  }
  if (Array.isArray(v)) return v.map(enc);
  if (v && typeof v === 'object') {
    return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, enc(x)]));
  }
  return v;
}

function chat(text: string) {
  return {status: 200, body: JSON.stringify({choices: [{message: {content: text}}]})};
}

function message(role: string, content: string) {
  return {role, content};
}

function candidate(over: any = {}) {
  return {
    projectId: 'p1',
    projectName: 'Alpha',
    description: 'Alpha project',
    status: 'active',
    updatedAt: '2026-01-01T00:00:00.000Z',
    sourceType: 'general_local',
    score: 1,
    exact: 0,
    source: 'query',
    matchedText: 'alpha',
    ...over,
  };
}

function entry(over: any = {}) {
  return {
    name: 'Entry',
    description: 'entry description',
    type: 'project',
    scope: 'project',
    projectId: 'p1',
    updatedAt: '2026-01-01T00:00:00.000Z',
    file: '/fixture/memory/one.md',
    relativePath: 'one.md',
    absolutePath: '/fixture/memory/one.md',
    ...over,
  };
}

function project(over: any = {}) {
  return {
    projectId: 'p1',
    projectName: 'Alpha',
    description: 'Alpha project',
    status: 'active',
    createdAt: '2026-01-01T00:00:00.000Z',
    updatedAt: '2026-01-01T00:00:00.000Z',
    relativePath: 'projects/alpha.md',
    absolutePath: '/fixture/memory/projects/alpha.md',
    ...over,
  };
}

function specs(): any[] {
  const out: any[] = [];
  const add = (spec: any) => out.push(spec);

  // -- decideFileMemoryRoute ------------------------------------------------
  add({
    name: 'decide-project',
    method: 'decideFileMemoryRoute',
    input: {
      query: 'What is the status of Alpha?',
      recentMessages: [
        message('user', '  earlier question  '),
        message('assistant', 'earlier answer'),
        message('user', `${'x'.repeat(225)}中😀 tail`),
      ],
    },
    steps: [chat('{"route":"project"}')],
  });
  add({
    name: 'decide-mix',
    method: 'decideFileMemoryRoute',
    input: {
      query: 'Who am I and what is this project doing?',
      recentMessages: [message('user', 'remember my background')],
    },
    steps: [chat('{"route":"mix"}')],
  });
  add({
    name: 'decide-user',
    method: 'decideFileMemoryRoute',
    input: {query: 'What is my name?'},
    steps: [chat('{"route":"user"}')],
  });
  add({
    name: 'decide-none',
    method: 'decideFileMemoryRoute',
    input: {query: 'Say hello'},
    steps: [chat('{"route":"none"}')],
  });
  add({
    name: 'decide-invalid-route',
    method: 'decideFileMemoryRoute',
    input: {query: 'Say hello'},
    steps: [chat('{"route":"USER"}')],
  });
  add({
    name: 'decide-malformed-json',
    method: 'decideFileMemoryRoute',
    input: {query: 'Say hello'},
    steps: [chat('{bad}')],
  });
  add({
    name: 'decide-no-json',
    method: 'decideFileMemoryRoute',
    input: {query: 'Say hello'},
    steps: [chat('no json here')],
  });
  add({
    name: 'decide-empty-raw',
    method: 'decideFileMemoryRoute',
    input: {query: 'Say hello'},
    steps: [chat('')],
  });
  add({
    name: 'decide-surrogate',
    method: 'decideFileMemoryRoute',
    input: {
      query: 'a\ud800b',
      recentMessages: [message('user', 'x\udfffy'), message('assistant', 'ok')],
    },
    steps: [chat('{"route":"none"}')],
  });
  add({
    name: 'decide-recent-slice',
    method: 'decideFileMemoryRoute',
    input: {
      query: 'Continue',
      recentMessages: [
        message('user', 'one'),
        message('assistant', 'two'),
        message('user', 'three'),
        message('assistant', 'four'),
        message('user', 'five'),
        message('assistant', 'six'),
      ],
    },
    steps: [chat('{"route":"none"}')],
  });

  // -- selectRecallProject -------------------------------------------------
  add({
    name: 'select-match',
    method: 'selectRecallProject',
    input: {
      query: 'Alpha status',
      recentUserMessages: [message('user', 'Alpha please'), message('user', 'not Beta')],
      shortlist: [
        candidate(),
        candidate({projectId: 'p2', projectName: 'Bravo', description: 'Bravo project'}),
      ],
    },
    steps: [chat('{"selected_project_id":"p2","reason":"  matches  "}')],
  });
  add({
    name: 'select-match-whitespace-id',
    method: 'selectRecallProject',
    input: {
      query: 'Alpha status',
      shortlist: [candidate(), candidate({projectId: 'p2', projectName: 'Bravo'})],
    },
    steps: [chat('{"selected_project_id":"  p1  ","reason":""}')],
  });
  add({
    name: 'select-outside-fallback',
    method: 'selectRecallProject',
    input: {
      query: 'Unknown project',
      shortlist: [candidate()],
      allowEmpty: false,
    },
    steps: [chat('{"selected_project_id":"nope","reason":""}')],
  });
  add({
    name: 'select-outside-allow-empty',
    method: 'selectRecallProject',
    input: {
      query: 'Unknown project',
      shortlist: [candidate()],
      allowEmpty: true,
    },
    steps: [chat('{"selected_project_id":"nope","reason":""}')],
  });
  add({
    name: 'select-unmatched-allow-empty',
    method: 'selectRecallProject',
    input: {
      query: 'Unknown project',
      shortlist: [candidate()],
      allowEmpty: true,
    },
    steps: [chat('{"selected_project_id":""}')],
  });
  add({
    name: 'select-empty-shortlist',
    method: 'selectRecallProject',
    input: {query: 'Anything', shortlist: [], allowEmpty: false},
    steps: [],
  });
  add({
    name: 'select-malformed-fallback',
    method: 'selectRecallProject',
    input: {query: 'Alpha', shortlist: [candidate()]},
    steps: [chat('{bad}')],
  });
  add({
    name: 'select-malformed-allow-empty',
    method: 'selectRecallProject',
    input: {query: 'Alpha', shortlist: [candidate()], allowEmpty: true},
    steps: [chat('{bad}')],
  });
  add({
    name: 'select-fallback-exact',
    method: 'selectRecallProject',
    input: {
      query: 'pick',
      shortlist: [
        candidate({projectId: 'p1', projectName: 'Lowexact', exact: 0, score: 9}),
        candidate({projectId: 'p2', projectName: 'Highexact', exact: 1, score: 0}),
      ],
    },
    steps: [chat('{"selected_project_id":"missing"}')],
  });
  add({
    name: 'select-fallback-score',
    method: 'selectRecallProject',
    input: {
      query: 'pick',
      shortlist: [
        candidate({projectId: 'p1', projectName: 'Lowscore', exact: 1, score: 3}),
        candidate({projectId: 'p2', projectName: 'Highscore', exact: 1, score: 9}),
      ],
    },
    steps: [chat('{"selected_project_id":"missing"}')],
  });
  add({
    name: 'select-fallback-priority',
    method: 'selectRecallProject',
    input: {
      query: 'pick',
      shortlist: [
        candidate({
          projectId: 'p1',
          projectName: 'LocalOlder',
          exact: 1,
          score: 5,
          sourceType: 'general_local',
          updatedAt: '2026-01-01T00:00:00.000Z',
        }),
        candidate({
          projectId: 'p2',
          projectName: 'ExternalNewer',
          exact: 1,
          score: 5,
          sourceType: 'workspace_external',
          updatedAt: '2026-03-01T00:00:00.000Z',
        }),
      ],
    },
    steps: [chat('{"selected_project_id":"missing"}')],
  });
  add({
    name: 'select-fallback-updated-locale',
    method: 'selectRecallProject',
    input: {
      query: 'pick',
      shortlist: [
        candidate({
          projectId: 'p1',
          projectName: 'LowerA',
          exact: 1,
          score: 5,
          updatedAt: 'a',
        }),
        candidate({
          projectId: 'p2',
          projectName: 'UpperB',
          exact: 1,
          score: 5,
          updatedAt: 'B',
        }),
      ],
    },
    steps: [chat('{"selected_project_id":"missing"}')],
  });
  // JS number semantics: `!==` keeps +0 and -0 equal, NaN unequal, and the
  // subtraction returns NaN which Array#sort treats as +0.
  add({
    name: 'select-fallback-zero',
    method: 'selectRecallProject',
    input: {
      query: 'pick',
      shortlist: [
        candidate({projectId: 'a', projectName: 'MinusZero', exact: -0, score: 2}),
        candidate({projectId: 'b', projectName: 'PlusZero', exact: 0, score: 1}),
      ],
    },
    steps: [chat('{"selected_project_id":"missing"}')],
  });
  add({
    name: 'select-fallback-nan-first',
    method: 'selectRecallProject',
    input: {
      query: 'pick',
      shortlist: [
        candidate({projectId: 'a', projectName: 'NanFirst', exact: NaN, score: 1}),
        candidate({projectId: 'b', projectName: 'ZeroSecond', exact: 0, score: 9}),
      ],
    },
    steps: [chat('{"selected_project_id":"missing"}')],
  });
  add({
    name: 'select-fallback-nan-second',
    method: 'selectRecallProject',
    input: {
      query: 'pick',
      shortlist: [
        candidate({projectId: 'b', projectName: 'ZeroFirst', exact: 0, score: 9}),
        candidate({projectId: 'a', projectName: 'NanSecond', exact: NaN, score: 1}),
      ],
    },
    steps: [chat('{"selected_project_id":"missing"}')],
  });
  add({
    name: 'select-fallback-nan-both',
    method: 'selectRecallProject',
    input: {
      query: 'pick',
      shortlist: [
        candidate({projectId: 'a', projectName: 'NanLowScore', exact: NaN, score: 1}),
        candidate({projectId: 'b', projectName: 'NanHighScore', exact: NaN, score: 9}),
      ],
    },
    steps: [chat('{"selected_project_id":"missing"}')],
  });
  add({
    name: 'select-fallback-nan-three',
    method: 'selectRecallProject',
    input: {
      query: 'pick',
      shortlist: [
        candidate({projectId: 'a', projectName: 'NanOne', exact: NaN, score: 1}),
        candidate({projectId: 'b', projectName: 'ZeroNine', exact: 0, score: 9}),
        candidate({projectId: 'c', projectName: 'NanFive', exact: NaN, score: 5}),
      ],
    },
    steps: [chat('{"selected_project_id":"missing"}')],
  });
  add({
    name: 'select-fallback-infinity',
    method: 'selectRecallProject',
    input: {
      query: 'pick',
      shortlist: [
        candidate({projectId: 'a', projectName: 'PositiveInfinity', exact: Infinity, score: 1}),
        candidate({projectId: 'b', projectName: 'ZeroNine', exact: 0, score: 9}),
      ],
    },
    steps: [chat('{"selected_project_id":"missing"}')],
  });
  add({
    name: 'select-source-unknown',
    method: 'selectRecallProject',
    input: {query: 'Alpha', shortlist: [candidate({sourceType: undefined})]},
    steps: [chat('{"selected_project_id":"p1"}')],
  });
  add({
    name: 'select-description-truncate',
    method: 'selectRecallProject',
    input: {
      query: 'Alpha',
      shortlist: [candidate({
        description: `${'d'.repeat(190)}😀 tail`,
        matchedText: `${'m'.repeat(190)}中😀 tail`,
      })],
    },
    steps: [chat('{"selected_project_id":"p1"}')],
  });
  add({
    name: 'select-reason-truncate',
    method: 'selectRecallProject',
    input: {query: 'Alpha', shortlist: [candidate()]},
    steps: [chat(JSON.stringify({selected_project_id: 'p1', reason: `${'r'.repeat(230)}😀 tail`}))],
  });
  add({
    name: 'select-recent-user-slice',
    method: 'selectRecallProject',
    input: {
      query: 'Alpha',
      recentUserMessages: Array.from({length: 6}, (_, i) => message('user', `message ${i}`)),
      shortlist: [candidate()],
    },
    steps: [chat('{"selected_project_id":"p1"}')],
  });

  // -- selectFileManifestEntries ------------------------------------------
  add({
    name: 'manifest-normal',
    method: 'selectFileManifestEntries',
    input: {
      query: 'Where is Alpha documented?',
      route: 'project',
      recentUserMessages: [message('user', 'Alpha docs')],
      projectMeta: project(),
      manifest: [
        entry({relativePath: 'a.md', description: 'A'}),
        entry({relativePath: 'b.md', description: 'B'}),
        entry({relativePath: 'c.md', description: 'C'}),
      ],
    },
    steps: [chat('{"selected_ids":["a.md"," b.md ",1,null,""," a.md "]}')],
  });
  add({
    name: 'manifest-single-string',
    method: 'selectFileManifestEntries',
    input: {
      query: 'Alpha',
      route: 'project',
      manifest: [entry()],
    },
    steps: [chat('{"selected_ids":"  a.md  "}')],
  });
  add({
    name: 'manifest-non-array',
    method: 'selectFileManifestEntries',
    input: {query: 'Alpha', route: 'project', manifest: [entry()]},
    steps: [chat('{"selected_ids":42}')],
  });
  add({
    name: 'manifest-missing-ids',
    method: 'selectFileManifestEntries',
    input: {query: 'Alpha', route: 'project', manifest: [entry()]},
    steps: [chat('{}')],
  });
  add({
    name: 'manifest-empty-ids',
    method: 'selectFileManifestEntries',
    input: {query: 'Alpha', route: 'project', manifest: [entry()]},
    steps: [chat('{"selected_ids":[]}')],
  });
  add({
    name: 'manifest-malformed',
    method: 'selectFileManifestEntries',
    input: {query: 'Alpha', route: 'project', manifest: [entry()]},
    steps: [chat('{bad}')],
  });
  add({
    name: 'manifest-project-null',
    method: 'selectFileManifestEntries',
    input: {query: 'Alpha', route: 'user', manifest: [entry()]},
    steps: [chat('{"selected_ids":["one.md"]}')],
  });
  add({
    name: 'manifest-empty-manifest',
    method: 'selectFileManifestEntries',
    input: {query: 'Alpha', route: 'mix', manifest: []},
    steps: [chat('{"selected_ids":[]}')],
  });
  for (const [label, limit] of [
    ['absent', undefined],
    ['zero', 0],
    ['fraction', 2.9],
    ['over', 9],
  ] as [string, number | undefined][]) {
    add({
      name: `manifest-limit-${label}`,
      method: 'selectFileManifestEntries',
      input: {
        query: 'Alpha',
        route: 'project',
        manifest: Array.from({length: 8}, (_, i) => entry({relativePath: `m${i}.md`})),
        ...(limit === undefined ? {} : {limit}),
      },
      steps: [chat(JSON.stringify({selected_ids: Array.from({length: 8}, (_, i) => `m${i}.md`) }))],
    });
  }
  add({
    name: 'manifest-description-truncate',
    method: 'selectFileManifestEntries',
    input: {
      query: 'Alpha',
      route: 'project',
      projectMeta: project({description: `${'d'.repeat(190)}😀 tail`}),
      manifest: [
        entry({
          relativePath: 'long.md',
          description: `${'e'.repeat(205)}中😀 tail`,
          type: 'feedback',
          scope: 'global',
          projectId: null,
        }),
      ],
    },
    steps: [chat('{"selected_ids":["long.md"]}')],
  });
  add({
    name: 'manifest-200',
    method: 'selectFileManifestEntries',
    input: {
      query: 'Alpha',
      route: 'project',
      manifest: Array.from({length: 205}, (_, i) => entry({relativePath: `n${i}.md`})),
    },
    steps: [chat('{"selected_ids":["n0.md"]}')],
  });
  add({
    name: 'manifest-recent-user-slice',
    method: 'selectFileManifestEntries',
    input: {
      query: 'Alpha',
      route: 'project',
      recentUserMessages: Array.from({length: 6}, (_, i) => message('user', `recent ${i}`)),
      manifest: [entry()],
    },
    steps: [chat('{"selected_ids":["one.md"]}')],
  });
  // Empty model config fails before any request; the catch branch must warn.
  add({
    name: 'decide-gate-fallback-warning',
    method: 'decideFileMemoryRoute',
    config: 'empty',
    input: {query: 'Say hello'},
    steps: [],
  });
  add({
    name: 'select-fallback-warning',
    method: 'selectRecallProject',
    config: 'empty',
    input: {query: 'Alpha', shortlist: [candidate()]},
    steps: [],
  });
  add({
    name: 'select-fallback-warning-allow-empty',
    method: 'selectRecallProject',
    config: 'empty',
    input: {query: 'Alpha', shortlist: [candidate()], allowEmpty: true},
    steps: [],
  });
  add({
    name: 'manifest-fallback-warning',
    method: 'selectFileManifestEntries',
    config: 'empty',
    input: {query: 'Alpha', route: 'project', manifest: [entry()]},
    steps: [],
  });
  return out;
}

function normalized(value: any, origin: string): any {
  if (typeof value === 'string') return value.split(origin).join('$ORIGIN');
  if (Array.isArray(value)) return value.map(item => normalized(item, origin));
  if (value && typeof value === 'object') {
    return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, normalized(item, origin)]));
  }
  return value;
}

export async function collect(baseline: string) {
  assert.equal(process.version, 'v22.22.0');
  const home = mkdtempSync(join(tmpdir(), 'pdrs-recall-home-'));
  const oldHome = process.env.HOME;
  const oldDateNow = Date.now;
  const oldRandom = Math.random;
  process.env.HOME = home;
  Date.now = () => Date.parse(NOW);
  Math.random = () => 0.25;
  const {LlmMemoryExtractor} = await import(pathToFileURL(join(baseline, ENTRY)).href);
  const cases: any[] = [];
  try {
    for (const spec of specs()) {
      const server = await stub(spec.steps, () => 0);
      const config = spec.config === 'empty' ? {} : {
        agents: {defaults: {model: {primary: 'p/m'}}},
        models: {
          providers: {
            p: {
              api: 'chat',
              baseUrl: server.origin,
              apiKey: 'fixture-key',
              models: [{id: 'm'}],
            },
          },
        },
      };
      const runtime = {telemetry: {}};
      const warnings: string[] = [];
      const events: string[] = [];
      const logger = {warn: (message: any) => {
        warnings.push(String(message));
        events.push('warn');
      }};
      const extractor: any = new LlmMemoryExtractor(config, runtime, logger);
      const debug: any[] = [];
      const input = {...spec.input, debugTrace: (value: any) => {
        debug.push(value);
        events.push('debug');
      }};
      let result: any;
      try {
        result = enc(await extractor[spec.method](input));
      } catch (error: any) {
        result = {threw: error.message};
      }
      cases.push({
        // Numeric sentinels keep -0 / NaN / Infinity and explicit undefined
        // through JSON; the Rust replay decodes them before calling the port.
        spec: enc({
          name: spec.name,
          method: spec.method,
          input: spec.input,
          steps: spec.steps,
          ...(spec.config === undefined ? {} : {config: spec.config}),
        }),
        expected: normalized({result, requests: server.requests, debug, warnings, events}, server.origin),
      });
      await server.close();
    }
  } finally {
    Date.now = oldDateNow;
    Math.random = oldRandom;
    if (oldHome === undefined) delete process.env.HOME;
    else process.env.HOME = oldHome;
    rmSync(home, {recursive: true, force: true});
  }
  return {cases, clockMs: Date.parse(NOW)};
}
