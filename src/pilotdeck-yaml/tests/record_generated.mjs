// Deterministic local-only corpus; no downloaded YAML tests. Node v22, yaml 2.9.0.
import { readFileSync, writeFileSync, mkdirSync } from 'node:fs';
import { resolve, join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
const baseline = resolve(process.argv[2]);
const output = resolve(process.argv[3]);
if (Number(process.versions.node.split('.')[0]) !== 22) throw Error('Use Node v22');
const yamlRoot = join(baseline, 'node_modules/yaml');
if (JSON.parse(readFileSync(join(yamlRoot, 'package.json'))).version !== '2.9.0') throw Error('Use yaml 2.9.0');
const { Lexer, Parser, CST, parseDocument, parseAllDocuments, LineCounter } = await import(pathToFileURL(join(yamlRoot, 'dist/index.js')));
const seed = 0x294a4a6b;
let state = seed;
const next = () => { state ^= state << 13; state ^= state >>> 17; state ^= state << 5; return state >>> 0; };
const pick = list => list[next() % list.length];
const cases = [];
const scalar = ['null', '~', 'true', 'False', 'yes', 'NO', 'on', 'off', '0', '-0', '+0', '01', '012', '0o17', '0xFF', '0b10', '1_000', '1:20', '1.2e3', '.5', '1.', '.inf', '-.Inf', '.nan', '1e999', '9007199254740993', 'hello', 'a:b', '🦀', '\ud800', '\udc00', '2001-12-15', '"true"', "'it''s'", '"\\uD800"', '"\\uDC00"', '"\\uD83D\\uDE00"', '"\\U0001F600"', '"\\x41\\0\\N\\_\\L\\P"', '"\\q"', '"\\u12xx"', '"\\U00110000"', '"a\\\nb"', "'a\n b'", '"a\n b"', '!custom yes', '!!str null', '!!int "10"', '!!int nope', '!!bool yes', '!!null nope', '!!float 2.0'];
const comments = ['', ' # note', ' # 🦀 comment', ' # \ud800'];
function flow(depth) {
  if (depth <= 0 || next() % 3 === 0) return pick(scalar);
  const n = 1 + next() % 4;
  return next() % 2 ? `[${Array.from({length:n}, () => flow(depth-1)).join(pick([', ', ',\n  ']))}]` : `{${Array.from({length:n}, (_,i) => `k${i}: ${flow(depth-1)}`).join(', ')}}`;
}
for (let i = 0; i < 2304; ++i) {
  const a = pick(scalar), b = pick(scalar), comment = pick(comments);
  let text;
  switch (i % 24) {
    case 0: text = `${a}${comment}\n`; break;
    case 1: text = `first: ${a}\nsecond: ${b}${comment}\n`; break;
    case 2: text = `- ${a}\n- ${b}\n- ${flow(3)}\n`; break;
    case 3: text = `${flow(4)}${comment}\n`; break;
    case 4: text = `a: &x ${flow(2)}\nb: *x\nc: &x ${b}\nd: *x\n`; break;
    case 5: text = pick(['a: *missing\n', 'a: *later\nb: &later 1\n', 'a: &x {self: *x}\n', '&a [*a]\n', 'a: &x key\n*x: value\n']); break;
    case 6: text = `a: ${pick(['|', '>', '|-', '>-', '|+', '>+', '|2', '>2-', '|0', '|22'])}${comment}\n  ${next()%2 ? 'one' : ''}\n  two\n\nnext: ${b}\n`; break;
    case 7: text = `%YAML ${pick(['1.1','1.2','1.3','9.9','1.1\n%YAML 1.2'])}\n---\na: ${a}\nb: ${b}\n`; break;
    case 8: text = `%TAG !e! tag:example.com,2020:\n%TAG !f! tag:example.org,2021:\n---\na: !e!thing ${a}\nb: !f!thing ${b}\n`; break;
    case 9: text = `---\n${flow(2)}\n${pick(['---','...\n---'])}\n${flow(2)}\n`; break;
    case 10: text = pick(['[a,,b]\n','{a: 1, a: 2}\n','a:\n\tb: 1\n','a: [1, 2\n','[a}\n','a: "unterminated\n','a: @bad\n', '- a\n b: c\n', 'a: b: c\n', '{a b}\n', '{a: b c: d}\n']); break;
    case 11: text = `? ${flow(2)}\n: ${a}\n? ${b}\n: ${flow(1)}\n`; break;
    case 12: text = `${pick(['!!set {x: null, y: null}', '!!omap [{x: 1}, {y: 2}]', '!!pairs [{x: 1}, {x: 2}]', '!!timestamp 2001-12-15T02:59:43.1Z', '!!binary SGVsbG8=', '!!seq [1,2]', '!!map {a: 1}'])}\n`; break;
    case 13: text = `base: &b {x: 1, z: 0}\nitem: {${pick(['<<','!!merge <<','"<<"'])}: ${pick(['*b','7','[*b, {x: 2, y: 3}]'])}, y: 2}\n`; break;
    case 14: text = `${pick(['a','1','null','.nan','true'])}: ${a}\n${pick(['a','01','~','.NaN','True'])}: ${b}\n`; break;
    case 15: text = '\ufeff' + pick(['# comment\n', '%YAML 1.2\n---\n', '']) + `a: ${a}\r\nb: ${b}\r\n`; break;
    case 16: { const depth = 24 + next()%105; text = '['.repeat(depth) + a + ']'.repeat(depth) + '\n'; break; }
    case 17: text = `long: "${'x'.repeat(512+next()%4096)}${pick(['\\uD800','🦀','\\n'])}"\n`; break;
    case 18: text = `# head\n\n- # item\n  a: ${a}\n  # middle\n  b: ${flow(2)}\n\n# tail\n`; break;
    case 19: text = pick(['%FOO bar\n---\na\n','%TAG !e!\n---\na\n','%YAML\n---\na\n','%YAML 1.2\na: b\n','!e!missing value\n','!<tag:example.com,a> yes\n', '!<missing yes\n']); break;
    case 20: text = `a:\n  - ${a}\n  - k: ${b}\n    v:\n      ${flow(2)}\n`; break;
    case 21: text = `?\n  ${pick(['- a\n  - b','k: 1\n  v: 2'])}\n: ${flow(2)}\n`; break;
    case 22: text = pick(['[a,\n b,\n]\n','{? a : b, c: d}\n', '[a: b, c: d]\n', 'a: [\n  b,\n  c\n]\n', 'a: [\nb\n---\nc\n', 'a: >+\n\n  x\n\n\n']); break;
    default: text = `${a}\n${pick(['...\n# end\n','---\n[broken\n','\n# tail\n','... extra\n','\n'])}`;
  }
  cases.push({id:`generated-${String(i).padStart(4,'0')}`, yaml:text});
}
// Branch probes for scalar folding, tags, alias sharing and complex-key conversion.
const probes = [
  "' \nfoo'", "'\nfoo'", "' \n  \n foo'", '" \\nfoo"',
  '!!int "10\\n"', '!!float "2.0\\n"', '!!bool "true\\n"',
  '!!set {a: null, b: 1}', '!!set {a: !!null null}', '!!omap [{a: 1}, {a: 2}]',
  '!!omap [{a: 1, b: 2}]', '!!pairs [a, {}, {b: 2}, null]',
  '%YAML 1.1\n---\n[!!merge wrong, !!binary SGVsbG8, ._, .e+3, +., 0b_, 0_]',
  '%TAG !e! tag:example:\n---\n[!e!x%F0%9F%A6%80 yes, !e!x%FF no]',
  '[& a, * ]', '[! a, !!str]', 'a: &x\n  - a\nb: *x',
  '%YAML 1.1\n---\nyes\n---\nyes\n%YAML 1.2\n---\nyes',
  '---\n{a: 1}\n... tail\n', '# alone\n\n', '%TAG !e! tag:example:\n',
];
for (const [i,yaml] of probes.entries()) cases.push({id:`branch-${i}`,yaml:yaml+'\n'});
const keyScalars = ['null', '~', '-0', '0xFF', '0o777', '1.200', '1e+20', 'yes', '!!bool yes', '!!int nope', '!!null nope', '!!str 1', '!custom true', '"a\\nb"', "'a\n b'", '"'+ 'long '.repeat(35) +'"', '"'+ 'x'.repeat(120) +'"', '"\\uD800"', '"a \\nb"', '"a\\n b"'];
for (const [i,value] of keyScalars.entries()) {
  for (const [j,key] of [`[${value}]`, `{a: ${value}}`, `[${value}, # comment\n b]`, `[ # before\n ${value}\n]`, `[&x ${value}, *x]`, `[{? [a, b]: ${value}}]`].entries()) {
    cases.push({id:`key-branch-${i}-${j}`,yaml:`? ${key}\n: result\n`});
  }
}

const edgeProbes = [
  '['.repeat(256)+'0'+']'.repeat(256), '['.repeat(512)+'0'+']'.repeat(512),
  '!!set {-0: null}', '!!omap [{-0: value}]',
  '%YAML 1.1\n---\na: &a {<<: *a}',
  '%YAML 1.1\n---\nbase: &base {? &a [*a]: value}\nobj: {<<: *base}',

  '[!!timestamp 9999-99-99T99:99:99.1Z, !!timestamp 0000-01-01]',
  '? [!!binary YQ, !!timestamp 2001-1-1t1:2:3.000Z]\n: result',
  '? [0x123456789abcdef0123456789abcdef, 0o1234567123456712345671234567]\n: result',
  '? [&bad\u0007 value]\n: result',
  '!!omap [{!!binary YQ==: 1}, {!!binary YQ==: 2}]',
  '!!omap [{!!timestamp 2001-01-01: 1}, {!!timestamp 2001-01-01: 2}]',
  '!!omap [{!!merge <<: 1}, {!!merge <<: 2}]',
  '!!omap [{.nan: 1}, {.NaN: 2}]',
  '!!set {? [&x a, *x]}',
  '? !!pairs [ # head\n {a: 1} # tail\n ]\n: result',
  '%YAML 1.1\n---\nyes\n...\n%TAG !e! tag:example:\n---\nyes',
  '%YAML 1.1\n---\nyes\n...\n---\nyes\n---\nyes\n---\nyes',
  '', '%YAML 1.1\n', '%YAML invalid\n', '# x\n%TAG !e! tag:example:\n# y\n',
  '!!int "\\uD800"', '!!float "\\uDC00"', '!!timestamp "\\uD800"',
  '!!binary "YQ\\uD800=="', '!!timestamp 0000-00-00T99:99:99.123456+29:59',
  'a: &a [0,0,0]\nb: &b [*a,*a,*a,*a,*a,*a,*a,*a,*a,*a]\nc: [*b,*b,*b,*b,*b,*b,*b,*b,*b,*b,*b]',
  '%YAML 1.1\n---\n? [!!timestamp 2001-01-01, 1:02:03.5, 0b101, 017]\n: value',
];
for (const [i,yaml] of edgeProbes.entries()) cases.push({id:`edge-${i}`,yaml});
const mutations = ['\n', '\t', ' ', ':', '-', '[', ']', '{', '}', ',', '&a ', '*a', '!', '"', "'", '#', '---\n', '\ud800', '\r', '%'];
for (let i = 0; i < 512; ++i) {
  let yaml = cases[next() % 2304].yaml;
  for (let j = 0, count = 1 + next() % 4; j < count; ++j) {
    const at = next() % (yaml.length + 1), remove = next() % 3;
    yaml = yaml.slice(0, at) + pick(mutations) + yaml.slice(at + remove);
  }
  cases.push({id:`mutation-${i}`,yaml});
}

// Keep the original 71-case recordings untouched; record additional stage oracles.
// Review round 4: separate shards preserve all earlier recordings byte-for-byte.
const round4 = [];
for (const escape of ['0', 'a', 'b', 'e', 'f', 'n', 'r', 't', '\t', 'v', 'N', '_', 'L', 'P', ' ', '"', '/', '\\', 'xFF', 'uFFFF', 'U0010FFFE', 'U0010FFFF', 'U00110000', 'UFFFFFFFF']) {
  round4.push({ id: `round4-escape-${round4.length}`, yaml: `"left\\${escape}right"\n` });
}
for (const length of [1023, 1024, 1025]) {
  const key = 'k'.repeat(length);
  round4.push({ id: `round4-block-key-${length}`, yaml: `${key}: value\n` });
  round4.push({ id: `round4-flow-seq-key-${length}`, yaml: `[${key}: value]\n` });
  round4.push({ id: `round4-flow-map-key-${length}`, yaml: `{${key}: value}\n` });
  round4.push({ id: `round4-explicit-key-${length}`, yaml: `? ${key}\n: value\n` });
}
// The indicator distance is UTF-16, including quotes, tags, spaces and pairs.
for (const units of [1023, 1024, 1025]) {
  const key = '🦀'.repeat(Math.floor(units / 2)) + (units % 2 ? 'k' : '');
  round4.push({ id: `round4-utf16-key-${units}`, yaml: `${key}: value\n` });
  round4.push({ id: `round4-quoted-key-${units}`, yaml: `"${'k'.repeat(units - 2)}": value\n` });
}
const legacy = JSON.parse(readFileSync(new URL('./fixtures/yaml.json', import.meta.url))).cases;
function graph(value) {
  const seen = new Map(), nodes = [];
  function visit(v) {
    if (v === null) return {type:'null'};
    if (typeof v === 'string') return {type:'string', json:JSON.stringify(v)};
    if (typeof v === 'number') return {type:'number', value:Object.is(v,-0)?'-0':String(v)};
    if (typeof v !== 'object') return {type:typeof v, value:String(v)};
    if (seen.has(v)) return {ref:seen.get(v)};
    const id = nodes.length; seen.set(v,id); nodes.push(null);
    const entries = Object.entries(v).map(([k,v])=>[JSON.stringify(k),visit(v)]);
    nodes[id] = {type:Object.prototype.toString.call(v), entries,
      ...(v instanceof Date ? {iso:v.toISOString()}:{}),
      ...(v instanceof Map ? {map:[...v].map(([k,v])=>[visit(k),visit(v)])}:{}),
      ...(v instanceof Set ? {set:[...v].map(visit)}:{})};
    return {ref:id};
  }
  return {root:visit(value),nodes};
}
const diagnostic = e => ({name:e.name,code:e.code,message:e.message,pos:e.pos,...(e.linePos?{linePos:e.linePos}:{})});
function documentResult(doc) {
  const errors=doc.errors.map(diagnostic), warnings=doc.warnings.map(diagnostic);
  if(errors.length) return {phase:'document.errors',errors,warnings};
  try { return {phase:'value',errors,warnings,value:graph(doc.toJSON())}; }
  catch(e) { return {phase:'toJSON.throw',errors,warnings,error:diagnostic(e)}; }
}
function result(test, prettyErrors) {
  try { return documentResult(parseDocument(test.yaml,{prettyErrors})); }
  catch(e) {return {phase:'parseDocument.throw',error:diagnostic(e)};}
}
function allDocuments(yaml) {
  const docs = parseAllDocuments(yaml, { prettyErrors: false });
  return {documents: docs.map(documentResult), versions: docs.map(doc => doc.directives.yaml),
    ...(docs.empty ? {empty: {comment:docs.comment, yaml:docs.directives.yaml, tags:Object.entries(docs.directives.tags), errors:docs.errors.map(diagnostic), warnings:docs.warnings.map(diagnostic)}} : {})};
}
function cstRead(cst) {
  return cst.filter(token => token.type === 'document').map(doc => {
    const items=[];
    CST.visit(doc, (item,path) => {
      const entry={path};
      for(const field of ['key','value']) {
        const errors=[];
        const result=CST.resolveAsScalar(item[field],true,(pos,code,message)=>errors.push({name:'YAMLParseError',code,message,pos:[pos,pos+1]}));
        if(result) entry[field]={...result,errors};
      }
      items.push(entry);
    });
    return items;
  });
}
mkdirSync(output,{recursive:true});
const manifest = {baselineCommit:execFileSync('git',['-C',baseline,'rev-parse','HEAD'],{encoding:'utf8'}).trim(),seed,node:process.version,yaml:'2.9.0',generated:cases.length,legacy:legacy.length,round4:round4.length,shards:[]};
for (const [prefix, inputs] of [['legacy-stages',legacy],['generated',cases],['round4',round4]]) {
  let shard=[], bytes=0, part=0;
  function flush() {
    if(!shard.length)return;
    const file=`${prefix}-${String(part++).padStart(3,'0')}.json`;
    const data=JSON.stringify(shard)+'\n';
    if(Buffer.byteLength(data)>2_000_000)throw Error('Shard exceeds 2 MB');
    writeFileSync(join(output,file),data);
    manifest.shards.push({file,cases:shard.length,bytes:Buffer.byteLength(data),sha256:createHash('sha256').update(data).digest('hex')});
    shard=[];bytes=0;
  }
  for(const test of inputs) {
    const lines=new LineCounter();
    const item={id:test.id,yaml:test.yaml,lex:[...new Lexer().lex(test.yaml)],cst:[...new Parser(lines.addNewLine).parse(test.yaml)],lineStarts:lines.lineStarts,
      expected:result(test,false),pretty:result(test,true),allDocuments:allDocuments(test.yaml)};
    const docLineCounter = new LineCounter(); parseDocument(test.yaml,{lineCounter:docLineCounter,prettyErrors:false}); item.docLines=docLineCounter.lineStarts;
    item.cstRead = cstRead(item.cst);
    item.cstSource = item.cst.map(CST.stringify).join('');
    if (prefix === 'legacy-stages' || prefix === 'round4' || test.id.startsWith('branch-')) {
      item.streaming = [1,2,7,31].map(size => {
        const lexer = new Lexer(), lc = new LineCounter(), parser = new Parser(lc.addNewLine);
        const lex = [], cst = [];
        for (let i = 0; i < test.yaml.length; i += size) {
          const chunk = test.yaml.slice(i, i + size);
          lex.push(...lexer.lex(chunk, true)); cst.push(...parser.parse(chunk, true));
        }
        lex.push(...lexer.lex('',false)); cst.push(...parser.parse('',false));
        return {size,lex,cst,lineStarts:lc.lineStarts};
      });
    }
    const size=Buffer.byteLength(JSON.stringify(item))+1;
    if(bytes+size>1_800_000)flush();
    shard.push(item);bytes+=size;
  }
  flush();
}
writeFileSync(join(output,'manifest.json'),JSON.stringify(manifest,null,2)+'\n');
console.log(JSON.stringify(manifest));
