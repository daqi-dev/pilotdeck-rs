import {execFileSync} from 'node:child_process';
import {createHash} from 'node:crypto';
import {readFile,mkdir,writeFile} from 'node:fs/promises';
import {resolve,join} from 'node:path';
import {pathToFileURL} from 'node:url';
const [base,out]=process.argv.slice(2);
if(!base||!out||process.argv.length!==4)throw Error('usage: generate-permissions.mjs BASELINE NEW_DIR');
if(process.version!=='v22.22.0'||process.versions.icu!=='78.2')throw Error('Node/ICU mismatch');
if(execFileSync('git',['-C',base,'rev-parse','HEAD'],{encoding:'utf8'}).trim()!=='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85')throw Error('commit mismatch');
const source=join(resolve(base),'src/permission/settings.ts');
if(createHash('sha256').update(await readFile(source)).digest('hex')!=='0c05642ec72f1617259457aed51aff9e04e0a8a0c7d6f9ef4a508c41384d112b')throw Error('source mismatch');
await mkdir(out);const env={PILOT_HOME:resolve(out)};
const mod=await import(pathToFileURL(source).href);
const inputs=[null,{}, {allowedTools:[' Read ','Read','Bash(git status)','Bash()','',null,7,'unknown'],askTools:['Write','Edit'],disallowedTools:['  Glob  '],skipPermissions:[],lastUpdated:'old'}, {skipPermissions:'false',allowedTools:'Read',lastUpdated:4}];
const normalized=inputs.map(input=>({input,output:mod.normalizePermissionSettings(input)}));
const missing=mod.readPermissionSettings(env);
await writeFile(join(out,'permissions.json'),JSON.stringify({allowedTools:['Read'],askTools:['Grep'],disallowedTools:['Write'],skipPermissions:false}));
const OriginalDate=Date;globalThis.Date=class extends OriginalDate {constructor(...args){super(...(args.length?args:['2026-11-08T00:00:00.000Z']));}};
const written=mod.writePermissionSettings({allowedTools:['Read','Bash(git status)'],skipPermissions:true},env);
const bytes=await readFile(join(out,'permissions.json'),'utf8');
const rawCases = [
  '{"allowedTools":["\\ud800","\\ud800"," Read ","Bash(git  status)"],"disallowedTools":["\\udfff"],"unknown":{"keep":1},"lastUpdated":"\\ud800"}',
  '{"allowedTools":["\\udc00","x"],"skipPermissions":1e400,"unknown":9007199254740993}',
  '{"allowedTools":[],"askTools":[],"disallowedTools":[],"skipPermissions":false}',
];
const rawNormalized=rawCases.map(raw=>({raw,output:JSON.stringify(mod.normalizePermissionSettings(JSON.parse(raw)))}));
const malformedDir=join(out,'malformed');await mkdir(malformedDir);const malformedEnv={PILOT_HOME:malformedDir};
await writeFile(join(malformedDir,'permissions.json'),'{bad');
const malformedRead=mod.readPermissionSettings(malformedEnv);
const malformedWrite=mod.writePermissionSettings({skipPermissions:true},malformedEnv);
const malformedAfter=await readFile(join(malformedDir,'permissions.json'),'utf8');
await writeFile(join(out,'fixture.json'),JSON.stringify({normalized,missing,written,bytes,rawNormalized,malformedRead,malformedWrite,malformedAfter},null,2)+'\n',{flag:'wx'});
