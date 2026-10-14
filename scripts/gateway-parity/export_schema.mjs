#!/usr/bin/env node
// A3 · gateway 协议 JSON Schema 导出（可重复、字节稳定）
//
// 用法：
//   node scripts/gateway-parity/export_schema.mjs                 # 生成/覆写 schema/ 下两份产物（只生成一次）
//   node scripts/gateway-parity/export_schema.mjs --check         # 双生成自比对 + 与已提交产物字节比对
//   node scripts/gateway-parity/export_schema.mjs --baseline <dir>  # 指定基线（默认 $PDRS_BASELINE，再逐级向上找 resources/PilotDeck）
//
// 产物：
//   schema/gateway-protocol.schema.json   根 = $ref #/definitions/WsGatewayFrame；definitions 含帧可达类型
//                                         + Gateway 接口全部载荷类型（按 method 的 Input/Result，
//                                         含 abortTurn/resumeSession 等内联参数/返回对象——经虚拟别名按方法导出）
//   schema/gateway-methods.snapshot.json  方法清单快照（CI 没有基线目录时的一致性基准）
//
// 可重复性：同一输入（基线源码 × 本目录钉死的生成器版本）两次生成字节一致。
//   序列化前递归排序对象键；--check 里另做双生成自比对（非 --check 只生成一次）。
//
// 已知生成器缺口与对策（R4：生成器输出只是基准，错了改生成配置，不改基线）：
//   * ts-json-schema-generator@2.9.0 不认 `import("...").X`（ImportTypeNode，frames.ts:18）——
//     用官方 createParser augmentor 钩子补一个 ImportTypeNodeParser，沿 checker 解析到真实声明。
//   * types.ts 对 web/client/protocol.js 的 12 个 `export type X = 导入的X` 影子别名会让
//     findNamedNode 撞名（MultipleDefinitionsError）——根节点不走名字查找，直接用 checker
//     解析成唯一声明节点（createSchemaFromNodes），影子别名跳过本层取 origin。
//   * Gateway 接口方法签名里的内联载荷（如 abortTurn 的 `{ sessionKey: string; … }`）没有名字，
//     名字查找拿不到——在程序里给 types.ts 虚拟追加「按方法导出的别名声明」（CompilerHost 注入，
//     不落盘、不改基线、不进 sha256），再把这些别名作为根节点一并生成。

import { createHash } from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(here, '..', '..');
const genPkg = JSON.parse(fs.readFileSync(path.join(here, 'node_modules', 'ts-json-schema-generator', 'package.json'), 'utf8'));
const tsPkg = JSON.parse(fs.readFileSync(path.join(here, 'node_modules', 'typescript', 'package.json'), 'utf8'));
const {
  DEFAULT_CONFIG, createParser, createFormatter, SchemaGenerator,
} = await import(path.join(here, 'node_modules', 'ts-json-schema-generator', 'dist', 'index.js'));
const ts = (await import(path.join(here, 'node_modules', 'typescript', 'lib', 'typescript.js'))).default;

// ── 参数 ──
const argv = process.argv.slice(2);
const checkMode = argv.includes('--check');
const baselineFlagIdx = argv.indexOf('--baseline');
const baselineFlag = baselineFlagIdx >= 0 ? argv[baselineFlagIdx + 1] : undefined;

function resolveBaseline() {
  const candidates = [baselineFlag, process.env.PDRS_BASELINE].filter(Boolean);
  // 从本仓根逐级向上找 <祖先>/resources/PilotDeck——适配任意深度的 worktree（含 _build/wt-*）
  for (let dir = repoRoot; ; dir = path.dirname(dir)) {
    candidates.push(path.join(dir, 'resources', 'PilotDeck'));
    if (path.dirname(dir) === dir) break;
  }
  for (const dir of candidates) {
    if (fs.existsSync(path.join(dir, 'src', 'gateway', 'protocol', 'frames.ts'))) return path.resolve(dir);
  }
  throw new Error(`找不到基线（需要 <基线>/src/gateway/protocol/frames.ts）。候选：${candidates.join(' ; ')}
用 --baseline <dir> 或环境变量 PDRS_BASELINE 指定。`);
}
const baseline = resolveBaseline();
const protoDir = path.join(baseline, 'src', 'gateway', 'protocol');
const framesPath = path.join(protoDir, 'frames.ts');
const typesPath = path.join(protoDir, 'types.ts');
const versionPath = path.join(protoDir, 'version.ts');

const sha256 = (p) => createHash('sha256').update(fs.readFileSync(p)).digest('hex');
const read = (p) => fs.readFileSync(p, 'utf8');
// 每个源文件只算一次哈希（doc 与 snapshot 共用；此前两处各算一遍）
const sourceHashes = {
  'frames.ts': sha256(framesPath),
  'types.ts': sha256(typesPath),
  'version.ts': sha256(versionPath),
};

// ── 基线 AST 提取（frames.ts / types.ts / version.ts）──
function parseFile(p) {
  return ts.createSourceFile(p, read(p), ts.ScriptTarget.ES2022, true, ts.ScriptKind.TS);
}
const framesSF = parseFile(framesPath);
const typesSF = parseFile(typesPath);
const versionSF = parseFile(versionPath);

// version.ts: export const PILOTDECK_GATEWAY_PROTOCOL_VERSION = "1.2";
let protocolVersion = null;
for (const st of versionSF.statements) {
  if (ts.isVariableStatement(st)) {
    for (const d of st.declarationList.declarations) {
      if (ts.isIdentifier(d.name) && d.name.text === 'PILOTDECK_GATEWAY_PROTOCOL_VERSION' && d.initializer && ts.isStringLiteral(d.initializer)) {
        protocolVersion = d.initializer.text;
      }
    }
  }
}
if (!protocolVersion) throw new Error('version.ts 里找不到 PILOTDECK_GATEWAY_PROTOCOL_VERSION 字符串常量');

// frames.ts: WsGatewayMethod 联合（有序、逐字），记录行锚点
let methodList = null;
let methodAnchor = null;
let wsGatewayFrameDecl = null;
const frameTypeConsts = []; // 六种帧的 type 字面量，按声明序
const wsClientNameLiterals = [];
for (const st of framesSF.statements) {
  if (!ts.isTypeAliasDeclaration(st)) continue;
  if (st.name.text === 'WsGatewayMethod' && st.type && ts.isUnionTypeNode(st.type)) {
    methodList = st.type.types.map((t) => {
      if (!ts.isLiteralTypeNode(t) || !ts.isStringLiteral(t.literal)) throw new Error(`WsGatewayMethod 成员不是字符串字面量: ${t.getText()}`);
      return t.literal.text;
    });
    methodAnchor = [framesSF.getLineAndCharacterOfPosition(st.getStart(framesSF)).line + 1,
      framesSF.getLineAndCharacterOfPosition(st.getEnd()).line + 1];
  }
  if (st.name.text === 'WsGatewayFrame') wsGatewayFrameDecl = st;
  if (st.name.text === 'GatewayWsClientName' && st.type && ts.isUnionTypeNode(st.type)) {
    wsClientNameLiterals.push(...st.type.types
      .filter((t) => ts.isLiteralTypeNode(t) && ts.isStringLiteral(t.literal)).map((t) => t.literal.text));
  }
  if (/^Ws(HelloFrame|HelloOk|RequestFrame|ResponseFrame|EventFrame|NotificationFrame)$/.test(st.name.text)) {
    const inner = ts.isTypeLiteralNode(st.type) ? st.type
      : ts.isUnionTypeNode(st.type) ? st.type.types.find(ts.isTypeLiteralNode) : null;
    const typeProp = inner?.members.find((m) => ts.isPropertySignature(m) && m.name.getText() === 'type');
    const lit = typeProp && ts.isLiteralTypeNode(typeProp.type) && ts.isStringLiteral(typeProp.type.literal) ? typeProp.type.literal.text : null;
    if (lit) frameTypeConsts.push(lit);
  }
}
if (!methodList || !wsGatewayFrameDecl) throw new Error('frames.ts 里找不到 WsGatewayMethod / WsGatewayFrame');
if (!wsClientNameLiterals.length) throw new Error('frames.ts 里找不到 GatewayWsClientName 字面量联合');

// types.ts: import 表 / 本地声明 / 影子别名 / GatewayChannelKey 字面量 / GatewayMode 字面量
const importAlias = new Map();
for (const st of typesSF.statements) {
  if (ts.isImportDeclaration(st) && st.importClause?.namedBindings && ts.isNamedImports(st.importClause.namedBindings)) {
    for (const el of st.importClause.namedBindings.elements) importAlias.set(el.name.text, el.propertyName?.text ?? el.name.text);
  }
}
const shadowed = new Map(); // name -> 内层 import 本地名（export type X = 导入的X 且 X 与源名同名）
for (const st of typesSF.statements) {
  if (ts.isTypeAliasDeclaration(st) && st.type && ts.isTypeReferenceNode(st.type) && ts.isIdentifier(st.type.typeName)
    && importAlias.has(st.type.typeName.text) && st.name.text === importAlias.get(st.type.typeName.text)) {
    shadowed.set(st.name.text, st.type.typeName.text);
  }
}
function literalsOf(typeAliasName, sf = typesSF) {
  for (const st of sf.statements) {
    if (ts.isTypeAliasDeclaration(st) && st.name.text === typeAliasName && st.type && ts.isUnionTypeNode(st.type)) {
      return st.type.types.filter((t) => ts.isLiteralTypeNode(t) && ts.isStringLiteral(t.literal)).map((t) => t.literal.text);
    }
  }
  throw new Error(`${sf.fileName} 里找不到 ${typeAliasName} 字面量联合`);
}
const channelKeyLiterals = literalsOf('GatewayChannelKey');
const gatewayModeLiterals = literalsOf('GatewayMode');

// ── Gateway 接口 → 按方法导出的内联载荷别名 ──
// 只处理「类型没有名字、名字查找拿不到」的内联对象：参数类型是 TypeLiteral、或返回类型（拆掉
// Promise 后）含 TypeLiteral 的方法，各生成一条 `export type Gateway<Method>Input/Result = <原文>`。
// 命名类型（GatewaySubmitTurnInput 等）本就走名字查找，不在此列。文本逐字取自基线 AST，
// 基线改内联形状 → 别名跟着变（不可能漂移）。
const unwrapParens = (n) => (ts.isParenthesizedTypeNode(n) ? unwrapParens(n.type) : n);
const isPromise = (n) => ts.isTypeReferenceNode(n) && ts.isIdentifier(n.typeName) && n.typeName.text === 'Promise'
  && (n.typeArguments?.length ?? 0) === 1;
const unwrapPromise = (n) => (isPromise(n) ? n.typeArguments[0] : n);

const localDeclNames = new Set();
for (const st of typesSF.statements) {
  if (ts.isTypeAliasDeclaration(st) || ts.isInterfaceDeclaration(st) || ts.isEnumDeclaration(st)) localDeclNames.add(st.name.text);
}
function isLocalDecl(name) { return localDeclNames.has(name); }

const methodPayloadAliases = []; // { method, input?, result? }（接口方法名 → 别名，按 method 排序）
const aliasDecls = []; // 追加进虚拟 types.ts 的声明文本（排序，保证字节稳定）
{
  const pascal = (s) => s[0].toUpperCase() + s.slice(1);
  const taken = new Set([...localDeclNames, ...importAlias.keys(), ...shadowed.keys()]);
  for (const st of typesSF.statements) {
    if (!ts.isInterfaceDeclaration(st) || st.name.text !== 'Gateway') continue;
    for (const member of st.members) {
      if (!ts.isMethodSignature(member) || !ts.isIdentifier(member.name)) continue;
      const method = member.name.text;
      const entry = { method };
      // 参数：取第一个参数，仅当其类型本身就是内联 TypeLiteral（多参/命名/散参不是 wire 载荷形状）
      const paramType = member.parameters[0]?.type && unwrapParens(member.parameters[0].type);
      let inputText = null;
      if (paramType && ts.isTypeLiteralNode(paramType)) inputText = paramType.getText(typesSF);
      // 返回：联合成员各自拆一层 Promise，只要出现内联 TypeLiteral 就把整段原文导出
      const ret = member.type;
      let resultText = null;
      if (ret) {
        const candidates = (ts.isUnionTypeNode(ret) ? ret.types : [ret]).map(unwrapPromise);
        if (candidates.some((c) => ts.isTypeLiteralNode(unwrapParens(c)))) {
          resultText = candidates.map((c) => c.getText(typesSF)).join(' | ');
        }
      }
      if (inputText) entry.input = `Gateway${pascal(method)}Input`;
      if (resultText) entry.result = `Gateway${pascal(method)}Result`;
      if (entry.input || entry.result) methodPayloadAliases.push(entry);
      if (inputText) aliasDecls.push(`export type ${entry.input} = ${inputText};`);
      if (resultText) aliasDecls.push(`export type ${entry.result} = ${resultText};`);
    }
  }
  for (const name of methodPayloadAliases.flatMap((e) => [e.input, e.result].filter(Boolean))) {
    if (taken.has(name)) throw new Error(`载荷别名 ${name} 与基线已有类型撞名`);
    taken.add(name);
  }
  methodPayloadAliases.sort((a, b) => (a.method < b.method ? -1 : a.method > b.method ? 1 : 0));
  aliasDecls.sort();
}
const aliasNameSet = new Set(methodPayloadAliases.flatMap((e) => [e.input, e.result].filter(Boolean)));

// 虚拟注入的 types.ts：原文 + 按方法别名（只存在于本进程的 CompilerHost，绝不落盘）
const augmentedTypesText = `${read(typesPath)}
// pdrs-export: 以下别名由 export_schema.mjs 从 Gateway 接口按方法导出（虚拟注入，不落盘）
${aliasDecls.join('\n')}
`;

// Gateway 接口 → 载荷类型名（Input/Result 等命名类型；全局类型 Promise/AsyncIterable/AbortSignal 只拆参不进根）
const EXTRA_ROOTS = ['GatewaySubmitTurnInput', 'GatewayEvent', 'GatewayServerInfo', 'GatewayChannelKey'];
const wireNames = new Set(EXTRA_ROOTS);
function collectTypeRefs(node) {
  if (!node) return;
  if (ts.isTypeReferenceNode(node)) {
    if (ts.isIdentifier(node.typeName) && (importAlias.has(node.typeName.text) || shadowed.has(node.typeName.text))) {
      wireNames.add(node.typeName.text);
    } else if (ts.isIdentifier(node.typeName) && isLocalDecl(node.typeName.text)) wireNames.add(node.typeName.text);
    for (const ta of node.typeArguments ?? []) collectTypeRefs(ta);
    return;
  }
  if (ts.isUnionTypeNode(node) || ts.isIntersectionTypeNode(node)) return node.types.forEach(collectTypeRefs);
  if (ts.isArrayTypeNode(node)) return collectTypeRefs(node.elementType);
  if (ts.isTypeLiteralNode(node)) return node.members.forEach(collectTypeRefs);
  if (ts.isTupleTypeNode(node)) return node.elements.forEach(collectTypeRefs);
  if (ts.isIndexedAccessTypeNode(node)) { collectTypeRefs(node.objectType); collectTypeRefs(node.indexType); return; }
  if (ts.isParenthesizedTypeNode(node) || ts.isTypeOperatorNode(node)) return collectTypeRefs(node.type);
  if (ts.isPropertySignature(node)) return collectTypeRefs(node.type);
  if (ts.isMethodSignature(node)) { node.parameters.forEach((p) => collectTypeRefs(p.type)); collectTypeRefs(node.type); return; }
  if (ts.isConditionalTypeNode(node)) { collectTypeRefs(node.checkType); collectTypeRefs(node.extendsType); collectTypeRefs(node.trueType); collectTypeRefs(node.falseType); return; }
  if (ts.isMappedTypeNode(node)) return collectTypeRefs(node.type);
}
for (const st of typesSF.statements) {
  if (ts.isInterfaceDeclaration(st) && st.name.text === 'Gateway') st.members.forEach(collectTypeRefs);
}

// ── ts-json-schema-generator 组装 ──
class ImportTypeNodeParser {
  constructor(typeChecker, childNodeParser) {
    this.typeChecker = typeChecker;
    this.childNodeParser = childNodeParser;
  }
  supportsNode(node) {
    return node.kind === ts.SyntaxKind.ImportType;
  }
  createType(node, context, reference) {
    if (!node.qualifier) throw new Error('unsupported import() type without qualifier');
    let symbol = this.typeChecker.getSymbolAtLocation(node.qualifier);
    for (let i = 0; i < 10 && symbol; i++) {
      const d = symbol.declarations?.[0];
      if (d && (ts.isExportSpecifier(d) || ts.isImportSpecifier(d))) symbol = this.typeChecker.getAliasedSymbol(symbol);
      else break;
    }
    const decl = symbol?.declarations?.find((d) => ts.isTypeAliasDeclaration(d) || ts.isInterfaceDeclaration(d)
      || ts.isEnumDeclaration(d) || ts.isClassDeclaration(d) || ts.isTypeLiteralNode(d));
    if (!decl) throw new Error(`cannot resolve import() type: ${node.getText()}`);
    return this.childNodeParser.createType(decl, context, reference);
  }
}

const tmpTsconfigDir = fs.mkdtempSync(path.join(os.tmpdir(), 'pdrs-schema-tsconfig-'));
const tmpTsconfig = path.join(tmpTsconfigDir, 'tsconfig.json');

const generatorConfig = {
  ...DEFAULT_CONFIG,
  skipTypeCheck: true, // 只需要类型解析；基线全仓的类型诊断与本任务无关
  functions: 'fail', // 线上帧不可能带函数；可达类型里出现函数应当报错而不是静默吞掉
};

// 自建 program：CompilerHost 给 types.ts 注入虚拟别名文本（其余文件照常读盘）
function buildProgram() {
  const cfgRaw = fs.readFileSync(tmpTsconfig, 'utf8');
  const parsed = ts.parseJsonConfigFileContent(
    ts.parseConfigFileTextToJson(tmpTsconfig, cfgRaw).config, ts.sys, tmpTsconfigDir,
  );
  const options = { ...parsed.options, noEmit: true };
  const augmented = new Map([[typesPath, augmentedTypesText]]);
  const host = ts.createCompilerHost(options, true);
  const origFileExists = host.fileExists.bind(host);
  const origReadFile = host.readFile.bind(host);
  const origGetSourceFile = host.getSourceFile.bind(host);
  host.fileExists = (f) => augmented.has(f) || origFileExists(f);
  host.readFile = (f) => (augmented.has(f) ? augmented.get(f) : origReadFile(f));
  host.getSourceFile = (fileName, languageVersionOrOptions, onError, shouldCreateDiagnostics) => (augmented.has(fileName)
    ? ts.createSourceFile(fileName, augmented.get(fileName), languageVersionOrOptions, true)
    : origGetSourceFile(fileName, languageVersionOrOptions, onError, shouldCreateDiagnostics));
  return ts.createProgram({ rootNames: [framesPath], options, host });
}

function generate() {
  const program = buildProgram();
  const checker = program.getTypeChecker();
  const parser = createParser(program, generatorConfig, (chain) => {
    chain.addNodeParser(new ImportTypeNodeParser(checker, chain));
  });
  const generator = new SchemaGenerator(program, parser, createFormatter(generatorConfig), generatorConfig);

  // 根节点：不走名字查找（影子别名会撞名），直接用 checker 解析成唯一声明节点
  const typesSFInProgram = program.getSourceFile(typesPath);
  const typesModule = checker.getSymbolAtLocation(typesSFInProgram);
  const exportSymbols = new Map();
  for (const [name, sym] of typesModule?.exports ?? []) exportSymbols.set(name, sym);
  const importSymbols = new Map();
  typesSFInProgram.forEachChild((st) => {
    if (ts.isImportDeclaration(st) && st.importClause?.namedBindings && ts.isNamedImports(st.importClause.namedBindings)) {
      for (const el of st.importClause.namedBindings.elements) {
        const sym = checker.getSymbolAtLocation(el.name);
        if (sym) importSymbols.set(el.name.text, sym);
      }
    }
  });
  const follow = (symbol) => {
    let s = symbol;
    for (let i = 0; i < 10 && s; i++) {
      const d = s.declarations?.[0];
      if (d && (ts.isExportSpecifier(d) || ts.isImportSpecifier(d))) s = checker.getAliasedSymbol(s);
      else break;
    }
    return s;
  };
  const declFor = (name) => {
    if (shadowed.has(name)) {
      const origin = follow(importSymbols.get(shadowed.get(name)));
      const decl = origin?.declarations?.find((d) => ts.isTypeAliasDeclaration(d) || ts.isInterfaceDeclaration(d) || ts.isEnumDeclaration(d));
      if (decl) return decl;
    }
    const target = follow(exportSymbols.get(name) ?? importSymbols.get(name));
    return target?.declarations?.find((d) => ts.isTypeAliasDeclaration(d) || ts.isInterfaceDeclaration(d)
      || ts.isEnumDeclaration(d) || ts.isClassDeclaration(d)) ?? null;
  };
  const payloadNodes = [];
  for (const name of [...wireNames].sort()) {
    const decl = declFor(name);
    if (!decl) throw new Error(`载荷类型 ${name} 解析不到声明节点`);
    payloadNodes.push(decl);
  }
  const framesModule = checker.getSymbolAtLocation(program.getSourceFile(framesPath));
  const wsFrameDecl = follow(framesModule?.exports?.get('WsGatewayFrame'))?.declarations?.find(ts.isTypeAliasDeclaration);
  if (!wsFrameDecl) throw new Error('WsGatewayFrame 解析不到声明节点');

  // 虚拟别名作为根节点（按方法导出的内联载荷）
  const aliasNodes = [];
  typesSFInProgram.forEachChild((st) => {
    if (ts.isTypeAliasDeclaration(st) && aliasNameSet.has(st.name.text)) aliasNodes.push(st);
  });
  if (aliasNodes.length !== aliasNameSet.size) {
    const missing = [...aliasNameSet].filter((n) => !aliasNodes.some((n2) => n2.name.text === n));
    throw new Error(`虚拟别名未全部进入程序: ${missing.join(', ')}`);
  }
  aliasNodes.sort((a, b) => (a.name.text < b.name.text ? -1 : 1));

  const rootTypes = ['WsGatewayFrame', ...[...wireNames].sort(), ...aliasNodes.map((n) => n.name.text)];
  const generated = generator.createSchemaFromNodes([wsFrameDecl, ...payloadNodes, ...aliasNodes]);
  const definitions = generated.definitions ?? generated.$defs;
  if (!definitions || !definitions.WsGatewayFrame || !definitions.WsGatewayMethod) {
    throw new Error('生成结果缺 WsGatewayFrame / WsGatewayMethod definitions');
  }
  if (definitions.WsGatewayMethod.enum?.length !== methodList.length) {
    throw new Error(`schema method 枚举数 ${definitions.WsGatewayMethod.enum?.length} != 基线 ${methodList.length}`);
  }
  // roots 齐备：每个根类型名都必须有对应 definition（缺 = 有根无定义，正例校验会悬空）
  for (const name of rootTypes) {
    if (!definitions[name]) throw new Error(`根类型 ${name} 缺 definition`);
  }

  // ── 后处理：组装最终文档（协议版本以注解形式出现在 hello / hello_ok，不约束校验）──
  const versionAnnotate = (def) => {
    if (def?.properties?.protocolVersion) def.properties.protocolVersion['x-pilotdeck-protocol-version'] = protocolVersion;
  };
  versionAnnotate(definitions.WsHelloFrame);
  versionAnnotate(definitions.WsHelloOk);
  // 线上 JSON 语义：result === undefined 时 JSON.stringify 省略该键（如 run_get / run_reattach 查无
  // 记录、GatewayWsConnection.ts:187 直发 result），ok:true 分支的 result 不许列为 required——
  // 否则 schema 会把正常响应拒之门外（第 2 轮评审 P1，负责任的正例见 fixtures）。
  const okBranch = definitions.WsResponseFrame?.anyOf?.find((b) => b?.properties?.ok?.const === true);
  if (!okBranch || !Array.isArray(okBranch.required)) throw new Error('WsResponseFrame ok:true 分支结构异常');
  const reqIdx = okBranch.required.indexOf('result');
  if (reqIdx >= 0) okBranch.required.splice(reqIdx, 1);
  okBranch.properties.result = {
    description: '未知载荷；实现返回 undefined 时序列化省略本键（如 run_get 查无记录）——校验允许缺省',
    ...(okBranch.properties.result ?? {}),
  };

  const doc = {
    $schema: 'http://json-schema.org/draft-07/schema#',
    $id: `pilotdeck://gateway-protocol/${protocolVersion}/frames.schema.json`,
    title: 'PilotDeck gateway WS 协议（WsGatewayFrame 及 Gateway 接口载荷类型）',
    $ref: '#/definitions/WsGatewayFrame',
    definitions,
    'x-pilotdeck': {
      protocolVersion,
      methodCount: methodList.length,
      frameTypes: frameTypeConsts,
      source: {
        'frames.ts': { sha256: sourceHashes['frames.ts'], methodLines: methodAnchor },
        'types.ts': { sha256: sourceHashes['types.ts'] },
        'version.ts': { sha256: sourceHashes['version.ts'] },
      },
      generator: `ts-json-schema-generator@${genPkg.version} (typescript@${tsPkg.version})`,
      rootTypes,
    },
  };
  const snapshot = {
    $comment: 'gateway 协议方法清单快照：CI 没有基线目录时，check_schema.py 以本文件为基准比对 schema 与 method-inventory.md。由 export_schema.mjs 生成，勿手改。',
    protocolVersion,
    methodCount: methodList.length,
    methods: methodList,
    frameTypes: frameTypeConsts,
    channelKeyLiterals,
    gatewayModeLiterals,
    gatewayWsClientNameLiterals: wsClientNameLiterals,
    payloadAliases: methodPayloadAliases,
    source: {
      'frames.ts': { sha256: sourceHashes['frames.ts'], methodLines: methodAnchor },
      'types.ts': { sha256: sourceHashes['types.ts'] },
      'version.ts': { sha256: sourceHashes['version.ts'] },
    },
    generatedBy: `ts-json-schema-generator@${genPkg.version} (typescript@${tsPkg.version})`,
  };
  return {
    schemaText: canonicalJson(doc),
    snapshotText: canonicalJson(snapshot),
  };
}

// 递归排序对象键 → JSON.stringify(2 空格) + 换行：保证同一输入字节稳定
function canonicalJson(value) {
  const sort = (v) => {
    if (Array.isArray(v)) return v.map(sort);
    if (v && typeof v === 'object') {
      return Object.fromEntries(Object.keys(v).sort().map((k) => [k, sort(v[k])]));
    }
    return v;
  };
  return `${JSON.stringify(sort(value), null, 2)}\n`;
}

// 原子写：先写同目录临时文件再 rename，中断不留半份产物
function writeFileAtomic(p, text) {
  const tmp = `${p}.tmp-${process.pid}`;
  fs.writeFileSync(tmp, text);
  fs.renameSync(tmp, p);
}

// ── 主流程 ──
const outDir = path.join(repoRoot, 'schema');
const schemaPath = path.join(outDir, 'gateway-protocol.schema.json');
const snapshotPath = path.join(outDir, 'gateway-methods.snapshot.json');

try {
  fs.writeFileSync(tmpTsconfig, JSON.stringify({
    compilerOptions: {
      target: 'ES2022',
      module: 'NodeNext',
      moduleResolution: 'NodeNext',
      strict: true,
      esModuleInterop: true,
      forceConsistentCasingInFileNames: true,
      skipLibCheck: true,
      types: [],
      noEmit: true,
    },
  }, null, 2));
  const first = generate();
  const second = checkMode ? generate() : null; // 双生成自比对只在 --check 做；平时生成一次就够
  const selfStable = !checkMode || (first.schemaText === second.schemaText && first.snapshotText === second.snapshotText);

  if (checkMode) {
    const problems = [];
    if (!selfStable) problems.push('两次生成字节不一致（生成器不确定）');
    for (const [label, want, file] of [['schema', first.schemaText, schemaPath], ['snapshot', first.snapshotText, snapshotPath]]) {
      if (!fs.existsSync(file)) { problems.push(`${label} 产物缺失: ${path.relative(repoRoot, file)}`); continue; }
      const got = read(file);
      if (got !== want) problems.push(`${label} 与再生成结果不一致: ${path.relative(repoRoot, file)}（磁盘 ${sha256(file)} vs 再生成 ${createHash('sha256').update(want).digest('hex')}）`);
    }
    if (problems.length) {
      console.error('FAIL export_schema --check');
      for (const p of problems) console.error(`  - ${p}`);
      process.exitCode = 1;
    } else {
      console.log(`OK export_schema --check：双生成字节一致；schema 与 snapshot 均与已提交文件一致`);
      console.log(`  基线: ${baseline}`);
      console.log(`  schema:   ${sha256(schemaPath)}  (${fs.statSync(schemaPath).size} B)`);
      console.log(`  snapshot: ${sha256(snapshotPath)}  (${fs.statSync(snapshotPath).size} B)`);
    }
  } else {

    fs.mkdirSync(outDir, { recursive: true });
    writeFileAtomic(schemaPath, first.schemaText);
    writeFileAtomic(snapshotPath, first.snapshotText);
    console.log(`基线: ${baseline}`);
    console.log(`方法数: ${methodList.length}（frames.ts:${methodAnchor[0]}-${methodAnchor[1]}）；协议版本 ${protocolVersion}`);
    console.log(`按方法内联载荷别名: ${methodPayloadAliases.length} 个方法、${aliasNameSet.size} 条别名`);
    console.log(`写出 ${path.relative(repoRoot, schemaPath)} (${first.schemaText.length} B, sha256 ${createHash('sha256').update(first.schemaText).digest('hex')})`);
    console.log(`写出 ${path.relative(repoRoot, snapshotPath)} (${first.snapshotText.length} B)`);
  }
} finally {
  // 临时 tsconfig 用完即删（不留残目录在系统临时区）
  fs.rmSync(tmpTsconfigDir, { recursive: true, force: true });
}
