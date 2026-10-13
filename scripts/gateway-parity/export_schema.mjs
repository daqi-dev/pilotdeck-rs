#!/usr/bin/env node
// A3 · gateway 协议 JSON Schema 导出（可重复、字节稳定）
//
// 用法：
//   node scripts/gateway-parity/export_schema.mjs                 # 生成/覆写 schema/ 下两份产物
//   node scripts/gateway-parity/export_schema.mjs --check         # 重新生成（含双生成自比对），与已提交产物字节比对
//   node scripts/gateway-parity/export_schema.mjs --baseline <dir>  # 指定基线（默认 $PDRS_BASELINE，再退回 ../..）
//
// 产物：
//   schema/gateway-protocol.schema.json   根 = $ref #/definitions/WsGatewayFrame；definitions 含帧可达类型
//                                         + Gateway 接口全部载荷类型（按 method 的 Input/Result）
//   schema/gateway-methods.snapshot.json  方法清单快照（CI 没有基线目录时的一致性基准）
//
// 可重复性：同一输入（基线源码 × 本目录钉死的生成器版本）两次生成字节一致。
//   序列化前递归排序对象键；--check 里另做双生成自比对。
//
// 已知生成器缺口与对策（R4：生成器输出只是基准，错了改生成配置，不改基线）：
//   * ts-json-schema-generator@2.9.0 不认 `import("...").X`（ImportTypeNode，frames.ts:18）——
//     用官方 createParser augmentor 钩子补一个 ImportTypeNodeParser，沿 checker 解析到真实声明。
//   * types.ts 对 web/client/protocol.js 的 12 个 `export type X = 导入的X` 影子别名会让
//     findNamedNode 撞名（MultipleDefinitionsError）——根节点不走名字查找，直接用 checker
//     解析成唯一声明节点（createSchemaFromNodes），影子别名跳过本层取 origin。

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
  DEFAULT_CONFIG, createProgram, createParser, createFormatter, SchemaGenerator,
} = await import(path.join(here, 'node_modules', 'ts-json-schema-generator', 'dist', 'index.js'));
const ts = (await import(path.join(here, 'node_modules', 'typescript', 'lib', 'typescript.js'))).default;

// ── 参数 ──
const argv = process.argv.slice(2);
const checkMode = argv.includes('--check');
const baselineFlagIdx = argv.indexOf('--baseline');
const baselineFlag = baselineFlagIdx >= 0 ? argv[baselineFlagIdx + 1] : undefined;

function resolveBaseline() {
  const candidates = [
    baselineFlag,
    process.env.PDRS_BASELINE,
    path.resolve(repoRoot, '..', '..', 'resources', 'PilotDeck'), // 常规 worktree：<主仓>/_build/wt-*
    path.resolve(repoRoot, 'resources', 'PilotDeck'), // 主仓本身
  ].filter(Boolean);
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
  if (/^Ws(HelloFrame|HelloOk|RequestFrame|ResponseFrame|EventFrame|NotificationFrame)$/.test(st.name.text)) {
    const inner = ts.isTypeLiteralNode(st.type) ? st.type
      : ts.isUnionTypeNode(st.type) ? st.type.types.find(ts.isTypeLiteralNode) : null;
    const typeProp = inner?.members.find((m) => ts.isPropertySignature(m) && m.name.getText() === 'type');
    const lit = typeProp && ts.isLiteralTypeNode(typeProp.type) && ts.isStringLiteral(typeProp.type.literal) ? typeProp.type.literal.text : null;
    if (lit) frameTypeConsts.push(lit);
  }
}
if (!methodList || !wsGatewayFrameDecl) throw new Error('frames.ts 里找不到 WsGatewayMethod / WsGatewayFrame');

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
function literalsOf(typeAliasName) {
  for (const st of typesSF.statements) {
    if (ts.isTypeAliasDeclaration(st) && st.name.text === typeAliasName && st.type && ts.isUnionTypeNode(st.type)) {
      return st.type.types.filter((t) => ts.isLiteralTypeNode(t) && ts.isStringLiteral(t.literal)).map((t) => t.literal.text);
    }
  }
  throw new Error(`types.ts 里找不到 ${typeAliasName} 字面量联合`);
}
const channelKeyLiterals = literalsOf('GatewayChannelKey');
const gatewayModeLiterals = literalsOf('GatewayMode');

// Gateway 接口 → 载荷类型名（Input/Result 等；全局类型 Promise/AsyncIterable/AbortSignal 只拆参不进根）
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
const localDeclNames = new Set();
for (const st of typesSF.statements) {
  if (ts.isTypeAliasDeclaration(st) || ts.isInterfaceDeclaration(st) || ts.isEnumDeclaration(st)) localDeclNames.add(st.name.text);
}
function isLocalDecl(name) { return localDeclNames.has(name); }
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

const generatorConfig = {
  ...DEFAULT_CONFIG,
  path: framesPath,
  tsconfig: tmpTsconfig,
  type: 'WsGatewayFrame',
  skipTypeCheck: true, // 只需要类型解析；基线全仓的类型诊断与本任务无关
  functions: 'fail', // 线上帧不可能带函数；可达类型里出现函数应当报错而不是静默吞掉
};

function generate() {
  const program = createProgram(generatorConfig);
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

  const generated = generator.createSchemaFromNodes([wsFrameDecl, ...payloadNodes]);
  const definitions = generated.definitions ?? generated.$defs;
  if (!definitions || !definitions.WsGatewayFrame || !definitions.WsGatewayMethod) {
    throw new Error('生成结果缺 WsGatewayFrame / WsGatewayMethod definitions');
  }
  if (definitions.WsGatewayMethod.enum?.length !== methodList.length) {
    throw new Error(`schema method 枚举数 ${definitions.WsGatewayMethod.enum?.length} != 基线 ${methodList.length}`);
  }

  // ── 后处理：组装最终文档（协议版本以注解形式出现在 hello / hello_ok，不约束校验）──
  const versionAnnotate = (def) => {
    if (def?.properties?.protocolVersion) def.properties.protocolVersion['x-pilotdeck-protocol-version'] = protocolVersion;
  };
  versionAnnotate(definitions.WsHelloFrame);
  versionAnnotate(definitions.WsHelloOk);
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
        'frames.ts': { sha256: sha256(framesPath), methodLines: methodAnchor },
        'types.ts': { sha256: sha256(typesPath) },
        'version.ts': { sha256: sha256(versionPath) },
      },
      generator: `ts-json-schema-generator@${genPkg.version} (typescript@${tsPkg.version})`,
      rootTypes: ['WsGatewayFrame', ...[...wireNames].sort()],
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
    source: {
      'frames.ts': { sha256: sha256(framesPath), methodLines: methodAnchor },
      'types.ts': { sha256: sha256(typesPath) },
      'version.ts': { sha256: sha256(versionPath) },
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

// ── 主流程 ──
const outDir = path.join(repoRoot, 'schema');
const schemaPath = path.join(outDir, 'gateway-protocol.schema.json');
const snapshotPath = path.join(outDir, 'gateway-methods.snapshot.json');

const first = generate();
const second = generate(); // 双生成自比对（可重复性检查的一半）
const selfStable = first.schemaText === second.schemaText && first.snapshotText === second.snapshotText;

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
    process.exit(1);
  }
  console.log(`OK export_schema --check：双生成字节一致；schema 与 snapshot 均与已提交文件一致`);
  console.log(`  基线: ${baseline}`);
  console.log(`  schema:   ${sha256(schemaPath)}  (${fs.statSync(schemaPath).size} B)`);
  console.log(`  snapshot: ${sha256(snapshotPath)}  (${fs.statSync(snapshotPath).size} B)`);
  process.exit(0);
}

fs.mkdirSync(outDir, { recursive: true });
fs.writeFileSync(schemaPath, first.schemaText);
fs.writeFileSync(snapshotPath, first.snapshotText);
console.log(`基线: ${baseline}`);
console.log(`方法数: ${methodList.length}（frames.ts:${methodAnchor[0]}-${methodAnchor[1]}）；协议版本 ${protocolVersion}`);
console.log(`写出 ${path.relative(repoRoot, schemaPath)} (${first.schemaText.length} B, sha256 ${createHash('sha256').update(first.schemaText).digest('hex')})`);
console.log(`写出 ${path.relative(repoRoot, snapshotPath)} (${first.snapshotText.length} B)`);
console.log(`双生成自比对: ${selfStable ? '一致' : '不一致(!!)'}`);
if (!selfStable) process.exit(1);
