// Execute frozen source without importing its server entrypoints or listening.
// These responses are offline proof outputs only; never used by a runtime.
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { baseline, requireUI } from './env.mjs';

const ts = requireUI('typescript');
const parse = (file, source) => ts.createSourceFile(file, source, ts.ScriptTarget.Latest, true, ts.ScriptKind.JS);
const compile = source => ts.transpileModule(source, { compilerOptions: {
  module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022,
} }).outputText;
function declarations(ast, names) {
  return names.map(name => {
    const node = ast.statements.find(node => node.name?.text === name);
    assert(node, `Frozen declaration ${name}`);
    return node.getText(ast).replace(/^export /, '');
  }).join('\n');
}
export async function proveReplacementBaselineStale() {
  const profile = requireUI('yaml').parse(await readFile(resolve(baseline,
    '../products/pilotdeck-staffdeck-sop/profiles/replacement-knowledge.yaml'), 'utf8'));
  assert.equal(profile.modules.knowledge.implementationId, 'replacement.knowledge');
  assert.equal(profile.modules.knowledge.frontendModule, 'fixture.knowledge-search');
  assert.equal(profile.webui.staffdeckCopy, undefined);
  const moduleSource = await readFile(resolve(baseline, 'server/routes/modules.js'), 'utf8');
  const ast = parse('modules.js', moduleSource), handlers = {};
  const visit = node => {
    if (ts.isCallExpression(node) && node.expression.getText(ast) === 'route.post'
      && ['/knowledge/query', '/knowledge/citation'].includes(node.arguments[0]?.text)) {
      handlers[node.arguments[0].text] = node.arguments[1].getText(ast);
    }
    ts.forEachChild(node, visit);
  };
  visit(ast);
  assert.equal(Object.keys(handlers).length, 2);
  const copyAST = parse('staffdeck-copy.js', await readFile(resolve(baseline, 'server/routes/staffdeck-copy.js'), 'utf8'));
  const copyCode = declarations(copyAST, ['CopyError', 'requiredText', 'configuredText', 'bindingFor',
    'officialRequest', 'visibleDirectory', 'verifyFormalUser', 'verifyStaffDeckKnowledgeBinding']);
  let fetches = 0;
  const noFetch = () => { fetches++; throw new Error('Offline proof reached an unexpected network operation'); };
  const verify = new Function('exports', 'fetch', 'CONTRACT', 'moduleUpstreamSignal',
    compile(copyCode) + '\nreturn verifyStaffDeckKnowledgeBinding;')({}, noFetch, 'staffdeck.enterprise-copy/v1', noFetch);
  const identityCode = declarations(ast, ['hasKnowledgeIdentity', 'withTrustedKnowledgeIdentity', 'isRecord', 'knowledgeHeaders']);
  const observations = [];
  for (const [path, source] of Object.entries(handlers)) for (const seedIdentity of [false, true]) {
    const config = structuredClone(profile);
    // A proposed orchestration-only seed cannot satisfy the next frozen guard.
    if (seedIdentity) Object.assign(config.modules.knowledge, { tenantId: 'offline-tenant', actorUserId: 'offline-actor' });
    const handler = new Function('exports', 'configFor', 'verifyStaffDeckKnowledgeBinding', 'fetch', 'moduleUpstreamSignal',
      compile(identityCode + `\nconst handler = ${source};`) + '\nreturn handler;')({}, () => config, verify, noFetch, noFetch);
    let status = 200, body;
    const res = { status(value) { status = value; return this; }, json(value) { body = value; return this; } };
    await handler({ body: path.endsWith('query') ? { query: 'replacement proof' } : { chunkId: 'replacement-citation-001' },
      user: { id: 1 } }, res);
    assert.equal(status, 501);
    assert.equal(body.error.code, seedIdentity ? 'COPY_UNAVAILABLE' : 'MODULE_IDENTITY_UNAVAILABLE');
    assert.equal(fetches, 0, 'Frozen host rejects before independent module call');
    observations.push({ path, seedIdentity, status, body, upstreamCalls: fetches });
  }
  // Run the unedited independent fixture handler directly. Capture createServer
  // and listen in memory; do not bind, connect, spawn, or write runtime evidence.
  const fixtureSource = await readFile(resolve(baseline,
    '../products/pilotdeck-staffdeck-sop/fixtures/replacement-knowledge-runtime.mjs'), 'utf8');
  const fixtureAST = parse('replacement-knowledge-runtime.mjs', fixtureSource);
  const fixtureCode = fixtureAST.statements.filter(node => !ts.isImportDeclaration(node)).map(node => node.getText(fixtureAST)).join('\n');
  let fixtureHandler;
  new Function('createServer', 'appendFile', 'process', 'Buffer', 'console', fixtureCode)(handler => {
    fixtureHandler = handler;
    return { listen(port, host) { assert.equal(host, '127.0.0.1'); assert.equal(port, 18093); } };
  }, () => { throw new Error('Offline proof must not write replacement evidence'); }, { env: {} }, Buffer, { log() {} });
  const fixtureResults = [];
  for (const operation of ['query', 'resolve_citation']) {
    const envelope = { messageId: 'offline-proof', payload: { operation,
      input: { query: 'replacement proof', chunkId: 'replacement-citation-001', limit: 3 } } };
    let status, text;
    await fixtureHandler({ method: 'POST', url: '/v2/module/call',
      async *[Symbol.asyncIterator]() { yield Buffer.from(JSON.stringify(envelope)); } }, {
      writeHead(value) { status = value; return this; }, end(value) { text = value; },
    });
    const reply = JSON.parse(text);
    assert.equal(status, 200); assert.equal(reply.inReplyTo, envelope.messageId); assert.equal(reply.ok, true);
    assert.equal(reply.payload.result.implementation, 'replacement-knowledge-runtime');
    if (operation === 'query') assert.equal(reply.payload.result.chunks[0].content,
      'Replacement runtime verified this query through an independent module process.');
    fixtureResults.push({ operation, status, reply });
  }
  return { status: 'PASS', scope: 'OFFLINE_FROZEN_SOURCE_ONLY: no listen/spawn/browser; not runtime evidence',
    observations, fixtureResults, hostUpstreamCalls: fetches,
    diagnosis: 'Frozen replacement profile lacks mandatory identity; adding identity still requires the real StaffDeck copy bridge, absent from the independent replacement fixture.' };
}
if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const { validateBaselineStale } = await import('./baseline-stale.mjs');
  await validateBaselineStale();
  console.log(JSON.stringify(await proveReplacementBaselineStale(), null, 2));
}
