/* Shared production rules and adversarial in-memory parser controls. */
const nodeErrno=new Set(Object.keys(require('node:os').constants.errno));
function errorCodeLiteral(value) {
  return /^(?:[A-Z][A-Z0-9]*(?:_[A-Z0-9]+)+|[a-z][a-z0-9]*(?:_[a-z0-9]+)+)$/.test(value)||nodeErrno.has(value)||
    ['aborted','conflict','internal','required','terminal','timeout','unauthorized','unavailable','unsupported'].includes(value);
}
function errorClassInfo(ts, node, file, line) {
  const parent=node.heritageClauses?.[0]?.types?.[0]?.expression.getText();
  const ctor=node.members.find(ts.isConstructorDeclaration);
  const arg=ctor?.parameters.findIndex(p=>codeParameter(p.name.getText()));
  const propertyName=n=>n?.name?.text ?? (n?.name&&ts.isComputedPropertyName(n.name)?n.name.expression.text:undefined);
  let code=node.members.some(m=>propertyName(m)==='code') || !!ctor?.parameters.some(p=>
    p.name.getText()==='code'&&p.modifiers?.some(m=>[ts.SyntaxKind.PublicKeyword,ts.SyntaxKind.PrivateKeyword,ts.SyntaxKind.ProtectedKeyword,ts.SyntaxKind.ReadonlyKeyword].includes(m.kind)));
  function assignments(n) {
    if(ts.isBinaryExpression(n)&&n.operatorToken.kind===ts.SyntaxKind.EqualsToken) {
      const left=n.left;
      if((ts.isPropertyAccessExpression(left)&&left.name.text==='code'||ts.isElementAccessExpression(left)&&left.argumentExpression?.text==='code')&&left.expression.kind===ts.SyntaxKind.ThisKeyword)code=true;
    }
    ts.forEachChild(n,assignments);
  }
  assignments(node);
  return {file,line,parent,code,code_argument:arg>=0?arg:null};
}
// Resolve only immutable bindings, never a type's possible values or a
// parameter's default. Unknown branches remain explicit dynamic obligations.
function codeResolution(ts, expression, checker, trail=new Set(), access=[]) {
  const unknown=()=>({values:[],dynamic:true});
  const combine=rows=>({values:[...new Set(rows.flatMap(r=>r.values))],dynamic:!rows.length||rows.some(r=>r.dynamic)});
  if(!expression||trail.has(expression)||trail.size>60)return unknown();
  const next=new Set(trail);next.add(expression);
  const resolve=(n,selection=access)=>codeResolution(ts,n,checker,next,selection);
  if(ts.isStringLiteralLike(expression)||ts.isNumericLiteral(expression)||
     [ts.SyntaxKind.TrueKeyword,ts.SyntaxKind.FalseKeyword,ts.SyntaxKind.NullKeyword].includes(expression.kind)||
     ts.isPrefixUnaryExpression(expression)&&ts.isNumericLiteral(expression.operand))return access.length?unknown():{values:[expression],dynamic:false};
  if(ts.isParenthesizedExpression(expression)||ts.isAsExpression(expression)||ts.isNonNullExpression(expression)||ts.isTypeAssertionExpression(expression))return resolve(expression.expression);
  if(ts.isConditionalExpression(expression))return combine([resolve(expression.whenTrue),resolve(expression.whenFalse)]);
  if(ts.isBinaryExpression(expression)&&[ts.SyntaxKind.QuestionQuestionToken,ts.SyntaxKind.BarBarToken].includes(expression.operatorToken.kind))return combine([resolve(expression.left),resolve(expression.right)]);
  function declarations(n) {
    if(!checker)return [];
    let sym=ts.isShorthandPropertyAssignment(n.parent)?checker.getShorthandAssignmentValueSymbol(n.parent):checker.getSymbolAtLocation(n);
    if(sym?.flags&ts.SymbolFlags.Alias)sym=checker.getAliasedSymbol(sym);
    return sym?.declarations||[];
  }
  function initializer(d) {
    if(ts.isVariableDeclaration(d)&&ts.isVariableDeclarationList(d.parent)&&d.parent.flags&ts.NodeFlags.Const)return d.initializer;
    if(ts.isEnumMember(d))return d.initializer;
  }
  if(ts.isObjectLiteralExpression(expression)) {
    const keys=access.length?access[0]:null,remaining=access.slice(1),rows=[];
    for(const p of expression.properties) {
      if(ts.isSpreadAssignment(p)) {rows.push(resolve(p.expression));continue;}
      const property=ts.isComputedPropertyName(p.name)?resolve(p.name.expression,[]):null;
      const names=property?property.values.map(n=>n.text):[p.name?.text];
      if(keys===null||names.some(n=>keys.includes(n))||property?.dynamic)
        rows.push(resolve(ts.isShorthandPropertyAssignment(p)?p.name:p.initializer,remaining));
    }
    // Unknown keys select all members. Their runtime value is irrelevant once
    // every possible map value is known; unknown member values stay dynamic.
    return combine(rows);
  }
  if(ts.isIdentifier(expression)) {
    const decls=declarations(expression);
    return combine(decls.map(d=>initializer(d)?resolve(initializer(d)):unknown()));
  }
  if(ts.isPropertyAccessExpression(expression)||ts.isElementAccessExpression(expression)) {
    // Enum members and namespace-imported consts have concrete declarations;
    // interface fields and mutable class properties deliberately stay dynamic.
    const decls=declarations(ts.isPropertyAccessExpression(expression)?expression.name:expression);
    if(decls.length&&decls.every(d=>initializer(d)))return combine(decls.map(d=>resolve(initializer(d))));
    const keys=ts.isPropertyAccessExpression(expression)?{values:[expression.name],dynamic:false}:resolve(expression.argumentExpression,[]);
    return resolve(expression.expression,[keys.dynamic?null:keys.values.map(n=>n.text??n.getText()),...access]);
  }
  return unknown();
}
function codeValues(ts, expression, checker) { return codeResolution(ts,expression,checker).values; }
function codeLiterals(ts, expression, checker) { return codeValues(ts,expression,checker).filter(ts.isStringLiteralLike); }
function codeObservations(ts, expression, checker) {
  return codeValues(ts,expression,checker).map(node=>({node,
    code:ts.isStringLiteralLike(node)?node.text:node.getText(),
    valid:ts.isStringLiteralLike(node)&&errorCodeLiteral(node.text)}));
}
// Parameter names locate the value only after the declaration is proven to
// produce a code. Consumers such as isErrno(code) are not factories.
function codeParameter(name) { return /^(?:code|errorCode|error_code)$/.test(name); }
function factoryCodePositions(ts, node, catalog, forwarded=()=>[]) {
  const candidates=node.parameters.map((p,i)=>codeParameter(p.name.getText())?i:-1).filter(i=>i>=0);
  const used=new Set();
  function observe(expression) {
    if(!expression)return;
    function identifiers(n) {
      if(ts.isIdentifier(n))for(const i of candidates)if(n.text===node.parameters[i].name.getText())used.add(i);
      ts.forEachChild(n,identifiers);
    }
    identifiers(expression);
  }
  function visit(n) {
    if(n!==node.body&&(ts.isFunctionDeclaration(n)||ts.isMethodDeclaration(n)))return;
    if((ts.isPropertyAssignment(n)||ts.isPropertyDeclaration(n))&&n.name?.getText()==='code')observe(n.initializer);
    if(ts.isShorthandPropertyAssignment(n)&&n.name.text==='code')observe(n.name);
    if(ts.isBinaryExpression(n)&&n.operatorToken.kind===ts.SyntaxKind.EqualsToken&&
      ((ts.isPropertyAccessExpression(n.left)&&n.left.name.text==='code')||
       (ts.isElementAccessExpression(n.left)&&n.left.argumentExpression?.text==='code')))observe(n.right);
    if(ts.isNewExpression(n)) {
      const info=catalog[n.expression.getText()];
      if(info?.code&&info.code_argument!==null)observe(n.arguments?.[info.code_argument]);
    }
    if(ts.isCallExpression(n))for(const i of forwarded(n))observe(n.arguments[i]);
    ts.forEachChild(n,visit);
  }
  visit(node.body);return [...used].sort((a,b)=>a-b);
}
function constructorLiterals(ts, node, catalog) {
  const info=catalog[node.expression.getText()];
  return !info?.code||info.code_argument===null?[]:codeLiterals(ts,node.arguments?.[info.code_argument]);
}
function constructorLiteral(ts,node,catalog) { return constructorLiterals(ts,node,catalog)[0]||null; }
function unknownError(name,catalog) {
  return /Error$/.test(name)&&!['Error','TypeError','RangeError','SyntaxError','AggregateError','URIError','ReferenceError'].includes(name)&&!catalog[name];
}
function controls(ts) {
  const tests=[];
  const accepted=['PROJECT_DELETED','gateway_request_failed','ETIMEDOUT','required','terminal'];
  const rejected=['object','string','function','number','boolean','undefined','Object','Error','ERROR','AgentLoop','ordinary English words'];
  tests.push({name:'error_code_literal_shapes',result:accepted.every(errorCodeLiteral)&&rejected.every(s=>!errorCodeLiteral(s))?'PASS':'FAIL'});
  const samples=[
    ['quoted_comment_regex_template',[
      'function target() {',
      '  const a = "}) /* not a comment */";',
      "  const b = '({) // not a comment';",
      '  /* } ) ] */ // } )',
      String.raw`  const r = /[{}()\/]\/\/(?:x|y)/g;`,
      '  const t = `text } ${(() => /}/.test("}"))()} ${({x: "{"}).x}`;',
      '  return a + b + t + r;',
      '}',
      'function outside() { throw new Error("OUTSIDE"); }',
    ].join('\n'),8],
    ['expression_arrow',[
      'const target = (x: string): string =>',
      '  /[{}()]/.test(x) ? `} ${x}` : "(";',
      'const outside = () => "OUTSIDE";',
    ].join('\n'),2],
    ['object_return_type',[
      'function target(): {x: string; nested: {y: number}} {',
      '  return {x: "}", nested: {y: 1}};',
      '}',
      'const outside = 1;',
    ].join('\n'),3],
  ];
  for(const [name,source,end] of samples) {
    const sf=ts.createSourceFile('fixture.ts',source,ts.ScriptTarget.Latest,true);
    let target;function visitRange(n){if((ts.isFunctionDeclaration(n)&&n.name?.text==='target')||(ts.isVariableDeclaration(n)&&n.name.getText()==='target'))target=ts.isVariableDeclaration(n)?n.initializer:n;ts.forEachChild(n,visitRange);}visitRange(sf);
    const observed=sf.getLineAndCharacterOfPosition(target.end).line+1;
    tests.push({name,expected_end:end,observed_end:observed,result:!sf.parseDiagnostics.length&&observed===end?'PASS':'FAIL'});
  }
  const sf=ts.createSourceFile('renamed-error.ts',[
    'class R3PreviouslyUncoveredError extends Error {',
    '  constructor(readonly code: string, message: string) { super(message); }',
    '}',
    'function provider() { throw new R3PreviouslyUncoveredError("R3_NEW_DOMAIN_CODE", "test"); }',
  ].join('\n'),ts.ScriptTarget.Latest,true);
  let declaration,call;function visitFixture(n){if(ts.isClassDeclaration(n))declaration=n;if(ts.isNewExpression(n))call=n;ts.forEachChild(n,visitFixture);}visitFixture(sf);
  const catalog={[declaration.name.text]:errorClassInfo(ts,declaration,'renamed-error.ts',1)};
  const arg=constructorLiteral(ts,call,catalog);
  tests.push({name:'renamed_code_error_class',observed_code:arg?.text,result:arg?.text==='R3_NEW_DOMAIN_CODE'?'PASS':'FAIL'});
  tests.push({name:'undefined_error_class',result:unknownError('R3UndefinedError',catalog)?'PASS':'FAIL'});
  const fallback=ts.createSourceFile('fallback.ts','new R3PreviouslyUncoveredError(remote.code ?? "R3_FALLBACK_CODE", "test");',ts.ScriptTarget.Latest,true);
  let fallbackCall;function findFallback(n){if(ts.isNewExpression(n))fallbackCall=n;ts.forEachChild(n,findFallback);}findFallback(fallback);
  const fallbackCodes=constructorLiterals(ts,fallbackCall,catalog).map(n=>n.text);
  tests.push({name:'conditional_dynamic_fallback_code',result:fallbackCodes.includes('R3_FALLBACK_CODE')?'PASS':'FAIL'});
  for(const [label,source,wanted] of [
    ['factory_code_first', 'const failure=(code: string,message: string)=>Object.assign(new Error(message),{code}); failure("PROJECT_BINDING_STORE_INVALID","ordinary message");', 0],
    ['factory_same_name_code_second', 'const failure=(status: number,code: string)=>Object.assign(new Error(code),{status,code}); failure(409,"SECOND_CODE");', 1],
    ['factory_errorCode_alias', 'function renamed(message: string,errorCode: string){return {code:errorCode};} renamed("ordinary message","RENAMED_CODE");', 1],
    ['factory_reordered_code_third', 'function controlError(call: unknown,message: string,code="tool_execution_failed"){return {code};} controlError({},"ordinary message","tool_aborted");', 2],
    ['code_consumer_is_not_factory', 'function isErrno(error: any,code: string){return error.code===code;} isErrno({},"ENOENT");', -1],
  ]) {
    const sf=ts.createSourceFile(label+'.ts',source,ts.ScriptTarget.Latest,true);
    let fn,call;
    function find(n){if(ts.isFunctionDeclaration(n)||ts.isArrowFunction(n))fn=n;if(ts.isCallExpression(n)&&!n.expression.getText().includes('.'))call=n;ts.forEachChild(n,find);}find(sf);
    const positions=factoryCodePositions(ts,fn,{});
    const observations=codeObservations(ts,call.arguments[positions[0]]);
    tests.push({name:label,observed_positions:positions,result:
      wanted<0?(positions.length===0?'PASS':'FAIL'):
      positions.length===1&&positions[0]===wanted&&observations.length===1&&observations[0].valid?'PASS':'FAIL'});
  }
  const invalid=ts.createSourceFile('invalid-code.ts','["ordinary message", remote.code ?? "ordinary fallback", 404, null, true]',ts.ScriptTarget.Latest,true).statements[0].expression;
  tests.push({name:'invalid_produced_values_fail_instead_of_filter',result:
    invalid.elements.every(expr=>codeObservations(ts,expr).some(row=>!row.valid))?'PASS':'FAIL'});
  const wrapper=ts.createSourceFile('wrapper.ts','function wrapper(message: string,errorCode: string){return renamed(message,errorCode);}',ts.ScriptTarget.Latest,true).statements[0];
  tests.push({name:'factory_forwarded_errorCode',result:
    JSON.stringify(factoryCodePositions(ts,wrapper,{},()=>[1]))==='[1]'?'PASS':'FAIL'});
  const fixtureFiles={
    '/codes.ts':'export const IMPORTED = "IMPORTED_CODE"; export const MAP = {a:"MAP_A", ["b"]:"MAP_B"} as const;',
    '/consumer.ts':[
      'import {IMPORTED as ALIAS, MAP} from "./codes";',
      'enum Codes { One="ENUM_CODE" }',
      'const TOP="TOP_CODE"; const CHOICE = flag ? "CHOICE_A" : "CHOICE_B";',
      'const SPREAD={...MAP,c:TOP} as const; const CYCLE=CYCLE;',
      'const NESTED={codes:MAP} as const; const MIXED=flag ? MAP : getMap(); const RECURSIVE={...RECURSIVE};',
      'let mutable="MUTABLE_CODE";',
      'function target(param:string) {',
      ' const LOCAL=CHOICE; const PARTIAL=flag ? TOP : param;',
      ' const probes=[TOP,LOCAL,ALIAS,Codes.One,MAP.a,MAP["b"],MAP[param],SPREAD[param],param,mutable,getCode(),CYCLE,PARTIAL,NESTED.codes[param],MIXED[param],RECURSIVE[param],({a:TOP,b:ALIAS} as const)];',
      '}',
    ].join('\n'),
  };
  const host=ts.createCompilerHost({});
  host.getSourceFile=(file,version)=>fixtureFiles[file]===undefined?undefined:ts.createSourceFile(file,fixtureFiles[file],version,true);
  host.fileExists=file=>fixtureFiles[file]!==undefined;
  host.readFile=file=>fixtureFiles[file];
  const program=ts.createProgram(Object.keys(fixtureFiles),{noLib:true,moduleResolution:ts.ModuleResolutionKind.Node10},host);
  const checker=program.getTypeChecker();let probes;
  function findProbes(n){if(ts.isVariableDeclaration(n)&&n.name.getText()==='probes')probes=n.initializer;ts.forEachChild(n,findProbes);}
  findProbes(program.getSourceFile('/consumer.ts'));
  const expected=[
    ['top_level_const',['TOP_CODE'],false],['local_conditional_const',['CHOICE_A','CHOICE_B'],false],
    ['imported_const_alias',['IMPORTED_CODE'],false],['enum_member',['ENUM_CODE'],false],
    ['as_const_member',['MAP_A'],false],['computed_literal_member',['MAP_B'],false],
    ['computed_dynamic_map',['MAP_A','MAP_B'],false],['spread_map',['MAP_A','MAP_B','TOP_CODE'],false],
    ['parameter_dynamic',[],true],['let_dynamic',[],true],['call_dynamic',[],true],['cycle_dynamic',[],true],
    ['partial_resolution',['TOP_CODE'],true],
    ['nested_object_map',['MAP_A','MAP_B'],false],['partial_object_map',['MAP_A','MAP_B'],true],
    ['spread_cycle_dynamic',[],true],['direct_object_all_members',['TOP_CODE','IMPORTED_CODE'],false],
  ];
  for(let i=0;i<expected.length;i++) {
    const [label,wanted,dynamic]=expected[i];const result=codeResolution(ts,probes.elements[i],checker);
    const actual=result.values.map(n=>n.text).sort();
    tests.push({name:'symbol_'+label,result:JSON.stringify(actual)===JSON.stringify([...wanted].sort())&&result.dynamic===dynamic?'PASS':'FAIL'});
  }
  return tests;
}
module.exports={codeResolution,codeValues,codeObservations,codeParameter,factoryCodePositions,errorCodeLiteral,errorClassInfo,codeLiterals,constructorLiterals,constructorLiteral,unknownError,controls};
