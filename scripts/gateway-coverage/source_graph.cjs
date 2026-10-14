#!/usr/bin/env node
/* Read-only TypeScript AST analysis. Uses the frozen checkout's existing parser;
 * never installs packages or executes/imports product modules. */
const fs = require('fs');
const path = require('path');
const root = path.resolve(process.argv[2]);
const ts = require(path.join(root, 'node_modules/typescript/lib/typescript.js'));
const DEPTH = 8;
const rules=require('./ast_rules.cjs');
const files = [];
function walk(dir) {
  if (!fs.existsSync(dir)) return;
  for (const e of fs.readdirSync(dir, {withFileTypes:true}).sort((a,b)=>a.name<b.name?-1:a.name>b.name?1:0)) {
    if (['node_modules','dist','lib','.git','tests','__tests__'].includes(e.name)) continue;
    const p = path.join(dir,e.name);
    if(e.isDirectory()) walk(p);
    else if (/\.(?:ts|tsx|mts|mjs|js)$/.test(p) && !/\.(?:spec|test|d)\.(?:ts|mts|cts)$/.test(p)) files.push(p);
  }
}
for (const dir of ['src','packages','products','tools','apps']) walk(path.join(root,dir));
files.push(path.join(root,'ui/server/staffdeck-approval-authority.mjs'));
const graphFiles=[...files];
for(const dir of ['ui','skills'])walk(path.join(root,dir));
const inventoryFiles=files.filter(p=>!graphFiles.includes(p));
const program = ts.createProgram([...graphFiles,...inventoryFiles],{target:ts.ScriptTarget.Latest,module:ts.ModuleKind.NodeNext,
  moduleResolution:ts.ModuleResolutionKind.NodeNext,allowJs:true,skipLibCheck:true,noResolve:false});
const checker = program.getTypeChecker();
const sources = program.getSourceFiles().filter(s=>graphFiles.includes(s.fileName));
for(const source of sources)if(source.parseDiagnostics.length)throw Error('Source syntax error: '+path.relative(root,source.fileName));
const functions = [], byNode = new Map(), classes = new Map(), sites = new Map(), assignmentsBySymbol = new Map(), optionWrites = new Map(), namedCallSites = new Map();
const rel = n=>path.relative(root,n.getSourceFile().fileName).split(path.sep).join('/');
const line = (n,p=n.getStart())=>n.getSourceFile().getLineAndCharacterOfPosition(p).line+1;
const name = n=>n?.name?.getText() || '';
const callable = n=>n && n.body && (ts.isFunctionDeclaration(n)||ts.isMethodDeclaration(n)||
  ts.isArrowFunction(n)||ts.isFunctionExpression(n)||ts.isConstructorDeclaration(n)||ts.isGetAccessor(n));
function enclosingClass(n) {for(let p=n.parent;p;p=p.parent)if(ts.isClassDeclaration(p))return p;}
function visit(n) {
  if(ts.isClassDeclaration(n)&&n.name) classes.set(n.name.text,n);
  if(callable(n)) {
    const f={file:rel(n),start:line(n),end:line(n,n.end),symbol:name(n)||name(n.parent)||'<anonymous>',
      class:enclosingClass(n)?.name?.text||null,calls:[],errors:[],unknown_errors:[],literals:[],constructs:[]};
    f.id=functions.length; functions.push(f); byNode.set(n,f);
  }
  if(ts.isNewExpression(n)) {
    const key=n.expression.getText(); if(!sites.has(key))sites.set(key,[]);sites.get(key).push(n);
  }
  if(ts.isCallExpression(n)&&ts.isPropertyAccessExpression(n.expression)) {
    const member=n.expression.name.text;
    if(['setAlwaysOnControl','setPrepareWeixinLogin','updateSubsystems','publishGeneration'].includes(member)) {
      if(!namedCallSites.has(member))namedCallSites.set(member,[]);namedCallSites.get(member).push(n);
    }
  }
  if(ts.isBinaryExpression(n)&&n.operatorToken.kind===ts.SyntaxKind.EqualsToken&&ts.isIdentifier(n.left)) {
    const sym=checker.getSymbolAtLocation(n.left);
    if(sym) {if(!assignmentsBySymbol.has(sym))assignmentsBySymbol.set(sym,[]);assignmentsBySymbol.get(sym).push(n.right);}
  }
  if(ts.isBinaryExpression(n)&&n.operatorToken.kind===ts.SyntaxKind.EqualsToken&&ts.isPropertyAccessExpression(n.left)) {
    const left=n.left.expression.getText().replace(/\s/g,'');
    if(left.startsWith('(this.options')||left==='this.options') {
      const cl=enclosingClass(n);const key=name(cl)+'.'+n.left.name.text;
      if(!optionWrites.has(key))optionWrites.set(key,[]);optionWrites.get(key).push(n.right);
    }
  }
  ts.forEachChild(n,visit);
}
for(const s of sources) visit(s);
// Whole-repository class inventory without resolving frontend call graphs.
for(const file of inventoryFiles) {
  const sf=program.getSourceFile(file);
  if(sf.parseDiagnostics.length)throw Error('Inventory syntax error: '+path.relative(root,file));
  function inventory(n){if(ts.isClassDeclaration(n)&&n.name)classes.set(n.name.text,n);ts.forEachChild(n,inventory);}
  inventory(sf);
}
function declarations(n) {
  let sym=ts.isShorthandPropertyAssignment(n.parent)?checker.getShorthandAssignmentValueSymbol(n.parent):checker.getSymbolAtLocation(n);if(sym?.flags&ts.SymbolFlags.Alias)sym=checker.getAliasedSymbol(sym);
  return sym?.declarations||[];
}
function unwrap(n) {
  while(n&&(ts.isParenthesizedExpression(n)||ts.isAsExpression(n)||ts.isNonNullExpression(n)||
    ts.isTypeAssertionExpression(n)||ts.isAwaitExpression(n)))n=n.expression;
  return n;
}
const cache = new Map();let assemblySites=null;
function value(n,trail=new Set()) {
  n=unwrap(n);if(!n||trail.has(n)||trail.size>30||!sources.includes(n.getSourceFile()))return [];
  if(cache.has(n))return cache.get(n);
  const next=new Set(trail); next.add(n);let out=[];
  const v=x=>value(x,next);
  if(callable(n)||ts.isClassDeclaration(n)||ts.isObjectLiteralExpression(n))out=[n];
  else if(n.kind===ts.SyntaxKind.ThisKeyword)out=[enclosingClass(n)].filter(Boolean);
  else if(ts.isNewExpression(n))out=declarations(n.expression).filter(ts.isClassDeclaration);
  else if(ts.isConditionalExpression(n))out=[...v(n.whenTrue),...v(n.whenFalse)];
  else if(ts.isBinaryExpression(n)&&[ts.SyntaxKind.QuestionQuestionToken,ts.SyntaxKind.BarBarToken,ts.SyntaxKind.AmpersandAmpersandToken].includes(n.operatorToken.kind))out=[...v(n.left),...v(n.right)];
  else if(ts.isIdentifier(n)) {
    for(const d of declarations(n)) {
      if(callable(d)||ts.isClassDeclaration(d))out.push(d);
      // Parameters are resolved only through concrete constructor sites, never
      // through an interface type or a ledger-provided implementation name.
      else if(ts.isParameter(d)&&ts.isConstructorDeclaration(d.parent)) {
        const cl=d.parent.parent;const ix=d.parent.parameters.indexOf(d);let hasSite=false;
        for(const s of sites.get(name(cl))||[]) {
          if((assemblySites===null||assemblySites.has(s))&&rel(s).startsWith('src/') && (name(cl)!=='InProcessGateway'||rel(s)==='src/cli/createLocalGateway.ts')){hasSite=true;out.push(...v(s.arguments?.[ix]));}
        }
        if(hasSite&&!out.length&&d.initializer)out.push(...v(d.initializer));
      } else if(ts.isParameter(d)&&name(d.parent)==='createLocalGateway') {
        for(const sf of sources)if(rel(sf)==='src/cli/pilotdeck.ts') {
          function calls(a) {
            if(ts.isCallExpression(a)&&a.expression.getText()==='createLocalGateway'&&line(a)===107)out.push(...v(a.arguments[0]));
            ts.forEachChild(a,calls);
          } calls(sf);
        }
      } else if(ts.isParameter(d)&&['setAlwaysOnControl','setPrepareWeixinLogin','updateSubsystems','publishGeneration'].includes(name(d.parent)||name(d.parent.parent))) {
        const ix=d.parent.parameters?.indexOf(d);
        for(const call of namedCallSites.get(name(d.parent)||name(d.parent.parent))||[])out.push(...v(call.arguments[ix]));
        if(d.initializer)out.push(...v(d.initializer));
      } else if(d.initializer)out.push(...v(d.initializer));
    }
    for(const a of assignmentsBySymbol.get(checker.getSymbolAtLocation(n))||[])out.push(...v(a));
  } else if(ts.isPropertyAccessExpression(n)||ts.isElementAccessExpression(n)) {
    const prop=ts.isPropertyAccessExpression(n)?n.name.text:n.argumentExpression?.text;
    for(const base of v(n.expression)) {
      if(ts.isObjectLiteralExpression(base))out.push(...objectMember(base,prop,next));
      if(ts.isClassDeclaration(base)) {
        for(const m of base.members) {
          if(name(m)===prop)out.push(...(callable(m)?[m]:v(m.initializer)));
          if(ts.isConstructorDeclaration(m)) {
            for(const p of m.parameters)if(name(p)===prop)out.push(...v(p.name));
            function assignments(a) {
              if (!a) return;
              if(ts.isBinaryExpression(a)&&a.operatorToken.kind===ts.SyntaxKind.EqualsToken&&
                ts.isPropertyAccessExpression(a.left)&&a.left.expression.kind===ts.SyntaxKind.ThisKeyword&&a.left.name.text===prop)out.push(...v(a.right));
              ts.forEachChild(a,assignments);
            }
            assignments(m.body);
          } else if(callable(m)) {
            function fieldAssignments(a) {
              if(ts.isBinaryExpression(a)&&a.operatorToken.kind===ts.SyntaxKind.EqualsToken&&ts.isPropertyAccessExpression(a.left)&&a.left.expression.kind===ts.SyntaxKind.ThisKeyword&&a.left.name.text===prop)out.push(...v(a.right));
              ts.forEachChild(a,fieldAssignments);
            } fieldAssignments(m.body);
          }
        }
      }
    }
    if(ts.isPropertyAccessExpression(n)&&n.expression.getText()==='this.options') {
      const key=name(enclosingClass(n))+'.'+prop;
      for(const expr of optionWrites.get(key)||[])out.push(...v(expr));
    }
    // A declaration with an actual body is sound for a concrete class method;
    // signatures of interface ports are intentionally not treated as providers.
    if(!out.length)for(const d of declarations(ts.isPropertyAccessExpression(n)?n.name:n)) {
      if(callable(d))out.push(d);
      else if(d.initializer)out.push(...v(d.initializer));
    }
  } else if(ts.isCallExpression(n)) {
    // Object.freeze and factories returning concrete objects/instances.
    if(n.expression.getText()==='Object.freeze')out=v(n.arguments[0]);
    else for(const target of v(n.expression))if(callable(target)) {
      if(!ts.isBlock(target.body))out.push(...v(target.body));
      else {
        function returns(a) {
          if(a!==target.body&&callable(a))return;
          if(ts.isReturnStatement(a))out.push(...v(a.expression));
          ts.forEachChild(a,returns);
        }
        returns(target.body);
      }
    }
  }
  out=[...new Set(out)];if(trail.size===0||out.length)cache.set(n,out);return out;
}
function objectMember(obj,prop,trail) {
  if(trail.has(obj)||trail.size>30)return [];
  trail=new Set(trail);trail.add(obj);
  let out=[];for(const p of obj.properties) {
    if(ts.isSpreadAssignment(p)) {
      for(const b of value(p.expression,trail))if(ts.isObjectLiteralExpression(b))out.push(...objectMember(b,prop,trail));
    }
    else if(name(p)===prop)out.push(...(callable(p)?[p]:value(p.initializer||p.name,trail)));
  }return out;
}
// Inventory every Error class, including inherited code fields. A changed class
// name cannot silently escape a handwritten constructor regex.
const errorClasses={};
for(const [nm,c] of classes)if(/Error$/.test(nm)) {
  errorClasses[nm]=rules.errorClassInfo(ts,c,rel(c),line(c));
}
for(let i=0;i<10;i++)for(const c of Object.values(errorClasses))if(errorClasses[c.parent]?.code)c.code=true;
// Discover code-producing factories from their bodies, then propagate through
// wrappers to a fixed point. No callee spelling or fixed argument offset.
const factoryPositions=new Map();
let factoriesChanged;
do {
  factoriesChanged=false;
  for(const node of byNode.keys())if(callable(node)) {
    const positions=rules.factoryCodePositions(ts,node,errorClasses,call=>
      [...new Set(value(call.expression).flatMap(target=>factoryPositions.get(target)||[]))]);
    if(JSON.stringify(positions)!==JSON.stringify(factoryPositions.get(node)||[])) {
      factoryPositions.set(node,positions);factoriesChanged=true;
    }
  }
} while(factoriesChanged);
const factoryInventory=[...factoryPositions].filter(([,positions])=>positions.length).map(([n,positions])=>
  ({file:rel(n),line:line(n),symbol:name(n)||name(n.parent),parameters:n.parameters.map(p=>name(p)),code_positions:positions}));
// Whole code-bearing class declarations are independent supplementary scopes,
// including fixed code fields and inherited super-constructor codes.
for(const [nm,c] of classes)if(errorClasses[nm]?.code) {
  const f={id:functions.length,file:rel(c),start:line(c),end:line(c,c.end),symbol:nm+'#code',class:nm,kind:'error_class',calls:[],errors:[],unknown_errors:[],literals:[],constructs:[]};
  functions.push(f);byNode.set(c,f);
}
const resolvedCalls=new Map();
for(const [n,f] of byNode) {
  f.code_expressions=[];f.result_expressions=[];f.invalid_code_literals=[];f.dynamic_code_expressions=[];
  function production(a){if(a)f.code_expressions.push({expression:a.getText(),start:line(a),end:line(a,a.end)});}
  function observeCode(a,kind) {
    production(a);
    if(!a)return;
    const resolved=rules.codeResolution(ts,a,checker);
    if(resolved.dynamic)f.dynamic_code_expressions.push({expression:a.getText(),start:line(a),end:line(a,a.end)});
    for(const observation of rules.codeObservations(ts,a,checker)) {
      // The origin is the producing expression, even when its value lives in
      // another file or at module scope outside this callable's bounds.
      const row={code:observation.code,line:line(a),class:kind};
      if(observation.valid)f.literals.push({value:observation.code,line:line(a)});
      if(observation.valid)f.errors.push(row);
      else f.invalid_code_literals.push(row);
    }
  }
  function scan(a) {
    // Nested callbacks execute within this function; their bodies are scanned,
    // but declarations of nested named functions are reached via call edges.
    if(a!==n.body&&(ts.isFunctionDeclaration(a)||ts.isMethodDeclaration(a)))return;
    // Literal membership is collected over the entire independently bounded
    // declaration below, including nested declarations and return type literals.
    if(ts.isReturnStatement(a))f.result_expressions.push({expression:a.getText(),start:line(a),end:line(a,a.end)});
    if(ts.isNewExpression(a)) {
      const nm=a.expression.getText();f.constructs.push({class:nm,file:rel(a),line:line(a)});
      if(rules.unknownError(nm,errorClasses))f.unknown_errors.push({class:nm,line:line(a)});
      if(errorClasses[nm]?.code) {
        observeCode(a.arguments?.[errorClasses[nm].code_argument],nm);
        const declaration=classes.get(nm);
        const ctor=declaration.members.find(x=>ts.isConstructorDeclaration(x)&&x.body);
        let ancestor=nm;
        while(errorClasses[ancestor]?.code) {
          const declaration=classes.get(ancestor);
          if(byNode.has(declaration))f.calls.push({expression:'code definition '+ancestor,line:line(a),targets:[byNode.get(declaration).id]});
          ancestor=errorClasses[ancestor].parent;
        }
        if(ctor&&byNode.has(ctor))f.calls.push({expression:'new '+nm,line:line(a),targets:[byNode.get(ctor).id]});
      }
    }
    if(ts.isGetAccessor(a)&&a.name?.text==='code'&&a.body) {
      function returns(r){if(ts.isReturnStatement(r)&&r.expression){observeCode(r.expression,f.class);}ts.forEachChild(r,returns);}
      returns(a.body);
    }
    if(ts.isCallExpression(a)&&a.expression.kind===ts.SyntaxKind.SuperKeyword) {
      const parent=errorClasses[f.class]?.parent;
      if(errorClasses[parent]?.code)observeCode(a.arguments[errorClasses[parent].code_argument],parent);
    }
    if(ts.isShorthandPropertyAssignment(a)&&a.name.text==='code')observeCode(a.name,'code');
    if((ts.isPropertyAssignment(a)||ts.isPropertyDeclaration(a))&&name(a)==='code')observeCode(a.initializer,'code');
    if(ts.isBinaryExpression(a)&&a.operatorToken.kind===ts.SyntaxKind.EqualsToken&&
      ((ts.isPropertyAccessExpression(a.left)&&a.left.name.text==='code')||
       (ts.isElementAccessExpression(a.left)&&a.left.argumentExpression?.text==='code')))observeCode(a.right,f.class);
    if(ts.isGetAccessor(n)&&name(n)==='code'&&ts.isReturnStatement(a))observeCode(a.expression,f.class);
    if(ts.isCallExpression(a)) {
      const expr=a.expression.getText();const targets=value(a.expression).map(x=>byNode.get(x)?.id).filter(x=>x!==undefined);
      const call={expression:expr,line:line(a),targets:[...new Set(targets)]};
      f.calls.push(call);resolvedCalls.set(call,a);
      for(const target of value(a.expression))for(const position of factoryPositions.get(target)||[]) {
        const argument=a.arguments[position]||target.parameters[position].initializer;
        if(argument)observeCode(argument,expr);
        else if(!target.parameters[position].questionToken)f.invalid_code_literals.push({code:'<missing argument>',line:line(a),class:expr});
      }
    }
    ts.forEachChild(a,scan);
  }
  scan(n.body||n);

  function literals(a){if(ts.isStringLiteralLike(a)&&rules.errorCodeLiteral(a.text))f.literals.push({value:a.text,line:line(a)});ts.forEachChild(a,literals);}
  literals(n);
}
const ws=sources.find(s=>rel(s)==='src/gateway/server/GatewayWsConnection.ts');
const methods={submit_turn:'submitTurn'}, wsBranchLiterals={};
function wsMethods(n) {
  if(ts.isCaseClause(n)&&ts.isStringLiteral(n.expression)) {
    const body=n.getText();const m=body.match(/this\.options\.gateway\.(\w+)/)||body.match(/this\.requireCapability\("[^"]+", "(\w+)"/);
    if(m) {
      methods[n.expression.text]=m[1];
      const literals=new Set();
      function collect(a){if(ts.isStringLiteralLike(a)&&rules.errorCodeLiteral(a.text))literals.add(a.text);ts.forEachChild(a,collect);}
      for(const statement of n.statements)collect(statement);wsBranchLiterals[n.expression.text]=[...literals];
    }
  }ts.forEachChild(n,wsMethods);
}
wsMethods(ws);
// Default assembly is rooted at this exact CLI call, including the concrete
// values passed to it. Follow calls, callbacks and instance initializers; a new
// expression elsewhere in src is not construction evidence.
const cli=sources.find(s=>rel(s)==='src/cli/pilotdeck.ts');let boot,bootNode;
function findBoot(n){if(ts.isCallExpression(n)&&n.expression.getText()==='createLocalGateway'&&line(n)===107){
  if(bootNode)throw Error('Ambiguous CLI boot root');
  bootNode=n;boot={file:rel(n),line:line(n),keys:n.arguments[0].properties.map(p=>name(p)||p.getText())};
}ts.forEachChild(n,findBoot);}
findBoot(cli);if(!bootNode)throw Error('CLI boot root missing');
const defaultNodes=new Set(),defaultFunctions=new Set(),defaultSites=new Set(),pending=[bootNode];
assemblySites=defaultSites;cache.clear();
function enqueue(n){if(n&&sources.includes(n.getSourceFile())&&!defaultNodes.has(n))pending.push(n);}
function assembled() {
  while(pending.length) {
    const node=pending.pop();if(defaultNodes.has(node))continue;defaultNodes.add(node);
    if(byNode.has(node))defaultFunctions.add(byNode.get(node).id);
    function inspect(a) {
      if(a!==node&&callable(a)){if(!ts.isFunctionDeclaration(a))enqueue(a);return;}
      if(ts.isClassDeclaration(a))return;
      if(ts.isCallExpression(a))for(const target of value(a.expression))if(callable(target))enqueue(target);
      if(ts.isNewExpression(a)) {
        defaultSites.add(a);
        for(const cl of declarations(a.expression).filter(ts.isClassDeclaration))for(const m of cl.members) {
          if(ts.isConstructorDeclaration(m))enqueue(m);
          else if(ts.isPropertyDeclaration(m)&&m.initializer)enqueue(m.initializer);
        }
      }
      // Concrete values supplied to factories/constructors and returned in
      // objects can include lazy provider callbacks and prebuilt CLI resources.
      if(ts.isIdentifier(a))for(const d of declarations(a)) {
        if(d.initializer)enqueue(d.initializer);
        else if(callable(d))enqueue(d);
      }
      ts.forEachChild(a,inspect);
    }
    inspect(callable(node)?node.body:node);
  }
}
// Discover sites monotonically. A later constructor site can resolve an earlier
// parameter binding, so revisit the finite set until it reaches a fixed point.
function assemblyFixedPoint() {
  let before;
  do {
    before=defaultNodes.size+defaultSites.size;
    const revisit=[...defaultNodes];defaultNodes.clear();cache.clear();pending.push(...revisit);
    assembled();
  } while(before!==defaultNodes.size+defaultSites.size);
}
assemblyFixedPoint();
if(![...defaultSites].some(s=>s.expression.getText()==='InProcessGateway'))throw Error('CLI root does not construct InProcessGateway');
// The constructed Gateway exposes the independently discovered RPC handlers.
// These are runtime entry points of the root's returned instance, and include
// lazy project/runtime construction after boot.
for(const member of Object.values(methods))for(const [node,f] of byNode)
  if(f.class==='InProcessGateway'&&f.symbol===member&&f.start>630)enqueue(node);
assemblyFixedPoint();
cache.clear();
// Runtime route resolution uses only constructor sites admitted by the root.
for(const [call,node] of resolvedCalls)call.targets=[...new Set(value(node.expression).map(x=>byNode.get(x)?.id).filter(x=>x!==undefined))];
const defaultPoints=[...defaultSites].map(s=>({class:s.expression.getText(),ref:'B/'+rel(s)+':'+line(s)}));
const routes={};
for(const [method,member] of Object.entries(methods)) {
  const handler=functions.find(f=>f.class==='InProcessGateway'&&f.symbol===member&&f.start>630);
  if(!handler)throw Error('No handler for '+method);
  const reached=new Map([[handler.id,0]]);let frontier=[handler.id];
  for(let depth=1;depth<=DEPTH;depth++) {
    const next=[];
    for(const id of frontier)for(const call of functions[id].calls)for(const target of call.targets)if(!reached.has(target)){reached.set(target,depth);next.push(target);}
    frontier=next;
  }
  const providers=[...reached].filter(([id,depth])=>depth<=2&&functions[id].class!=='InProcessGateway'&&!/Error$/.test(functions[id].class||'')).map(([id])=>id);
  const routeClasses=new Set([...reached.keys()].map(id=>functions[id].class).filter(n=>n&&!/Error$/.test(n)));
  const constructors=defaultPoints.filter(p=>routeClasses.has(p.class)&&p.ref.startsWith('B/src/'));
  routes[method]={handler:handler.id,providers:[...new Set(providers)],functions:[...reached].map(([id,depth])=>({id,depth})),construction_points:constructors.sort((a,b)=>(a.class+'|'+a.ref).localeCompare(b.class+'|'+b.ref,'en'))};
}
process.stdout.write(JSON.stringify({parser_controls:rules.controls(ts),parser:ts.version,depth:DEPTH,boot,errorClasses,factory_inventory:factoryInventory,node_errno:[...new Set([...Object.keys(require("node:os").constants.errno),...Object.keys(require("node:constants"))])],functions,routes,ws_branch_literals:wsBranchLiterals,
  default_functions:[...defaultFunctions].sort((a,b)=>a-b),default_construction_points:defaultPoints}));
