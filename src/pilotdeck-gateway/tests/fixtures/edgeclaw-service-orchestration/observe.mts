// Shared real-TS scenario driver for the EdgeClaw service async orchestration
// (A6.6-edgeclaw-19, M19 concurrency matrix).
//
// `generate.mts` and `verify.mts` both call `collect`, which imports the fixed
// `service.ts`, freezes the clock and `Math.random`, isolates HOME, and runs
// the same declarative operation sequence that
// `tests/edgeclaw_service_orchestration.rs` replays on the Rust side against a
// scripted `ModelHttpTransport`.
//
// Interleaving is deterministic on both sides: only the request at `gate` (by
// fetch-call ordinal) is parked; the `during` event runs while it is parked and
// the parked request only reaches the stub server after `release`, so the
// server arrival order — and therefore the script consumption order — is
// fixed. `awaitDuring` marks the two cells whose during event is itself a
// full async service call awaited to completion before the release
// (flush∥flush, dream∥dream).
//
// Every observed value is encoded so `undefined`, `NaN` and `Infinity`
// survive JSON.
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {mkdtempSync, mkdirSync, readFileSync, readdirSync, rmSync, statSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join, resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {DatabaseSync} from 'node:sqlite';

export const ENTRY = 'src/context/memory/edgeclaw-memory-core/src/service.ts';
export const NOW = '2026-10-22T00:00:00.000Z';
export const NOW_MS = Date.parse(NOW);
// Every value has at most one digit after "0." in base 36, so
// `Math.random().toString(36).slice(2, 10)` — the dream trace suffix — is
// always the empty string, and the Rust replay can answer the suffix from a
// fixed injected token while cycling the same draws.
export const RANDOM_VALUES = [0.25, 0.5, 0.75];

const MESSAGES = [
  {role: 'user', content: 'alpha user'},
  {role: 'assistant', content: 'alpha assistant'},
];

let candidateSerial = 0;
// sameOrigin dedups on (capturedAt, sourceSessionKey, type); every seed needs
// its own sourceSessionKey.
const CANDIDATE = (type: string, name: string) => ({
  type,
  scope: type === 'user' ? 'global' : 'project',
  name,
  description: `${name} description`,
  body: `## Summary\n${name} body`,
  capturedAt: NOW,
  sourceSessionKey: `seed-session-${++candidateSerial}`,
});

/// `undefined`, non-finite numbers, finite `-0`. Rust mirrors this exact
/// encoding in its observation writer.
export function encode(value: any): any {
  if (value === undefined) return {$type: 'undefined'};
  if (typeof value === 'number' && (Object.is(value, -0) || !Number.isFinite(value))) {
    return {$type: 'number', value: Object.is(value, -0) ? '-0' : String(value)};
  }
  if (Array.isArray(value)) return value.map(encode);
  if (value && typeof value === 'object') {
    return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, encode(item)]));
  }
  return value;
}

function clean(value: any, root: string, workspaceHash: string): any {
  if (typeof value === 'string') {
    return value
      .split(root).join('$ROOT')
      .replace(/\/workspaces\/([0-9a-f]{10})(?=\/|$)/gu, (_match, hash: string) => {
        // Bind the observed slug to sha1(resolve(workspaceDir))[:10] so a wrong
        // seed (root, fixed string, …) cannot be erased by the replacement.
        assert.equal(hash, workspaceHash,
          `workspace data-dir slug ${hash} is not sha1(resolve(workspaceDir))=${workspaceHash}`);
        return '/workspaces/$HASH';
      });
  }
  if (Array.isArray(value)) return value.map((item) => clean(item, root, workspaceHash));
  if (value && typeof value === 'object') {
    return Object.fromEntries(
      Object.entries(value).map(([key, item]) => [key, clean(item, root, workspaceHash)]),
    );
  }
  return value;
}

const errorShape = (error: any) => ({name: error?.name ?? null, message: error?.message ?? null});

function tree(root: string, base = root): any[] {
  if (!statSync(root, {throwIfNoEntry: false})) return [];
  return readdirSync(root).sort().flatMap((name) => {
    const path = join(root, name);
    const relative = path.slice(base.length + 1);
    return statSync(path).isDirectory()
      ? tree(path, base)
      : [{relativePath: relative, content: readFileSync(path, 'utf8')}];
  });
}

function sqliteDump(path: string) {
  const db = new DatabaseSync(path, {readOnly: true});
  try {
    return {
      pipeline: db.prepare('SELECT state_key, state_json, updated_at FROM pipeline_state ORDER BY state_key').all(),
      l0: db.prepare('SELECT l0_index_id, session_key, timestamp, messages_json, source, indexed, created_at FROM l0_sessions ORDER BY l0_index_id').all(),
    };
  } finally {
    db.close();
  }
}

// -- LLM stub server ----------------------------------------------------------

async function stubServer(steps: any[]) {
  const requests: any[] = [];
  const {createServer} = await import('node:http');
  const server = createServer(async (req: any, res: any) => {
    const chunks: Buffer[] = [];
    for await (const chunk of req) chunks.push(Buffer.from(chunk));
    const body = Buffer.concat(chunks).toString('utf8');
    let kind = 'unknown';
    const system = /"content":"You are the ([^"\\]{0,60})/.exec(body);
    if (system) kind = system[1];
    requests.push({method: req.method, url: req.url, kind, body});
    const step = steps[requests.length - 1] ?? steps.at(-1);
    if (!step) {
      res.writeHead(599);
      res.end('script exhausted');
      return;
    }
    res.writeHead(step.status ?? 200, {'content-type': 'application/json'});
    res.end(step.body ?? '');
  });
  await new Promise<void>((done) => server.listen(0, '127.0.0.1', done));
  return {
    origin: `http://127.0.0.1:${(server.address() as any).port}`,
    requests,
    close: () => new Promise<void>((done) => server.close(() => done())),
  };
}

const response = (value: any) => ({
  status: 200,
  body: JSON.stringify({choices: [{message: {content: JSON.stringify(value)}}]}),
});
const http500 = {status: 500, body: JSON.stringify({error: {message: 'fixture boom'}})};

// dream success script (single mode): plan(project) → refine → meta review.
// With 0 user notes rewriteUserProfile makes no request, so each dream
// consumes exactly 3 steps. `member_relative_paths` is the only spelling
// `normalizeDreamCluster` reads; refine is flat.
function dreamSteps(paths: string[]) {
  return [
    response({clusters: [{member_relative_paths: paths, reason: 'fixture merge'}], summary: 'fixture plan summary'}),
    response({name: 'Merged Note', description: 'merged description', markdown: '## Summary\nmerged body', summary: 'fixture refine summary'}),
    response({should_update: false, reason: 'fixture no meta change'}),
  ];
}
// no-op dream: one scripted response, consumed by the meta review when the
// plan short-circuits below the 2-file threshold.
function dreamNoopSteps() {
  return [response({clusters: [], summary: 'fixture no clusters'})];
}

// -- fetch gate: only the request at ordinal `holdAt` parks -------------------

function fetchGate(holdAt: number) {
  const realFetch = globalThis.fetch;
  let seen = 0;
  let release: (() => void) | undefined;
  const gate = new Promise<void>((done) => (release = done));
  const requestSeen = new Promise<void>((done) => {
    (globalThis as any).fetch = async (url: any, init: any) => {
      const at = ++seen;
      if (at === holdAt) {
        done();
        await gate;
      }
      return await realFetch(url, init);
    };
  });
  return {
    requestSeen,
    release: () => release!(),
    restore: () => {
      (globalThis as any).fetch = realFetch;
    },
  };
}

// -- per-cell context ----------------------------------------------------------

interface Ctx {
  service: any;
  server: any;
  steps: any[];
  state: any;
  gate: (ordinal: number) => {requestSeen: Promise<void>, release: () => void, restore: () => void};
  pipeline: (keys: string[]) => any;
  l0: () => any[];
  files: () => any[];
  lastDreamTree: () => any[];
  names: () => string[];
  kinds: () => string[];
  cleaned: (value: any) => any;
}

interface Cell {
  name: string;
  seeds?: any[][];
  captures?: any[];
  settings?: Record<string, any> | null;
  setState?: [string, string][] | null;
  gate?: number;
  during?: any;
  awaitDuring?: boolean;
  run: (ctx: Ctx) => Promise<any>;
}

function cells(): Cell[] {
  return [
    {
      name: 'idle-flush-empty',
      run: async (ctx) => {
        ctx.steps.push(response({should_store: false, labels: []}));
        const stats = await ctx.service.flush();
        return {stats, pipeline: ctx.pipeline(['autoIndexAnchorAt', 'autoDreamAnchorAt', 'lastIndexedAt', 'lastDreamAt'])};
      },
    },
    {
      name: 'idle-rollback-none',
      run: async (ctx) => {
        try {
          return {ok: true, result: await ctx.service.rollbackLastDream()};
        } catch (error) {
          return {ok: false, error: errorShape(error)};
        }
      },
    },
    {
      name: 'idle-maintenance-none',
      run: async (ctx) => ({result: await ctx.service.runDueScheduledMaintenance()}),
    },
    {
      name: 'flush-inflight-flush',
      captures: [
        {messages: MESSAGES, input: {sessionKey: 's1', timestamp: '2026-10-22T00:00:01.000Z'}},
        {messages: MESSAGES, input: {sessionKey: 's2', timestamp: '2026-10-22T00:00:02.000Z'}},
      ],
      gate: 1,
      during: {op: 'flush', options: {reason: 'manual'}},
      awaitDuring: true,
      run: async (ctx) => {
        // flush2's four steps reach the server while flush1 is parked; then
        // flush1's two (its parked classify arrives last).
        ctx.steps.push(
          response({should_store: true, labels: [{type: 'project', reason: 'r'}]}),
          response({skip: false, reason: '', name: 'Alpha', description: 'Alpha description', markdown: '## Summary\nAlpha body'}),
          response({should_store: true, labels: [{type: 'project', reason: 'r'}]}),
          response({skip: false, reason: '', name: 'Beta', description: 'Beta description', markdown: '## Summary\nBeta body'}),
          response({should_store: true, labels: [{type: 'project', reason: 'r'}]}),
          response({skip: false, reason: '', name: 'Alpha', description: 'Alpha description', markdown: '## Summary\nAlpha body'}),
        );
        const gate = ctx.gate(1);
        const first = ctx.service.flush({reason: 'manual', sessionKeys: ['s1']});
        await gate.requestSeen;
        const second = await ctx.service.flush({reason: 'manual'});
        gate.release();
        const a = await first;
        return {
          first: a,
          second,
          files: ctx.files().map((file) => file.relativePath),
          l0: ctx.l0(),
          pipeline: ctx.pipeline(['autoIndexAnchorAt', 'lastIndexedAt']),
          requestKinds: ctx.kinds(),
        };
      },
    },
    {
      name: 'flush-inflight-capture',
      captures: [
        {messages: MESSAGES, input: {sessionKey: 's1', timestamp: '2026-10-22T00:00:01.000Z'}},
      ],
      gate: 1,
      during: {op: 'captureTurn', messages: MESSAGES, input: {sessionKey: 's2', timestamp: '2026-10-22T00:00:02.000Z'}},
      run: async (ctx) => {
        ctx.steps.push(
          response({should_store: true, labels: [{type: 'project', reason: 'r'}]}),
          response({skip: false, reason: '', name: 'Alpha', description: 'Alpha description', markdown: '## Summary\nAlpha body'}),
        );
        const gate = ctx.gate(1);
        const flush = ctx.service.flush({reason: 'manual'});
        await gate.requestSeen;
        const capture = ctx.service.captureTurn(MESSAGES, {sessionKey: 's2', timestamp: '2026-10-22T00:00:02.000Z'});
        gate.release();
        const stats = await flush;
        return {stats, capture, l0: ctx.l0(), requestKinds: ctx.kinds()};
      },
    },
    {
      name: 'flush-inflight-close',
      captures: [
        {messages: MESSAGES, input: {sessionKey: 's1', timestamp: '2026-10-22T00:00:01.000Z'}},
      ],
      gate: 1,
      during: {op: 'close'},
      run: async (ctx) => {
        ctx.steps.push(
          response({should_store: true, labels: [{type: 'project', reason: 'r'}]}),
          response({skip: false, reason: '', name: 'Alpha', description: 'Alpha description', markdown: '## Summary\nAlpha body'}),
        );
        const gate = ctx.gate(1);
        const flush = ctx.service.flush({reason: 'manual'});
        await gate.requestSeen;
        let closeError: any = null;
        try {
          ctx.service.close();
        } catch (error) {
          closeError = errorShape(error);
        }
        gate.release();
        let out: any;
        try {
          out = {ok: true, stats: await flush};
        } catch (error) {
          out = {ok: false, error: errorShape(error)};
        }
        out.closeError = closeError;
        out.files = ctx.files().map((file) => file.relativePath);
        return out;
      },
    },
    {
      name: 'dream-stage-act',
      seeds: [[CANDIDATE('project', 'Alpha one'), CANDIDATE('project', 'Alpha two')]],
      gate: 1,
      run: async (ctx) => {
        const seeds = ctx.state.seeds[0].map((input: any) => ctx.service.repository.getFileMemoryStore().upsertCandidate(input));
        ctx.steps.push(...dreamSteps(seeds.map((seed: any) => seed.relativePath)));
        const gate = ctx.gate(1);
        const dream = ctx.service.dream('manual');
        await gate.requestSeen;
        const act = ctx.service.act({action: 'edit_entry', id: seeds[0].relativePath, name: 'Alpha one EDITED DURING DREAM', description: 'edited during dream'});
        gate.release();
        const result = await dream;
        return {
          act,
          result: {rewrittenProjects: result.rewrittenProjects, deletedFiles: result.deletedFiles, status: result.status, summary: result.summary},
          namesAfter: ctx.names(),
          editedSurvived: ctx.names().includes('Alpha one EDITED DURING DREAM'),
          files: ctx.files(),
          requestKinds: ctx.kinds(),
        };
      },
    },
    {
      name: 'dream-stage-file-write',
      seeds: [[CANDIDATE('project', 'Alpha one'), CANDIDATE('project', 'Alpha two')]],
      gate: 1,
      during: {op: 'upsertCandidate', input: CANDIDATE('project', 'Written During Dream')},
      run: async (ctx) => {
        const seeds = ctx.state.seeds[0].map((input: any) => ctx.service.repository.getFileMemoryStore().upsertCandidate(input));
        ctx.steps.push(...dreamSteps(seeds.map((seed: any) => seed.relativePath)));
        const gate = ctx.gate(1);
        const dream = ctx.service.dream('manual');
        await gate.requestSeen;
        const written = ctx.service.repository.getFileMemoryStore().upsertCandidate(ctx.state.during.input);
        gate.release();
        const result = await dream;
        return {
          writtenName: written.name,
          result: {rewrittenProjects: result.rewrittenProjects, status: result.status},
          writeSurvived: ctx.names().includes('Written During Dream'),
          requestKinds: ctx.kinds(),
        };
      },
    },
    {
      name: 'dream-stage-capture',
      seeds: [[CANDIDATE('project', 'Alpha one'), CANDIDATE('project', 'Alpha two')]],
      gate: 1,
      during: {op: 'captureTurn', messages: MESSAGES, input: {sessionKey: 'during-dream', timestamp: '2026-10-22T00:00:05.000Z'}},
      run: async (ctx) => {
        const seeds = ctx.state.seeds[0].map((input: any) => ctx.service.repository.getFileMemoryStore().upsertCandidate(input));
        ctx.steps.push(...dreamSteps(seeds.map((seed: any) => seed.relativePath)));
        const gate = ctx.gate(1);
        const dream = ctx.service.dream('manual');
        await gate.requestSeen;
        const capture = ctx.service.captureTurn(MESSAGES, {sessionKey: 'during-dream', timestamp: '2026-10-22T00:00:05.000Z'});
        gate.release();
        const result = await dream;
        return {
          capture,
          result: {status: result.status, rewrittenProjects: result.rewrittenProjects},
          l0: ctx.l0(),
          requestKinds: ctx.kinds(),
        };
      },
    },
    {
      name: 'dream-stage-clear-session',
      seeds: [[CANDIDATE('project', 'Alpha one'), CANDIDATE('project', 'Alpha two')]],
      captures: [
        {messages: MESSAGES, input: {sessionKey: 's1', timestamp: '2026-10-22T00:00:01.000Z'}},
      ],
      gate: 1,
      during: {op: 'clearSession', sessionKey: 's1'},
      run: async (ctx) => {
        const seeds = ctx.state.seeds[0].map((input: any) => ctx.service.repository.getFileMemoryStore().upsertCandidate(input));
        ctx.steps.push(...dreamSteps(seeds.map((seed: any) => seed.relativePath)));
        const gate = ctx.gate(1);
        const dream = ctx.service.dream('manual');
        await gate.requestSeen;
        const cleared = ctx.service.clearSession('s1');
        gate.release();
        const result = await dream;
        return {
          cleared,
          result: {status: result.status, rewrittenProjects: result.rewrittenProjects},
          l0: ctx.l0(),
          requestKinds: ctx.kinds(),
        };
      },
    },
    {
      name: 'dream-stage-rollback-guard',
      seeds: [
        [CANDIDATE('project', 'Beta one'), CANDIDATE('project', 'Beta two')],
        [CANDIDATE('project', 'Gamma one'), CANDIDATE('project', 'Gamma two')],
      ],
      gate: 1,
      during: {op: 'rollbackLastDream'},
      run: async (ctx) => {
        const store = ctx.service.repository.getFileMemoryStore();
        const beta = ctx.state.seeds[0].map((input: any) => store.upsertCandidate(input));
        ctx.steps.push(...dreamSteps(beta.map((seed: any) => seed.relativePath)));
        const dream1 = await ctx.service.dream('manual');
        const namesAfterDream1 = ctx.names();
        const gamma = ctx.state.seeds[1].map((input: any) => store.upsertCandidate(input));
        ctx.steps.push(...dreamSteps(gamma.map((seed: any) => seed.relativePath)));
        const gate = ctx.gate(1);
        const dream2 = ctx.service.dream('manual');
        await gate.requestSeen;
        let rollback: any;
        try {
          rollback = {ok: true, rolledBackAt: (await ctx.service.rollbackLastDream()).rolledBackAt != null};
        } catch (error) {
          rollback = {ok: false, error: errorShape(error)};
        }
        gate.release();
        let dream2result: any;
        try {
          const result = await dream2;
          dream2result = {ok: true, result: {status: result.status, rewrittenProjects: result.rewrittenProjects, summary: result.summary}};
        } catch (error) {
          dream2result = {ok: false, error: errorShape(error)};
        }
        return {
          dream1: {rewrittenProjects: dream1.rewrittenProjects, names: namesAfterDream1},
          rollbackDuringDream: rollback,
          dream2: dream2result,
          namesFinal: ctx.names(),
          files: ctx.files(),
          pipeline: ctx.pipeline(['lastDreamAt', 'lastDreamStatus', 'autoDreamAnchorAt']),
          requestKinds: ctx.kinds(),
        };
      },
    },
    {
      name: 'dream-stage-rollback-ok',
      seeds: [
        [CANDIDATE('project', 'Beta one'), CANDIDATE('project', 'Beta two')],
        [],
      ],
      gate: 1,
      during: {op: 'rollbackLastDream'},
      run: async (ctx) => {
        const store = ctx.service.repository.getFileMemoryStore();
        const beta = ctx.state.seeds[0].map((input: any) => store.upsertCandidate(input));
        ctx.steps.push(...dreamSteps(beta.map((seed: any) => seed.relativePath)));
        const dream1 = await ctx.service.dream('manual');
        const namesAfterDream1 = ctx.names();
        ctx.steps.push(...dreamNoopSteps());
        const gate = ctx.gate(1);
        const dream2 = ctx.service.dream('manual');
        await gate.requestSeen;
        let rollback: any;
        try {
          rollback = {ok: true, result: await ctx.service.rollbackLastDream()};
        } catch (error) {
          rollback = {ok: false, error: errorShape(error)};
        }
        gate.release();
        let dream2result: any;
        try {
          const result = await dream2;
          dream2result = {ok: true, result: {status: result.status, rewrittenProjects: result.rewrittenProjects, summary: result.summary}};
        } catch (error) {
          dream2result = {ok: false, error: errorShape(error)};
        }
        return {
          dream1: {rewrittenProjects: dream1.rewrittenProjects, names: namesAfterDream1},
          rollbackDuringDream: rollback,
          dream2: dream2result,
          namesFinal: ctx.names(),
          files: ctx.files(),
          pipeline: ctx.pipeline(['lastDreamAt', 'lastDreamStatus', 'autoDreamAnchorAt']),
          requestKinds: ctx.kinds(),
        };
      },
    },
    {
      name: 'dream-stage-error',
      seeds: [[CANDIDATE('project', 'Alpha one'), CANDIDATE('project', 'Alpha two')]],
      run: async (ctx) => {
        ctx.steps.push(http500, http500, http500);
        const service = ctx.service;
        const store = service.repository.getFileMemoryStore();
        ctx.state.seeds[0].forEach((input: any) => store.upsertCandidate(input));
        let stageCreated = 0;
        let disposed = 0;
        const originalCreate = (service.repository as any).createDreamStage.bind(service.repository);
        (service.repository as any).createDreamStage = (...args: any[]) => {
          stageCreated += 1;
          const stage = originalCreate(...args);
          const originalDispose = stage.dispose.bind(stage);
          stage.dispose = (...disposeArgs: any[]) => {
            disposed += 1;
            return originalDispose(...disposeArgs);
          };
          return stage;
        };
        let out: any;
        try {
          const result = await service.dream('manual');
          out = {ok: true, result: {status: result.status}};
        } catch (error) {
          out = {ok: false, error: errorShape(error)};
        }
        out.stageCreated = stageCreated;
        out.disposed = disposed;
        out.namesAfter = ctx.names();
        out.files = ctx.files();
        out.pipeline = ctx.pipeline(['lastDreamAt', 'lastDreamStatus', 'lastDreamSummary']);
        out.lastDreamTree = ctx.lastDreamTree();
        out.stageTempLeft = tree(service.memoryDir).map((file) => file.relativePath).filter((p: string) => p.includes('dream-'));
        return out;
      },
    },
    {
      name: 'dream-noop',
      run: async (ctx) => {
        const result = await ctx.service.dream('manual');
        return {
          result: {
            status: result.status,
            rewrittenProjects: result.rewrittenProjects,
            deletedFiles: result.deletedFiles,
            deletedProjects: result.deletedProjects,
            profileUpdated: result.profileUpdated,
            duplicateTopicCount: result.duplicateTopicCount,
            conflictTopicCount: result.conflictTopicCount,
            reviewedFiles: result.reviewedFiles,
            summary: result.summary,
            trigger: result.trigger,
          },
          files: ctx.files(),
          pipeline: ctx.pipeline(['lastDreamAt', 'lastDreamStatus', 'lastDreamSummary', 'autoDreamAnchorAt', 'autoIndexAnchorAt']),
          lastDreamTree: ctx.lastDreamTree(),
        };
      },
    },
    {
      name: 'dream-stage-close',
      seeds: [[CANDIDATE('project', 'Alpha one'), CANDIDATE('project', 'Alpha two')]],
      gate: 1,
      during: {op: 'close'},
      run: async (ctx) => {
        const seeds = ctx.state.seeds[0].map((input: any) => ctx.service.repository.getFileMemoryStore().upsertCandidate(input));
        ctx.steps.push(...dreamSteps(seeds.map((seed: any) => seed.relativePath)));
        const gate = ctx.gate(1);
        const dream = ctx.service.dream('manual');
        await gate.requestSeen;
        let closeError: any = null;
        try {
          ctx.service.close();
        } catch (error) {
          closeError = errorShape(error);
        }
        gate.release();
        let out: any;
        try {
          const result = await dream;
          out = {ok: true, result: {status: result.status}};
        } catch (error) {
          out = {ok: false, error: errorShape(error)};
        }
        out.closeError = closeError;
        out.files = ctx.files();
        out.lastDreamTree = ctx.lastDreamTree();
        out.pipeline = ctx.pipeline(['lastDreamAt', 'lastDreamStatus']);
        return out;
      },
    },
    {
      name: 'dream-stage-dream',
      seeds: [
        [CANDIDATE('project', 'Alpha one'), CANDIDATE('project', 'Alpha two')],
        [],
      ],
      gate: 1,
      during: {op: 'dream', trigger: 'manual'},
      awaitDuring: true,
      run: async (ctx) => {
        const seeds = ctx.state.seeds[0].map((input: any) => ctx.service.repository.getFileMemoryStore().upsertCandidate(input));
        const paths = seeds.map((seed: any) => seed.relativePath);
        ctx.steps.push(...dreamSteps(paths));
        ctx.steps.push(...dreamSteps(paths));
        const gate = ctx.gate(1);
        const dream1 = ctx.service.dream('manual');
        await gate.requestSeen;
        const dream2 = ctx.service.dream('manual');
        const b = await dream2;
        gate.release();
        const a = await dream1;
        return {
          first: {status: a.status, rewrittenProjects: a.rewrittenProjects, summary: a.summary},
          second: {status: b.status, rewrittenProjects: b.rewrittenProjects, summary: b.summary},
          names: ctx.names(),
          files: ctx.files(),
          pipeline: ctx.pipeline(['lastDreamAt', 'lastDreamStatus']),
          requestKinds: ctx.kinds(),
        };
      },
    },
    {
      name: 'maintenance-backlog',
      captures: Array.from({length: 20}, (_unused, index) => ({
        messages: MESSAGES,
        input: {sessionKey: `s${index + 1}`, timestamp: '2026-10-22T00:00:01.000Z'},
      })),
      run: async (ctx) => {
        for (let i = 0; i < 20; i++) {
          ctx.steps.push(
            response({should_store: true, labels: [{type: 'project', reason: 'r'}]}),
            response({skip: false, reason: '', name: 'Backlog Note', description: 'Backlog description', markdown: '## Summary\nBacklog body'}),
          );
        }
        const result = await ctx.service.runDueScheduledMaintenance();
        return {
          result: {indexRan: result.indexRan, dreamRan: result.dreamRan, indexStats: result.indexStats},
          files: ctx.files(),
          pipeline: ctx.pipeline(['autoIndexAnchorAt', 'autoDreamAnchorAt', 'lastIndexedAt']),
          l0: ctx.l0(),
          requestKinds: ctx.kinds(),
        };
      },
    },
    {
      name: 'maintenance-interval',
      captures: [
        {messages: MESSAGES, input: {sessionKey: 'old', timestamp: '2026-10-21T23:00:00.000Z'}},
      ],
      run: async (ctx) => {
        ctx.steps.push(
          response({should_store: true, labels: [{type: 'project', reason: 'r'}]}),
          response({skip: false, reason: '', name: 'Interval Note', description: 'Interval description', markdown: '## Summary\nInterval body'}),
        );
        const result = await ctx.service.runDueScheduledMaintenance();
        return {
          result: {indexRan: result.indexRan, dreamRan: result.dreamRan, indexStats: result.indexStats},
          files: ctx.files(),
          pipeline: ctx.pipeline(['autoIndexAnchorAt', 'autoDreamAnchorAt', 'lastIndexedAt']),
          requestKinds: ctx.kinds(),
        };
      },
    },
    {
      name: 'maintenance-interval-zero',
      captures: [
        {messages: MESSAGES, input: {sessionKey: 'old', timestamp: '2026-10-21T23:00:00.000Z'}},
      ],
      settings: {autoIndexIntervalMinutes: 0, autoDreamIntervalMinutes: 0},
      run: async (ctx) => {
        const result = await ctx.service.runDueScheduledMaintenance();
        return {
          result: {indexRan: result.indexRan, dreamRan: result.dreamRan},
          files: ctx.files().map((file) => file.relativePath),
          pipeline: ctx.pipeline(['autoIndexAnchorAt', 'autoDreamAnchorAt']),
          l0: ctx.l0(),
        };
      },
    },
    {
      name: 'maintenance-dream-due',
      seeds: [[CANDIDATE('project', 'Alpha one'), CANDIDATE('project', 'Alpha two')]],
      setState: [['lastIndexedAt', '2026-10-21T22:00:00.000Z'], ['autoDreamAnchorAt', '2026-10-21T22:00:00.000Z']],
      run: async (ctx) => {
        const seeds = ctx.state.seeds[0].map((input: any) => ctx.service.repository.getFileMemoryStore().upsertCandidate(input));
        ctx.steps.push(...dreamSteps(seeds.map((seed: any) => seed.relativePath)));
        const result = await ctx.service.runDueScheduledMaintenance();
        return {
          result: {
            indexRan: result.indexRan,
            dreamRan: result.dreamRan,
            dreamResult: result.dreamResult && {trigger: result.dreamResult.trigger, status: result.dreamResult.status, rewrittenProjects: result.dreamResult.rewrittenProjects},
          },
          files: ctx.files(),
          pipeline: ctx.pipeline(['lastDreamAt', 'lastDreamStatus', 'autoDreamAnchorAt']),
          lastDreamTree: ctx.lastDreamTree(),
          requestKinds: ctx.kinds(),
        };
      },
    },
  ];
}

export async function collect(baseline: string) {
  assert.equal(process.version, 'v22.22.0');
  const root = mkdtempSync(join(tmpdir(), 'pdrs-service-orchestration-'));
  const home = join(root, 'home');
  mkdirSync(home, {recursive: true});
  const oldHome = process.env.HOME;
  const oldDate = globalThis.Date;
  const oldDateNow = Date.now;
  const oldRandom = Math.random;
  let randomIndex = 0;
  class FrozenDate extends oldDate {
    constructor(...args: any[]) {
      super(...((args.length ? args : [NOW_MS]) as [number]));
    }
    static now() {
      return NOW_MS;
    }
  }
  (globalThis as any).Date = FrozenDate;
  Date.now = () => NOW_MS;
  Math.random = () => RANDOM_VALUES[randomIndex++ % RANDOM_VALUES.length];
  process.env.HOME = home;
  const {EdgeClawMemoryService} = await import(pathToFileURL(join(baseline, ENTRY)).href);
  const observed: any[] = [];
  let instance = 0;
  try {
    for (const cell of cells()) {
      const dir = join(root, `svc-${++instance}`);
      mkdirSync(join(dir, 'workspace'), {recursive: true});
      const steps: any[] = [];
      const server = await stubServer(steps);
      const service = new EdgeClawMemoryService({
        workspaceDir: join(dir, 'workspace'),
        rootDir: join(dir, 'root'),
        llm: {
          provider: 'p', model: 'm', modelRef: 'p/m',
          baseUrl: server.origin + '/v1', apiKey: 'fixture-key', apiType: 'openai-completions',
        },
      });
      const workspaceHash = createHash('sha1')
        .update(resolve(service.workspaceDir)).digest('hex').slice(0, 10);
      const state: any = {
        seeds: cell.seeds ?? [],
        during: cell.during ?? null,
      };
      const ctx: Ctx = {
        service,
        server,
        steps,
        state,
        gate: fetchGate,
        pipeline: (keys) => {
          const out: any = {};
          for (const key of keys) {
            try {
              out[key] = service.repository.getPipelineState(key) ?? null;
            } catch (error) {
              out[key] = {error: errorShape(error).message};
            }
          }
          return out;
        },
        l0: () => service.repository.listAllL0().map((row: any) => ({key: row.sessionKey, indexed: row.indexed})),
        files: () => tree(service.memoryDir),
        lastDreamTree: () => tree(join(service.memoryDir, '..', 'last_dream')),
        names: () => service.repository.listMemoryEntries({includeDeprecated: true}).map((entry: any) => entry.name),
        kinds: () => server.requests.map((request) => request.kind),
        cleaned: (value) => clean(encode(value), root, workspaceHash),
      };
      for (const capture of cell.captures ?? []) {
        service.captureTurn(capture.messages, capture.input);
      }
      if (cell.settings) service.saveSettings(cell.settings);
      for (const [key, value] of cell.setState ?? []) service.repository.setPipelineState(key, value);
      let result: any;
      let failed: any = null;
      try {
        result = await cell.run(ctx);
      } catch (error) {
        failed = {cellError: errorShape(error)};
      }
      try { service.close(); } catch {}
      await server.close();
      const cleaned = ctx.cleaned;
      observed.push({
        name: cell.name,
        // The steps actually pushed, in push order == server arrival order
        // (only the parked request is delayed and it is consumed last).
        script: steps.map((step) => ({status: step.status ?? 200, body: step.body ?? ''})),
        seeds: cleaned(cell.seeds ?? []),
        captures: cleaned(cell.captures ?? []),
        settings: cleaned(cell.settings ?? null),
        setState: cell.setState ?? null,
        gate: cell.gate ?? null,
        during: cleaned(cell.during ?? null),
        awaitDuring: cell.awaitDuring ?? false,
        observed: failed ? cleaned(failed) : cleaned(result),
        sqlite: cleaned(sqliteDump(service.dbPath)),
      });
    }
  } finally {
    if (oldHome === undefined) delete process.env.HOME;
    else process.env.HOME = oldHome;
    (globalThis as any).Date = oldDate;
    Date.now = oldDateNow;
    Math.random = oldRandom;
    rmSync(root, {recursive: true, force: true});
  }
  return {
    sourceCommit: 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85',
    nodeVersion: process.version,
    randomValues: RANDOM_VALUES,
    scenarios: observed,
  };
}
