// Distinguish literal backslashes from actual control characters in YAML.
import {createRequire} from 'node:module';
import {execFileSync} from 'node:child_process';
import {writeFileSync} from 'node:fs';
import {resolve,join} from 'node:path';
const baseline=resolve(process.argv[2]??'');
const output=resolve(process.argv[3]??'');
if(process.argv.length!==4)throw new Error('Usage: generate-escapes.mjs <baseline> <new-output-file>');
const head=execFileSync('git',['-C',baseline,'rev-parse','HEAD'],{encoding:'utf8'}).trim();
if(head!=='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85'||process.version!=='v22.22.0'||process.versions.icu!=='78.2')throw new Error('Baseline/Node/ICU mismatch');
const require=createRequire(join(baseline,'package.json'));const yaml=require('yaml');
if(require('yaml/package.json').version!=='2.9.0')throw new Error('yaml mismatch');
const values=[' \\u0000',' \\u0007',' \\u000b',' \\u001b',' \0',' \x07',' \x0b',' \x1b'];
const adapter={enabled:true,appId:'fixture-app',appSecret:'fixture-secret',connectionMode:'stream',domainName:'feishu'};
const cases=values.map((value,index)=>{
 const input=yaml.stringify({note:value},{lineWidth:0,singleQuote:false});const object=yaml.parse(input);object.adapters={feishu:adapter};
 return {id:String(index),value,input,expected:yaml.stringify(object,{lineWidth:0,singleQuote:false})};
});
writeFileSync(output,JSON.stringify({node:process.version,icu:process.versions.icu,yamlVersion:'2.9.0',cases},null,2)+'\n',{flag:'wx'});
