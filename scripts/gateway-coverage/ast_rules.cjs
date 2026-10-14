/* Shared production rules and adversarial in-memory parser controls. */
const nodeErrno=new Set(Object.keys(require('node:os').constants.errno));
function errorCodeLiteral(value) {
  return /^(?:[A-Z][A-Z0-9]*(?:_[A-Z0-9]+)+|[a-z][a-z0-9]*(?:_[a-z0-9]+)+)$/.test(value)||nodeErrno.has(value)||
    ['aborted','conflict','internal','required','terminal','timeout','unauthorized','unavailable','unsupported'].includes(value);
}
function errorClassInfo(ts, node, file, line) {
  const parent=node.heritageClauses?.[0]?.types?.[0]?.expression.getText();
  const ctor=node.members.find(ts.isConstructorDeclaration);
  const arg=ctor?.parameters.findIndex(p=>p.name.getText()==='code');
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
function codeLiterals(ts, expression) {
  if(!expression)return [];
  if(ts.isStringLiteralLike(expression))return [expression];
  if(ts.isParenthesizedExpression(expression)||ts.isAsExpression(expression)||ts.isNonNullExpression(expression))return codeLiterals(ts,expression.expression);
  if(ts.isConditionalExpression(expression))return [...codeLiterals(ts,expression.whenTrue),...codeLiterals(ts,expression.whenFalse)];
  if(ts.isBinaryExpression(expression)&&[ts.SyntaxKind.QuestionQuestionToken,ts.SyntaxKind.BarBarToken].includes(expression.operatorToken.kind))return [...codeLiterals(ts,expression.left),...codeLiterals(ts,expression.right)];
  return [];
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
  return tests;
}
module.exports={errorCodeLiteral,errorClassInfo,codeLiterals,constructorLiterals,constructorLiteral,unknownError,controls};
