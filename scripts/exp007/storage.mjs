import {readdir,readFile,writeFile} from 'node:fs/promises';
import {resolve,relative} from 'node:path';
import {baseline,requireUI,evidence,root} from './env.mjs';
const ts=requireUI('typescript');
async function files(dir){const entries=await readdir(dir,{withFileTypes:true});const nested=await Promise.all(entries.map(e=>e.isDirectory()?files(resolve(dir,e.name)):/\.(tsx?|jsx?|mjs)$/.test(e.name)?[resolve(dir,e.name)]:[]));return nested.flat().sort();}
const sources=[];
for(const file of await files(resolve(baseline,'src'))){const text=await readFile(file,'utf8');sources.push({file,text,ast:ts.createSourceFile(file,text,ts.ScriptTarget.Latest,true),constants:new Map(),aliases:new Map()});}
const globalConstants=new Map();
const visit=(node,fn)=>{fn(node);ts.forEachChild(node,child=>visit(child,fn));};
for(const source of sources)visit(source.ast,node=>{
 if(ts.isVariableDeclaration(node)&&ts.isIdentifier(node.name)&&node.initializer){
   let init=node.initializer;while(ts.isAsExpression(init)||ts.isParenthesizedExpression(init)||ts.isSatisfiesExpression(init))init=init.expression;
   if(ts.isStringLiteralLike(init)){source.constants.set(node.name.text,init.text);const values=globalConstants.get(node.name.text)||new Set();values.add(init.text);globalConstants.set(node.name.text,values);}
   const area=init.getText(source.ast).match(/^(?:(?:window|globalThis)\.)?(localStorage|sessionStorage)$/)?.[1];
   if(area)source.aliases.set(node.name.text,area);
 }
});
const calls=[];
for(const source of sources)visit(source.ast,node=>{
 if(!ts.isCallExpression(node)||!ts.isPropertyAccessExpression(node.expression))return;
 const method=node.expression.name.text;
 if(!['getItem','setItem','removeItem','clear','key'].includes(method))return;
 const receiver=node.expression.expression.getText(source.ast);
 const area=receiver.match(/^(?:(?:window|globalThis)\.)?(localStorage|sessionStorage)$/)?.[1]||source.aliases.get(receiver);
 if(!area)return;
 const arg=node.arguments[0];let key=arg?.getText(source.ast)||'*';let kind='dynamic/expression';
 if(arg&&ts.isStringLiteralLike(arg)){key=arg.text;kind='literal';}
 else if(arg&&ts.isIdentifier(arg)){const values=globalConstants.get(arg.text);const value=source.constants.get(arg.text)||(values?.size===1?[...values][0]:undefined);if(value!==undefined){key=value;kind='resolved constant';}}
 const {line}=source.ast.getLineAndCharacterOfPosition(node.getStart(source.ast));
 calls.push({path:relative(baseline,source.file),line:line+1,area,method,key,kind,argument:arg?.getText(source.ast)||null,test:/\.(test|spec)\./.test(source.file)});
});
const summary={scannedFiles:sources.length,callSites:calls.length,productionCallSites:calls.filter(c=>!c.test).length,literalAndConstantKeys:[...new Set(calls.filter(c=>!c.test&&c.kind!=='dynamic/expression').map(c=>`${c.area}:${c.key}`))].sort(),dynamicProductionCalls:calls.filter(c=>!c.test&&c.kind==='dynamic/expression').length,coverage:'All direct storage calls and simple aliases in ui/src AST; dynamic key expressions retained, runtime branches/third-party calls not proven exhaustive',calls};
await writeFile(resolve(evidence,'storage.json'),JSON.stringify(summary,null,2)+'\n');
let md=`# F1 storage 键盘点（静态，非运行时穷举）\n\n扫描 ui/src 的 ${summary.scannedFiles} 个 TS/JS 文件；${summary.callSites} 个调用点（生产 ${summary.productionCallSites}）。直接调用与简单 storage 别名全留档；函数封装、动态模板和第三方调用仍需 F2 网络/运行时录制。\n\n`;
md+='| area | 已解析键 | 依据（baseline ui/） |\n|---|---|---|\n';
for(const full of summary.literalAndConstantKeys){const colon=full.indexOf(':');const area=full.slice(0,colon),key=full.slice(colon+1);const call=calls.find(c=>!c.test&&c.area===area&&c.key===key);md+=`| ${area} | \`${key.replaceAll('|','\\|')}\` | ${call.path}:${call.line} |\n`;}
md+=`\n生产动态调用 ${summary.dynamicProductionCalls} 处（完整表达式、源码路径、行号见 evidence/storage.json）；不得把表达式当作已穷举的键。\n\n`;
md+='| 动态键表达式 | area | 依据 |\n|---|---|---|\n';
for(const c of calls.filter(c=>!c.test&&c.kind==='dynamic/expression'))md+=`| \`${c.argument?.replaceAll('|','\\|').replaceAll('\n',' ')}\` | ${c.area} | ${c.path}:${c.line} |\n`;
md+='\n共享契约最少包括 auth-token、sessionStorage 的 pilotdeck-active-project、draft_input_<projectName>:<sessionId|new>、themeMode/theme、userLanguage。i18next detector 默认 i18nextLng 是第三方隐含键，须 F2 录制复核；不是本 AST 扫描的直接调用。实验只读取源码，不读取用户浏览器 storage。\n';
await writeFile(resolve(root,'docs/experiments/exp007-frontend-probe/storage-keys.md'),md);
console.log(JSON.stringify({...summary,calls:undefined},null,2));
