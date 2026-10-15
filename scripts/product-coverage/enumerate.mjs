// Offline inventory: parse the immutable baseline, never import its product code.
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { createRequire } from 'node:module';
import { dirname, posix, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const ts = require('../gateway-parity/node_modules/typescript');
export const REF = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';

// Reject unsupported Express receiver/chain shapes; Map.get etc are not routes.
export function assertRouterShapes(sf) {
  const visit = (node, fn) => { fn(node); ts.forEachChild(node, child => visit(child, fn)); };
  const bindings = new Set(['app', 'router', 'route']);
  const declarations = [];
  visit(sf, node => { if (ts.isVariableDeclaration(node) && ts.isIdentifier(node.name)) declarations.push(node); });
  for (let pass = 0; pass <= declarations.length; pass++) {
    const before = bindings.size;
    for (const node of declarations) {
      const init = node.initializer;
      if (init && ((ts.isCallExpression(init) && ['express', 'express.Router'].includes(init.expression.getText(sf)))
          || (ts.isIdentifier(init) && bindings.has(init.text)))) bindings.add(node.name.text);
    }
    if (bindings.size === before) break;
  }
  visit(sf, node => {
    if (!ts.isCallExpression(node) || !ts.isPropertyAccessExpression(node.expression)) return;
    const recv = node.expression.expression, method = node.expression.name.text;
    if (!['get', 'post', 'put', 'patch', 'delete', 'all', 'head', 'options', 'use', 'route'].includes(method)) return;
    if (ts.isIdentifier(recv) && bindings.has(recv.text) && !['app', 'router', 'route'].includes(recv.text)) {
      let parent = node; while (parent && !ts.isFunctionDeclaration(parent)) parent = parent.parent;
      // This exported alternate factory is not mounted by the fixed index.js.
      if (sf.fileName === 'ui/server/routes/update.js' && recv.text === 'restartRouter'
          && parent?.name?.text === 'createUpdateRouter') return;
      throw Error(`unsupported Express receiver: ${sf.fileName}/${recv.text}`);
    }
    if (method === 'route' && ts.isIdentifier(recv) && bindings.has(recv.text)
        && !ts.isStringLiteralLike(node.arguments[0]) && !ts.isArrayLiteralExpression(node.arguments[0]))
      throw Error(`dynamic Express route chain: ${sf.fileName}`);
  });
}

export function enumerate(baseline) {
  const git = (...args) => execFileSync('git', ['-C', baseline, ...args], {
    encoding: 'utf8', maxBuffer: 16 * 1024 * 1024, timeout: 30000, killSignal: 'SIGKILL',
  });
  if (git('rev-parse', `${REF}^{commit}`).trim() !== REF) throw Error('missing fixed baseline');
  const cache = new Map(), hashes = {}, entries = new Map();
  function source(file) {
    if (!cache.has(file)) {
      const text = git('show', `${REF}:${file}`);
      hashes[file] = { sha256: createHash('sha256').update(text).digest('hex') };
      const sf = ts.createSourceFile(file, text, ts.ScriptTarget.Latest, true);
      if (sf.parseDiagnostics.length) throw Error(`unparsed source: ${file}`);
      cache.set(file, sf);
    }
    return cache.get(file);
  }
  const walk = (node, fn) => { fn(node); ts.forEachChild(node, child => walk(child, fn)); };
  const strings = node => !node ? null : ts.isStringLiteralLike(node) ? [node.text]
    : ts.isArrayLiteralExpression(node) ? node.elements.flatMap(item => {
      const value = strings(item); if (!value) throw Error('nonliteral array'); return value;
    }) : null;
  function add(id, file, node) {
    const sf = source(file), line = sf.getLineAndCharacterOfPosition(node.getStart(sf)).line + 1;
    if (!entries.has(id)) entries.set(id, { id, sources: [] });
    const ref = `${file}:${line}`;
    if (!entries.get(id).sources.includes(ref)) entries.get(id).sources.push(ref);
  }
  function anchor(id, file, needle) {
    const sf = source(file), start = sf.text.indexOf(needle);
    if (start < 0) throw Error(`missing semantic anchor: ${id}`);
    add(id, file, { getStart: () => start });
  }
  function comparisons(file, variable, prefix, functionName) {
    const sf = source(file); let count = 0;
    walk(sf, node => {
      if (!ts.isBinaryExpression(node) || !['===', '!=='].includes(node.operatorToken.getText(sf))
          || node.left.getText(sf) !== variable || !ts.isStringLiteralLike(node.right)) return;
      if (functionName) {
        let p = node;
        while (p && !ts.isFunctionDeclaration(p)) p = p.parent;
        if (p?.name?.text !== functionName) return;
      }
      add(prefix + node.right.text, file, node); count++;
    });
    if (!count) throw Error(`no dispatch literals: ${file}/${variable}/${functionName}`);
  }

  const pkg = JSON.parse(source('package.json').text);
  for (const [name, target] of Object.entries(pkg.bin)) anchor(`bin:${name}`, 'package.json', `"${name}": "${target}"`);
  if (Object.keys(pkg.bin).length !== 2 || pkg.bin.pilotdeck !== 'dist/src/cli/pilotdeck.js'
      || pkg.bin['pilotdeck-agent-loop-sidecar'] !== 'dist/src/cli/pilotdeck-agent-loop-sidecar.js') throw Error('bin mapping drift');
  const cli = 'src/cli/pilotdeck.ts', sdk = 'src/cli/sdkCli.ts';
  comparisons(cli, 'command', 'cli:pilotdeck/', 'main');
  comparisons(cli, 'command', 'cli:pilotdeck/cron/', 'handleCronCommand');
  comparisons(cli, 'command', 'cli:pilotdeck/skills/', 'handleSkillsCommand');
  comparisons(cli, 'sub', 'cli:pilotdeck/gateway/', 'main');
  comparisons(sdk, 'subcommand', 'cli:pilotdeck/sessions/', 'runSessionsCommand');
  comparisons(sdk, 'subcommand', 'cli:pilotdeck/settings/', 'runSettingsCommand');
  comparisons('src/cli/commands/gatewaySetup.ts', 'platform', 'cli:pilotdeck/gateway/setup/');
  anchor('cli:pilotdeck/chat/search', 'src/cli/commands/chatSearch.ts', '"search"');
  for (const [id, file, needle] of [
    ['cli:pilotdeck/<stdin-lines>', 'src/adapters/channel/cli/CliChannel.ts', 'createInterface'],
    ['cli:pilotdeck/<argv-as-prompt>', 'src/adapters/channel/cli/CliChannel.ts', '.join(" ")'],
    ['cli:pilotdeck/sessions/<default-list>', sdk, 'argv[0] ?? "list"'],
    ['cli:pilotdeck/settings/<default-get>', sdk, 'argv[0] ?? "get"'],
    ['cli:pilotdeck/gateway/setup/<interactive-or-unknown>', 'src/cli/commands/gatewaySetup.ts', 'createInterface'],
    ['sidecar:stdio', 'src/cli/pilotdeck-agent-loop-sidecar.ts', 'await sidecar.serve()'],
    ['sidecar:factory', 'src/cli/pilotdeck-agent-loop-sidecar.ts', 'PILOTDECK_AGENT_LOOP_FACTORY'],
    ['sidecar:tcp', 'src/cli/pilotdeck-agent-loop-sidecar.ts', 'PILOTDECK_AGENT_LOOP_TCP_PORT'],
  ]) anchor(id, file, needle);
  for (const [id, file, needle] of [
    ['cli:pilotdeck/gateway/<usage-error>', cli, 'Usage: pilotdeck gateway setup'],
    ['cli:pilotdeck/cron/<usage-error>', cli, 'Usage: pilotdeck cron'],
    ['cli:pilotdeck/skills/<usage-error>', cli, 'Usage: pilotdeck skills migrate'],
    ['cli:pilotdeck/chat/<usage-error>', 'src/cli/commands/chatSearch.ts', 'Usage: pilotdeck chat search'],
    ['cli:pilotdeck/sessions/<usage-error>', sdk, 'Usage: pilotdeck sessions'],
    ['cli:pilotdeck/settings/<usage-error>', sdk, 'Usage: pilotdeck settings'],
    ['cli:pilotdeck/tui/<non-tty>', cli, 'pilotdeck tui requires'],
    ['cli:pilotdeck/tui/<unknown-slash>', 'src/adapters/channel/tui/app/TuiApp.tsx', 'Unknown command'],
    ['cli:pilotdeck/tui//permissions/<default>', 'src/adapters/channel/tui/app/TuiApp.tsx', 'if (!sub)'],
    ['cli:pilotdeck/tui//mode/<default>', 'src/adapters/channel/tui/app/TuiApp.tsx', ': "default"'],
  ]) anchor(id, file, needle);
  const tui = 'src/adapters/channel/tui/app/TuiApp.tsx';
  walk(source(tui), node => {
    if (ts.isCaseClause(node) && ts.isStringLiteralLike(node.expression) && node.expression.text.startsWith('/'))
      add(`cli:pilotdeck/tui/${node.expression.text}`, tui, node);
  });
  comparisons(tui, 'sub', 'cli:pilotdeck/tui//permissions/');
  comparisons(tui, 'requestedMode', 'cli:pilotdeck/tui//mode/');
  // Options are entry profiles, including flags omitted from usage/help text.
  for (const file of [cli, sdk, 'src/cli/commands/chatSearch.ts', 'src/cli/commands/gatewaySetup.ts', 'scripts/update-web.mjs']) {
    walk(source(file), node => {
      if (ts.isStringLiteralLike(node) && /^--?[a-z][a-z-]*$/.test(node.text)) add(`cli.option:${file}:${node.text}`, file, node);
    });
  }
  const frames = 'src/gateway/protocol/frames.ts';
  walk(source(frames), node => {
    if (ts.isTypeAliasDeclaration(node) && node.name.text === 'WsGatewayMethod') {
      if (!ts.isUnionTypeNode(node.type)) throw Error('method union drift');
      for (const item of node.type.types) {
        if (!ts.isLiteralTypeNode(item) || !ts.isStringLiteral(item.literal)) throw Error('open method union');
        add(`gateway.method:${item.literal.text}`, frames, item);
      }
    }
  });

  const verbs = new Set(['get', 'post', 'put', 'patch', 'delete', 'all', 'head', 'options']);
  const join = (a, b) => ('/' + a + '/' + b).replace(/\/+/g, '/').replace(/\/$/, '') || '/';
  const visited = new Set();
  function ui(file, prefix) {
    const key = `${file}@${prefix}`; if (visited.has(key)) return; visited.add(key);
    const sf = source(file), imports = new Map();
    assertRouterShapes(sf);
    for (const node of sf.statements) {
      if (!ts.isImportDeclaration(node) || !node.moduleSpecifier.text.startsWith('.')) continue;
      const target = posix.normalize(posix.join(posix.dirname(file), node.moduleSpecifier.text));
      const c = node.importClause;
      if (c?.name) imports.set(c.name.text, target);
      if (c?.namedBindings && ts.isNamedImports(c.namedBindings))
        for (const item of c.namedBindings.elements) imports.set(item.name.text, target);
    }
    function chainedPath(node) {
      if (!ts.isCallExpression(node) || !ts.isPropertyAccessExpression(node.expression)) return null;
      return node.expression.name.text === 'route' ? strings(node.arguments[0]) : chainedPath(node.expression.expression);
    }
    walk(sf, node => {
      if (!ts.isCallExpression(node) || !ts.isPropertyAccessExpression(node.expression)) return;
      const method = node.expression.name.text, recv = node.expression.expression;
      const direct = ts.isIdentifier(recv) && ['app', 'router', 'route'].includes(recv.text);
      if (verbs.has(method)) {
        const paths = direct ? strings(node.arguments[0]) : chainedPath(recv);
        if (direct && !paths) throw Error(`unresolved HTTP route: ${file}:${node.getStart()}`);
        if (paths) for (const p of paths) add(`ui.http:${method.toUpperCase()} ${join(prefix, p)}`, file, node);
      }
      if (method !== 'use' || !direct) return;
      const paths = strings(node.arguments[0]);
      add(`ui.middleware:${file}:${sf.getLineAndCharacterOfPosition(node.getStart()).line + 1}`, file, node);
      for (const arg of node.arguments.slice(paths ? 1 : 0)) {
        const name = ts.isIdentifier(arg) ? arg.text : ts.isCallExpression(arg) && ts.isIdentifier(arg.expression) ? arg.expression.text : null;
        const target = imports.get(name);
        if (target?.includes('/routes/')) for (const p of paths || ['']) ui(target, join(prefix, p));
      }
    });
  }
  ui('ui/server/index.js', '');
  // Runtime capture excludes USE, static fallbacks, transport dispatch and streams.
  const index = 'ui/server/index.js';
  comparisons(index, 'pathname', 'ui.ws:');
  walk(source(index), node => {
    if (ts.isVariableDeclaration(node) && node.name.getText() === 'PROVIDER_REMOVED_PATHS') {
      const paths = strings(node.initializer); if (!paths) throw Error('dynamic removed prefixes');
      paths.forEach(p => add(`ui.middleware-path:${p}`, index, node));
    }
    if (ts.isCallExpression(node) && node.expression.getText(source(index)) === 'pathname.startsWith') {
      const paths = strings(node.arguments[0]);
      if (!paths) throw Error('dynamic WS prefix');
      paths.forEach(p => add(`ui.ws:${p}*`, index, node));
    }
    if (ts.isBinaryExpression(node) && node.left.getText(source(index)) === 'data.type'
        && ts.isStringLiteralLike(node.right) && ['===', '!=='].includes(node.operatorToken.getText(source(index))))
      add(`ui.ws-message:${node.right.text}`, index, node);
  });
  for (const [file, prefix] of [['ui/server/routes/modules.js', 'ui.sse:'], ['ui/server/routes/webUpdate.js', 'ui.sse:'],
    ['ui/server/routes/uploads.js', 'ui.sse:'], ['ui/server/routes/projects.js', 'ui.sse:'], ['ui/server/routes/agent.js', 'ui.sse:'], [index, 'ui.sse:']]) {
    walk(source(file), node => {
      if (ts.isStringLiteralLike(node) && /^(text\/event-stream|application\/x-ndjson)/.test(node.text))
        add(`${prefix}${file}:${source(file).getLineAndCharacterOfPosition(node.getStart()).line + 1}`, file, node);
    });
  }
  const server = 'src/gateway/server/GatewayServer.ts';
  comparisons(server, 'url.pathname', 'gateway.http:ANY ');
  const ws = entries.get('gateway.http:ANY /ws');
  if (!ws) throw Error('missing gateway upgrade path');
  entries.delete(ws.id); ws.id = 'gateway.ws:/ws'; entries.set(ws.id, ws);
  for (const file of ['src/composition/publicHostHttpTransport.ts', 'src/composition/publicApprovalHttpTransport.ts'])
    comparisons(file, 'path', 'gateway.http:POST ');
  for (const [id, file, needle] of [
    ['gateway.http:GET /api/web/projects', 'src/adapters/web/httpRouter.ts', '"/api/web/projects"'],
    ['gateway.http:GET /api/web/projects/:projectKey', 'src/adapters/web/httpRouter.ts', 'PROJECT_RE.exec'],
    ['gateway.http:GET /api/web/projects/:projectKey/files/tree', 'src/adapters/web/httpRouter.ts', '"/files/tree"'],
    ['gateway.http:GET /api/web/projects/:projectKey/files/read', 'src/adapters/web/httpRouter.ts', '"/files/read"'],
    ['gateway.http:POST /api/web/projects/:projectKey/files/write', 'src/adapters/web/httpRouter.ts', '"/files/write"'],
    ['gateway.http:static-fallback', server, 'serveStaticAsset(resolve'],
    ['gateway.http:<unmatched-404>', server, 'response.end("not found")'],
    ['gateway.background:always-on', 'src/cli/ProjectAutomationBundle.ts', 'await generation.alwaysOn.start()'],
    ['gateway.background:cron', 'src/cli/ProjectAutomationBundle.ts', 'await generation.cron.start()'],
    ['ui.background:watcher', index, 'chokidar.watch(rootPath,'],
    ['ui.background:memory-scheduler', index, 'startMemoryScheduler();'],
    ['ui.background:plugins', index, 'startEnabledPluginServers().catch'],
    ['ui.background:session-forwarding', index, 'registerAlwaysOnNotificationForwarding(connectedClients, createBackgroundSessionForwarder({'],
  ]) anchor(id, file, needle);
  const loaders = 'src/adapters/channel/loadEnabledChannels.ts';
  walk(source(loaders), node => {
    if (ts.isVariableDeclaration(node) && node.name.getText() === 'CHANNEL_LOADERS') {
      if (!ts.isObjectLiteralExpression(node.initializer)) throw Error('dynamic channel registry');
      for (const member of node.initializer.properties) add(`gateway.channel:${member.name.getText()}`, loaders, member);
    }
  });
  for (const name of ['feishu', 'weixin', 'qq', 'wecom']) anchor(`gateway.channel:${name}`, 'src/cli/ChannelAdapterBundle.ts', name);
  for (const [name, file] of [['api-server', 'ApiServer'], ['webhook', 'Webhook'], ['wecom-callback', 'WeComCallback'], ['sms', 'Sms']]) {
    const path = `src/adapters/channel/${name}/${file}Channel.ts`;
    walk(source(path), node => {
      if (ts.isStringLiteralLike(node) && /^(\/health|\/v1\/|\/webhooks\/)/.test(node.text)) add(`gateway.channel-http:${name}:${node.text}`, path, node);
    });
    anchor(`gateway.channel-http:${name}:listener-and-method-dispatch`, path,
      name === 'wecom-callback' ? 'this.server = http.createServer(' : 'this.server = createServer(');
  }
  for (const [id, file, needle] of [
    ['gateway.channel-http:webhook:POST /webhooks/:name', 'src/adapters/channel/webhook/WebhookChannel.ts', 'const match ='],
    ['gateway.channel-http:api-server:OPTIONS *', 'src/adapters/channel/api-server/ApiServerChannel.ts', '"OPTIONS"'],
    ['gateway.channel-http:wecom-callback:GET *', 'src/adapters/channel/wecom-callback/WeComCallbackChannel.ts', '"GET"'],
    ['gateway.channel-http:wecom-callback:POST *', 'src/adapters/channel/wecom-callback/WeComCallbackChannel.ts', '"POST"'],
    ['gateway.channel-http:sms:POST <webhookPath; default=/sms/incoming>', 'src/adapters/channel/sms/SmsChannel.ts', 'url.pathname !== this.path || req.method !== "POST"'],
    ['desktop.backend-dependency:process-tree', 'apps/desktop/src/main.ts', 'ui/server/utils/processTree.js'],
    ['desktop.backend-dependency:release-service', 'apps/desktop/src/main.ts', 'ui/server/services/releaseService.js'],
    ['desktop.backend-dependency:pilotdeck-config', 'apps/desktop/src/main.ts', 'ui/server/services/pilotdeckConfig.js'],
  ]) anchor(id, file, needle);

  const desktopFiles = git('ls-tree', '-r', '--name-only', REF, 'apps/desktop/src').trim().split('\n').filter(f => /\.ts$/.test(f));
  for (const file of desktopFiles) {
    const sf = source(file);
    walk(sf, node => {
      if (ts.isStringLiteralLike(node) && node.text.startsWith('pilotdeck:')) add(`desktop.ipc:${node.text}`, file, node);
      if (ts.isMethodDeclaration(node) && node.parent.name?.text === 'RuntimeManager') add(`desktop.runtime:${node.name.getText()}`, file, node);
      if (ts.isCallExpression(node) && ts.isPropertyAccessExpression(node.expression)) {
        const expr = node.expression.getText(sf);
        if (/^(app\.(on|once)|protocol\.handle)$/.test(expr)) {
          const names = strings(node.arguments[0]); if (!names) throw Error('dynamic desktop lifecycle');
          names.forEach(name => add(`desktop.lifecycle:${name}`, file, node));
        }
      }
    });
  }
  const commands = 'ui/shared/desktopCommands.d.ts';
  walk(source(commands), node => {
    if (ts.isTypeAliasDeclaration(node) && node.name.text === 'DesktopCommand')
      walk(node.type, item => { if (ts.isStringLiteralLike(item)) add(`desktop.command:${item.text}`, commands, item); });
  });
  for (const [id, file, needle] of [
    ['desktop.local:appearance-storage', 'apps/desktop/src/appearanceStorage.ts', 'export'],
    ['desktop.local:file-picker', 'apps/desktop/src/filePicker.ts', 'export'],
    ['desktop.local:update-controller', 'apps/desktop/src/updates.ts', 'export'],
    ['desktop.local:tray', 'apps/desktop/src/desktopTray.ts', 'export'],
    ['desktop.local:renderer-recovery', 'apps/desktop/src/rendererRecovery.ts', 'export'],
    ['desktop.local:platform-chrome', 'apps/desktop/src/windowChrome.ts', 'export'],
  ]) anchor(id, file, needle);
  const rows = [...entries.values()].sort((a, b) => a.id.localeCompare(b.id, 'en'));
  for (const [prefix, expected] of [['gateway.method:', 101], ['ui.http:', 263], ['gateway.channel:', 19], ['bin:', 2]]) {
    if (rows.filter(row => row.id.startsWith(prefix)).length !== expected) throw Error(`baseline inventory count mismatch: ${prefix}`);
  }
  return { baseline: REF, sources: Object.fromEntries(Object.entries(hashes).sort()), entries: rows };
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const baseline = process.argv[2] || resolve(dirname(fileURLToPath(import.meta.url)), '../../resources/PilotDeck');
    process.stdout.write(JSON.stringify(enumerate(baseline), null, 2) + '\n');
  } catch (error) { console.error(error.message); process.exitCode = 1; }
}
