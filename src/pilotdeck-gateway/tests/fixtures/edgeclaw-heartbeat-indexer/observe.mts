// Scenario definitions shared by the generator and verifier. Every observation
// below comes from the fixed cc7c4e6 `HeartbeatIndexer` running with its real
// `MemoryRepository`, `FileMemoryStore` and `LlmMemoryExtractor`; the only
// substitutions are a loopback scripted HTTP server, a frozen clock and an
// isolated HOME.
import assert from 'node:assert/strict';
import {mkdtempSync, mkdirSync, readdirSync, readFileSync, rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join, resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {stub} from '../edgeclaw-llm/stub.mts';

export const NOW = '2026-10-22T00:00:00.000Z';
export const ENTRY = 'src/context/memory/edgeclaw-memory-core/src/service.ts';
const SOURCE_ROOT = 'src/context/memory/edgeclaw-memory-core/src';

const u = (content: string, msgId?: string) => msgId ? {role: 'user', content, msgId} : {role: 'user', content};
const a = (content: string) => ({role: 'assistant', content});

const response = (value: any) => ({
  status: 200,
  body: JSON.stringify({choices: [{message: {content: JSON.stringify(value)}}]}),
});
const classify = (...labels: any[]) => response({should_store: labels.length > 0, labels});
const label = (type: string, reason: string) => ({type, reason, evidence: `${type} evidence`});
const create = (name: string, description: string, markdown: string) =>
  response({skip: false, reason: '', name, description, markdown});
const select = (decision: 'attach_existing' | 'create_new', projectId = '', reason = '') =>
  response({decision, selected_project_id: projectId, reason});

const PROJECT_LABEL = () => label('project', 'durable project fact');
const PROJECT_NOTE = () => create('Alpha', 'Alpha description', '## Summary\nAlpha body');
// 79 UTF-16 units plus one non-BMP code point: `truncateForPrompt(name, 80)`
// keeps the high surrogate and drops the low one, so every trace string that
// carries this name must keep the lone surrogate instead of replacing or
// dropping it.
const LONG_EMOJI_NAME = 'a'.repeat(79) + '😀';
const NON_BMP_NAME = '项目😀标题';

function capture(sessionKey: string, timestamp: string, messages: any[], source?: string) {
  return {sessionKey, timestamp, messages, ...(source === undefined ? {} : {source})};
}

// Cases whose timing is `null` exercise the acceptance path (captureTurn then
// runHeartbeat). The `timing`/`action` cases are the M13 concurrency matrix.
export const cases = () => [
  {
    name: 'single-project',
    mode: 'single',
    captures: [
      capture('s1', '2026-10-22T00:00:01.000Z', [u('first user'), a('first answer')]),
      capture('s1', '2026-10-22T00:00:02.000Z', [u('second user'), a('second answer')]),
    ],
    llm: [classify(PROJECT_LABEL()), PROJECT_NOTE(), classify(PROJECT_LABEL()), PROJECT_NOTE()],
  },
  {
    name: 'single-all-kinds',
    mode: 'single',
    captures: [
      capture('s1', '2026-10-22T00:00:01.000Z', [u('remember all three'), a('ok')]),
    ],
    llm: [
      classify(label('user', 'identity'), PROJECT_LABEL(), label('feedback', 'rule')),
      create('User Note', 'user description', '## Identity\nUser body'),
      PROJECT_NOTE(),
      create('Feedback Note', 'feedback description', '## Rule\nFeedback body'),
    ],
  },
  {
    name: 'general-create',
    mode: 'general',
    captures: [
      capture('s1', '2026-10-22T00:00:01.000Z', [u('first general user'), a('first answer')]),
      capture('s1', '2026-10-22T00:00:02.000Z', [u('second general user'), a('second answer')]),
    ],
    llm: [
      classify(PROJECT_LABEL()), PROJECT_NOTE(), select('create_new', '', 'new project'),
      classify(PROJECT_LABEL()), PROJECT_NOTE(), select('create_new', '', 'new project'),
    ],
  },
  {
    name: 'general-attach',
    mode: 'general',
    seedProjects: [{projectName: 'Existing Project', description: 'existing description', status: 'in_progress'}],
    captures: [
      capture('s1', '2026-10-22T00:00:01.000Z', [u('attach this general fact'), a('ok')]),
    ],
    llm: (ctx: any) => [
      classify(PROJECT_LABEL()), PROJECT_NOTE(),
      select('attach_existing', ctx.seeds[0].projectId, 'attach existing'),
    ],
  },
  {
    name: 'single-long-emoji-name',
    mode: 'single',
    captures: [
      capture('s1', '2026-10-22T00:00:01.000Z', [u('long emoji project name'), a('ok')]),
    ],
    llm: [
      classify(PROJECT_LABEL()),
      create(LONG_EMOJI_NAME, 'long emoji description', '## Summary\nlong emoji body'),
    ],
  },
  {
    name: 'general-attach-long-emoji-name',
    mode: 'general',
    seedProjects: [{projectName: 'Existing Project', description: 'existing description', status: 'in_progress'}],
    captures: [
      capture('s1', '2026-10-22T00:00:01.000Z', [u('attach long emoji fact'), a('ok')]),
    ],
    llm: (ctx: any) => [
      classify(PROJECT_LABEL()),
      create(LONG_EMOJI_NAME, 'long emoji description', '## Summary\nlong emoji body'),
      select('attach_existing', ctx.seeds[0].projectId, 'attach existing'),
    ],
  },
  {
    name: 'single-non-bmp-name',
    mode: 'single',
    captures: [
      capture('s1', '2026-10-22T00:00:01.000Z', [u('non bmp project name'), a('ok')]),
    ],
    llm: [
      classify(PROJECT_LABEL()),
      create(NON_BMP_NAME, 'non bmp description', '## Summary\nnon bmp body'),
    ],
  },
  {
    name: 'capture-duplicate-suppression',
    mode: 'single',
    directCaptures: [
      capture('s1', '2026-10-22T00:00:01.000Z', [u('same user'), a('same answer')]),
      capture('s1', '2026-10-22T00:00:02.000Z', [u('same user'), a('same answer')]),
      capture('s1', '2026-10-22T00:00:03.000Z', [u('new user'), a('new answer')]),
    ],
    llm: [],
  },
  // -- M13 matrix ----------------------------------------------------------
  {
    name: 'matrix-single-afterList-clearSession',
    mode: 'single',
    captures: [capture('s1', '2026-10-22T00:00:01.000Z', [u('cleared after list'), a('a')])],
    timing: 'afterList', action: 'clearSession',
    llm: [classify(PROJECT_LABEL()), PROJECT_NOTE()],
  },
  {
    name: 'matrix-single-classify-capture',
    mode: 'single',
    captures: [capture('s1', '2026-10-22T00:00:01.000Z', [u('first'), a('a')])],
    timing: 'classify', action: 'capture',
    eventCapture: capture('s1', '2026-10-22T00:00:03.000Z', [u('third user'), a('third answer')]),
    llm: [classify(PROJECT_LABEL()), PROJECT_NOTE()],
  },
  {
    name: 'matrix-single-classify-clearSession',
    mode: 'single',
    captures: [capture('s1', '2026-10-22T00:00:01.000Z', [u('cleared during classify'), a('a')])],
    timing: 'classify', action: 'clearSession',
    llm: [classify(PROJECT_LABEL()), PROJECT_NOTE()],
  },
  {
    name: 'matrix-single-createNote-clearSession',
    mode: 'single',
    captures: [capture('s1', '2026-10-22T00:00:01.000Z', [u('cleared during create'), a('a')])],
    timing: 'createNote', action: 'clearSession',
    llm: [classify(PROJECT_LABEL()), PROJECT_NOTE()],
  },
  {
    name: 'matrix-single-beforeMark-clearSession',
    mode: 'single',
    captures: [capture('s1', '2026-10-22T00:00:01.000Z', [u('cleared before mark'), a('a')])],
    timing: 'beforeMark', action: 'clearSession',
    llm: [classify(PROJECT_LABEL()), PROJECT_NOTE()],
  },
  {
    name: 'matrix-general-classify-clearSession',
    mode: 'general',
    captures: [capture('s1', '2026-10-22T00:00:01.000Z', [u('general cleared during classify'), a('a')])],
    timing: 'classify', action: 'clearSession',
    llm: [classify(PROJECT_LABEL()), PROJECT_NOTE(), select('create_new', '', 'new')],
  },
  {
    name: 'matrix-general-createNote-clearSession',
    mode: 'general',
    captures: [capture('s1', '2026-10-22T00:00:01.000Z', [u('general cleared during create'), a('a')])],
    timing: 'createNote', action: 'clearSession',
    llm: [classify(PROJECT_LABEL()), PROJECT_NOTE(), select('create_new', '', 'new')],
  },
  {
    name: 'matrix-general-select-clearProject',
    mode: 'general',
    seedProjects: [{projectName: 'Existing Project', description: 'existing description', status: 'in_progress'}],
    captures: [capture('s1', '2026-10-22T00:00:01.000Z', [u('cleared during select'), a('a')])],
    timing: 'select', action: 'clearProject',
    llm: (ctx: any) => [
      classify(PROJECT_LABEL()), PROJECT_NOTE(),
      select('attach_existing', ctx.seeds[0].projectId, 'attach existing'),
    ],
  },
  {
    name: 'matrix-single-classify-close',
    mode: 'single',
    captures: [capture('s1', '2026-10-22T00:00:01.000Z', [u('close during classify'), a('a')])],
    timing: 'classify', action: 'close',
    llm: [classify(PROJECT_LABEL()), PROJECT_NOTE()],
  },
  {
    name: 'matrix-single-beforeMark-close',
    mode: 'single',
    captures: [capture('s1', '2026-10-22T00:00:01.000Z', [u('close before mark'), a('a')])],
    timing: 'beforeMark', action: 'close',
    llm: [classify(PROJECT_LABEL()), PROJECT_NOTE()],
  },
  {
    name: 'matrix-general-select-close',
    mode: 'general',
    seedProjects: [{projectName: 'Existing Project', description: 'existing description', status: 'in_progress'}],
    captures: [capture('s1', '2026-10-22T00:00:01.000Z', [u('close during select'), a('a')])],
    timing: 'select', action: 'close',
    llm: (ctx: any) => [
      classify(PROJECT_LABEL()), PROJECT_NOTE(),
      select('attach_existing', ctx.seeds[0].projectId, 'attach existing'),
    ],
  },
];

export async function collect(baseline: string) {
  assert.equal(process.version, 'v22.22.0');
  const scratch = mkdtempSync(join(tmpdir(), 'pdrs-heartbeat-indexer-'));
  const home = join(scratch, 'home');
  const oldHome = process.env.HOME;
  mkdirSync(home, {recursive: true});
  process.env.HOME = home;
  const oldDateNow = Date.now;
  const oldDate = globalThis.Date;
  const oldRandom = Math.random;
  const ms = Date.parse(NOW);
  class FrozenDate extends oldDate {
    constructor(...args: any[]) {
      super(...((args.length ? args : [ms]) as [number]));
    }
    static now() {
      return ms;
    }
  }
  (globalThis as any).Date = FrozenDate;
  Date.now = () => ms;
  Math.random = () => 0.25;
  const {EdgeClawMemoryService} = await import(pathToFileURL(join(baseline, ENTRY)).href);
  const observed: any[] = [];
  try {
    for (const spec of cases()) {
      const root = join(scratch, 'cases', spec.name);
      mkdirSync(root, {recursive: true});
      const workspaceDir = spec.mode === 'general' ? join(home, '.pilotdeck') : join(root, 'workspace');
      mkdirSync(workspaceDir, {recursive: true});
      const steps: any[] = [];
      const server = await stub(steps, () => ms);
      const logs: any[] = [];
      const service = new EdgeClawMemoryService({
        workspaceDir,
        rootDir: join(root, 'root'),
        memoryDir: join(root, 'memory'),
        llm: {
          provider: 'p', model: 'm', modelRef: 'p/m',
          baseUrl: server.origin + '/v1', apiKey: 'fixture-key', apiType: 'openai-completions',
        },
        logger: {
          info: (...args: any[]) => logs.push({level: 'info', message: args.map(String)}),
          warn: (...args: any[]) => logs.push({level: 'warn', message: args.map(String)}),
        },
      });
      const seeds = (spec.seedProjects ?? []).map((project: any) =>
        service.repository.getFileMemoryStore().upsertProjectMeta(project));
      const llmSteps = typeof spec.llm === 'function' ? spec.llm({seeds}) : spec.llm;
      steps.push(...llmSteps);

      const captures: any[] = [];
      for (const input of spec.captures ?? []) {
        captures.push({
          input,
          result: service.captureTurn(input.messages, {
            sessionKey: input.sessionKey,
            timestamp: input.timestamp,
            ...(input.source === undefined ? {} : {source: input.source}),
          }),
        });
      }
      const directCaptures: any[] = [];
      for (const input of spec.directCaptures ?? []) {
        const record = service.indexer.captureL0Session({
          sessionKey: input.sessionKey,
          timestamp: input.timestamp,
          messages: input.messages,
          ...(input.source === undefined ? {} : {source: input.source}),
        });
        directCaptures.push({input, record: record ?? null});
      }

      let release: (() => void) | undefined;
      const gate = new Promise<void>((resolve) => (release = resolve));
      let seen: (() => void) | undefined;
      const requestSeen = new Promise<void>((resolve) => (seen = resolve));
      const realFetch = globalThis.fetch;
      let armed = false;
      let done = false;
      (globalThis as any).fetch = async (url: any, init: any) => {
        if (!done && armed) {
          done = true;
          seen!();
          await gate;
        }
        return await realFetch(url, init);
      };
      const doEvent = () => {
        if (spec.action === 'capture') {
          service.captureTurn(spec.eventCapture.messages, {
            sessionKey: spec.eventCapture.sessionKey,
            timestamp: spec.eventCapture.timestamp,
          });
        } else if (spec.action === 'clearSession') {
          service.clearSession(spec.captures[0].sessionKey);
        } else if (spec.action === 'clearProject') {
          service.clear('current_project');
        } else if (spec.action === 'close') {
          service.close();
        }
      };
      const repo = service.repository as any;
      if (spec.timing === 'afterList') {
        const original = repo.listUnindexedL0BySession.bind(repo);
        let fired = false;
        repo.listUnindexedL0BySession = (...args: any[]) => {
          const out = original(...args);
          if (!fired) { fired = true; doEvent(); }
          return out;
        };
      }
      if (spec.timing === 'beforeMark') {
        const original = repo.markL0Indexed.bind(repo);
        let fired = false;
        repo.markL0Indexed = (...args: any[]) => {
          if (!fired) { fired = true; doEvent(); }
          return original(...args);
        };
      }
      if (spec.timing === 'classify') {
        const original = service.extractor.classifyMemoryTurn.bind(service.extractor);
        service.extractor.classifyMemoryTurn = async (...args: any[]) => {
          armed = true;
          return await original(...args);
        };
      }
      if (spec.timing === 'createNote') {
        for (const key of ['createUserMemoryNote', 'createProjectMemoryNote', 'createFeedbackMemoryNote']) {
          const original = (service.extractor as any)[key].bind(service.extractor);
          (service.extractor as any)[key] = async (...args: any[]) => {
            armed = true;
            return await original(...args);
          };
        }
      }
      if (spec.timing === 'select') {
        const original = service.extractor.selectIndexProject.bind(service.extractor);
        service.extractor.selectIndexProject = async (...args: any[]) => {
          armed = true;
          return await original(...args);
        };
      }

      let result: any;
      const pending = spec.directCaptures
        ? Promise.resolve()
        : service.indexer.runHeartbeat({reason: 'manual'}).then(
            (stats: any) => { result = {stats}; },
            (error: any) => { result = {error: {name: error?.name, message: error?.message}}; },
          );
      if (['classify', 'createNote', 'select'].includes(spec.timing)) {
        await requestSeen;
        doEvent();
        release!();
      } else {
        release!();
      }
      await pending;
      globalThis.fetch = realFetch;

      let db: any;
      try {
        const sessionKeys = (spec.captures ?? []).map((c: any) => c.sessionKey);
        const keys = ['lastIndexedAt', ...sessionKeys.map((key: string) => `lastIndexedCursor:${key}`), 'workspaceDir'];
        db = {
          l0: service.repository.listAllL0(),
          traces: service.repository.listRecentIndexTraces(20),
          pipeline: Object.fromEntries(keys.map((key) => [key, service.repository.getPipelineState(key)])),
        };
      } catch (error: any) {
        db = {error: {name: error?.name, message: error?.message}};
      }
      const files = {
        memory: tree(service.memoryDir),
        global: tree((service.repository as any).globalRootDir),
      };
      // Request bodies and headers are part of the observation: a mutation
      // that drops batch messages or truncates recent user messages still
      // hits the same URL and consumes the same scripted responses, so the
      // sequence alone cannot catch it.
      const requests = server.requests.map((request) => ({
        method: request.method,
        url: request.url,
        headers: request.headers,
        body: request.body,
      }));
      await server.close();
      try { service.close(); } catch {}
      observed.push(clean({
        name: spec.name,
        mode: spec.mode,
        timing: spec.timing ?? null,
        action: spec.action ?? null,
        eventCapture: spec.eventCapture ?? null,
        seeds,
        captures,
        directCaptures,
        llmSteps,
        result,
        db,
        files,
        requests,
        logs,
      }, root, home));
      rmSync(root, {recursive: true, force: true});
    }
  } finally {
    (globalThis as any).Date = oldDate;
    Date.now = oldDateNow;
    Math.random = oldRandom;
    if (oldHome === undefined) delete process.env.HOME;
    else process.env.HOME = oldHome;
    rmSync(scratch, {recursive: true, force: true});
  }
  return {cases: observed};
}

function tree(root: string, dir = ''): any[] {
  return readdirSync(join(root, dir), {withFileTypes: true})
    .sort((left, right) => left.name < right.name ? -1 : left.name > right.name ? 1 : 0)
    .flatMap((entry) => {
      const relative = dir ? `${dir}/${entry.name}` : entry.name;
      return entry.isDirectory()
        ? tree(root, relative)
        : [{relativePath: relative, content: readFileSync(join(root, relative), 'utf8')}];
    });
}

function clean(value: any, root: string, home: string): any {
  if (typeof value === 'string') return value.split(home).join('$HOME').split(root).join('$ROOT');
  if (Array.isArray(value)) return value.map((item) => clean(item, root, home));
  if (value && typeof value === 'object') {
    return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, clean(item, root, home)]));
  }
  return value === undefined ? null : value;
}
